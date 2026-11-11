//! A Graph as the validator of a Run.
//!
//! The Run is a kernel task resource whose state is the frozen Graph and its
//! progress. The validator never calls anyone: [`open`](GraphValidator::open)
//! says which node invocations it would currently accept, and
//! [`judge`](GraphValidator::judge) applies a submitted result, chooses the
//! route and decides whether the Run has settled. Stop, pause, resume and
//! abandon are not here at all: they are the kernel's task lifecycle.
//!
//! Parallel regions are not a special execution path: a fanout opens one slot
//! per branch, and the join opens once every branch has completed. Whether a
//! scheduler runs the open slots in waves or as soon as they open is the
//! scheduler's choice, not the Graph's.
//!
//! Op.call is not a special path either: host code binds the child Run, the
//! Run waits on that child as a kernel waiter, and host code submits the
//! child's settlement. A detached call's output is built here, so a child's
//! business result can never leak through a detach.
//!
//! Covered: serial routing, feedback edges, skipped paths, node and module
//! round limits (including modules in parallel branches), fanout/join,
//! Op.call wait/detach.

use anchor_kernel::{
    Action, Evidence, JudgeCtx, Judgement, Outcome, Phase, PrincipalKind, Reason, Resource, Slot,
    Submission, Validator, Verification,
};
use serde::Deserialize;

use super::*;

/// The validator id a host installs this under.
pub const GRAPH_VALIDATOR: &str = "validator/graph";

pub struct GraphValidator;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchKind {
    /// Work for an executor: an Agent, Op.run or host operation.
    Work,
    /// Deterministic control, submitted only by host code.
    Fanout,
    Join,
    /// An Op.call: host code binds and settles a child Run.
    Call,
}

/// One node invocation a Run would currently accept.
#[derive(Debug, Clone, PartialEq)]
pub struct Dispatch {
    pub node: GraphNode,
    pub invocation: u64,
    pub kind: DispatchKind,
    /// The parallel branch this invocation belongs to, if any.
    pub branch: Option<usize>,
    /// Committed inputs, computed after the same advance that chose the node:
    /// a stale selected edge retired by that advance is not an input.
    pub inputs: Vec<CommitRef>,
}

impl Dispatch {
    pub fn slot(&self) -> String {
        format!("{}#{}", self.node.id, self.invocation)
    }
}

/// Where a Run stands after applying everything known so far.
#[derive(Debug, Clone, PartialEq)]
pub enum Advance {
    Open(Vec<Dispatch>),
    /// No node can run again.
    Done,
    /// The Graph cannot continue, for example a parallel branch hit its
    /// round limit.
    Failed(String),
}

/// The content of a node submission. `commit` is the immutable artifact the
/// executor froze before submitting; the Graph never freezes anything.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSubmission {
    pub summary: String,
    /// A host-observed interruption, using the canonical control completion.
    /// The executor must freeze `commit` with [`ArtifactKind::Interruption`]
    /// without freezing the interrupted node workspace. A `CommitRef` carries
    /// identity only; the validator does not inspect artifacts or perform I/O.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interruption: Option<String>,
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub output: Value,
    #[serde(default)]
    pub model_requests: u64,
    pub commit: CommitRef,
    #[serde(default)]
    pub plugin_views: BTreeMap<String, String>,
    #[serde(default)]
    pub plugin_digests: BTreeMap<String, String>,
}

/// What host code reports when a node fails: the Run cannot continue.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettleSubmission {
    pub reason: String,
}

/// What host code reports about one Op.call invocation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "call", rename_all = "snake_case", deny_unknown_fields)]
pub enum CallSubmission {
    /// Host code proved that this identity has no admitted child. This keeps
    /// a pre-admission failure distinct from a child that settled as failed.
    AdmissionFailed { reason: String },
    /// A wait-mode child was admitted; the Run now waits on it.
    Bound { child_run_id: String },
    /// The child settled (wait) or was accepted (detach).
    Completed {
        child_run_id: String,
        #[serde(default)]
        output: Value,
        commit: CommitRef,
    },
    Failed {
        child_run_id: Option<String>,
        reason: String,
    },
    /// Whether the child exists is not known.
    Uncertain {
        child_run_id: Option<String>,
        reason: String,
    },
}

