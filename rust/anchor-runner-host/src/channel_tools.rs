//! Rust AgentNode bindings for the already supervised channel gateway.
//!
//! The gateway remains the owner of platform credentials, delivery retries and
//! EventLedger facts.  These tools only validate the node side of the contract,
//! call its private Unix socket, or persist the final rich reply for the host
//! adapter to deliver.

use crate::channel_replies::{MAX_IMAGE_BYTES, MAX_IMAGE_COUNT, MAX_IMAGE_TOTAL_BASE64};
use crate::tool_names::{IMAGE_TOOL, SEND_FILE_TOOL, SEND_TOOL};

use anchor_runtime::{
    Cancellation, ReadOnlyInput, ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::ImageFormat;
use md5::{Digest as Md5Digest, Md5};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    future::Future,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::atomic::Ordering,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArguments {
    #[serde(default)]
    bot_graph: Option<String>,
    userid: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageArguments {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileArguments {
    #[serde(default)]
    bot_graph: Option<String>,
    userid: String,
    path: String,
    content: String,
    #[serde(default)]
    context_files: Vec<String>,
}

/// A mounted channel Plugin grants outbound access to this owner's enabled
/// bots using that Plugin. Ordinary tasks must name the bot explicitly;
/// assistants remain bound to their own bot and alone can attach reply images.
pub(crate) fn wrap(
    inner: std::sync::Arc<dyn ToolPort>,
    bindings: &[anchor_runtime::graph::PluginBinding],
    key: anchor_runtime::graph::InvocationKey,
    application: crate::application::RunApplication,
    workspace: PathBuf,
    readonly_inputs: Vec<ReadOnlyInput>,
    cancellation: Cancellation,
) -> std::sync::Arc<dyn ToolPort> {
    let Ok(Some(metadata)) = application.metadata(&key.run_id) else {
        return inner;
    };
    let Some(owner) = metadata.oauth_owner else {
        return inner;
    };
    if metadata
        .assistant
        .as_ref()
        .is_some_and(|source| source.work_node != key.node_id)
    {
        return inner;
    }
    let catalog = crate::plugin_library::Catalog::new(application.library_root());
    let channels = bindings
        .iter()
        .filter(|binding| {
            catalog
                .channels(&binding.id)
                .is_ok_and(|channels| channels.iter().any(|channel| channel.platform == "wecom"))
        })
        .cloned()
        .collect::<Vec<_>>();
    if channels.is_empty() {
        return inner;
    }
    std::sync::Arc::new(ChannelTools {
        inner,
        key,
        assistant_graph: metadata.assistant.map(|_| metadata.graph),
        channels,
        owner,
        state_root: application.data_root.clone(),
        application,
        workspace,
        readonly_inputs,
        cancellation,
    })
}

struct ChannelTools {
    inner: std::sync::Arc<dyn ToolPort>,
    key: anchor_runtime::graph::InvocationKey,
    state_root: PathBuf,
    application: crate::application::RunApplication,
    owner: String,
    channels: Vec<anchor_runtime::graph::PluginBinding>,
    assistant_graph: Option<String>,
    workspace: PathBuf,
    readonly_inputs: Vec<ReadOnlyInput>,
    cancellation: Cancellation,
}

fn uncertain() -> ToolError {
    ToolError::Failed("channel delivery was not confirmed; do not resend automatically".into())
}

/// Open each component relative to its descriptor: a sandbox process cannot
/// replace a checked directory with a symlink between validation and read.
fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>, ToolError> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let failure = || {
        ToolError::Failed("file must be regular, without symlinks and within its byte limit".into())
    };
    if !path.is_absolute() {
        return Err(failure());
    }
    let mut directory = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| failure())?;
    let components = path
        .components()
        .filter(|part| !matches!(part, Component::RootDir))
        .collect::<Vec<_>>();
    for (index, part) in components.iter().enumerate() {
        if !matches!(part, Component::Normal(_)) {
            return Err(failure());
        }
        let last = index + 1 == components.len();
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let fd = openat(
            &directory,
            Path::new(part.as_os_str()),
            if last {
                flags
            } else {
                flags | OFlags::DIRECTORY
            },
            Mode::empty(),
        )
        .map_err(|_| failure())?;
        if last {
            let file = fs::File::from(fd);
            let metadata = file.metadata().map_err(|_| failure())?;
            if !metadata.is_file() || metadata.len() > limit as u64 {
                return Err(failure());
            }
            let mut bytes = Vec::new();
            file.take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| failure())?;
            if bytes.is_empty() || bytes.len() > limit {
                return Err(failure());
            }
            return Ok(bytes);
        }
        directory = fd;
    }
    Err(failure())
}

