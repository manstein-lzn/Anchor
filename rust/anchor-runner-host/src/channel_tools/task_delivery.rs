//! Task result effects keep their identity when a user interrupts processing.
use super::*;
use anchor_platform_session::{
    AssistantInput, ChannelConversationKind, ChannelIdentity, TaskResult,
};
use sha2::Sha256;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ResolutionArguments {
    Delivered,
    Silent { reason: String },
}

pub(super) fn resolution_definition() -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new(TASK_RESULT_RESOLVE_TOOL).expect("static tool name"),
        "Explicitly resolve this task input before final_result. Use action=delivered only after sending with the channel send tools: the Host queries the original Gateway ledger and requires confirmed ACKs, including both file and introduction. Model-written summaries or receipt IDs are not proof. Use action=silent with a nonempty reason only when the authorized request requires no notification; this records deliberate silence, never delivery. Any unknown or partial send blocks resolution. Identity and recipient come from the active task, not arguments. Resolution survives interruption, is idempotent, and closes this result to further sends.",
        json!({"type":"object","properties":{
            "action":{"type":"string","enum":["delivered","silent"]},
            "reason":{"type":"string","minLength":1,"maxLength":8192,"description":"Required for silent; omit for delivered."}
        },"required":["action"],"additionalProperties":false}),
    )
}

pub(super) struct TaskDelivery {
    pub(super) source: crate::run_metadata::AssistantSource,
    pub(super) result: TaskResult,
    pub(super) identity: ChannelIdentity,
}

impl ChannelTools {
    pub(super) fn request_task_delivery(&self, payload: &Value) -> Result<(), ToolError> {
        let Some(task) = self.task_delivery_context()? else {
            return Ok(());
        };
        let id = payload["request_id"]
            .as_str()
            .ok_or_else(|| ToolError::Failed("delivery request identity is missing".into()))?;
        let requests = if payload["operation"] == "send_file" {
            let logical = id
                .strip_suffix("-file")
                .ok_or_else(|| ToolError::Failed("file delivery identity is invalid".into()))?;
            vec![id.to_owned(), format!("{logical}-intro")]
        } else {
            vec![id.to_owned()]
        };
        let binding = crate::assistant::binding(&self.state_root, &self.key)
            .map_err(ToolError::Failed)?
            .ok_or_else(|| ToolError::Failed("task input binding is missing".into()))?;
        crate::assistant::sessions(&self.state_root)
            .map_err(ToolError::Failed)?
            .request_task_result_delivery(
                &task.source.owner,
                &task.source.session,
                &self.key.run_id,
                &binding.wait_key,
                &requests,
            )
            .map_err(|error| ToolError::Failed(error.to_string()))
    }

