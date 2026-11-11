use super::*;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::sync::atomic::AtomicU64;

pub(crate) static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
pub struct RunResult {
    pub node_id: String,
    pub key: InvocationKey,
    /// The dependency view this invocation actually used, keyed by Plugin id:
    /// the declaration digest the view belongs to.
    ///
    /// An observation, never a gate (standard invariant 10): it is how
    /// "from which step did this Run use a different view" is readable from the
    /// results, and a Plugin that changed under a parked Run does not refuse it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_views: BTreeMap<String, String>,
    /// The Plugin code digest actually used by this invocation, keyed by Plugin
    /// id. Like dependency views, this is an observation and never a gate.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_digests: BTreeMap<String, String>,
    pub completion: NodeCompletion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interruption: Option<String>,
    pub commit: CommitRef,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallIdentity {
    pub parent_run_id: String,
    pub parent_graph_digest: String,
    pub node_id: String,
    pub invocation: u64,
    pub call_spec_digest: String,
}

impl CallIdentity {
    pub fn durable_key(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("CallIdentity serialization is infallible");
        format!("{:x}", sha2::Sha256::digest(bytes))
    }
}

/// Stable child-run association for one Op.call invocation. It contains no
/// credentials or live host handles and is safe to persist with the parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphCallRecord {
    pub identity: CallIdentity,
    pub graph: String,
    pub child_run_id: Option<String>,
    pub mode: String,
    #[serde(deserialize_with = "deserialize_graph_call_status")]
    pub status: GraphCallStatus,
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphCallStatus {
    Prepared,
    Waiting,
    Completed,
    Detached,
    Failed,
    Uncertain,
}

fn deserialize_graph_call_status<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<GraphCallStatus, D::Error> {
    let status = String::deserialize(deserializer)?;
    if status == "deleted" {
        return Err(serde::de::Error::custom(
            "old Graph Run format (deleted call); offline migration required",
        ));
    }
    GraphCallStatus::deserialize(serde::de::value::StringDeserializer::<D::Error>::new(
        status,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParallelBranchStatus {
    Ready,
    Running,
    Completed,
    Failed,
    Stopped,
}

/// Durable branch-local progress for one explicit fanout activation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParallelBranchRecord {
    pub branch_id: String,
    pub entry: String,
    pub nodes: Vec<String>,
    pub next_index: usize,
    pub status: ParallelBranchStatus,
    /// Commits for the completed prefix of `nodes`, in dependency order.
    pub completed: Vec<CommitRef>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParallelActivation {
    pub activation_id: String,
    pub fanout_node: String,
    pub join_node: String,
    pub fanout_invocation: u64,
    pub branches: Vec<ParallelBranchRecord>,
}

/// Current persisted Graph Run record format. Older records require offline migration.
pub const RUN_RECORD_FORMAT: u32 = 15;

/// The only supported persisted Run schema. No invalid in-memory format can be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct RunRecordFormat;

impl TryFrom<u32> for RunRecordFormat {
    type Error = String;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value == RUN_RECORD_FORMAT {
            Ok(Self)
        } else {
            Err(format!(
                "old Graph Run format or unsupported graph run format {value}; offline migration required"
            ))
        }
    }
}
impl From<RunRecordFormat> for u32 {
    fn from(_: RunRecordFormat) -> Self {
        RUN_RECORD_FORMAT
    }
}
impl PartialEq<u32> for RunRecordFormat {
    fn eq(&self, other: &u32) -> bool {
        u32::from(*self) == *other
    }
}

/// RFC 3339 UTC timestamp of Graph admission.
pub(crate) fn timestamp_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphRunRecord {
    pub format: RunRecordFormat,
    /// Immutable admission-time observation; unknown time stays absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    pub run_id: String,
    pub graph_digest: String,
    pub snapshot: GraphSnapshot,
    pub input: Value,
    #[serde(default)]
    pub parallel: Option<ParallelActivation>,
    #[serde(default)]
    pub graph_calls: BTreeMap<String, GraphCallRecord>,
    #[serde(default)]
    pub plugin_bindings: BTreeMap<String, PluginBinding>,
    #[serde(default)]
    pub plugin_bindings_initialized: bool,
    #[serde(default)]
    pub invocations: BTreeMap<String, u64>,
    pub passes: BTreeMap<String, u64>,
    pub module_activations: BTreeMap<String, u64>,
    pub ceased: BTreeSet<String>,
    pub results: BTreeMap<String, Vec<RunResult>>,
    pub decided: BTreeMap<String, EdgeDecision>,
    /// Graph-local result and edge-decision ordering, independent of Kernel submissions.
    pub sequence: u64,
}

#[derive(Deserialize)]
#[serde(remote = "GraphRunRecord", deny_unknown_fields)]
struct GraphRunRecordWire {
    pub format: RunRecordFormat,
    /// Immutable admission-time observation; unknown time stays absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    pub run_id: String,
    pub graph_digest: String,
    pub snapshot: GraphSnapshot,
    pub input: Value,
    #[serde(default)]
    pub parallel: Option<ParallelActivation>,
    #[serde(default)]
    pub graph_calls: BTreeMap<String, GraphCallRecord>,
    #[serde(default)]
    pub plugin_bindings: BTreeMap<String, PluginBinding>,
    #[serde(default)]
    pub plugin_bindings_initialized: bool,
    #[serde(default)]
    pub invocations: BTreeMap<String, u64>,
    pub passes: BTreeMap<String, u64>,
    pub module_activations: BTreeMap<String, u64>,
    pub ceased: BTreeSet<String>,
    pub results: BTreeMap<String, Vec<RunResult>>,
    pub decided: BTreeMap<String, EdgeDecision>,
    /// Graph-local result and edge-decision ordering, independent of Kernel submissions.
    pub sequence: u64,
}

impl<'de> Deserialize<'de> for GraphRunRecord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RecordVisitor;
        impl<'de> serde::de::Visitor<'de> for RecordVisitor {
            type Value = GraphRunRecord;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a Graph Run object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                map: M,
            ) -> Result<Self::Value, M::Error> {
                struct RecordMap<M>(M);
                impl<'de, M: serde::de::MapAccess<'de>> serde::de::MapAccess<'de> for RecordMap<M> {
                    type Error = M::Error;
                    fn next_key_seed<K: serde::de::DeserializeSeed<'de>>(
                        &mut self,
                        seed: K,
                    ) -> Result<Option<K::Value>, Self::Error> {
                        use serde::de::IntoDeserializer;
                        let Some(key) = self.0.next_key::<String>()? else {
                            return Ok(None);
                        };
                        if matches!(
                            key.as_str(),
                            "environments"
                                | "status"
                                | "cursor"
                                | "updated"
                                | "error"
                                | "recovery"
                                | "recovery_submissions"
                        ) {
                            return Err(serde::de::Error::custom(
                                "old Graph Run format; offline migration required",
                            ));
                        }
                        seed.deserialize(key.into_deserializer()).map(Some)
                    }
                    fn next_value_seed<V: serde::de::DeserializeSeed<'de>>(
                        &mut self,
                        seed: V,
                    ) -> Result<V::Value, Self::Error> {
                        self.0.next_value_seed(seed)
                    }
                }
                GraphRunRecordWire::deserialize(serde::de::value::MapAccessDeserializer::new(
                    RecordMap(map),
                ))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                _: A,
            ) -> Result<Self::Value, A::Error> {
                Err(serde::de::Error::custom(
                    "old non-object Graph Run format; offline migration required",
                ))
            }
        }
        deserializer.deserialize_any(RecordVisitor)
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
        Self::create_with_id(snapshot, input, run_id)
    }

    /// Create a Run using an identity assigned by the owning host.
    ///
    /// A platform admission passes its identity into the shared Runtime.
    /// The ID is validated before it can become a store path.
    pub fn create_with_id(
        snapshot: GraphSnapshot,
        input: Value,
        run_id: impl Into<String>,
    ) -> Result<Self, GraphError> {
        snapshot.validate()?;
        let graph_digest = snapshot.digest()?;
        let run_id = run_id.into();
        validate_component(&run_id)?;
        let input = merge_values(&snapshot.input, &input, &Value::Null);
        let now = timestamp_now();
        Ok(Self {
            format: RunRecordFormat,
            started: Some(now),
            run_id,
            graph_digest,
            snapshot,
            input,
            parallel: None,
            graph_calls: BTreeMap::new(),
            plugin_bindings: BTreeMap::new(),
            plugin_bindings_initialized: false,
            invocations: BTreeMap::new(),
            passes: BTreeMap::new(),
            module_activations: BTreeMap::new(),
            ceased: BTreeSet::new(),
            results: BTreeMap::new(),
            decided: BTreeMap::new(),
            sequence: 0,
        })
    }

    /// Admit the Graph and its complete frozen Plugin authority in one value.
    /// This constructor performs no storage or lifecycle mutation.
    pub fn create_with_plugins(
        snapshot: GraphSnapshot,
        input: Value,
        run_id: impl Into<String>,
        plugins: Vec<PluginBinding>,
    ) -> Result<Self, GraphError> {
        let mut record = Self::create_with_id(snapshot, input, run_id)?;
        let expected = record
            .snapshot
            .nodes
            .iter()
            .flat_map(|node| {
                node.plugins
                    .iter()
                    .filter(|reference| reference.id != "network")
                    .map(|reference| reference.id.clone())
            })
            .collect::<BTreeSet<_>>();
        let mut expected = expected;
        if record
            .snapshot
            .nodes
            .iter()
            .any(|node| record.snapshot.node_network_mode(node) == NetworkMode::Enabled)
        {
            expected.insert("network".to_owned());
        }
        for binding in plugins {
            if binding.digest.is_empty() || record.plugin_bindings.contains_key(&binding.id) {
                return Err(GraphError::InvalidSnapshot(
                    "Plugin admission has an empty code digest or duplicate binding".into(),
                ));
            }
            record.plugin_bindings.insert(binding.id.clone(), binding);
        }
        let network_possible = record.snapshot.nodes.iter().any(|node| {
            record.snapshot.node_network_mode(node) != NetworkMode::Denied
                || node
                    .plugins
                    .iter()
                    .any(|reference| reference.id == "network" && reference.network.is_none())
        });
        if expected
            .iter()
            .any(|id| !record.plugin_bindings.contains_key(id))
            || record
                .plugin_bindings
                .keys()
                .any(|id| !(expected.contains(id) || id == "network" && network_possible))
        {
            return Err(GraphError::InvalidSnapshot(
                "Plugin admission must bind the Graph's executable Plugins and only its authorized network provider".into(),
            ));
        }
        record.plugin_bindings_initialized = true;
        Ok(record)
    }

    /// Returns node IDs in first durable completion order.
    /// The persisted `results` map is keyed by node ID and therefore cannot
    /// itself represent execution order.
    pub fn executed_nodes(&self) -> Vec<String> {
        let mut results = self.results.values().flatten().collect::<Vec<_>>();
        results.sort_by_key(|result| result.sequence);
        let mut seen = BTreeSet::new();
        results
            .into_iter()
            .filter(|result| seen.insert(result.node_id.clone()))
            .map(|result| result.node_id.clone())
            .collect()
    }

    pub fn validate(&self) -> Result<(), GraphError> {
        validate_component(&self.run_id)?;
        self.snapshot.validate()?;
        if self.snapshot.digest()? != self.graph_digest {
            return Err(GraphError::CorruptRun("snapshot digest mismatch".into()));
        }
        for (key, call) in &self.graph_calls {
            let spec_exists = self.snapshot.nodes.iter().any(|node| {
                node.id == call.identity.node_id
                    && node
                        .op
                        .as_ref()
                        .and_then(|name| self.snapshot.ops.get(name))
                        .is_some_and(|op| op.get("call").is_some())
            });
            if key != &call.identity.durable_key()
                || call.identity.parent_run_id != self.run_id
                || call.identity.parent_graph_digest != self.graph_digest
                || call.identity.invocation == 0
                || call.graph.is_empty()
                || !spec_exists
                || matches!(
                    call.status,
                    GraphCallStatus::Waiting
                        | GraphCallStatus::Completed
                        | GraphCallStatus::Detached
                ) && call.child_run_id.as_deref().is_none_or(str::is_empty)
            {
                return Err(GraphError::CorruptRun(
                    "Graph call record identity or child reference is invalid".into(),
                ));
            }
        }
        let regions = self.snapshot.parallel_regions().map_err(|error| {
            GraphError::CorruptRun(format!("invalid parallel snapshot: {error}"))
        })?;
        if let Some(activation) = &self.parallel {
            let region = regions.get(&activation.fanout_node).ok_or_else(|| {
                GraphError::CorruptRun("parallel activation references unknown fanout".into())
            })?;
            let expected_id = format!(
                "{}:{}:{}",
                self.run_id, activation.fanout_node, activation.fanout_invocation
            );
            let fanout_result = self
                .results
                .get(&activation.fanout_node)
                .and_then(|results| {
                    results
                        .iter()
                        .find(|result| result.key.invocation == activation.fanout_invocation)
                });
            if activation.activation_id != expected_id
                || activation.join_node != region.join
                || activation.fanout_invocation == 0
                || self.invocations.get(&activation.fanout_node).copied()
                    != Some(activation.fanout_invocation)
                || activation.branches.len() != region.branches.len()
                || fanout_result.is_none()
            {
                return Err(GraphError::CorruptRun(
                    "parallel activation identity does not match its Graph Run".into(),
                ));
            }
            let fanout_result = fanout_result.expect("checked above");
            for (index, (branch, path)) in
                activation.branches.iter().zip(&region.branches).enumerate()
            {
                let expected_branch_id = format!("{}:branch:{index}", activation.activation_id);
                if branch.branch_id != expected_branch_id
                    || branch.nodes != *path
                    || branch.entry != path[0]
                    || branch.next_index > path.len()
                    || branch.completed.len() != branch.next_index
                {
                    return Err(GraphError::CorruptRun(format!(
                        "parallel branch `{index}` identity or path is invalid"
                    )));
                }
                let mut previous_result_sequence = fanout_result.sequence;
                for (position, commit) in branch.completed.iter().enumerate() {
                    let branch_result = self
                        .results
                        .get(&commit.node_id)
                        .and_then(|results| results.iter().find(|result| result.commit == *commit));
                    if commit.node_id != path[position]
                        || branch_result.is_none_or(|result| {
                            let stale = result.sequence <= previous_result_sequence;
                            previous_result_sequence = result.sequence;
                            stale
                        })
                    {
                        return Err(GraphError::CorruptRun(format!(
                            "parallel branch `{index}` has a missing or mismatched completed result"
                        )));
                    }
                }
                let valid_state = match branch.status {
                    ParallelBranchStatus::Ready
                    | ParallelBranchStatus::Running
                    | ParallelBranchStatus::Stopped => branch.next_index < path.len(),
                    ParallelBranchStatus::Completed => {
                        branch.next_index == path.len() && branch.error.is_none()
                    }
                    ParallelBranchStatus::Failed => branch.error.is_some(),
                };
                if !valid_state {
                    return Err(GraphError::CorruptRun(format!(
                        "parallel branch `{index}` status disagrees with its completed prefix"
                    )));
                }
            }
            for path in &region.branches {
                let key = edge_key(&activation.fanout_node, &path[0]);
                if !self.decided.get(&key).is_some_and(|decision| {
                    (decision.selected
                        && decision.source_invocation == activation.fanout_invocation
                        && decision.result_sequence == fanout_result.sequence)
                        || (!decision.selected
                            && decision.source_invocation == 0
                            && decision.result_sequence == 0
                            && decision.sequence > fanout_result.sequence
                            && self.snapshot.module_rounds.iter().any(|(scope, limit)| {
                                path[0].starts_with(&format!("{scope}/"))
                                    && self.ceased.contains(&format!("{scope}@{limit}"))
                            }))
                }) {
                    return Err(GraphError::CorruptRun(format!(
                        "parallel activation branch `{}` lacks its selected fanout edge fact",
                        path[0]
                    )));
                }
            }
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
                if let Some(reason) = &result.interruption {
                    let routes = self
                        .snapshot
                        .edges
                        .iter()
                        .filter(|edge| edge.from_node == *map_node_id)
                        .map(|edge| edge.to_node.clone())
                        .collect::<Vec<_>>();
                    if result.completion
                        != interruption_completion(reason, result.completion.route.clone())
                        || select_route(&result.completion, &routes).is_err()
                        || regions.values().any(|region| {
                            region.fanout == *map_node_id
                                || region.join == *map_node_id
                                || region
                                    .branches
                                    .iter()
                                    .any(|branch| branch.contains(map_node_id))
                        })
                    {
                        return Err(GraphError::CorruptRun(format!(
                            "invalid interruption control evidence for node `{map_node_id}`"
                        )));
                    }
                }
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
        Ok(())
    }
}
