//! Stable boundary for commands that need Anchor's sandbox.
//!
//! This module intentionally contains no process, shell, or Bubblewrap code.  A
//! host supplies a [`SandboxPort`] implementation when it has a sandbox
//! backend.  Keeping the request and result types here lets the Rust kernel
//! depend on a small policy contract without granting it host capabilities.

use std::{
    collections::HashSet,
    fmt,
    future::Future,
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::atomic::Ordering,
    time::Duration,
};

use crate::Cancellation;

/// An input mounted read-only inside a sandbox invocation.
///
/// `source` is a host path owned by the host adapter. `destination` is an
/// absolute path in the sandbox namespace.  The port validates its shape but
/// deliberately does not inspect or open the source; that belongs to the
/// concrete sandbox adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyInput {
    pub source: PathBuf,
    pub destination: PathBuf,
}

/// One environment entry. Values may contain credentials, so Debug deliberately
/// reveals the key but never the value.
#[derive(Clone, PartialEq, Eq)]
pub struct SandboxEnvironment {
    pub key: String,
    value: String,
}

impl SandboxEnvironment {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Access the value for a host adapter that constructs the child environment.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Debug for SandboxEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SandboxEnvironment")
            .field("key", &self.key)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// Host-owned output spill destination. The host directory is an authority
/// binding supplied by the host, never a path selected by a model or Graph.
#[derive(Clone, PartialEq, Eq)]
pub struct SpillPolicy {
    pub host_directory: PathBuf,
    /// Maximum total bytes retained beyond stdout/stderr previews.
    pub max_bytes: Option<usize>,
    /// Optional read-only path at which the command can read retained output.
    pub sandbox_mount: Option<PathBuf>,
}

impl fmt::Debug for SpillPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SpillPolicy")
            .field("host_directory", &"[HOST PATH REDACTED]")
            .field("max_bytes", &self.max_bytes)
            .field("sandbox_mount", &self.sandbox_mount)
            .finish()
    }
}

impl ReadOnlyInput {
    pub fn new(source: impl Into<PathBuf>, destination: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            destination: destination.into(),
        }
    }
}

/// Whether the process namespace may reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkPolicy {
    #[default]
    Disabled,
    Enabled,
}

/// The smallest request needed to run one sandboxed command.
///
/// `command` is an argv vector. It is never interpreted as a shell string by
/// this contract. A real adapter remains responsible for choosing whether a
/// shell is permitted, and must preserve all workspace, mount, output,
/// environment, network, timeout, and cancellation boundaries described here.
#[derive(Clone)]
pub struct SandboxRequest {
    pub workspace: PathBuf,
    /// Optional process working directory inside the workspace or a mounted
    /// read-only input. Defaults to `/workspace` in the concrete adapter.
    pub working_directory: Option<PathBuf>,
    pub command: Vec<String>,
    pub readonly_inputs: Vec<ReadOnlyInput>,
    /// Workspace-relative paths mounted read-only after the workspace bind.
    pub workspace_readonly: Vec<PathBuf>,
    /// Absolute host paths prepended to the command's PATH. They do not imply
    /// a mount; required files must also appear in `readonly_inputs`.
    pub tool_dirs: Vec<PathBuf>,
    /// Explicit child environment. Values are redacted from Debug output.
    pub environment: Vec<SandboxEnvironment>,
    pub network: NetworkPolicy,
    pub timeout: Duration,
    /// Maximum preview bytes retained for each output stream.
    pub max_output_bytes: usize,
    /// Optional bounded host spill target for output beyond the previews.
    pub spill: Option<SpillPolicy>,
    pub cancellation: Cancellation,
}

impl fmt::Debug for SandboxRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SandboxRequest")
            .field("workspace", &self.workspace)
            .field("working_directory", &self.working_directory)
            .field("command", &self.command)
            .field("readonly_inputs", &self.readonly_inputs)
            .field("workspace_readonly", &self.workspace_readonly)
            .field("tool_dirs", &self.tool_dirs)
            .field("environment", &self.environment)
            .field("network", &self.network)
            .field("timeout", &self.timeout)
            .field("max_output_bytes", &self.max_output_bytes)
            .field("spill", &self.spill)
            .field("cancellation", &"[HOST HANDLE]")
            .finish()
    }
}

