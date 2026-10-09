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

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct YieldFact {
    key: InvocationKey,
    reason: String,
    route: Option<String>,
}

impl HostIoResolver {
    pub(super) fn seed_assistant_workspace(
        &self,
        key: &InvocationKey,
        workspace: &std::path::Path,
    ) -> Result<(), String> {
        let root = self.local_inputs.state_root();
        let Some(source) = assistant::source(root, key)? else {
            return Ok(());
        };
        if key.invocation != 1 || key.node_id != source.work_node {
            return Ok(());
        }
        // A handed-over instance already received its predecessor's live scene
        // inside the preparation lock. Copying the committed mount over it here
        // would replace newer uncommitted files with older committed bytes.
        if assistant::handover_workspace_source(root, key)?.is_some() {
            return Ok(());
        }
        let fact_path = root
            .join("assistant-seeds")
            .join(format!("{}.json", fact_stem(key)));
        if let Some(saved) = assistant::read_json::<Value>(&fact_path)? {
            if saved.get("key")
                != Some(&serde_json::to_value(key).map_err(|error| error.to_string())?)
            {
                return Err("assistant seed identity changed".into());
            }
            return Ok(());
        }
        let previous = self
            .conversation(key)?
            .map(|conversation| self.previous_mount(key, &conversation))
            .transpose()?
            .flatten();
        if let Some(previous) = previous.as_ref() {
            copy_seed(&previous.source, workspace)?;
        }
        assistant::save_immutable(
            &fact_path,
            &json!({"key":key,"previous":previous.map(|input| input.source)}),
        )
    }
}

fn copy_seed(source: &std::path::Path, destination: &std::path::Path) -> Result<(), String> {
    for entry in std::fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            crate::create_durable_directory(&target).map_err(|error| error.to_string())?;
            copy_seed(&entry.path(), &target)?;
        } else if kind.is_file() {
            if target.exists()
                && !std::fs::symlink_metadata(&target)
                    .map_err(|error| error.to_string())?
                    .is_file()
            {
                return Err("assistant seed target is not a file".into());
            }
            std::fs::copy(entry.path(), &target).map_err(|error| error.to_string())?;
            std::fs::File::open(target)
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
        } else {
            return Err("assistant seed contains an unsupported file".into());
        }
    }
    std::fs::File::open(destination)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

impl HostNodes {
    fn yield_assistant(
        &self,
        key: &InvocationKey,
        route: String,
    ) -> Result<CompletionFact, GraphError> {
        if self.artifacts.workspace_path(key)?.exists() {
            self.artifacts.retain_interrupted_workspace(key)?;
        }
        let fact = YieldFact {
            key: key.clone(),
            reason: "input superseded by a newer Turn".into(),
            route: Some(route),
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
            let record = self
                .io_resolver
                .run_store
                .load(&key.run_id)?
                .ok_or_else(|| graph_error("assistant Run is missing"))?;
            let cursor = record
                .cursor
                .as_ref()
                .filter(|cursor| cursor.key == *key)
                .ok_or_else(|| graph_error("assistant interrupted cursor is missing"))?;
            self.io_resolver
                .prepare_node_workspace(key, &cursor.input_commits)
                .map_err(graph_error)?;
            return self.yield_assistant(key, source.reply_node).map(Some);
        }
        Ok(None)
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
                loop {
                    let notified = signal.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if request.cancellation.load(Ordering::Acquire) {
                        return Ok(NodeExecutionOutcome::Cancelled);
                    }
                    if self.control.pause.load(Ordering::Acquire) {
                        return Ok(NodeExecutionOutcome::Suspended);
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
                            )
                            .map_err(graph_error)?;
                        }
                        let selected = assistant::attachment_metadata(
                            root,
                            &crate::application::metadata::load(root, &request.key.run_id)
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
                            json!({"message":message.request.text,"turn":message.relation.turn_id,"attachments":attachments})
                        }).collect::<Vec<_>>();
                        let output = json!({
                            "message":input.request.text,
                            "channel":input.request.identity,
                            "attachments":input.request.attachments,
                            "session":source.session,
                            "turn":input.relation.turn_id,
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
                            output: json!({"suppressed":true,"reply_for":input.relation.turn_id}),
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
                if completion.output.get("interrupted") == Some(&json!(true)) {
                    return Err(graph_error("an interrupted Agent cannot produce a reply"));
                }
                if completion.submission.trim().is_empty() {
                    return Err(graph_error("assistant reply is empty"));
                }
                let work_key = InvocationKey {
                    node_id: commit.node_id.clone(),
                    invocation: commit.invocation,
                    ..request.key.clone()
                };
                let items = crate::channel_tools::read_reply_images_for_invocation(root, &work_key)
                    .map_err(graph_error)?;
                let reply = NodeCompletion {
                    submission: completion.submission,
                    route: Some(source.wait_node),
                    model_requests: 0,
                    output: json!({"reply_for":input.relation.turn_id,"source_commit":commit,"items":items}),
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
                self.yield_assistant(&request.key, source.reply_node.clone())?
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
                if let CompletionFact::Yielded { reason, route } = self.yield_assistant(&key, source.reply_node.clone())? { return Ok(NodeExecutionOutcome::Yielded { reason, route }); }
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
                self.yield_assistant(&key, source.reply_node)?
        {
            return Ok(NodeExecutionOutcome::Yielded { reason, route });
        }
        Ok(outcome)
    }
}