/// The durable identity of one Op.call invocation. A host admits the child
/// idempotently under it; the Run binds exactly one child to it.
pub fn call_identity(
    record: &GraphRunRecord,
    dispatch: &Dispatch,
) -> Result<CallIdentity, GraphError> {
    let spec = op_of(record, &dispatch.node)
        .and_then(|op| op.get("call"))
        .ok_or_else(|| {
            GraphError::InvalidSnapshot(format!("`{}` is not an Op.call", dispatch.node.id))
        })?;
    let bytes = serde_json::to_vec(spec).map_err(GraphError::SnapshotDecode)?;
    Ok(CallIdentity {
        parent_run_id: record.run_id.clone(),
        parent_graph_digest: record.graph_digest.clone(),
        node_id: dispatch.node.id.clone(),
        invocation: dispatch.invocation,
        call_spec_digest: format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(bytes)),
    })
}

/// The canonical Kernel resource id for a business Run id.
pub fn run_resource_id(run_id: &str) -> String {
    format!("run/{run_id}")
}

/// The kernel resource for a new Run.
pub fn run_resource(record: &GraphRunRecord, budget: u64) -> Result<Resource, GraphError> {
    // Host code cannot choose a route for a call, so a call with several exits
    // is an authoring error found here, not a failure found mid-Run.
    for node in &record.snapshot.nodes {
        if op_of(record, node).is_some_and(|op| op.get("call").is_some())
            && record
                .snapshot
                .edges
                .iter()
                .filter(|e| e.from_node == node.id)
                .count()
                > 1
        {
            return Err(GraphError::InvalidSnapshot(format!(
                "Op.call node `{}` cannot choose between multiple outgoing edges",
                node.id
            )));
        }
    }
    record.snapshot.parallel_regions()?;
    let state = serde_json::to_value(record).map_err(GraphError::SnapshotDecode)?;
    Ok(Resource::new(run_resource_id(&record.run_id), GRAPH_VALIDATOR, state).task(budget))
}

pub fn record_of(resource: &Resource) -> Result<GraphRunRecord, GraphError> {
    GraphRunRecord::deserialize(&resource.state).map_err(GraphError::SnapshotDecode)
}

/// Committed inputs of a dispatch: the selected incoming edges' source results.
fn dispatch_inputs(record: &GraphRunRecord, node_id: &str) -> Vec<CommitRef> {
    record
        .snapshot
        .edges
        .iter()
        .filter(|e| e.to_node == node_id)
        .filter_map(|e| {
            let decision = record
                .decided
                .get(&edge_key(&e.from_node, &e.to_node))
                .filter(|d| d.selected)?;
            let sequence = if decision.result_sequence == 0 {
                decision.sequence
            } else {
                decision.result_sequence
            };
            record
                .results
                .get(&e.from_node)?
                .iter()
                .find(|r| r.sequence == sequence)
                .map(|r| r.commit.clone())
        })
        .collect()
}

fn op_of<'a>(record: &'a GraphRunRecord, node: &GraphNode) -> Option<&'a Value> {
    node.op
        .as_ref()
        .and_then(|name| record.snapshot.ops.get(name))
}

/// Advance as far as the Graph allows, applying round limits and skipped
/// paths on the way. Mutates exactly what the Runner mutated before dispatch.
pub(crate) fn prepare(record: &mut GraphRunRecord) -> Result<Advance, GraphError> {
    prepare_submission(record, None)
}