impl SandboxRequest {
    pub fn new(
        workspace: impl Into<PathBuf>,
        command: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            working_directory: None,
            command: command.into_iter().map(Into::into).collect(),
            readonly_inputs: Vec::new(),
            workspace_readonly: Vec::new(),
            tool_dirs: Vec::new(),
            environment: Vec::new(),
            network: NetworkPolicy::default(),
            timeout: Duration::from_secs(30),
            max_output_bytes: 1_000_000,
            spill: None,
            cancellation: Cancellation::default(),
        }
    }

    /// Validate policy-shaped data before a backend receives the request.
    /// This performs no filesystem access and cannot grant a host capability.
    pub fn validate(&self) -> Result<(), SandboxError> {
        if !is_safe_absolute(&self.workspace) {
            return Err(SandboxError::InvalidRequest(
                "workspace must be an absolute path without `..` components".to_owned(),
            ));
        }
        if let Some(working_directory) = &self.working_directory
            && !is_safe_absolute(working_directory)
        {
            return Err(SandboxError::InvalidRequest(
                "working_directory must be an absolute path without `..` components".to_owned(),
            ));
        }
        if self.command.is_empty() {
            return Err(SandboxError::InvalidRequest(
                "command must contain at least one argv item".to_owned(),
            ));
        }
        if self.command.iter().any(|item| item.contains('\0')) {
            return Err(SandboxError::InvalidRequest(
                "command arguments must not contain NUL".to_owned(),
            ));
        }
        if self.timeout.is_zero() {
            return Err(SandboxError::InvalidRequest(
                "timeout must be greater than zero".to_owned(),
            ));
        }
        for input in &self.readonly_inputs {
            if !is_safe_absolute(&input.source) {
                return Err(SandboxError::InvalidRequest(
                    "read-only input source must be an absolute path without `..` components"
                        .to_owned(),
                ));
            }
            if !is_safe_absolute(&input.destination) {
                return Err(SandboxError::InvalidRequest(
                    "read-only input destination must be an absolute path without `..` components"
                        .to_owned(),
                ));
            }
        }
        for path in &self.workspace_readonly {
            if !is_safe_workspace_relative(path) {
                return Err(SandboxError::InvalidRequest(
                    "workspace_readonly entries must be safe relative paths".to_owned(),
                ));
            }
        }
        for path in &self.tool_dirs {
            if !is_safe_absolute(path) {
                return Err(SandboxError::InvalidRequest(
                    "tool_dirs entries must be absolute paths without `..` components".to_owned(),
                ));
            }
        }
        let mut env_keys = HashSet::new();
        for variable in &self.environment {
            if !is_valid_environment_key(&variable.key) {
                return Err(SandboxError::InvalidRequest(
                    "environment keys must match [A-Za-z_][A-Za-z0-9_]*".to_owned(),
                ));
            }
            if variable.value.contains('\0') {
                return Err(SandboxError::InvalidRequest(
                    "environment values must not contain NUL".to_owned(),
                ));
            }
            if !env_keys.insert(&variable.key) {
                return Err(SandboxError::InvalidRequest(format!(
                    "duplicate environment key `{}`",
                    variable.key
                )));
            }
        }
        if let Some(spill) = &self.spill {
            if !is_safe_absolute(&spill.host_directory) {
                return Err(SandboxError::InvalidRequest(
                    "spill host directory must be an absolute path without `..` components"
                        .to_owned(),
                ));
            }
            if let Some(mount) = &spill.sandbox_mount
                && !is_safe_absolute(mount)
            {
                return Err(SandboxError::InvalidRequest(
                    "spill sandbox mount must be an absolute path without `..` components"
                        .to_owned(),
                ));
            }
        }
        Ok(())
    }
}

fn is_safe_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| !matches!(component, Component::ParentDir))
}

fn is_safe_workspace_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn is_valid_environment_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

/// Normalized outcome of one sandbox request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxStatus {
    Completed,
    TimedOut,
    Cancelled,
    NotExecuted,
}

/// Output and status returned by a [`SandboxPort`].
#[derive(Clone, PartialEq, Eq)]
pub struct SandboxResult {
    pub status: SandboxStatus,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub reason: String,
    /// Host-only spill paths. Implementations and callers must not expose these
    /// to the model; Debug redacts them because they identify host storage.
    pub spilled_host_paths: Vec<PathBuf>,
    /// Paths visible inside the sandbox, if spill output was mounted there.
    pub visible_spill_paths: Vec<PathBuf>,
    /// True when output was truncated and could not be retained in full.
    pub incomplete: bool,
}

