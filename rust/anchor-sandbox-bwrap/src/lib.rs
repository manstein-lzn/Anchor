//! Host-owned Bubblewrap adapter for the runtime sandbox port.
//!
//! Process creation and filesystem authority stay here, outside the runtime
//! kernel. Construction probes that Bubblewrap can create a namespace; a host
//! where that probe fails gets an unavailable adapter, never an unisolated
//! fallback.

use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anchor_runtime::{
    NetworkPolicy, ReadOnlyInput, SandboxError, SandboxPort, SandboxRequest, SandboxResult,
    SandboxStatus, SpillPolicy,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader},
    net::UnixStream as TokioUnixStream,
    process::{Child, Command},
    time::{Instant, sleep},
};

const WORKSPACE_MOUNT: &str = "/workspace";
const BWRAP_INFO_FD: i32 = 200;
const SYSTEM_DIRS: &[&str] = &["/usr", "/bin", "/lib", "/lib64", "/sbin"];
const SYSTEM_FILES: &[&str] = &[
    "/etc/resolv.conf",
    "/etc/ssl",
    "/etc/pki",
    "/etc/ca-certificates.conf",
    "/etc/hosts",
    "/etc/passwd",
    "/etc/group",
    "/etc/nsswitch.conf",
    "/etc/localtime",
];

/// Explicit host authorization for this adapter. The command allowlist is
/// mandatory and network access is opt-in at both policy and request levels.
#[derive(Debug, Clone)]
pub struct BubblewrapPolicy {
    pub binary: PathBuf,
    pub allowed_commands: HashSet<String>,
    pub allow_network: bool,
    /// Host-owned roots within which a graph request may select a workspace.
    pub workspace_roots: Vec<PathBuf>,
    pub readonly_input_roots: Vec<PathBuf>,
    /// Sandbox-namespace roots where host policy permits additional read-only mounts.
    pub readonly_destination_roots: Vec<PathBuf>,
    /// Exact host directories that may be added to the sandbox PATH.
    pub tool_dirs: Vec<PathBuf>,
    pub spill_roots: Vec<PathBuf>,
}

impl BubblewrapPolicy {
    pub fn new(
        binary: impl Into<PathBuf>,
        allowed_commands: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            binary: binary.into(),
            allowed_commands: allowed_commands.into_iter().map(Into::into).collect(),
            allow_network: false,
            workspace_roots: Vec::new(),
            readonly_input_roots: Vec::new(),
            readonly_destination_roots: Vec::new(),
            tool_dirs: Vec::new(),
            spill_roots: Vec::new(),
        }
    }

    pub fn authorize_workspace_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.workspace_roots.push(path.into());
        self
    }
    pub fn authorize_readonly_input_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.readonly_input_roots.push(path.into());
        self
    }
    pub fn authorize_readonly_destination_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.readonly_destination_roots.push(path.into());
        self
    }
    pub fn authorize_tool_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.tool_dirs.push(path.into());
        self
    }
    pub fn authorize_spill_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.spill_roots.push(path.into());
        self
    }
    pub fn allow_network(mut self) -> Self {
        self.allow_network = true;
        self
    }
}

/// Bubblewrap implementation of the host-owned [`SandboxPort`].
#[derive(Clone)]
pub struct BubblewrapSandbox {
    binary: PathBuf,
    allowed_commands: HashSet<String>,
    allow_network: bool,
    workspace_roots: Vec<PathBuf>,
    readonly_input_roots: Vec<PathBuf>,
    readonly_destination_roots: Vec<PathBuf>,
    readonly_grants: Vec<(PathBuf, PathBuf)>,
    tool_dirs: Vec<PathBuf>,
    spill_roots: Vec<PathBuf>,
}

impl std::fmt::Debug for BubblewrapSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BubblewrapSandbox")
            .field("binary", &self.binary)
            .field("allowed_commands", &self.allowed_commands)
            .field("allow_network", &self.allow_network)
            .field("workspace_roots", &self.workspace_roots)
            .field("readonly_input_roots", &self.readonly_input_roots)
            .field(
                "readonly_destination_roots",
                &self.readonly_destination_roots,
            )
            .field("readonly_grants", &self.readonly_grants)
            .field("tool_dirs", &self.tool_dirs)
            .field("spill_roots", &self.spill_roots)
            .finish()
    }
}

impl BubblewrapSandbox {
    /// Host-approved command basenames, sorted so diagnostics stay stable.
    ///
    /// The allowlist itself is host authority; exposing the names lets the tool
    /// layer tell a model which commands it may actually use.
    pub fn allowed_commands(&self) -> Vec<String> {
        let mut commands = self.allowed_commands.iter().cloned().collect::<Vec<_>>();
        commands.sort();
        commands
    }

    /// Whether a host directory is covered by an authorized spill root.
    ///
    /// Callers use this to decide whether output retention is actually available
    /// before promising it to a model; the adapter still re-checks the resolved
    /// path when a request is executed.
    pub fn authorizes_spill_root(&self, path: &Path) -> bool {
        self.spill_roots.iter().any(|root| path.starts_with(root))
    }

    /// Construct an adapter only after confirming the configured binary can
    /// make a user/mount/network namespace on this host.
    pub fn new(policy: BubblewrapPolicy) -> Result<Self, SandboxError> {
        if policy.allowed_commands.is_empty() {
            return Err(SandboxError::InvalidRequest(
                "Bubblewrap requires an explicit non-empty command allowlist".into(),
            ));
        }
        if policy
            .allowed_commands
            .iter()
            .any(|name| name.is_empty() || name.contains('/') || name.contains('\0'))
        {
            return Err(SandboxError::InvalidRequest(
                "allowed commands must be non-empty executable basenames".into(),
            ));
        }
        if policy.workspace_roots.is_empty() {
            return Err(SandboxError::InvalidRequest(
                "at least one host-authorized workspace root is required".into(),
            ));
        }
        for destination_root in &policy.readonly_destination_roots {
            normalized_destination(destination_root)?;
            if is_reserved_mount(destination_root) {
                return Err(SandboxError::InvalidRequest(
                    "host-authorized destination roots must not overlap reserved sandbox mounts"
                        .into(),
                ));
            }
        }
        let workspace_roots = canonical_roots(policy.workspace_roots, "workspace")?;
        let readonly_input_roots = canonical_roots(policy.readonly_input_roots, "read-only input")?;
        let readonly_destination_roots = policy.readonly_destination_roots;
        let tool_dirs = canonical_roots(policy.tool_dirs, "tool directory")?;
        let spill_roots = canonical_roots(policy.spill_roots, "spill")?;
        let binary = if policy.binary.components().count() == 1 {
            find_on_path(&policy.binary).ok_or_else(|| {
                SandboxError::Unavailable(format!(
                    "Bubblewrap binary not found: {}",
                    policy.binary.display()
                ))
            })?
        } else {
            fs::canonicalize(&policy.binary).map_err(|error| {
                SandboxError::Unavailable(format!("Bubblewrap binary unavailable: {error}"))
            })?
        };
        probe(&binary)?;
        Ok(Self {
            binary,
            allowed_commands: policy.allowed_commands,
            allow_network: policy.allow_network,
            workspace_roots,
            readonly_input_roots,
            readonly_destination_roots,
            readonly_grants: Vec::new(),
            tool_dirs,
            spill_roots,
        })
    }