fn prepare_submission(
    record: &mut GraphRunRecord,
    submitted_slot: Option<&str>,
) -> Result<Advance, GraphError> {
    loop {
        propagate_inactive_edges(record)?;
        if let Some(activation) = &record.parallel
            && activation
                .branches
                .iter()
                .any(|b| b.status != ParallelBranchStatus::Completed)
        {
            return parallel_frontier(record, submitted_slot);
        }
        // Started but without a result: the same invocation continues (a call
        // whose child was bound). It was counted when it started, so round
        // limits and module entry were already applied to it. This is what
        // the old Runner's cursor remembered; here it is derived from facts.
        if let Some(node) = record
            .snapshot
            .nodes
            .iter()
            .find(|n| {
                let started = record.invocations.get(&n.id).copied().unwrap_or(0);
                let finished = record
                    .results
                    .get(&n.id)
                    .and_then(|r| r.iter().map(|x| x.key.invocation).max())
                    .unwrap_or(0);
                started > finished
            })
            .cloned()
        {
            let invocation = record.invocations[&node.id];
            let inputs = dispatch_inputs(record, &node.id);
            let kind = if op_of(record, &node).is_some_and(|op| op.get("call").is_some()) {
                DispatchKind::Call
            } else {
                DispatchKind::Work
            };
            return Ok(Advance::Open(vec![Dispatch {
                node,
                invocation,
                kind,
                branch: None,
                inputs,
            }]));
        }
        let Some(node) = next_node(record)? else {
            return Ok(Advance::Done);
        };
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
                .filter(|e| e.to_node == node.id && !e.from_node.starts_with(&format!("{scope}/")))
                .any(|e| {
                    record
                        .decided
                        .get(&edge_key(&e.from_node, &e.to_node))
                        .is_some_and(|d| d.selected && d.sequence > prior_sequence)
                });
        if enters_scope && let Some(limit) = record.snapshot.module_rounds.get(&scope).copied() {
            let next = record.module_activations.get(&scope).copied().unwrap_or(0) + 1;
            if next > u64::from(limit) {
                record.ceased.insert(format!("{scope}@{limit}"));
                refuse_module_activation(record, &scope, &node.id)?;
                continue;
            }
            record.module_activations.insert(scope.clone(), next);
            let members = record
                .snapshot
                .nodes
                .iter()
                .filter(|m| m.id.starts_with(&format!("{scope}/")))
                .map(|m| m.id.clone())
                .collect::<Vec<_>>();
            for member in members {
                record.passes.remove(&member);
            }
            let nested = record
                .module_activations
                .keys()
                .filter(|n| n.starts_with(&format!("{scope}/")))
                .cloned()
                .collect::<Vec<_>>();
            for n in nested {
                record.module_activations.remove(&n);
            }
        }
        if let Some(limit) = node.max_rounds
            && record.passes.get(&node.id).copied().unwrap_or(0) >= u64::from(limit)
        {
            record.ceased.insert(format!("{}@{limit}", node.id));
            refuse_outgoing(record, &node.id);
            continue;
        }
        let invocation = record.invocations.get(&node.id).copied().unwrap_or(0) + 1;
        let op = op_of(record, &node);
        let is_join = op.is_some_and(|op| op.get("join").is_some())
            && record
                .parallel
                .as_ref()
                .is_some_and(|a| a.join_node == node.id);
        let (kind, inputs) = if op.is_some_and(|op| op.get("fanout").is_some()) {
            (DispatchKind::Fanout, dispatch_inputs(record, &node.id))
        } else if op.is_some_and(|op| op.get("call").is_some()) {
            (DispatchKind::Call, dispatch_inputs(record, &node.id))
        } else if is_join {
            let inputs = record
                .parallel
                .as_ref()
                .expect("join activation")
                .branches
                .iter()
                .flat_map(|b| b.completed.iter().cloned())
                .collect();
            (DispatchKind::Join, inputs)
        } else {
            (DispatchKind::Work, dispatch_inputs(record, &node.id))
        };
        return Ok(Advance::Open(vec![Dispatch {
            node,
            invocation,
            kind,
            branch: None,
            inputs,
        }]));
    }
}