impl ChannelTools {
    fn check_active(&self) -> Result<(), ToolError> {
        if self.cancellation.load(Ordering::Relaxed) {
            Err(ToolError::Failed(
                "this Run has been stopped; channel operation cancelled".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn control_endpoint(descriptor: &Path) -> Result<(PathBuf, String), ToolError> {
        let raw = fs::read(descriptor)
            .map_err(|_| ToolError::Failed("WeCom gateway descriptor is unavailable".into()))?;
        let value: Value = serde_json::from_slice(&raw)
            .map_err(|_| ToolError::Failed("invalid gateway descriptor".into()))?;
        let socket = value
            .get("socket")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty());
        let token = value
            .get("token")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty());
        match (socket, token) {
            (Some(socket), Some(token)) => Ok((PathBuf::from(socket), token.to_owned())),
            _ => Err(ToolError::Failed("invalid gateway descriptor".into())),
        }
    }

    fn send_endpoint(&self, args: &SendArguments) -> Result<(PathBuf, String), ToolError> {
        let graph = match (&self.assistant_graph, args.bot_graph.as_deref()) {
            (Some(own), Some(requested)) if own != requested => {
                return Err(ToolError::Failed(
                    "an assistant can only send through its own bot".into(),
                ));
            }
            (Some(own), _) => own.as_str(),
            (None, Some(graph)) => graph,
            (None, None) => {
                return Err(ToolError::Failed(
                    "bot_graph is required for task notifications".into(),
                ));
            }
        };
        if !anchor_runtime::identifier::is_name(graph) {
            return Err(ToolError::Failed("invalid bot_graph".into()));
        }
        let bot = self
            .application
            .channel_snapshot(graph)
            .map_err(|_| ToolError::Failed("bot configuration is unavailable".into()))?
            .filter(|bot| {
                bot.enabled && crate::credential_store::binding_owner(&bot.owner) == self.owner
            })
            .ok_or_else(|| {
                ToolError::Failed("bot is unavailable or not authorized for this Run".into())
            })?;
        let binding = self
            .channels
            .iter()
            .find(|binding| binding.id == bot.binding.plugin)
            .ok_or_else(|| {
                ToolError::Failed("the bot's channel Plugin is not mounted by this node".into())
            })?;
        crate::plugin_library::Catalog::new(self.application.library_root())
            .checked_definition(
                &binding.id,
                crate::plugin_library::ExpectedDefinition::Authorization(binding),
            )
            .map_err(|_| {
                ToolError::Failed("channel Plugin authorization changed; start a new Run".into())
            })?;
        if args.userid == "@all"
            || !bot
                .binding
                .send_users
                .iter()
                .any(|user| user == "*" || user == &args.userid)
        {
            return Err(ToolError::Failed("recipient is not allowed".into()));
        }
        let descriptor = bot
            .descriptor
            .ok_or_else(|| ToolError::Failed("WeCom gateway descriptor is unavailable".into()))?;
        Self::control_endpoint(&descriptor)
    }

    fn reply_path(&self) -> Result<PathBuf, ToolError> {
        let binding = crate::assistant::binding(&self.state_root, &self.key)
            .map_err(ToolError::Failed)?
            .ok_or_else(|| {
                ToolError::Failed("reply images require a trusted Turn binding".into())
            })?;
        Ok(self
            .state_root
            .join("channel-replies")
            .join(format!("turn-{}.json", binding.turn)))
    }

    fn visible_file(&self, value: &str) -> Result<PathBuf, ToolError> {
        let virtual_path = if value.starts_with('/') {
            PathBuf::from(value)
        } else {
            Path::new("/workspace").join(value)
        };
        if virtual_path
            .components()
            .any(|c| matches!(c, Component::ParentDir) || c.as_os_str() == ".git")
        {
            return Err(ToolError::Failed(
                "image path must stay inside an allowed workspace/input".into(),
            ));
        }
        let mut mounts = vec![(self.workspace.as_path(), Path::new("/workspace"))];
        mounts.extend(
            self.readonly_inputs
                .iter()
                .filter(|input| input.destination.starts_with("/in"))
                .map(|input| (input.source.as_path(), input.destination.as_path())),
        );
        mounts.sort_by_key(|(_, dest)| std::cmp::Reverse(dest.components().count()));
        for (root, dest) in mounts {
            if let Ok(relative) = virtual_path.strip_prefix(dest) {
                return Ok(root.join(relative));
            }
        }
        Err(ToolError::Failed(
            "image path is not visible to this node".into(),
        ))
    }

    fn image_item(&self, path: &str) -> Result<(Value, String), ToolError> {
        let target = self.visible_file(path)?;
        let bytes = read_regular(&target, MAX_IMAGE_BYTES)?;
        let mime = match image::guess_format(&bytes) {
            Ok(ImageFormat::Png) => "image/png",
            Ok(ImageFormat::Jpeg) => "image/jpeg",
            _ => {
                return Err(ToolError::Failed(
                    "unsupported image format; PNG/JPEG required".into(),
                ));
            }
        };
        crate::attachments::validate_image(&bytes, mime).map_err(ToolError::Failed)?;
        let encoded = STANDARD.encode(&bytes);
        let mut digest = Md5::new();
        Md5Digest::update(&mut digest, &bytes);
        Ok((
            json!({"msgtype":"image", "image": {
                "base64": encoded,
                "md5": format!("{:x}", Md5Digest::finalize(digest)),
            }}),
            target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("image")
                .to_owned(),
        ))
    }

    fn save_image(&self, item: Value) -> Result<Value, ToolError> {
        self.check_active()?;
        let path = self.reply_path()?;
        let mut values = if path.exists() {
            serde_json::from_slice::<Vec<Value>>(
                &fs::read(&path).map_err(|e| ToolError::Failed(e.to_string()))?,
            )
            .map_err(|_| ToolError::Failed("saved channel reply is invalid".into()))?
        } else {
            Vec::new()
        };
        let md5 = item
            .pointer("/image/md5")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if values
            .iter()
            .any(|value| value.pointer("/image/md5").and_then(Value::as_str) == Some(md5))
        {
            return Ok(json!({"attached":true,"duplicate":true}));
        }
        let size = item
            .pointer("/image/base64")
            .and_then(Value::as_str)
            .map_or(usize::MAX, str::len);
        if values.len() >= MAX_IMAGE_COUNT
            || values
                .iter()
                .map(|value| {
                    value
                        .pointer("/image/base64")
                        .and_then(Value::as_str)
                        .map_or(0, str::len)
                })
                .sum::<usize>()
                + size
                > MAX_IMAGE_TOTAL_BASE64
        {
            return Err(ToolError::Failed(
                "reply images exceed the count/total size limit".into(),
            ));
        }
        values.push(item);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| ToolError::Failed(e.to_string()))?;
        }
        let temporary = path.with_extension("tmp");
        let bytes = serde_json::to_vec(&values).map_err(|e| ToolError::Failed(e.to_string()))?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        file.write_all(&bytes)
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        file.sync_all()
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        self.check_active()?;
        fs::rename(&temporary, &path).map_err(|e| ToolError::Failed(e.to_string()))?;
        fs::File::open(path.parent().expect("reply directory"))
            .and_then(|directory| directory.sync_all())
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(json!({"attached":true,"count":values.len()}))
    }