    /// Derive a node sandbox with exact host-approved read-only mounts.
    /// Callers must resolve these grants from operator configuration, never
    /// from model arguments or editable Graph fields. The original policy,
    /// its roots, command authority, and network authority remain unchanged.
    pub fn with_readonly_grants(&self, grants: &[ReadOnlyInput]) -> Result<Self, SandboxError> {
        let mut sandbox = self.clone();
        for grant in grants {
            let source = fs::canonicalize(&grant.source).map_err(|error| {
                SandboxError::InvalidRequest(format!(
                    "host read-only grant cannot be resolved: {error}"
                ))
            })?;
            let destination = normalized_destination(&grant.destination)?;
            if is_reserved_mount(&destination) {
                return Err(SandboxError::InvalidRequest(
                    "host read-only grant overlaps a reserved sandbox mount".into(),
                ));
            }
            if let Some((existing, _)) = sandbox
                .readonly_grants
                .iter()
                .find(|(_, target)| *target == destination)
            {
                if *existing != source {
                    return Err(SandboxError::InvalidRequest(
                        "host read-only grants have conflicting destinations".into(),
                    ));
                }
                continue;
            }
            sandbox.readonly_grants.push((source, destination));
        }
        Ok(sandbox)
    }

    fn validate_authority(
        &self,
        request: &SandboxRequest,
    ) -> Result<ResolvedRequest, SandboxError> {
        request.validate()?;
        let command = &request.command[0];
        let basename = Path::new(command)
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("");
        if Path::new(command)
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(SandboxError::InvalidRequest(
                "command paths must not contain `..`".into(),
            ));
        }
        let mounted_command = Path::new(command).starts_with("/plugins/")
            || Path::new(command).starts_with("/tools/");
        let registered_command = Path::new(command).is_absolute()
            && request.tool_dirs.iter().any(|directory| {
                Path::new(command).parent() == Some(directory.as_path())
                    && directory
                        .canonicalize()
                        .is_ok_and(|path| self.tool_dirs.contains(&path))
            });
        let system_command = Path::new(command).is_absolute()
            && SYSTEM_DIRS
                .iter()
                .any(|root| Path::new(command).starts_with(root))
            && fs::canonicalize(command)
                .is_ok_and(|path| SYSTEM_DIRS.iter().any(|root| path.starts_with(root)));
        if command != basename && !mounted_command && !registered_command && !system_command {
            return Err(SandboxError::InvalidRequest(
                "command must select an allowlisted basename/system executable or a mounted Plugin/tool path".into(),
            ));
        }
        let mounted_plugin_executable = if mounted_command {
            request.readonly_inputs.iter().any(|input| {
                let destination = &input.destination;
                let Some(relative) = Path::new(command).strip_prefix(destination).ok() else {
                    return false;
                };
                let Ok(source) = fs::canonicalize(&input.source) else {
                    return false;
                };
                let candidate = if relative.as_os_str().is_empty() {
                    source
                } else {
                    source.join(relative)
                };
                candidate.is_file()
                    && std::fs::metadata(&candidate).is_ok_and(|metadata| {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            metadata.permissions().mode() & 0o111 != 0
                        }
                        #[cfg(not(unix))]
                        {
                            true
                        }
                    })
            })
        } else {
            false
        };
        if !self.allowed_commands.contains(basename)
            && !mounted_plugin_executable
            && !registered_command
        {
            return Err(SandboxError::InvalidRequest(format!(
                "command `{basename}` is not authorized by the host sandbox policy"
            )));
        }
        if request.network == NetworkPolicy::Enabled && !self.allow_network {
            return Err(SandboxError::InvalidRequest(
                "network access was requested but is not authorized by the host policy".into(),
            ));
        }

        // Canonicalize every host path before passing it to bwrap. In
        // particular, workspace_readonly symlinks must remain inside the
        // canonical workspace; lexical `..` checks alone do not prove this.
        let workspace = fs::canonicalize(&request.workspace).map_err(|error| {
            SandboxError::InvalidRequest(format!("workspace cannot be resolved: {error}"))
        })?;
        ensure_within(&workspace, &self.workspace_roots, "workspace")?;
        if !workspace.is_dir() {
            return Err(SandboxError::InvalidRequest(
                "workspace is not a directory".into(),
            ));
        }
        let mut readonly_inputs = Vec::with_capacity(request.readonly_inputs.len());
        for input in &request.readonly_inputs {
            let source = fs::canonicalize(&input.source).map_err(|error| {
                SandboxError::InvalidRequest(format!("read-only input cannot be resolved: {error}"))
            })?;
            let destination = normalized_destination(&input.destination)?;
            if !self
                .readonly_grants
                .iter()
                .any(|grant| grant == &(source.clone(), destination.clone()))
            {
                ensure_within(&source, &self.readonly_input_roots, "read-only input")?;
                self.authorize_destination(&destination)?;
            }
            readonly_inputs.push((source, destination));
        }
        let working_directory = request
            .working_directory
            .clone()
            .unwrap_or_else(|| PathBuf::from(WORKSPACE_MOUNT));
        if working_directory != Path::new(WORKSPACE_MOUNT) {
            let in_workspace = working_directory
                .strip_prefix(WORKSPACE_MOUNT)
                .ok()
                .is_some_and(|relative| workspace.join(relative).is_dir());
            let in_readonly_input = readonly_inputs.iter().any(|(source, destination)| {
                working_directory
                    .strip_prefix(destination)
                    .ok()
                    .is_some_and(|relative| source.join(relative).is_dir())
            });
            if !in_workspace && !in_readonly_input {
                return Err(SandboxError::InvalidRequest(
                    "working_directory must be inside the workspace or a mounted read-only input"
                        .into(),
                ));
            }
        }
        let mut workspace_readonly = Vec::with_capacity(request.workspace_readonly.len());
        for relative in &request.workspace_readonly {
            reject_symlink_components(&workspace, relative)?;
            let source = fs::canonicalize(workspace.join(relative)).map_err(|error| {
                SandboxError::InvalidRequest(format!(
                    "workspace read-only path cannot be resolved: {error}"
                ))
            })?;
            if !source.starts_with(&workspace) {
                return Err(SandboxError::InvalidRequest(
                    "workspace read-only path resolves outside the workspace".into(),
                ));
            }
            workspace_readonly.push((source, Path::new(WORKSPACE_MOUNT).join(relative)));
        }
        let mut tool_dirs = Vec::with_capacity(request.tool_dirs.len());
        for path in &request.tool_dirs {
            let resolved = fs::canonicalize(path).map_err(|error| {
                SandboxError::InvalidRequest(format!("tool directory cannot be resolved: {error}"))
            })?;
            if !resolved.is_dir() {
                return Err(SandboxError::InvalidRequest(
                    "tool_dirs entries must be directories".into(),
                ));
            }
            if !self.tool_dirs.contains(&resolved) {
                return Err(SandboxError::InvalidRequest(
                    "tool directory is not explicitly authorized by host policy".into(),
                ));
            }
            tool_dirs.push(resolved);
        }
        let spill = resolve_spill(request.spill.as_ref(), &self.spill_roots)?;
        if let Some(mount) = spill
            .as_ref()
            .and_then(|spill| spill.sandbox_mount.as_ref())
        {
            self.authorize_destination(mount)?;
        }
        Ok(ResolvedRequest {
            workspace,
            working_directory,
            readonly_inputs,
            workspace_readonly,
            tool_dirs,
            spill,
        })
    }

    fn authorize_destination(&self, destination: &Path) -> Result<(), SandboxError> {
        if self
            .readonly_destination_roots
            .iter()
            .any(|root| destination.starts_with(root))
            && !is_reserved_mount(destination)
        {
            return Ok(());
        }
        Err(SandboxError::InvalidRequest(
            "sandbox mount destination is outside host-authorized roots or overlaps a reserved mount".into(),
        ))
    }

    fn argv(
        &self,
        request: &SandboxRequest,
        resolved: &ResolvedRequest,
        include_info_fd: bool,
    ) -> Vec<String> {
        let mut args = vec!["--unshare-all".to_owned(), "--die-with-parent".to_owned()];
        let mut made_dirs = HashSet::new();
        if request.network == NetworkPolicy::Enabled {
            args.push("--share-net".into());
        }
        for path in SYSTEM_DIRS.iter().filter(|p| Path::new(p).exists()) {
            args.extend(["--ro-bind".into(), (*path).into(), (*path).into()]);
        }
        for path in SYSTEM_FILES.iter().filter(|p| Path::new(p).exists()) {
            args.extend(["--ro-bind-try".into(), (*path).into(), (*path).into()]);
        }
        args.extend(["--tmpfs".into(), "/tmp".into()]);
        args.extend(["--dir".into(), WORKSPACE_MOUNT.into()]);
        args.extend([
            "--bind".into(),
            path_string(&resolved.workspace),
            WORKSPACE_MOUNT.into(),
        ]);
        for (source, destination) in &resolved.workspace_readonly {
            args.extend([
                "--ro-bind".into(),
                path_string(source),
                path_string(destination),
            ]);
        }
        for (source, destination) in &resolved.readonly_inputs {
            add_mount_parent_dirs(&mut args, destination, &mut made_dirs);
            args.extend([
                "--ro-bind".into(),
                path_string(source),
                path_string(destination),
            ]);
        }
        if let Some(spill) = &resolved.spill
            && let Some(mount) = &spill.sandbox_mount
        {
            add_mount_parent_dirs(&mut args, mount, &mut made_dirs);
            if made_dirs.insert(mount.clone()) {
                args.extend(["--dir".into(), path_string(mount)]);
            }
            args.extend([
                "--ro-bind".into(),
                path_string(&spill.host_directory),
                path_string(mount),
            ]);
        }
        args.extend([
            "--chdir".into(),
            path_string(&resolved.working_directory),
            "--dev".into(),
            "/dev".into(),
        ]);
        if include_info_fd {
            args.extend(["--info-fd".into(), BWRAP_INFO_FD.to_string()]);
        }
        args.extend(["--".into()]);
        args.extend(request.command.iter().cloned());
        args
    }

    /// Build an interactive command inside the same Bubblewrap boundary used
    /// by [`SandboxPort::run`]. The caller owns the child's lifetime and may
    /// attach an MCP stdio transport to its piped stdin/stdout. Validation is
    /// identical to a normal sandbox request; this method does not provide an
    /// unisolated process fallback.
    pub fn isolated_command(&self, request: SandboxRequest) -> Result<Command, SandboxError> {
        let resolved = self.validate_authority(&request)?;
        if request.cancellation.load(Ordering::Relaxed) {
            return Err(SandboxError::InvalidRequest(
                "sandbox command was cancelled before launch".into(),
            ));
        }
        let argv = self.argv(&request, &resolved, false);
        let mut command = Command::new(&self.binary);
        command
            .args(argv)
            .env_clear()
            .env("PATH", make_path(&resolved.tool_dirs))
            .env("HOME", WORKSPACE_MOUNT)
            .env("TMPDIR", "/tmp")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for variable in &request.environment {
            command.env(&variable.key, variable.value());
        }
        command.as_std_mut().process_group(0);
        command.kill_on_drop(true);
        Ok(command)
    }
}

