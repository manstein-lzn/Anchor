//! Kernel scheduling adapter used by the Host. The Kernel owns all
//! progress and lifecycle facts; this adapter only executes and submits.
use super::validator::{Dispatch, GraphValidator, record_of};
use super::*;
use anchor_kernel::{
    Action, Kernel, Phase, Principal, PrincipalKind, Reason, Resource, Store, Submission, Verdict,
};
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use std::{collections::VecDeque, panic::AssertUnwindSafe};

mod calls;
pub use calls::graph_call_completion;
mod execution;
pub use execution::{dispatch_input, observation_stop};
#[cfg(test)]
mod tests;

/// Input for one live dispatch, derived from the current validator frontier.
/// It is never persisted as a second execution cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchInput {
    pub node_id: String,
    pub key: InvocationKey,
    pub input_commits: Vec<CommitRef>,
    pub prepared_input: Value,
    /// The parallel branch this dispatch runs, when the frontier opened one.
    pub branch: Option<usize>,
}

pub const EXECUTOR_ID: &str = "anchor/executor";
pub const HOST_ID: &str = "anchor/host";
pub const OPERATOR_ID: &str = "anchor/operator";

pub fn kernel_error(error: anchor_kernel::KernelError) -> GraphError {
    GraphError::Io(io::Error::other(error.to_string()))
}

/// Optional Host read observation; called only after an accepted submission.
/// A failed observer cannot undo the accepted Kernel fact.
pub trait SubmissionObserver: Send + Sync {
    fn accepted(&self, resource_id: &str) -> Result<(), GraphError>;
}

/// Host control events are handled between committed node results and the next
/// dispatch. This does not inspect the executor's cancellation or pause flags.
pub trait DispatchBoundary: Send + Sync {
    fn before_dispatch(&self, resource_id: &str) -> Result<(), GraphError>;
}

/// Charge a unit of work once, including stale/late refusals. Repeated stale
/// submissions carry zero additional cost (K0.1/D1).
pub fn submit_with_retry<S: Store>(
    kernel: &Kernel<S>,
    principal: &Principal,
    mut cost: u64,
    mut submission: Submission,
    observer: Option<&dyn SubmissionObserver>,
) -> Result<Verdict, GraphError> {
    loop {
        let verdict = kernel
            .submit(principal, cost, submission.clone())
            .map_err(kernel_error)?;
        if verdict.reason != Reason::StaleVersion {
            if verdict.is_accepted()
                && let Some(observer) = observer
                && let Err(error) = observer.accepted(&submission.target)
            {
                eprintln!(
                    "Host update observation unavailable for {}: {error}",
                    submission.target
                );
            }
            return Ok(verdict);
        }
        cost = 0;
        submission.seen = kernel
            .load(&submission.target)
            .map_err(kernel_error)?
            .ok_or_else(|| GraphError::CorruptRun("Kernel Run disappeared".into()))?
            .version;
    }
}

pub struct KernelScheduler<'a, S: Store, A: ArtifactPort + ?Sized, N: NodeExecutionPort + ?Sized> {
    pub kernel: &'a Kernel<S>,
    pub observer: Option<&'a dyn SubmissionObserver>,
    pub boundary: Option<&'a dyn DispatchBoundary>,
    pub artifacts: &'a A,
    pub nodes: &'a N,
    pub cancellation: crate::Cancellation,
}

