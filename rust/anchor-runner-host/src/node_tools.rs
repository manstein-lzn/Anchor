//! AgentNode command tools delegated to the host-authorized Bubblewrap adapter.

use anchor_runtime::{
    Cancellation, NetworkPolicy, ReadOnlyInput, SandboxError, SandboxPort, SandboxRequest,
    SandboxStatus, SpillPolicy, ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent,
};
use anchor_sandbox_bwrap::BubblewrapSandbox;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub(crate) const RUN_TOOL_NAME: &str = "anchor_run";

/// Wall-clock limit for one `anchor_run` command.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Bytes retained per stream before the remainder is spilled or dropped.
const DEFAULT_PREVIEW_BYTES: usize = 64 * 1024;
/// Bounded retention for output beyond the previews.
const SPILL_MAX_BYTES: usize = 1024 * 1024;
/// Retained streams kept per invocation; older ones are pruned so a long node
/// cannot fill the state root with output that was already read.
const MAX_SPILL_FILES: usize = 8;
/// How many authorized commands to name in the description and in refusals.
const MAX_LISTED_COMMANDS: usize = 24;

/// Host-chosen destination for output beyond the stream previews.
///
/// The host directory is authority supplied by the Host, never a path chosen by
/// the model; `mount` is where the command can read the retained text.
pub(crate) struct SpillDirectory {
    host: PathBuf,
    mount: PathBuf,
}

impl SpillDirectory {
    pub(crate) fn new(host: impl Into<PathBuf>, mount: impl Into<PathBuf>) -> Self {
        Self {
            host: host.into(),
            mount: mount.into(),
        }
    }
}

/// AgentNode tools own their Plugin port and sandbox adapter so they can cross
/// the async resolver boundary into the io-harness worker runtime.
pub(crate) struct NodeTools {
    inner: Arc<dyn ToolPort>,
    sandbox: Arc<BubblewrapSandbox>,
    workspace: PathBuf,
    readonly_inputs: Vec<ReadOnlyInput>,
    cancellation: Cancellation,
    environment: crate::tool_environment::ToolEnvironment,
    network: NetworkPolicy,
    spill: Option<SpillDirectory>,
    timeout: Duration,
    preview_bytes: usize,
}

impl NodeTools {
    pub fn new(
        inner: Arc<dyn ToolPort>,
        sandbox: Arc<BubblewrapSandbox>,
        workspace: PathBuf,
        readonly_inputs: Vec<ReadOnlyInput>,
        cancellation: Cancellation,
    ) -> Self {
        Self {
            inner,
            sandbox,
            workspace,
            readonly_inputs,
            cancellation,
            environment: Default::default(),
            network: NetworkPolicy::Disabled,
            spill: None,
            timeout: DEFAULT_TIMEOUT,
            preview_bytes: DEFAULT_PREVIEW_BYTES,
        }
    }

    pub fn with_environment(
        mut self,
        environment: crate::tool_environment::ToolEnvironment,
    ) -> Self {
        self.environment = environment;
        self
    }

    pub fn with_network(mut self, enabled: bool) -> Self {
        self.network = if enabled {
            NetworkPolicy::Enabled
        } else {
            NetworkPolicy::Disabled
        };
        self
    }

    /// Retain output beyond the previews at a host-owned, read-only location.
    pub fn with_spill(mut self, spill: SpillDirectory) -> Self {
        self.spill = Some(spill);
        self
    }

    fn authorized_commands_summary(&self) -> String {
        let commands = self.sandbox.allowed_commands();
        if commands.is_empty() {
            return "none".to_owned();
        }
        let listed = commands
            .iter()
            .take(MAX_LISTED_COMMANDS)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if commands.len() > MAX_LISTED_COMMANDS {
            format!("{listed}, … ({} total)", commands.len())
        } else {
            listed
        }
    }

    /// The spill destination only counts when the sandbox policy actually
    /// authorizes its host directory, so the description never promises retention
    /// the adapter would refuse.
    fn effective_spill(&self) -> Option<&SpillDirectory> {
        self.spill
            .as_ref()
            .filter(|spill| self.sandbox.authorizes_spill_root(&spill.host))
    }

    /// The model-visible tool description states the sandbox's real limits so a
    /// command can be shaped to fit them instead of being cut off by surprise.
    fn run_description(&self) -> String {
        let retention = match self.effective_spill() {
            Some(spill) => format!(
                "; longer output is retained read-only at {} and its exact paths are returned as `full_output`",
                spill.mount.display()
            ),
            None => "; longer output is discarded".to_owned(),
        };
        format!(
            "Run an authorized command as an argv array (no shell) in the node sandbox. \
             The working directory is /workspace and is writable; /in and /plugins are read-only inputs. \
             Authorized commands: {}. \
             The command is killed after {}s, and each stream keeps a {} KiB preview{}. \
             A non-zero exit code is a normal result: read `status`, `exit_code`, `stdout`, `stderr` and `incomplete`.",
            self.authorized_commands_summary(),
            self.timeout.as_secs(),
            self.preview_bytes / 1024,
            retention,
        )
    }