/// One slot per unfinished branch: the branch's next node.
fn parallel_frontier(
    record: &mut GraphRunRecord,
    submitted_slot: Option<&str>,
) -> Result<Advance, GraphError> {
    let activation = record.parallel.clone().expect("active parallel region");
    let mut open = Vec::new();
    let mut submitted_state = None;
    for (index, branch) in activation.branches.iter().enumerate() {
        if branch.status == ParallelBranchStatus::Completed {
            continue;
        }
        let node_id = &branch.nodes[branch.next_index];
        let node = record
            .snapshot
            .nodes
            .iter()
            .find(|n| &n.id == node_id)
            .cloned()
            .expect("validated parallel branch node");
        // Preview each candidate independently. Only the submitted slot's
        // entry effects are committed: observing another open branch is not
        // an invocation and must not consume its module activation.
        let mut candidate = record.clone();
        let scope = scope_of(&node.id);
        let prefix = format!("{scope}/");
        let prior_sequence = record
            .results
            .get(&node.id)
            .and_then(|results| results.last())
            .map(|result| result.sequence)
            .unwrap_or(0);
        let scope_started = activation.branches.iter().any(|branch| {
            branch
                .nodes
                .iter()
                .take(branch.next_index)
                .any(|node| node.starts_with(&prefix))
        });
        let enters_scope = !scope.is_empty()
            && !scope_started
            && record
                .snapshot
                .edges
                .iter()
                .filter(|edge| edge.to_node == node.id && !edge.from_node.starts_with(&prefix))
                .any(|edge| {
                    record
                        .decided
                        .get(&edge_key(&edge.from_node, &edge.to_node))
                        .is_some_and(|decision| {
                            decision.selected && decision.sequence > prior_sequence
                        })
                });
        if enters_scope && let Some(limit) = record.snapshot.module_rounds.get(&scope).copied() {
            let next = record.module_activations.get(&scope).copied().unwrap_or(0) + 1;
            if next > u64::from(limit) {
                record.ceased.insert(format!("{scope}@{limit}"));
                refuse_module_activation(record, &scope, &node.id)?;
                return Ok(Advance::Failed(format!(
                    "parallel branch `{}` exceeded module activation ceiling `{scope}@{limit}`",
                    node.id
                )));
            }
            candidate.module_activations.insert(scope.clone(), next);
            let members = candidate
                .snapshot
                .nodes
                .iter()
                .filter(|member| member.id.starts_with(&prefix))
                .map(|member| member.id.clone())
                .collect::<Vec<_>>();
            for member in members {
                candidate.passes.remove(&member);
            }
            let nested = candidate
                .module_activations
                .keys()
                .filter(|nested| nested.starts_with(&prefix))
                .cloned()
                .collect::<Vec<_>>();
            for nested in nested {
                candidate.module_activations.remove(&nested);
            }
        }
        if let Some(limit) = node.max_rounds
            && candidate.passes.get(&node.id).copied().unwrap_or(0) >= u64::from(limit)
        {
            record.ceased.insert(format!("{}@{limit}", node.id));
            refuse_outgoing(record, &node.id);
            return Ok(Advance::Failed(format!(
                "parallel branch `{}` exceeded node round ceiling `{limit}`",
                node.id
            )));
        }
        let invocation = record.invocations.get(&node.id).copied().unwrap_or(0) + 1;
        let inputs = dispatch_inputs(record, &node.id);
        let dispatch = Dispatch {
            node,
            invocation,
            kind: DispatchKind::Work,
            branch: Some(index),
            inputs,
        };
        if submitted_slot == Some(dispatch.slot().as_str()) {
            submitted_state = Some(candidate);
        }
        open.push(dispatch);
    }
    if let Some(submitted_state) = submitted_state {
        *record = submitted_state;
    }
    Ok(Advance::Open(open))
}

fn refuse_outgoing(record: &mut GraphRunRecord, node_id: &str) {
    record.sequence += 1;
    let invocation = record.invocations.get(node_id).copied().unwrap_or(0);
    let targets = record
        .snapshot
        .edges
        .iter()
        .filter(|e| e.from_node == node_id)
        .map(|e| e.to_node.clone())
        .collect::<Vec<_>>();
    for target in targets {
        record.decided.insert(
            edge_key(node_id, &target),
            EdgeDecision {
                selected: false,
                sequence: record.sequence,
                source_invocation: invocation,
                result_sequence: 0,
            },
        );
    }
}

