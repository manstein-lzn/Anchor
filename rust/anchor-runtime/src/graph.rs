//! Durable, serial coordinator for an already-expanded Anchor graph snapshot.
//!
//! This module owns graph/run facts only. Agent checkpoints, providers, tools,
//! and host execution remain behind `NodeExecutionPort`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    future::Future,
    io::{self, Write},
    path::PathBuf,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphSnapshot {
    pub objective: String,
    #[serde(default)]
    pub input: Value,
    pub entry: String,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentDefinition>,
    #[serde(default)]
    pub ops: BTreeMap<String, Value>,
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Vec<GraphEdge>,
    #[serde(default, rename = "_module_rounds")]
    pub module_rounds: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub max_steps: Option<u64>,
    #[serde(default)]
    pub wall_time_limit_seconds: Option<f64>,
    #[serde(default)]
    pub reads: Vec<String>,
    #[serde(default)]
    pub writes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    pub id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default, rename = "with")]
    pub input: Option<Value>,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub max_rounds: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Agent,
    OpRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEdge {
    #[serde(rename = "from")]
    pub from_node: String,
    #[serde(rename = "to")]
    pub to_node: String,
}

impl GraphSnapshot {
    pub fn admit(value: Value) -> Result<Self, GraphError> {
        if let Value::Object(fields) = &value {
            let allowed = [
                "objective",
                "input",
                "entry",
                "agents",
                "ops",
                "nodes",
                "edges",
                "_module_rounds",
            ];
            if let Some(unknown) = fields.keys().find(|key| !allowed.contains(&key.as_str())) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "unknown graph snapshot field `{unknown}`"
                )));
            }
        }
        let snapshot: Self = serde_json::from_value(value).map_err(GraphError::SnapshotDecode)?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), GraphError> {
        if self.nodes.is_empty() {
            return Err(GraphError::InvalidSnapshot("graph has no nodes".into()));
        }
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            validate_node_id(&node.id)?;
            if !ids.insert(node.id.as_str()) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "duplicate node id `{}`",
                    node.id
                )));
            }
            if node.max_rounds == Some(0) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "node `{}` max_rounds must be positive",
                    node.id
                )));
            }
            if node.agent.is_some() == node.op.is_some() {
                return Err(GraphError::InvalidSnapshot(format!(
                    "node `{}` must have exactly one of agent/op",
                    node.id
                )));
            }
            if !node.plugins.is_empty() {
                return Err(GraphError::Unsupported(format!(
                    "node `{}` declares plugins, unsupported in R5",
                    node.id
                )));
            }
            if let Some(op_name) = &node.op {
                let op = self.ops.get(op_name).ok_or_else(|| {
                    GraphError::InvalidSnapshot(format!(
                        "node `{}` references missing op `{op_name}`",
                        node.id
                    ))
                })?;
                if op.get("fanout").is_some() || op.get("join").is_some() {
                    return Err(GraphError::Unsupported(format!(
                        "fanout/join op `{op_name}` is reserved for R6"
                    )));
                }
                if op.get("call").is_some() {
                    return Err(GraphError::Unsupported(format!(
                        "op.call `{op_name}` is reserved for R7"
                    )));
                }
                if op.get("run").is_none() {
                    return Err(GraphError::Unsupported(format!(
                        "op `{op_name}` has no supported R5 execution capability"
                    )));
                }
            }
            if let Some(agent_name) = &node.agent {
                let agent = self.agents.get(agent_name).ok_or_else(|| {
                    GraphError::InvalidSnapshot(format!(
                        "node `{}` references missing agent `{agent_name}`",
                        node.id
                    ))
                })?;
                validate_wall_time(agent.wall_time_limit_seconds, &node.id)?;
            }
            if let Some(op_name) = &node.op {
                let op = &self.ops[op_name];
                if op.get("network").is_some_and(|value| !value.is_boolean()) {
                    return Err(GraphError::InvalidSnapshot(format!(
                        "op `{op_name}` network must be a boolean"
                    )));
                }
                if op
                    .get("wall_time_limit_seconds")
                    .is_some_and(|value| value.as_f64().is_none())
                {
                    return Err(GraphError::InvalidSnapshot(format!(
                        "op `{op_name}` wall_time_limit_seconds must be a number"
                    )));
                }
                validate_wall_time(
                    op.get("wall_time_limit_seconds").and_then(Value::as_f64),
                    &node.id,
                )?;
            }
        }
        if !ids.contains(self.entry.as_str()) {
            return Err(GraphError::InvalidSnapshot(format!(
                "entry `{}` does not name a node",
                self.entry
            )));
        }
        let mut edge_set = BTreeSet::new();
        for edge in &self.edges {
            if !edge_set.insert((edge.from_node.as_str(), edge.to_node.as_str())) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "duplicate edge {} -> {}",
                    edge.from_node, edge.to_node
                )));
            }
            if !ids.contains(edge.from_node.as_str()) || !ids.contains(edge.to_node.as_str()) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "edge {} -> {} references unknown node",
                    edge.from_node, edge.to_node
                )));
            }
        }
        for (scope, limit) in &self.module_rounds {
            if *limit == 0 || !ids.iter().any(|id| id.starts_with(&format!("{scope}/"))) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "invalid module-round scope or ceiling `{scope}`"
                )));
            }
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, GraphError> {
        let bytes = serde_json::to_vec(self).map_err(GraphError::SnapshotDecode)?;
        Ok(format!("{:x}", sha2::Sha256::digest(bytes)))
    }
}

