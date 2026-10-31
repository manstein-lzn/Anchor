#![forbid(unsafe_code)]
//! A thin native Git tool; Host/Sandbox owns filesystem and network authority.
mod auth;
pub use auth::{Config, credential_response};

use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
const OUTPUT_LIMIT: usize = 64 * 1024;
const MAX_TIMEOUT: u64 = 600;
fn default_timeout() -> u64 {
    120
}
fn default_cwd() -> String {
    ".".into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitRequest {
    pub args: Vec<String>,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

#[derive(Clone)]
pub struct GitServer {
    workspace: Arc<PathBuf>,
    auth: Arc<auth::Authentication>,
    serial: Arc<Mutex<()>>,
}

impl GitServer {
    pub fn new(
        workspace: impl AsRef<Path>,
        config: Config,
        executable: PathBuf,
    ) -> Result<Self, Error> {
        let workspace = workspace.as_ref().canonicalize()?;
        if !workspace.is_dir() {
            return Err("workspace must be a directory".into());
        }
        Ok(Self {
            workspace: Arc::new(workspace),
            auth: Arc::new(auth::Authentication::new(config, executable)?),
            serial: Arc::new(Mutex::new(())),
        })
    }

    pub async fn run(
        &self,
        request: GitRequest,
        cancellation: CancellationToken,
    ) -> Result<Value, Error> {
        if request.args.is_empty()
            || request.args.len() > 1024
            || request.args.iter().any(|arg| arg.contains('\0'))
            || !(1..=MAX_TIMEOUT).contains(&request.timeout_seconds)
        {
            return Err(
                "args must contain 1–1024 Git arguments; timeout_seconds must be 1–600".into(),
            );
        }
        let cwd = Path::new(&request.cwd);
        if cwd.is_absolute()
            || cwd
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err("cwd must be a relative directory inside /workspace".into());
        }
        let cwd = self.workspace.join(cwd).canonicalize()?;
        if !cwd.starts_with(self.workspace.as_ref()) || !cwd.is_dir() {
            return Err("cwd must resolve to a directory inside /workspace".into());
        }
        let _serial = tokio::select! {
            guard = self.serial.lock() => guard,
            _ = cancellation.cancelled() => return Ok(json!({"status":"cancelled","exit_code":null,"stdout":"","stderr":"","truncated":false})),
        };
        let mut command = Command::new("git");
        command
            .current_dir(cwd)
            .env("HOME", self.workspace.as_ref())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("GCM_INTERACTIVE", "never")
            .env("LC_ALL", "C.UTF-8")
            .env_remove("ANCHOR_GIT_SSH_PRIVATE_KEY")
            .env_remove("ANCHOR_GIT_SSH_KNOWN_HOSTS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        self.auth.apply(&mut command)?;
        command.args(&request.args);
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|_| "native git could not start; install git in the deployment PATH")?;
        let group = ProcessGroup(
            child
                .id()
                .and_then(|pid| rustix::process::Pid::from_raw(pid as i32)),
        );
        let mut stdout = Capture::new(child.stdout.take().expect("piped stdout"));
        let mut stderr = Capture::new(child.stderr.take().expect("piped stderr"));
        let result = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(request.timeout_seconds), async {
                let (status, out, err) = tokio::join!(child.wait(), stdout.drain(), stderr.drain());
                out?; err?; status
            }) => match result {
                Ok(result) => ("exited", Some(result?)),
                Err(_) => ("timed_out", None),
            },
            _ = cancellation.cancelled() => ("cancelled", None),
        };
        // Kill any hook/helper descendants too, including on a dropped MCP call.
        drop(group);
        if result.1.is_none() {
            let _ = child.wait().await;
        }
        Ok(
            json!({"status":result.0,"exit_code":result.1.and_then(|status| status.code()),
            "stdout":self.auth.redact(String::from_utf8_lossy(&stdout.bytes).into_owned()),
            "stderr":self.auth.redact(String::from_utf8_lossy(&stderr.bytes).into_owned()),
            "truncated":stdout.truncated || stderr.truncated}),
        )
    }
}

struct ProcessGroup(Option<rustix::process::Pid>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

struct Capture<R> {
    stream: R,
    bytes: Vec<u8>,
    truncated: bool,
}
impl<R: AsyncRead + Unpin> Capture<R> {
    fn new(stream: R) -> Self {
        Self {
            stream,
            bytes: Vec::new(),
            truncated: false,
        }
    }
    async fn drain(&mut self) -> std::io::Result<()> {
        let mut buffer = [0u8; 8192];
        loop {
            let count = self.stream.read(&mut buffer).await?;
            if count == 0 {
                return Ok(());
            }
            let keep = count.min(OUTPUT_LIMIT - self.bytes.len());
            self.bytes.extend_from_slice(&buffer[..keep]);
            self.truncated |= keep < count;
        }
    }
}

impl ServerHandler for GitServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("anchor-git", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = json!({"type":"object","properties":{
            "args":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":1024,"description":"Native Git arguments, without leading git or shell syntax."},
            "cwd":{"type":"string","default":".","description":"Existing directory relative to /workspace."},
            "timeout_seconds":{"type":"integer","minimum":1,"maximum":MAX_TIMEOUT,"default":default_timeout()}
        },"required":["args"],"additionalProperties":false});
        Ok(ListToolsResult {
            tools: vec![Tool::new(
                "git_run",
                "Run native Git in the Agent's workspace. Shares files with anchor_read/edit/run. Uses Plugin-configured credentials for remotes. Returns status, exit_code, bounded stdout/stderr and truncated. Follow user authorization for remote writes and destructive operations; inspect state after interruption before retrying.",
                Arc::new(schema.as_object().unwrap().clone()),
            )],
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if request.name != "git_run" {
            return Err(ErrorData::invalid_params("unknown tool", None));
        }
        let result = match serde_json::from_value(json!(request.arguments.unwrap_or_default())) {
            Ok(arguments) => self.run(arguments, context.ct).await,
            Err(error) => Err(error.into()),
        };
        match result {
            Ok(value) => {
                let content = vec![ContentBlock::text(value.to_string())];
                let mut response = if value["status"] == "exited" && value["exit_code"] == 0 {
                    CallToolResult::success(content)
                } else {
                    CallToolResult::error(content)
                };
                response.structured_content = Some(value);
                Ok(response)
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                self.auth.redact(error.to_string()),
            )])),
        }
    }
}

#[cfg(test)]
mod tests;
