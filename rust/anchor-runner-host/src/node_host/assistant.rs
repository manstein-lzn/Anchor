use super::*;
use crate::assistant::{self, graph_error};
use anchor_platform_session::TurnStatus;
use anchor_runtime::graph::ArtifactPort;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

fn stopped_input(input: &anchor_platform_session::ChannelInboundAdmission) -> bool {
    input.relation.superseded_by_turn_id.is_some()
        || matches!(
            input.turn.status,
            TurnStatus::Interrupted | TurnStatus::Stopped | TurnStatus::Failed
        )
}

const SUPERSEDED: &str = "input superseded by a newer Turn";
const PROVIDER_REJECTED: &str = "model provider rejected the request";

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct YieldFact {
    key: InvocationKey,
    reason: String,
    route: Option<String>,
    /// Set when the provider rejected this Turn's request; the reply node then
    /// answers for the Host instead of suppressing the Turn. Older facts lack
    /// it and are superseded-input yields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_failure: Option<String>,
}

/// The Host's reply when the provider rejected a Turn. Only the classification
/// is shown: raw provider text may quote the user's content or internal detail.
fn provider_reply(kind: &str) -> String {
    match kind {
        "authentication" => "模型服务认证失败，本条消息未处理。请联系管理员检查模型凭据，之后可直接再发消息。".into(),
        "credits_exhausted" => "模型服务额度已用完，本条消息未处理。请联系管理员处理额度，之后可直接再发消息。".into(),
        // `provider_error` covers both transient faults (rate limits, 5xx)
        // and content rejections; the Host cannot tell them apart.
        _ => "模型服务拒绝了本次请求，本条消息未处理。可能是服务临时故障，也可能是内容被拦截：可以稍后重发或调整内容；如果连续出现，请联系管理员。".into(),
    }
}

impl HostNodes {
    fn yield_assistant(
        &self,
        key: &InvocationKey,
        route: String,
        provider_failure: Option<String>,
    ) -> Result<CompletionFact, GraphError> {
        let fact = YieldFact {
            key: key.clone(),
            reason: if provider_failure.is_some() {
                PROVIDER_REJECTED
            } else {
                SUPERSEDED
            }
            .into(),
            route: Some(route),
            provider_failure,
        };
        assistant::save_immutable(
            &self
                .facts_root
                .join(format!("{}.yielded.json", fact_stem(key))),
            &fact,
        )
        .map_err(graph_error)?;
        Ok(CompletionFact::Yielded {
            reason: fact.reason,
            route: fact.route,
        })
    }

    pub(super) fn assistant_completion_fact(
        &self,
        key: &InvocationKey,
    ) -> Result<Option<CompletionFact>, GraphError> {
        let root = self.io_resolver.local_inputs.state_root();
        let Some(source) = assistant::source(root, key).map_err(graph_error)? else {
            return Ok(None);
        };
        if let Some(fact) = assistant::read_json::<YieldFact>(
            &self
                .facts_root
                .join(format!("{}.yielded.json", fact_stem(key))),
        )
        .map_err(graph_error)?
        {
            if fact.key != *key || fact.route.as_deref() != Some(source.reply_node.as_str()) {
                return Err(GraphError::CorruptRun(
                    "assistant interruption identity changed".into(),
                ));
            }
            return Ok(Some(CompletionFact::Yielded {
                reason: fact.reason,
                route: fact.route,
            }));
        }
        if key.node_id == source.work_node
            && let Some(input) = assistant::input(root, key).map_err(graph_error)?
            && stopped_input(&input)
        {
            let inputs = self
                .io_resolver
                .run_application()
                .dispatch_inputs(key)
                .map_err(|error| {
                    graph_error(format!("assistant interrupted dispatch: {error:?}"))
                })?;
            self.io_resolver
                .prepare_node_workspace(key, &inputs)
                .map_err(graph_error)?;
            return self.yield_assistant(key, source.reply_node, None).map(Some);
        }
        Ok(None)
    }

    /// The provider classification of a work invocation's yield, if the Host
    /// yielded it because the provider rejected the request.
    fn provider_yield(&self, key: &InvocationKey) -> Result<Option<String>, GraphError> {
        Ok(assistant::read_json::<YieldFact>(
            &self
                .facts_root
                .join(format!("{}.yielded.json", fact_stem(key))),
        )
        .map_err(graph_error)?
        .map(|fact| {
            if fact.key == *key {
                Ok(fact.provider_failure)
            } else {
                Err(GraphError::CorruptRun(
                    "assistant interruption identity changed".into(),
                ))
            }
        })
        .transpose()?
        .flatten())
    }