    async fn send(&self, args: SendArguments) -> Result<Value, ToolError> {
        if self.cancellation.load(Ordering::Relaxed) {
            return Err(ToolError::Failed(
                "this Run has been stopped; channel operation cancelled".into(),
            ));
        }
        if args.userid.trim().is_empty() || args.content.trim().is_empty() {
            return Err(ToolError::Failed("recipient is not allowed".into()));
        }
        let (socket, token) = self.send_endpoint(&args)?;
        let request_id = self.send_request_id()?;
        let payload = json!({"operation":"send","request_id":request_id,"userid":args.userid,"content":args.content,"token":token});
        if args.content.len() > 20480 || args.userid.chars().count() > 200 {
            return Err(ToolError::Failed(
                "message or identifier exceeds platform limits".into(),
            ));
        }
        self.deliver(socket, payload).await
    }

    /// The source must already be an immutable selected delivery. Retention
    /// preserves those exact bytes for the recipient's next assistant Turn.
    fn notification_file(
        &self,
        value: &str,
    ) -> Result<(anchor_platform_session::ChannelNotificationFile, Vec<u8>), ToolError> {
        use anchor_runtime::artifact::{ArtifactDescriptor, is_delivery_path};
        let value = Path::new(value);
        for input in self
            .readonly_inputs
            .iter()
            .filter(|input| input.destination.starts_with("/in"))
        {
            let Ok(relative) = value.strip_prefix(&input.destination) else {
                continue;
            };
            let Some(relative) = relative.to_str().filter(|path| is_delivery_path(path)) else {
                continue;
            };
            let descriptor = ArtifactDescriptor::read(&input.source).map_err(ToolError::Failed)?;
            // /in also includes untrusted uploaded/local files. A descriptor
            // inside one of those inputs cannot grant another Artifact.
            let artifacts = crate::HostArtifacts::new(
                self.state_root.join("artifacts"),
                self.workspace.clone(),
            );
            let reference = artifacts
                .reference(&descriptor.artifact.id)
                .map_err(|error| ToolError::Failed(error.to_string()))?
                .filter(|reference| reference == &descriptor.artifact)
                .ok_or_else(|| {
                    ToolError::Failed("notification source is not a Host delivery".into())
                })?;
            let view = artifacts
                .files_path(&reference)
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            if input
                .source
                .canonicalize()
                .map_err(|error| ToolError::Failed(error.to_string()))?
                != view
                    .canonicalize()
                    .map_err(|error| ToolError::Failed(error.to_string()))?
            {
                return Err(ToolError::Failed(
                    "notification source is not the authorized Host delivery view".into(),
                ));
            }
            let entry = descriptor
                .files
                .get(relative)
                .ok_or_else(|| ToolError::Failed("file is not selected by this delivery".into()))?;
            if entry.bytes > crate::attachments::MAX_FILE_BYTES as u64 {
                return Err(ToolError::Failed("file exceeds the 20 MiB limit".into()));
            }
            let bytes = descriptor
                .read_file(&input.source, relative)
                .map_err(ToolError::Failed)?;
            let name = relative.rsplit('/').next().unwrap_or_default();
            if !crate::attachments::safe_name(name) {
                return Err(ToolError::Failed("invalid notification filename".into()));
            }
            return Ok((
                anchor_platform_session::ChannelNotificationFile {
                    artifact_id: descriptor.artifact.id,
                    path: relative.into(),
                    name: name.into(),
                },
                bytes,
            ));
        }
        Err(ToolError::Failed("send_file requires a committed read-only /in delivery; select the file in the producing node's final_result first".into()))
    }

