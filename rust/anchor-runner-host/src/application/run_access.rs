mod kernel_calls;
pub(super) mod kernel_runs;
mod run_observations;
pub(super) mod run_view;
use super::{RunApplication, new_control, storage};
use anchor_kernel::{Outcome, Phase, Waiter};
mod abandon;
mod admission;
mod artifact_files;
mod assistant_policy;
mod channels;
mod control;
mod conversations;
mod deletion;
mod dispatch;
mod file_facts;
mod graph_policy;
pub(crate) use artifact_files::ArtifactAccessError;
pub(crate) mod http;
pub(super) mod projection;
mod reads;
mod recovery;
mod session_calls;
mod store;
pub(crate) mod workspaces;

pub(crate) use assistant_policy::{graph_lease, graph_read_lease};
pub(crate) use projection::projected_run_is_unfinished;
pub(crate) use reads::{load, status_projection};
use store::wait_child_ids;

use super::{ApplicationError, RunMetadata};
use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot, PluginBinding, RunResult};
use std::{borrow::Cow, collections::BTreeMap};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunPhase<'record> {
    Executing,
    Waiting {
        for_what: RunWait,
        reason: Option<&'record str>,
    },
    Settled {
        outcome: RunOutcome,
        reason: Option<&'record str>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunWait {
    Human,
    Child,
    Effect,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunOutcome {
    Done,
    Failed,
    Escalated,
    Unknown,
    Aborted,
}

#[derive(Clone)]
pub(crate) struct RunProgress<'record> {
    pub(crate) run_id: &'record str,
    pub(crate) graph_digest: &'record str,
    pub(crate) snapshot: &'record GraphSnapshot,
    pub(crate) results: &'record BTreeMap<String, Vec<RunResult>>,
    pub(crate) plugin_bindings: &'record BTreeMap<String, PluginBinding>,
    pub(crate) invocations: &'record BTreeMap<String, u64>,
    executed: Vec<String>,
}

impl<'record> From<&'record GraphRunRecord> for RunProgress<'record> {
    fn from(record: &'record GraphRunRecord) -> Self {
        Self {
            run_id: &record.run_id,
            graph_digest: &record.graph_digest,
            snapshot: &record.snapshot,
            results: &record.results,
            plugin_bindings: &record.plugin_bindings,
            invocations: &record.invocations,
            executed: record.executed_nodes(),
        }
    }
}
impl<'view> From<&'view RunView<'_>> for RunProgress<'view> {
    fn from(view: &'view RunView<'_>) -> Self {
        view.progress()
    }
}
impl<'view, 'record: 'view> From<&'view RunProgress<'record>> for RunProgress<'view> {
    fn from(progress: &'view RunProgress<'record>) -> Self {
        progress.clone()
    }
}

impl RunProgress<'_> {
    pub(crate) fn executed_nodes(&self) -> &[String] {
        &self.executed
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RunView<'record> {
    record: Cow<'record, GraphRunRecord>,
    active: bool,
    requested: Option<&'static str>,
    session_coordinated: bool,
    lifecycle: run_view::RunView,
    // Read-only Kernel history projection, never Graph state.
    recovery: Vec<anchor_runtime::graph::PendingRecovery>,
    // Read-only validator dispatch, never a saved execution cursor.
    frontier: Vec<anchor_runtime::graph::scheduler::DispatchInput>,
}

impl<'record> RunView<'record> {
    pub(crate) fn with_active(mut self, active: bool) -> Self {
        self.active |= active;
        self.lifecycle.worker = self.active;
        self
    }

