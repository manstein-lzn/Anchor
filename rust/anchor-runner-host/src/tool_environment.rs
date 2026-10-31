//! Operator-installed tool environments, using the existing Library tool.json.
//! These paths are deployment grants, never fields of an editable Graph/Plugin.

use crate::dependency_provision::PYTHON_INTERPRETER_ENV;
use anchor_runtime::{ReadOnlyInput, SandboxEnvironment, SandboxRequest};
use anchor_sandbox_bwrap::BubblewrapPolicy;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

const PILOT_RUST_BIN: &str = "/tools/anchor-pilot-rust/bin";
const PILOT_CARGO_HOME: &str = "/workspace/.anchor-rust/cargo";
const PILOT_PYTHON_BIN: &str = "/tools/anchor-pilot-python/bin";
const PYTHON_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PYTHON_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default)]
pub(crate) struct ToolEnvironment {
    mounts: Vec<ReadOnlyInput>,
    dirs: Vec<PathBuf>,
    imports: Vec<PathBuf>,
    /// Operator-registered GPU/accelerator device nodes a tool may reach.
    devices: Vec<PathBuf>,
    /// Operator grants selected only when this Plugin launches an MCP server.
    plugin_environments: BTreeMap<String, ToolEnvironment>,
    rust: Option<RustDevelopment>,
    python: Option<PythonDevelopment>,
    // Formal Plugins may use the shared interpreter independently of Pilot's
    // authoring switch. This capability contains only read-only Python mounts.
    mcp_python: Option<Arc<ToolEnvironment>>,
    // Keep Host-generated read-only launchers alive across environment clones.
    _launchers: Vec<Arc<tempfile::TempDir>>,
}

fn shell_path(path: &Path) -> Result<String, String> {
    let path = path.to_str().ok_or("tool entrypoint must be UTF-8")?;
    shlex::try_quote(path)
        .map(|quoted| quoted.into_owned())
        .map_err(|error| error.to_string())
}

fn write_launcher(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|error| error.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o555)).map_err(|error| error.to_string())
}

/// Startup configuration remains synchronous. Nonblocking pipes bound both
/// the subprocess wait and output collection without leaving reader threads
/// behind when a misconfigured executable hangs or inherits an open pipe.
fn probe_python(interpreter: &Path, timeout: Duration) -> Result<std::process::Output, String> {
    run_python_probe(
        interpreter,
        timeout,
        "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')",
    )
}

/// A display observation only: retain the short ABI version used by execution.
/// The HTTP caller must run this bounded synchronous probe off the async worker.
pub(crate) fn probe_python_version(interpreter: &Path) -> Result<String, String> {
    let output = run_python_probe(
        interpreter,
        PYTHON_VERSION_PROBE_TIMEOUT,
        "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}')",
    )?;
    if !output.status.success() {
        return Err("Python version probe failed".into());
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|_| "Python version probe returned invalid UTF-8")?
        .trim()
        .to_owned();
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || part.parse::<u32>().is_err()
        })
    {
        return Err(
            "Python version probe did not report a numeric major.minor.patch version".into(),
        );
    }
    Ok(version)
}

fn run_python_probe(
    interpreter: &Path,
    timeout: Duration,
    script: &str,
) -> Result<std::process::Output, String> {
    let mut child = std::process::Command::new(interpreter)
        .args(["-I", "-S", "-c", script])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("{PYTHON_INTERPRETER_ENV} cannot be probed: {error}"))?;
    let group = rustix::process::Pid::from_raw(child.id() as i32);
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    let output = (|| {
        for fd in [
            std::os::fd::AsFd::as_fd(&stdout),
            std::os::fd::AsFd::as_fd(&stderr),
        ] {
            rustix::fs::fcntl_setfl(fd, rustix::fs::OFlags::NONBLOCK)
                .map_err(|error| format!("{PYTHON_INTERPRETER_ENV} cannot be probed: {error}"))?;
        }
        let started = Instant::now();
        let mut out = Vec::new();
        let mut err = Vec::new();
        loop {
            stdout_closed |= read_probe_output(&mut stdout, &mut out)?;
            stderr_closed |= read_probe_output(&mut stderr, &mut err)?;
            if let Some(status) = child.try_wait().map_err(|error| {
                format!("{PYTHON_INTERPRETER_ENV} probe could not exit: {error}")
            })? {
                stdout_closed |= read_probe_output(&mut stdout, &mut out)?;
                stderr_closed |= read_probe_output(&mut stderr, &mut err)?;
                return Ok(std::process::Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            if started.elapsed() >= timeout {
                return Err(format!("{PYTHON_INTERPRETER_ENV} probe timed out"));
            }
            std::thread::sleep(
                Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed())),
            );
        }
    })();
    if output.is_err() || !stdout_closed || !stderr_closed {
        if let Some(group) = group {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    output
}

fn read_probe_output(reader: &mut impl Read, output: &mut Vec<u8>) -> Result<bool, String> {
    const LIMIT: usize = 4096;
    let mut buffer = [0u8; 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                if output.len() + count > LIMIT {
                    return Err(format!(
                        "{PYTHON_INTERPRETER_ENV} probe output exceeded {LIMIT} bytes"
                    ));
                }
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(format!(
                    "{PYTHON_INTERPRETER_ENV} probe output failed: {error}"
                ));
            }
        }
    }
}

