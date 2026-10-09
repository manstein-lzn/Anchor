//! AgentNode command tools delegated to the host-authorized Bubblewrap adapter.

use anchor_runtime::{
    Cancellation, NetworkPolicy, ReadOnlyInput, SandboxError, SandboxPort, SandboxRequest,
    SandboxStatus, SpillPolicy, ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent,
};
use anchor_sandbox_bwrap::BubblewrapSandbox;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub(crate) const RUN_TOOL_NAME: &str = "anchor_run";
pub(crate) const READ_TOOL_NAME: &str = "anchor_read";
pub(crate) const EDIT_TOOL_NAME: &str = "anchor_edit";

/// Lines returned by one `anchor_read` call unless the model asks for fewer.
const READ_DEFAULT_LINES: usize = 200;
/// Hard ceiling for one `anchor_read` call.
const READ_MAX_LINES: usize = 400;
/// Byte ceiling for one `anchor_read` preview.
const READ_MAX_BYTES: usize = 256 * 1024;

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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    path: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

/// One edit request. `content` replaces the whole file; otherwise `old_string`
/// and `new_string` describe a counted replacement.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArguments {
    path: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    old_string: Option<String>,
    #[serde(default)]
    new_string: Option<String>,
    #[serde(default)]
    expected_matches: Option<u64>,
    #[serde(default)]
    base_sha256: Option<String>,
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

impl NodeTools {
    /// Resolve a workspace-relative path that must already exist.
    ///
    /// Canonicalizing rejects symlinks that would leave the workspace, which is the
    /// only writable area a node owns.
    fn workspace_read_path(&self, path: &str) -> Result<PathBuf, ToolError> {
        let candidate = self.relative_candidate(path)?;
        let workspace = std::fs::canonicalize(&self.workspace).map_err(|error| {
            ToolError::Failed(format!("anchor file access is unavailable: {error}"))
        })?;
        let resolved = std::fs::canonicalize(self.workspace.join(&candidate)).map_err(|error| {
            ToolError::Failed(format!("`{path}` cannot be read in /workspace: {error}"))
        })?;
        if !resolved.starts_with(&workspace) {
            return Err(ToolError::Failed(format!(
                "`{path}` resolves outside /workspace; only the node workspace is available"
            )));
        }
        if !resolved.is_file() {
            return Err(ToolError::Failed(format!(
                "`{path}` is not a regular file in /workspace"
            )));
        }
        Ok(resolved)
    }

    /// Resolve a workspace-relative path that may not exist yet.
    fn workspace_write_path(&self, path: &str) -> Result<PathBuf, ToolError> {
        let candidate = self.relative_candidate(path)?;
        let workspace = std::fs::canonicalize(&self.workspace).map_err(|error| {
            ToolError::Failed(format!("anchor file access is unavailable: {error}"))
        })?;
        let target = self.workspace.join(&candidate);
        let parent = target.parent().ok_or_else(|| {
            ToolError::Failed(format!("`{path}` has no parent directory in /workspace"))
        })?;
        let resolved_parent = std::fs::canonicalize(parent).map_err(|error| {
            ToolError::Failed(format!(
                "`{path}` cannot be written: its directory does not exist in /workspace ({error})"
            ))
        })?;
        if !resolved_parent.starts_with(&workspace) {
            return Err(ToolError::Failed(format!(
                "`{path}` resolves outside /workspace; only the node workspace is writable"
            )));
        }
        let name = target
            .file_name()
            .ok_or_else(|| self.invalid_arguments("path must name a file"))?;
        Ok(resolved_parent.join(name))
    }

