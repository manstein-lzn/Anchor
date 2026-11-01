//! AgentNode command tools delegated to the host-authorized Bubblewrap adapter.

use crate::{
    node_host::envelope::DEFAULT_TIMEOUT,
    tool_names::{EDIT_TOOL_NAME, READ_TOOL_NAME, RUN_TOOL_NAME},
};

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

mod workspace_files;

/// Where the sandbox mounts the workspace, as a path prefix.
const WORKSPACE_PREFIX: &str = "/workspace/";

/// Lines returned by one `anchor_read` call unless the model asks for fewer.
const READ_DEFAULT_LINES: usize = 200;
/// Hard ceiling for one `anchor_read` call.
const READ_MAX_LINES: usize = 400;
/// Byte ceiling for one `anchor_read` preview.
const READ_MAX_BYTES: usize = 256 * 1024;

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
    // (binding, deployment library root)
    network: NetworkPolicy,
    spill: Option<SpillDirectory>,
    timeout: Duration,
    timeout_ceiling: Duration,
    preview_bytes: usize,
    development_projects: Option<(PathBuf, PathBuf)>,
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
            timeout_ceiling: DEFAULT_TIMEOUT,
            preview_bytes: DEFAULT_PREVIEW_BYTES,
            development_projects: None,
        }
    }

    pub(crate) fn with_envelope(
        mut self,
        envelope: &crate::node_host::envelope::InvocationEnvelope,
    ) -> Self {
        self.sandbox = Arc::new(envelope.command_sandbox());
        self.with_environment(envelope.environment.clone())
            .with_network(envelope.network == NetworkPolicy::Enabled)
            .with_timeout_ceiling(envelope.timeout_ceiling)
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

    pub(crate) fn with_development_projects(mut self, library: PathBuf, scope: PathBuf) -> Self {
        self.development_projects = Some((library, scope));
        self
    }

    pub(crate) fn with_timeout_ceiling(mut self, ceiling: Duration) -> Self {
        self.timeout_ceiling = ceiling.max(self.timeout);
        self
    }

    /// Retain output beyond the previews at a host-owned, read-only location.
    pub fn with_spill(mut self, spill: SpillDirectory) -> Self {
        self.spill = Some(spill);
        self
    }

    fn authorized_commands_summary(&self) -> String {
        let mut commands = self.sandbox.allowed_commands();
        commands.extend(self.environment.pilot_command_aliases());
        commands.extend(self.environment.pilot_rust_command_aliases());
        commands.sort();
        commands.dedup();
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
             HOME is /workspace and persists with it; programs installed under {} are on PATH. \
             System directories are read-only and there is no root, so install into HOME instead. \
             Authorized commands: {}. \
             The command defaults to {}s; timeout_seconds may request up to {}s. Each stream keeps a {} KiB preview{}. \
             A non-zero exit code is a normal result: read `status`, `exit_code`, `stdout`, `stderr` and `incomplete`.",
            crate::tool_environment::WORKSPACE_HOME_BINS
                .map(|bin| bin.replacen("/workspace", "~", 1))
                .join(", "),
            self.authorized_commands_summary(),
            self.timeout.as_secs(),
            self.timeout_ceiling.as_secs(),
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

    /// Invalid arguments to anchor_read or anchor_edit, named as such and
    /// with their own correction.
    fn invalid_path_arguments(&self, tool: &str, error: impl std::fmt::Display) -> ToolError {
        ToolError::Failed(format!(
            "{tool} refused: invalid arguments: {error}. Pass `path` relative to /workspace \
             (`/workspace/<path>` names the same file); anchor_read also accepts an absolute \
             path inside an authorized read-only input. No change was made."
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

    fn hint(
        &self,
        status: &str,
        incomplete: bool,
        retained: bool,
        timeout: Duration,
    ) -> Option<String> {
        if status == "timed_out" {
            return Some(format!(
                "the command was killed after the {}s wall-clock limit and did not finish; narrow it, \
                 or write its output to a file under /workspace and read that file in parts.",
                timeout.as_secs()
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
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
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
    fn workspace_files(&self) -> Result<workspace_files::WorkspaceFiles, ToolError> {
        workspace_files::WorkspaceFiles::open(&self.workspace).map_err(|error| {
            ToolError::Failed(format!("anchor file access is unavailable: {error}"))
        })
    }

    fn relative_candidate(&self, tool: &str, path: &str) -> Result<PathBuf, ToolError> {
        // The sandbox mounts the workspace at /workspace, so a model writes
        // its files that way; the rest of the path is checked as given.
        let path = path.strip_prefix(WORKSPACE_PREFIX).unwrap_or(path);
        if path.trim().is_empty() {
            return Err(self.invalid_path_arguments(tool, "path must not be empty"));
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
            return Err(self.invalid_path_arguments(
                tool,
                format!("`{path}` must be relative to /workspace without `..`"),
            ));
        }
        Ok(candidate.to_path_buf())
    }

    fn read_description(&self) -> String {
        format!(
            "Read a text file by line. `path` is relative to /workspace (`/workspace/<path>` names the same file), or an absolute path inside an explicitly authorized read-only input mount. Other absolute paths and `..` are refused. \
             Returns at most {READ_DEFAULT_LINES} lines (ceiling {READ_MAX_LINES}) or {READ_MAX_BYTES} bytes with 1-based line numbers, plus the file's `sha256`. \
             Page with `offset`/`next_offset`, and pass `sha256` as `base_sha256` to anchor_edit."
        )
    }

    fn edit_description(&self) -> String {
        "Modify a text file inside /workspace; `path` is relative to it, and `/workspace/<path>` names the same file. Either pass `content` to replace the whole file \
         (requires `base_sha256` from anchor_read unless the file does not exist yet) or pass `old_string` \
         and `new_string` to replace exactly `expected_matches` occurrences (default 1). \
         The edit changes nothing and fails when the file hash or the match count does not match, so read the file first."
            .to_owned()
    }

    fn read_file(&self, arguments: Value) -> Result<Vec<ToolResultContent>, ToolError> {
        let arguments: ReadArguments = serde_json::from_value(arguments)
            .map_err(|error| self.invalid_path_arguments(READ_TOOL_NAME, error))?;
        let input = self
            .readonly_inputs
            .iter()
            .find(|mount| Path::new(&arguments.path).starts_with(&mount.destination));
        let bytes = if Path::new(&arguments.path).is_absolute()
            && (input.is_some() || !arguments.path.starts_with(WORKSPACE_PREFIX))
        {
            let mount = input.ok_or_else(|| {
                self.invalid_path_arguments(
                    READ_TOOL_NAME,
                    "absolute read paths must belong to an authorized read-only input",
                )
            })?;
            let relative = Path::new(&arguments.path)
                .strip_prefix(&mount.destination)
                .map_err(|_| self.invalid_path_arguments(READ_TOOL_NAME, "invalid input path"))?;
            let relative = relative
                .to_str()
                .ok_or_else(|| self.invalid_path_arguments(READ_TOOL_NAME, "invalid input path"))?;
            let mut file =
                crate::resource_read::open_resource(&mount.source, relative).map_err(|error| {
                    ToolError::Failed(format!("anchor_read input unavailable: {error}"))
                })?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(
                &mut std::io::Read::take(&mut file, crate::attachments::MAX_FILE_BYTES as u64 + 1),
                &mut bytes,
            )
            .map_err(|error| ToolError::Failed(error.to_string()))?;
            if bytes.len() > crate::attachments::MAX_FILE_BYTES {
                return Err(ToolError::Failed(
                    "input file exceeds its read limit".into(),
                ));
            }
            bytes
        } else {
            let candidate = self.relative_candidate(READ_TOOL_NAME, &arguments.path)?;
            self.workspace_files()?.read(&candidate).map_err(|error| {
                ToolError::Failed(format!("anchor_read failed: `{}`: {error}", arguments.path))
            })?
        };
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
            let line_bytes = line.len().saturating_add(1);
            if bytes_used.saturating_add(line_bytes) > READ_MAX_BYTES {
                if lines.is_empty() {
                    return Err(ToolError::Failed(format!(
                        "anchor_read refused: `{}` line {} exceeds the {READ_MAX_BYTES} byte page limit including its counted newline; use anchor_run to inspect a bounded byte range",
                        arguments.path,
                        index + 1,
                    )));
                }
                truncated_for_bytes = true;
                break;
            }
            bytes_used += line_bytes;
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
        let arguments: EditArguments = serde_json::from_value(arguments)
            .map_err(|error| self.invalid_path_arguments(EDIT_TOOL_NAME, error))?;
        let candidate = self.relative_candidate(EDIT_TOOL_NAME, &arguments.path)?;
        let read_error =
            |error| ToolError::Failed(format!("anchor_edit failed: `{}`: {error}", arguments.path));
        let target = self
            .workspace_files()?
            .edit_target(&candidate)
            .map_err(read_error)?;
        let existing = target.read_existing().map_err(read_error)?;
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
                    return Err(self.invalid_path_arguments(
                        EDIT_TOOL_NAME,
                        "expected_matches must be at least 1",
                    ));
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
                return Err(self.invalid_path_arguments(
                    EDIT_TOOL_NAME,
                    "pass either `content`, or both `old_string` and `new_string`",
                ));
            }
        };
        target.replace(&after).map_err(|error| {
            ToolError::Failed(format!(
                "anchor_edit failed to write `{}`: {error}",
                arguments.path
            ))
        })?;
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
        let mut run = json!({
            "type": "object",
            "properties": {
                "command": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "timeout_seconds": {"type":"integer", "minimum":1, "maximum":self.timeout_ceiling.as_secs()}
            },
            "required": ["command"],
            "additionalProperties": false
        });
        // Offer `project` only where it can be used: a model shown a
        // parameter tries it.
        if self.development_projects.is_some() {
            run["properties"]["project"] = json!({"type":"string","description":"Prepared workspace-relative Python project. Runs inside this directory using its private venv/pip/PATH, or its preserved read-only legacy view until explicit rebuild. Use '.' for workspace root. Same-project commands and preparation serialize; interrupted pip may partially change state, so inspect before continuing."});
        }
        definitions.push(ToolDefinition::new(
            ToolName::new(RUN_TOOL_NAME).expect("static tool name"),
            self.run_description(),
            run,
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
            let timeout =
                Duration::from_secs(arguments.timeout_seconds.unwrap_or(self.timeout.as_secs()));
            if timeout.is_zero() || timeout > self.timeout_ceiling {
                return Err(self.invalid_arguments(format!(
                    "timeout_seconds must be between 1 and {}",
                    self.timeout_ceiling.as_secs()
                )));
            }
            let development = if let Some(project) = &arguments.project {
                let (library, scope) = self.development_projects.as_ref().ok_or_else(|| {
                    self.refusal("development projects are available only in a Pilot Session")
                })?;
                if project != "." {
                    crate::pilot_tools::workspace_directory(&self.workspace, project)?;
                }
                Some(
                    crate::application::development_environment::select_environment(
                        library,
                        scope,
                        project,
                        self.environment.pilot_python_interpreter(),
                        &crate::dependency_provision::DependencyAccess {
                            network: self.network == NetworkPolicy::Enabled,
                            cancellation: self.cancellation.clone(),
                        },
                    )
                    .await
                    .map_err(|error| self.refusal(error))?,
                )
            } else {
                None
            };
            let python_alias = arguments
                .command
                .first()
                .filter(|command| {
                    command.starts_with("python")
                        && self.environment.pilot_command_aliases().contains(command)
                })
                .cloned();
            let mut request = SandboxRequest {
                writable_inputs: Vec::new(),
                workspace: self.workspace.clone(),
                working_directory: arguments
                    .project
                    .as_ref()
                    .map(|project| PathBuf::from("/workspace").join(project)),
                command: arguments.command,
                readonly_inputs: self.readonly_inputs.clone(),
                workspace_readonly: Vec::new(),
                tool_dirs: Vec::new(),
                devices: Vec::new(),
                environment: Vec::new(),
                network: self.network,
                timeout,
                max_output_bytes: self.preview_bytes,
                spill: self.spill_policy()?,
                cancellation: self.cancellation.clone(),
            };
            self.environment.apply_to_workspace_command(&mut request);
            let sandbox = self
                .environment
                .sandbox_for_command(&self.sandbox, &mut request)
                .map_err(|reason| self.refusal(reason))?;
            let mut sandbox = Arc::new(sandbox);
            if let Some(development) = &development {
                sandbox = development
                    .apply(sandbox, &mut request, python_alias.is_some())
                    .map_err(|reason| self.refusal(reason))?;
            }
            let private_environment = development.as_ref().is_some_and(|env| env.is_private());
            if let Some(development) = &development {
                development.begin_operation().map_err(ToolError::Failed)?;
            }
            let result = match sandbox.run(request).await {
                Ok(result) => result,
                // Raised by the adapter before any process launch, so the model
                // can safely correct its command.
                Err(SandboxError::InvalidRequest(reason)) => {
                    if let Some(development) = &development {
                        development
                            .finish_operation("not_executed", None)
                            .map_err(ToolError::Failed)?;
                    }
                    return Err(self.refusal(reason));
                }
                Err(error) => {
                    if let Some(development) = &development {
                        development
                            .finish_operation("failed", None)
                            .map_err(ToolError::Failed)?;
                    }
                    return Err(ToolError::Failed(error.to_string()));
                }
            };
            let environment_observation = if let Some(development) = &development {
                development
                    .finish_operation(
                        match result.status {
                            SandboxStatus::Completed => "completed",
                            SandboxStatus::Cancelled => "cancelled",
                            SandboxStatus::TimedOut => "timed_out",
                            SandboxStatus::NotExecuted => "not_executed",
                        },
                        result.exit_code,
                    )
                    .map_err(ToolError::Failed)?
            } else {
                Value::Null
            };
            drop(development);
            let status = match result.status {
                SandboxStatus::Completed => "completed",
                SandboxStatus::TimedOut => "timed_out",
                SandboxStatus::Cancelled => {
                    return Err(ToolError::Failed(if private_environment {
                        "anchor_run was cancelled; the private environment may be partially changed. Inspect pip list/check and project state before continuing; do not replay from an old receipt.".into()
                    } else {
                        "anchor_run was cancelled".into()
                    }));
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
            output["timeout_seconds"] = json!(timeout.as_secs());
            if private_environment {
                output["environment_observation"] = environment_observation;
            }
            if private_environment && (status == "timed_out" || result.exit_code != Some(0)) {
                output["environment_changes_possible"] = json!(true);
                output["environment_guidance"] = json!(
                    "Private environment may be partially changed; inspect pip list/check and project state before continuing. An old receipt does not establish current health."
                );
            }
            if let Some(hint) = self.hint(status, result.incomplete, !retained.is_empty(), timeout)
            {
                output["hint"] = json!(hint);
            }
            Ok(vec![ToolResultContent::json(output)])
        })
    }
}

#[cfg(test)]
mod tests;
