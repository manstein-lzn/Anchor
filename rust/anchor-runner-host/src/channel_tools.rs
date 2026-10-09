//! Rust AgentNode bindings for the already supervised channel gateway.
//!
//! The gateway remains the owner of platform credentials, delivery retries and
//! EventLedger facts.  These tools only validate the node side of the contract,
//! call its private Unix socket, or persist the final rich reply for the host
//! adapter to deliver.

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

pub(crate) const SEND_TOOL: &str = "wecom_send_message";
pub(crate) const IMAGE_TOOL: &str = "wecom_attach_image";

const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_IMAGE_COUNT: usize = 10;
const MAX_IMAGE_TOTAL_BASE64: usize = 14 * 1024 * 1024;

/// Read the rich reply one Run registered for the channel gateway.
///
/// The file is the Host's own record, but it is read back after the node
/// finished, so every item is re-validated here: this is the last point that
/// still holds the Host's authority over the bytes the gateway will deliver.
pub(crate) fn read_reply_images(state_root: &Path, run_id: &str) -> Result<Vec<Value>, String> {
    let path = state_root
        .join("channel-replies")
        .join(format!("{run_id}.json"));
    read_reply_images_at(&path)
}

pub(crate) fn read_reply_images_for_invocation(
    state_root: &Path,
    key: &anchor_runtime::graph::InvocationKey,
) -> Result<Vec<Value>, String> {
    let binding = crate::assistant::binding(state_root, key)?
        .ok_or("reply images require a trusted Turn binding")?;
    let path = state_root
        .join("channel-replies")
        .join(format!("turn-{}.json", binding.turn));
    read_reply_images_at(&path)
}

