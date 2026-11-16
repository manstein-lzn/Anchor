//! The channel binding, not model arguments or a shared Plugin login, selects
//! the application. Files cross the boundary only by explicit bounded copies.
use super::*;

const FILE_LIMIT: usize = 20 * 1024 * 1024;
const RESPONSE_LIMIT: usize = 30 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Arguments {
    argv: Vec<String>,
    #[serde(default)]
    stdin: String,
    #[serde(default)]
    files: Vec<FileInput>,
    #[serde(default)]
    outputs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileInput {
    path: String,
    name: String,
}

pub(super) fn definition() -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new(FEISHU_CLI_TOOL).expect("static name"),
        "Use official lark-cli as THIS bot's application, never a user login or another bot. argv ['context'] returns the current channel, platform message ID and explicitly authorized peer bot IDs from Host facts. Discover with argv ['--help'], ['skills','list'], ['skills','read','lark-doc'], domain --help, or schema service.resource.method; no per-API Anchor tool is needed. argv ['capabilities'] reads this app's published versions/scopes (filter token_types tenant); failure means unknown, not no permissions. Schema is supported functionality, NOT granted scope or resource access. Business commands use all this app's published scopes and resource ACLs, separately authorized by cli_enabled. No shell, auth/config/profile/event/update or identity flags. Inline JSON or stdin is supported. Stage explicit files as /workspace/name; request output basenames to retain them in this node's workspace. All local state is isolated and temporary. 120s timeout; narrow output using --jq or pagination. accepted confirms adapter execution only: check exit_code and stdout API result. Never retry an uncertain write or change arguments to bypass its receipt. Normal channel replies still use final_result/session.reply. To collaborate, reply to the actual platform message using text and a structured peer @ mention; do not create a new uncorrelated bot message. Sending alone does not prove peer receipt.",
        json!({"type":"object","properties":{
            "argv":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":128,"description":"Arguments without lark-cli. No shell expansion or pipes."},
            "stdin":{"type":"string","description":"Optional literal stdin, e.g. JSON for --data -."},
            "files":{"type":"array","maxItems":8,"items":{"type":"object","properties":{"path":{"type":"string"},"name":{"type":"string"}},"required":["path","name"],"additionalProperties":false},"description":"Explicit visible /workspace or /in input files, total <=20 MiB; name is an ASCII basename in the CLI workspace."},
            "outputs":{"type":"array","maxItems":8,"items":{"type":"string"},"description":"Output basenames to copy back, total <=20 MiB."}
        },"required":["argv"],"additionalProperties":false}),
    )
}

impl ChannelTools {
    pub(super) fn feishu_cli_endpoint(&self) -> Result<(PathBuf, String), ToolError> {
        self.check_active()?;
        let graph = self
            .assistant_graph
            .as_deref()
            .ok_or_else(|| ToolError::Failed("Feishu CLI requires this bot's assistant".into()))?;
        let bot = self
            .application
            .channel_snapshot(graph)
            .map_err(|_| ToolError::Failed("bot configuration unavailable".into()))?
            .filter(|bot| {
                bot.enabled
                    && bot.platform == ChannelPlatform::Feishu
                    && bot.binding.cli_enabled
                    && crate::credential_store::binding_owner(&bot.owner) == self.owner
            })
            .ok_or_else(|| {
                ToolError::Failed(
                    "Feishu application tools are not authorized for this bot (cli_enabled)".into(),
                )
            })?;
        let binding = self
            .channels
            .iter()
            .find(|binding| binding.id == bot.binding.plugin)
            .ok_or_else(|| ToolError::Failed("the channel Plugin is not mounted".into()))?;
        crate::plugin_library::Catalog::new(self.application.library_root())
            .checked_definition(
                &binding.id,
                crate::plugin_library::ExpectedDefinition::Authorization(binding),
            )
            .map_err(|_| {
                ToolError::Failed("Feishu Plugin authorization changed; start a new Turn".into())
            })?;
        let descriptor = bot
            .descriptor
            .ok_or_else(|| ToolError::Failed("Feishu gateway unavailable".into()))?;
        Self::control_endpoint(&descriptor)
    }

