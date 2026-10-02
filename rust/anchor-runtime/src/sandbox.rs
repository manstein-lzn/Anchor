//! Stable boundary for commands that need Anchor's sandbox.
//!
//! This module intentionally contains no process, shell, or Bubblewrap code.  A
//! host supplies a [`SandboxPort`] implementation when it has a sandbox
//! backend.  Keeping the request and result types here lets the Rust kernel
//! depend on a small policy contract without granting it host capabilities.

use std::{
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
/// `command` is an argv vector.  It is never interpreted as a shell string by
/// this contract.  A real adapter remains responsible for choosing whether a
/// shell is permitted, and must preserve the workspace, read-only input,
/// network, timeout, and cancellation boundaries described here.
#[derive(Debug, Clone)]
pub struct SandboxRequest {
    pub workspace: PathBuf,
    pub command: Vec<String>,
    pub readonly_inputs: Vec<ReadOnlyInput>,
    pub network: NetworkPolicy,
    pub timeout: Duration,
    pub cancellation: Cancellation,
}

impl SandboxRequest {
    pub fn new(
        workspace: impl Into<PathBuf>,
        command: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            command: command.into_iter().map(Into::into).collect(),
            readonly_inputs: Vec::new(),
            network: NetworkPolicy::default(),
            timeout: Duration::from_secs(30),
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
        Ok(())
    }
}

fn is_safe_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| !matches!(component, Component::ParentDir))
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxResult {
    pub status: SandboxStatus,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub reason: String,
}

impl SandboxResult {
    fn not_executed(reason: impl Into<String>) -> Self {
        Self {
            status: SandboxStatus::NotExecuted,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            reason: reason.into(),
        }
    }

    fn cancelled() -> Self {
        Self {
            status: SandboxStatus::Cancelled,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            reason: "cancelled before sandbox execution".to_owned(),
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
        NetworkPolicy, NoopSandbox, ReadOnlyInput, SandboxError, SandboxPort, SandboxRequest,
        SandboxStatus,
    };

    fn request() -> SandboxRequest {
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
        let mut request = request();
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

        let mut traversal = request();
        traversal.readonly_inputs[0].destination = PathBuf::from("/in/../workspace");
        assert!(matches!(
            NoopSandbox.run(traversal).await,
            Err(SandboxError::InvalidRequest(_))
        ));
    }

    #[test]
    fn request_defaults_to_network_disabled_and_positive_timeout() {
        let request = request();
        assert_eq!(request.network, NetworkPolicy::Disabled);
        assert!(!request.timeout.is_zero());
        assert!(!request.cancellation.load(Ordering::Relaxed));
    }
}