use sha2::Digest;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationKey {
    pub run_id: String,
    pub graph_digest: String,
    pub node_id: String,
    pub invocation: u64,
}
impl InvocationKey {
    pub fn durable_key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.run_id, self.graph_digest, self.node_id, self.invocation
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitRef {
    pub id: String,
    pub node_id: String,
    pub invocation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeCompletion {
    pub submission: String,
    pub route: Option<String>,
    pub model_requests: u64,
    pub output: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Ready,
    Running,
    Paused,
    BudgetStopped,
    Completed,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeDecision {
    pub selected: bool,
    pub sequence: u64,
    pub source_invocation: u64,
    #[serde(default)]
    pub result_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCursor {
    pub node_id: String,
    pub key: InvocationKey,
    pub input_commits: Vec<CommitRef>,
    pub prepared_input: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunResult {
    pub node_id: String,
    pub key: InvocationKey,
    pub completion: NodeCompletion,
    pub commit: CommitRef,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphRunRecord {
    pub format: u32,
    pub run_id: String,
    pub graph_digest: String,
    pub snapshot: GraphSnapshot,
    pub input: Value,
    pub status: RunStatus,
    pub cursor: Option<RunCursor>,
    pub invocations: BTreeMap<String, u64>,
    pub passes: BTreeMap<String, u64>,
    pub module_activations: BTreeMap<String, u64>,
    pub ceased: BTreeSet<String>,
    pub results: BTreeMap<String, Vec<RunResult>>,
    pub decided: BTreeMap<String, EdgeDecision>,
    pub sequence: u64,
    pub error: Option<String>,
}

pub trait RunStore: Send + Sync {
    fn load(&self, run_id: &str) -> Result<Option<GraphRunRecord>, GraphError>;
    fn save(&self, record: &GraphRunRecord) -> Result<(), GraphError>;
    fn acquire_lease(&self, run_id: &str) -> Result<Box<dyn RunLease>, GraphError>;
}
pub trait RunLease: Send {}

#[derive(Debug, Clone)]
pub struct FileRunStore {
    root: PathBuf,
}
impl FileRunStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn path(&self, id: &str) -> Result<PathBuf, GraphError> {
        validate_component(id)?;
        Ok(self.root.join(format!("{id}.json")))
    }
}
impl RunStore for FileRunStore {
    fn load(&self, run_id: &str) -> Result<Option<GraphRunRecord>, GraphError> {
        let bytes = match fs::read(self.path(run_id)?) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut record: GraphRunRecord =
            serde_json::from_slice(&bytes).map_err(GraphError::RunDecode)?;
        if record.run_id != run_id {
            return Err(GraphError::CorruptRun(
                "run identity or format mismatch".into(),
            ));
        }
        record.migrate_format()?;
        record.validate()?;
        Ok(Some(record))
    }
    fn save(&self, record: &GraphRunRecord) -> Result<(), GraphError> {
        record.validate()?;
        let path = self.path(&record.run_id)?;
        fs::create_dir_all(&self.root)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = self.root.join(format!(
            ".{}.{}.{}.tmp",
            record.run_id,
            std::process::id(),
            stamp
        ));
        let bytes = serde_json::to_vec(record).map_err(GraphError::RunDecode)?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(tmp);
            return Err(e.into());
        }
        fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    fn acquire_lease(&self, run_id: &str) -> Result<Box<dyn RunLease>, GraphError> {
        validate_component(run_id)?;
        fs::create_dir_all(&self.root)?;
        let path = self.root.join(format!(".{run_id}.lock"));
        // Keep the lock inode stable. The OS releases this advisory lock if
        // the process exits, including an unclean crash; never unlink it.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => GraphError::RunBusy(run_id.to_owned()),
            std::fs::TryLockError::Error(error) => GraphError::Io(error),
        })?;
        file.sync_all()?;
        Ok(Box::new(FileRunLease { _file: file }))
    }
}
struct FileRunLease {
    _file: fs::File,
}
impl RunLease for FileRunLease {}

pub trait ArtifactPort: Send + Sync {
    /// Must be idempotent for an InvocationKey. A retry after freeze succeeded
    /// but before the RunStore commit must resolve to the same immutable commit.
    fn freeze<'a>(
        &'a self,
        key: &'a InvocationKey,
        completion: &'a NodeCompletion,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>>;
    /// Read/materialize the exact fixed commit. Implementations must not follow
    /// a mutable workspace head or create/advance a commit as a side effect.
    fn resolve<'a>(
        &'a self,
        commit: &'a CommitRef,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>>;
}

#[derive(Debug, Clone)]
pub struct NodeExecutionRequest {
    pub key: InvocationKey,
    /// Provider/model reference selected by the Graph for Agent nodes.
    pub model: Option<String>,
    pub task: String,
    pub instructions: String,
    pub routes: Vec<String>,
    pub input: Value,
    pub input_commits: Vec<CommitRef>,
    /// Python max_steps is a cumulative provider-request budget. Implementors
    /// must honor this exact meaning; it is not Rig max_turns.
    pub max_provider_requests: Option<u64>,
    pub wall_time_limit_seconds: Option<f64>,
    /// Graph request intent only. The host adapter must apply its own network authorization.
    pub network: bool,
    pub kind: NodeKind,
    pub operation: Option<Value>,
    pub cancellation: crate::Cancellation,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompletionFact {
    NotStarted,
    Completed(NodeCompletion),
    Failed(String),
    Uncertain(String),
}

pub trait NodeExecutionPort: Send + Sync {
    fn capabilities(&self) -> NodeExecutionCapabilities;
    fn completion_fact<'a>(
        &'a self,
        key: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>>;
    /// On `Completed` or `Failed`, the implementation MUST durably write the
    /// matching completion fact before resolving this future. If it cannot
    /// establish whether execution completed it must persist/return Uncertain.
    /// BudgetExhausted retains the same invocation's resumable checkpoint and
    /// MUST NOT publish a completion fact.
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NodeExecutionCapabilities {
    pub agent: bool,
    pub op_run: bool,
    /// True only when max_provider_requests is enforced as the cumulative
    /// provider request count, rather than translated to a turn count.
    pub exact_provider_request_budget: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeExecutionOutcome {
    Completed(NodeCompletion),
    BudgetExhausted {
        model_requests: u64,
    },
    Cancelled,
    /// A known terminal node failure. The host has established that the
    /// invocation failed; the Graph Run records a terminal failure and never
    /// treats it as an invitation to replay the invocation.
    Failed {
        reason: String,
    },
}

pub trait RunControl: Send + Sync {
    fn pause_requested(&self) -> bool;
    fn stop_requested(&self) -> bool;
    fn cancellation(&self) -> crate::Cancellation;
}

pub struct GraphRunner<'a, S, A, N, C> {
    store: &'a S,
    artifacts: &'a A,
    nodes: &'a N,
    control: &'a C,
}
impl<'a, S: RunStore, A: ArtifactPort, N: NodeExecutionPort, C: RunControl>
    GraphRunner<'a, S, A, N, C>
{
    pub fn new(store: &'a S, artifacts: &'a A, nodes: &'a N, control: &'a C) -> Self {
        Self {
            store,
            artifacts,
            nodes,
            control,
        }
    }
    pub async fn run(&self, mut record: GraphRunRecord) -> Result<GraphRunRecord, GraphError> {
        record.migrate_format()?;
        record.validate()?;
        self.admit_capabilities(&record.snapshot)?;
        let _lease = self.store.acquire_lease(&record.run_id)?;
        if let Some(mut saved) = self.store.load(&record.run_id)? {
            saved.migrate_format()?;
            if saved != record {
                return Err(GraphError::RunConflict);
            }
        }
        // Upgrade an in-flight record written by the earlier R5 counter timing:
        // it persisted a cursor before dispatch, but counted only completed nodes.
        // The cursor itself proves this invocation started, so promote that fact
        // once before writing the resumed record.
        if let Some(cursor) = &record.cursor
            && record.invocations.get(&cursor.node_id).copied() != Some(cursor.key.invocation)
        {
            record
                .invocations
                .insert(cursor.node_id.clone(), cursor.key.invocation);
            *record.passes.entry(cursor.node_id.clone()).or_default() += 1;
        }
        if matches!(record.status, RunStatus::Failed | RunStatus::Completed) {
            self.store.save(&record)?;
            return Ok(record);
        }
        record.status = RunStatus::Running;
        self.store.save(&record)?;
        loop {
            if self.control.stop_requested() {
                record.status = RunStatus::Stopped;
                self.store.save(&record)?;
                return Ok(record);
            }
            if self.control.pause_requested() && record.cursor.is_none() {
                record.status = RunStatus::Paused;
                self.store.save(&record)?;
                return Ok(record);
            }
            if propagate_inactive_edges(&mut record)? {
                self.store.save(&record)?;
            }
            let Some(node) = next_node(&record)? else {
                record.status = if record.ceased.is_empty() {
                    RunStatus::Completed
                } else {
                    RunStatus::Stopped
                };
                self.store.save(&record)?;
                return Ok(record);
            };
            if record.cursor.is_none() {
                let scope = scope_of(&node.id);
                let prior_sequence = record
                    .results
                    .get(&node.id)
                    .and_then(|results| results.last())
                    .map(|r| r.sequence)
                    .unwrap_or(0);
                let enters_scope = !scope.is_empty()
                    && record
                        .snapshot
                        .edges
                        .iter()
                        .filter(|e| {
                            e.to_node == node.id && !e.from_node.starts_with(&format!("{scope}/"))
                        })
                        .any(|e| {
                            record
                                .decided
                                .get(&edge_key(&e.from_node, &e.to_node))
                                .is_some_and(|d| d.selected && d.sequence > prior_sequence)
                        });
                if enters_scope
                    && let Some(limit) = record.snapshot.module_rounds.get(&scope).copied()
                {
                    let next = record.module_activations.get(&scope).copied().unwrap_or(0) + 1;
                    if next > u64::from(limit) {
                        record.ceased.insert(format!("{scope}@{limit}"));
                        refuse_module_activation(&mut record, &scope, &node.id)?;
                        continue;
                    }
                    record.module_activations.insert(scope.clone(), next);
                    for member in &record.snapshot.nodes {
                        if member.id.starts_with(&format!("{scope}/")) {
                            record.passes.remove(&member.id);
                        }
                    }
                    for nested in record
                        .module_activations
                        .keys()
                        .filter(|nested| nested.starts_with(&format!("{scope}/")))
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        record.module_activations.remove(&nested);
                    }
                }
                if let Some(limit) = node.max_rounds
                    && record.passes.get(&node.id).copied().unwrap_or(0) >= u64::from(limit)
                {
                    record.ceased.insert(format!("{}@{limit}", node.id));
                    self.refuse_edges(&mut record, &node.id)?;
                    continue;
                }
                let invocation = record.invocations.get(&node.id).copied().unwrap_or(0) + 1;
                let key = InvocationKey {
                    run_id: record.run_id.clone(),
                    graph_digest: record.graph_digest.clone(),
                    node_id: node.id.clone(),
                    invocation,
                };
                let input_commits = record
                    .snapshot
                    .edges
                    .iter()
                    .filter(|e| e.to_node == node.id)
                    .filter_map(|e| {
                        record
                            .decided
                            .get(&edge_key(&e.from_node, &e.to_node))
                            .filter(|d| d.selected)
                            .and_then(|decision| {
                                record
                                    .results
                                    .get(&e.from_node)
                                    .and_then(|rs| {
                                        let result_sequence = if decision.result_sequence == 0 {
                                            decision.sequence
                                        } else {
                                            decision.result_sequence
                                        };
                                        rs.iter().find(|r| r.sequence == result_sequence)
                                    })
                                    .map(|r| r.commit.clone())
                            })
                    })
                    .collect::<Vec<_>>();
                let mut resolved = Vec::new();
                for commit in &input_commits {
                    resolved.push(self.artifacts.resolve(commit).await?);
                }
                let prepared_input =
                    serde_json::json!({"input":record.input,"committed_inputs":resolved});
                record.cursor = Some(RunCursor {
                    node_id: node.id.clone(),
                    key,
                    input_commits,
                    prepared_input,
                });
                // Python's Graph runner records a pass/run when the node starts,
                // including attempts that stop at the provider budget boundary.
                // Persist these counters with the cursor so resume observes the
                // same started invocation without counting it a second time.
                let cursor = record.cursor.as_ref().expect("cursor established");
                record
                    .invocations
                    .insert(cursor.node_id.clone(), cursor.key.invocation);
                *record.passes.entry(cursor.node_id.clone()).or_default() += 1;
                record.status = RunStatus::Running;
                self.store.save(&record)?; // durable before dispatch
            }
            if self.control.stop_requested() {
                record.status = RunStatus::Stopped;
                self.store.save(&record)?;
                return Ok(record);
            }
            let cursor = record.cursor.clone().expect("cursor established");
            let fact = self.nodes.completion_fact(&cursor.key).await?;
            let completion = match fact {
                CompletionFact::Completed(c) => Some(c),
                CompletionFact::Failed(reason) => {
                    return self
                        .fail_known_node(record, format!("{} failed: {reason}", cursor.node_id));
                }
                CompletionFact::Uncertain(reason) => {
                    return self.fail(
                        record,
                        format!("uncertain node result for {}: {reason}", cursor.node_id),
                    );
                }
                CompletionFact::NotStarted => None,
            };
            let completion = if let Some(c) = completion {
                c
            } else {
                if self.control.pause_requested() {
                    record.status = RunStatus::Paused;
                    self.store.save(&record)?;
                    return Ok(record);
                }
                let def = record
                    .snapshot
                    .nodes
                    .iter()
                    .find(|n| n.id == cursor.node_id)
                    .expect("validated node");
                let agent = def
                    .agent
                    .as_ref()
                    .and_then(|name| record.snapshot.agents.get(name));
                let operation = def
                    .op
                    .as_ref()
                    .and_then(|name| record.snapshot.ops.get(name))
                    .and_then(|value| value.get("run"))
                    .cloned();
                let routes = record
                    .snapshot
                    .edges
                    .iter()
                    .filter(|e| e.from_node == def.id)
                    .map(|e| e.to_node.clone())
                    .collect::<Vec<_>>();
                if self.control.stop_requested() {
                    record.status = RunStatus::Stopped;
                    self.store.save(&record)?;
                    return Ok(record);
                }
                let (
                    kind,
                    model,
                    instructions,
                    max_provider_requests,
                    wall_time_limit_seconds,
                    network,
                ) = if let Some(agent) = agent {
                    (
                        NodeKind::Agent,
                        Some(agent.model.clone()),
                        agent.instructions.clone(),
                        agent.max_steps,
                        Some(agent.wall_time_limit_seconds.unwrap_or(3600.0)),
                        agent.network,
                    )
                } else {
                    let op = def
                        .op
                        .as_ref()
                        .and_then(|name| record.snapshot.ops.get(name))
                        .expect("validated op");
                    (
                        NodeKind::OpRun,
                        None,
                        String::new(),
                        None,
                        Some(
                            op.get("wall_time_limit_seconds")
                                .and_then(Value::as_f64)
                                .unwrap_or(3600.0),
                        ),
                        op.get("network").and_then(Value::as_bool).unwrap_or(false),
                    )
                };
                let local_instruction = def
                    .input
                    .as_ref()
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let request = NodeExecutionRequest {
                    key: cursor.key.clone(),
                    model,
                    task: node_task(
                        &record.snapshot.objective,
                        &instructions,
                        local_instruction,
                        &cursor.prepared_input,
                    ),
                    instructions,
                    routes,
                    input: cursor.prepared_input.clone(),
                    input_commits: cursor.input_commits.clone(),
                    max_provider_requests,
                    wall_time_limit_seconds,
                    network,
                    kind,
                    operation,
                    cancellation: self.control.cancellation(),
                };
                match self.nodes.execute(request).await? {
                    NodeExecutionOutcome::Completed(c) => c,
                    NodeExecutionOutcome::BudgetExhausted { .. } => {
                        record.status = RunStatus::BudgetStopped;
                        self.store.save(&record)?;
                        return Ok(record);
                    }
                    NodeExecutionOutcome::Cancelled => {
                        record.status = RunStatus::Stopped;
                        self.store.save(&record)?;
                        return Ok(record);
                    }
                    NodeExecutionOutcome::Failed { reason } => {
                        return self.fail_known_node(
                            record,
                            format!("{} failed: {reason}", cursor.node_id),
                        );
                    }
                }
            };
            let routes = record
                .snapshot
                .edges
                .iter()
                .filter(|e| e.from_node == cursor.node_id)
                .map(|e| e.to_node.clone())
                .collect::<Vec<_>>();
            let chosen = match select_route(&completion, &routes) {
                Ok(route) => route,
                Err(error) => {
                    return self.fail_known_node(record, format!("{}: {error}", cursor.node_id));
                }
            };
            let commit = self.artifacts.freeze(&cursor.key, &completion).await?;
            record.sequence += 1;
            let result = RunResult {
                node_id: cursor.node_id.clone(),
                key: cursor.key.clone(),
                completion,
                commit,
                sequence: record.sequence,
            };
            // completion fact is durable in the NodeExecutionPort, then artifact freeze,
            // then this atomic Run record establishes result/history/edge decisions.
            let result_sequence = result.sequence;
            record
                .results
                .entry(cursor.node_id.clone())
                .or_default()
                .push(result);
            // Edge decisions happen after the result is committed. Giving them
            // their own sequence lets a selected self-loop be recognized as a
            // fresh re-entry after the node's latest result.
            record.sequence += 1;
            for target in routes {
                record.decided.insert(
                    edge_key(&cursor.node_id, &target),
                    EdgeDecision {
                        selected: Some(&target) == chosen.as_ref(),
                        sequence: record.sequence,
                        source_invocation: cursor.key.invocation,
                        result_sequence,
                    },
                );
            }
            record.cursor = None;
            record.status = RunStatus::Running;
            self.store.save(&record)?;
        }
    }
    fn refuse_edges(&self, record: &mut GraphRunRecord, node_id: &str) -> Result<(), GraphError> {
        record.sequence += 1;
        let invocation = record.invocations.get(node_id).copied().unwrap_or(0);
        for edge in record
            .snapshot
            .edges
            .iter()
            .filter(|edge| edge.from_node == node_id)
        {
            record.decided.insert(
                edge_key(node_id, &edge.to_node),
                EdgeDecision {
                    selected: false,
                    sequence: record.sequence,
                    source_invocation: invocation,
                    result_sequence: 0,
                },
            );
        }
        record.status = RunStatus::Running;
        self.store.save(record)
    }
    fn admit_capabilities(&self, snapshot: &GraphSnapshot) -> Result<(), GraphError> {
        let caps = self.nodes.capabilities();
        for node in &snapshot.nodes {
            if node.agent.is_some() && !caps.agent {
                return Err(GraphError::Unsupported(format!(
                    "NodeExecutionPort does not support Agent node `{}`",
                    node.id
                )));
            }
            if node.op.is_some() && !caps.op_run {
                return Err(GraphError::Unsupported(format!(
                    "NodeExecutionPort does not support Op.run node `{}`",
                    node.id
                )));
            }
            if let Some(agent) = node.agent.as_ref().and_then(|n| snapshot.agents.get(n))
                && agent.max_steps.is_some()
                && !caps.exact_provider_request_budget
            {
                return Err(GraphError::Unsupported(format!(
                    "Agent node `{}` has max_steps but NodeExecutionPort cannot enforce cumulative provider-request budget",
                    node.id
                )));
            }
        }
        Ok(())
    }
    fn fail(
        &self,
        mut record: GraphRunRecord,
        reason: String,
    ) -> Result<GraphRunRecord, GraphError> {
        record.status = RunStatus::Failed;
        record.error = Some(reason);
        self.store.save(&record)?;
        Ok(record)
    }

    fn fail_known_node(
        &self,
        mut record: GraphRunRecord,
        reason: String,
    ) -> Result<GraphRunRecord, GraphError> {
        // A known failed outcome cannot be resumed as the same invocation and
        // must never settle outgoing edges. Its started counters remain factual.
        record.cursor = None;
        self.fail(record, reason)
    }
}

impl GraphRunRecord {
    pub fn create(snapshot: GraphSnapshot, input: Value) -> Result<Self, GraphError> {
        snapshot.validate()?;
        let graph_digest = snapshot.digest()?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let serial = RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let run_id = format!("{}-{stamp:x}-{serial:x}", &graph_digest[..16]);
        let input = merge_values(&snapshot.input, &input, &Value::Null);
        Ok(Self {
            format: 2,
            run_id,
            graph_digest,
            snapshot,
            input,
            status: RunStatus::Ready,
            cursor: None,
            invocations: BTreeMap::new(),
            passes: BTreeMap::new(),
            module_activations: BTreeMap::new(),
            ceased: BTreeSet::new(),
            results: BTreeMap::new(),
            decided: BTreeMap::new(),
            sequence: 0,
            error: None,
        })
    }

    fn validate(&self) -> Result<(), GraphError> {
        validate_component(&self.run_id)?;
        if self.format != 2 {
            return Err(GraphError::CorruptRun(format!(
                "unsupported graph run format {}",
                self.format
            )));
        }
        self.snapshot.validate()?;
        if self.snapshot.digest()? != self.graph_digest {
            return Err(GraphError::CorruptRun("snapshot digest mismatch".into()));
        }
        match self.status {
            RunStatus::Ready if self.cursor.is_some() => {
                return Err(GraphError::CorruptRun(
                    "ready Run cannot have an active cursor".into(),
                ));
            }
            RunStatus::BudgetStopped if self.cursor.is_none() => {
                return Err(GraphError::CorruptRun(
                    "budget-stopped Run must retain its cursor".into(),
                ));
            }
            RunStatus::Completed if self.cursor.is_some() => {
                return Err(GraphError::CorruptRun(
                    "completed Run cannot have an active cursor".into(),
                ));
            }
            _ => {}
        }
        if let Some(cursor) = &self.cursor
            && (cursor.key.node_id != cursor.node_id
                || cursor.key.run_id != self.run_id
                || cursor.key.graph_digest != self.graph_digest
                || self.invocations.get(&cursor.node_id).copied() != Some(cursor.key.invocation)
                || self.passes.get(&cursor.node_id).copied().unwrap_or(0) == 0
                || !self
                    .snapshot
                    .nodes
                    .iter()
                    .any(|node| node.id == cursor.node_id))
        {
            return Err(GraphError::CorruptRun(
                "cursor identity does not match its Graph Run".into(),
            ));
        }

        let graph_nodes = self
            .snapshot
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut result_sequences = BTreeSet::new();
        let mut results_by_identity = BTreeMap::new();
        for (map_node_id, results) in &self.results {
            if !graph_nodes.contains(map_node_id.as_str()) {
                return Err(GraphError::CorruptRun(format!(
                    "results contain unknown node `{map_node_id}`"
                )));
            }
            let mut previous_invocation = 0;
            let mut previous_sequence = 0;
            for result in results {
                if result.node_id != *map_node_id
                    || result.key.node_id != *map_node_id
                    || result.key.run_id != self.run_id
                    || result.key.graph_digest != self.graph_digest
                    || result.key.invocation == 0
                    || result.key.invocation <= previous_invocation
                    || result.commit.node_id != *map_node_id
                    || result.commit.invocation != result.key.invocation
                    || result.commit.id.is_empty()
                    || result.sequence == 0
                    || result.sequence <= previous_sequence
                    || result.sequence > self.sequence
                    || !result_sequences.insert(result.sequence)
                {
                    return Err(GraphError::CorruptRun(format!(
                        "invalid result identity or sequence for node `{map_node_id}`"
                    )));
                }
                previous_invocation = result.key.invocation;
                previous_sequence = result.sequence;
                results_by_identity.insert(
                    (map_node_id.as_str(), result.key.invocation),
                    result.sequence,
                );
            }
        }

        for (node_id, invocation) in &self.invocations {
            if !graph_nodes.contains(node_id.as_str()) || *invocation == 0 {
                return Err(GraphError::CorruptRun(format!(
                    "invalid invocation counter for node `{node_id}`"
                )));
            }
            if self
                .results
                .get(node_id)
                .and_then(|node_results| node_results.last())
                .is_some_and(|result| result.key.invocation > *invocation)
            {
                return Err(GraphError::CorruptRun(format!(
                    "invocation counter trails results for node `{node_id}`"
                )));
            }
        }
        for (node_id, node_results) in &self.results {
            if let Some(last_result) = node_results.last()
                && self.invocations.get(node_id).copied().unwrap_or(0) < last_result.key.invocation
            {
                return Err(GraphError::CorruptRun(format!(
                    "invocation counter is missing or stale for node `{node_id}`"
                )));
            }
        }
        for (node_id, passes) in &self.passes {
            if !graph_nodes.contains(node_id.as_str()) || *passes == 0 {
                return Err(GraphError::CorruptRun(format!(
                    "invalid pass counter for node `{node_id}`"
                )));
            }
        }

        let graph_edges = self
            .snapshot
            .edges
            .iter()
            .map(|edge| edge_key(&edge.from_node, &edge.to_node))
            .collect::<BTreeSet<_>>();
        for (key, decision) in &self.decided {
            let Some((from, to)) = key.split_once('|') else {
                return Err(GraphError::CorruptRun(format!(
                    "invalid edge decision key `{key}`"
                )));
            };
            if key.matches('|').count() != 1 || !graph_edges.contains(key) {
                return Err(GraphError::CorruptRun(format!(
                    "edge decision `{key}` is not in the Graph snapshot"
                )));
            }
            if decision.sequence == 0 || decision.sequence > self.sequence {
                return Err(GraphError::CorruptRun(format!(
                    "edge decision `{key}` has an invalid sequence"
                )));
            }
            if decision.result_sequence > 0
                && results_by_identity
                    .get(&(from, decision.source_invocation))
                    .is_none_or(|result_sequence| decision.sequence <= *result_sequence)
            {
                return Err(GraphError::CorruptRun(format!(
                    "edge decision `{key}` is not newer than its source result"
                )));
            }
            match (
                decision.selected,
                decision.source_invocation,
                decision.result_sequence,
            ) {
                (true, source_invocation, result_sequence)
                    if source_invocation > 0
                        && result_sequence > 0
                        && results_by_identity.get(&(from, source_invocation))
                            == Some(&result_sequence) => {}
                (false, 0, 0) => {}
                (false, source_invocation, 0)
                    if source_invocation > 0
                        && self.invocations.get(from).copied().unwrap_or(0)
                            >= source_invocation => {}
                (false, source_invocation, result_sequence)
                    if source_invocation > 0
                        && result_sequence > 0
                        && results_by_identity.get(&(from, source_invocation))
                            == Some(&result_sequence) => {}
                _ => {
                    return Err(GraphError::CorruptRun(format!(
                        "edge decision `{key}` has an invalid source result reference"
                    )));
                }
            }
            let _ = to;
        }
        if let Some(cursor) = &self.cursor {
            let mut expected_input_commits = Vec::new();
            for edge in self
                .snapshot
                .edges
                .iter()
                .filter(|edge| edge.to_node == cursor.node_id)
            {
                let Some(decision) = self.decided.get(&edge_key(&edge.from_node, &edge.to_node))
                else {
                    continue;
                };
                if !decision.selected {
                    continue;
                }
                let result = self
                    .results
                    .get(&edge.from_node)
                    .and_then(|results| {
                        results.iter().find(|result| {
                            result.key.invocation == decision.source_invocation
                                && result.sequence == decision.result_sequence
                        })
                    })
                    .ok_or_else(|| {
                        GraphError::CorruptRun(format!(
                            "cursor input edge {} -> {} has no source result",
                            edge.from_node, edge.to_node
                        ))
                    })?;
                expected_input_commits.push(result.commit.clone());
            }
            if cursor.input_commits != expected_input_commits {
                return Err(GraphError::CorruptRun(format!(
                    "cursor input commits do not match selected inputs for `{}`",
                    cursor.node_id
                )));
            }
        }
        Ok(())
    }

    fn migrate_format(&mut self) -> Result<(), GraphError> {
        match self.format {
            1 => {
                self.snapshot.validate()?;
                if self.snapshot.digest()? != self.graph_digest {
                    return Err(GraphError::CorruptRun("snapshot digest mismatch".into()));
                }
                if let Some(cursor) = &self.cursor {
                    let previous = self.invocations.get(&cursor.node_id).copied().unwrap_or(0);
                    if cursor.key.node_id != cursor.node_id
                        || cursor.key.run_id != self.run_id
                        || cursor.key.graph_digest != self.graph_digest
                        || previous.checked_add(1) != Some(cursor.key.invocation)
                        || !self
                            .snapshot
                            .nodes
                            .iter()
                            .any(|node| node.id == cursor.node_id)
                    {
                        return Err(GraphError::CorruptRun(
                            "legacy cursor identity does not match its Graph Run".into(),
                        ));
                    }
                    self.invocations
                        .insert(cursor.node_id.clone(), cursor.key.invocation);
                    *self.passes.entry(cursor.node_id.clone()).or_default() += 1;
                }
                for (edge_key_value, decision) in &mut self.decided {
                    if decision.source_invocation == 0 || decision.result_sequence != 0 {
                        continue;
                    }
                    let Some((from, _)) = edge_key_value.split_once('|') else {
                        continue;
                    };
                    if let Some(result) = self.results.get(from).and_then(|results| {
                        results
                            .iter()
                            .find(|result| result.key.invocation == decision.source_invocation)
                    }) {
                        decision.result_sequence = result.sequence;
                    }
                }
                self.format = 2;
                Ok(())
            }
            2 => Ok(()),
            other => Err(GraphError::CorruptRun(format!(
                "unsupported graph run format {other}"
            ))),
        }
    }
}

fn next_node(record: &GraphRunRecord) -> Result<Option<GraphNode>, GraphError> {
    let back_edges = structural_back_edges(&record.snapshot);
    for node in &record.snapshot.nodes {
        if record.ceased.contains(&node.id)
            || node
                .max_rounds
                .is_some_and(|limit| record.ceased.contains(&format!("{}@{limit}", node.id)))
        {
            continue;
        }
        let scope = scope_of(&node.id);
        if record
            .snapshot
            .module_rounds
            .get(&scope)
            .is_some_and(|limit| record.ceased.contains(&format!("{scope}@{limit}")))
        {
            continue;
        }
        if node.id == record.snapshot.entry
            && record.results.get(&node.id).is_none_or(Vec::is_empty)
        {
            return Ok(Some(node.clone()));
        }
        let incoming = record
            .snapshot
            .edges
            .iter()
            .filter(|e| e.to_node == node.id)
            .collect::<Vec<_>>();
        if incoming.is_empty() {
            continue;
        }
        let all_decided = incoming.iter().all(|e| {
            record
                .decided
                .contains_key(&edge_key(&e.from_node, &e.to_node))
                || (back_edges.contains(&(e.from_node.as_str(), e.to_node.as_str()))
                    && !record.invocations.contains_key(&e.from_node))
        });
        if !all_decided {
            continue;
        }
        let chosen = incoming
            .iter()
            .filter(|e| {
                record
                    .decided
                    .get(&edge_key(&e.from_node, &e.to_node))
                    .is_some_and(|d| d.selected)
            })
            .collect::<Vec<_>>();
        if chosen.is_empty() {
            continue;
        }
        let latest_input = chosen
            .iter()
            .filter_map(|e| {
                record
                    .decided
                    .get(&edge_key(&e.from_node, &e.to_node))
                    .map(|d| d.sequence)
            })
            .max()
            .unwrap_or(0);
        let completed = record
            .results
            .get(&node.id)
            .and_then(|v| v.last())
            .map(|r| r.key.invocation);
        // A selected back-edge can re-enter after its source advances; ordinary selected edges only execute once.
        let has_new = chosen.iter().any(|e| {
            record
                .decided
                .get(&edge_key(&e.from_node, &e.to_node))
                .is_some_and(|d| {
                    d.sequence
                        > record
                            .results
                            .get(&node.id)
                            .and_then(|v| v.last())
                            .map(|r| r.sequence)
                            .unwrap_or(0)
                })
        });
        if completed.is_none() || (latest_input > 0 && has_new) {
            return Ok(Some(node.clone()));
        }
    }
    Ok(record.cursor.as_ref().and_then(|c| {
        record
            .snapshot
            .nodes
            .iter()
            .find(|n| n.id == c.node_id)
            .cloned()
    }))
}

/// Persist false decisions through nodes that were structurally skipped because
/// every incoming route was explicitly unselected. A previously-run node is
/// inactive again only when it receives a newer false input than its latest
/// result; this retires stale outgoing selections on later loop rounds.
fn propagate_inactive_edges(record: &mut GraphRunRecord) -> Result<bool, GraphError> {
    if record.cursor.is_some() {
        return Ok(false);
    }

    let mut changed = false;
    loop {
        let mut pass_changed = false;
        // First close provably inactive cycles as units. Internal back-edges
        // cannot become false one node at a time because each waits for the
        // other; SCC condensation gives us the correct finite fixed point.
        let candidates = record
            .snapshot
            .nodes
            .iter()
            .filter(|node| {
                node.id != record.snapshot.entry
                    && !record.invocations.contains_key(&node.id)
                    && record
                        .cursor
                        .as_ref()
                        .is_none_or(|cursor| cursor.node_id != node.id)
            })
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();
        let components = strongly_connected_components(&record.snapshot, &candidates);
        let mut inactive_components = BTreeSet::<usize>::new();
        loop {
            let mut component_changed = false;
            for (component_index, component) in components.iter().enumerate() {
                if inactive_components.contains(&component_index) {
                    continue;
                }
                let external_incoming = record
                    .snapshot
                    .edges
                    .iter()
                    .filter(|edge| component.contains(&edge.to_node))
                    .filter(|edge| !component.contains(&edge.from_node))
                    .collect::<Vec<_>>();
                let all_inactive = external_incoming.iter().all(|edge| {
                    let source_component = components
                        .iter()
                        .position(|candidate| candidate.contains(&edge.from_node));
                    if source_component.is_some_and(|index| inactive_components.contains(&index)) {
                        return true;
                    }
                    record
                        .decided
                        .get(&edge_key(&edge.from_node, &edge.to_node))
                        .is_some_and(|decision| !decision.selected)
                });
                if !all_inactive {
                    continue;
                }
                inactive_components.insert(component_index);
                component_changed = true;
                let component_edges = record
                    .snapshot
                    .edges
                    .iter()
                    .filter(|edge| component.contains(&edge.from_node))
                    .map(|edge| (edge.from_node.clone(), edge.to_node.clone()))
                    .collect::<Vec<_>>();
                for (from, to) in component_edges {
                    let key = edge_key(&from, &to);
                    if record.decided.get(&key).is_some_and(|decision| {
                        !decision.selected && decision.source_invocation == 0
                    }) {
                        continue;
                    }
                    record.sequence += 1;
                    record.decided.insert(
                        key,
                        EdgeDecision {
                            selected: false,
                            sequence: record.sequence,
                            source_invocation: 0,
                            result_sequence: 0,
                        },
                    );
                    pass_changed = true;
                    changed = true;
                }
            }
            if !component_changed {
                break;
            }
        }
        for node in &record.snapshot.nodes {
            if node.id == record.snapshot.entry
                || record.cursor.as_ref().is_some_and(|c| c.node_id == node.id)
            {
                continue;
            }
            let incoming = record
                .snapshot
                .edges
                .iter()
                .filter(|edge| edge.to_node == node.id)
                .collect::<Vec<_>>();
            if incoming.is_empty()
                || incoming.iter().any(|edge| {
                    record
                        .decided
                        .get(&edge_key(&edge.from_node, &edge.to_node))
                        .is_none_or(|decision| decision.selected)
                })
            {
                continue;
            }

            let latest_result_sequence = record
                .results
                .get(&node.id)
                .and_then(|results| results.last())
                .map(|result| result.sequence);
            let latest_input_sequence = incoming
                .iter()
                .filter_map(|edge| {
                    record
                        .decided
                        .get(&edge_key(&edge.from_node, &edge.to_node))
                        .map(|decision| decision.sequence)
                })
                .max()
                .unwrap_or(0);
            if latest_result_sequence.is_some_and(|result| latest_input_sequence <= result) {
                continue;
            }

            let outgoing = record
                .snapshot
                .edges
                .iter()
                .filter(|edge| edge.from_node == node.id)
                .map(|edge| edge.to_node.clone())
                .collect::<Vec<_>>();
            for target in outgoing {
                let key = edge_key(&node.id, &target);
                if record.decided.get(&key).is_some_and(|decision| {
                    !decision.selected
                        && latest_result_sequence
                            .is_none_or(|result_sequence| decision.sequence > result_sequence)
                }) {
                    continue;
                }
                record.sequence += 1;
                record.decided.insert(
                    key,
                    EdgeDecision {
                        selected: false,
                        sequence: record.sequence,
                        source_invocation: 0,
                        result_sequence: 0,
                    },
                );
                pass_changed = true;
                changed = true;
            }
        }
        if !pass_changed {
            return Ok(changed);
        }
    }
}

fn strongly_connected_components(
    snapshot: &GraphSnapshot,
    candidates: &BTreeSet<String>,
) -> Vec<BTreeSet<String>> {
    fn visit(
        node: &str,
        adjacency: &BTreeMap<String, Vec<String>>,
        visited: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if !visited.insert(node.to_owned()) {
            return;
        }
        if let Some(neighbors) = adjacency.get(node) {
            for neighbor in neighbors {
                visit(neighbor, adjacency, visited, order);
            }
        }
        order.push(node.to_owned());
    }
    fn collect(
        node: &str,
        adjacency: &BTreeMap<String, Vec<String>>,
        component: &mut BTreeSet<String>,
    ) {
        if !component.insert(node.to_owned()) {
            return;
        }
        if let Some(neighbors) = adjacency.get(node) {
            for neighbor in neighbors {
                collect(neighbor, adjacency, component);
            }
        }
    }

    let mut forward = candidates
        .iter()
        .map(|node| (node.clone(), Vec::new()))
        .collect::<BTreeMap<_, _>>();
    let mut reverse = forward.clone();
    for edge in &snapshot.edges {
        if candidates.contains(&edge.from_node) && candidates.contains(&edge.to_node) {
            forward
                .get_mut(&edge.from_node)
                .expect("candidate adjacency")
                .push(edge.to_node.clone());
            reverse
                .get_mut(&edge.to_node)
                .expect("candidate reverse adjacency")
                .push(edge.from_node.clone());
        }
    }
    let mut order = Vec::new();
    let mut visited = BTreeSet::new();
    for node in candidates {
        visit(node, &forward, &mut visited, &mut order);
    }
    visited.clear();
    let mut components = Vec::new();
    while let Some(node) = order.pop() {
        if visited.contains(&node) {
            continue;
        }
        let mut component = BTreeSet::new();
        collect(&node, &reverse, &mut component);
        visited.extend(component.iter().cloned());
        components.push(component);
    }
    components
}

fn refuse_module_activation(
    record: &mut GraphRunRecord,
    scope: &str,
    entry: &str,
) -> Result<(), GraphError> {
    let prefix = format!("{scope}/");
    // A denied activation invalidates the incoming trigger for this round.
    // Keep prior commits/results as history, while making the latest edge facts
    // show that this attempt did not enter or traverse the module.
    let prior_result_sequence = record
        .results
        .get(entry)
        .and_then(|results| results.last())
        .map(|result| result.sequence)
        .unwrap_or(0);
    let trigger = record
        .snapshot
        .edges
        .iter()
        .filter(|edge| edge.to_node == entry && !edge.from_node.starts_with(&prefix))
        .filter_map(|edge| {
            record
                .decided
                .get(&edge_key(&edge.from_node, entry))
                .filter(|decision| decision.selected && decision.sequence > prior_result_sequence)
                .map(|decision| (edge.from_node.clone(), decision.sequence))
        })
        .max_by_key(|(_, sequence)| *sequence)
        .map(|(source, _)| source);
    if let Some(source) = trigger {
        record.sequence += 1;
        record.decided.insert(
            edge_key(&source, entry),
            EdgeDecision {
                selected: false,
                sequence: record.sequence,
                source_invocation: 0,
                result_sequence: 0,
            },
        );
    }

    let members = record
        .snapshot
        .nodes
        .iter()
        .filter(|node| node.id.starts_with(&prefix))
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let outgoing = record
        .snapshot
        .edges
        .iter()
        .filter(|edge| members.contains(&edge.from_node))
        .map(|edge| (edge.from_node.clone(), edge.to_node.clone()))
        .collect::<Vec<_>>();
    for (from, to) in outgoing {
        record.sequence += 1;
        record.decided.insert(
            edge_key(&from, &to),
            EdgeDecision {
                selected: false,
                sequence: record.sequence,
                source_invocation: 0,
                result_sequence: 0,
            },
        );
    }
    Ok(())
}

fn structural_back_edges(snapshot: &GraphSnapshot) -> BTreeSet<(&str, &str)> {
    let mut color: BTreeMap<&str, u8> = BTreeMap::new();
    let mut found = BTreeSet::new();
    let mut stack = vec![(snapshot.entry.as_str(), 0usize)];
    color.insert(snapshot.entry.as_str(), 1);
    while let Some((node, index)) = stack.pop() {
        let outgoing = snapshot
            .edges
            .iter()
            .filter(|e| e.from_node == node)
            .collect::<Vec<_>>();
        if index >= outgoing.len() {
            color.insert(node, 2);
            continue;
        }
        stack.push((node, index + 1));
        let target = outgoing[index].to_node.as_str();
        match color.get(target).copied() {
            Some(1) => {
                found.insert((node, target));
            }
            None => {
                color.insert(target, 1);
                stack.push((target, 0));
            }
            _ => {}
        }
    }
    found
}

fn select_route(c: &NodeCompletion, routes: &[String]) -> Result<Option<String>, GraphError> {
    if let Some(route) = &c.route
        && !routes.iter().any(|allowed| allowed == route)
    {
        return Err(GraphError::InvalidRoute(format!(
            "route `{route}` is not an exit"
        )));
    }
    match routes.len() {
        0 => Ok(None),
        1 => Ok(Some(routes[0].clone())),
        _ => {
            let route = c
                .route
                .as_ref()
                .ok_or_else(|| GraphError::InvalidRoute("multiple exits require route".into()))?;
            Ok(Some(route.clone()))
        }
    }
}
fn edge_key(a: &str, b: &str) -> String {
    format!("{a}|{b}")
}
fn merge_values(default: &Value, override_value: &Value, node_value: &Value) -> Value {
    fn merge(base: &Value, over: &Value) -> Value {
        match (base, over) {
            (Value::Object(a), Value::Object(b)) => {
                let mut out = a.clone();
                for (k, v) in b {
                    out.insert(
                        k.clone(),
                        if out.get(k).is_some_and(Value::is_object) && v.is_object() {
                            merge(&out[k], v)
                        } else {
                            v.clone()
                        },
                    );
                }
                Value::Object(out)
            }
            (_, Value::Null) => base.clone(),
            (_, v) => v.clone(),
        }
    }
    merge(&merge(default, override_value), node_value)
}
fn node_task(
    objective: &str,
    instructions: &str,
    local_instruction: &str,
    input: &Value,
) -> String {
    format!(
        "Objective:\n{objective}\n\nInstructions:\n{instructions}{}\n\nInput:\n{}",
        if local_instruction.is_empty() {
            String::new()
        } else {
            format!("\n\nNode instructions:\n{local_instruction}")
        },
        input
    )
}
fn scope_of(node_id: &str) -> String {
    node_id
        .rsplit_once('/')
        .map(|(scope, _)| scope.to_owned())
        .unwrap_or_default()
}
fn validate_component(s: &str) -> Result<(), GraphError> {
    if s.is_empty()
        || s == "."
        || s == ".."
        || s.chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        Err(GraphError::InvalidRunId(s.to_owned()))
    } else {
        Ok(())
    }
}
fn validate_node_id(s: &str) -> Result<(), GraphError> {
    if s.is_empty()
        || s.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part == ".graph-calls"
                || part
                    .chars()
                    .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        })
    {
        Err(GraphError::InvalidSnapshot(format!("unsafe node id `{s}`")))
    } else {
        Ok(())
    }
}
fn validate_wall_time(value: Option<f64>, node_id: &str) -> Result<(), GraphError> {
    if value.is_some_and(|seconds| !seconds.is_finite() || seconds <= 0.0) {
        return Err(GraphError::InvalidSnapshot(format!(
            "node `{node_id}` wall_time_limit_seconds must be positive and finite"
        )));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("invalid graph snapshot: {0}")]
    InvalidSnapshot(String),
    #[error("unsupported graph capability: {0}")]
    Unsupported(String),
    #[error("snapshot decode failed: {0}")]
    SnapshotDecode(serde_json::Error),
    #[error("run decode failed: {0}")]
    RunDecode(serde_json::Error),
    #[error("run store I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("corrupt graph run: {0}")]
    CorruptRun(String),
    #[error("run ID is not a safe file name: {0}")]
    InvalidRunId(String),
    #[error("run record conflict: supplied state differs from durable state")]
    RunConflict,
    #[error("run `{0}` already has an active writer")]
    RunBusy(String),
    #[error("invalid node route: {0}")]
    InvalidRoute(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::BufRead,
        process::{Command, Stdio},
        sync::{Arc, Mutex},
    };

    #[derive(Default)]
    struct MemStore {
        record: Mutex<Option<GraphRunRecord>>,
        saves: Mutex<usize>,
        fail_on: Mutex<Option<usize>>,
    }
    struct MemLease;
    impl RunLease for MemLease {}
    impl RunStore for MemStore {
        fn load(&self, id: &str) -> Result<Option<GraphRunRecord>, GraphError> {
            Ok(self
                .record
                .lock()
                .unwrap()
                .clone()
                .filter(|r| r.run_id == id))
        }
        fn save(&self, r: &GraphRunRecord) -> Result<(), GraphError> {
            let mut n = self.saves.lock().unwrap();
            *n += 1;
            if *self.fail_on.lock().unwrap() == Some(*n) {
                return Err(GraphError::CorruptRun("injected save failure".into()));
            }
            *self.record.lock().unwrap() = Some(r.clone());
            Ok(())
        }
        fn acquire_lease(&self, _: &str) -> Result<Box<dyn RunLease>, GraphError> {
            Ok(Box::new(MemLease))
        }
    }
    struct PersistThenFailStore {
        inner: FileRunStore,
        fail_after_result_commit: Mutex<bool>,
    }
    impl RunStore for PersistThenFailStore {
        fn load(&self, id: &str) -> Result<Option<GraphRunRecord>, GraphError> {
            self.inner.load(id)
        }
        fn save(&self, record: &GraphRunRecord) -> Result<(), GraphError> {
            self.inner.save(record)?;
            let committed_result = record.cursor.is_none()
                && record.status == RunStatus::Running
                && record.results.values().any(|results| !results.is_empty());
            if committed_result {
                let mut fail = self.fail_after_result_commit.lock().unwrap();
                if *fail {
                    *fail = false;
                    return Err(GraphError::CorruptRun(
                        "injected feedback failure after durable Run save".into(),
                    ));
                }
            }
            Ok(())
        }
        fn acquire_lease(&self, id: &str) -> Result<Box<dyn RunLease>, GraphError> {
            self.inner.acquire_lease(id)
        }
    }
    #[derive(Default)]
    struct MemoryArtifacts {
        values: Mutex<BTreeMap<String, Value>>,
        freezes: Mutex<usize>,
    }
    impl ArtifactPort for MemoryArtifacts {
        fn freeze<'a>(
            &'a self,
            key: &'a InvocationKey,
            c: &'a NodeCompletion,
        ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
            Box::pin(async move {
                let id = key.durable_key();
                let mut values = self.values.lock().unwrap();
                if let Some(existing) = values.get(&id) {
                    if existing != &c.output {
                        return Err(GraphError::CorruptRun("conflicting freeze retry".into()));
                    }
                } else {
                    values.insert(id.clone(), c.output.clone());
                    *self.freezes.lock().unwrap() += 1;
                }
                Ok(CommitRef {
                    id,
                    node_id: key.node_id.clone(),
                    invocation: key.invocation,
                })
            })
        }
        fn resolve<'a>(
            &'a self,
            c: &'a CommitRef,
        ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
            Box::pin(async move {
                self.values
                    .lock()
                    .unwrap()
                    .get(&c.id)
                    .cloned()
                    .ok_or_else(|| GraphError::CorruptRun("missing commit".into()))
            })
        }
    }
    #[derive(Default)]
    struct FakeNodes {
        facts: Mutex<BTreeMap<String, CompletionFact>>,
        calls: Mutex<Vec<NodeExecutionRequest>>,
        budget_once: Mutex<bool>,
        uncertain: Mutex<bool>,
        caps_budget_off: bool,
        cancel_once: Mutex<bool>,
        op_run: bool,
        fail_once: Mutex<Option<String>>,
        invalid_route_once: Mutex<bool>,
    }
    impl NodeExecutionPort for FakeNodes {
        fn capabilities(&self) -> NodeExecutionCapabilities {
            NodeExecutionCapabilities {
                agent: true,
                op_run: self.op_run,
                exact_provider_request_budget: !self.caps_budget_off,
            }
        }
        fn completion_fact<'a>(
            &'a self,
            key: &'a InvocationKey,
        ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
            Box::pin(async move {
                if *self.uncertain.lock().unwrap() {
                    return Ok(CompletionFact::Uncertain("fake unknown".into()));
                }
                Ok(self
                    .facts
                    .lock()
                    .unwrap()
                    .get(&key.durable_key())
                    .cloned()
                    .unwrap_or(CompletionFact::NotStarted))
            })
        }
        fn execute<'a>(
            &'a self,
            request: NodeExecutionRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>>
        {
            Box::pin(async move {
                if self
                    .facts
                    .lock()
                    .unwrap()
                    .contains_key(&request.key.durable_key())
                {
                    return Err(GraphError::CorruptRun(
                        "duplicate dispatch after completion fact".into(),
                    ));
                }
                self.calls.lock().unwrap().push(request.clone());
                if let Some(reason) = self.fail_once.lock().unwrap().take() {
                    self.facts.lock().unwrap().insert(
                        request.key.durable_key(),
                        CompletionFact::Failed(reason.clone()),
                    );
                    return Ok(NodeExecutionOutcome::Failed { reason });
                }
                {
                    let mut budget = self.budget_once.lock().unwrap();
                    if *budget {
                        *budget = false;
                        return Ok(NodeExecutionOutcome::BudgetExhausted { model_requests: 2 });
                    }
                }
                {
                    let mut cancel = self.cancel_once.lock().unwrap();
                    if *cancel {
                        *cancel = false;
                        request.cancellation.store(true, Ordering::Relaxed);
                        return Ok(NodeExecutionOutcome::Cancelled);
                    }
                }
                let route = {
                    let mut invalid = self.invalid_route_once.lock().unwrap();
                    if *invalid {
                        *invalid = false;
                        Some("unlisted-route".into())
                    } else {
                        request.routes.first().cloned()
                    }
                };
                let completion = NodeCompletion {
                    submission: format!("done:{}", request.key.node_id),
                    route,
                    model_requests: 1,
                    output: serde_json::json!({"node":request.key.node_id,"input":request.input}),
                };
                self.facts.lock().unwrap().insert(
                    request.key.durable_key(),
                    CompletionFact::Completed(completion.clone()),
                );
                Ok(NodeExecutionOutcome::Completed(completion))
            })
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum CrashPoint {
        NotStartedFactCheck,
        NodeFact,
        FailedFact,
        ArtifactCommit,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct DurableTestArtifact {
        commit: CommitRef,
        output: Value,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    enum DurableTestNodeFact {
        Completed(NodeCompletion),
        Failed(String),
    }

    struct DurableTestPorts {
        root: PathBuf,
        crash_point: Option<CrashPoint>,
    }

    impl DurableTestPorts {
        fn node_fact_path(&self, key: &InvocationKey) -> PathBuf {
            self.root.join(format!("node-{}.json", key.durable_key()))
        }

        fn artifact_path(&self, key: &InvocationKey) -> PathBuf {
            self.root
                .join(format!("artifact-{}.json", key.durable_key()))
        }

        fn execute_count_path(&self) -> PathBuf {
            self.root.join("execute-count")
        }

        fn freeze_count_path(&self) -> PathBuf {
            self.root.join("freeze-count")
        }
    }

    impl NodeExecutionPort for DurableTestPorts {
        fn capabilities(&self) -> NodeExecutionCapabilities {
            NodeExecutionCapabilities {
                agent: true,
                op_run: false,
                exact_provider_request_budget: true,
            }
        }

        fn completion_fact<'a>(
            &'a self,
            key: &'a InvocationKey,
        ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
            Box::pin(async move {
                let path = self.node_fact_path(key);
                match fs::read(path) {
                    Ok(bytes) => {
                        let fact: DurableTestNodeFact =
                            serde_json::from_slice(&bytes).map_err(GraphError::RunDecode)?;
                        match fact {
                            DurableTestNodeFact::Completed(completion) => {
                                Ok(CompletionFact::Completed(completion))
                            }
                            DurableTestNodeFact::Failed(reason) => {
                                Ok(CompletionFact::Failed(reason))
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        if self.crash_point == Some(CrashPoint::NotStartedFactCheck) {
                            std::process::exit(70);
                        }
                        Ok(CompletionFact::NotStarted)
                    }
                    Err(error) => Err(error.into()),
                }
            })
        }

        fn execute<'a>(
            &'a self,
            request: NodeExecutionRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>>
        {
            Box::pin(async move {
                if self.node_fact_path(&request.key).exists() {
                    return Err(GraphError::CorruptRun(
                        "durable test Node port received a duplicate dispatch".into(),
                    ));
                }
                write_durable_bytes(&self.execute_count_path(), b"1")?;
                if self.crash_point == Some(CrashPoint::FailedFact) {
                    let reason = "durable test failure".to_owned();
                    write_durable_json(
                        &self.node_fact_path(&request.key),
                        &DurableTestNodeFact::Failed(reason),
                    )?;
                    std::process::exit(73);
                }
                let completion = NodeCompletion {
                    submission: format!("completed:{}", request.key.node_id),
                    route: request.routes.first().cloned(),
                    model_requests: 1,
                    output: serde_json::json!({"node":request.key.node_id,"input":request.input}),
                };
                write_durable_json(
                    &self.node_fact_path(&request.key),
                    &DurableTestNodeFact::Completed(completion.clone()),
                )?;
                if self.crash_point == Some(CrashPoint::NodeFact) {
                    std::process::exit(71);
                }
                Ok(NodeExecutionOutcome::Completed(completion))
            })
        }
    }

    impl ArtifactPort for DurableTestPorts {
        fn freeze<'a>(
            &'a self,
            key: &'a InvocationKey,
            completion: &'a NodeCompletion,
        ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
            Box::pin(async move {
                let path = self.artifact_path(key);
                let artifact = match fs::read(&path) {
                    Ok(bytes) => {
                        let existing: DurableTestArtifact =
                            serde_json::from_slice(&bytes).map_err(GraphError::RunDecode)?;
                        if existing.output != completion.output {
                            return Err(GraphError::CorruptRun(
                                "artifact retry changed the frozen output".into(),
                            ));
                        }
                        existing
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        let artifact = DurableTestArtifact {
                            commit: CommitRef {
                                id: key.durable_key(),
                                node_id: key.node_id.clone(),
                                invocation: key.invocation,
                            },
                            output: completion.output.clone(),
                        };
                        write_durable_json(&path, &artifact)?;
                        write_durable_bytes(&self.freeze_count_path(), b"1")?;
                        artifact
                    }
                    Err(error) => return Err(error.into()),
                };
                if self.crash_point == Some(CrashPoint::ArtifactCommit) {
                    std::process::exit(72);
                }
                Ok(artifact.commit)
            })
        }

        fn resolve<'a>(
            &'a self,
            commit: &'a CommitRef,
        ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
            Box::pin(async move {
                let path = self.root.join(format!("artifact-{}.json", commit.id));
                let bytes = fs::read(path)?;
                let artifact: DurableTestArtifact =
                    serde_json::from_slice(&bytes).map_err(GraphError::RunDecode)?;
                if artifact.commit != *commit {
                    return Err(GraphError::CorruptRun(
                        "test artifact commit identity mismatch".into(),
                    ));
                }
                Ok(artifact.output)
            })
        }
    }

    fn write_durable_json(
        path: &std::path::Path,
        value: &impl Serialize,
    ) -> Result<(), GraphError> {
        let bytes = serde_json::to_vec(value).map_err(GraphError::RunDecode)?;
        write_durable_bytes(path, &bytes)
    }

    fn write_durable_bytes(path: &std::path::Path, bytes: &[u8]) -> Result<(), GraphError> {
        let parent = path
            .parent()
            .ok_or_else(|| GraphError::CorruptRun("test durable path has no parent".into()))?;
        fs::create_dir_all(parent)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| GraphError::CorruptRun("test durable path is invalid".into()))?;
        let temporary = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), stamp));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    }

    struct Control {
        pause: bool,
        stop: bool,
        token: crate::Cancellation,
    }
    impl Default for Control {
        fn default() -> Self {
            Self {
                pause: false,
                stop: false,
                token: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }
        }
    }
    impl RunControl for Control {
        fn pause_requested(&self) -> bool {
            self.pause
        }
        fn stop_requested(&self) -> bool {
            self.stop
        }
        fn cancellation(&self) -> crate::Cancellation {
            self.token.clone()
        }
    }
    fn graph(nodes: &[&str], edges: &[(&str, &str)], entry: &str) -> GraphSnapshot {
        let agents = BTreeMap::from([(
            "worker".to_string(),
            AgentDefinition {
                model: "m".into(),
                instructions: "base".into(),
                network: false,
                max_steps: None,
                wall_time_limit_seconds: None,
                reads: vec![],
                writes: vec![],
            },
        )]);
        GraphSnapshot {
            objective: "objective".into(),
            input: serde_json::json!({"defaults":{"a":1,"b":1}}),
            entry: entry.into(),
            agents,
            ops: BTreeMap::new(),
            nodes: nodes
                .iter()
                .map(|id| GraphNode {
                    id: (*id).into(),
                    agent: Some("worker".into()),
                    op: None,
                    input: None,
                    plugins: vec![],
                    max_rounds: None,
                })
                .collect(),
            edges: edges
                .iter()
                .map(|(a, b)| GraphEdge {
                    from_node: (*a).into(),
                    to_node: (*b).into(),
                })
                .collect(),
            module_rounds: BTreeMap::new(),
        }
    }
    fn setup() -> (MemStore, MemoryArtifacts, FakeNodes, Control) {
        (
            MemStore::default(),
            MemoryArtifacts::default(),
            FakeNodes::default(),
            Control::default(),
        )
    }

    #[tokio::test]
    async fn serial_routing_skips_unselected_and_passes_fixed_commit_input() {
        let mut g = graph(
            &["start", "left", "right"],
            &[("start", "left"), ("start", "right")],
            "start",
        );
        let worker = g.agents.get_mut("worker").unwrap();
        worker.network = true;
        worker.wall_time_limit_seconds = Some(45.0);
        g.nodes[1].input = Some(Value::String("review left".into()));
        let (s, a, n, c) = setup();
        let record =
            GraphRunRecord::create(g, serde_json::json!({"defaults":{"b":2},"request":"x"}))
                .unwrap();
        let runner = GraphRunner::new(&s, &a, &n, &c);
        let done = runner.run(record).await.unwrap();
        assert_eq!(done.status, RunStatus::Completed);
        assert_eq!(
            n.calls
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.key.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["start", "left"]
        );
        let next = &n.calls.lock().unwrap()[1];
        assert_eq!(next.input_commits.len(), 1);
        assert_eq!(next.input["input"]["defaults"]["a"], 1);
        assert_eq!(next.input["input"]["defaults"]["b"], 2);
        assert_eq!(next.input["input"]["request"], "x");
        assert!(next.task.contains("review left"));
        assert_eq!(next.model.as_deref(), Some("m"));
        assert!(next.network);
        assert_eq!(next.wall_time_limit_seconds, Some(45.0));
        assert_eq!(done.graph_digest, done.snapshot.digest().unwrap());
    }

    #[tokio::test]
    async fn skipped_diamond_branch_propagates_false_edge_and_releases_join() {
        let snapshot = graph(
            &["start", "left", "right", "merge"],
            &[
                ("start", "left"),
                ("start", "right"),
                ("left", "merge"),
                ("right", "merge"),
            ],
            "start",
        );
        let (store, artifacts, nodes, control) = setup();
        let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
            .await
            .unwrap();

        assert_eq!(result.status, RunStatus::Completed);
        let executed: Vec<String> = nodes
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.key.node_id.clone())
            .collect();
        assert_eq!(executed, vec!["start", "left", "merge"]);
        assert!(!result.decided["start|right"].selected);
        let propagated = &result.decided["right|merge"];
        assert!(!propagated.selected);
        assert_eq!(propagated.source_invocation, 0);
        assert_eq!(propagated.result_sequence, 0);
        assert!(!result.invocations.contains_key("right"));
    }

    #[tokio::test]
    async fn skipped_multi_level_chain_propagates_to_join_without_running_nodes() {
        let snapshot = graph(
            &["start", "live", "skip-a", "skip-b", "join"],
            &[
                ("start", "live"),
                ("start", "skip-a"),
                ("live", "join"),
                ("skip-a", "skip-b"),
                ("skip-b", "join"),
            ],
            "start",
        );
        let (store, artifacts, nodes, control) = setup();
        let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
            .await
            .unwrap();

        assert_eq!(result.status, RunStatus::Completed);
        let executed: Vec<String> = nodes
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.key.node_id.clone())
            .collect();
        assert_eq!(executed, vec!["start", "live", "join"]);
        assert_eq!(result.decided["start|skip-a"].source_invocation, 1);
        for edge in ["skip-a|skip-b", "skip-b|join"] {
            let decision = &result.decided[edge];
            assert!(!decision.selected, "{edge}");
            assert_eq!(decision.source_invocation, 0, "{edge}");
            assert_eq!(decision.result_sequence, 0, "{edge}");
        }
        for node in ["skip-a", "skip-b"] {
            assert!(!result.invocations.contains_key(node));
            assert!(!result.passes.contains_key(node));
        }
    }

    #[tokio::test]
    async fn inactive_unstarted_cycle_closes_and_releases_selected_merge() {
        let snapshot = graph(
            &["start", "live", "cycle_a", "cycle_b", "merge"],
            &[
                ("start", "live"),
                ("start", "cycle_a"),
                ("cycle_a", "cycle_b"),
                ("cycle_b", "cycle_a"),
                ("cycle_b", "merge"),
                ("live", "merge"),
            ],
            "start",
        );
        let (store, artifacts, nodes, control) = setup();
        let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
            .await
            .unwrap();

        assert_eq!(result.status, RunStatus::Completed);
        let executed: Vec<String> = nodes
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.key.node_id.clone())
            .collect();
        assert_eq!(executed, vec!["start", "live", "merge"]);
        assert_eq!(result.decided["start|cycle_a"].source_invocation, 1);
        for edge in ["cycle_a|cycle_b", "cycle_b|cycle_a", "cycle_b|merge"] {
            let decision = &result.decided[edge];
            assert!(!decision.selected, "{edge}");
            assert_eq!(decision.source_invocation, 0, "{edge}");
            assert_eq!(decision.result_sequence, 0, "{edge}");
        }
        assert!(result.decided["live|merge"].selected);
    }

    #[test]
    fn inactive_scc_waits_for_external_incoming_decision() {
        let snapshot = graph(
            &["entry", "source", "cycle_a", "cycle_b"],
            &[
                ("entry", "source"),
                ("source", "cycle_a"),
                ("cycle_a", "cycle_b"),
                ("cycle_b", "cycle_a"),
            ],
            "entry",
        );
        let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
        record.invocations.insert("entry".into(), 1);
        record.invocations.insert("source".into(), 1);
        record.passes.insert("entry".into(), 1);
        record.passes.insert("source".into(), 1);
        record.decided.insert(
            "entry|source".into(),
            EdgeDecision {
                selected: true,
                sequence: 1,
                source_invocation: 1,
                result_sequence: 1,
            },
        );

        assert!(!propagate_inactive_edges(&mut record).unwrap());
        assert_eq!(record.decided.len(), 1);
        assert!(!record.decided.contains_key("source|cycle_a"));
        assert!(!record.decided.contains_key("cycle_a|cycle_b"));
        assert!(!record.decided.contains_key("cycle_b|cycle_a"));
    }

    #[test]
    fn false_ingress_closes_cascaded_sccs_deterministically() {
        // Deliberately list downstream components first. Closure must converge
        // independently of graph storage order and produce stable sequences.
        let snapshot = graph(
            &[
                "entry", "join", "second/a", "second/b", "first/a", "first/b", "source",
            ],
            &[
                ("entry", "source"),
                ("source", "first/a"),
                ("first/a", "first/b"),
                ("first/b", "first/a"),
                ("first/b", "second/a"),
                ("second/a", "second/b"),
                ("second/b", "second/a"),
                ("second/b", "join"),
            ],
            "entry",
        );
        let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
        let entry_key = InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: "entry".into(),
            invocation: 1,
        };
        let source_key = InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: "source".into(),
            invocation: 1,
        };
        record.invocations.insert("entry".into(), 1);
        record.invocations.insert("source".into(), 1);
        record.passes.insert("entry".into(), 1);
        record.passes.insert("source".into(), 1);
        record.results.insert(
            "entry".into(),
            vec![RunResult {
                node_id: "entry".into(),
                key: entry_key,
                completion: NodeCompletion {
                    submission: "entry complete".into(),
                    route: None,
                    model_requests: 1,
                    output: Value::Null,
                },
                commit: CommitRef {
                    id: "entry-result".into(),
                    node_id: "entry".into(),
                    invocation: 1,
                },
                sequence: 1,
            }],
        );
        record.results.insert(
            "source".into(),
            vec![RunResult {
                node_id: "source".into(),
                key: source_key,
                completion: NodeCompletion {
                    submission: "source chose another route".into(),
                    route: None,
                    model_requests: 1,
                    output: Value::Null,
                },
                commit: CommitRef {
                    id: "source-result".into(),
                    node_id: "source".into(),
                    invocation: 1,
                },
                sequence: 2,
            }],
        );
        record.decided.insert(
            "entry|source".into(),
            EdgeDecision {
                selected: true,
                sequence: 1,
                source_invocation: 1,
                result_sequence: 1,
            },
        );
        record.decided.insert(
            "source|first/a".into(),
            EdgeDecision {
                selected: false,
                sequence: 2,
                source_invocation: 1,
                result_sequence: 2,
            },
        );
        record.sequence = 2;

        assert!(propagate_inactive_edges(&mut record).unwrap());
        let first_sequences = record
            .decided
            .iter()
            .map(|(edge, decision)| (edge.clone(), decision.sequence))
            .collect::<BTreeMap<_, _>>();
        assert!(
            record.decided["first/a|first/b"].sequence
                < record.decided["second/a|second/b"].sequence
        );
        assert!(
            record.decided["second/a|second/b"].sequence < record.decided["second/b|join"].sequence
        );

        assert!(!propagate_inactive_edges(&mut record).unwrap());
        let second_sequences = record
            .decided
            .iter()
            .map(|(edge, decision)| (edge.clone(), decision.sequence))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(first_sequences, second_sequences);
        for edge in [
            "source|first/a",
            "first/a|first/b",
            "first/b|first/a",
            "first/b|second/a",
            "second/a|second/b",
            "second/b|second/a",
            "second/b|join",
        ] {
            assert!(!record.decided[edge].selected, "{edge}");
        }
    }

    #[test]
    fn skipped_propagation_does_not_invent_decisions_inside_unentered_cycle() {
        let snapshot = graph(
            &["entry", "loop/a", "loop/b"],
            &[
                ("entry", "loop/a"),
                ("loop/a", "loop/b"),
                ("loop/b", "loop/a"),
            ],
            "entry",
        );
        let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
        record.decided.insert(
            edge_key("entry", "loop/a"),
            EdgeDecision {
                selected: false,
                sequence: 1,
                source_invocation: 0,
                result_sequence: 0,
            },
        );
        record.sequence = 1;

        assert!(propagate_inactive_edges(&mut record).unwrap());
        for edge in ["entry|loop/a", "loop/a|loop/b", "loop/b|loop/a"] {
            let decision = &record.decided[edge];
            assert!(!decision.selected, "{edge}");
            assert_eq!(decision.source_invocation, 0, "{edge}");
            assert_eq!(decision.result_sequence, 0, "{edge}");
        }
    }

    #[test]
    fn later_false_loop_input_retires_stale_selected_output() {
        let snapshot = graph(
            &["source", "branch", "merge"],
            &[
                ("source", "branch"),
                ("branch", "source"),
                ("branch", "merge"),
            ],
            "source",
        );
        let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
        record.invocations.insert("source".into(), 2);
        record.invocations.insert("branch".into(), 1);
        record.passes.insert("source".into(), 2);
        record.passes.insert("branch".into(), 1);
        let key = |node: &str, invocation| InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: node.into(),
            invocation,
        };
        record.results.insert(
            "branch".into(),
            vec![RunResult {
                node_id: "branch".into(),
                key: key("branch", 1),
                completion: NodeCompletion {
                    submission: "branch completed in round one".into(),
                    route: Some("merge".into()),
                    model_requests: 1,
                    output: serde_json::json!({"stale":true}),
                },
                commit: CommitRef {
                    id: "branch-round-one".into(),
                    node_id: "branch".into(),
                    invocation: 1,
                },
                sequence: 10,
            }],
        );
        record.decided.insert(
            "source|branch".into(),
            EdgeDecision {
                selected: false,
                sequence: 11,
                source_invocation: 2,
                result_sequence: 0,
            },
        );
        record.decided.insert(
            "branch|merge".into(),
            EdgeDecision {
                selected: true,
                sequence: 12,
                source_invocation: 1,
                result_sequence: 10,
            },
        );
        record.sequence = 12;

        assert!(propagate_inactive_edges(&mut record).unwrap());
        let stale_output = &record.decided["branch|merge"];
        assert!(!stale_output.selected);
        assert_eq!(stale_output.source_invocation, 0);
        assert_eq!(stale_output.result_sequence, 0);
        assert!(
            record.results["branch"]
                .iter()
                .any(|result| { result.commit.id == "branch-round-one" })
        ); // Preserve the old artifact as history, but no longer route it.
    }

    #[tokio::test]
    async fn op_run_policy_and_command_are_passed_to_the_host_port() {
        let mut snapshot = graph(&["command"], &[], "command");
        snapshot.ops.insert(
            "script".into(),
            serde_json::json!({
                "run": "printf fixture",
                "network": true,
                "wall_time_limit_seconds": 90
            }),
        );
        snapshot.nodes[0].agent = None;
        snapshot.nodes[0].op = Some("script".into());
        let (store, artifacts, mut nodes, control) = setup();
        nodes.op_run = true;
        let record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
        let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(record)
            .await
            .unwrap();
        assert_eq!(result.status, RunStatus::Completed);
        let calls = nodes.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].kind, NodeKind::OpRun);
        assert_eq!(calls[0].model, None);
        assert_eq!(
            calls[0].operation,
            Some(Value::String("printf fixture".into()))
        );
        assert!(calls[0].network);
        assert_eq!(calls[0].wall_time_limit_seconds, Some(90.0));
    }

    #[tokio::test]
    async fn budget_stop_keeps_cursor_and_resume_uses_same_invocation() {
        let mut g = graph(&["one"], &[], "one");
        g.agents.get_mut("worker").unwrap().max_steps = Some(0);
        let (s, a, n, c) = setup();
        *n.budget_once.lock().unwrap() = true;
        let record = GraphRunRecord::create(g, Value::Null).unwrap();
        let run_id = record.run_id.clone();
        let runner = GraphRunner::new(&s, &a, &n, &c);
        let stopped = runner.run(record).await.unwrap();
        assert_eq!(stopped.status, RunStatus::BudgetStopped);
        assert!(stopped.cursor.is_some());
        assert_eq!(stopped.invocations["one"], 1);
        assert_eq!(stopped.passes["one"], 1);
        let durable = s.load(&run_id).unwrap().unwrap();
        assert_eq!(durable.invocations["one"], 1);
        assert_eq!(durable.passes["one"], 1);
        assert_eq!(
            n.facts
                .lock()
                .unwrap()
                .get(&stopped.cursor.as_ref().unwrap().key.durable_key()),
            None
        );
        assert_eq!(*a.freezes.lock().unwrap(), 0);
        let resumed = runner.run(stopped).await.unwrap();
        assert_eq!(resumed.status, RunStatus::Completed);
        let calls = n.calls.lock().unwrap();
        assert_eq!(calls[0].key, calls[1].key);
        assert_eq!(resumed.invocations["one"], 1);
        assert_eq!(resumed.passes["one"], 1);
    }

    #[tokio::test]
    async fn format_one_cursor_migrates_under_lease_and_persists_once() {
        let root = std::env::temp_dir().join(format!(
            "anchor-run-migration-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = FileRunStore::new(&root);
        let artifacts = MemoryArtifacts::default();
        let nodes = FakeNodes::default();
        let control = Control::default();
        let mut record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        let id = record.run_id.clone();
        record.cursor = Some(RunCursor {
            node_id: "one".into(),
            key: InvocationKey {
                run_id: id.clone(),
                graph_digest: record.graph_digest.clone(),
                node_id: "one".into(),
                invocation: 1,
            },
            input_commits: vec![],
            prepared_input: Value::Null,
        });
        record.format = 1;
        record.status = RunStatus::BudgetStopped;
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(format!("{id}.json")),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();

        // Loading migrates in memory, without writing outside the Runner lease.
        let loaded = store.load(&id).unwrap().unwrap();
        assert_eq!(loaded.format, 2);
        assert_eq!(loaded.invocations["one"], 1);
        assert_eq!(loaded.passes["one"], 1);
        let persisted: Value =
            serde_json::from_slice(&fs::read(root.join(format!("{id}.json"))).unwrap()).unwrap();
        assert_eq!(persisted["format"], 1);

        let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
        let result = runner.run(loaded).await.unwrap();
        assert_eq!(result.status, RunStatus::Completed);
        assert_eq!(result.invocations["one"], 1);
        assert_eq!(result.passes["one"], 1);
        let persisted = store.load(&id).unwrap().unwrap();
        assert_eq!(persisted.format, 2);
        assert_eq!(persisted.invocations["one"], 1);
        assert_eq!(persisted.passes["one"], 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn format_one_terminal_run_migrates_without_changing_counters() {
        let root = std::env::temp_dir().join(format!(
            "anchor-run-terminal-migration-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = FileRunStore::new(&root);
        let artifacts = MemoryArtifacts::default();
        let nodes = FakeNodes::default();
        let control = Control::default();
        let mut record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        record.status = RunStatus::Completed;
        record.format = 1;
        record.passes.insert("one".into(), 3);
        record.invocations.insert("one".into(), 3);
        let id = record.run_id.clone();
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(format!("{id}.json")),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();

        let loaded = store.load(&id).unwrap().unwrap();
        assert_eq!(loaded.format, 2);
        assert_eq!(loaded.passes["one"], 3);
        assert_eq!(loaded.invocations["one"], 3);
        let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(loaded)
            .await
            .unwrap();
        assert_eq!(result.status, RunStatus::Completed);
        let persisted = store.load(&id).unwrap().unwrap();
        assert_eq!(persisted.format, 2);
        assert_eq!(persisted.passes["one"], 3);
        assert_eq!(persisted.invocations["one"], 3);
        assert!(nodes.calls.lock().unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn known_node_failure_is_terminal_and_does_not_advance_edges() {
        let snapshot = graph(
            &["fails", "downstream"],
            &[("fails", "downstream")],
            "fails",
        );
        let (store, artifacts, nodes, control) = setup();
        *nodes.fail_once.lock().unwrap() = Some("provider rejected request".into());
        *store.fail_on.lock().unwrap() = Some(3);
        let record = GraphRunRecord::create(snapshot.clone(), Value::Null).unwrap();
        let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
        assert!(runner.run(record).await.is_err());
        let interrupted = store
            .load(&nodes.calls.lock().unwrap()[0].key.run_id)
            .unwrap()
            .unwrap();
        assert!(interrupted.cursor.is_some());
        let failed = runner.run(interrupted).await.unwrap();
        assert_eq!(failed.status, RunStatus::Failed);
        assert!(
            failed
                .error
                .as_deref()
                .unwrap()
                .contains("provider rejected request")
        );
        assert!(failed.error.as_deref().unwrap().contains("fails"));
        assert!(failed.cursor.is_none());
        assert_eq!(failed.invocations["fails"], 1);
        assert_eq!(failed.passes["fails"], 1);
        assert_eq!(nodes.calls.lock().unwrap().len(), 1);
        assert!(failed.decided.is_empty());
        assert!(!failed.invocations.contains_key("downstream"));
        assert!(!failed.passes.contains_key("downstream"));

        // Even an explicit second call with the durable terminal record cannot replay it.
        let terminal = runner.run(failed).await.unwrap();
        assert_eq!(terminal.status, RunStatus::Failed);
        assert_eq!(nodes.calls.lock().unwrap().len(), 1);
        assert_eq!(*artifacts.freezes.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn invalid_durable_route_fails_run_without_settling_or_replaying() {
        let snapshot = graph(&["start", "next"], &[("start", "next")], "start");
        let (store, artifacts, nodes, control) = setup();
        *nodes.invalid_route_once.lock().unwrap() = true;
        *store.fail_on.lock().unwrap() = Some(3);
        let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
        let record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
        let id = record.run_id.clone();
        assert!(runner.run(record).await.is_err());
        let interrupted = store.load(&id).unwrap().unwrap();
        assert!(interrupted.cursor.is_some());
        *store.fail_on.lock().unwrap() = None;
        let failed = runner.run(interrupted).await.unwrap();
        assert_eq!(failed.status, RunStatus::Failed);
        assert!(failed.error.as_deref().unwrap().contains("unlisted-route"));
        assert!(failed.cursor.is_none());
        assert_eq!(failed.invocations["start"], 1);
        assert_eq!(failed.passes["start"], 1);
        assert!(failed.decided.is_empty());
        assert!(failed.results.is_empty());
        assert_eq!(nodes.calls.lock().unwrap().len(), 1);
        let terminal = runner.run(failed).await.unwrap();
        assert_eq!(terminal.status, RunStatus::Failed);
        assert_eq!(nodes.calls.lock().unwrap().len(), 1);
        assert_eq!(*artifacts.freezes.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn completion_fact_closes_run_store_crash_window_without_reexecution() {
        let (s, a, n, c) = setup();
        *s.fail_on.lock().unwrap() = Some(3);
        let record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        let id = record.run_id.clone();
        let runner = GraphRunner::new(&s, &a, &n, &c);
        assert!(runner.run(record).await.is_err());
        let recovered = s.load(&id).unwrap().unwrap();
        assert!(recovered.cursor.is_some());
        *s.fail_on.lock().unwrap() = None;
        let done = runner.run(recovered).await.unwrap();
        assert_eq!(done.status, RunStatus::Completed);
        assert_eq!(n.calls.lock().unwrap().len(), 1);
        assert_eq!(*a.freezes.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn durable_run_save_feedback_error_requires_reloading_before_resume() {
        let root = std::env::temp_dir().join(format!(
            "anchor-run-save-feedback-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let store = PersistThenFailStore {
            inner: FileRunStore::new(&root),
            fail_after_result_commit: Mutex::new(true),
        };
        let ports = DurableTestPorts {
            root: root.clone(),
            crash_point: None,
        };
        let control = Control::default();
        let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        let run_id = initial.run_id.clone();

        let first = GraphRunner::new(&store, &ports, &ports, &control)
            .run(initial.clone())
            .await;
        assert!(first.is_err(), "wrapper must report its post-save error");

        // The atomic file replacement committed before the wrapper lost the
        // success feedback, so replaying the caller's old in-memory snapshot
        // must conflict with the durable Run facts.
        assert!(matches!(
            GraphRunner::new(&store, &ports, &ports, &control)
                .run(initial)
                .await,
            Err(GraphError::RunConflict)
        ));

        // Recovery starts from the durable latest record, using a fresh Runner
        // and the underlying FileRunStore adapter.
        let recovered_record = store.load(&run_id).unwrap().unwrap();
        assert_eq!(recovered_record.status, RunStatus::Running);
        assert!(recovered_record.cursor.is_none());
        assert_eq!(recovered_record.results["one"].len(), 1);
        let result = &recovered_record.results["one"][0];
        assert_eq!(result.commit.id, result.key.durable_key());
        let recovered = GraphRunner::new(&store.inner, &ports, &ports, &control)
            .run(recovered_record)
            .await
            .unwrap();

        assert_eq!(recovered.status, RunStatus::Completed);
        assert!(recovered.cursor.is_none());
        assert_eq!(recovered.results["one"].len(), 1);
        assert_eq!(recovered.invocations["one"], 1);
        assert_eq!(recovered.passes["one"], 1);
        assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
        assert_eq!(fs::read(root.join("freeze-count")).unwrap(), b"1");
        assert_eq!(store.inner.load(&run_id).unwrap().unwrap(), recovered);

        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn uncertain_and_pause_fail_closed_at_invocation_boundary() {
        let (s, a, n, mut c) = setup();
        *n.uncertain.lock().unwrap() = true;
        let record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        let runner = GraphRunner::new(&s, &a, &n, &c);
        let failed = runner.run(record).await.unwrap();
        assert_eq!(failed.status, RunStatus::Failed);
        assert!(n.calls.lock().unwrap().is_empty());
        assert!(failed.cursor.is_some());
        assert_eq!(failed.invocations["one"], 1);
        assert_eq!(failed.passes["one"], 1);
        assert!(failed.decided.is_empty());
        c.pause = true;
        let paused = GraphRunner::new(&s, &a, &n, &c)
            .run(GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap())
            .await
            .unwrap();
        assert_eq!(paused.status, RunStatus::Paused);
        assert!(paused.cursor.is_none());
    }

    #[tokio::test]
    async fn stop_before_dispatch_and_executor_cancel_preserve_safe_cursor() {
        let (s, a, n, mut control) = setup();
        control.stop = true;
        let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        let stopped = GraphRunner::new(&s, &a, &n, &control)
            .run(initial)
            .await
            .unwrap();
        assert_eq!(stopped.status, RunStatus::Stopped);
        assert!(stopped.cursor.is_none());
        assert!(n.calls.lock().unwrap().is_empty());

        control.stop = false;
        *n.cancel_once.lock().unwrap() = true;
        let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        let stopped = GraphRunner::new(&s, &a, &n, &control)
            .run(initial)
            .await
            .unwrap();
        assert_eq!(stopped.status, RunStatus::Stopped);
        let cursor = stopped
            .cursor
            .as_ref()
            .expect("cancelled attempt stays resumable");
        assert_eq!(cursor.key.invocation, 1);
        assert_eq!(n.facts.lock().unwrap().get(&cursor.key.durable_key()), None);
    }

    #[tokio::test]
    async fn run_rejects_cursor_identity_mismatch_before_dispatch() {
        let (store, artifacts, nodes, control) = setup();
        let mut record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        let key = InvocationKey {
            run_id: "another-run".into(),
            graph_digest: record.graph_digest.clone(),
            node_id: "one".into(),
            invocation: 1,
        };
        record.cursor = Some(RunCursor {
            node_id: "one".into(),
            key,
            input_commits: vec![],
            prepared_input: Value::Null,
        });
        assert!(matches!(
            GraphRunner::new(&store, &artifacts, &nodes, &control)
                .run(record)
                .await,
            Err(GraphError::CorruptRun(_))
        ));
        assert!(nodes.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn admission_rejects_plugins_parallel_nodes_and_finite_budget_without_exact_port() {
        let mut g = graph(&["one"], &[], "one");
        g.nodes[0].plugins = vec!["p".into()];
        assert!(matches!(
            GraphSnapshot::admit(serde_json::to_value(g).unwrap()),
            Err(GraphError::Unsupported(_))
        ));
        let mut g = graph(&["one"], &[], "one");
        g.ops
            .insert("fan".into(), serde_json::json!({"fanout":{"join":"x"}}));
        g.nodes[0].agent = None;
        g.nodes[0].op = Some("fan".into());
        assert!(matches!(
            GraphSnapshot::admit(serde_json::to_value(g).unwrap()),
            Err(GraphError::Unsupported(_))
        ));
        let mut g = graph(&["one"], &[], "one");
        g.agents.get_mut("worker").unwrap().max_steps = Some(2);
        let (s, a, mut n, c) = setup();
        n.caps_budget_off = true;
        let record = GraphRunRecord::create(g, Value::Null).unwrap();
        assert!(matches!(
            GraphRunner::new(&s, &a, &n, &c).run(record).await,
            Err(GraphError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn loop_reentry_uses_fresh_selected_back_edge_and_ceiling() {
        let mut g = graph(
            &["gather", "review"],
            &[("gather", "review"), ("review", "gather")],
            "gather",
        );
        g.nodes[0].max_rounds = Some(2);
        let (s, a, n, c) = setup();
        let run = GraphRunRecord::create(g, Value::Null).unwrap();
        let result = GraphRunner::new(&s, &a, &n, &c).run(run).await.unwrap();
        assert_eq!(
            n.calls
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.key.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["gather", "review", "gather", "review"]
        );
        assert_eq!(result.status, RunStatus::Stopped);
    }

    #[tokio::test]
    async fn module_reentry_counts_activations_and_resets_node_pass_ceiling() {
        let mut snapshot = graph(
            &["entry", "module/a", "module/b", "again", "done"],
            &[
                ("entry", "module/a"),
                ("module/a", "module/b"),
                ("module/b", "again"),
                ("module/b", "done"),
                ("again", "module/a"),
            ],
            "entry",
        );
        snapshot.module_rounds.insert("module".into(), 2);
        snapshot.nodes[1].max_rounds = Some(1);
        let (store, artifacts, nodes, control) = setup();
        let run = GraphRunRecord::create(snapshot, Value::Null).unwrap();
        let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(run)
            .await
            .unwrap();
        assert_eq!(result.status, RunStatus::Stopped);
        assert_eq!(result.module_activations["module"], 2);
        assert_eq!(result.passes["module/a"], 1);
        assert_eq!(result.invocations["module/a"], 2);
    }

    #[test]
    fn route_selection_checks_any_explicit_route_and_requires_multiple_exit_choice() {
        let completion = |route: Option<&str>| NodeCompletion {
            submission: "done".into(),
            route: route.map(str::to_owned),
            model_requests: 1,
            output: Value::Null,
        };
        assert_eq!(
            select_route(&completion(None), &["only".into()]).unwrap(),
            Some("only".into())
        );
        assert!(matches!(
            select_route(&completion(Some("wrong")), &["only".into()]),
            Err(GraphError::InvalidRoute(_))
        ));
        assert!(matches!(
            select_route(&completion(None), &["left".into(), "right".into()]),
            Err(GraphError::InvalidRoute(_))
        ));
    }

    #[test]
    fn snapshot_admission_preserves_module_rounds_and_rejects_unsafe_shapes() {
        let mut snapshot = graph(&["enter", "m/a", "m/b"], &[("enter", "m/a")], "enter");
        snapshot.module_rounds.insert("m".into(), 2);
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["_module_rounds"]["m"], 2);
        assert_eq!(GraphSnapshot::admit(value).unwrap().module_rounds["m"], 2);

        let mut bad = graph(&["a", "b"], &[("a", "b"), ("a", "b")], "a");
        assert!(bad.validate().is_err());
        bad = graph(&[".graph-calls/x"], &[], ".graph-calls/x");
        assert!(bad.validate().is_err());
        bad = graph(&["a"], &[], "a");
        bad.module_rounds.insert("missing".into(), 2);
        assert!(bad.validate().is_err());
        let mut raw = serde_json::to_value(graph(&["a"], &[], "a")).unwrap();
        raw["unexpected"] = Value::Bool(true);
        assert!(GraphSnapshot::admit(raw).is_err());
    }

    #[test]
    fn python_expanded_graph_snapshot_fixture_preserves_node_policy() {
        let raw: Value =
            serde_json::from_str(include_str!("../tests/fixtures/one-search.snapshot.json"))
                .unwrap();
        let snapshot = GraphSnapshot::admit(raw).unwrap();
        assert_eq!(snapshot.entry, "search");
        let agent = &snapshot.agents["searcher"];
        assert_eq!(agent.model, "models.academic");
        assert!(agent.network);
        assert_eq!(agent.max_steps, Some(25));
        assert_eq!(agent.wall_time_limit_seconds, Some(600.0));
        assert_eq!(snapshot.nodes[0].id, "search");
    }

    #[test]
    fn file_run_store_roundtrips_rejects_corruption_and_releases_lease_after_process_crash() {
        let root = std::env::temp_dir().join(format!(
            "anchor-run-store-crash-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = FileRunStore::new(&root);
        let record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        store.save(&record).unwrap();
        assert_eq!(store.load(&record.run_id).unwrap(), Some(record.clone()));
        let lease = store.acquire_lease(&record.run_id).unwrap();
        assert!(matches!(
            store.acquire_lease(&record.run_id),
            Err(GraphError::RunBusy(_))
        ));
        drop(lease);
        let reopened = store.acquire_lease(&record.run_id).unwrap();
        drop(reopened);

        // Hold the lease in another test process and synchronize using pipes:
        // the child announces only after acquiring the OS lock, and exits via
        // process::exit so Rust destructors cannot release it explicitly.
        let token = format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let executable = std::env::current_exe().unwrap();
        let mut child = Command::new(&executable)
            .args([
                "--exact",
                "graph::tests::file_run_store_lease_process_helper",
                "--nocapture",
            ])
            .env("ANCHOR_FILE_LEASE_TEST_TOKEN", &token)
            .env("ANCHOR_FILE_LEASE_TEST_MODE", "holder")
            .env("ANCHOR_FILE_LEASE_TEST_ROOT", &root)
            .env("ANCHOR_FILE_LEASE_TEST_RUN", &record.run_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let child_stdout = child.stdout.take().unwrap();
        let mut child_output = io::BufReader::new(child_stdout);
        let marker = format!("ANCHOR_FILE_LEASE_HELD:{token}");
        let mut line = String::new();
        let mut acquired = false;
        loop {
            line.clear();
            let bytes = child_output.read_line(&mut line).unwrap();
            if bytes == 0 {
                break;
            }
            if line.trim_end() == marker {
                acquired = true;
                break;
            }
        }
        if !acquired {
            let output = child.wait_with_output().unwrap();
            panic!(
                "lease holder exited before announcing acquisition; status={}, stdout={}, stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let contender = Command::new(&executable)
            .args([
                "--exact",
                "graph::tests::file_run_store_lease_process_helper",
                "--nocapture",
            ])
            .env("ANCHOR_FILE_LEASE_TEST_TOKEN", &token)
            .env("ANCHOR_FILE_LEASE_TEST_MODE", "contender")
            .env("ANCHOR_FILE_LEASE_TEST_ROOT", &root)
            .env("ANCHOR_FILE_LEASE_TEST_RUN", &record.run_id)
            .output();
        let release_result = writeln!(child.stdin.as_mut().unwrap(), "exit-crash:{token}");
        drop(child.stdin.take());
        let child_status = child.wait().unwrap();
        release_result.unwrap();
        assert!(
            child_status.success(),
            "lease holder exited unsuccessfully: {child_status}"
        );

        let contender = contender.unwrap();
        assert!(
            contender.status.success(),
            "lease contender failed: stdout={}, stderr={}",
            String::from_utf8_lossy(&contender.stdout),
            String::from_utf8_lossy(&contender.stderr)
        );
        let contender_output = String::from_utf8_lossy(&contender.stdout);
        assert!(
            contender_output.contains(&format!("ANCHOR_FILE_LEASE_BUSY:{token}")),
            "contending process did not observe RunBusy: {contender_output}"
        );

        let after_crash = store.acquire_lease(&record.run_id).unwrap();
        drop(after_crash);

        fs::write(root.join(format!("{}.json", record.run_id)), b"not json").unwrap();
        assert!(matches!(
            store.load(&record.run_id),
            Err(GraphError::RunDecode(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn file_run_store_rejects_damaged_result_and_edge_facts() {
        let root = std::env::temp_dir().join(format!(
            "anchor-run-validation-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = FileRunStore::new(&root);
        let (memory_store, artifacts, nodes, control) = setup();
        let valid = GraphRunner::new(&memory_store, &artifacts, &nodes, &control)
            .run(
                GraphRunRecord::create(
                    graph(&["one", "two"], &[("one", "two")], "one"),
                    Value::Null,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(valid.status, RunStatus::Completed);
        let id = valid.run_id.clone();
        let path = root.join(format!("{id}.json"));

        let mut bad_records = Vec::new();
        let mut bad = valid.clone();
        bad.results.get_mut("one").unwrap()[0].node_id = "two".into();
        bad_records.push(bad);
        let mut bad = valid.clone();
        bad.results.get_mut("one").unwrap()[0].key.node_id = "two".into();
        bad_records.push(bad);
        let mut bad = valid.clone();
        bad.results.get_mut("one").unwrap()[0].commit.node_id = "two".into();
        bad_records.push(bad);
        let mut bad = valid.clone();
        bad.results.get_mut("one").unwrap()[0].sequence = valid.sequence + 1;
        bad_records.push(bad);
        let mut bad = valid.clone();
        let duplicate_sequence = bad.results["one"][0].sequence;
        bad.results.get_mut("two").unwrap()[0].sequence = duplicate_sequence;
        bad_records.push(bad);
        let mut bad = valid.clone();
        let selected = bad.decided.get_mut("one|two").unwrap();
        selected.result_sequence = valid.sequence + 1;
        bad_records.push(bad);
        let mut bad = valid.clone();
        bad.decided.insert(
            "one|ghost".into(),
            EdgeDecision {
                selected: false,
                sequence: valid.sequence,
                source_invocation: 0,
                result_sequence: 0,
            },
        );
        bad_records.push(bad);
        let mut bad = valid.clone();
        bad.decided.get_mut("one|two").unwrap().sequence = valid.sequence + 1;
        bad_records.push(bad);
        let cursor = RunCursor {
            node_id: "two".into(),
            key: InvocationKey {
                run_id: valid.run_id.clone(),
                graph_digest: valid.graph_digest.clone(),
                node_id: "two".into(),
                invocation: 1,
            },
            input_commits: vec![],
            prepared_input: Value::Null,
        };
        let mut bad = valid.clone();
        bad.cursor = Some(cursor.clone());
        bad_records.push(bad); // Completed + cursor.
        let mut bad = valid.clone();
        bad.status = RunStatus::BudgetStopped;
        bad.cursor = None;
        bad_records.push(bad); // BudgetStopped without resumable cursor.
        let mut bad = valid.clone();
        bad.status = RunStatus::Ready;
        bad.cursor = Some(cursor);
        bad_records.push(bad); // Ready + cursor.

        let mut valid_cursor = valid.clone();
        valid_cursor.status = RunStatus::BudgetStopped;
        valid_cursor.invocations.insert("two".into(), 2);
        valid_cursor.passes.insert("two".into(), 2);
        valid_cursor.cursor = Some(RunCursor {
            node_id: "two".into(),
            key: InvocationKey {
                run_id: valid.run_id.clone(),
                graph_digest: valid.graph_digest.clone(),
                node_id: "two".into(),
                invocation: 2,
            },
            input_commits: vec![valid.results["one"][0].commit.clone()],
            prepared_input: Value::Null,
        });
        valid_cursor.validate().unwrap();
        let mut bad = valid_cursor.clone();
        bad.cursor.as_mut().unwrap().input_commits[0].id = "foreign-commit".into();
        bad_records.push(bad);
        let mut bad = valid_cursor.clone();
        bad.cursor.as_mut().unwrap().input_commits.clear();
        bad_records.push(bad);
        let mut bad = valid_cursor.clone();
        bad.cursor
            .as_mut()
            .unwrap()
            .input_commits
            .push(valid.results["one"][0].commit.clone());
        bad_records.push(bad);

        for bad in bad_records {
            assert!(matches!(store.save(&bad), Err(GraphError::CorruptRun(_))));
            assert!(
                !path.exists(),
                "invalid save must not create final run file"
            );
            fs::create_dir_all(&root).unwrap();
            fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(matches!(store.load(&id), Err(GraphError::CorruptRun(_))));
            fs::remove_file(&path).unwrap();
        }

        let mut uncertain = valid.clone();
        uncertain.status = RunStatus::Failed;
        uncertain.error = Some("uncertain node result for two: outcome unavailable".into());
        uncertain.invocations.insert("two".into(), 2);
        uncertain.passes.insert("two".into(), 2);
        uncertain.cursor = Some(RunCursor {
            node_id: "two".into(),
            key: InvocationKey {
                run_id: valid.run_id.clone(),
                graph_digest: valid.graph_digest.clone(),
                node_id: "two".into(),
                invocation: 2,
            },
            input_commits: vec![valid.results["one"][0].commit.clone()],
            prepared_input: Value::Null,
        });
        // Failed + cursor is retained as a representable uncertain-result fact.
        store.save(&uncertain).unwrap();
        assert_eq!(store.load(&id).unwrap(), Some(uncertain));

        let mut loop_snapshot = graph(&["spin"], &[("spin", "spin")], "spin");
        loop_snapshot.nodes[0].max_rounds = Some(2);
        let looped = GraphRunner::new(&memory_store, &artifacts, &nodes, &control)
            .run(GraphRunRecord::create(loop_snapshot, Value::Null).unwrap())
            .await
            .unwrap();
        assert_eq!(looped.results["spin"].len(), 2);
        let loop_id = looped.run_id.clone();
        let loop_path = root.join(format!("{loop_id}.json"));
        let mut reversed = looped.clone();
        reversed.results.get_mut("spin").unwrap().reverse();
        assert!(matches!(
            store.save(&reversed),
            Err(GraphError::CorruptRun(_))
        ));
        assert!(!loop_path.exists());
        fs::create_dir_all(&root).unwrap();
        fs::write(&loop_path, serde_json::to_vec(&reversed).unwrap()).unwrap();
        assert!(matches!(
            store.load(&loop_id),
            Err(GraphError::CorruptRun(_))
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_run_store_rejects_edge_decision_not_newer_than_its_result() {
        let root = std::env::temp_dir().join(format!(
            "anchor-self-loop-freshness-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = FileRunStore::new(&root);
        let mut record =
            GraphRunRecord::create(graph(&["spin"], &[("spin", "spin")], "spin"), Value::Null)
                .unwrap();
        let key = InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: "spin".into(),
            invocation: 1,
        };
        let result_sequence = 1;
        record.results.insert(
            "spin".into(),
            vec![RunResult {
                node_id: "spin".into(),
                key: key.clone(),
                completion: NodeCompletion {
                    submission: "spin completed".into(),
                    route: Some("spin".into()),
                    model_requests: 1,
                    output: Value::Null,
                },
                commit: CommitRef {
                    id: key.durable_key(),
                    node_id: "spin".into(),
                    invocation: 1,
                },
                sequence: result_sequence,
            }],
        );
        record.invocations.insert("spin".into(), 1);
        record.passes.insert("spin".into(), 1);
        record.sequence = 2;
        record.status = RunStatus::Stopped;
        record.decided.insert(
            "spin|spin".into(),
            EdgeDecision {
                selected: true,
                sequence: 2,
                source_invocation: 1,
                result_sequence,
            },
        );

        // This is a valid completed node result whose self-loop was selected;
        // the Run was then stopped before the next invocation was dispatched.
        store.save(&record).unwrap();
        let id = record.run_id.clone();
        assert_eq!(store.load(&id).unwrap(), Some(record.clone()));

        let mut stale = record.clone();
        stale.decided.get_mut("spin|spin").unwrap().sequence = result_sequence;
        assert!(matches!(store.save(&stale), Err(GraphError::CorruptRun(_))));

        let path = root.join(format!("{id}.json"));
        fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert!(matches!(store.load(&id), Err(GraphError::CorruptRun(_))));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_run_store_lease_process_helper() {
        // This test is intentionally inert when run by the normal harness. The
        // parent test supplies a per-invocation token and a private temp root.
        let (Ok(token), Ok(mode), Ok(root), Ok(run_id)) = (
            std::env::var("ANCHOR_FILE_LEASE_TEST_TOKEN"),
            std::env::var("ANCHOR_FILE_LEASE_TEST_MODE"),
            std::env::var("ANCHOR_FILE_LEASE_TEST_ROOT"),
            std::env::var("ANCHOR_FILE_LEASE_TEST_RUN"),
        ) else {
            return;
        };
        if token.is_empty()
            || !["holder", "contender"].contains(&mode.as_str())
            || !PathBuf::from(&root)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("anchor-run-store-crash-"))
        {
            return;
        }

        let store = FileRunStore::new(root);
        if mode == "contender" {
            match store.acquire_lease(&run_id) {
                Err(GraphError::RunBusy(_)) => println!("ANCHOR_FILE_LEASE_BUSY:{token}"),
                Ok(lease) => {
                    drop(lease);
                    println!("ANCHOR_FILE_LEASE_ACQUIRED:{token}");
                }
                Err(error) => panic!("contender failed to check lease: {error}"),
            }
            return;
        }

        let _lease = store.acquire_lease(&run_id).unwrap();
        println!("ANCHOR_FILE_LEASE_HELD:{token}");
        io::stdout().flush().unwrap();
        let mut command = String::new();
        io::stdin().read_line(&mut command).unwrap();
        assert_eq!(command.trim_end(), format!("exit-crash:{token}"));
        // Deliberately bypass `drop(lease)`: process termination must release
        // the kernel lock while leaving its stable lock file/inode in place.
        std::process::exit(0);
        #[allow(unreachable_code)]
        drop(_lease);
    }

    #[tokio::test]
    async fn file_run_store_recovers_completed_node_and_artifact_facts_after_process_crash() {
        let executable = std::env::current_exe().unwrap();
        for (crash_name, expected_exit) in [("after-node-fact", 71), ("after-artifact-commit", 72)]
        {
            let root = std::env::temp_dir().join(format!(
                "anchor-graph-recovery-{}-{}-{crash_name}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            let store = FileRunStore::new(&root);
            let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
            let run_id = initial.run_id.clone();
            write_durable_json(&root.join("initial.json"), &initial).unwrap();

            let crashed = Command::new(&executable)
                .args([
                    "--exact",
                    "graph::tests::graph_runner_recovery_process_helper",
                    "--nocapture",
                ])
                .env("ANCHOR_GRAPH_RECOVERY_TEST_ROOT", &root)
                .env("ANCHOR_GRAPH_RECOVERY_TEST_RUN", &run_id)
                .env("ANCHOR_GRAPH_RECOVERY_TEST_CRASH", crash_name)
                .output()
                .unwrap();
            assert_eq!(
                crashed.status.code(),
                Some(expected_exit),
                "child did not crash at {crash_name}; stdout={}, stderr={}",
                String::from_utf8_lossy(&crashed.stdout),
                String::from_utf8_lossy(&crashed.stderr)
            );

            let interrupted = store.load(&run_id).unwrap().unwrap();
            assert_eq!(interrupted.status, RunStatus::Running);
            let cursor = interrupted
                .cursor
                .as_ref()
                .expect("cursor persisted before dispatch");
            assert_eq!(cursor.node_id, "one");
            assert_eq!(cursor.key.invocation, 1);
            assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
            assert!(
                root.join(format!("node-{}.json", cursor.key.durable_key()))
                    .exists(),
                "durable completion fact must survive {crash_name}"
            );

            let ports = DurableTestPorts {
                root: root.clone(),
                crash_point: None,
            };
            let control = Control::default();
            let recovered = GraphRunner::new(&store, &ports, &ports, &control)
                .run(interrupted)
                .await
                .unwrap();
            assert_eq!(recovered.status, RunStatus::Completed, "{crash_name}");
            assert!(recovered.cursor.is_none());
            assert_eq!(recovered.invocations["one"], 1);
            assert_eq!(recovered.passes["one"], 1);
            assert_eq!(recovered.results["one"].len(), 1);
            assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
            assert_eq!(fs::read(root.join("freeze-count")).unwrap(), b"1");

            let result = &recovered.results["one"][0];
            let artifact_path = ports.artifact_path(&result.key);
            let artifact: DurableTestArtifact =
                serde_json::from_slice(&fs::read(artifact_path).unwrap()).unwrap();
            assert_eq!(result.commit, artifact.commit);
            assert_eq!(result.commit.id, result.key.durable_key());
            assert_eq!(
                store.load(&run_id).unwrap().unwrap(),
                recovered,
                "the recovered result must be the durable Run fact"
            );
            drop(store);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn file_run_store_recovers_not_started_invocation_after_process_crash() {
        let executable = std::env::current_exe().unwrap();
        let root = std::env::temp_dir().join(format!(
            "anchor-graph-recovery-{}-{}-before-dispatch",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let store = FileRunStore::new(&root);
        let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        let run_id = initial.run_id.clone();
        write_durable_json(&root.join("initial.json"), &initial).unwrap();

        let crashed = Command::new(&executable)
            .args([
                "--exact",
                "graph::tests::graph_runner_recovery_process_helper",
                "--nocapture",
            ])
            .env("ANCHOR_GRAPH_RECOVERY_TEST_ROOT", &root)
            .env("ANCHOR_GRAPH_RECOVERY_TEST_RUN", &run_id)
            .env("ANCHOR_GRAPH_RECOVERY_TEST_CRASH", "after-not-started")
            .output()
            .unwrap();
        assert_eq!(crashed.status.code(), Some(70));

        let interrupted = store.load(&run_id).unwrap().unwrap();
        assert_eq!(interrupted.status, RunStatus::Running);
        let cursor = interrupted
            .cursor
            .clone()
            .expect("cursor saved before fact lookup");
        assert_eq!(cursor.node_id, "one");
        assert_eq!(cursor.key.invocation, 1);
        assert!(
            !root
                .join(format!("node-{}.json", cursor.key.durable_key()))
                .exists()
        );
        assert!(!root.join("execute-count").exists());
        drop(store.acquire_lease(&run_id).unwrap());

        let ports = DurableTestPorts {
            root: root.clone(),
            crash_point: None,
        };
        let recovered = GraphRunner::new(&store, &ports, &ports, &Control::default())
            .run(interrupted)
            .await
            .unwrap();
        assert_eq!(recovered.status, RunStatus::Completed);
        assert!(recovered.cursor.is_none());
        assert_eq!(recovered.invocations["one"], 1);
        assert_eq!(recovered.passes["one"], 1);
        assert_eq!(recovered.results["one"].len(), 1);
        assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
        let result = &recovered.results["one"][0];
        assert_eq!(result.key, cursor.key);
        assert_eq!(result.commit.id, cursor.key.durable_key());
        assert_eq!(store.load(&run_id).unwrap().unwrap(), recovered);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn file_run_store_recovers_durable_failed_node_without_replay_or_freeze() {
        let executable = std::env::current_exe().unwrap();
        let root = std::env::temp_dir().join(format!(
            "anchor-graph-recovery-{}-{}-after-failed-fact",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let store = FileRunStore::new(&root);
        let initial = GraphRunRecord::create(
            graph(&["one", "two"], &[("one", "two")], "one"),
            Value::Null,
        )
        .unwrap();
        let run_id = initial.run_id.clone();
        write_durable_json(&root.join("initial.json"), &initial).unwrap();

        let crashed = Command::new(&executable)
            .args([
                "--exact",
                "graph::tests::graph_runner_recovery_process_helper",
                "--nocapture",
            ])
            .env("ANCHOR_GRAPH_RECOVERY_TEST_ROOT", &root)
            .env("ANCHOR_GRAPH_RECOVERY_TEST_RUN", &run_id)
            .env("ANCHOR_GRAPH_RECOVERY_TEST_CRASH", "after-failed-fact")
            .output()
            .unwrap();
        assert_eq!(crashed.status.code(), Some(73));

        let interrupted = store.load(&run_id).unwrap().unwrap();
        assert_eq!(interrupted.status, RunStatus::Running);
        let cursor = interrupted
            .cursor
            .clone()
            .expect("cursor saved before dispatch");
        assert_eq!(cursor.node_id, "one");
        assert_eq!(cursor.key.invocation, 1);
        assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
        let fact: DurableTestNodeFact = serde_json::from_slice(
            &fs::read(root.join(format!("node-{}.json", cursor.key.durable_key()))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            fact,
            DurableTestNodeFact::Failed("durable test failure".into())
        );
        drop(store.acquire_lease(&run_id).unwrap());

        let ports = DurableTestPorts {
            root: root.clone(),
            crash_point: None,
        };
        let recovered = GraphRunner::new(&store, &ports, &ports, &Control::default())
            .run(interrupted)
            .await
            .unwrap();
        assert_eq!(recovered.status, RunStatus::Failed);
        assert!(recovered.cursor.is_none());
        assert_eq!(recovered.invocations["one"], 1);
        assert_eq!(recovered.passes["one"], 1);
        assert!(recovered.results.is_empty());
        assert!(
            recovered.decided.is_empty(),
            "failure must not settle outgoing edges"
        );
        assert!(
            recovered
                .error
                .as_deref()
                .unwrap()
                .contains("durable test failure")
        );
        assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
        assert!(!root.join("freeze-count").exists());
        assert!(
            !root
                .join(format!("artifact-{}.json", cursor.key.durable_key()))
                .exists()
        );
        assert_eq!(store.load(&run_id).unwrap().unwrap(), recovered);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn graph_runner_recovery_process_helper() {
        let (Ok(root), Ok(run_id), Ok(crash_name)) = (
            std::env::var("ANCHOR_GRAPH_RECOVERY_TEST_ROOT"),
            std::env::var("ANCHOR_GRAPH_RECOVERY_TEST_RUN"),
            std::env::var("ANCHOR_GRAPH_RECOVERY_TEST_CRASH"),
        ) else {
            return;
        };
        if !PathBuf::from(&root)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("anchor-graph-recovery-"))
        {
            return;
        }
        let crash_point = match crash_name.as_str() {
            "after-not-started" => CrashPoint::NotStartedFactCheck,
            "after-node-fact" => CrashPoint::NodeFact,
            "after-failed-fact" => CrashPoint::FailedFact,
            "after-artifact-commit" => CrashPoint::ArtifactCommit,
            _ => panic!("unknown graph recovery crash point `{crash_name}`"),
        };
        let bytes = fs::read(PathBuf::from(&root).join("initial.json")).unwrap();
        let record: GraphRunRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record.run_id, run_id);
        let store = FileRunStore::new(&root);
        let ports = DurableTestPorts {
            root: PathBuf::from(root),
            crash_point: Some(crash_point),
        };
        let control = Control::default();
        let result = GraphRunner::new(&store, &ports, &ports, &control)
            .run(record)
            .await
            .unwrap();
        assert_eq!(result.status, RunStatus::Completed);
    }

    #[derive(Default)]
    struct OracleNodes {
        outcomes: Vec<Value>,
        calls: Mutex<Vec<(String, u64)>>,
        facts: Mutex<BTreeMap<String, CompletionFact>>,
    }
    impl NodeExecutionPort for OracleNodes {
        fn capabilities(&self) -> NodeExecutionCapabilities {
            NodeExecutionCapabilities {
                agent: true,
                op_run: false,
                exact_provider_request_budget: true,
            }
        }

        fn completion_fact<'a>(
            &'a self,
            key: &'a InvocationKey,
        ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(self
                    .facts
                    .lock()
                    .unwrap()
                    .get(&key.durable_key())
                    .cloned()
                    .unwrap_or(CompletionFact::NotStarted))
            })
        }

        fn execute<'a>(
            &'a self,
            request: NodeExecutionRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>>
        {
            Box::pin(async move {
                let index = self.calls.lock().unwrap().len();
                let outcome = self.outcomes.get(index).ok_or_else(|| {
                    GraphError::CorruptRun("oracle has no outcome for node invocation".into())
                })?;
                if outcome["node"] != request.key.node_id
                    || outcome["invocation"] != request.key.invocation
                {
                    return Err(GraphError::CorruptRun(format!(
                        "oracle expected {} invocation {}, got {} invocation {}",
                        outcome["node"],
                        outcome["invocation"],
                        request.key.node_id,
                        request.key.invocation
                    )));
                }
                self.calls
                    .lock()
                    .unwrap()
                    .push((request.key.node_id.clone(), request.key.invocation));
                if outcome["exit_status"] == "budget_exhausted" {
                    return Ok(NodeExecutionOutcome::BudgetExhausted { model_requests: 2 });
                }
                if outcome["exit_status"] != "Submitted" {
                    return Ok(NodeExecutionOutcome::Failed {
                        reason: outcome["exit_status"]
                            .as_str()
                            .unwrap_or("fixture failure")
                            .into(),
                    });
                }
                let completion = NodeCompletion {
                    submission: format!(
                        "oracle:{}:{}",
                        request.key.node_id, request.key.invocation
                    ),
                    route: outcome["route"].as_str().map(str::to_owned),
                    model_requests: 1,
                    output: serde_json::json!({"node":request.key.node_id,"invocation":request.key.invocation}),
                };
                self.facts.lock().unwrap().insert(
                    request.key.durable_key(),
                    CompletionFact::Completed(completion.clone()),
                );
                Ok(NodeExecutionOutcome::Completed(completion))
            })
        }
    }

    #[tokio::test]
    async fn rust_graph_runner_matches_python_runtime_oracle_scenarios() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/r5-python-oracle.json"
        ))
        .unwrap();
        assert_eq!(fixture["format"], 1);
        assert!(
            fixture["source"]
                .as_str()
                .unwrap()
                .contains("anchor.simple.run.run")
        );

        let scenarios = fixture["scenarios"].as_array().unwrap();
        assert_eq!(scenarios.len(), 10);
        for scenario in scenarios {
            let snapshot = GraphSnapshot::admit(scenario["graph_snapshot"].clone()).unwrap();
            let override_input = scenario["run_override"]["input"].clone();
            let nodes = OracleNodes {
                outcomes: scenario["node_outcomes"].as_array().unwrap().clone(),
                ..OracleNodes::default()
            };
            let (store, artifacts, _, mut control) = setup();
            control.pause = scenario["control"]["pause_before_dispatch"] == true;
            let record = GraphRunRecord::create(snapshot, override_input).unwrap();
            assert_eq!(record.input, scenario["effective_input"]);
            let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
                .run(record)
                .await
                .unwrap();
            let expected = &scenario["python"];

            let rust_status = match result.status {
                RunStatus::Completed => "completed",
                RunStatus::BudgetStopped => "budget_stopped",
                RunStatus::Stopped => "stopped",
                RunStatus::Failed => "failed",
                RunStatus::Ready => "ready",
                RunStatus::Running => "running",
                RunStatus::Paused => "paused",
            };
            assert_eq!(
                rust_status, expected["status"],
                "scenario {}, reason {:?}, ceased {:?}",
                scenario["id"], result.error, result.ceased
            );

            let calls = nodes.calls.lock().unwrap().clone();
            let ordered_executed: Vec<&str> = calls.iter().map(|(node, _)| node.as_str()).collect();
            let expected_executed: Vec<&str> = expected["ordered_executed_nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|node| node.as_str().unwrap())
                .collect();
            assert_eq!(
                ordered_executed, expected_executed,
                "scenario {}",
                scenario["id"]
            );

            let executed_set: BTreeSet<&str> =
                calls.iter().map(|(node, _)| node.as_str()).collect();
            let derived_skipped: Vec<&str> = result
                .snapshot
                .nodes
                .iter()
                .filter(|node| !executed_set.contains(node.id.as_str()))
                .map(|node| node.id.as_str())
                .collect();
            let expected_skipped: Vec<&str> = expected["skipped_nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|node| node.as_str().unwrap())
                .collect();
            if scenario["id"] == "deterministic_failure_settles_single_exit" {
                assert_eq!(derived_skipped, vec!["downstream"]);
            } else if scenario["id"] == "pause_before_first_dispatch" {
                // Unstarted nodes are not considered skipped when the run pauses
                // before admission reaches the scheduler's propagation pass.
                assert!(expected_skipped.is_empty());
            } else {
                assert_eq!(
                    derived_skipped, expected_skipped,
                    "skipped nodes, scenario {}",
                    scenario["id"]
                );
            }

            let expected_passes = expected["passes"].as_object().unwrap();
            assert_eq!(
                result.passes.len(),
                expected_passes.len(),
                "scenario {}",
                scenario["id"]
            );
            for (node, passes) in expected_passes {
                assert_eq!(
                    result.passes.get(node),
                    passes.as_u64().as_ref(),
                    "passes for {node}, scenario {}",
                    scenario["id"]
                );
            }
            if scenario["id"] == "budget_stop_preserves_cursor" {
                assert_eq!(
                    expected["passes"]["budgeted"], 1,
                    "Python oracle must capture the started budgeted pass"
                );
            }
            if scenario["id"] == "pause_before_first_dispatch" {
                assert_eq!(expected["reason"], "asked");
                assert_eq!(result.status, RunStatus::Paused);
                assert!(nodes.calls.lock().unwrap().is_empty());
                assert!(result.cursor.is_none());
                assert!(result.passes.is_empty());
                assert!(result.decided.is_empty());
                assert!(scenario["node_outcomes"].as_array().unwrap().is_empty());
            }

            let mut ceased: Vec<String> = result.ceased.iter().cloned().collect();
            ceased.sort();
            let mut expected_ceased: Vec<String> = expected["ceased"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item.as_str().unwrap().to_owned())
                .collect();
            expected_ceased.sort();
            assert_eq!(ceased, expected_ceased, "scenario {}", scenario["id"]);

            let expected_cursor = &expected["cursor"];
            let cursor_fact = result.cursor.as_ref().map_or_else(
                || serde_json::json!({"exists":false}),
                |cursor| {
                    serde_json::json!({
                        "exists":true,
                        "node":cursor.node_id,
                        "pass":result.passes.get(&cursor.node_id).copied().unwrap_or(0),
                        "invocation":cursor.key.invocation
                    })
                },
            );

            let mut decisions: Vec<Value> = result
                .decided
                .iter()
                .map(|(key, decision)| {
                    let (from, to) = key.split_once('|').unwrap();
                    serde_json::json!({"from":from,"to":to,"selected":decision.selected})
                })
                .collect();
            decisions.sort_by(|a, b| {
                (a["from"].as_str(), a["to"].as_str()).cmp(&(b["from"].as_str(), b["to"].as_str()))
            });
            let mut expected_decisions = expected["edge_decisions"].as_array().unwrap().clone();
            expected_decisions.sort_by(|a, b| {
                (a["from"].as_str(), a["to"].as_str()).cmp(&(b["from"].as_str(), b["to"].as_str()))
            });
            if scenario["id"] == "deterministic_failure_settles_single_exit" {
                // Intentional semantic divergence: Python settles a single exit
                // even after node failure; Rust treats the failed fact as terminal.
                assert_eq!(
                    result.status,
                    RunStatus::Failed,
                    "intentional semantic divergence: failure is terminal"
                );
                assert_eq!(result.passes.get("fail"), Some(&1));
                assert!(result.cursor.is_none());
                assert!(result.decided.is_empty());
                assert_eq!(expected_decisions.len(), 1);
                assert_eq!(expected_decisions[0]["to"], "downstream");
                assert_eq!(expected_decisions[0]["selected"], true);
                assert_eq!(cursor_fact, expected_cursor.clone());
            } else {
                assert_eq!(
                    (decisions, cursor_fact),
                    (expected_decisions, expected_cursor.clone()),
                    "edge decisions and cursor, scenario {}",
                    scenario["id"]
                );
            }
        }
    }
}