#[derive(Clone, Debug)]
struct RustDevelopment {
    proxy: PathBuf,
    home: PathBuf,
    toolchain: Option<String>,
}

#[derive(Clone, Debug)]
struct PythonDevelopment {
    interpreter: PathBuf,
    version: String,
}

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
struct ToolSpec {
    /// Omit for the existing deployment-wide tool environment.
    plugin: Option<String>,
    entrypoint: PathBuf,
    environment: Option<PathBuf>,
    #[serde(default)]
    imports: Vec<PathBuf>,
    /// Exact host device nodes this tool may see at the same absolute path.
    #[serde(default)]
    devices: Vec<PathBuf>,
}

/// An operator registers a device node by absolute path. It must be a real
/// character/block device below `/dev`; nothing broad or indirect is accepted,
/// and no request or Graph field can add one.
pub(crate) fn validate_device_grant(device: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::FileTypeExt;

    if !device.is_absolute()
        || device
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        || device == Path::new("/dev")
        || !device.starts_with("/dev/")
    {
        return Err(format!(
            "tool device grant must be an absolute path below /dev: {}",
            device.display()
        ));
    }
    let resolved = device
        .canonicalize()
        .map_err(|e| format!("tool device grant: {e}"))?;
    let metadata = fs::metadata(&resolved).map_err(|e| format!("tool device grant: {e}"))?;
    if !metadata.file_type().is_char_device() && !metadata.file_type().is_block_device() {
        return Err(format!(
            "tool device grant must name a device node: {}",
            resolved.display()
        ));
    }
    Ok(resolved)
}

pub(crate) fn validate_installation_prefix(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || !path.is_dir()
        || ["/", "/root", "/home", "/tmp"]
            .iter()
            .any(|root| path == Path::new(root))
        || std::env::var_os("HOME").is_some_and(|home| path == Path::new(&home))
        || [
            "/workspace",
            "/in",
            "/plugins",
            "/tools",
            "/proc",
            "/dev",
            "/etc",
        ]
        .iter()
        .any(|root| path.starts_with(root))
    {
        return Err("installation prefix overlaps a broad or reserved host path".into());
    }
    Ok(())
}

impl ToolEnvironment {
    /// Explicit deployment grants for Pilot development. Never inferred from
    /// HOME, a project rust-toolchain file, or a Plugin declaration.
    pub(crate) fn with_pilot_rust(mut self) -> Result<Self, String> {
        let Some(proxy) = std::env::var_os("ANCHOR_PILOT_RUSTUP") else {
            return Ok(self);
        };
        let home = std::env::var_os("ANCHOR_PILOT_RUSTUP_HOME")
            .ok_or("ANCHOR_PILOT_RUSTUP_HOME is required")?;
        let cargo = std::env::var_os("ANCHOR_PILOT_CARGO_HOME")
            .ok_or("ANCHOR_PILOT_CARGO_HOME is required")?;
        self.configure_pilot_rust(Path::new(&proxy), Path::new(&home), Path::new(&cargo))?;
        Ok(self)
    }

