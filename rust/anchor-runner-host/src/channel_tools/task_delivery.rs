//! Task result effects keep their identity when a user interrupts processing.
use super::*;
use anchor_platform_session::{
    AssistantInput, ChannelConversationKind, ChannelIdentity, TaskResult, TurnStatus,
};
use sha2::Sha256;

pub(super) struct TaskDelivery {
    pub(super) source: crate::run_metadata::AssistantSource,
    pub(super) result: TaskResult,
    pub(super) identity: ChannelIdentity,
}

impl ChannelTools {
    pub(super) fn task_delivery_context(&self) -> Result<Option<TaskDelivery>, ToolError> {
        if self.assistant_graph.is_none()
            || !crate::assistant::binding(&self.state_root, &self.key)
                .map_err(ToolError::Failed)?
                .is_some_and(|binding| binding.input_kind == crate::assistant::InputKind::Task)
        {
            return Ok(None);
        }
        let invalid = || {
            ToolError::Failed(
                "this task result no longer belongs to an active private assistant Turn".into(),
            )
        };
        let source = crate::assistant::source(&self.state_root, &self.key)
            .map_err(ToolError::Failed)?
            .ok_or_else(invalid)?;
        if source.work_node != self.key.node_id
            || crate::credential_store::binding_owner(&source.owner) != self.owner
        {
            return Err(invalid());
        }
        let Some(AssistantInput::Task(input)) =
            crate::assistant::input(&self.state_root, &self.key).map_err(ToolError::Failed)?
        else {
            return Err(invalid());
        };
        if input.run_id != self.key.run_id || input.turn.status != TurnStatus::Running {
            return Err(invalid());
        }
        let sessions = crate::assistant::sessions(&self.state_root).map_err(ToolError::Failed)?;
        if sessions
            .get_channel_assistant(&source.owner, &source.session)
            .map_err(|error| ToolError::Failed(error.to_string()))?
            .is_none_or(|assistant| assistant.run_id != self.key.run_id)
            || !sessions
                .channel_session_is_selected(&source.owner, &source.session)
                .map_err(|error| ToolError::Failed(error.to_string()))?
        {
            return Err(invalid());
        }
        let identity = sessions
            .channel_identity(&source.owner, &source.session)
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        if identity.source != "wecom"
            || identity.conversation_kind != ChannelConversationKind::Single
            || identity.sender_id.is_empty()
            || identity.conversation_id.is_empty()
        {
            return Err(invalid());
        }
        Ok(Some(TaskDelivery {
            source,
            result: input.result,
            identity,
        }))
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