fn read_reply_images_at(path: &Path) -> Result<Vec<Value>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let values: Vec<Value> =
        serde_json::from_slice(&bytes).map_err(|_| "saved channel reply is invalid".to_string())?;
    if values.len() > MAX_IMAGE_COUNT {
        return Err("saved channel reply exceeds the image count limit".into());
    }
    let mut total = 0usize;
    for value in &values {
        let object = value
            .as_object()
            .filter(|object| {
                object.len() == 2 && object.get("msgtype").and_then(Value::as_str) == Some("image")
            })
            .ok_or("saved channel reply has an unsupported item")?;
        let image = object
            .get("image")
            .and_then(Value::as_object)
            .filter(|image| image.len() == 2)
            .ok_or("saved channel reply has an unsupported item")?;
        let encoded = image
            .get("base64")
            .and_then(Value::as_str)
            .ok_or("saved channel reply has no image content")?;
        let digest = image
            .get("md5")
            .and_then(Value::as_str)
            .filter(|digest| {
                digest.len() == 32
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or("saved channel reply image digest is invalid")?;
        total = total.saturating_add(encoded.len());
        if total > MAX_IMAGE_TOTAL_BASE64 {
            return Err("saved channel reply exceeds the total image limit".into());
        }
        let data = STANDARD
            .decode(encoded)
            .map_err(|_| "saved channel reply image is not valid base64".to_string())?;
        if data.is_empty() || data.len() > MAX_IMAGE_BYTES {
            return Err("saved channel reply image size is invalid".into());
        }
        let mime = match image::guess_format(&data) {
            Ok(ImageFormat::Png) => "image/png",
            Ok(ImageFormat::Jpeg) => "image/jpeg",
            _ => return Err("saved channel reply image is not PNG or JPEG".into()),
        };
        crate::channel_inputs::validate_image(&data, mime).map_err(|error| error.to_string())?;
        let mut hasher = Md5::new();
        Md5Digest::update(&mut hasher, &data);
        if format!("{:x}", Md5Digest::finalize(hasher)) != digest {
            return Err("saved channel reply image digest does not match its content".into());
        }
    }
    Ok(values)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArguments {
    userid: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageArguments {
    path: String,
}

/// Add channel tools only when the admitted node explicitly mounts `wecom`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn wrap(
    inner: std::sync::Arc<dyn ToolPort>,
    bindings: &[anchor_runtime::graph::PluginBinding],
    key: anchor_runtime::graph::InvocationKey,
    state_root: PathBuf,
    reply_node: Option<String>,
    workspace: PathBuf,
    readonly_inputs: Vec<ReadOnlyInput>,
    cancellation: Cancellation,
) -> std::sync::Arc<dyn ToolPort> {
    if !bindings.iter().any(|binding| binding.id == "wecom") {
        return inner;
    }
    std::sync::Arc::new(ChannelTools {
        inner,
        key,
        state_root,
        reply_node,
        workspace,
        readonly_inputs,
        cancellation,
    })
}

struct ChannelTools {
    inner: std::sync::Arc<dyn ToolPort>,
    key: anchor_runtime::graph::InvocationKey,
    state_root: PathBuf,
    reply_node: Option<String>,
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
        ToolError::Failed(
            "image must be a regular file without symlinks and within its byte limit".into(),
        )
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

    fn control_endpoint(&self) -> Result<(PathBuf, String), ToolError> {
        if let (Some(socket), Ok(token)) = (
            std::env::var_os("ANCHOR_CHANNEL_CONTROL_SOCKET").map(PathBuf::from),
            std::env::var("ANCHOR_CHANNEL_CONTROL_TOKEN"),
        ) {
            return Ok((socket, token));
        }
        let descriptor = std::env::var_os("ANCHOR_CHANNEL_CONTROL_DESCRIPTOR")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.state_root.join("channels/wecom/control.json"));
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

    fn allowed_recipient(userid: &str) -> bool {
        let raw = std::env::var("ANCHOR_WECOM_SEND_USERS")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var("ANCHOR_WECOM_USERS").ok())
            .unwrap_or_default();
        let allowed = raw
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty());
        userid != "@all" && allowed.clone().any(|value| value == "*" || value == userid)
    }

    fn reply_path(&self) -> Result<PathBuf, ToolError> {
        if crate::assistant::source(&self.state_root, &self.key)
            .map_err(ToolError::Failed)?
            .is_some()
        {
            let binding = crate::assistant::binding(&self.state_root, &self.key)
                .map_err(ToolError::Failed)?
                .ok_or_else(|| {
                    ToolError::Failed("reply images require a trusted Turn binding".into())
                })?;
            return Ok(self
                .state_root
                .join("channel-replies")
                .join(format!("turn-{}.json", binding.turn)));
        }
        Ok(self
            .state_root
            .join("channel-replies")
            .join(format!("{}.json", self.key.run_id)))
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
                .filter(|input| {
                    input.destination.starts_with("/in")
                        || input.destination == Path::new("/previous")
                })
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
        crate::channel_inputs::validate_image(&bytes, mime).map_err(ToolError::Failed)?;
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
        if args.userid.trim().is_empty()
            || args.content.trim().is_empty()
            || !Self::allowed_recipient(&args.userid)
        {
            return Err(ToolError::Failed("recipient is not allowed".into()));
        }
        let request_id = self.send_request_id()?;
        let (socket, token) = self.control_endpoint()?;
        let payload = json!({"operation":"send","request_id":request_id,"userid":args.userid,"content":args.content,"token":token});
        if args.content.len() > 20480 || args.userid.chars().count() > 200 {
            return Err(ToolError::Failed(
                "message or identifier exceeds platform limits".into(),
            ));
        }
        self.deliver(socket, payload).await
    }

    async fn deliver(&self, socket: PathBuf, payload: Value) -> Result<Value, ToolError> {
        self.check_active()?;
        let mut bytes =
            serde_json::to_vec(&payload).map_err(|e| ToolError::Failed(e.to_string()))?;
        bytes.push(b'\n');
        if bytes.len() > 64 * 1024 {
            return Err(ToolError::Failed(
                "message or identifier exceeds platform limits".into(),
            ));
        }
        let value = timeout(Duration::from_secs(20), async {
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
        definitions.push(ToolDefinition::new(ToolName::new(SEND_TOOL).expect("static tool name"),
            "Send Markdown to an explicitly authorized WeCom userid. The gateway owns delivery and idempotency; an error or timeout is not automatically retried.",
            json!({"type":"object","properties":{"userid":{"type":"string"},"content":{"type":"string"}},"required":["userid","content"],"additionalProperties":false})));
        if self.reply_node.as_deref() == Some(self.key.node_id.as_str()) {
            definitions.push(ToolDefinition::new(ToolName::new(IMAGE_TOOL).expect("static tool name"),
                "Attach one validated PNG/JPEG from /workspace or an authorized read-only /in input to the final WeCom reply. This prepares the image; it does not send immediately.",
                json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false})));
        }
        definitions
    }

    fn is_read_only(&self, name: &str) -> bool {
        if name == SEND_TOOL || name == IMAGE_TOOL {
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
            if name == IMAGE_TOOL {
                self.check_active()?;
                if self.reply_node.as_deref() != Some(self.key.node_id.as_str()) {
                    return Err(ToolError::Unknown(name.to_owned()));
                }
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
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};
    struct Empty;
    impl ToolPort for Empty {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![]
        }
        fn call<'a>(
            &'a self,
            name: &'a str,
            _: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>>
        {
            Box::pin(async move { Err(ToolError::Unknown(name.into())) })
        }
    }
    fn tools(root: &Path) -> ChannelTools {
        fs::create_dir_all(root.join("workspace")).unwrap();
        ChannelTools {
            inner: Arc::new(Empty),
            key: anchor_runtime::graph::InvocationKey {
                run_id: "run".into(),
                graph_digest: "digest".into(),
                node_id: "reply".into(),
                invocation: 1,
            },
            state_root: root.into(),
            reply_node: Some("reply".into()),
            workspace: root.join("workspace"),
            readonly_inputs: vec![
                ReadOnlyInput::new(root.join("input"), "/in/channel"),
                ReadOnlyInput::new(root.join("plugin"), "/plugins/wecom"),
            ],
            cancellation: Arc::new(AtomicBool::new(false)),
        }
    }
    fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }
    #[test]
    fn goose_gateway_send_refuses_before_connecting_or_inventing_an_attempt() {
        let root = tempfile::tempdir().unwrap();
        let host = tools(root.path());
        assert!(
            host.send_request_id()
                .unwrap_err()
                .to_string()
                .contains("no message was sent")
        );
        assert!(!host.state_root.join("io-harness").exists());
    }

    fn delivery_payload(request_id: &str) -> Value {
        json!({"operation":"send","request_id":request_id,"userid":"fixture-user","content":"same body","token":"fixture-token"})
    }

    async fn gateway_request(stream: &mut UnixStream) -> Value {
        let mut bytes = Vec::new();
        BufReader::new(stream)
            .read_until(b'\n', &mut bytes)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn scoped_concurrent_calls_with_identical_bodies_have_independent_acks() {
        use crate::goose_tool_context::{GooseToolIdentity, scope};
        let root = tempfile::tempdir().unwrap();
        let host = tools(root.path());
        let socket = root.path().join("gateway.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let gateway = async {
            let mut payloads = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let payload = gateway_request(&mut stream).await;
                let response = json!({"accepted":true,"request_id":payload["request_id"]});
                stream
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
                payloads.push(payload);
            }
            payloads
        };
        let send = |tool_call: &str| {
            scope(
                Some(GooseToolIdentity {
                    key: host.key.clone(),
                    session: "fixture-session".into(),
                    tool_call: tool_call.into(),
                }),
                async {
                    let request_id = host.send_request_id().unwrap();
                    host.deliver(socket.clone(), delivery_payload(&request_id))
                        .await
                        .unwrap()
                },
            )
        };
        let (first, second, payloads) =
            tokio::join!(send("native-first"), send("native-second"), gateway);
        assert_ne!(first["request_id"], second["request_id"]);
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0]["content"], payloads[1]["content"]);
        assert!(host.send_request_id().is_err());
    }

    #[tokio::test]
    async fn late_cancellation_preserves_confirmed_gateway_ack() {
        let root = tempfile::tempdir().unwrap();
        let host = tools(root.path());
        let socket = root.path().join("gateway.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let gateway = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let payload = gateway_request(&mut stream).await;
            host.cancellation.store(true, Ordering::SeqCst);
            stream
                .write_all(
                    format!(
                        "{}\n",
                        json!({"accepted":true,"request_id":payload["request_id"]})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        };
        let (result, _) = tokio::join!(
            host.deliver(socket, delivery_payload("fixture-request")),
            gateway
        );
        assert_eq!(result.unwrap()["accepted"], true);
        assert!(host.check_active().is_err());
    }

    #[tokio::test]
    async fn stopped_or_unauthorized_calls_do_not_connect() {
        let root = tempfile::tempdir().unwrap();
        let host = tools(root.path());
        let socket = root.path().join("gateway.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let error = host
            .call(SEND_TOOL, json!({"userid":"@all","content":"blocked"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("recipient is not allowed"));
        let error = host.call(SEND_TOOL, json!({"userid":"fixture-user","content":"blocked","agent-tool-call-request-id":"spoofed"})).await.unwrap_err();
        assert!(error.to_string().contains("invalid WeCom send arguments"));
        host.cancellation.store(true, Ordering::SeqCst);
        assert!(
            host.deliver(socket, delivery_payload("fixture-request"))
                .await
                .is_err()
        );
        assert!(
            timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn missing_or_wrong_invocation_identity_never_opens_the_socket() {
        use crate::goose_tool_context::{GooseToolIdentity, scope};
        let root = tempfile::tempdir().unwrap();
        let host = tools(root.path());
        let socket = root.path().join("gateway.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let mut wrong_key = host.key.clone();
        wrong_key.invocation += 1;
        for identity in [
            None,
            Some(GooseToolIdentity {
                key: wrong_key,
                session: "fixture-session".into(),
                tool_call: "native-call".into(),
            }),
        ] {
            let result = scope(identity, async {
                let request_id = host.send_request_id()?;
                host.deliver(socket.clone(), delivery_payload(&request_id))
                    .await
            })
            .await;
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("no message was sent")
            );
        }
        assert!(
            timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn missing_malformed_or_mismatched_ack_is_uncertain_and_never_retried() {
        for response in [
            "",
            "not-json\n",
            "{\"accepted\":false}\n",
            "{\"accepted\":true,\"request_id\":\"wrong\"}\n",
        ] {
            let root = tempfile::tempdir().unwrap();
            let host = tools(root.path());
            let socket = root.path().join("gateway.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let gateway = async {
                let (mut stream, _) = listener.accept().await.unwrap();
                gateway_request(&mut stream).await;
                stream.write_all(response.as_bytes()).await.unwrap();
            };
            let (result, _) = tokio::join!(
                host.deliver(socket, delivery_payload("fixture-request")),
                gateway
            );
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("do not resend automatically")
            );
            assert!(
                timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err()
            );
        }
    }
    #[tokio::test]
    async fn saved_reply_images_are_revalidated_before_the_gateway_delivers_them() {
        let root = tempfile::tempdir().unwrap();
        let host = tools(root.path());
        fs::write(host.workspace.join("image.png"), png()).unwrap();
        host.call(IMAGE_TOOL, json!({"path":"image.png"}))
            .await
            .unwrap();
        let items = read_reply_images(root.path(), "run").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["msgtype"], "image");

        // A digest that no longer matches the bytes must not reach the channel.
        let path = host.reply_path().unwrap();
        let mut tampered: Vec<Value> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        tampered[0]["image"]["md5"] = json!("0".repeat(32));
        fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert!(read_reply_images(root.path(), "run").is_err());

        // Foreign item shapes are refused instead of being silently downgraded.
        for foreign in [
            json!([{"msgtype":"file","image":{"base64":"AA==","md5":"0".repeat(32)}}]),
            json!([{"msgtype":"image"}]),
            json!([{"msgtype":"image","image":{"base64":"AA==","md5":"0".repeat(32),"extra":1}}]),
        ] {
            fs::write(&path, serde_json::to_vec(&foreign).unwrap()).unwrap();
            assert!(read_reply_images(root.path(), "run").is_err());
        }
        assert!(
            read_reply_images(root.path(), "unregistered-run")
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn channel_image_is_scoped_durable_deduplicated_and_cancellable() {
        let root = tempfile::tempdir().unwrap();
        let mut host = tools(root.path());
        fs::write(host.workspace.join("image.png"), png()).unwrap();
        host.call(IMAGE_TOOL, json!({"path":"image.png"}))
            .await
            .unwrap();
        let reopened = tools(root.path());
        let duplicate = reopened
            .call(IMAGE_TOOL, json!({"path":"/workspace/image.png"}))
            .await
            .unwrap();
        assert_eq!(duplicate[0].as_json().unwrap()["duplicate"], true);
        let saved: Vec<Value> =
            serde_json::from_slice(&fs::read(host.reply_path().unwrap()).unwrap()).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(
            STANDARD
                .decode(saved[0]["image"]["base64"].as_str().unwrap())
                .unwrap(),
            png()
        );
        host.cancellation.store(true, Ordering::Relaxed);
        assert!(
            host.call(IMAGE_TOOL, json!({"path":"image.png"}))
                .await
                .is_err()
        );
        host.cancellation.store(false, Ordering::Relaxed);
        host.reply_node = Some("other".into());
        assert_eq!(host.definitions().len(), 1);
        assert!(
            host.call(IMAGE_TOOL, json!({"path":"image.png"}))
                .await
                .is_err()
        );
        assert!(
            host.send(SendArguments {
                userid: "@all".into(),
                content: "blocked".into()
            })
            .await
            .is_err()
        );
        let plain: Arc<dyn ToolPort> = Arc::new(Empty);
        assert!(
            wrap(
                plain,
                &[],
                host.key.clone(),
                root.path().into(),
                None,
                host.workspace,
                vec![],
                host.cancellation
            )
            .definitions()
            .is_empty()
        );
    }
    #[test]
    fn channel_image_rejects_escape_symlinks_fifo_corruption_and_oversize() {
        let root = tempfile::tempdir().unwrap();
        let host = tools(root.path());
        fs::create_dir(root.path().join("input")).unwrap();
        fs::write(root.path().join("input/good.png"), png()).unwrap();
        assert!(host.image_item("/in/channel/good.png").is_ok());
        for path in [
            "/etc/passwd",
            "../input/good.png",
            "/plugins/wecom/secret",
            "/workspace/.git/config",
        ] {
            assert!(host.image_item(path).is_err(), "{path}");
        }
        std::os::unix::fs::symlink(root.path().join("input"), host.workspace.join("link")).unwrap();
        std::os::unix::fs::symlink(
            root.path().join("input/good.png"),
            host.workspace.join("link.png"),
        )
        .unwrap();
        assert!(host.image_item("link/good.png").is_err());
        assert!(host.image_item("link.png").is_err());
        let dir = rustix::fs::open(
            &host.workspace,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        rustix::fs::mkfifoat(&dir, "pipe", rustix::fs::Mode::RUSR).unwrap();
        assert!(host.image_item("pipe").is_err());
        fs::write(host.workspace.join("corrupt.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(host.image_item("corrupt.png").is_err());
        fs::File::create(host.workspace.join("large.png"))
            .unwrap()
            .set_len(MAX_IMAGE_BYTES as u64 + 1)
            .unwrap();
        assert!(host.image_item("large.png").is_err());
    }
    #[test]
    fn channel_image_reply_count_and_total_limits() {
        let root = tempfile::tempdir().unwrap();
        let host = tools(root.path());
        for n in 0..MAX_IMAGE_COUNT {
            host.save_image(json!({"image":{"md5":n.to_string(),"base64":"YQ=="}}))
                .unwrap();
        }
        assert!(
            host.save_image(json!({"image":{"md5":"extra","base64":"YQ=="}}))
                .is_err()
        );
        fs::remove_file(host.reply_path().unwrap()).unwrap();
        assert!(
            host.save_image(
                json!({"image":{"md5":"huge","base64":"a".repeat(MAX_IMAGE_TOTAL_BASE64+1)}})
            )
            .is_err()
        );
    }
}