    pub(crate) fn phase(&self) -> RunPhase<'_> {
        match &self.lifecycle.phase {
            Phase::Executing => RunPhase::Executing,
            Phase::Waiting { waiter } => match waiter {
                Waiter::Human { reason } => RunPhase::Waiting {
                    for_what: RunWait::Human,
                    reason: Some(reason),
                },
                Waiter::Child { .. } => RunPhase::Waiting {
                    for_what: RunWait::Child,
                    reason: self.lifecycle.reason.as_deref(),
                },
                Waiter::Effect { .. } => RunPhase::Waiting {
                    for_what: RunWait::Effect,
                    reason: self.lifecycle.reason.as_deref(),
                },
            },
            Phase::Settled { result } => {
                let (outcome, reason) = match result {
                    Outcome::Done => (RunOutcome::Done, None),
                    Outcome::Failed { detail, .. } => (RunOutcome::Failed, Some(detail.as_str())),
                    Outcome::Escalated { detail } => (RunOutcome::Escalated, Some(detail.as_str())),
                    Outcome::Unknown { detail } => (RunOutcome::Unknown, Some(detail.as_str())),
                    Outcome::Aborted { detail } => (RunOutcome::Aborted, Some(detail.as_str())),
                };
                RunPhase::Settled { outcome, reason }
            }
        }
    }

    pub(super) fn from_kernel(
        record: GraphRunRecord,
        lifecycle: run_view::RunView,
    ) -> RunView<'static> {
        RunView {
            active: lifecycle.worker,
            record: Cow::Owned(record),
            requested: None,
            session_coordinated: false,
            lifecycle,
            recovery: Vec::new(),
            frontier: Vec::new(),
        }
    }
    pub(crate) fn active(&self) -> bool {
        self.active
    }

    pub(crate) fn dispatches(&self) -> &[anchor_runtime::graph::scheduler::DispatchInput] {
        &self.frontier
    }

    fn cursor(&self) -> Option<&anchor_runtime::graph::scheduler::DispatchInput> {
        self.frontier.first()
    }

    fn with_kernel_frontier(
        mut self,
        resource: &anchor_kernel::Resource,
    ) -> Result<Self, anchor_runtime::graph::GraphError> {
        if !resource.is_settled() {
            self.frontier = anchor_runtime::graph::validator::GraphValidator::frontier(resource)?
                .into_iter()
                .map(|dispatch| anchor_runtime::graph::scheduler::DispatchInput {
                    node_id: dispatch.node.id.clone(),
                    key: anchor_runtime::graph::InvocationKey {
                        run_id: self.record.run_id.clone(),
                        graph_digest: self.record.graph_digest.clone(),
                        node_id: dispatch.node.id,
                        invocation: dispatch.invocation,
                    },
                    input_commits: dispatch.inputs,
                    prepared_input: serde_json::Value::Null,
                    branch: dispatch.branch,
                })
                .collect();
        }
        Ok(self)
    }

    #[allow(
        dead_code,
        reason = "Required read facade contract; current HTTP timestamps retain filesystem metadata semantics"
    )]
    pub(crate) fn updated(&self) -> Option<&str> {
        self.lifecycle.updated.as_deref()
    }

    pub(crate) fn progress(&self) -> RunProgress<'_> {
        RunProgress {
            run_id: &self.record.run_id,
            graph_digest: &self.record.graph_digest,
            snapshot: &self.record.snapshot,
            results: &self.record.results,
            plugin_bindings: &self.record.plugin_bindings,
            invocations: &self.record.invocations,
            executed: self.record.executed_nodes(),
        }
    }

    pub(crate) fn is_settled(&self) -> bool {
        self.lifecycle.is_settled()
    }
    pub(crate) fn is_quiescent(&self) -> bool {
        !self.active
            && !self.has_pending_recovery()
            && matches!(
                self.lifecycle.phase,
                Phase::Settled { .. }
                    | Phase::Waiting {
                        waiter: Waiter::Human { .. }
                    }
            )
    }
    #[allow(
        dead_code,
        reason = "Stable read facade predicate; the phase projection does not serialize a separate attention flag"
    )]
    pub(crate) fn needs_attention(&self) -> bool {
        matches!(
            self.phase(),
            RunPhase::Waiting { .. }
                | RunPhase::Settled {
                    outcome: RunOutcome::Failed
                        | RunOutcome::Escalated
                        | RunOutcome::Unknown
                        | RunOutcome::Aborted,
                    ..
                }
        ) || (!self.active && matches!(self.phase(), RunPhase::Executing))
    }
    pub(crate) fn can_pause(&self) -> bool {
        self.lifecycle.actions().pause && !self.session_coordinated
    }
    pub(crate) fn can_stop(&self) -> bool {
        self.lifecycle.actions().stop
    }
    pub(crate) fn can_resume(&self) -> bool {
        self.lifecycle.actions().resume && !self.session_coordinated
    }
    pub(crate) fn has_stopped_execution(&self) -> bool {
        self.is_settled()
            || matches!(&self.lifecycle.phase,
            Phase::Waiting { waiter: Waiter::Human { reason } }
                if !matches!(reason.as_str(), "paused" | "budget") && !self.has_pending_recovery())
    }
    pub(crate) fn can_dispatch(&self) -> bool {
        self.lifecycle.phase == Phase::Executing
    }

    pub(crate) fn waits_for_children(&self) -> bool {
        matches!(
            self.lifecycle.phase,
            Phase::Waiting {
                waiter: Waiter::Child { .. }
            }
        )
    }

    pub(crate) fn waits_for_invocation(&self, node: &str, invocation: u64) -> bool {
        if !self.waits_for_children() {
            return false;
        }
        let Phase::Waiting {
            waiter: Waiter::Child { task },
        } = &self.lifecycle.phase
        else {
            return false;
        };
        self.record.graph_calls.values().any(|call| {
            call.identity.node_id == node
                && call.identity.invocation == invocation
                && call
                    .child_run_id
                    .as_ref()
                    .is_some_and(|id| *task == format!("run/{id}"))
        })
    }

    pub(crate) fn has_succeeded(&self) -> bool {
        matches!(
            self.phase(),
            RunPhase::Settled {
                outcome: RunOutcome::Done,
                ..
            }
        )
    }

    pub(crate) fn has_pending_recovery(&self) -> bool {
        !self.is_settled() && !self.recovery.is_empty()
    }

    pub(crate) fn occupies_conversation_execution(&self) -> bool {
        self.active || matches!(self.phase(), RunPhase::Executing)
    }

    pub(crate) fn can_abandon(&self) -> bool {
        self.lifecycle.actions().abandon
    }
}

impl<'view> From<&'view RunView<'_>> for RunView<'view> {
    fn from(view: &'view RunView<'_>) -> Self {
        RunView {
            record: Cow::Borrowed(&view.record),
            active: view.active,
            requested: view.requested,
            session_coordinated: view.session_coordinated,
            lifecycle: view.lifecycle.clone(),
            recovery: view.recovery.clone(),
            frontier: view.frontier.clone(),
        }
    }
}

fn validate_control_identity(
    record: &GraphRunRecord,
    metadata: &RunMetadata,
) -> Result<(), ApplicationError> {
    if metadata.graph_digest != record.graph_digest {
        return Err(ApplicationError::Invalid(
            "Run Graph identity metadata does not match its frozen snapshot".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests;
pub(crate) use abandon::AbandonResult;