impl SandboxPort for BubblewrapSandbox {
    fn run<'a>(
        &'a self,
        request: SandboxRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<SandboxResult, SandboxError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let resolved = self.validate_authority(&request)?;
            if request.cancellation.load(Ordering::Relaxed) {
                return Ok(SandboxResult {
                    status: SandboxStatus::Cancelled,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    reason: "cancelled before sandbox execution".into(),
                    spilled_host_paths: Vec::new(),
                    visible_spill_paths: Vec::new(),
                    incomplete: false,
                });
            }
            let argv = self.argv(&request, &resolved, true);
            let mut command = Command::new(&self.binary);
            command
                .args(argv)
                .env_clear()
                .env("PATH", make_path(&resolved.tool_dirs))
                .env("HOME", WORKSPACE_MOUNT)
                .env("TMPDIR", "/tmp")
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // Child environment values never appear in process argv. Bubblewrap
            // inherits this explicit, cleared environment into its command.
            for variable in &request.environment {
                command.env(&variable.key, variable.value());
            }
            command.as_std_mut().process_group(0);
            let (info_parent, info_child) =
                std::os::unix::net::UnixStream::pair().map_err(|error| {
                    SandboxError::Failed(format!(
                        "could not create Bubblewrap startup channel: {error}"
                    ))
                })?;
            let info_child_fd = info_child.as_raw_fd();
            unsafe {
                command
                    .as_std_mut()
                    .pre_exec(move || inherit_startup_descriptor(info_child_fd));
            }
            command.kill_on_drop(true);
            let mut child = command.spawn().map_err(|error| {
                SandboxError::Unavailable(format!("Bubblewrap could not be started: {error}"))
            })?;
            drop(info_child);
            info_parent.set_nonblocking(true).map_err(|error| {
                SandboxError::Failed(format!("could not configure startup channel: {error}"))
            })?;
            let info = TokioUnixStream::from_std(info_parent).map_err(|error| {
                SandboxError::Failed(format!("could not configure startup channel: {error}"))
            })?;
            let mut info_reader = BufReader::new(info);
            let stdout = child.stdout.take().expect("stdout was piped");
            let stderr = child.stderr.take().expect("stderr was piped");
            let capture = Arc::new(CaptureState::new(
                request.max_output_bytes,
                spill_budget(request.spill.as_ref()),
            ));
            let out_task = tokio::spawn(read_stream(stdout, capture.clone(), "stdout"));
            let err_task = tokio::spawn(read_stream(stderr, capture.clone(), "stderr"));
            let deadline = Instant::now() + request.timeout;
            let mut info_line = String::new();
            let mut info_done = false;
            let mut sandbox_started = false;
            let status = loop {
                tokio::select! {
                    biased;
                    result = info_reader.read_line(&mut info_line), if !info_done => {
                        match result {
                            Ok(count) if count > 0 => sandbox_started = true,
                            Ok(_) => info_done = true,
                            Err(error) => {
                                out_task.abort();
                                err_task.abort();
                                terminate_and_wait(child).await?;
                                return Err(SandboxError::Failed(format!("reading Bubblewrap startup status failed: {error}")));
                            }
                        }
                    }
                    _ = sleep(Duration::from_millis(20)) => {
                        match child_exited_without_reaping(&child) {
                            Ok(true) => break SandboxStatus::Completed,
                            Ok(false) => {}
                            Err(error) => {
                                out_task.abort();
                                err_task.abort();
                                if error.raw_os_error() == Some(libc::ECHILD) {
                                    // Tokio cannot disable kill_on_drop after losing PID ownership.
                                    // Retain its few handles rather than signal a possibly reused PID.
                                    std::mem::forget(child);
                                } else {
                                    terminate_and_wait(child).await?;
                                }
                                return Err(SandboxError::Failed(format!("observing Bubblewrap exit failed: {error}")));
                            }
                        }
                        if request.cancellation.load(Ordering::Relaxed) {
                            terminate_process_group(&child)?;
                            break SandboxStatus::Cancelled;
                        }
                        if Instant::now() >= deadline {
                            terminate_process_group(&child)?;
                            break SandboxStatus::TimedOut;
                        }
                    }
                }
            };
            if status == SandboxStatus::Completed && !sandbox_started {
                terminate_process_group(&child)?;
            }
            drain_output(
                out_task,
                err_task,
                &capture,
                &request.cancellation,
                deadline,
                &child,
            )
            .await?;
            let exit_status = match child.wait().await {
                Ok(status) => status,
                Err(error) => {
                    if error.raw_os_error() == Some(libc::ECHILD) {
                        std::mem::forget(child);
                    }
                    return Err(SandboxError::Failed(format!(
                        "waiting for Bubblewrap failed: {error}"
                    )));
                }
            };
            if status == SandboxStatus::Completed && !sandbox_started {
                return Err(SandboxError::Failed(format!(
                    "Bubblewrap exited before sandbox startup was confirmed (exit code {:?}); command was not reported as executed",
                    exit_status.code()
                )));
            }
            let capture = capture.finish()?;
            let (stdout, stdout_incomplete, stdout_host, stdout_visible) =
                capture.output("stdout", &resolved.spill);
            let (stderr, stderr_incomplete, stderr_host, stderr_visible) =
                capture.output("stderr", &resolved.spill);
            let mut host = stdout_host
                .into_iter()
                .chain(stderr_host)
                .collect::<Vec<_>>();
            let mut visible = stdout_visible
                .into_iter()
                .chain(stderr_visible)
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            if resolved
                .spill
                .as_ref()
                .and_then(|s| s.sandbox_mount.as_ref())
                .is_none()
            {
                visible.clear();
            }
            Ok(SandboxResult {
                status,
                exit_code: if status == SandboxStatus::Completed {
                    exit_status.code()
                } else {
                    None
                },
                stdout,
                stderr,
                reason: match status {
                    SandboxStatus::Completed => {
                        "Bubblewrap startup was confirmed and the command exited".into()
                    }
                    SandboxStatus::TimedOut => {
                        "sandbox command timed out and was terminated".into()
                    }
                    SandboxStatus::Cancelled => {
                        "sandbox command was cancelled and terminated".into()
                    }
                    SandboxStatus::NotExecuted => unreachable!(),
                },
                spilled_host_paths: std::mem::take(&mut host),
                visible_spill_paths: visible,
                incomplete: stdout_incomplete || stderr_incomplete,
            })
        })
    }
}

