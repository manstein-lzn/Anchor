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

use anchor_runtime_rig::{
    NetworkPolicy, SandboxError, SandboxPort, SandboxRequest, SandboxResult, SandboxStatus,
    SpillPolicy,
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
pub struct BubblewrapSandbox {
    binary: PathBuf,
    allowed_commands: HashSet<String>,
    allow_network: bool,
    workspace_roots: Vec<PathBuf>,
    readonly_input_roots: Vec<PathBuf>,
    readonly_destination_roots: Vec<PathBuf>,
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
            .field("tool_dirs", &self.tool_dirs)
            .field("spill_roots", &self.spill_roots)
            .finish()
    }
}

impl BubblewrapSandbox {
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
            tool_dirs,
            spill_roots,
        })
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
        if command != basename && !Path::new(command).starts_with("/plugins/") {
            return Err(SandboxError::InvalidRequest(
                "command must select an allowlisted executable by basename or a mounted Plugin path".into(),
            ));
        }
        let mounted_plugin_executable = if Path::new(command).starts_with("/plugins/") {
            request.readonly_inputs.iter().any(|input| {
                let destination = &input.destination;
                let Some(relative) = Path::new(command).strip_prefix(destination).ok() else {
                    return false;
                };
                let Ok(source) = fs::canonicalize(&input.source) else {
                    return false;
                };
                let candidate = source.join(relative);
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
        if !self.allowed_commands.contains(basename) && !mounted_plugin_executable {
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
            ensure_within(&source, &self.readonly_input_roots, "read-only input")?;
            let destination = normalized_destination(&input.destination)?;
            self.authorize_destination(&destination)?;
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
                command.as_std_mut().pre_exec(move || {
                    if libc::dup2(info_child_fd, BWRAP_INFO_FD) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
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
                                terminate_and_wait(&mut child).await?;
                                return Err(SandboxError::Failed(format!("reading Bubblewrap startup status failed: {error}")));
                            }
                        }
                    }
                    result = child.wait() => {
                        let status = match result {
                            Ok(status) => status,
                            Err(error) => {
                                terminate_and_wait(&mut child).await?;
                                return Err(SandboxError::Failed(format!("waiting for Bubblewrap failed: {error}")));
                            }
                        };
                        if !sandbox_started {
                            let _ = tokio::join!(out_task, err_task);
                            return Err(SandboxError::Failed(format!("Bubblewrap exited before sandbox startup was confirmed (exit code {:?}); command was not reported as executed", status.code())));
                        }
                        break (SandboxStatus::Completed, status.code());
                    }
                    _ = sleep(Duration::from_millis(20)) => {
                        if request.cancellation.load(Ordering::Relaxed) {
                            terminate_and_wait(&mut child).await?;
                            break (SandboxStatus::Cancelled, None);
                        }
                        if Instant::now() >= deadline {
                            terminate_and_wait(&mut child).await?;
                            break (SandboxStatus::TimedOut, None);
                        }
                    }
                }
            };
            let (stdout_read, stderr_read) = tokio::join!(out_task, err_task);
            if stdout_read.is_err() || stderr_read.is_err() {
                capture.read_failed.store(true, Ordering::Relaxed);
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
                status: status.0,
                exit_code: status.1,
                stdout,
                stderr,
                reason: match status.0 {
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

async fn terminate_and_wait(child: &mut Child) -> Result<(), SandboxError> {
    if let Some(pid) = child.id() {
        let result = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                child.start_kill().map_err(|kill_error| {
                    SandboxError::Failed(format!("could not terminate Bubblewrap process group ({error}) or process ({kill_error})"))
                })?;
            }
        }
    }
    child.wait().await.map_err(|error| {
        SandboxError::Failed(format!(
            "could not reap Bubblewrap after termination: {error}"
        ))
    })?;
    Ok(())
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
    use anchor_runtime_rig::SpillPolicy;
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
            .push(anchor_runtime_rig::ReadOnlyInput::new(
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
            .push(anchor_runtime_rig::ReadOnlyInput::new(
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
    fn environment_secrets_are_not_bubblewrap_argv() {
        let dir = temp_dir("env");
        let workspace = dir.join("ws");
        fs::create_dir(&workspace).unwrap();
        let sandbox = unchecked(fake_bwrap(&dir));
        let mut request = SandboxRequest::new(&workspace, ["true"]);
        request
            .environment
            .push(anchor_runtime_rig::SandboxEnvironment::new(
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
            .push(anchor_runtime_rig::ReadOnlyInput::new(
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
}
