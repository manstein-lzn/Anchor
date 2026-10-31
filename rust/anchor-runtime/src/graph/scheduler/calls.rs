//! Host observations of durable child admission and settlement. No call
//! lifecycle or child execution is performed by the Graph validator.
use super::{execution::StampedSubmission, *};
use crate::graph::validator::{CallSubmission, DispatchKind, call_identity};
use anchor_kernel::{Evidence, Waiter};

/// The Runtime and Host publication boundary use one Call delivery contract.
pub fn graph_call_completion(
    spec: &Value,
    child_run_id: &str,
    output: Value,
) -> (Value, NodeCompletion) {
    let output = if spec["mode"] == "detach" {
        serde_json::json!({"graph":spec["graph"],"run_id":child_run_id,
            "mode":"detach","status":"accepted"})
    } else {
        output
    };
    let files = output
        .pointer("/result/files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(Value::as_str)
                .map(|file| Value::String(format!("result/{file}")))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut delivery = output.clone();
    if let Some(fields) = delivery.as_object_mut() {
        fields.insert("files".into(), Value::Array(files));
    } else {
        delivery = serde_json::json!({"value":output,"files":files});
    }
    let completion = NodeCompletion {
        submission: output.to_string(),
        route: None,
        output: delivery,
        model_requests: 0,
    };
    (output, completion)
}

impl<S: Store, A: ArtifactPort + ?Sized, N: NodeExecutionPort + ?Sized>
    KernelScheduler<'_, S, A, N>
{
    pub(super) async fn execute_call(
        &self,
        resource: &Resource,
        dispatch: &Dispatch,
    ) -> Result<StampedSubmission, GraphError> {
        let record = record_of(resource)?;
        let identity = call_identity(&record, dispatch)?;
        let spec = &record.snapshot.ops[dispatch.node.op.as_ref().expect("call op")]["call"];
        let key = InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: dispatch.node.id.clone(),
            invocation: dispatch.invocation,
        };
        let input = self.input(&record, dispatch, key.clone()).await?;
        let port = self
            .nodes
            .graph_call_port()
            .ok_or_else(|| GraphError::Unsupported("op.call requires a GraphCallPort".into()))?;
        let outcome = port
            .call(
                &identity,
                spec,
                &input.prepared_input,
                &dispatch.inputs,
                self.cancellation.clone(),
            )
            .await;
        let mode = spec["mode"].as_str().unwrap_or_default();
        let (content, evidence) = match outcome {
            Err(GraphError::CallAdmissionFailed(reason)) => (
                CallSubmission::AdmissionFailed { reason },
                Some(Evidence::Observation {
                    id: format!("call-not-admitted/{}", identity.durable_key()),
                }),
            ),
            Err(error) => return Err(error),
            Ok(outcome) => match outcome {
                GraphCallOutcome::Waiting { child_run_id } if mode == "wait" => {
                    (CallSubmission::Bound { child_run_id }, None)
                }
                GraphCallOutcome::Detached { child_run_id } if mode == "detach" => {
                    self.completed_call(&key, dispatch, spec, child_run_id, Value::Null)
                        .await?
                }
                GraphCallOutcome::Completed {
                    child_run_id,
                    output,
                } => {
                    self.completed_call(&key, dispatch, spec, child_run_id, output)
                        .await?
                }
                GraphCallOutcome::Failed {
                    child_run_id,
                    reason,
                } => (
                    CallSubmission::Failed {
                        child_run_id,
                        reason,
                    },
                    None,
                ),
                GraphCallOutcome::Uncertain {
                    child_run_id,
                    reason,
                } => (
                    CallSubmission::Uncertain {
                        child_run_id,
                        reason,
                    },
                    None,
                ),
                incompatible => {
                    return Err(GraphError::CorruptRun(format!(
                        "GraphCallPort returned incompatible outcome: {incompatible:?}"
                    )));
                }
            },
        };
        let mut submission = Submission::new(&resource.id, resource.version, Action::Write)
            .slot(dispatch.slot())
            .content(serde_json::to_value(content).map_err(GraphError::RunDecode)?);
        if let Some(evidence) = evidence {
            submission = submission.evidence(evidence);
        }
        Ok((Principal::new(HOST_ID, PrincipalKind::Code), 0, submission))
    }

    async fn completed_call(
        &self,
        key: &InvocationKey,
        dispatch: &Dispatch,
        spec: &Value,
        child_run_id: String,
        output: Value,
    ) -> Result<(CallSubmission, Option<Evidence>), GraphError> {
        let (output, completion) = graph_call_completion(spec, &child_run_id, output);
        let commit = self
            .artifacts
            .freeze_with_context(
                key,
                &completion,
                &ArtifactFreezeContext {
                    kind: ArtifactKind::GraphCall,
                    input_commits: dispatch.inputs.clone(),
                },
            )
            .await?;
        let evidence = Evidence::Resource {
            id: commit.id.clone(),
            version: 0,
        };
        Ok((
            CallSubmission::Completed {
                child_run_id,
                output,
                commit,
            },
            Some(evidence),
        ))
    }

    /// Observe a waiting child once. A still-running child leaves the parent
    /// waiting; its owning Host arranges the next drive when new facts arrive.
    pub(super) async fn observe_child(&self, resource: &Resource) -> Result<bool, GraphError> {
        let dispatch = GraphValidator::frontier(resource)?
            .into_iter()
            .find(|d| d.kind == DispatchKind::Call)
            .ok_or_else(|| GraphError::CorruptRun("child wait has no call slot".into()))?;
        let (host, cost, submission) = self.execute_call(resource, &dispatch).await?;
        let content = serde_json::from_value::<CallSubmission>(submission.content.clone())
            .map_err(GraphError::RunDecode)?;
        let child = match &content {
            CallSubmission::Bound { .. } => return Ok(false),
            CallSubmission::Completed { child_run_id, .. } => Some(child_run_id.as_str()),
            CallSubmission::Failed { child_run_id, .. }
            | CallSubmission::Uncertain { child_run_id, .. } => child_run_id.as_deref(),
            CallSubmission::AdmissionFailed { .. } => None,
        };
        // A release is an observation of this particular wait target. A port
        // returning another identity must not release the original child wait.
        if let Some(Phase::Waiting {
            waiter: Waiter::Child { task },
        }) = resource.phase()
            && child.map(|id| format!("run/{id}")).as_ref() != Some(task)
        {
            return Err(GraphError::CorruptRun(format!(
                "Graph call changed child Run while waiting for `{task}`"
            )));
        }
        let fresh = self.load(&resource.id)?;
        if matches!(
            fresh.phase(),
            Some(Phase::Waiting {
                waiter: Waiter::Child { .. }
            })
        ) {
            let resume = Submission::new(&fresh.id, fresh.version, Action::Request)
                .content(serde_json::json!({"control":"resume"}));
            let verdict = submit_with_retry(self.kernel, &host, 0, resume, self.observer)?;
            if !verdict.is_accepted()
                && !matches!(verdict.reason, Reason::TaskWaiting | Reason::TaskSettled)
            {
                return Err(GraphError::CorruptRun(format!(
                    "child release refused: {:?}",
                    verdict.reason
                )));
            }
        }
        let verdict = submit_with_retry(self.kernel, &host, cost, submission, self.observer)?;
        if !verdict.is_accepted()
            && !matches!(verdict.reason, Reason::TaskWaiting | Reason::TaskSettled)
        {
            return Err(GraphError::CorruptRun(format!(
                "child result refused: {:?}",
                verdict.reason
            )));
        }
        Ok(true)
    }
}