impl GraphValidator {
    /// The dispatches the Run would currently accept, computed without
    /// mutating the stored state (the same computation reruns when judged).
    pub fn frontier(resource: &Resource) -> Result<Vec<Dispatch>, GraphError> {
        Self::frontier_owned(record_of(resource)?)
    }

    /// The same read-only frontier for an already decoded Run record.
    pub fn frontier_from_record(record: &GraphRunRecord) -> Result<Vec<Dispatch>, GraphError> {
        Self::frontier_owned(record.clone())
    }

    fn frontier_owned(mut record: GraphRunRecord) -> Result<Vec<Dispatch>, GraphError> {
        Ok(match prepare(&mut record)? {
            Advance::Open(open) => open,
            Advance::Done | Advance::Failed(_) => Vec::new(),
        })
    }
}

impl Validator for GraphValidator {
    fn open(&self, resource: &Resource) -> Vec<Slot> {
        Self::frontier(resource)
            .unwrap_or_default()
            .iter()
            .map(|d| Slot {
                id: d.slot(),
                action: Action::Write,
            })
            .collect()
    }

    fn judge(&self, ctx: &JudgeCtx, s: &Submission) -> Judgement {
        let Ok(mut record) = record_of(ctx.resource) else {
            return Judgement::refused(Reason::Unclassified);
        };
        let Ok(Advance::Open(open)) = prepare_submission(&mut record, s.slot.as_deref()) else {
            return Judgement::refused(Reason::SlotClosed);
        };
        let Some(dispatch) = open
            .into_iter()
            .find(|d| s.slot.as_deref() == Some(d.slot().as_str()))
        else {
            return Judgement::refused(Reason::SlotClosed);
        };
        if s.action == Action::Settle {
            return judge_settle(record, &dispatch, s, ctx);
        }
        if s.action != Action::Write {
            return Judgement::refused(Reason::SlotClosed);
        }
        // Inspect the original object: explicit null still claims this host
        // field, even though Option<String> would deserialize it as None.
        if s.content.get("interruption").is_some() && ctx.principal.kind != PrincipalKind::Code {
            return Judgement::refused(Reason::Unauthorized);
        }
        // Control nodes are deterministic host code; no proposer can fake one.
        if dispatch.kind != DispatchKind::Work && ctx.principal.kind != PrincipalKind::Code {
            return Judgement::refused(Reason::Unauthorized);
        }
        if dispatch.kind == DispatchKind::Call {
            return judge_call(record, &dispatch, s);
        }
        let Ok(content) = serde_json::from_value::<NodeSubmission>(s.content.clone()) else {
            return Judgement::returned(Reason::ContentInvalid);
        };
        let node_id = dispatch.node.id.clone();
        if content.commit.node_id != node_id || content.commit.invocation != dispatch.invocation {
            return Judgement::returned(Reason::EvidenceMissing);
        }
        let completion = NodeCompletion {
            submission: content.summary,
            route: content.route,
            model_requests: content.model_requests,
            output: content.output,
        };
        if let Some(reason) = &content.interruption {
            // Preserve the existing RunResult control-evidence contract: an
            // interrupted invocation cannot masquerade as a model success,
            // and interruptions in parallel regions remain unsupported.
            if dispatch.kind != DispatchKind::Work || dispatch.branch.is_some() {
                return Judgement::refused(Reason::ContentInvalid);
            }
            if completion != interruption_completion(reason, completion.route.clone()) {
                return Judgement::returned(Reason::ContentInvalid);
            }
        }
        let routes = routes_of(&record, &node_id);
        if dispatch.branch.is_some() && routes.len() != 1 {
            return Judgement::refused(Reason::RouteInvalid);
        }
        let chosen = if dispatch.kind == DispatchKind::Fanout {
            None
        } else {
            // An invalid route is fixable by the proposer: returned, not a failed Run.
            match select_route(&completion, &routes) {
                Ok(route) => route,
                Err(_) => return Judgement::returned(Reason::RouteInvalid),
            }
        };
        count_dispatch(&mut record, &dispatch);
        let commit = content.commit.clone();
        record_result(
            &mut record,
            &dispatch,
            completion,
            commit.clone(),
            (
                content.plugin_views,
                content.plugin_digests,
                content.interruption,
            ),
            &routes,
            chosen.as_ref(),
        );
        match (dispatch.kind, dispatch.branch) {
            (DispatchKind::Fanout, _) => {
                let Ok(regions) = record.snapshot.parallel_regions() else {
                    return Judgement::refused(Reason::Unclassified);
                };
                let region = regions[&node_id].clone();
                let activation_id =
                    format!("{}:{}:{}", record.run_id, node_id, dispatch.invocation);
                record.parallel = Some(ParallelActivation {
                    activation_id: activation_id.clone(),
                    fanout_node: node_id.clone(),
                    join_node: region.join,
                    fanout_invocation: dispatch.invocation,
                    branches: region
                        .branches
                        .into_iter()
                        .enumerate()
                        .map(|(index, nodes)| ParallelBranchRecord {
                            branch_id: format!("{activation_id}:branch:{index}"),
                            entry: nodes[0].clone(),
                            nodes,
                            next_index: 0,
                            status: ParallelBranchStatus::Ready,
                            completed: Vec::new(),
                            error: None,
                        })
                        .collect(),
                });
            }
            (DispatchKind::Join, _) => record.parallel = None,
            (DispatchKind::Work, Some(index)) => {
                let branch = &mut record.parallel.as_mut().expect("parallel").branches[index];
                branch.completed.push(commit);
                branch.next_index += 1;
                branch.status = if branch.next_index == branch.nodes.len() {
                    ParallelBranchStatus::Completed
                } else {
                    ParallelBranchStatus::Ready
                };
            }
            (DispatchKind::Work | DispatchKind::Call, _) => {}
        }
        conclude(record, verification_of(s))
    }
}