    pub(crate) fn configure_pilot_rust(
        &mut self,
        proxy: &Path,
        home: &Path,
        cargo: &Path,
    ) -> Result<(), String> {
        let proxy = proxy.canonicalize().map_err(|error| error.to_string())?;
        let home = home.canonicalize().map_err(|error| error.to_string())?;
        let cargo = cargo.canonicalize().map_err(|error| error.to_string())?;
        validate_installation_prefix(&home)?;
        validate_installation_prefix(&cargo)?;
        let bin = proxy.parent().ok_or("rustup proxy has no bin directory")?;
        if proxy.file_name().is_none_or(|name| name != "rustup")
            || !bin.join("cargo").exists()
            || !bin.join("rustc").exists()
        {
            return Err(
                "ANCHOR_PILOT_RUSTUP must name a shared Rustup proxy with Cargo and rustc".into(),
            );
        }
        validate_installation_prefix(bin)?;
        for name in ["rustup", "rustc", "rustdoc"] {
            let entry = bin.join(name);
            if entry.exists() {
                self.mounts.push(ReadOnlyInput::new(
                    entry.canonicalize().map_err(|error| error.to_string())?,
                    entry,
                ));
            }
        }
        self.mount_at_original_path(&home)?;
        self.dirs.push(bin.to_path_buf());
        for name in ["registry", "git"] {
            let source = cargo.join(name);
            if source.is_dir() {
                self.mounts.push(ReadOnlyInput::new(
                    source,
                    format!("/anchor-rust/cache/{name}"),
                ));
            }
        }
        // Seed only when Cargo is used, and only inside the existing sandbox.
        // cp -RP creates private regular files rather than shared hard links,
        // and does not follow symlinks in the operator cache. A destination a
        // Session changed can reach only that invocation's existing grants.
        let seed = r#"seed_cargo_cache() {
    mkdir -p "$CARGO_HOME"
    (
        /usr/bin/flock -x 9
        if [ ! -f "$CARGO_HOME/.anchor-cache-seeded" ]; then
            staging="$(mktemp -d "$CARGO_HOME/.anchor-cache-seed.XXXXXX")"
            trap 'rm -rf "$staging"' 0
            trap 'exit 1' HUP INT TERM
            for cache in registry git; do
                if [ -d "/anchor-rust/cache/$cache" ] && [ ! -e "$CARGO_HOME/$cache" ]; then
                    cp -RP "/anchor-rust/cache/$cache" "$staging/$cache"
                    mv "$staging/$cache" "$CARGO_HOME/$cache"
                fi
            done
            : > "$CARGO_HOME/.anchor-cache-seeded"
        fi
    ) 9>"$CARGO_HOME/.anchor-cache-seed.lock"
}
"#;
        let launchers = tempfile::Builder::new()
            .prefix("anchor-pilot-rust-")
            .tempdir()
            .map_err(|error| error.to_string())?;
        for (name, initialization) in [
            ("cargo", "seed_cargo_cache\n"),
            ("rustc", ""),
            (
                "rustup",
                "if [ \"${1-}\" = run ]; then seed_cargo_cache; fi\n",
            ),
        ] {
            let entry = if name == "cargo" {
                "/tools/anchor-pilot-rust/proxy/cargo".to_owned()
            } else {
                shell_path(&bin.join(name))?
            };
            write_launcher(
                &launchers.path().join(name),
                &format!("#!/bin/sh\nset -eu\n{seed}{initialization}exec {entry} \"$@\"\n"),
            )?;
        }
        self.mounts
            .push(ReadOnlyInput::new(launchers.path(), PILOT_RUST_BIN));
        // An absolute Cargo path in a build script must use the same private
        // cache preparation as a bare command. The underlying proxy is still
        // the single frozen Rustup executable, mounted at a second read-only
        // path whose Cargo basename selects the official toolchain.
        self.mounts.push(ReadOnlyInput::new(
            bin.join("cargo")
                .canonicalize()
                .map_err(|error| error.to_string())?,
            "/tools/anchor-pilot-rust/proxy/cargo",
        ));
        self.mounts.push(ReadOnlyInput::new(
            launchers.path().join("cargo"),
            bin.join("cargo"),
        ));
        self._launchers.push(Arc::new(launchers));
        self.rust = Some(RustDevelopment {
            proxy,
            home,
            toolchain: None,
        });
        Ok(())
    }

    /// Explicit deployment grant for the Python interpreter Pilot may use while
    /// authoring a Plugin. The interpreter path is shared with dependency
    /// preparation; this separate switch grants Pilot permission to use it.
    /// The interpreter prefix and exact command aliases are mounted read-only.
    /// Plugin dependency views remain the owner of third-party
    /// packages; this grant never gives Pilot a writable host Python or pip.
    pub(crate) fn with_pilot_python(mut self) -> Result<Self, String> {
        if std::env::var_os("ANCHOR_PILOT_PYTHON").is_some_and(|value| !value.is_empty()) {
            return Err(format!(
                "ANCHOR_PILOT_PYTHON is no longer supported; configure {PYTHON_INTERPRETER_ENV} and set ANCHOR_PILOT_PYTHON_ENABLED=1"
            ));
        }
        match std::env::var_os("ANCHOR_PILOT_PYTHON_ENABLED") {
            None => return Ok(self),
            Some(value) if value == "0" => return Ok(self),
            Some(value) if value == "1" => {}
            Some(_) => {
                return Err(
                    "ANCHOR_PILOT_PYTHON_ENABLED must be 0 or 1 (set 1 to grant Pilot Python)"
                        .into(),
                );
            }
        }
        let path = std::env::var_os(PYTHON_INTERPRETER_ENV).ok_or_else(|| {
            format!("{PYTHON_INTERPRETER_ENV} is required when ANCHOR_PILOT_PYTHON_ENABLED=1")
        })?;
        self.configure_pilot_python(Path::new(&path))?;
        Ok(self)
    }

    #[cfg(test)]
    pub(crate) fn with_pilot_python_path(mut self, configured: &Path) -> Result<Self, String> {
        self.configure_pilot_python(configured)?;
        Ok(self)
    }

    pub(crate) fn configure_pilot_python(&mut self, configured: &Path) -> Result<(), String> {
        if !configured.is_absolute() {
            return Err(format!("{PYTHON_INTERPRETER_ENV} must be an absolute path"));
        }
        let interpreter = configured
            .canonicalize()
            .map_err(|error| format!("{PYTHON_INTERPRETER_ENV} is unavailable: {error}"))?;
        let metadata = fs::metadata(&interpreter)
            .map_err(|error| format!("{PYTHON_INTERPRETER_ENV} is unavailable: {error}"))?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "{PYTHON_INTERPRETER_ENV} must be an executable regular file"
            ));
        }
        let bin = interpreter
            .parent()
            .filter(|path| path.file_name().is_some_and(|name| name == "bin"))
            .ok_or_else(|| {
                format!("{PYTHON_INTERPRETER_ENV} must be installed below a bin directory")
            })?
            .to_path_buf();
        let prefix = bin
            .parent()
            .ok_or_else(|| format!("{PYTHON_INTERPRETER_ENV} has no installation prefix"))?
            .to_path_buf();
        validate_installation_prefix(&prefix)?;
        let output = probe_python(&interpreter, PYTHON_PROBE_TIMEOUT)?;
        if !output.status.success() {
            return Err(format!(
                "{PYTHON_INTERPRETER_ENV} probe failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let mut parts = version.split('.');
        let major = parts.next().and_then(|part| part.parse::<u32>().ok());
        let minor = parts.next().and_then(|part| part.parse::<u32>().ok());
        if major != Some(3) || minor.is_none_or(|minor| minor < 8) || parts.next().is_some() {
            return Err(format!(
                "{PYTHON_INTERPRETER_ENV} must be Python 3.8 or newer (found {version})"
            ));
        }
        self.mount_at_original_path(&prefix)?;
        self.python = Some(PythonDevelopment {
            interpreter,
            version,
        });
        // PATH must select the same interpreter even for a /usr installation,
        // without authorizing every executable in its bin directory. These
        // aliases are Host-owned scripts, never files in a writable Session.
        let launchers = tempfile::Builder::new()
            .prefix("anchor-pilot-python-")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let entry = shell_path(&self.python.as_ref().expect("configured Python").interpreter)?;
        for alias in self.pilot_command_aliases() {
            write_launcher(
                &launchers.path().join(alias),
                &format!("#!/bin/sh\nexec {entry} \"$@\"\n"),
            )?;
        }
        self.mounts
            .push(ReadOnlyInput::new(launchers.path(), PILOT_PYTHON_BIN));
        self._launchers.push(Arc::new(launchers));
        Ok(())
    }

    fn normalize_pilot_python_command(&self, request: &mut SandboxRequest) -> bool {
        let Some(python) = &self.python else {
            return false;
        };
        let Some(command) = request.command.first() else {
            return false;
        };
        let basename = Path::new(command)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let is_python_alias = command == basename
            && self
                .pilot_command_aliases()
                .iter()
                .any(|alias| alias == basename);
        let is_configured_interpreter = Path::new(command) == python.interpreter
            || Path::new(command)
                .canonicalize()
                .is_ok_and(|path| path == python.interpreter);
        if is_python_alias || is_configured_interpreter {
            request.command[0] = format!("{PILOT_PYTHON_BIN}/python3");
            return true;
        }
        false
    }

    /// A formal MCP server without a dependency view can use the interpreter
    /// the Host has authorized for Plugins. Never grant the broader authoring
    /// command path (including shared Rustup writes) here.
    pub(crate) fn grant_mcp_python(
        &self,
        sandbox: &anchor_sandbox_bwrap::BubblewrapSandbox,
        request: &mut SandboxRequest,
    ) -> Result<anchor_sandbox_bwrap::BubblewrapSandbox, String> {
        let Some(python) = self.mcp_python.as_deref() else {
            return Ok(sandbox.clone());
        };
        if !python.normalize_pilot_python_command(request) {
            return Ok(sandbox.clone());
        }
        if self.python.is_none() {
            python.apply(request);
        }
        python.grant_sandbox(sandbox)
    }

    fn normalize_pilot_rust_command(&self, request: &mut SandboxRequest) {
        let Some(rust) = &self.rust else {
            return;
        };
        let Some(command) = request.command.first() else {
            return;
        };
        let bin = rust.proxy.parent().expect("validated Rustup proxy");
        for name in ["cargo", "rustup", "rustc"] {
            if command == name || Path::new(command) == bin.join(name) {
                request.command[0] = format!("{PILOT_RUST_BIN}/{name}");
                return;
            }
        }
    }

    pub(crate) fn pilot_python_status(&self) -> serde_json::Value {
        match &self.python {
            None => serde_json::json!({
                "configured": false,
                "reason": "this deployment has not granted a Pilot Python interpreter"
            }),
            Some(python) => serde_json::json!({
                "configured": true,
                "version": python.version,
                "pip_write": false,
                "pip_write_scope": "shared_interpreter",
                "guidance": "The shared interpreter is read-only. With Python authoring and dependency preparation granted, call development_prepare for the project, then use native pip through anchor_run(project=...) for trial packages. Declare delivery dependencies in plugin.json dependencies.python and use plugin_prepare/plugin_test."
            }),
        }
    }

    pub(crate) fn pilot_command_aliases(&self) -> Vec<String> {
        let Some(python) = &self.python else {
            return Vec::new();
        };
        let mut aliases = vec!["python".to_owned(), "python3".to_owned()];
        if let Some(("3", minor)) = python.version.split_once('.') {
            let versioned = format!("python3.{minor}");
            if !aliases.contains(&versioned) {
                aliases.push(versioned);
            }
        }
        aliases
    }

    pub(crate) fn pilot_python_interpreter(&self) -> Option<&Path> {
        self.python
            .as_ref()
            .map(|python| python.interpreter.as_path())
    }

    pub(crate) fn pilot_rust_command_aliases(&self) -> Vec<String> {
        if self.rust.is_some() {
            ["cargo", "rustc", "rustup"].map(str::to_owned).into()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn sandbox_for_command(
        &self,
        sandbox: &anchor_sandbox_bwrap::BubblewrapSandbox,
        request: &mut SandboxRequest,
    ) -> Result<anchor_sandbox_bwrap::BubblewrapSandbox, String> {
        self.normalize_pilot_python_command(request);
        let mut scoped = sandbox
            .with_readonly_grants(&self.mounts)
            .map_err(|error| error.to_string())?
            .with_executable_dirs(&self.dirs)
            .map_err(|error| error.to_string())?;
        if let Some(rust) = &self.rust {
            // This is the deployment-authorized Pilot Rust context, including
            // shell build scripts that invoke Cargo. It only exposes the
            // command's private PID namespace; toolchain writes remain a
            // separate grant for the exact direct Rustup entrypoint below.
            scoped = scoped.with_pid_namespace_proc();
            if request.command.first().is_some_and(|command| {
                Path::new(command)
                    .file_name()
                    .is_some_and(|name| name == "rustup")
            }) && request.command.get(1).is_some_and(|arg| arg == "toolchain")
                && request.command.get(2).is_some_and(|arg| arg == "install")
            {
                let command = &request.command[0];
                if command != "rustup" && Path::new(command) != rust.proxy {
                    return Err(
                        "toolchain installation must use the deployment's shared Rustup proxy"
                            .into(),
                    );
                }
                // Bind a bare name to the exact frozen entrypoint. PATH may
                // also contain an operator tool with the same basename.
                request.command[0] = rust.proxy.to_string_lossy().into_owned();
                if request.network != anchor_runtime::NetworkPolicy::Enabled {
                    return Err("rustup toolchain install requires this Turn's task network authorization; use an installed toolchain while offline".into());
                }
                // Cargo/build.rs only sees read-only toolchains. The direct
                // deployment Rustup command alone receives update authority.
                request
                    .readonly_inputs
                    .retain(|mount| mount.source != rust.home);
                let grants = vec![anchor_runtime::WritableInput::new(&rust.home, &rust.home)];
                request.writable_inputs.extend(grants.clone());
                // Rebuild the invocation grants without the conflicting RO root.
                let mounts: Vec<_> = self
                    .mounts
                    .iter()
                    .filter(|mount| mount.source != rust.home)
                    .cloned()
                    .collect();
                scoped = sandbox
                    .with_readonly_grants(&mounts)
                    .map_err(|error| error.to_string())?
                    .with_executable_dirs(&self.dirs)
                    .map_err(|error| error.to_string())?
                    .with_writable_grants(&grants)
                    .map_err(|error| error.to_string())?
                    .with_pid_namespace_proc();
                if !request.command.iter().any(|arg| arg == "--no-self-update") {
                    request.command.push("--no-self-update".into());
                }
            } else {
                self.normalize_pilot_rust_command(request);
            }
        }
        Ok(scoped)
    }

    pub(crate) fn pilot_rust_status(&self, network: bool) -> serde_json::Value {
        match &self.rust {
            None => serde_json::json!({
                "configured":false,
                "rustup_available":false,
                "cargo_available":false,
                "rustc_available":false,
                "installed_toolchains":[],
                "toolchain_install_network_allowed":false,
                "cargo_offline":false,
                "build_verified":false,
                "reason":"this deployment has not granted a shared Pilot Rust toolchain"
            }),
            Some(rust) => {
                let executable = |name: &str| {
                    rust.proxy
                        .parent()
                        .map(|bin| fs::metadata(bin.join(name)))
                        .is_some_and(|metadata| {
                            metadata.is_ok_and(|metadata| {
                                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                            })
                        })
                };
                let toolchains = fs::read_dir(rust.home.join("toolchains"))
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.path().is_dir())
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect::<BTreeSet<_>>();
                let rustup_available = executable("rustup");
                serde_json::json!({"configured":rustup_available,
                    "rustup_available":rustup_available,
                    "cargo_available":executable("cargo"),
                    "rustc_available":executable("rustc"),
                    "installed_toolchains":toolchains,
                    "toolchain_install_network_allowed":rustup_available && network,
                    "cargo_offline":rustup_available && !network,"build_verified":false,
                    "guidance":"Use anchor_run to check cargo/rustc and build the project's Cargo.toml/Cargo.lock; configured files do not prove a build succeeded."})
            }
        }
    }

    /// A model-facing, read-only summary of the development capabilities this
    /// Turn can actually use. Deployment paths and package index URLs stay on
    /// the Host side; this reports only their configured/available state.
    pub(crate) fn pilot_platform_status(
        &self,
        network: bool,
        dependency: serde_json::Value,
    ) -> serde_json::Value {
        let python = self.pilot_python_status();
        let rust = self.pilot_rust_status(network);
        let authoring_granted = python["configured"].as_bool().unwrap_or(false);
        let preparation_configured = dependency["configured"].as_bool().unwrap_or(false);
        let shared_interpreter_configured = dependency["shared_interpreter_configured"]
            .as_bool()
            .unwrap_or(authoring_granted);
        let python_version = python
            .get("version")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let dependency_python_available = dependency["shared_interpreter_available"]
            .as_bool()
            .unwrap_or(authoring_granted);
        let dependency_sources = dependency
            .get("sources")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]));

        let python_preparation = if preparation_configured {
            serde_json::json!({
                "configured": true,
                "shared_interpreter_available": dependency_python_available,
                "uv_available": dependency["uv_available"],
                "sources": dependency_sources,
                "network_fetch_allowed_this_turn": network,
            })
        } else {
            serde_json::json!({
                "configured": false,
                "shared_interpreter_available": false,
                "uv_available": false,
                "sources": dependency_sources,
                "network_fetch_allowed_this_turn": false,
                "reason": "this deployment has not configured Python dependency preparation",
            })
        };

        let rust_configured = rust["configured"].as_bool().unwrap_or(false);
        serde_json::json!({
            "python": {
                "shared_interpreter_configured": shared_interpreter_configured,
                "shared_interpreter_available": dependency_python_available,
                "authoring_granted": authoring_granted,
                "version": python_version,
                "dependency_preparation": python_preparation,
                "pip_write": false,
                "pip_write_scope": "shared_interpreter",
                "private_project_pip": {
                    "available": authoring_granted && preparation_configured && dependency_python_available && dependency["uv_available"].as_bool().unwrap_or(false),
                    "requires_project_preparation": true,
                },
                "guidance": "The shared interpreter is read-only. When private_project_pip.available is true, call development_prepare for the project, then use native pip through anchor_run(project=...) for trial packages. Declare delivery dependencies in plugin.json dependencies.python and use plugin_prepare/plugin_test; capability availability does not prove a project environment is prepared.",
            },
            "rust": {
                "configured": rust_configured,
                "rustup_available": rust["rustup_available"],
                "cargo_available": rust["cargo_available"],
                "rustc_available": rust["rustc_available"],
                "installed_toolchains": rust.get("installed_toolchains").cloned().unwrap_or_else(|| serde_json::json!([])),
                "toolchain_install_network_allowed_this_turn": rust["toolchain_install_network_allowed"],
                "cargo_offline": rust["cargo_offline"],
                "build_verified": false,
                "guidance": "Use anchor_run to inspect and build the project's Cargo.toml/Cargo.lock; configured files do not prove a build succeeded.",
            },
            "network": {
                "allowed_this_turn": network,
                "source": "frozen Session network authorization and mounted network Plugin",
            },
            "mutations_performed": false,
        })
    }

    pub(crate) fn with_runtime_snapshot(
        mut self,
        snapshot: &crate::runtime_configuration::RuntimeSnapshot,
    ) -> Result<Self, String> {
        if snapshot.python_enabled && snapshot.python.is_none() {
            return Err("Pilot Python is enabled without an interpreter".into());
        }
        if let Some(python) = &snapshot.python {
            let mut grant = Self::default();
            grant.configure_pilot_python(python)?;
            if snapshot.python_enabled {
                self.mounts.extend(grant.mounts.clone());
                self.python = grant.python.clone();
                self._launchers.extend(grant._launchers.clone());
            }
            self.mcp_python = Some(Arc::new(grant));
        }
        if let Some(rust) = &snapshot.rust {
            self.configure_pilot_rust(&rust.proxy, &rust.home, &rust.cargo)?;
            if let Some(development) = self.rust.as_mut() {
                development.toolchain = rust.toolchain.clone();
            }
        }
        Ok(self)
    }

    pub(crate) fn for_library(root: Option<&Path>) -> Result<Self, String> {
        root.map(Self::load)
            .unwrap_or_else(|| Ok(Default::default()))
    }

    pub fn load(root: &Path) -> Result<Self, String> {
        let mut result = Self::default();
        let directory = root
            .canonicalize()
            .map_err(|e| format!("tool library: {e}"))?
            .join("tools");
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
            Err(error) => return Err(format!("tool library: {error}")),
        };
        let mut entries = entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort();
        for directory in entries {
            if !directory.is_dir() {
                continue;
            }
            let id = directory
                .file_name()
                .and_then(|id| id.to_str())
                .ok_or("invalid tool id")?;
            if !anchor_runtime::identifier::is_plugin_id(id) {
                return Err(format!("invalid tool id: {id}"));
            }
            let spec: ToolSpec = serde_json::from_slice(
                &fs::read(directory.join("tool.json")).map_err(|e| format!("tool {id}: {e}"))?,
            )
            .map_err(|e| format!("tool {id}: {e}"))?;
            let target = match &spec.plugin {
                Some(plugin) => {
                    if !anchor_runtime::identifier::is_plugin_id(plugin) {
                        return Err(format!("tool {id}: invalid Plugin scope"));
                    }
                    result
                        .plugin_environments
                        .entry(plugin.clone())
                        .or_default()
                }
                None => &mut result,
            };
            let entry = if spec.entrypoint.is_absolute() {
                spec.entrypoint
            } else {
                let entry = directory
                    .join(spec.entrypoint)
                    .canonicalize()
                    .map_err(|e| e.to_string())?;
                if !entry.starts_with(directory.canonicalize().map_err(|e| e.to_string())?) {
                    return Err(format!(
                        "tool {id}: relative entrypoint escapes tool directory"
                    ));
                }
                entry
            };
            if !entry.is_file()
                || fs::metadata(&entry)
                    .map_err(|e| e.to_string())?
                    .permissions()
                    .mode()
                    & 0o111
                    == 0
            {
                return Err(format!("tool {id}: entrypoint is not executable"));
            }
            target
                .mounts
                .push(ReadOnlyInput::new(&directory, format!("/tools/{id}/files")));
            target
                .mounts
                .push(ReadOnlyInput::new(&entry, format!("/tools/{id}/run")));
            if let Some(environment) = spec.environment {
                if !environment.is_absolute()
                    || !environment.is_dir()
                    || !entry.starts_with(&environment)
                {
                    return Err(format!(
                        "tool {id}: entrypoint must belong to its absolute environment directory"
                    ));
                }
                target.mount_at_original_path(&environment)?;
                let bin = environment.join("bin");
                if bin.is_dir() {
                    let canonical_bin = bin.canonicalize().map_err(|e| e.to_string())?;
                    for directory in [bin, canonical_bin] {
                        if !target.dirs.contains(&directory) {
                            target.dirs.push(directory);
                        }
                    }
                }
                // Preserve every interpreter alias in a venv symlink chain.
                let mut python = environment.join("bin/python");
                let mut seen = BTreeSet::new();
                while python.is_symlink() {
                    if !seen.insert(python.clone()) {
                        return Err(format!("tool {id}: interpreter symlink cycle"));
                    }
                    let link_target = fs::read_link(&python).map_err(|e| e.to_string())?;
                    python = if link_target.is_absolute() {
                        link_target
                    } else {
                        python.parent().unwrap().join(link_target)
                    };
                    let parent = python.parent().ok_or("invalid interpreter path")?;
                    let prefix = if parent.file_name().is_some_and(|name| name == "bin") {
                        parent.parent().unwrap()
                    } else {
                        parent
                    };
                    target.mount_at_original_path(prefix)?;
                }
            }
            for import in spec.imports {
                target.mount_at_original_path(&import)?;
                if !target.imports.contains(&import) {
                    target.imports.push(import);
                }
            }
            for device in spec.devices {
                let resolved = validate_device_grant(&device)?;
                if !target.devices.contains(&resolved) {
                    target.devices.push(resolved);
                }
            }
        }
        Ok(result)
    }

    /// Select an operator-registered Plugin grant without exposing it to node
    /// shell commands or another Plugin. The default environment remains intact.
    pub(crate) fn for_plugin(&self, plugin: &str, include_global: bool) -> Self {
        let mut selected = if include_global {
            self.clone()
        } else {
            Self::default()
        };
        let grant = self.plugin_environments.get(plugin).cloned();
        selected.plugin_environments.clear();
        if let Some(grant) = grant {
            for mount in grant.mounts {
                if !selected.mounts.contains(&mount) {
                    selected.mounts.push(mount);
                }
            }
            for (paths, extra) in [
                (&mut selected.dirs, grant.dirs),
                (&mut selected.imports, grant.imports),
                (&mut selected.devices, grant.devices),
            ] {
                for path in extra {
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                }
            }
        }
        selected
    }

    fn mount_at_original_path(&mut self, path: &Path) -> Result<(), String> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(
                "tool environment/import paths must be absolute without parent traversal".into(),
            );
        }
        let resolved = path
            .canonicalize()
            .map_err(|e| format!("tool environment: {e}"))?;
        for candidate in [path, resolved.as_path()] {
            if ["/", "/root", "/home", "/tmp"]
                .iter()
                .any(|p| candidate == Path::new(p))
                || std::env::var_os("HOME").is_some_and(|home| candidate == Path::new(&home))
                || [
                    "/workspace",
                    "/in",
                    "/plugins",
                    "/tools",
                    "/proc",
                    "/dev",
                    "/etc",
                ]
                .iter()
                .any(|p| candidate.starts_with(p))
            {
                return Err("tool environment overlaps a reserved or broad host path".into());
            }
        }
        // Preserve both installation aliases and canonical prefixes: shebangs
        // may name either, and Sandbox PATH uses canonical tool directories.
        for destination in [path, resolved.as_path()] {
            if ["/usr", "/bin", "/lib", "/lib64", "/sbin"]
                .iter()
                .any(|p| destination.starts_with(p))
            {
                continue; // already mounted read-only by Bubblewrap
            }
            let mount = ReadOnlyInput::new(&resolved, destination);
            if !self.mounts.contains(&mount) {
                self.mounts.push(mount);
            }
        }
        Ok(())
    }

    pub fn authorize(&self, mut policy: BubblewrapPolicy) -> BubblewrapPolicy {
        for mount in &self.mounts {
            policy = policy
                .authorize_readonly_input_root(&mount.source)
                .authorize_readonly_destination_root(&mount.destination);
        }
        for directory in &self.dirs {
            policy = policy.authorize_tool_dir(directory);
        }
        for device in &self.devices {
            policy = policy.authorize_device_grant(device);
        }
        policy
    }

    pub(crate) fn grant_sandbox(
        &self,
        sandbox: &anchor_sandbox_bwrap::BubblewrapSandbox,
    ) -> Result<anchor_sandbox_bwrap::BubblewrapSandbox, String> {
        let sandbox = sandbox
            .with_readonly_grants(&self.mounts)
            .and_then(|sandbox| sandbox.with_executable_dirs(&self.dirs))
            .and_then(|sandbox| sandbox.with_device_grants(&self.devices))
            .map_err(|error| error.to_string())?;
        Ok(if self.rust.is_some() {
            sandbox.with_pid_namespace_proc()
        } else {
            sandbox
        })
    }

    pub fn visible_entrypoint(&self, command: &Path) -> Option<PathBuf> {
        // Keep interpreters at their installation path so venv discovery works.
        if self
            .dirs
            .iter()
            .any(|dir| command.parent() == Some(dir.as_path()))
        {
            return None;
        }
        self.mounts.iter().find_map(|mount| {
            (mount.source == command
                && mount.destination.starts_with("/tools")
                && mount
                    .destination
                    .file_name()
                    .is_some_and(|name| name == "run"))
            .then(|| mount.destination.clone())
        })
    }

    pub fn apply(&self, request: &mut SandboxRequest) {
        request.readonly_inputs.extend(self.mounts.clone());
        request.tool_dirs.extend(self.dirs.clone());
        request.devices.extend(self.devices.clone());
        if let Some(rust) = &self.rust {
            request.environment.push(SandboxEnvironment::new(
                "RUSTUP_HOME",
                rust.home.to_string_lossy(),
            ));
            request
                .environment
                .push(SandboxEnvironment::new("CARGO_HOME", PILOT_CARGO_HOME));
            if let Some(toolchain) = &rust.toolchain {
                request
                    .environment
                    .push(SandboxEnvironment::new("RUSTUP_TOOLCHAIN", toolchain));
            }
            if request.network == anchor_runtime::NetworkPolicy::Disabled {
                request
                    .environment
                    .push(SandboxEnvironment::new("CARGO_NET_OFFLINE", "true"));
            }
        }
        if self.python.is_some() || self.rust.is_some() {
            let existing = request
                .environment
                .iter()
                .find(|variable| variable.key == "PATH")
                .map(|variable| variable.value().to_owned())
                .unwrap_or_else(|| {
                    request
                        .tool_dirs
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .chain(["/usr/bin".into(), "/bin".into()])
                        .collect::<Vec<_>>()
                        .join(":")
                });
            let aliases = [
                self.python.as_ref().map(|_| PILOT_PYTHON_BIN),
                self.rust.as_ref().map(|_| PILOT_RUST_BIN),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(":");
            request
                .environment
                .retain(|variable| variable.key != "PATH");
            request.environment.push(SandboxEnvironment::new(
                "PATH",
                format!("{aliases}:{existing}"),
            ));
        }
        if !self.imports.is_empty() && !request.environment.iter().any(|v| v.key == "PYTHONPATH") {
            request.environment.push(SandboxEnvironment::new(
                "PYTHONPATH",
                self.imports
                    .iter()
                    .map(|p| p.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(":"),
            ));
        }
    }
}