    /// Refuse without executing, in a form the model can correct.
    fn refusal(&self, reason: impl std::fmt::Display) -> ToolError {
        let example = self
            .sandbox
            .allowed_commands()
            .into_iter()
            .next()
            .unwrap_or_else(|| "sh".to_owned());
        ToolError::Failed(format!(
            "anchor_run not_executed: {reason} \
             Authorized commands in this sandbox: {}. \
             Pass the command as an argv array without a shell, for example [\"{example}\", \"-c\", \"printf ok\"].",
            self.authorized_commands_summary()
        ))
    }

    fn invalid_arguments(&self, error: impl std::fmt::Display) -> ToolError {
        ToolError::Failed(format!(
            "anchor_run not_executed: invalid arguments: {error}. \
             Pass {{\"command\": [\"<program>\", \"<arg>\", …]}} as an argv array without a shell. \
             Authorized commands in this sandbox: {}.",
            self.authorized_commands_summary()
        ))
    }

    /// The adapter requires a pre-created host directory inside an authorized
    /// spill root; the Host owns both the path and the authorization.
    fn spill_policy(&self) -> Result<Option<SpillPolicy>, ToolError> {
        let Some(spill) = self.effective_spill() else {
            return Ok(None);
        };
        std::fs::create_dir_all(&spill.host).map_err(|error| {
            ToolError::Failed(format!(
                "anchor_run output retention is unavailable: {error}"
            ))
        })?;
        // Best effort: retention must not fail a command, but it must not grow
        // without bound either.
        let _ = prune_spill(&spill.host, MAX_SPILL_FILES);
        Ok(Some(SpillPolicy {
            host_directory: spill.host.clone(),
            max_bytes: Some(SPILL_MAX_BYTES),
            sandbox_mount: Some(spill.mount.clone()),
        }))
    }

    fn hint(&self, status: &str, incomplete: bool, retained: bool) -> Option<String> {
        if status == "timed_out" {
            return Some(format!(
                "the command was killed after the {}s wall-clock limit and did not finish; narrow it, \
                 or write its output to a file under /workspace and read that file in parts.",
                self.timeout.as_secs()
            ));
        }
        if retained {
            return Some(
                "output was longer than the preview; read the retained text from `full_output` \
                 (for example a tail or head of that path with an authorized command)."
                    .to_owned(),
            );
        }
        if incomplete {
            return Some(
                "output was truncated and the remainder was not retained; narrow the command, \
                 or redirect its output to a file under /workspace and read that file in parts."
                    .to_owned(),
            );
        }
        None
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArguments {
    command: Vec<String>,
}

/// Keep the newest `keep` retained streams in a spill directory.
///
/// Retained output is already handed to the model through `full_output`, so a
/// node that truncates output on every call must not accumulate files forever.
fn prune_spill(directory: &Path, keep: usize) -> std::io::Result<()> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "spill")
        {
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok();
            files.push((modified, path));
        }
    }
    files.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    for (_, path) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

impl ToolPort for NodeTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.inner.definitions();
        definitions.push(ToolDefinition::new(
            ToolName::new(RUN_TOOL_NAME).expect("static tool name"),
            self.run_description(),
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "array", "items": {"type": "string"}, "minItems": 1}
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        ));
        definitions
    }

    fn is_read_only(&self, name: &str) -> bool {
        name != RUN_TOOL_NAME && self.inner.is_read_only(name)
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if name != RUN_TOOL_NAME {
                return self.inner.call(name, arguments).await;
            }
            if self.cancellation.load(Ordering::Relaxed) {
                return Err(ToolError::Failed("anchor_run was cancelled".into()));
            }
            let arguments: RunArguments = match serde_json::from_value(arguments) {
                Ok(arguments) => arguments,
                Err(error) => return Err(self.invalid_arguments(error)),
            };
            if arguments.command.is_empty() {
                return Err(self.invalid_arguments("command must contain at least one element"));
            }
            let mut request = SandboxRequest {
                workspace: self.workspace.clone(),
                working_directory: None,
                command: arguments.command,
                readonly_inputs: self.readonly_inputs.clone(),
                workspace_readonly: Vec::new(),
                tool_dirs: Vec::new(),
                environment: Vec::new(),
                network: self.network,
                timeout: self.timeout,
                max_output_bytes: self.preview_bytes,
                spill: self.spill_policy()?,
                cancellation: self.cancellation.clone(),
            };
            self.environment.apply(&mut request);
            let result = match self.sandbox.run(request).await {
                Ok(result) => result,
                // Raised by the adapter before any process launch, so the model
                // can safely correct its command.
                Err(SandboxError::InvalidRequest(reason)) => return Err(self.refusal(reason)),
                Err(error) => return Err(ToolError::Failed(error.to_string())),
            };
            let status = match result.status {
                SandboxStatus::Completed => "completed",
                SandboxStatus::TimedOut => "timed_out",
                SandboxStatus::Cancelled => {
                    return Err(ToolError::Failed("anchor_run was cancelled".into()));
                }
                SandboxStatus::NotExecuted => {
                    return Err(ToolError::Failed("anchor_run was not executed".into()));
                }
            };
            let retained = result
                .visible_spill_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            let mut output = json!({
                "status": status,
                "exit_code": result.exit_code,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "incomplete": result.incomplete
            });
            if !retained.is_empty() {
                output["full_output"] = json!(retained);
            }
            if let Some(hint) = self.hint(status, result.incomplete, !retained.is_empty()) {
                output["hint"] = json!(hint);
            }
            Ok(vec![ToolResultContent::json(output)])
        })
    }
}

#[cfg(test)]
mod tests;