fn judge_settle(
    mut record: GraphRunRecord,
    dispatch: &Dispatch,
    s: &Submission,
    ctx: &JudgeCtx,
) -> Judgement {
    if ctx.principal.kind != PrincipalKind::Code {
        return Judgement::refused(Reason::Unauthorized);
    }
    if dispatch.kind != DispatchKind::Work {
        return Judgement::refused(Reason::SlotClosed);
    }
    let Ok(content) = serde_json::from_value::<SettleSubmission>(s.content.clone()) else {
        return Judgement::returned(Reason::ContentInvalid);
    };
    count_dispatch(&mut record, dispatch);
    if let Some(index) = dispatch.branch {
        let branch = &mut record.parallel.as_mut().expect("parallel").branches[index];
        branch.status = ParallelBranchStatus::Failed;
        branch.error = Some(content.reason.clone());
    }
    let phase = Phase::Settled {
        result: Outcome::Failed {
            reason: Reason::Unclassified,
            detail: format!("{} failed: {}", dispatch.node.id, content.reason),
        },
    };
    accept(&record, verification_of(s), Some(phase))
}

pub(crate) fn routes_of(record: &GraphRunRecord, node_id: &str) -> Vec<String> {
    record
        .snapshot
        .edges
        .iter()
        .filter(|e| e.from_node == node_id)
        .map(|e| e.to_node.clone())
        .collect()
}

fn verification_of(s: &Submission) -> Verification {
    if s.evidence
        .iter()
        .any(|e| matches!(e, Evidence::Resource { .. }))
    {
        Verification::Evidenced
    } else {
        Verification::Asserted
    }
}

/// An invocation counts as started when its first fact is accepted.
fn count_dispatch(record: &mut GraphRunRecord, dispatch: &Dispatch) {
    record
        .invocations
        .insert(dispatch.node.id.clone(), dispatch.invocation);
    *record.passes.entry(dispatch.node.id.clone()).or_default() += 1;
}