impl fmt::Debug for SandboxResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SandboxResult")
            .field("status", &self.status)
            .field("exit_code", &self.exit_code)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .field("reason", &self.reason)
            .field("spilled_host_paths", &"[HOST PATHS REDACTED]")
            .field("visible_spill_paths", &self.visible_spill_paths)
            .field("incomplete", &self.incomplete)
            .finish()
    }
}

impl SandboxResult {
    fn not_executed(reason: impl Into<String>) -> Self {
        Self {
            status: SandboxStatus::NotExecuted,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            reason: reason.into(),
            spilled_host_paths: Vec::new(),
            visible_spill_paths: Vec::new(),
            incomplete: false,
        }
    }

    fn cancelled() -> Self {
        Self {
            status: SandboxStatus::Cancelled,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            reason: "cancelled before sandbox execution".to_owned(),
            spilled_host_paths: Vec::new(),
            visible_spill_paths: Vec::new(),
            incomplete: false,
        }
    }
}

/// Errors that prevent a sandbox request from producing a result.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SandboxError {
    #[error("invalid sandbox request: {0}")]
    InvalidRequest(String),
    #[error("sandbox backend unavailable: {0}")]
    Unavailable(String),
    #[error("sandbox backend failed: {0}")]
    Failed(String),
}

/// Host-provided execution port.  The kernel only knows this contract; it
/// never spawns a process or executes a shell itself.
pub trait SandboxPort: Send + Sync {
    fn run<'a>(
        &'a self,
        request: SandboxRequest,
    ) -> Pin<Box<dyn Future<Output = Result<SandboxResult, SandboxError>> + Send + 'a>>;
}

/// Safe placeholder used until a real Bubblewrap adapter is integrated.
///
/// It validates the request and records no host-side effects.  This adapter is
/// useful for provider-free kernel tests, but its `NotExecuted` result must not
/// be treated as a successful command run by a Graph Runner.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopSandbox;