    pub(super) async fn feishu_cli(&self, args: Arguments) -> Result<Value, ToolError> {
        let (socket, token) = self.feishu_cli_endpoint()?;
        let validate_turn = || {
            let context = self
                .application
                .active_channel_turn(&self.key)
                .map_err(|error| ToolError::Failed(format!("inactive Feishu Turn: {error:?}")))?;
            if context.identity.source != "feishu"
                || crate::credential_store::binding_owner(&context.source.owner) != self.owner
            {
                return Err(ToolError::Failed(
                    "this application tool requires its own Feishu Turn".into(),
                ));
            }
            let bot = self
                .application
                .channel_snapshot(self.assistant_graph.as_deref().ok_or_else(uncertain)?)
                .map_err(|_| uncertain())?
                .ok_or_else(uncertain)?;
            let identity = context
                .input
                .user()
                .map(|input| &input.request.identity)
                .unwrap_or(&context.identity);
            if identity.conversation_kind == anchor_platform_session::ChannelConversationKind::Group
                && (bot.binding.private_only
                    || !bot.binding.groups.contains(&identity.conversation_id))
            {
                return Err(ToolError::Failed(
                    "this group is no longer authorized".into(),
                ));
            }
            if let Some(input) = context.input.user() {
                let authorized = if input.request.bot_origin.is_some() {
                    bot.binding.peer_bots.contains(&identity.sender_id)
                } else {
                    bot.binding.allows_inbound(&identity.sender_id)
                        && bot.binding.allows_outbound(&identity.sender_id)
                };
                if !authorized {
                    return Err(ToolError::Failed(
                        "this sender is no longer authorized".into(),
                    ));
                }
            }
            Ok(json!({"channel":identity,"peer_bots":bot.binding.peer_bots,
                "platform_message_id":context.input.user().and_then(|input| input.request.platform_message_id.as_ref()),
                "bot_origin":context.input.user().and_then(|input| input.request.bot_origin.as_ref())}))
        };
        let context = validate_turn()?;
        if args.argv == ["context"]
            && args.stdin.is_empty()
            && args.files.is_empty()
            && args.outputs.is_empty()
        {
            return Ok(context);
        }
        let request_id =
            crate::goose_tool_context::operation_request_id(&self.key, FEISHU_CLI_TOOL)?;
        let mut files = Vec::new();
        let mut total = 0;
        if args.files.len() > 8 || args.outputs.len() > 8 {
            return Err(ToolError::Failed(
                "at most 8 staged input/output files".into(),
            ));
        }
        for file in args.files {
            let path = self.visible_file(&file.path)?;
            let bytes = anchor_file_lease::read_regular(&path, FILE_LIMIT.saturating_sub(total))
                .map_err(|_| {
                    ToolError::Failed(
                        "CLI input files must be regular visible files, total <=20 MiB".into(),
                    )
                })?;
            total += bytes.len();
            files.push(json!({"name":file.name,"data_base64":STANDARD.encode(bytes)}));
        }
        let payload = json!({"operation":"cli","token":token,"request_id":request_id,
            "invocation":{"argv":args.argv,"stdin":args.stdin,"files":files,"outputs":args.outputs}});
        let mut bytes = serde_json::to_vec(&payload).map_err(|_| uncertain())?;
        bytes.push(b'\n');
        let mut value: Value = timeout(Duration::from_secs(135), async {
            let mut stream = UnixStream::connect(socket).await.map_err(|_| uncertain())?;
            self.feishu_cli_endpoint()?;
            validate_turn()?;
            stream.write_all(&bytes).await.map_err(|_| uncertain())?;
            let mut response = Vec::new();
            use tokio::io::AsyncReadExt;
            BufReader::new(stream)
                .take(RESPONSE_LIMIT as u64 + 1)
                .read_until(b'\n', &mut response)
                .await
                .map_err(|_| uncertain())?;
            if response.len() > RESPONSE_LIMIT {
                return Err(uncertain());
            }
            serde_json::from_slice(&response).map_err(|_| uncertain())
        })
        .await
        .map_err(|_| uncertain())??;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return Err(ToolError::Failed(error.into()));
        }
        if value["accepted"] != true || value["request_id"] != request_id {
            return Err(uncertain());
        }
        let outputs = value
            .as_object_mut()
            .and_then(|object| object.remove("outputs"));
        let mut saved = Vec::new();
        total = 0;
        for output in outputs
            .as_ref()
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = output["name"]
                .as_str()
                .filter(|name| {
                    !name.is_empty()
                        && Path::new(name).components().count() == 1
                        && matches!(
                            Path::new(name).components().next(),
                            Some(Component::Normal(_))
                        )
                })
                .ok_or_else(uncertain)?;
            if !args.outputs.iter().any(|selected| selected == name) {
                return Err(uncertain());
            }
            let bytes = STANDARD
                .decode(output["data_base64"].as_str().ok_or_else(uncertain)?)
                .map_err(|_| uncertain())?;
            total += bytes.len();
            if total > FILE_LIMIT {
                return Err(uncertain());
            }
            // The invocation ID comes from native tool context, never the model.
            let directory = tempfile::Builder::new()
                .prefix("feishu-cli-")
                .tempdir_in(&self.workspace)
                .map_err(|_| ToolError::Failed("cannot retain CLI output".into()))?;
            let path = directory.path().join(name);
            fs::write(&path, bytes)
                .map_err(|_| ToolError::Failed("cannot retain CLI output".into()))?;
            let relative = path
                .strip_prefix(&self.workspace)
                .map_err(|_| uncertain())?;
            saved.push(json!({"name":name,"path":format!("/workspace/{}", relative.display())}));
            let _ = directory.keep();
        }
        value["files"] = json!(saved);
        Ok(value)
    }
}