fn record_result(
    record: &mut GraphRunRecord,
    dispatch: &Dispatch,
    completion: NodeCompletion,
    commit: CommitRef,
    (plugin_views, plugin_digests, interruption): (
        BTreeMap<String, String>,
        BTreeMap<String, String>,
        Option<String>,
    ),
    routes: &[String],
    chosen: Option<&String>,
) {
    let node_id = dispatch.node.id.clone();
    record.sequence += 1;
    let result_sequence = record.sequence;
    record
        .results
        .entry(node_id.clone())
        .or_default()
        .push(RunResult {
            node_id: node_id.clone(),
            key: InvocationKey {
                run_id: record.run_id.clone(),
                graph_digest: record.graph_digest.clone(),
                node_id: node_id.clone(),
                invocation: dispatch.invocation,
            },
            plugin_views,
            plugin_digests,
            completion,
            interruption,
            commit,
            sequence: result_sequence,
        });
    record.sequence += 1;
    for target in routes {
        record.decided.insert(
            edge_key(&node_id, target),
            EdgeDecision {
                selected: dispatch.kind == DispatchKind::Fanout || Some(target) == chosen,
                sequence: record.sequence,
                source_invocation: dispatch.invocation,
                result_sequence,
            },
        );
    }
}

/// Accept the new state, settling the Run if nothing can run again. The
/// look-ahead is persisted only when it shows the Run is over, so the next
/// node's dispatch-time effects are applied exactly once, when it is judged.
fn conclude(record: GraphRunRecord, verification: Verification) -> Judgement {
    let mut ahead = record.clone();
    let (record, phase) = match prepare(&mut ahead) {
        Ok(Advance::Open(_)) => (record, None),
        Ok(Advance::Done) if ahead.ceased.is_empty() => (
            ahead,
            Some(Phase::Settled {
                result: Outcome::Done,
            }),
        ),
        Ok(Advance::Done) => {
            let detail = format!(
                "round limit reached: {}",
                ahead.ceased.iter().cloned().collect::<Vec<_>>().join(", ")
            );
            (
                ahead,
                Some(Phase::Settled {
                    result: Outcome::Escalated { detail },
                }),
            )
        }
        Ok(Advance::Failed(detail)) => (
            ahead,
            Some(Phase::Settled {
                result: Outcome::Failed {
                    reason: Reason::Unclassified,
                    detail,
                },
            }),
        ),
        Err(_) => return Judgement::refused(Reason::Unclassified),
    };
    accept(&record, verification, phase)
}

fn accept(record: &GraphRunRecord, verification: Verification, phase: Option<Phase>) -> Judgement {
    let Ok(state) = serde_json::to_value(record) else {
        return Judgement::refused(Reason::Unclassified);
    };
    let judgement = Judgement::accept(state, verification);
    match phase {
        Some(phase) => judgement.with_phase(phase),
        None => judgement,
    }
}