    pub(super) fn settle_assistant_reply(
        &self,
        key: &InvocationKey,
        completion: &NodeCompletion,
    ) -> Result<(), GraphError> {
        let root = self.io_resolver.local_inputs.state_root();
        let Some(source) = assistant::source(root, key).map_err(graph_error)? else {
            return Ok(());
        };
        if key.node_id != source.reply_node
            || completion.output.get("suppressed") == Some(&json!(true))
        {
            return Ok(());
        }
        let input = assistant::input(root, key)
            .map_err(graph_error)?
            .ok_or_else(|| graph_error("assistant reply has no input binding"))?;
        if completion.output.get("reply_for").and_then(Value::as_str)
            != Some(input.relation.turn_id.as_str())
        {
            return Err(GraphError::CorruptRun(
                "assistant reply belongs to another Turn".into(),
            ));
        }
        if input.relation.superseded_by_turn_id.is_none()
            && input.turn.status == TurnStatus::Running
        {
            assistant::sessions(root)
                .map_err(graph_error)?
                .finish_turn(
                    &source.owner,
                    &source.session,
                    &input.relation.turn_id,
                    TurnStatus::Completed,
                    None,
                )
                .map_err(graph_error)?;
        }
        Ok(())
    }

    pub(super) async fn execute_host_operation(
        &self,
        request: NodeExecutionRequest,
    ) -> Result<NodeExecutionOutcome, GraphError> {
        let root = self.io_resolver.local_inputs.state_root();
        let source = assistant::source(root, &request.key)
            .map_err(graph_error)?
            .ok_or_else(|| {
                graph_error("host Session operations require an authorized assistant binding")
            })?;
        let operation = request
            .operation
            .as_ref()
            .and_then(|operation| operation.get("operation"))
            .and_then(Value::as_str)
            .ok_or_else(|| graph_error("host operation is missing"))?;
        match operation {
            assistant::WAIT_INPUT if request.key.node_id == source.wait_node => {
                let signal = assistant::signal(&source.session);
                let kernel_pause = self
                    .io_resolver
                    .run_application
                    .get()
                    .map(|app| app.kernel_wait_pause(&request.key.run_id))
                    .transpose()
                    .map_err(|error| graph_error(format!("pause event unavailable: {error:?}")))?
                    .flatten();
                loop {
                    let notified = signal.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if request.cancellation.load(Ordering::Acquire) {
                        return Ok(NodeExecutionOutcome::Cancelled);
                    }
                    let input = assistant::sessions(root)
                        .map_err(graph_error)?
                        .claim_channel_assistant_input(
                            &source.owner,
                            &source.session,
                            &request.key.run_id,
                            &request.key.durable_key(),
                        )
                        .map_err(graph_error)?;
                    if let Some(input) = input {
                        if assistant::binding(root, &request.key)
                            .map_err(graph_error)?
                            .is_none()
                        {
                            let mut pending = assistant::sessions(root)
                                .map_err(graph_error)?
                                .pending_channel_assistant_inputs(
                                    &source.owner,
                                    &source.session,
                                    &request.key.run_id,
                                    &input.relation.inbound_id,
                                    9,
                                )
                                .map_err(graph_error)?;
                            let truncated = pending.len() > 8;
                            if truncated {
                                pending.remove(0);
                            }
                            assistant::bind(
                                root,
                                &request.key,
                                request.key.durable_key(),
                                &input,
                                pending
                                    .into_iter()
                                    .map(|input| input.relation.inbound_id)
                                    .collect(),
                                truncated,
                                assistant::sessions(root)
                                    .map_err(graph_error)?
                                    .latest_channel_notification(&source.owner, &source.session)
                                    .map_err(graph_error)?,
                            )
                            .map_err(graph_error)?;
                        }
                        self.io_resolver
                            .freeze_assistant_plugins(
                                &request.key,
                                &input.relation.turn_id,
                                true,
                                request.cancellation.clone(),
                            )
                            .await
                            .map_err(graph_error)?;
                        let selected = assistant::attachment_metadata(
                            root,
                            &crate::run_metadata::load(root, &request.key.run_id)
                                .map_err(|error| graph_error(format!("{error:?}")))?
                                .ok_or_else(|| graph_error("assistant metadata is missing"))?,
                            &request.key,
                        )
                        .map_err(graph_error)?;
                        if !selected.attachments.is_empty()
                            && input.relation.superseded_by_turn_id.is_none()
                            && !crate::channel_inputs::input_directory(root, &selected.run_id)
                                .map_err(graph_error)?
                                .join("input.json")
                                .exists()
                        {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                        let pending =
                            assistant::pending_inputs(root, &request.key).map_err(graph_error)?;
                        let interrupted = pending.into_iter().map(|message| {
                            let mut attachments = message.request.attachments;
                            for file in &mut attachments.files {
                                file.path = format!("/in/channel-pending/{}/{}", message.relation.turn_id, file.name);
                            }
                            json!({"message":message.request.text,"channel":message.request.identity,"turn":message.relation.turn_id,"attachments":attachments})
                        }).collect::<Vec<_>>();
                        let output = json!({
                            "files":["input.json"],
                            "message":input.request.text,
                            "channel":input.request.identity,
                            "attachments":input.request.attachments,
                            "session":source.session,
                            "turn":input.relation.turn_id,
                            "latest_notification": assistant::binding(root, &request.key)
                                .map_err(graph_error)?.and_then(|binding| binding.notification)
                                .map(crate::channel_notifications::projection),
                            "interrupted_messages":interrupted,
                            "interrupted_messages_truncated":assistant::binding(root, &request.key).map_err(graph_error)?.is_some_and(|binding| binding.pending_truncated),
                        });
                        let workspace = self
                            .io_resolver
                            .prepare_node_workspace(&request.key, &request.input_commits)
                            .map_err(graph_error)?;
                        let input_path = workspace.join("input.json");
                        let bytes = serde_json::to_vec(&output).map_err(graph_error)?;
                        crate::write_durable(&input_path, &bytes)?;
                        return self.complete(
                            &request.key,
                            NodeCompletion {
                                submission: "received one Session input".into(),
                                route: Some(source.work_node),
                                model_requests: 0,
                                output,
                            },
                        );
                    }
                    tokio::select! {
                        _ = async {
                            match &kernel_pause {
                                Some(signal) => signal.notified().await,
                                None => std::future::pending().await,
                            }
                        } => return Ok(NodeExecutionOutcome::Suspended),
                        _ = notified => {},
                        _ = tokio::time::sleep(Duration::from_millis(250)) => {},
                    }
                }
            }
            assistant::REPLY if request.key.node_id == source.reply_node => {
                let input = assistant::bind_request(root, &request)
                    .map_err(graph_error)?
                    .ok_or_else(|| graph_error("reply requires a Turn"))?;
                self.io_resolver
                    .prepare_node_workspace(&request.key, &request.input_commits)
                    .map_err(graph_error)?;
                if stopped_input(&input) {
                    return self.complete(
                        &request.key,
                        NodeCompletion {
                            submission: "superseded reply suppressed".into(),
                            route: Some(source.wait_node),
                            model_requests: 0,
                            output: json!({"files":[],"suppressed":true,"reply_for":input.relation.turn_id}),
                        },
                    );
                }
                let commit = request
                    .input_commits
                    .iter()
                    .find(|commit| commit.node_id == source.work_node)
                    .ok_or_else(|| graph_error("reply requires the selected Agent commit"))?;
                let completion: NodeCompletion =
                    serde_json::from_value(self.artifacts.resolve(commit).await?)
                        .map_err(graph_error)?;
                let work_key = InvocationKey {
                    node_id: commit.node_id.clone(),
                    invocation: commit.invocation,
                    ..request.key.clone()
                };
                let reply = if completion.output.get("interrupted") == Some(&json!(true)) {
                    // Only a provider rejection recorded by the Host may answer
                    // for an interrupted Agent; it never carries Agent images.
                    let kind = self.provider_yield(&work_key)?.ok_or_else(|| {
                        graph_error("an interrupted Agent cannot produce a reply")
                    })?;
                    NodeCompletion {
                        submission: provider_reply(&kind),
                        route: Some(source.wait_node),
                        model_requests: 0,
                        output: json!({"files":[],"reply_for":input.relation.turn_id,"source_commit":commit,
                            "items":[],"host_reply":"provider_rejected","provider_failure":kind}),
                    }
                } else {
                    if completion.submission.trim().is_empty() {
                        return Err(graph_error("assistant reply is empty"));
                    }
                    let items =
                        crate::channel_replies::read_reply_images_for_invocation(root, &work_key)
                            .map_err(graph_error)?;
                    NodeCompletion {
                        submission: completion.submission,
                        route: Some(source.wait_node),
                        model_requests: 0,
                        output: json!({"files":[],"reply_for":input.relation.turn_id,"source_commit":commit,"items":items}),
                    }
                };
                let outcome = self.complete(&request.key, reply.clone())?;
                self.settle_assistant_reply(&request.key, &reply)?;
                Ok(outcome)
            }
            _ => Err(graph_error(
                "host operation is not authorized for this node",
            )),
        }
    }

    pub(super) async fn execute_assistant_request(
        &self,
        mut request: NodeExecutionRequest,
    ) -> Result<NodeExecutionOutcome, GraphError> {
        let root = self.io_resolver.local_inputs.state_root();
        let source = assistant::source(root, &request.key)
            .map_err(graph_error)?
            .ok_or_else(|| graph_error("assistant binding is missing"))?;
        if request.key.node_id != source.work_node {
            return Err(graph_error("assistant processing node is not authorized"));
        }
        let input = assistant::bind_request(root, &request)
            .map_err(graph_error)?
            .ok_or_else(|| graph_error("assistant input is missing"))?;
        self.io_resolver
            .prepare_node_workspace(&request.key, &request.input_commits)
            .map_err(graph_error)?;
        if stopped_input(&input)
            && let CompletionFact::Yielded { reason, route } =
                self.yield_assistant(&request.key, source.reply_node.clone(), None)?
        {
            return Ok(NodeExecutionOutcome::Yielded { reason, route });
        }
        let key = request.key.clone();
        let run_cancel = request.cancellation.clone();
        let node_cancel = Arc::new(AtomicBool::new(false));
        request.cancellation = node_cancel.clone();
        let signal = assistant::signal(&source.session);
        let monitor = async {
            loop {
                if run_cancel.load(Ordering::Acquire) {
                    return Ok::<bool, GraphError>(false);
                }
                let current = assistant::input(root, &key)
                    .map_err(graph_error)?
                    .ok_or_else(|| graph_error("assistant input disappeared"))?;
                if stopped_input(&current) {
                    return Ok(true);
                }
                tokio::select! {
                    _ = signal.notified() => {},
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
            }
        };
        let execution = self.execute_regular(request);
        tokio::pin!(execution);
        let outcome = tokio::select! {
            outcome = &mut execution => outcome?,
            interruption = monitor => {
                node_cancel.store(true, Ordering::Release);
                let _ = execution.await;
                if !interruption? { return Ok(NodeExecutionOutcome::Cancelled); }
                if let CompletionFact::Yielded { reason, route } = self.yield_assistant(&key, source.reply_node.clone(), None)? { return Ok(NodeExecutionOutcome::Yielded { reason, route }); }
                return Err(graph_error("assistant interruption was not recorded"));
            }
        };
        if run_cancel.load(Ordering::Acquire) {
            return Ok(NodeExecutionOutcome::Cancelled);
        }
        let current = assistant::input(root, &key)
            .map_err(graph_error)?
            .ok_or_else(|| graph_error("assistant input disappeared"))?;
        if stopped_input(&current)
            && let CompletionFact::Yielded { reason, route } =
                self.yield_assistant(&key, source.reply_node.clone(), None)?
        {
            return Ok(NodeExecutionOutcome::Yielded { reason, route });
        }
        // A provider rejection ends this Turn with a Host reply; the assistant
        // keeps waiting. Unclassified failures still stop it for inspection.
        if matches!(outcome, NodeExecutionOutcome::Interrupted { .. })
            && let Some(kind) = self
                .goose_nodes
                .as_ref()
                .map(|nodes| nodes.provider_failure(&key))
                .transpose()?
                .flatten()
            && let CompletionFact::Yielded { reason, route } =
                self.yield_assistant(&key, source.reply_node, Some(kind))?
        {
            return Ok(NodeExecutionOutcome::Yielded { reason, route });
        }
        Ok(outcome)
    }
}