    async fn send_file(&self, args: FileArguments) -> Result<Value, ToolError> {
        use anchor_platform_session::{
            ChannelConversationKind, ChannelIdentity, ChannelNotification,
        };
        self.check_active()?;
        let text = SendArguments {
            bot_graph: args.bot_graph.clone(),
            userid: args.userid.clone(),
            content: args.content.clone(),
        };
        let (socket, token) = self.send_endpoint(&text)?;
        if args.content.trim().is_empty()
            || args.content.len() > 20480
            || args.context_files.len() > 3
        {
            return Err(ToolError::Failed(
                "a bounded introduction and at most three context files are required".into(),
            ));
        }
        let request_id = self.send_request_id()?;
        let graph = self
            .assistant_graph
            .as_deref()
            .or(args.bot_graph.as_deref())
            .expect("send_endpoint validated bot_graph");
        let bot = self
            .application
            .channel_snapshot(graph)
            .map_err(|_| ToolError::Failed("bot configuration is unavailable".into()))?
            .ok_or_else(|| ToolError::Failed("bot configuration is unavailable".into()))?;
        let sessions = crate::assistant::sessions(&self.state_root).map_err(ToolError::Failed)?;
        let session = sessions
            .channel_session_for_identity(
                &bot.owner,
                &ChannelIdentity {
                    source: "wecom".into(),
                    account: bot.account.clone(),
                    conversation_kind: ChannelConversationKind::Single,
                    conversation_id: args.userid.clone(),
                    sender_id: args.userid.clone(),
                },
            )
            .map_err(|error| ToolError::Failed(error.to_string()))?
            .filter(|session| session.graph == graph)
            .ok_or_else(|| {
                ToolError::Failed(
                    "recipient must have an existing private conversation with this bot".into(),
                )
            })?;
        let (file, bytes) = self.notification_file(&args.path)?;
        if bytes.len() < 5 {
            return Err(ToolError::Failed(
                "platform files must contain at least five bytes".into(),
            ));
        }
        let filename = file.name.clone();
        let mut files = vec![file];
        for path in &args.context_files {
            let (file, _) = self.notification_file(path)?;
            if files.iter().any(|existing| existing.name == file.name) {
                return Err(ToolError::Failed(
                    "notification filenames must be distinct".into(),
                ));
            }
            files.push(file);
        }
        for id in files
            .iter()
            .map(|file| &file.artifact_id)
            .collect::<std::collections::BTreeSet<_>>()
        {
            self.application
                .retain_delivery(id, true, &self.workspace)
                .map_err(|error| {
                    ToolError::Failed(format!("could not retain notification delivery: {error:?}"))
                })?;
        }
        let payload = json!({"operation":"send_file","request_id":format!("{request_id}-file"),"userid":args.userid,
            "name":filename,"data_base64":STANDARD.encode(&bytes),"md5":format!("{:x}", Md5::digest(&bytes)),"token":token});
        // Recheck revocation immediately before each platform effect.
        self.send_endpoint(&text)?;
        let file_ack = self.deliver(socket, payload).await?;
        // Preserve the confirmed file even if the separate introduction fails
        // or execution is cancelled after the file's ACK.
        let notification = ChannelNotification {
            id: request_id.clone(),
            source_run: self.key.run_id.clone(),
            content: args.content.clone(),
            files,
        };
        sessions.record_channel_notification(&bot.owner, &session.id, &notification)
            .map_err(|error| ToolError::Failed(format!("platform acknowledged the file but Session recording failed: {error}; do not resend with a new identity")))?;
        let introduction = async {
            let (socket, token) = self.send_endpoint(&text)?;
            self.deliver(
                socket,
                json!({"operation":"send","request_id":format!("{request_id}-intro"),
            "userid":args.userid,"content":args.content,"token":token}),
            )
            .await
        }.await.map_err(|error| ToolError::Failed(format!("file confirmed and saved for the next Turn; introduction not confirmed: {error}; do not resend the file")))?;
        Ok(
            json!({"accepted":true,"notification":notification,"file":file_ack,"introduction":introduction}),
        )
    }