/// One identity binds one child. Bound puts the Run in a kernel wait; the
/// kernel lets host code release it when the child settles.
fn judge_call(mut record: GraphRunRecord, dispatch: &Dispatch, s: &Submission) -> Judgement {
    let Ok(content) = serde_json::from_value::<CallSubmission>(s.content.clone()) else {
        return Judgement::returned(Reason::ContentInvalid);
    };
    let Some(spec) = op_of(&record, &dispatch.node)
        .and_then(|op| op.get("call"))
        .cloned()
    else {
        return Judgement::refused(Reason::Unclassified);
    };
    let mode = spec["mode"].as_str().unwrap_or_default().to_owned();
    let graph = spec["graph"].as_str().unwrap_or_default().to_owned();
    let Ok(identity) = call_identity(&record, dispatch) else {
        return Judgement::refused(Reason::Unclassified);
    };
    let key = identity.durable_key();
    let bound = record
        .graph_calls
        .get(&key)
        .and_then(|call| call.child_run_id.clone());
    if matches!(content, CallSubmission::AdmissionFailed { .. })
        && record.graph_calls.contains_key(&key)
    {
        return Judgement::refused(Reason::ContentInvalid);
    }
    let child = match &content {
        CallSubmission::AdmissionFailed { .. } => None,
        CallSubmission::Bound { child_run_id } | CallSubmission::Completed { child_run_id, .. } => {
            Some(child_run_id.clone())
        }
        CallSubmission::Failed { child_run_id, .. }
        | CallSubmission::Uncertain { child_run_id, .. } => child_run_id.clone(),
    };
    if bound.is_some() && child != bound {
        // A different child under a stable identity would fork or replay work.
        return Judgement::refused(Reason::ContentInvalid);
    }
    if !record.graph_calls.contains_key(&key) {
        count_dispatch(&mut record, dispatch);
    }
    let call_record = |status, output: Option<Value>, error: Option<String>| GraphCallRecord {
        identity: identity.clone(),
        graph: graph.clone(),
        child_run_id: child.clone(),
        mode: mode.clone(),
        status,
        output,
        error,
    };
    match content {
        CallSubmission::AdmissionFailed { reason } => accept(
            &record,
            verification_of(s),
            Some(Phase::Settled {
                result: Outcome::Failed {
                    reason: Reason::Unclassified,
                    detail: format!("Graph call admission failed: {reason}"),
                },
            }),
        ),
        CallSubmission::Bound { child_run_id } => {
            if mode != "wait" {
                return Judgement::returned(Reason::ContentInvalid);
            }
            record
                .graph_calls
                .insert(key, call_record(GraphCallStatus::Waiting, None, None));
            accept(
                &record,
                Verification::Asserted,
                Some(Phase::Waiting {
                    waiter: anchor_kernel::Waiter::Child {
                        task: format!("run/{child_run_id}"),
                    },
                }),
            )
        }
        CallSubmission::Completed {
            child_run_id,
            output,
            commit,
        } => {
            if commit.node_id != dispatch.node.id || commit.invocation != dispatch.invocation {
                return Judgement::returned(Reason::EvidenceMissing);
            }
            // A detach returns the accepted reference only, whatever was sent.
            let (status, output) = if mode == "detach" {
                (
                    GraphCallStatus::Detached,
                    serde_json::json!({"graph": graph, "run_id": child_run_id,
                        "mode": "detach", "status": "accepted"}),
                )
            } else {
                (GraphCallStatus::Completed, output)
            };
            record
                .graph_calls
                .insert(key, call_record(status, Some(output.clone()), None));
            let completion = NodeCompletion {
                submission: output.to_string(),
                route: None,
                model_requests: 0,
                output,
            };
            let routes = routes_of(&record, &dispatch.node.id);
            let Ok(chosen) = select_route(&completion, &routes) else {
                return Judgement::refused(Reason::RouteInvalid);
            };
            record_result(
                &mut record,
                dispatch,
                completion,
                commit,
                Default::default(),
                &routes,
                chosen.as_ref(),
            );
            conclude(record, verification_of(s))
        }
        CallSubmission::Failed { reason, .. } => {
            record.graph_calls.insert(
                key,
                call_record(GraphCallStatus::Failed, None, Some(reason.clone())),
            );
            accept(
                &record,
                Verification::Asserted,
                Some(Phase::Settled {
                    result: Outcome::Failed {
                        reason: Reason::Unclassified,
                        detail: format!("Graph call failed: {reason}"),
                    },
                }),
            )
        }
        CallSubmission::Uncertain { reason, .. } => {
            record.graph_calls.insert(
                key,
                call_record(GraphCallStatus::Uncertain, None, Some(reason.clone())),
            );
            accept(
                &record,
                Verification::Asserted,
                Some(Phase::Settled {
                    result: Outcome::Unknown {
                        detail: format!("Graph call outcome unknown: {reason}"),
                    },
                }),
            )
        }
    }
}

#[cfg(test)]
pub(crate) mod differential;

#[cfg(test)]
mod interruption_tests;

#[cfg(test)]
mod settle_tests;

#[cfg(test)]
mod parallel_module_tests;
