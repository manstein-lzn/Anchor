use super::*;
use serde::{Deserialize, Serialize};
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
    pub cursor: Option<RunCursor>,
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
    #[serde(default)]
    pub parallel: Option<ParallelActivation>,
    pub invocations: BTreeMap<String, u64>,
    pub passes: BTreeMap<String, u64>,
    pub module_activations: BTreeMap<String, u64>,
    pub ceased: BTreeSet<String>,
    pub results: BTreeMap<String, Vec<RunResult>>,
    pub decided: BTreeMap<String, EdgeDecision>,
    pub sequence: u64,
    pub error: Option<String>,
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
            format: 3,
            run_id,
            graph_digest,
            snapshot,
            input,
            status: RunStatus::Ready,
            cursor: None,
            parallel: None,
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

    pub(crate) fn validate(&self) -> Result<(), GraphError> {
        validate_component(&self.run_id)?;
        if self.format != 3 {
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
            RunStatus::Ready if self.cursor.is_some() || self.parallel.is_some() => {
                return Err(GraphError::CorruptRun(
                    "ready Run cannot have an active cursor".into(),
                ));
            }
            RunStatus::BudgetStopped
                if self.cursor.is_none()
                    && !self.parallel.as_ref().is_some_and(|activation| {
                        activation
                            .branches
                            .iter()
                            .any(|branch| branch.cursor.is_some())
                    }) =>
            {
                return Err(GraphError::CorruptRun(
                    "budget-stopped Run must retain its cursor".into(),
                ));
            }
            RunStatus::Completed if self.cursor.is_some() || self.parallel.is_some() => {
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
        if self.cursor.is_some()
            && self.parallel.as_ref().is_some_and(|activation| {
                self.cursor
                    .as_ref()
                    .is_none_or(|cursor| cursor.node_id != activation.join_node)
            })
        {
            return Err(GraphError::CorruptRun(
                "serial cursor cannot coexist with a parallel activation".into(),
            ));
        }
        if let Some(activation) = &self.parallel {
            let regions = self.snapshot.parallel_regions().map_err(|error| {
                GraphError::CorruptRun(format!("invalid parallel snapshot: {error}"))
            })?;
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
            if self
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.node_id == activation.join_node)
                && activation
                    .branches
                    .iter()
                    .any(|branch| branch.status != ParallelBranchStatus::Completed)
            {
                return Err(GraphError::CorruptRun(
                    "join cursor requires every branch in this activation to complete".into(),
                ));
            }
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
                let cursor_matches = branch.cursor.as_ref().is_some_and(|cursor| {
                    path.get(branch.next_index).is_some_and(|node_id| {
                        cursor.node_id == *node_id
                            && cursor.key.run_id == self.run_id
                            && cursor.key.graph_digest == self.graph_digest
                            && cursor.key.node_id == cursor.node_id
                            && cursor.key.invocation > 0
                            && self.passes.get(&cursor.node_id).copied().unwrap_or(0) > 0
                            && self.invocations.get(&cursor.node_id).copied()
                                == Some(cursor.key.invocation)
                            && expected_input_commits(self, &cursor.node_id)
                                .is_ok_and(|expected| expected == cursor.input_commits)
                    })
                });
                let valid_state = match branch.status {
                    ParallelBranchStatus::Ready => {
                        branch.cursor.is_none() && branch.next_index < path.len()
                    }
                    ParallelBranchStatus::Running => cursor_matches,
                    ParallelBranchStatus::Completed => {
                        branch.cursor.is_none()
                            && branch.next_index == path.len()
                            && branch.error.is_none()
                    }
                    ParallelBranchStatus::Failed => {
                        branch.cursor.is_none() && branch.error.is_some()
                    }
                    ParallelBranchStatus::Stopped => branch.cursor.is_none() || cursor_matches,
                };
                if !valid_state {
                    return Err(GraphError::CorruptRun(format!(
                        "parallel branch `{index}` status disagrees with its cursor"
                    )));
                }
            }
            for path in &region.branches {
                let key = edge_key(&activation.fanout_node, &path[0]);
                if !self.decided.get(&key).is_some_and(|decision| {
                    decision.selected
                        && decision.source_invocation == activation.fanout_invocation
                        && decision.result_sequence == fanout_result.sequence
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
            let expected_input_commits = self
                .parallel
                .as_ref()
                .filter(|activation| activation.join_node == cursor.node_id)
                .map(|activation| {
                    activation
                        .branches
                        .iter()
                        .flat_map(|branch| branch.completed.iter().cloned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or(expected_input_commits);
            if cursor.input_commits != expected_input_commits {
                return Err(GraphError::CorruptRun(format!(
                    "cursor input commits do not match selected inputs for `{}`",
                    cursor.node_id
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn migrate_format(&mut self) -> Result<(), GraphError> {
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
                self.migrate_format()
            }
            2 => {
                self.parallel = None;
                self.format = 3;
                Ok(())
            }
            3 => Ok(()),
            other => Err(GraphError::CorruptRun(format!(
                "unsupported graph run format {other}"
            ))),
        }
    }
}