    async fn deliver(&self, socket: PathBuf, payload: Value) -> Result<Value, ToolError> {
        self.check_active()?;
        let is_file = payload.get("operation").and_then(Value::as_str) == Some("send_file");
        let byte_limit = if is_file {
            28 * 1024 * 1024 + 4096
        } else {
            65536
        };
        let seconds = if is_file { 180 } else { 20 };
        let mut bytes =
            serde_json::to_vec(&payload).map_err(|e| ToolError::Failed(e.to_string()))?;
        bytes.push(b'\n');
        if bytes.len() > byte_limit {
            return Err(ToolError::Failed(
                "message or identifier exceeds platform limits".into(),
            ));
        }
        let value = timeout(Duration::from_secs(seconds), async {
            let mut stream = UnixStream::connect(socket).await.map_err(|_| uncertain())?;
            self.check_active()?;
            stream.write_all(&bytes).await.map_err(|_| uncertain())?;
            let mut response = Vec::new();
            use tokio::io::AsyncReadExt;
            BufReader::new(stream)
                .take(65537)
                .read_until(b'\n', &mut response)
                .await
                .map_err(|_| uncertain())?;
            if response.len() > 65536 {
                return Err(uncertain());
            }
            let value: Value = serde_json::from_slice(&response).map_err(|_| uncertain())?;
            if let Some(error) = value.get("error").and_then(Value::as_str) {
                return Err(ToolError::Failed(error.to_owned()));
            }
            if value.get("accepted") != Some(&Value::Bool(true))
                || value.get("request_id") != payload.get("request_id")
            {
                return Err(uncertain());
            }
            Ok(value)
        })
        .await
        .map_err(|_| uncertain())??;
        // Preserve a confirmed ACK even if cancellation arrived during delivery.

        Ok(value)
    }