struct ResolvedSpill {
    host_directory: PathBuf,
    sandbox_mount: Option<PathBuf>,
}
struct ResolvedRequest {
    workspace: PathBuf,
    working_directory: PathBuf,
    readonly_inputs: Vec<(PathBuf, PathBuf)>,
    workspace_readonly: Vec<(PathBuf, PathBuf)>,
    tool_dirs: Vec<PathBuf>,
    spill: Option<ResolvedSpill>,
}

fn resolve_spill(
    policy: Option<&SpillPolicy>,
    roots: &[PathBuf],
) -> Result<Option<ResolvedSpill>, SandboxError> {
    let Some(policy) = policy else {
        return Ok(None);
    };
    let host_directory = fs::canonicalize(&policy.host_directory).map_err(|error| {
        SandboxError::InvalidRequest(format!(
            "spill directory must be pre-created by the host: {error}"
        ))
    })?;
    ensure_within(&host_directory, roots, "spill directory")?;
    if !host_directory.is_dir() {
        return Err(SandboxError::InvalidRequest(
            "spill destination is not a directory".into(),
        ));
    }
    Ok(Some(ResolvedSpill {
        host_directory,
        sandbox_mount: policy
            .sandbox_mount
            .as_deref()
            .map(normalized_destination)
            .transpose()?,
    }))
}

fn spill_budget(policy: Option<&SpillPolicy>) -> usize {
    policy.and_then(|p| p.max_bytes).unwrap_or(0)
}

fn normalized_destination(path: &Path) -> Result<PathBuf, SandboxError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(SandboxError::InvalidRequest(
            "sandbox mount destinations must be absolute and contain no `..`".into(),
        ));
    }
    Ok(path.to_path_buf())
}

fn canonical_roots(roots: Vec<PathBuf>, label: &str) -> Result<Vec<PathBuf>, SandboxError> {
    roots
        .into_iter()
        .map(|root| {
            fs::canonicalize(&root).map_err(|error| {
                SandboxError::InvalidRequest(format!(
                    "authorized {label} root cannot be resolved: {error}"
                ))
            })
        })
        .collect()
}

fn ensure_within(path: &Path, roots: &[PathBuf], label: &str) -> Result<(), SandboxError> {
    if roots.iter().any(|root| path.starts_with(root)) {
        return Ok(());
    }
    Err(SandboxError::InvalidRequest(format!(
        "{label} path is outside host-authorized roots"
    )))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn add_mount_parent_dirs(args: &mut Vec<String>, destination: &Path, made: &mut HashSet<PathBuf>) {
    let Some(parent) = destination.parent() else {
        return;
    };
    let mut current = PathBuf::from("/");
    for component in parent.components() {
        if let Component::Normal(part) = component {
            current.push(part);
            if made.insert(current.clone()) {
                args.extend(["--dir".into(), path_string(&current)]);
            }
        }
    }
}

fn is_reserved_mount(destination: &Path) -> bool {
    [
        "/",
        "/workspace",
        "/usr",
        "/bin",
        "/lib",
        "/lib64",
        "/sbin",
        "/etc",
        "/tmp",
        "/dev",
        "/proc",
    ]
    .iter()
    .any(|reserved| {
        let reserved = Path::new(reserved);
        destination == reserved
            || (reserved != Path::new("/")
                && reserved != Path::new("/tmp")
                && (destination.starts_with(reserved) || reserved.starts_with(destination)))
    })
}

fn reject_symlink_components(base: &Path, relative: &Path) -> Result<(), SandboxError> {
    let mut current = base.to_path_buf();
    for component in relative.components() {
        if let Component::Normal(part) = component {
            current.push(part);
            let metadata = fs::symlink_metadata(&current).map_err(|error| {
                SandboxError::InvalidRequest(format!(
                    "workspace read-only path cannot be inspected: {error}"
                ))
            })?;
            if metadata.file_type().is_symlink() {
                return Err(SandboxError::InvalidRequest(
                    "workspace read-only path components must not be symlinks".into(),
                ));
            }
        }
    }
    Ok(())
}

fn make_path(tool_dirs: &[PathBuf]) -> String {
    tool_dirs
        .iter()
        .map(|p| path_string(p))
        .chain(["/usr/bin".into(), "/bin".into()])
        .collect::<Vec<_>>()
        .join(":")
}

fn find_on_path(binary: &Path) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
}