impl<S: Store, A: ArtifactPort + ?Sized, N: NodeExecutionPort + ?Sized>
    KernelScheduler<'_, S, A, N>
{
    fn load(&self, id: &str) -> Result<Resource, GraphError> {
        let resource = self
            .kernel
            .load(id)
            .map_err(kernel_error)?
            .ok_or_else(|| GraphError::CorruptRun(format!("missing Kernel Run {id}")))?;
        record_of(&resource)?.validate()?;
        Ok(resource)
    }

    pub async fn run(&self, id: &str) -> Result<Resource, GraphError> {
        let initial = self.load(id)?;
        if initial.is_settled() {
            return Ok(initial);
        }
        admit_capabilities(self.nodes, &record_of(&initial)?.snapshot)?;
        // These sets only own live futures. All progress, limits and lifecycle
        // decisions are recomputed by the Graph validator, never by this loop.
        let mut pending = FuturesUnordered::new();
        let mut running = BTreeSet::new();
        let mut ready = VecDeque::new();
        let mut draining = false;
        let mut failure = None;
        loop {
            if !draining && ready.is_empty() {
                let resource = self.load(id)?;
                if matches!(
                    resource.phase(),
                    Some(Phase::Waiting {
                        waiter: anchor_kernel::Waiter::Child { .. }
                    })
                ) {
                    // A child observation is a Host operation. The validator
                    // only receives the resulting release and terminal fact.
                    if self.observe_child(&resource).await? {
                        continue;
                    }
                    draining = true;
                }
            }
            // Commit completions already observed before dispatching new work.
            // An unfinished sibling never holds up a newly open successor.
            if !draining && ready.is_empty() {
                let dispatches = self.load(id).and_then(|resource| {
                    if resource.phase() != Some(&Phase::Executing) {
                        return Ok((resource, Vec::new()));
                    }
                    let frontier = GraphValidator::frontier(&resource)?;
                    Ok((resource, frontier))
                });
                match dispatches {
                    Ok((resource, frontier)) => {
                        for dispatch in frontier {
                            let slot = dispatch.slot();
                            if running.insert(slot.clone()) {
                                let resource = resource.clone();
                                pending.push(async move {
                                    let result =
                                        AssertUnwindSafe(self.execute_slot(resource, dispatch))
                                            .catch_unwind()
                                            .await
                                            .unwrap_or_else(|_| {
                                                Err(GraphError::Io(io::Error::other(
                                                    "node executor panicked",
                                                )))
                                            });
                                    (slot, result)
                                });
                            }
                        }
                    }
                    Err(error) => {
                        failure.get_or_insert(error);
                        draining = true;
                    }
                }
            }
            if ready.is_empty()
                && let Some(result) = pending.next().await
            {
                ready.push_back(result);
            }
            // Give every admitted executor a chance to start before judging a
            // ready result. Do not wait for slow siblings; refill open slots
            // after each accepted completion.
            while let Some(Some(result)) = pending.next().now_or_never() {
                ready.push_back(result);
            }
            let Some((slot, result)) = ready.pop_front() else {
                if let Some(boundary) = self.boundary {
                    boundary.before_dispatch(id)?;
                }
                return match failure {
                    Some(error) => Err(error),
                    None => self.load(id),
                };
            };
            running.remove(&slot);
            let result = match result {
                Ok(Some((principal, cost, submission))) => {
                    submit_with_retry(self.kernel, &principal, cost, submission, self.observer)
                        .and_then(|verdict| {
                            if verdict.is_accepted()
                                || matches!(
                                    verdict.reason,
                                    Reason::TaskWaiting | Reason::TaskSettled
                                )
                            {
                                Ok(())
                            } else {
                                Err(GraphError::CorruptRun(format!(
                                    "Kernel submission refused: {:?}",
                                    verdict.reason
                                )))
                            }
                        })
                }
                Ok(None) => {
                    // Cancellation/suspension ends this worker; never redispatch
                    // the same open slot in this drive. Durable facts allow resume.
                    draining = true;
                    Ok(())
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                failure.get_or_insert(error);
                draining = true;
            }
            if let Some(boundary) = self.boundary
                && let Err(error) = boundary.before_dispatch(id)
            {
                failure.get_or_insert(error);
                draining = true;
            }
            // Refill after each completion, without waiting for its siblings.
            // Even after waiting/settlement, drain already running executors so
            // their late facts and costs receive a durable Kernel verdict.
        }
    }
}