    fn send_request_id(&self) -> Result<String, ToolError> {
        crate::goose_tool_context::channel_request_id(&self.key)
    }
}

impl ToolPort for ChannelTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.inner.definitions();
        let mut schema = json!({"type":"object","properties":{"userid":{"type":"string"},"content":{"type":"string"}},"required":["userid","content"],"additionalProperties":false});
        if self.assistant_graph.is_none() {
            schema["properties"]["bot_graph"] = json!({"type":"string","description":"The explicitly selected Graph whose enabled bot will send this notification. The bot must belong to this Run's owner and use a channel Plugin mounted by this node."});
            schema["required"]
                .as_array_mut()
                .expect("required fields")
                .push(json!("bot_graph"));
        }
        definitions.push(ToolDefinition::new(ToolName::new(SEND_TOOL).expect("static tool name"),
            "Send Markdown to an explicitly authorized WeCom userid. The gateway owns delivery and idempotency; an error or timeout is not automatically retried.",
            schema));
        let mut schema = json!({"type":"object","properties":{
            "userid":{"type":"string"},"path":{"type":"string","description":"Exact file in a committed read-only /in delivery."},
            "content":{"type":"string","description":"Concise Markdown introduction sent after the file."},
            "context_files":{"type":"array","items":{"type":"string"},"maxItems":3,"description":"Selected committed companion files for the recipient's later discussion; these are not sent to WeCom."}},
            "required":["userid","path","content"],"additionalProperties":false});
        if self.assistant_graph.is_none() {
            schema["properties"]["bot_graph"] = json!({"type":"string"});
            schema["required"]
                .as_array_mut()
                .expect("required")
                .push(json!("bot_graph"));
        }
        definitions.push(ToolDefinition::new(ToolName::new(SEND_FILE_TOOL).expect("static tool name"),
            "Send one committed file (up to 20 MiB), then a Markdown introduction through the authorized bot. Requires an existing private conversation. A file ACK retains its exact selected delivery/context for the recipient's next assistant Turn; the separate introduction can still fail. Success requires both ACKs. Unknown results must not be resent automatically.", schema));
        if self.assistant_graph.is_some() {
            definitions.push(ToolDefinition::new(ToolName::new(IMAGE_TOOL).expect("static tool name"),
            "Attach one validated PNG/JPEG from /workspace or an authorized read-only /in input to the final WeCom reply. This prepares the image; it does not send immediately.",
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false})));
        }
        definitions
    }

    fn is_read_only(&self, name: &str) -> bool {
        if name == SEND_TOOL || name == SEND_FILE_TOOL || name == IMAGE_TOOL {
            false
        } else {
            self.inner.is_read_only(name)
        }
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if name == SEND_TOOL {
                let args: SendArguments = serde_json::from_value(arguments)
                    .map_err(|e| ToolError::Failed(format!("invalid WeCom send arguments: {e}")))?;
                return Ok(vec![ToolResultContent::json(self.send(args).await?)]);
            }
            if name == SEND_FILE_TOOL {
                let args: FileArguments = serde_json::from_value(arguments).map_err(|error| {
                    ToolError::Failed(format!("invalid WeCom file arguments: {error}"))
                })?;
                return Ok(vec![ToolResultContent::json(self.send_file(args).await?)]);
            }
            if name == IMAGE_TOOL {
                if self.assistant_graph.is_none() {
                    return Err(ToolError::Unknown(name.into()));
                }
                self.check_active()?;
                let args: ImageArguments = serde_json::from_value(arguments).map_err(|e| {
                    ToolError::Failed(format!("invalid WeCom image arguments: {e}"))
                })?;
                let (item, name) = self.image_item(&args.path)?;
                let mut result = self.save_image(item)?;
                if let Some(object) = result.as_object_mut() {
                    object.insert("name".into(), Value::String(name));
                }
                return Ok(vec![ToolResultContent::json(result)]);
            }
            self.inner.call(name, arguments).await
        })
    }
}

#[cfg(test)]
mod tests;