    fn relative_candidate(&self, path: &str) -> Result<PathBuf, ToolError> {
        if path.trim().is_empty() {
            return Err(self.invalid_arguments("path must not be empty"));
        }
        let candidate = Path::new(path);
        if candidate.is_absolute()
            || candidate.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(self.invalid_arguments(format!(
                "`{path}` must be relative to /workspace without `..` or a leading `/`"
            )));
        }
        Ok(candidate.to_path_buf())
    }

    fn read_description(&self) -> String {
        format!(
            "Read a text file inside /workspace by line. `path` is relative to /workspace; absolute paths and `..` are refused. \
             Returns at most {READ_DEFAULT_LINES} lines (ceiling {READ_MAX_LINES}) or {READ_MAX_BYTES} bytes with 1-based line numbers, plus the file's `sha256`. \
             Page with `offset`/`next_offset`, and pass `sha256` as `base_sha256` to anchor_edit."
        )
    }

    fn edit_description(&self) -> String {
        "Modify a text file inside /workspace. Either pass `content` to replace the whole file \
         (requires `base_sha256` from anchor_read unless the file does not exist yet) or pass `old_string` \
         and `new_string` to replace exactly `expected_matches` occurrences (default 1). \
         The edit changes nothing and fails when the file hash or the match count does not match, so read the file first."
            .to_owned()
    }

    fn read_file(&self, arguments: Value) -> Result<Vec<ToolResultContent>, ToolError> {
        let arguments: ReadArguments =
            serde_json::from_value(arguments).map_err(|error| self.invalid_arguments(error))?;
        let path = self.workspace_read_path(&arguments.path)?;
        let bytes = std::fs::read(&path).map_err(|error| {
            ToolError::Failed(format!("anchor_read failed: `{}`: {error}", arguments.path))
        })?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            return Ok(vec![ToolResultContent::json(json!({
                "path": arguments.path,
                "binary": true,
                "bytes": bytes.len(),
                "sha256": sha256,
                "note": "binary file: lines are not returned; use anchor_run with a byte-level tool",
            }))]);
        }
        let text = String::from_utf8_lossy(&bytes);
        let total_lines = text.lines().count();
        let offset = arguments.offset.unwrap_or(0) as usize;
        let limit = arguments
            .limit
            .unwrap_or(READ_DEFAULT_LINES as u64)
            .clamp(1, READ_MAX_LINES as u64) as usize;
        let mut lines = Vec::new();
        let mut bytes_used = 0usize;
        let mut truncated_for_bytes = false;
        for (index, line) in text.lines().enumerate().skip(offset) {
            if lines.len() >= limit {
                break;
            }
            if !lines.is_empty() && bytes_used + line.len() > READ_MAX_BYTES {
                truncated_for_bytes = true;
                break;
            }
            bytes_used += line.len() + 1;
            lines.push(json!({"line": index + 1, "text": line}));
        }
        let returned = lines.len();
        let next_offset = offset + returned;
        let truncated = truncated_for_bytes || next_offset < total_lines;
        Ok(vec![ToolResultContent::json(json!({
            "path": arguments.path,
            "binary": false,
            "bytes": bytes.len(),
            "sha256": sha256,
            "total_lines": total_lines,
            "offset": offset,
            "returned_lines": returned,
            "lines": lines,
            "truncated": truncated,
            "next_offset": truncated.then_some(next_offset),
        }))])
    }

    fn edit_file(&self, arguments: Value) -> Result<Vec<ToolResultContent>, ToolError> {
        let arguments: EditArguments =
            serde_json::from_value(arguments).map_err(|error| self.invalid_arguments(error))?;
        let path = self.workspace_write_path(&arguments.path)?;
        let existing = std::fs::read(&path).ok();
        let current_hash = existing
            .as_ref()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
        let (before, after, replaced, created) = match (
            &arguments.content,
            &arguments.old_string,
            &arguments.new_string,
        ) {
            (Some(content), None, None) => {
                match (&arguments.base_sha256, &current_hash) {
                    (None, None) => {}
                    (None, Some(_)) => {
                        return Err(ToolError::Failed(format!(
                            "anchor_edit refused: `{}` already exists, so `base_sha256` is required; read it with anchor_read first. No change was made.",
                            arguments.path
                        )));
                    }
                    (Some(expected), current) => {
                        if current.as_deref() != Some(expected.as_str()) {
                            return Err(self.edit_conflict(&arguments.path, current.as_deref()));
                        }
                    }
                }
                (
                    existing.as_ref().map_or(0, Vec::len),
                    content.clone().into_bytes(),
                    0,
                    existing.is_none(),
                )
            }
            (None, Some(old), Some(new)) => {
                let Some(current) = existing
                    .as_ref()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                else {
                    return Err(ToolError::Failed(format!(
                        "anchor_edit failed: `{}` does not exist; pass `content` to create it",
                        arguments.path
                    )));
                };
                if let Some(expected) = &arguments.base_sha256
                    && current_hash.as_deref() != Some(expected.as_str())
                {
                    return Err(self.edit_conflict(&arguments.path, current_hash.as_deref()));
                }
                let expected = arguments.expected_matches.unwrap_or(1);
                if expected == 0 {
                    return Err(self.invalid_arguments("expected_matches must be at least 1"));
                }
                let found = current.matches(old.as_str()).count();
                if found as u64 != expected {
                    return Err(ToolError::Failed(format!(
                        "anchor_edit refused: `{old}` occurs {found} time(s) in `{}` but expected_matches is {expected}; pass the exact count, or use `content` with `base_sha256` for a whole-file write. No change was made.",
                        arguments.path
                    )));
                }
                (
                    current.len(),
                    current.replace(old.as_str(), new.as_str()).into_bytes(),
                    found,
                    false,
                )
            }
            _ => {
                return Err(self.invalid_arguments(
                    "pass either `content`, or both `old_string` and `new_string`",
                ));
            }
        };
        let temporary = path.with_file_name(format!(
            ".{}.anchor-edit-{}",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_owned()),
            std::process::id()
        ));
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(&after)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)
        };
        if let Err(error) = write() {
            let _ = std::fs::remove_file(&temporary);
            return Err(ToolError::Failed(format!(
                "anchor_edit failed to write `{}`: {error}",
                arguments.path
            )));
        }
        Ok(vec![ToolResultContent::json(json!({
            "path": arguments.path,
            "created": created,
            "replaced": replaced,
            "bytes_before": before,
            "bytes_after": after.len(),
            "sha256_before": current_hash,
            "sha256_after": format!("{:x}", Sha256::digest(&after)),
        }))])
    }

    fn edit_conflict(&self, path: &str, current: Option<&str>) -> ToolError {
        ToolError::Failed(format!(
            "anchor_edit refused: `{path}` changed since it was read (current sha256 {}); read it again with anchor_read and retry. No change was made.",
            current.unwrap_or("absent")
        ))
    }
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
        definitions.push(ToolDefinition::new(
            ToolName::new(READ_TOOL_NAME).expect("static tool name"),
            self.read_description(),
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "minLength": 1},
                    "offset": {"type": "integer", "minimum": 0},
                    "limit": {"type": "integer", "minimum": 1, "maximum": READ_MAX_LINES}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        ));
        definitions.push(ToolDefinition::new(
            ToolName::new(EDIT_TOOL_NAME).expect("static tool name"),
            self.edit_description(),
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "minLength": 1},
                    "content": {"type": "string"},
                    "old_string": {"type": "string", "minLength": 1},
                    "new_string": {"type": "string"},
                    "expected_matches": {"type": "integer", "minimum": 1},
                    "base_sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        ));
        definitions
    }

    fn is_read_only(&self, name: &str) -> bool {
        match name {
            READ_TOOL_NAME => true,
            RUN_TOOL_NAME | EDIT_TOOL_NAME => false,
            other => self.inner.is_read_only(other),
        }
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            match name {
                READ_TOOL_NAME => return self.read_file(arguments),
                EDIT_TOOL_NAME => return self.edit_file(arguments),
                RUN_TOOL_NAME => {}
                other => return self.inner.call(other, arguments).await,
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