impl SandboxPort for NoopSandbox {
    fn run<'a>(
        &'a self,
        request: SandboxRequest,
    ) -> Pin<Box<dyn Future<Output = Result<SandboxResult, SandboxError>> + Send + 'a>> {
        Box::pin(async move {
            request.validate()?;
            if request.cancellation.load(Ordering::Relaxed) {
                return Ok(SandboxResult::cancelled());
            }
            Ok(SandboxResult::not_executed(
                "no sandbox backend is installed; command was not executed",
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use super::{
        NetworkPolicy, NoopSandbox, ReadOnlyInput, SandboxEnvironment, SandboxError, SandboxPort,
        SandboxRequest, SandboxStatus, SpillPolicy,
    };

    fn base_request() -> SandboxRequest {
        let mut request =
            SandboxRequest::new("/tmp/anchor-runtime-workspace", ["printf", "fixture"]);
        request
            .readonly_inputs
            .push(ReadOnlyInput::new("/tmp/anchor-input", "/in/upstream"));
        request.network = NetworkPolicy::Disabled;
        request
    }

    #[tokio::test]
    async fn noop_validates_and_never_executes_the_argv() {
        let marker = PathBuf::from(format!(
            "/tmp/anchor-rust-sandbox-noop-{}",
            std::process::id()
        ));
        let command = vec!["touch".to_owned(), marker.to_string_lossy().into_owned()];
        let mut request = SandboxRequest::new(&marker, command);
        request
            .readonly_inputs
            .push(ReadOnlyInput::new("/tmp/source", "/in/source"));
        let result = NoopSandbox.run(request).await.expect("valid request");
        assert_eq!(result.status, SandboxStatus::NotExecuted);
        assert_eq!(result.exit_code, None);
        assert!(!marker.exists(), "noop must not execute host commands");
    }

    #[tokio::test]
    async fn noop_reports_pre_cancel_without_running_a_command() {
        let cancellation = Arc::new(AtomicBool::new(true));
        let mut request = base_request();
        request.cancellation = cancellation;
        let result = NoopSandbox.run(request).await.expect("valid request");
        assert_eq!(result.status, SandboxStatus::Cancelled);
        assert!(result.reason.contains("cancelled"));
    }

    #[tokio::test]
    async fn request_rejects_empty_or_unsafe_inputs() {
        let empty = SandboxRequest::new("relative", std::iter::empty::<String>());
        assert!(matches!(
            NoopSandbox.run(empty).await,
            Err(SandboxError::InvalidRequest(_))
        ));

        let mut traversal = base_request();
        traversal.readonly_inputs[0].destination = PathBuf::from("/in/../workspace");
        assert!(matches!(
            NoopSandbox.run(traversal).await,
            Err(SandboxError::InvalidRequest(_))
        ));
    }

    #[test]
    fn request_defaults_to_network_disabled_and_positive_timeout() {
        let request = base_request();
        assert_eq!(request.network, NetworkPolicy::Disabled);
        assert!(!request.timeout.is_zero());
        assert_eq!(request.max_output_bytes, 1_000_000);
        assert!(!request.cancellation.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn request_accepts_declared_sandbox_policy_fields() {
        let mut request = base_request();
        request.workspace_readonly.push(PathBuf::from(".git"));
        request.tool_dirs.push(PathBuf::from("/opt/anchor/bin"));
        request
            .environment
            .push(SandboxEnvironment::new("ANCHOR_NODE", "review"));
        request.max_output_bytes = 4096;
        request.spill = Some(SpillPolicy {
            host_directory: PathBuf::from("/var/tmp/anchor-spill/run-1"),
            max_bytes: Some(16_384),
            sandbox_mount: Some(PathBuf::from("/spill")),
        });
        let result = NoopSandbox.run(request).await.expect("policy is valid");
        assert_eq!(result.status, SandboxStatus::NotExecuted);
        assert!(!result.incomplete);
        assert!(result.spilled_host_paths.is_empty());
        assert!(result.visible_spill_paths.is_empty());
    }

    #[tokio::test]
    async fn request_rejects_unsafe_workspace_readonly_paths_and_tool_dirs() {
        for unsafe_path in ["../.git", "/.git", "a/../../outside"] {
            let mut request = base_request();
            request.workspace_readonly.push(PathBuf::from(unsafe_path));
            assert!(matches!(
                NoopSandbox.run(request).await,
                Err(SandboxError::InvalidRequest(_))
            ));
        }

        let mut request = base_request();
        request.tool_dirs.push(PathBuf::from("relative/bin"));
        assert!(matches!(
            NoopSandbox.run(request).await,
            Err(SandboxError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn request_rejects_invalid_environment_and_spill_paths() {
        let mut request = base_request();
        request
            .environment
            .push(SandboxEnvironment::new("BAD=KEY", "value"));
        assert!(matches!(
            NoopSandbox.run(request).await,
            Err(SandboxError::InvalidRequest(_))
        ));

        let mut request = base_request();
        request
            .environment
            .push(SandboxEnvironment::new("TOKEN", "a"));
        request
            .environment
            .push(SandboxEnvironment::new("TOKEN", "b"));
        assert!(matches!(
            NoopSandbox.run(request).await,
            Err(SandboxError::InvalidRequest(_))
        ));

        let mut request = base_request();
        request
            .environment
            .push(SandboxEnvironment::new("TOKEN", "bad\0value"));
        assert!(matches!(
            NoopSandbox.run(request).await,
            Err(SandboxError::InvalidRequest(_))
        ));

        let mut request = base_request();
        request.spill = Some(SpillPolicy {
            host_directory: PathBuf::from("relative/spill"),
            max_bytes: Some(100),
            sandbox_mount: None,
        });
        assert!(matches!(
            NoopSandbox.run(request).await,
            Err(SandboxError::InvalidRequest(_))
        ));
    }

    #[test]
    fn debug_redacts_environment_values_and_host_spill_paths() {
        let mut request = base_request();
        request.environment.push(SandboxEnvironment::new(
            "API_TOKEN",
            "secret-value-that-must-not-be-logged",
        ));
        request.spill = Some(SpillPolicy {
            host_directory: PathBuf::from("/private/anchor/run/spill"),
            max_bytes: Some(20),
            sandbox_mount: Some(PathBuf::from("/spill")),
        });
        let debug = format!("{request:?}");
        assert!(debug.contains("API_TOKEN"));
        assert!(!debug.contains("secret-value-that-must-not-be-logged"));
        assert!(!debug.contains("/private/anchor/run/spill"));

        let result = super::SandboxResult {
            status: SandboxStatus::Completed,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            reason: String::new(),
            spilled_host_paths: vec![PathBuf::from("/private/anchor/run/spill/stdout.txt")],
            visible_spill_paths: vec![PathBuf::from("/spill/stdout.txt")],
            incomplete: false,
        };
        let debug = format!("{result:?}");
        assert!(!debug.contains("/private/anchor/run/spill"));
        assert!(debug.contains("/spill/stdout.txt"));
    }
}