    pub(super) fn require_open_task_delivery(&self) -> Result<(), ToolError> {
        if let Some(task) = self.task_delivery_context()? {
            let sessions =
                crate::assistant::sessions(&self.state_root).map_err(ToolError::Failed)?;
            if sessions
                .task_result_resolution(&task.source.owner, &task.source.session, &task.result.id)
                .map_err(|error| ToolError::Failed(error.to_string()))?
                .is_some()
            {
                return Err(ToolError::Failed(
                    "this task result is already resolved; no further sends are allowed".into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) async fn resolve_task_delivery(
        &self,
        args: ResolutionArguments,
    ) -> Result<Value, ToolError> {
        use anchor_channel_transport::ledger::DeliveryStatus;
        use anchor_platform_session::TaskResultResolution;
        self.check_active()?;
        // This is a Host API action, not a completion field written by a model.
        self.send_request_id()?;
        let task = self.task_delivery_context()?.ok_or_else(|| {
            ToolError::Failed("resolution requires an active private task result".into())
        })?;
        let sessions = crate::assistant::sessions(&self.state_root).map_err(ToolError::Failed)?;
        let previous = sessions
            .task_result_resolution(&task.source.owner, &task.source.session, &task.result.id)
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        let resolution = if let Some(previous) = previous {
            match (&args, &previous) {
                (ResolutionArguments::Delivered, TaskResultResolution::Delivered { .. }) => {}
                (
                    ResolutionArguments::Silent { reason },
                    TaskResultResolution::Silent { reason: old },
                ) if reason == old => {}
                _ => {
                    return Err(ToolError::Failed(
                        "task result was already resolved differently".into(),
                    ));
                }
            }
            previous
        } else {
            let platform = ChannelPlatform::parse(&task.identity.source)
                .ok_or_else(|| ToolError::Failed("unsupported task delivery platform".into()))?;
            let (socket, token) = self.send_endpoint(
                &SendArguments {
                    bot_graph: None,
                    userid: task.identity.sender_id.clone(),
                    content: String::new(),
                },
                platform,
            )?;
            let text = self.delivery_request_id(&task.identity.sender_id, None)?;
            let mut ids = vec![text];
            for file in &task.result.files {
                let id = self.delivery_request_id(&task.identity.sender_id, Some(file))?;
                ids.extend([format!("{id}-file"), format!("{id}-intro")]);
            }
            ids.sort();
            ids.dedup();
            let mut deliveries = std::collections::BTreeMap::new();
            // Keep each control request/response bounded without truncating a
            // result's immutable file selection.
            for batch in ids.chunks(256) {
                let status = self
                    .control_request(
                        socket.clone(),
                        &json!({"operation":"delivery_status","token":token,"request_ids":batch}),
                    )
                    .await?;
                let batch_facts: std::collections::BTreeMap<String, DeliveryStatus> =
                    serde_json::from_value(status.get("deliveries").cloned().ok_or_else(|| {
                        ToolError::Failed(
                            "Gateway did not return delivery facts; result remains unresolved"
                                .into(),
                        )
                    })?)
                    .map_err(|_| ToolError::Failed("invalid Gateway delivery facts".into()))?;
                if batch_facts.keys().any(|id| !batch.contains(id)) {
                    return Err(ToolError::Failed(
                        "Gateway returned unrelated delivery facts".into(),
                    ));
                }
                deliveries.extend(batch_facts);
            }
            if deliveries
                .values()
                .any(|status| *status != DeliveryStatus::Confirmed)
            {
                return Err(ToolError::Failed("task delivery has an unknown outcome; inspect the original receipt, do not resend with a new identity or resolve as silent".into()));
            }
            if ids.iter().any(|id| {
                id.ends_with("-file")
                    && (deliveries.contains_key(id)
                        != deliveries
                            .contains_key(&format!("{}-intro", id.trim_end_matches("-file"))))
            }) {
                return Err(ToolError::Failed("file and introduction are only partially confirmed; task result remains unresolved".into()));
            }
            let requested = sessions
                .task_result_delivery_requests(
                    &task.source.owner,
                    &task.source.session,
                    &task.result.id,
                )
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            if requested.iter().any(|id| !deliveries.contains_key(id)) {
                return Err(ToolError::Failed("a send API was submitted but its Gateway receipt is still missing; result remains unresolved, not silent or delivered".into()));
            }
            match args {
                ResolutionArguments::Delivered => {
                    if deliveries.is_empty() {
                        return Err(ToolError::Failed("no confirmed channel send exists for this task result; call the channel send API first".into()));
                    }
                    TaskResultResolution::Delivered {
                        requests: deliveries.into_keys().collect(),
                    }
                }
                ResolutionArguments::Silent { reason } => {
                    if !deliveries.is_empty() {
                        return Err(ToolError::Failed(
                            "a task with channel sends cannot be resolved as silent".into(),
                        ));
                    }
                    TaskResultResolution::Silent { reason }
                }
            }
        };
        // Recheck after the Gateway read; a user may have interrupted this Turn.
        self.task_delivery_context()?;
        let binding = crate::assistant::binding(&self.state_root, &self.key)
            .map_err(ToolError::Failed)?
            .ok_or_else(|| ToolError::Failed("task input binding is missing".into()))?;
        sessions
            .resolve_task_result(
                &task.source.owner,
                &task.source.session,
                &self.key.run_id,
                &binding.wait_key,
                &resolution,
            )
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        Ok(json!({"resolved":true,"task_result":task.result.id,"resolution":resolution}))
    }

    pub(super) fn task_delivery_context(&self) -> Result<Option<TaskDelivery>, ToolError> {
        if self.assistant_graph.is_none()
            || !crate::assistant::binding(&self.state_root, &self.key)
                .map_err(ToolError::Failed)?
                .is_some_and(|binding| binding.input_kind == crate::assistant::InputKind::Task)
        {
            return Ok(None);
        }
        let (source, input, identity) = self.private_delivery_context()?;
        let AssistantInput::Task(input) = input else {
            return Err(ToolError::Failed(
                "a trusted task result is required".into(),
            ));
        };
        self.application
            .authorize_subscription_result(&source.owner, &source.session, &input.result)
            .map_err(|error| {
                ToolError::Failed(format!(
                    "subscription delivery is no longer authorized: {error:?}"
                ))
            })?;
        Ok(Some(TaskDelivery {
            source,
            result: input.result,
            identity,
        }))
    }

    /// Both task results and ordinary file sends stay inside the original
    /// private Turn; the model never chooses a conversation or participant.
    pub(super) fn private_delivery_context(
        &self,
    ) -> Result<
        (
            crate::run_metadata::AssistantSource,
            AssistantInput,
            ChannelIdentity,
        ),
        ToolError,
    > {
        let invalid = || {
            ToolError::Failed(
                "this input no longer belongs to an active private assistant Turn".into(),
            )
        };
        if self.assistant_graph.is_none() {
            return Err(invalid());
        }
        let context = self
            .application
            .active_channel_turn(&self.key)
            .map_err(|_| invalid())?;
        let source = context.source;
        let input = context.input;
        let identity = context.identity;
        if crate::credential_store::binding_owner(&source.owner) != self.owner
            || identity.conversation_kind != ChannelConversationKind::Single
            || identity.sender_id.is_empty()
        {
            return Err(invalid());
        }
        Ok((source, input, identity))
    }

    pub(super) fn bind_task_recipient(&self, userid: &mut String) -> Result<(), ToolError> {
        if let Some(task) = self.task_delivery_context()? {
            if userid.is_empty() {
                *userid = task.identity.sender_id;
            } else if *userid != task.identity.sender_id {
                return Err(ToolError::Failed(
                    "task results can only be delivered to their original private conversation"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn delivery_request_id(
        &self,
        recipient: &str,
        file: Option<&anchor_platform_session::ChannelNotificationFile>,
    ) -> Result<String, ToolError> {
        // Native identity remains mandatory; it does not identify the logical
        // delivery because a resumed result gets a new Turn and native call.
        let native = self.send_request_id()?;
        let Some(task) = self.task_delivery_context()? else {
            return Ok(native);
        };
        let binding = serde_json::to_vec(&(
            &task.source.owner,
            &task.source.session,
            &task.result.id,
            &self.assistant_graph,
            recipient,
            file.map(|file| (&file.artifact_id, &file.path)),
        ))
        .map_err(|error| ToolError::Failed(error.to_string()))?;
        let mut digest = Sha256::new();
        digest.update(b"anchor-channel-task-delivery-v1\0");
        digest.update(binding);
        Ok(format!("channel-task-{:x}", digest.finalize()))
    }
}
