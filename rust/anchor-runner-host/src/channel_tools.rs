//! Rust AgentNode bindings for the already supervised channel gateway.
//!
//! The gateway remains the owner of platform credentials, delivery retries and
//! EventLedger facts.  These tools only validate the node side of the contract,
//! call its private Unix socket, or persist the final rich reply for the host
//! adapter to deliver.

use anchor_runtime_rig::{Cancellation, ReadOnlyInput, ToolError, ToolPort};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::ImageFormat;
use md5::{Digest as Md5Digest, Md5};
use rig_agent::core::{
    completion::ToolDefinition,
    message::{ToolName, ToolResultContent},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
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
    bindings: &[anchor_runtime_rig::graph::PluginBinding],
    key: anchor_runtime_rig::graph::InvocationKey,
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
    key: anchor_runtime_rig::graph::InvocationKey,
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

    fn control_endpoint() -> Result<(PathBuf, String), ToolError> {
        if let (Some(socket), Ok(token)) = (
            std::env::var_os("ANCHOR_CHANNEL_CONTROL_SOCKET").map(PathBuf::from),
            std::env::var("ANCHOR_CHANNEL_CONTROL_TOKEN"),
        ) {
            return Ok((socket, token));
        }
        let descriptor =
            std::env::var_os("ANCHOR_CHANNEL_CONTROL_DESCRIPTOR").ok_or_else(|| {
                ToolError::Failed("WeCom gateway is unavailable; no message was sent".into())
            })?;
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
        let (socket, token) = Self::control_endpoint()?;
        let attempt = anchor_io_harness_runtime::node_port::active_tool_attempt(
            &self.state_root.join("io-harness/store"),
            &self.key,
            SEND_TOOL,
        )
        .map_err(ToolError::Failed)?;
        let request_id = format!(
            "channel-send-{:x}",
            Sha256::digest(format!("{}:{attempt}", self.key.durable_key()).as_bytes())
        );
        let payload = json!({"operation":"send","request_id":request_id,"userid":args.userid,"content":args.content,"token":token});
        let mut bytes =
            serde_json::to_vec(&payload).map_err(|e| ToolError::Failed(e.to_string()))?;
        bytes.push(b'\n');
        if bytes.len() > 64 * 1024
            || args.content.len() > 20480
            || args.userid.chars().count() > 200
        {
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
            key: anchor_runtime_rig::graph::InvocationKey {
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
