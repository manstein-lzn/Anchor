//! Feishu sends have no model-selected bot or recipient.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Arguments {
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileArguments {
    path: String,
    content: String,
    #[serde(default)]
    context_files: Vec<String>,
}

pub(super) fn definition() -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new(FEISHU_SEND_TOOL).expect("static tool name"),
        "Notify this task result's original Feishu private conversation with Markdown. Only available to this bot's assistant; callable only while handling a trusted task.result input, not a user message. The Host selects the original bot and recipient. Normal user replies use final_result and session.reply instead. A confirmed receipt means the platform accepted the message. Repeating identical content reads the original confirmed receipt even after interruption; unknown outcomes must not be retried with new content or identity. Scheduling and network access require separate Graph Tasks grants.",
        json!({"type":"object","properties":{"content":{"type":"string","minLength":1,"description":"The complete Markdown notification; at most 20480 UTF-8 bytes."}},"required":["content"],"additionalProperties":false}),
    )
}

pub(super) fn file_definition() -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new(FEISHU_SEND_FILE_TOOL).expect("static tool name"),
        "Send one file (nonempty, up to 20 MiB), then a Markdown introduction to this Turn's original Feishu private conversation. The Host selects the bot and recipient. A user Turn may send /workspace files or authorized read-only /in inputs; this send does not publish an Artifact. A task result may only send an exact selected immutable /in/tasks or /in/notifications file, preserving the producer's filename; the Host retains it and selected companions for later discussion. File and introduction have separate ACKs; only both confirmed means success. Task delivery identity survives interrupted Turns. Do not resend an unknown outcome or rewrite arguments to bypass its identity. Complete normal user replies through final_result and session.reply without duplicating the introduction.",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"Visible /workspace or authorized /in file for user input; exact selected immutable task file for task results."},
            "content":{"type":"string","minLength":1,"description":"Concise Markdown introduction sent after the file; at most 20480 UTF-8 bytes."},
            "context_files":{"type":"array","items":{"type":"string"},"maxItems":3,"description":"Task results only: exact selected immutable companion files retained for later discussion, not sent to Feishu."}
        },"required":["path","content"],"additionalProperties":false}),
    )
}

impl ChannelTools {
    pub(super) fn feishu_private_context(
        &self,
    ) -> Result<
        (
            crate::run_metadata::AssistantSource,
            anchor_platform_session::AssistantInput,
            anchor_platform_session::ChannelIdentity,
        ),
        ToolError,
    > {
        let context = self.private_delivery_context()?;
        if context.2.source != "feishu" {
            return Err(ToolError::Failed(
                "Feishu sends require the original private Feishu conversation".into(),
            ));
        }
        Ok(context)
    }

    pub(super) async fn send_feishu_file(&self, args: FileArguments) -> Result<Value, ToolError> {
        self.send_file(
            super::FileArguments {
                bot_graph: None,
                userid: String::new(),
                path: args.path,
                content: args.content,
                context_files: args.context_files,
            },
            ChannelPlatform::Feishu,
        )
        .await
    }

    pub(super) async fn send_feishu(&self, args: Arguments) -> Result<Value, ToolError> {
        self.check_active()?;
        let task = self.task_delivery_context()?.filter(|task| task.identity.source == "feishu")
            .ok_or_else(|| ToolError::Failed("Feishu notifications require an active task result from the original private conversation; use session.reply for user messages".into()))?;
        if args.content.trim().is_empty() || args.content.len() > 20480 {
            return Err(ToolError::Failed(
                "message content is empty or exceeds platform limits".into(),
            ));
        }
        let send = SendArguments {
            bot_graph: None,
            userid: task.identity.sender_id,
            content: args.content,
        };
        let request_id = self.delivery_request_id(&send.userid, None)?;
        let (socket, token) = self.send_endpoint(&send, ChannelPlatform::Feishu)?;
        let mut receipt = self
            .deliver(
                socket,
                json!({
                    "operation":"send","request_id":request_id,"open_id":send.userid,
                    "chat_id":task.identity.conversation_id,"content":send.content,"token":token,
                }),
            )
            .await
            .map_err(|error| {
                ToolError::Failed(format!("message {request_id} was not confirmed: {error}"))
            })?;
        receipt["status"] = json!("confirmed");
        Ok(receipt)
    }
}