fn probe(binary: &Path) -> Result<(), SandboxError> {
    let result = std::process::Command::new(binary)
        .args(["--unshare-all", "--ro-bind", "/", "/", "--", "true"])
        .env_clear()
        .output()
        .map_err(|error| SandboxError::Unavailable(format!("Bubblewrap probe failed: {error}")))?;
    if !result.status.success() {
        let detail = String::from_utf8_lossy(&result.stderr);
        return Err(SandboxError::Unavailable(format!(
            "Bubblewrap cannot create an isolated namespace: {}",
            detail.trim()
        )));
    }
    Ok(())
}

fn inherit_startup_descriptor(source: i32) -> io::Result<()> {
    if unsafe { libc::dup2(source, BWRAP_INFO_FD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // dup2(fd, fd) preserves CLOEXEC when allocation already chose the target.
    let flags = unsafe { libc::fcntl(BWRAP_INFO_FD, libc::F_GETFD) };
    if flags < 0
        || unsafe { libc::fcntl(BWRAP_INFO_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn child_exited_without_reaping(child: &Child) -> io::Result<bool> {
    let pid = child
        .id()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::ECHILD))?;
    observe_child_exit(pid)
}

fn observe_child_exit(pid: u32) -> io::Result<bool> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // Leave the owned root's PID reserved until output cleanup finishes.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } == 0
        {
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}

fn terminate_process_group(child: &Child) -> Result<(), SandboxError> {
    let process_group = child.id().ok_or_else(|| {
        SandboxError::Failed("Bubblewrap root was reaped before process-group cleanup".into())
    })?;
    if unsafe { libc::kill(-(process_group as i32), libc::SIGKILL) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(SandboxError::Failed(format!(
                "could not terminate Bubblewrap process group: {error}"
            )));
        }
    }
    Ok(())
}

async fn terminate_and_wait(mut child: Child) -> Result<(), SandboxError> {
    if let Err(error) = child_exited_without_reaping(&child) {
        if error.raw_os_error() == Some(libc::ECHILD) {
            std::mem::forget(child);
        }
        return Err(SandboxError::Failed(format!(
            "observing Bubblewrap before termination failed: {error}"
        )));
    }
    if let Err(error) = terminate_process_group(&child) {
        if child.id().is_none() {
            return Err(error);
        }
        child.start_kill().map_err(|kill_error| {
            SandboxError::Failed(format!(
                "{error}; could not terminate Bubblewrap process: {kill_error}"
            ))
        })?;
    }
    if let Err(error) = child.wait().await {
        if error.raw_os_error() == Some(libc::ECHILD) {
            std::mem::forget(child);
        }
        return Err(SandboxError::Failed(format!(
            "could not reap Bubblewrap after termination: {error}"
        )));
    }
    Ok(())
}

async fn drain_output(
    stdout: tokio::task::JoinHandle<()>,
    stderr: tokio::task::JoinHandle<()>,
    capture: &CaptureState,
    cancellation: &AtomicBool,
    deadline: Instant,
    child: &Child,
) -> Result<(), SandboxError> {
    let stdout_abort = stdout.abort_handle();
    let stderr_abort = stderr.abort_handle();
    let drained = async { tokio::join!(stdout, stderr) };
    tokio::pin!(drained);
    loop {
        tokio::select! {
            biased;
            (stdout, stderr) = &mut drained => {
                if stdout.is_err() || stderr.is_err() {
                    capture.read_failed.store(true, Ordering::Relaxed);
                }
                return Ok(());
            }
            _ = sleep(Duration::from_millis(20)) => {
                if cancellation.load(Ordering::Relaxed) || Instant::now() >= deadline {
                    // The unreaped root reserves its PID while pipe writers are cleaned up.
                    let terminated = terminate_process_group(child);
                    capture.read_failed.store(true, Ordering::Relaxed);
                    stdout_abort.abort();
                    stderr_abort.abort();
                    let _ = drained.await;
                    return terminated;
                }
            }
        }
    }
}

struct CaptureState {
    preview_limit: usize,
    spill_remaining: Mutex<usize>,
    streams: Mutex<Vec<StreamCapture>>,
    read_failed: AtomicBool,
}
struct StreamCapture {
    name: &'static str,
    preview: Vec<u8>,
    spill: Vec<u8>,
    total: usize,
}
impl CaptureState {
    fn new(preview_limit: usize, spill_limit: usize) -> Self {
        Self {
            preview_limit,
            spill_remaining: Mutex::new(spill_limit),
            streams: Mutex::new(vec![
                StreamCapture {
                    name: "stdout",
                    preview: Vec::new(),
                    spill: Vec::new(),
                    total: 0,
                },
                StreamCapture {
                    name: "stderr",
                    preview: Vec::new(),
                    spill: Vec::new(),
                    total: 0,
                },
            ]),
            read_failed: AtomicBool::new(false),
        }
    }
    fn push(&self, name: &'static str, chunk: &[u8]) {
        let mut streams = self.streams.lock().expect("capture mutex poisoned");
        let stream = streams
            .iter_mut()
            .find(|s| s.name == name)
            .expect("known stream");
        stream.total = stream.total.saturating_add(chunk.len());
        let preview_room = self.preview_limit.saturating_sub(stream.preview.len());
        let preview_count = preview_room.min(chunk.len());
        stream.preview.extend_from_slice(&chunk[..preview_count]);
        let spill_input = &chunk[preview_count..];
        if !spill_input.is_empty() {
            let mut remaining = self.spill_remaining.lock().expect("spill mutex poisoned");
            let count = spill_input.len().min(*remaining);
            stream.spill.extend_from_slice(&spill_input[..count]);
            *remaining -= count;
        }
    }
    fn finish(self: Arc<Self>) -> Result<Captured, SandboxError> {
        let streams = std::mem::take(&mut *self.streams.lock().expect("capture mutex poisoned"));
        Ok(Captured {
            streams,
            preview_limit: self.preview_limit,
            read_failed: self.read_failed.load(Ordering::Relaxed),
        })
    }
}
struct Captured {
    streams: Vec<StreamCapture>,
    preview_limit: usize,
    read_failed: bool,
}
impl Captured {
    fn output(
        &self,
        name: &str,
        spill: &Option<ResolvedSpill>,
    ) -> (String, bool, Vec<PathBuf>, Vec<String>) {
        let stream = self
            .streams
            .iter()
            .find(|s| s.name == name)
            .expect("known stream");
        let truncated = stream.total > self.preview_limit;
        if !truncated {
            return (
                if self.read_failed {
                    format!(
                        "{}\n[output capture failed; captured output may be incomplete]",
                        String::from_utf8_lossy(&stream.preview)
                    )
                } else {
                    String::from_utf8_lossy(&stream.preview).into_owned()
                },
                self.read_failed,
                vec![],
                vec![],
            );
        }
        let mut host_paths = Vec::new();
        let mut visible_paths = Vec::new();
        let mut persisted = false;
        if !stream.spill.is_empty()
            && let Some(policy) = spill
            && let Ok(path) = persist_spill(&policy.host_directory, name, &stream.spill)
        {
            persisted = true;
            if let Some(mount) = &policy.sandbox_mount {
                let filename = path.file_name().unwrap_or_default();
                visible_paths.push(path_string(&mount.join(filename)));
            }
            host_paths.push(path);
        }
        let incomplete = self.read_failed
            || stream.total > self.preview_limit + stream.spill.len()
            || (!stream.spill.is_empty() && !persisted);
        let extra = if incomplete {
            "the remaining output was not retained in full"
        } else if persisted && !stream.spill.is_empty() {
            "the remaining output was retained by the host"
        } else {
            "the remaining output was not retained"
        };
        let output = format!(
            "{}\n[truncated:{}-bytes; {}]",
            String::from_utf8_lossy(&stream.preview),
            stream.total - self.preview_limit,
            extra
        );
        (output, incomplete, host_paths, visible_paths)
    }
}

fn persist_spill(directory: &Path, name: &str, content: &[u8]) -> io::Result<PathBuf> {
    let unique = format!(
        "{}-{}-{}.spill",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let path = directory.join(unique);
    let mut file: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    use std::io::Write;
    if let Err(error) = file.write_all(content) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

async fn read_stream<R: AsyncRead + Unpin>(
    mut reader: R,
    state: Arc<CaptureState>,
    name: &'static str,
) {
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Err(_) => {
                state.read_failed.store(true, Ordering::Relaxed);
                break;
            }
            Ok(count) => state.push(name, &buffer[..count]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_runtime::SpillPolicy;
    use std::{
        os::unix::fs::PermissionsExt,
        pin::Pin,
        sync::{Arc, atomic::AtomicBool},
        task::{Context, Poll},
    };
    use tokio::io::ReadBuf;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "anchor-bwrap-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn unchecked(binary: PathBuf) -> BubblewrapSandbox {
        let root = fs::canonicalize(binary.parent().unwrap()).unwrap();
        BubblewrapSandbox {
            binary,
            allowed_commands: ["printf".into(), "sleep".into(), "cat".into(), "true".into()].into(),
            allow_network: false,
            workspace_roots: vec![root.clone()],
            readonly_input_roots: vec![root.clone()],
            readonly_destination_roots: vec![PathBuf::from("/in"), PathBuf::from("/spill")],
            readonly_grants: Vec::new(),
            tool_dirs: Vec::new(),
            spill_roots: vec![root],
        }
    }

    fn fake_bwrap(dir: &Path) -> PathBuf {
        let script = dir.join("bwrap-fake");
        fs::write(&script, "#!/bin/bash\nprintf '{\\\"child-pid\\\":1}\\n' >&200 || echo infofd-failed >&2\nwhile [ \"$1\" != \"--\" ]; do shift; done\nshift\nexec \"$@\"\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        script
    }

    #[tokio::test]
    async fn setup_failure_before_bwrap_startup_is_an_error_not_completed() {
        let dir = temp_dir("setup-failure");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let binary = dir.join("bwrap-fails");
        fs::write(&binary, "#!/bin/sh\necho namespace-denied >&2\nexit 1\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let sandbox = unchecked(binary);
        let request = SandboxRequest::new(&workspace, ["true"]);
        assert!(
            matches!(sandbox.run(request).await, Err(SandboxError::Failed(message)) if message.contains("before sandbox startup was confirmed"))
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn regression_startup_descriptor_with_same_number_survives_exec() {
        use std::io::Read;

        let (mut parent, child) = std::os::unix::net::UnixStream::pair().unwrap();
        parent
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let source = child.as_raw_fd();
        let mut command = std::process::Command::new("/bin/bash");
        command.args(["-c", "printf startup-confirmed >&200"]);
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(source, BWRAP_INFO_FD) < 0
                    || libc::fcntl(BWRAP_INFO_FD, libc::F_SETFD, libc::FD_CLOEXEC) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                inherit_startup_descriptor(BWRAP_INFO_FD)
            });
        }
        let status = command.status().unwrap();
        drop(child);
        let mut message = String::new();
        parent.read_to_string(&mut message).unwrap();
        assert!(status.success());
        assert_eq!(message, "startup-confirmed");
    }

    fn orphaning_fake_bwrap(dir: &Path, startup_confirmed: bool, detached: bool) -> PathBuf {
        let script = dir.join("bwrap-held-pipes");
        let confirmation = if startup_confirmed {
            "printf '{\"child-pid\":1}\\n' >&200\n"
        } else {
            ""
        };
        let background = if detached {
            "setsid sleep 30"
        } else {
            "sleep 30"
        };
        fs::write(
            &script,
            format!(
                "#!/bin/bash\nprintf '%s\\n' \"$$\" > \"${{0%/*}}/root.pid\"\n{confirmation}printf parent-complete\n{background} &\nprintf '%s\\n' \"$!\" > \"${{0%/*}}/descendant.pid\"\nexit {}\n",
                if startup_confirmed { 0 } else { 1 },
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        script
    }

    fn kill_fixture_descendant(dir: &Path) {
        if let Ok(pid) = fs::read_to_string(dir.join("descendant.pid"))
            && let Ok(pid) = pid.trim().parse::<i32>()
        {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }

    async fn fixture_descendant_terminated(dir: &Path) -> bool {
        let pid = fs::read_to_string(dir.join("descendant.pid")).unwrap();
        let path = PathBuf::from(format!("/proc/{}/status", pid.trim()));
        for _ in 0..100 {
            match fs::read_to_string(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
                Ok(status)
                    if status.lines().any(|line| {
                        line.starts_with("State:") && line.split_whitespace().nth(1) == Some("Z")
                    }) =>
                {
                    return true;
                }
                _ => {}
            }
            sleep(Duration::from_millis(2)).await;
        }
        false
    }

    #[tokio::test]
    async fn regression_setup_failure_with_inherited_output_is_bounded() {
        let dir = temp_dir("startup-held-pipes");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let sandbox = unchecked(orphaning_fake_bwrap(&dir, false, false));
        let mut request = SandboxRequest::new(&workspace, ["true"]);
        request.timeout = Duration::from_millis(100);
        let result = tokio::time::timeout(Duration::from_secs(2), sandbox.run(request)).await;
        let descendant_terminated = fixture_descendant_terminated(&dir).await;
        if !descendant_terminated {
            kill_fixture_descendant(&dir);
        }
        let _ = fs::remove_dir_all(dir);
        assert!(
            descendant_terminated,
            "startup failure left a running descendant"
        );
        assert!(
            matches!(result, Ok(Err(SandboxError::Failed(message))) if message.contains("before sandbox startup was confirmed"))
        );
    }

    #[tokio::test]
    async fn regression_exited_command_output_is_bounded_by_deadline() {
        let dir = temp_dir("completed-held-pipes");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let sandbox = unchecked(orphaning_fake_bwrap(&dir, true, false));
        let mut request = SandboxRequest::new(&workspace, ["true"]);
        request.timeout = Duration::from_millis(100);
        let result = tokio::time::timeout(Duration::from_secs(2), sandbox.run(request)).await;
        let descendant_terminated = fixture_descendant_terminated(&dir).await;
        if !descendant_terminated {
            kill_fixture_descendant(&dir);
        }
        let _ = fs::remove_dir_all(dir);
        assert!(
            descendant_terminated,
            "output deadline left a running descendant"
        );
        let result = result
            .expect("exited command retained output pipes")
            .unwrap();
        assert_eq!(result.status, SandboxStatus::Completed);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.starts_with("parent-complete"));
        assert!(result.incomplete);
    }

    #[tokio::test]
    async fn regression_exited_command_output_is_bounded_by_cancellation() {
        let dir = temp_dir("cancel-held-pipes");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let sandbox = unchecked(orphaning_fake_bwrap(&dir, true, false));
        let mut request = SandboxRequest::new(&workspace, ["true"]);
        request.timeout = Duration::from_secs(30);
        let cancellation = request.cancellation.clone();
        let root_pid = dir.join("root.pid");
        let cancel_task = tokio::spawn(async move {
            for _ in 0..200 {
                if let Ok(pid) = fs::read_to_string(&root_pid)
                    && let Ok(pid) = pid.trim().parse::<i32>()
                    && observe_child_exit(pid as u32).unwrap_or(false)
                {
                    cancellation.store(true, Ordering::Relaxed);
                    return;
                }
                sleep(Duration::from_millis(5)).await;
            }
            panic!("fake foreground command did not exit");
        });
        let result = tokio::time::timeout(Duration::from_secs(2), sandbox.run(request)).await;
        let descendant_terminated = fixture_descendant_terminated(&dir).await;
        if !descendant_terminated {
            kill_fixture_descendant(&dir);
        }
        let cancelled = cancel_task.await;
        let _ = fs::remove_dir_all(dir);
        assert!(
            descendant_terminated,
            "output cancellation left a running descendant"
        );
        cancelled.unwrap();
        let result = result
            .expect("cancellation did not finish output drainage")
            .unwrap();
        assert_eq!(result.status, SandboxStatus::Completed);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.incomplete);
    }

    #[tokio::test]
    async fn regression_detached_pipe_writer_keeps_root_unreaped_until_drain_finishes() {
        let dir = temp_dir("detached-held-pipes");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let sandbox = unchecked(orphaning_fake_bwrap(&dir, true, true));
        let mut request = SandboxRequest::new(&workspace, ["true"]);
        request.timeout = Duration::from_millis(250);
        let root_pid = dir.join("root.pid");
        let observe = tokio::spawn(async move {
            for _ in 0..100 {
                if let Ok(pid) = fs::read_to_string(&root_pid)
                    && let Ok(pid) = pid.trim().parse::<u32>()
                    && observe_child_exit(pid).unwrap_or(false)
                {
                    sleep(Duration::from_millis(30)).await;
                    return (pid, observe_child_exit(pid).unwrap_or(false));
                }
                sleep(Duration::from_millis(2)).await;
            }
            panic!("foreground identity was lost before drainage");
        });
        let result = tokio::time::timeout(Duration::from_secs(2), sandbox.run(request)).await;
        let pinned = observe.await;
        let detached_stopped = fixture_descendant_terminated(&dir).await;
        if !detached_stopped {
            kill_fixture_descendant(&dir);
        }
        let _ = fs::remove_dir_all(dir);
        let (pid, remained_owned) = pinned.unwrap();
        assert!(
            remained_owned,
            "foreground root was reaped during output drainage"
        );
        assert_eq!(
            observe_child_exit(pid).unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        assert!(
            !detached_stopped,
            "cleanup must remain within the original group"
        );
        let result = result
            .expect("detached writer retained the output pipes")
            .unwrap();
        assert_eq!(result.status, SandboxStatus::Completed);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.incomplete);
    }

    #[tokio::test]
    async fn fake_adapter_captures_bounded_output_and_spills_only_allowance() {
        let dir = temp_dir("capture");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let spill = dir.join("spill");
        fs::create_dir(&spill).unwrap();
        let sandbox = unchecked(fake_bwrap(&dir));
        let mut request = SandboxRequest::new(&workspace, ["printf", "abcdefghij"]);
        request.max_output_bytes = 4;
        request.spill = Some(SpillPolicy {
            host_directory: spill.clone(),
            max_bytes: Some(3),
            sandbox_mount: Some("/spill".into()),
        });
        let result = sandbox.run(request).await.unwrap();
        assert_eq!(result.status, SandboxStatus::Completed);
        assert!(result.stdout.starts_with("abcd\n[truncated:6-bytes"));
        assert!(result.incomplete);
        assert_eq!(fs::read(&result.spilled_host_paths[0]).unwrap(), b"efg");
        assert!(format!("{result:?}").contains("HOST PATHS REDACTED"));
        assert!(!format!("{result:?}").contains(&spill.display().to_string()));
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn spill_symlink_outside_authorized_root_is_rejected_without_creating_path() {
        let dir = temp_dir("spill-symlink");
        let workspace = dir.join("ws");
        let spill_root = dir.join("authorized-spill");
        let outside = dir.join("outside-spill");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&spill_root).unwrap();
        fs::create_dir(&outside).unwrap();
        symlink_dir(&outside, &spill_root.join("escape"));

        let mut sandbox = unchecked(fake_bwrap(&dir));
        sandbox.spill_roots = vec![fs::canonicalize(&spill_root).unwrap()];
        let unauthorized = outside.join("must-not-be-created");
        let mut request = SandboxRequest::new(&workspace, ["true"]);
        request.spill = Some(SpillPolicy {
            host_directory: spill_root.join("escape/must-not-be-created"),
            max_bytes: Some(128),
            sandbox_mount: None,
        });

        assert!(matches!(
            sandbox.validate_authority(&request),
            Err(SandboxError::InvalidRequest(_))
        ));
        assert!(!unauthorized.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn fake_adapter_rejects_unauthorized_commands_network_and_symlink_escape() {
        let dir = temp_dir("policy");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let outside = dir.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink_dir(&outside, &workspace.join("escape"));
        let inside = workspace.join("inside");
        fs::create_dir(&inside).unwrap();
        symlink_dir(&inside, &workspace.join("inside-link"));
        let sandbox = unchecked(fake_bwrap(&dir));
        let mut request = SandboxRequest::new(&workspace, ["sh", "-c", "true"]);
        assert!(matches!(
            sandbox.run(request.clone()).await,
            Err(SandboxError::InvalidRequest(_))
        ));
        request.command = vec!["true".into()];
        request.network = NetworkPolicy::Enabled;
        assert!(matches!(
            sandbox.run(request.clone()).await,
            Err(SandboxError::InvalidRequest(_))
        ));
        request.network = NetworkPolicy::Disabled;
        request.workspace_readonly.push("escape".into());
        assert!(matches!(
            sandbox.run(request).await,
            Err(SandboxError::InvalidRequest(_))
        ));

        let mut request = SandboxRequest::new(&workspace, ["true"]);
        request.workspace_readonly.push("inside-link".into());
        assert!(matches!(
            sandbox.run(request).await,
            Err(SandboxError::InvalidRequest(message)) if message.contains("must not be symlinks")
        ));

        let source = dir.join("input");
        fs::write(&source, "host-authorized input").unwrap();
        let mut request = SandboxRequest::new(&workspace, ["true"]);
        request
            .readonly_inputs
            .push(anchor_runtime::ReadOnlyInput::new(
                &source,
                "/workspace/override",
            ));
        assert!(matches!(
            sandbox.run(request).await,
            Err(SandboxError::InvalidRequest(_))
        ));

        let mut request = SandboxRequest::new(&workspace, ["true"]);
        request
            .readonly_inputs
            .push(anchor_runtime::ReadOnlyInput::new(
                &source,
                "/in/nested/input",
            ));
        let resolved = sandbox
            .validate_authority(&request)
            .expect("authorized read-only input");
        let argv = sandbox.argv(&request, &resolved, true);
        let dirs = argv
            .windows(2)
            .filter(|pair| pair[0] == "--dir")
            .map(|pair| pair[1].as_str())
            .collect::<Vec<_>>();
        assert!(dirs.contains(&"/in"));
        assert!(dirs.contains(&"/in/nested"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn explicit_system_commands_still_require_allowlisted_basenames() {
        let dir = temp_dir("absolute-command");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let sandbox = unchecked(fake_bwrap(&dir));
        for command in ["/usr/bin/printf", "/bin/printf"] {
            let request = SandboxRequest::new(&workspace, [command, "permitted"]);
            assert!(sandbox.validate_authority(&request).is_ok(), "{command}");
        }
        let request = SandboxRequest::new(&workspace, ["/usr/bin/sh"]);
        assert!(matches!(
            sandbox.validate_authority(&request),
            Err(SandboxError::InvalidRequest(message)) if message.contains("not authorized")
        ));
        let ungranted = dir.join("printf");
        fs::write(&ungranted, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&ungranted, fs::Permissions::from_mode(0o755)).unwrap();
        for command in [
            ungranted.display().to_string(),
            format!("/usr/..{}", ungranted.display()),
        ] {
            let request = SandboxRequest::new(&workspace, [command]);
            assert!(matches!(
                sandbox.validate_authority(&request),
                Err(SandboxError::InvalidRequest(_))
            ));
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn environment_secrets_are_not_bubblewrap_argv() {
        let dir = temp_dir("env");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let sandbox = unchecked(fake_bwrap(&dir));
        let mut request = SandboxRequest::new(&workspace, ["true"]);
        request
            .environment
            .push(anchor_runtime::SandboxEnvironment::new(
                "API_TOKEN",
                "argv-secret-must-not-appear",
            ));
        let resolved = sandbox.validate_authority(&request).unwrap();
        let argv = sandbox.argv(&request, &resolved, true).join(" ");
        assert!(!argv.contains("argv-secret-must-not-appear"));
        assert!(!format!("{request:?}").contains("argv-secret-must-not-appear"));
        let _ = fs::remove_dir_all(dir);
    }

    struct BrokenReader;
    impl AsyncRead for BrokenReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::other("synthetic stream read failure")))
        }
    }

    #[tokio::test]
    async fn stream_read_failure_is_reported_as_incomplete_even_under_preview_limit() {
        let state = Arc::new(CaptureState::new(100, 0));
        read_stream(BrokenReader, state.clone(), "stdout").await;
        let captured = state.finish().unwrap();
        let (output, incomplete, _, _) = captured.output("stdout", &None);
        assert!(incomplete);
        assert!(output.contains("output capture failed"));
    }

    #[tokio::test]
    async fn fake_adapter_terminates_on_timeout_and_cancellation() {
        let dir = temp_dir("stop");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let sandbox = unchecked(fake_bwrap(&dir));
        let mut request = SandboxRequest::new(&workspace, ["sleep", "5"]);
        request.timeout = Duration::from_millis(50);
        assert_eq!(
            sandbox.run(request).await.unwrap().status,
            SandboxStatus::TimedOut
        );
        let cancellation = Arc::new(AtomicBool::new(false));
        let mut request = SandboxRequest::new(&workspace, ["sleep", "5"]);
        request.cancellation = cancellation.clone();
        let task = tokio::spawn(async move { sandbox.run(request).await.unwrap() });
        sleep(Duration::from_millis(40)).await;
        cancellation.store(true, Ordering::Relaxed);
        assert_eq!(task.await.unwrap().status, SandboxStatus::Cancelled);
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn real_bubblewrap_execution_is_environment_gated() {
        let dir = temp_dir("real");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let readonly_file = workspace.join("source.txt");
        fs::write(&readonly_file, "readonly-content").unwrap();
        let host_input = dir.join("input.txt");
        fs::write(&host_input, "mounted-content").unwrap();
        let policy = BubblewrapPolicy::new("bwrap", ["cat", "sh"])
            .authorize_workspace_root(&dir)
            .authorize_readonly_input_root(&dir)
            .authorize_readonly_destination_root("/in");
        let adapter = match BubblewrapSandbox::new(policy) {
            Ok(adapter) => adapter,
            Err(error) => {
                eprintln!("skipping real bwrap integration: {error}");
                let _ = fs::remove_dir_all(dir);
                return;
            }
        };
        let mut request = SandboxRequest::new(&workspace, ["cat", "/in/input.txt"]);
        request
            .readonly_inputs
            .push(anchor_runtime::ReadOnlyInput::new(
                &host_input,
                "/in/input.txt",
            ));
        let result = adapter.run(request).await.unwrap();
        assert_eq!(result.status, SandboxStatus::Completed);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout, "mounted-content");

        let mut request = SandboxRequest::new(
            &workspace,
            ["sh", "-c", "printf changed > /workspace/source.txt"],
        );
        request.workspace_readonly.push("source.txt".into());
        let result = adapter.run(request).await.unwrap();
        assert_eq!(result.status, SandboxStatus::Completed);
        assert_ne!(result.exit_code, Some(0));
        assert_eq!(
            fs::read_to_string(readonly_file).unwrap(),
            "readonly-content"
        );
        let _ = fs::remove_dir_all(dir);
    }

    fn symlink_dir(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[tokio::test]
    async fn exact_node_grants_are_readonly_and_do_not_expand_base_or_network_authority() {
        let dir = temp_dir("exact-grants");
        let workspace = dir.join("ws");
        let inputs = dir.join("private-inputs");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&inputs).unwrap();
        let source = inputs.join("selected.txt");
        fs::write(&source, "selected-content").unwrap();
        fs::write(inputs.join("other.txt"), "ungranted").unwrap();
        let base = BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&workspace),
        )
        .unwrap();
        let grant = ReadOnlyInput::new(&source, "/local-inputs/selected");
        let node = base
            .with_readonly_grants(std::slice::from_ref(&grant))
            .unwrap();
        let mut request = SandboxRequest::new(
            &workspace,
            [
                "sh",
                "-c",
                "cat /local-inputs/selected; ! printf changed > /local-inputs/selected",
            ],
        );
        request.readonly_inputs.push(grant);
        assert!(matches!(
            base.run(request.clone()).await,
            Err(SandboxError::InvalidRequest(_))
        ));
        let result = node.run(request.clone()).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout, "selected-content");
        assert_eq!(fs::read_to_string(&source).unwrap(), "selected-content");

        let mut moved = request.clone();
        moved.readonly_inputs[0].destination = "/local-inputs/other".into();
        assert!(matches!(
            node.run(moved).await,
            Err(SandboxError::InvalidRequest(_))
        ));
        let mut sibling = request.clone();
        sibling.readonly_inputs[0].source = inputs.join("other.txt");
        assert!(matches!(
            node.run(sibling).await,
            Err(SandboxError::InvalidRequest(_))
        ));

        request.command = vec!["sh".into(), "-c".into(), "touch network-denied".into()];
        request.network = NetworkPolicy::Enabled;
        assert!(
            matches!(node.run(request).await, Err(SandboxError::InvalidRequest(reason)) if reason.contains("network access"))
        );
        assert!(!workspace.join("network-denied").exists());
        let _ = fs::remove_dir_all(dir);
    }
}
