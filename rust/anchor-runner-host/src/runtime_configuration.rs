//! Host-owned tool preparation. Explicit deployment values remain external;
//! managed fields are published only after the official tool finishes and the
//! resulting installation is probed. No process environment is mutated.

use crate::dependency_provision::{
    DependencyAccess, DependencyProvisionConfig, DependencyProvisioner, PackageSource,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

// The supported platform bundle targets GNU Linux. The bootstrap executable
// can be statically linked against musl without making musl the runtime host.
const RUST_TOOLCHAIN_HOST: &str = "x86_64-unknown-linux-gnu";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RustInstallation {
    pub(crate) proxy: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) cargo: PathBuf,
    #[serde(default)]
    pub(crate) toolchain: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedFields {
    format: u32,
    python: Option<PathBuf>,
    python_enabled: Option<bool>,
    sources: Option<Vec<String>>,
    rust: Option<RustInstallation>,
}

impl Default for ManagedFields {
    fn default() -> Self {
        Self {
            format: 1,
            python: None,
            python_enabled: None,
            sources: None,
            rust: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeSnapshot {
    pub(crate) python: Option<PathBuf>,
    pub(crate) python_enabled: bool,
    pub(crate) rust: Option<RustInstallation>,
    pub(crate) pixi: Option<PathBuf>,
    pub(crate) provisioner: Option<Arc<DependencyProvisioner>>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapTool {
    path: PathBuf,
    sha256: String,
    version: String,
    source: String,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapTools {
    uv: Option<BootstrapTool>,
    rustup_init: Option<BootstrapTool>,
    pixi: Option<BootstrapTool>,
}

pub(crate) struct RuntimeConfiguration {
    root: PathBuf,
    library: PathBuf,
    bwrap: PathBuf,
    external_python: Option<PathBuf>,
    external_python_enabled: Option<bool>,
    external_rust: Option<RustInstallation>,
    pixi: Option<PathBuf>,
    external_provision: Option<DependencyProvisionConfig>,
    external_fields: BTreeSet<&'static str>,
    release: Option<PathBuf>,
    bootstrap: BootstrapTools,
    current: std::sync::RwLock<Option<RuntimeSnapshot>>,
}

fn optional_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn release_root() -> Result<Option<PathBuf>, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let Some(root) = executable.parent().and_then(Path::parent) else {
        return Ok(None);
    };
    if !root.join("runtime-manifest.json").exists() {
        return Ok(None);
    }
    root.canonicalize()
        .map(Some)
        .map_err(|error| error.to_string())
}

pub(crate) fn install_release_network(library_root: &Path) -> Result<(), String> {
    let Some(release) = release_root()? else {
        return Ok(());
    };
    let source = release.join("plugins/network");
    if !source.is_dir() {
        return Ok(());
    }
    let catalog = crate::plugin_library::Catalog::new(library_root);
    if catalog.definition("network").is_ok() {
        return Ok(());
    }
    if library_root.join("plugins/network").exists() {
        return Err(
            "installed network Plugin is invalid; bundled bootstrap will not overwrite it".into(),
        );
    }
    let library = crate::plugin_library::PluginLibrary::new(library_root);
    let review = library
        .review_directory("network", &source)
        .map_err(|error| error.to_string())?;
    if !review.dependencies.is_empty() {
        return Err("bundled network bootstrap must not need Python dependencies".into());
    }
    library
        .install_directory("network", &source, false)
        .map_err(|error| error.to_string())?;
    let plugin = catalog
        .definition("network")
        .map_err(|error| error.to_string())?;
    if !plugin.network {
        return Err("bundled network bootstrap has invalid network declaration".into());
    }
    Ok(())
}

impl RuntimeConfiguration {
    pub(crate) fn from_env(state: &Path, library: &Path, bwrap: PathBuf) -> Result<Self, String> {
        Self::from_env_with_release(state, library, bwrap, release_root()?)
    }

    fn from_env_with_release(
        state: &Path,
        library: &Path,
        bwrap: PathBuf,
        release: Option<PathBuf>,
    ) -> Result<Self, String> {
        let root = state.join("managed");
        crate::host_fs::directory(&root)?;
        let bootstrap = if let Some(release) = &release {
            let manifest: Value = serde_json::from_slice(
                &fs::read(release.join("runtime-manifest.json"))
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            serde_json::from_value(
                manifest
                    .get("bootstrap")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            )
            .map_err(|_| "invalid release bootstrap manifest")?
        } else {
            BootstrapTools::default()
        };
        let external_provision =
            crate::dependency_provision::from_env(state, bwrap.clone(), Some(library))?;
        let external_python = optional_path(crate::dependency_provision::PYTHON_INTERPRETER_ENV);
        if optional_path("ANCHOR_PILOT_PYTHON").is_some() {
            return Err("ANCHOR_PILOT_PYTHON is no longer supported; use ANCHOR_DEPENDENCIES_PYTHON and ANCHOR_PILOT_PYTHON_ENABLED".into());
        }
        let external_python_enabled =
            match std::env::var("ANCHOR_PILOT_PYTHON_ENABLED").ok().as_deref() {
                None | Some("") => None,
                Some("0") => Some(false),
                Some("1") => Some(true),
                Some(_) => return Err("ANCHOR_PILOT_PYTHON_ENABLED must be 0 or 1".into()),
            };
        if optional_path("ANCHOR_PILOT_RUSTUP").is_none()
            && (optional_path("ANCHOR_PILOT_RUSTUP_HOME").is_some()
                || optional_path("ANCHOR_PILOT_CARGO_HOME").is_some())
        {
            return Err(
                "ANCHOR_PILOT_RUSTUP is required when Rustup/Cargo roots are externally configured"
                    .into(),
            );
        }
        let external_rust = optional_path("ANCHOR_PILOT_RUSTUP")
            .map(|proxy| {
                Ok::<_, String>(RustInstallation {
                    proxy,
                    home: optional_path("ANCHOR_PILOT_RUSTUP_HOME")
                        .ok_or("ANCHOR_PILOT_RUSTUP_HOME is required")?,
                    cargo: optional_path("ANCHOR_PILOT_CARGO_HOME")
                        .ok_or("ANCHOR_PILOT_CARGO_HOME is required")?,
                    toolchain: None,
                })
            })
            .transpose()?;
        let pixi = optional_path("ANCHOR_PILOT_PIXI")
            .map(|path| {
                crate::host_fs::executable(&path)
                    .map_err(|error| format!("ANCHOR_PILOT_PIXI {error}"))
            })
            .transpose()?;
        let external_fields = [
            "ANCHOR_DEPENDENCIES_PYTHON",
            "ANCHOR_PILOT_PYTHON_ENABLED",
            "ANCHOR_DEPENDENCIES_SOURCES",
            "ANCHOR_DEPENDENCIES_UV",
            "ANCHOR_DEPENDENCIES_UV_SHA256",
            "ANCHOR_DEPENDENCIES_BUILD_ROOT",
            "ANCHOR_PILOT_RUSTUP",
            "ANCHOR_PILOT_RUSTUP_HOME",
            "ANCHOR_PILOT_CARGO_HOME",
            "ANCHOR_PILOT_PIXI",
        ]
        .into_iter()
        .filter(|field| std::env::var_os(field).is_some())
        .collect();
        let mut result = Self {
            root,
            library: library.into(),
            bwrap,
            external_python,
            external_python_enabled,
            external_rust,
            pixi,
            external_provision,
            external_fields,
            release,
            bootstrap,
            current: std::sync::RwLock::new(None),
        };
        // Release files are read-only. Resolve and hash pixi only at startup;
        // status queries and each invocation reuse this verified deployment grant.
        if result.pixi.is_none() {
            result.pixi = result.bootstrap_tool("pixi").ok();
        }
        let snapshot = result.snapshot_fields(result.fields()?)?;
        *result
            .current
            .write()
            .map_err(|_| "runtime configuration lock unavailable")? = Some(snapshot);
        Ok(result)
    }

    fn fields(&self) -> Result<ManagedFields, String> {
        use rustix::fs::{Mode, OFlags, open, openat};
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut parent =
            open("/", flags, Mode::empty()).map_err(|_| "managed runtime root unavailable")?;
        for component in self.root.components() {
            match component {
                std::path::Component::RootDir => {}
                std::path::Component::Normal(name) => {
                    parent = openat(&parent, name, flags, Mode::empty())
                        .map_err(|_| "managed runtime root must not contain symlinks")?
                }
                _ => return Err("managed runtime root must be absolute".into()),
            }
        }
        let file: fs::File = match openat(
            &parent,
            "runtime-config.json",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => file.into(),
            Err(error) if error == rustix::io::Errno::NOENT => return Ok(ManagedFields::default()),
            Err(_) => return Err("managed runtime config unavailable".into()),
        };
        if !file
            .metadata()
            .map_err(|_| "managed runtime metadata unavailable")?
            .is_file()
        {
            return Err("managed runtime config must be a bounded regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(65537)
            .read_to_end(&mut bytes)
            .map_err(|_| "managed runtime config unavailable")?;
        if bytes.len() > 65536 {
            return Err("managed runtime config must be a bounded regular file".into());
        }
        let fields: ManagedFields =
            serde_json::from_slice(&bytes).map_err(|_| "managed runtime config is invalid")?;
        if fields.format != 1 {
            return Err("unsupported managed runtime config format".into());
        }
        for path in fields.python.iter().chain(
            fields
                .rust
                .iter()
                .flat_map(|rust| [&rust.proxy, &rust.home, &rust.cargo]),
        ) {
            if !path.is_absolute()
                || !path.starts_with(&self.root)
                || path
                    .components()
                    .any(|component| component == std::path::Component::ParentDir)
            {
                return Err("managed installation path escapes its owned root".into());
            }
        }
        Ok(fields)
    }

    fn bootstrap_tool(&self, name: &str) -> Result<PathBuf, String> {
        let tool = match name {
            "uv" => self.bootstrap.uv.as_ref(),
            "rustup_init" => self.bootstrap.rustup_init.as_ref(),
            "pixi" => self.bootstrap.pixi.as_ref(),
            _ => None,
        }
        .ok_or_else(|| {
            format!("the installed Anchor package has no native {name} bootstrap tool")
        })?;
        let root = self
            .release
            .as_ref()
            .ok_or("the native bootstrap package is unavailable")?;
        if tool.path.is_absolute()
            || tool
                .path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || tool.sha256.len() != 64
            || !tool.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || tool.version.is_empty()
            || !tool.source.starts_with("https://")
        {
            return Err("invalid native bootstrap tool identity".into());
        }
        let path = crate::host_fs::executable(&root.join(&tool.path))
            .map_err(|error| format!("native bootstrap tool {error}"))?;
        if !path.starts_with(root) {
            return Err("native bootstrap tool escapes release".into());
        }
        let mut reader = fs::File::open(&path).map_err(|error| error.to_string())?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        if format!("{:x}", digest.finalize()) != tool.sha256 {
            return Err("native bootstrap tool digest changed".into());
        }
        Ok(path)
    }

    pub(crate) fn snapshot(&self) -> Result<RuntimeSnapshot, String> {
        self.current
            .read()
            .map_err(|_| "runtime configuration lock unavailable")?
            .clone()
            .ok_or_else(|| "runtime configuration not initialized".into())
    }

    #[cfg(test)]
    pub(crate) fn from_snapshot_for_test(
        state: &Path,
        library: &Path,
        bwrap: PathBuf,
        snapshot: RuntimeSnapshot,
    ) -> Self {
        Self {
            root: state.join("managed"),
            library: library.to_path_buf(),
            bwrap,
            external_python: None,
            external_python_enabled: None,
            external_rust: None,
            pixi: snapshot.pixi.clone(),
            external_provision: None,
            external_fields: BTreeSet::new(),
            release: None,
            bootstrap: BootstrapTools::default(),
            current: std::sync::RwLock::new(Some(snapshot)),
        }
    }

    #[cfg(test)]
    pub(crate) fn publish_snapshot_for_test(&self, snapshot: RuntimeSnapshot) {
        *self.current.write().expect("runtime snapshot test lock") = Some(snapshot);
    }

    fn snapshot_fields(&self, managed: ManagedFields) -> Result<RuntimeSnapshot, String> {
        let python = self.external_python.clone().or(managed.python);
        let python_enabled = self
            .external_python_enabled
            .or(managed.python_enabled)
            .unwrap_or(false);
        let rust = self.external_rust.clone().or(managed.rust);
        let config = if let Some(config) = &self.external_provision {
            Some(config.clone())
        } else if let (Some(python), Some(sources)) = (&python, managed.sources) {
            if sources.is_empty()
                || sources
                    .iter()
                    .any(|source| source != "https://pypi.org/simple")
            {
                return Err(
                    "managed Python package source is not the supported public index".into(),
                );
            }
            let uv = self.bootstrap_tool("uv")?;
            let uv_digest = self
                .bootstrap
                .uv
                .as_ref()
                .expect("verified uv")
                .sha256
                .clone();
            Some(DependencyProvisionConfig {
                uv,
                uv_digest,
                python: python.clone(),
                sources: sources.into_iter().map(PackageSource::Index).collect(),
                build_root: self.root.join("dependency-build"),
                bwrap: self.bwrap.clone(),
            })
        } else {
            None
        };
        if self.library.starts_with(&self.root) || self.root.starts_with(&self.library) {
            return Err("managed runtime and Library roots must be disjoint".into());
        }
        let provisioner = config
            .map(DependencyProvisioner::new)
            .transpose()
            .map_err(|error| error.to_string())?
            .map(Arc::new);
        Ok(RuntimeSnapshot {
            python,
            python_enabled,
            rust,
            pixi: self.pixi.clone(),
            provisioner,
        })
    }

    fn python_preparation_fields_conflict(&self) -> bool {
        self.external_provision.is_none()
            && [
                "ANCHOR_DEPENDENCIES_SOURCES",
                "ANCHOR_DEPENDENCIES_UV",
                "ANCHOR_DEPENDENCIES_UV_SHA256",
                "ANCHOR_DEPENDENCIES_BUILD_ROOT",
            ]
            .iter()
            .any(|field| self.external_fields.contains(field))
    }

    pub(crate) fn status(&self) -> Value {
        let snapshot = self.snapshot().ok();
        let python = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.python.as_ref());
        let rust = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.rust.as_ref());
        let pixi_available = snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.pixi.is_some());
        // An external executable is authorized, but its version is not probed.
        let pixi_version = pixi_available
            .then_some(self.bootstrap.pixi.as_ref())
            .flatten()
            .filter(|_| !self.external_fields.contains("ANCHOR_PILOT_PIXI"))
            .map(|tool| tool.version.as_str());
        let python_bootstrap_available = self.bootstrap_tool("uv").is_ok();
        let python_prepare_available = self.external_python_enabled != Some(false)
            && !self.python_preparation_fields_conflict()
            && ((self.external_python.is_some() && self.external_provision.is_some())
                || python_bootstrap_available);
        json!({"pixi_available":pixi_available, "pixi_version":pixi_version, "python_bootstrap_available":python_bootstrap_available, "python_prepare_available":python_prepare_available, "rust_bootstrap_available":self.bootstrap_tool("rustup_init").is_ok(), "python_configuration_owner":if self.external_fields.contains("ANCHOR_DEPENDENCIES_PYTHON") {"deployment"} else {"host"}, "rust_configuration_owner":if self.external_fields.contains("ANCHOR_PILOT_RUSTUP") {"deployment"} else {"host"}, "python_default_version":"3.12", "rust_default_toolchain":"stable", "python_configured":python.is_some_and(|path| path.is_file()), "python_enabled":snapshot.as_ref().is_some_and(|snapshot| snapshot.python_enabled), "python_explicitly_disabled":self.external_python_enabled == Some(false), "python_path":python, "rust_configured":rust.is_some_and(|installation| installation.proxy.is_file()), "rust_toolchain":rust.and_then(|installation| installation.toolchain.as_ref())})
    }

    async fn preparation_lease(
        &self,
        access: &DependencyAccess,
    ) -> Result<anchor_file_lease::FileLease, String> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(self.root.join("runtime-prepare.lock"))
            .map_err(|error| error.to_string())?;
        if !file
            .metadata()
            .map_err(|_| "runtime preparation lock unavailable")?
            .is_file()
        {
            return Err("runtime preparation lock must be a regular file".into());
        }
        let file = anchor_file_lease::FileLease::new(file);
        loop {
            access.check().map_err(|error| error.to_string())?;
            match file.lock(rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(file),
                Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    fn publish(&self, fields: &ManagedFields) -> Result<(), String> {
        let mut temporary =
            tempfile::NamedTempFile::new_in(&self.root).map_err(|error| error.to_string())?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
        use std::io::Write;
        temporary
            .write_all(&serde_json::to_vec(fields).map_err(|error| error.to_string())?)
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|error| error.to_string())?;
        temporary
            .persist(self.root.join("runtime-config.json"))
            .map_err(|error| error.to_string())?;
        fs::File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn prepare(
        &self,
        kind: &str,
        version: Option<&str>,
        access: &DependencyAccess,
    ) -> Result<Value, String> {
        self.prepare_observed(kind, version, access, |_| {}).await
    }

    pub(crate) async fn prepare_observed(
        &self,
        kind: &str,
        version: Option<&str>,
        access: &DependencyAccess,
        progress: impl Fn(&str) + Send + Sync,
    ) -> Result<Value, String> {
        access.check().map_err(|error| error.to_string())?;
        if !access.network {
            return Err("runtime preparation requires task network authorization; call session_network first and continue after this Turn finishes".into());
        }
        progress("waiting_for_lock");
        let _lease = self.preparation_lease(access).await?;
        progress("checking_configuration");
        let mut fields = self.fields()?;
        let before = fields.clone();
        match kind {
            "python" => {
                if self.external_python_enabled == Some(false) {
                    return Err("Pilot Python is explicitly disabled by the deployment".into());
                }
                if self.python_preparation_fields_conflict() {
                    return Err("dependency preparation fields are externally managed and cannot be overwritten by Pilot".into());
                }
                let version = version.unwrap_or("3.12");
                let parts = version.split('.').collect::<Vec<_>>();
                if !(2..=3).contains(&parts.len())
                    || parts.first() != Some(&"3")
                    || parts.iter().any(|part| {
                        part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit())
                    })
                    || parts[1].parse::<u32>().is_ok_and(|minor| minor < 8)
                {
                    return Err("python version must be a numeric Python 3.8+ major.minor or major.minor.patch".into());
                }
                if let Some(python) = &self.external_python {
                    let mut environment = crate::tool_environment::ToolEnvironment::default();
                    environment.configure_pilot_python(python)?;
                    if self.external_python_enabled.is_none() {
                        fields.python_enabled = Some(true);
                    }
                    if self.external_provision.is_none() {
                        self.bootstrap_tool("uv")?;
                        fields.sources = Some(vec!["https://pypi.org/simple".into()]);
                    }
                } else {
                    let uv = self.bootstrap_tool("uv")?;
                    let python_root = self.root.join("python");
                    crate::host_fs::directory(&python_root)?;
                    let home = self.root.join("bootstrap-home");
                    crate::host_fs::directory(&home)?;
                    let environment = [
                        ("HOME", home.as_path()),
                        ("UV_PYTHON_INSTALL_DIR", python_root.as_path()),
                    ];
                    progress("installing_python");
                    run_native(
                        &uv,
                        &[
                            "--no-config",
                            "python",
                            "install",
                            "--no-bin",
                            "--install-dir",
                            python_root.to_str().ok_or("Python root must be UTF-8")?,
                            version,
                        ],
                        &environment,
                        access,
                    )
                    .await?;
                    progress("probing_python");
                    let output = run_native(
                        &uv,
                        &[
                            "--no-config",
                            "python",
                            "find",
                            "--managed-python",
                            "--no-python-downloads",
                            version,
                        ],
                        &environment,
                        access,
                    )
                    .await?;
                    let python = PathBuf::from(output.trim());
                    let canonical = python
                        .canonicalize()
                        .map_err(|_| "prepared Python could not be located")?;
                    if !canonical.starts_with(&python_root) {
                        return Err(
                            "uv selected a Python outside the owned installation root".into()
                        );
                    }
                    let mut environment = crate::tool_environment::ToolEnvironment::default();
                    environment.configure_pilot_python(&canonical)?;
                    fields.python = Some(canonical);
                    if self.external_python_enabled.is_none() {
                        fields.python_enabled = Some(true);
                    }
                    fields.sources = Some(vec!["https://pypi.org/simple".into()]);
                }
            }
            "rust" => {
                if self.external_fields.iter().any(|field| {
                    matches!(
                        *field,
                        "ANCHOR_PILOT_RUSTUP"
                            | "ANCHOR_PILOT_RUSTUP_HOME"
                            | "ANCHOR_PILOT_CARGO_HOME"
                    )
                }) {
                    return Err("Rust runtime fields are managed by this deployment and cannot be overwritten by Pilot".into());
                }
                let requested = version.unwrap_or("stable");
                let host_suffix = format!("-{RUST_TOOLCHAIN_HOST}");
                let version = requested.strip_suffix(&host_suffix).unwrap_or(requested);
                if version.is_empty()
                    || requested.len() > 80
                    || version.starts_with('-')
                    || !version
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
                {
                    return Err(
                        "rust version must be a release/toolchain name without paths or options"
                            .into(),
                    );
                }
                // Full selection also avoids changing an existing rustup's
                // default-host, which still resolves toolchains in older Turns.
                let selected = format!("{version}{host_suffix}");
                let initializer = self.bootstrap_tool("rustup_init")?;
                let home = self.root.join("bootstrap-home");
                let cargo = self.root.join("rust/cargo");
                let rustup = self.root.join("rust/rustup");
                for root in [&home, &cargo, &rustup] {
                    crate::host_fs::directory(root)?;
                }
                let environment = [
                    ("HOME", home.as_path()),
                    ("CARGO_HOME", cargo.as_path()),
                    ("RUSTUP_HOME", rustup.as_path()),
                ];
                if !cargo.join("bin/rustup").is_file() {
                    progress("installing_rustup");
                    run_native(
                        &initializer,
                        &[
                            "-y",
                            "--no-modify-path",
                            "--default-host",
                            RUST_TOOLCHAIN_HOST,
                            "--default-toolchain",
                            "none",
                            "--profile",
                            "minimal",
                        ],
                        &environment,
                        access,
                    )
                    .await?;
                }
                let proxy = cargo.join("bin/rustup");
                // `rustup run` does not install without `--install`. Failure
                // can mean an installed compiler is unusable, so consult the
                // official inventory before considering installation.
                progress("checking_rust_toolchain");
                let runnable = run_native(
                    &proxy,
                    &["run", &selected, "rustc", "--version"],
                    &environment,
                    access,
                )
                .await
                .is_ok();
                if !runnable {
                    let installed = run_native(
                        &proxy,
                        &["toolchain", "list", "--quiet"],
                        &environment,
                        access,
                    )
                    .await?;
                    if !installed.lines().any(|name| name == selected) {
                        progress("installing_rust_toolchain");
                        // A static musl rustup needs explicit non-host selection
                        // even for this bundle's supported GNU Linux platform.
                        run_native(
                            &proxy,
                            &[
                                "toolchain",
                                "install",
                                &selected,
                                "--profile",
                                "minimal",
                                "--no-self-update",
                                "--no-update",
                                "--force-non-host",
                            ],
                            &environment,
                            access,
                        )
                        .await?;
                    }
                }
                // Installation and directory validation alone do not prove
                // that a toolchain can execute on this host.
                progress("probing_rust_toolchain");
                for command in ["rustc", "cargo"] {
                    run_native(
                        &proxy,
                        &["run", &selected, command, "--version"],
                        &environment,
                        access,
                    )
                    .await
                    .map_err(|error| {
                        format!(
                            "prepared Rust toolchain {selected} {command} probe failed: {error}"
                        )
                    })?;
                }
                run_native(
                    &proxy,
                    &["default", &selected, "--force-non-host"],
                    &environment,
                    access,
                )
                .await?;
                let mut tools = crate::tool_environment::ToolEnvironment::default();
                tools.configure_pilot_rust(&proxy, &rustup, &cargo)?;
                fields.rust = Some(RustInstallation {
                    proxy,
                    home: rustup,
                    cargo,
                    toolchain: Some(selected),
                });
            }
            _ => return Err("kind must be python or rust".into()),
        }
        access.check().map_err(|error| error.to_string())?;
        progress("publishing_configuration");
        let snapshot = self.snapshot_fields(fields.clone())?;
        self.publish(&fields)?;
        *self
            .current
            .write()
            .map_err(|_| "runtime configuration lock unavailable")? = Some(snapshot);
        Ok(
            json!({"prepared":true,"kind":kind,"changed":before!=fields,"available_next_turn":before!=fields,"pip_write":false,"bootstrap":self.status()}),
        )
    }
}

async fn limited_output(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    reader
        .take(256 * 1024 + 1)
        .read_to_end(&mut output)
        .await
        .map_err(|_| "native bootstrap output unavailable")?;
    if output.len() > 256 * 1024 {
        return Err("native bootstrap output exceeded its limit".into());
    }
    Ok(output)
}

async fn run_native(
    executable: &Path,
    arguments: &[&str],
    environment: &[(&str, &Path)],
    access: &DependencyAccess,
) -> Result<String, String> {
    access.check().map_err(|error| error.to_string())?;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    for (name, path) in environment {
        command.env(name, path);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "native bootstrap process could not start")?;
    let group = child
        .id()
        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32));
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let result = tokio::select! {
        result = async { let (status, out, err) = tokio::try_join!(async {child.wait().await.map_err(|_|"native bootstrap process wait failed".to_owned())}, limited_output(&mut stdout),limited_output(&mut stderr))?; if !status.success() { return Err(format!("native bootstrap failed: {}",String::from_utf8_lossy(&err))) } String::from_utf8(out).map_err(|_|"native bootstrap output is not UTF-8".to_owned()) } => result,
        _ = access.cancelled() => Err("native bootstrap was cancelled".into()),
        _ = tokio::time::sleep(Duration::from_secs(600)) => Err("native bootstrap timed out".into()),
    };
    if result.is_err() {
        if let Some(group) = group {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, RuntimeConfiguration) {
        let root = tempfile::tempdir().unwrap();
        let release = root.path().join("release");
        let managed = root.path().join("state/managed");
        crate::host_fs::directory(&release).unwrap();
        crate::host_fs::directory(&managed).unwrap();
        let uv = release.join("uv");
        let script = r#"#!/bin/sh
set -eu
case "$*" in
    *"python install"*)
        mkdir -p "$UV_PYTHON_INSTALL_DIR/runtime/bin"
        printf '#!/bin/sh\nprintf "3.12\\n"\n' > "$UV_PYTHON_INSTALL_DIR/runtime/bin/python3"
        chmod 755 "$UV_PYTHON_INSTALL_DIR/runtime/bin/python3" ;;
    *"python find"*) printf '%s\n' "$UV_PYTHON_INSTALL_DIR/runtime/bin/python3" ;;
    *) exit 1 ;;
esac
"#;
        fs::write(&uv, script).unwrap();
        fs::set_permissions(&uv, fs::Permissions::from_mode(0o755)).unwrap();
        let configuration = RuntimeConfiguration {
            root: managed,
            library: root.path().join("library"),
            bwrap: "bwrap".into(),
            external_python: None,
            external_python_enabled: None,
            external_rust: None,
            pixi: None,
            external_provision: None,
            external_fields: BTreeSet::new(),
            release: Some(release),
            bootstrap: BootstrapTools {
                uv: Some(BootstrapTool {
                    path: "uv".into(),
                    sha256: format!("{:x}", Sha256::digest(script.as_bytes())),
                    version: "test-native-seam".into(),
                    source: "https://example.com/uv".into(),
                }),
                rustup_init: None,
                pixi: None,
            },
            current: std::sync::RwLock::new(Some(RuntimeSnapshot {
                python: None,
                python_enabled: false,
                rust: None,
                pixi: None,
                provisioner: None,
            })),
        };
        (root, configuration)
    }
    #[tokio::test]
    async fn pixi_release_is_verified_once_and_deployment_override_is_validated_without_execution()
    {
        use crate::test_support::Environment;
        let _guard = crate::PROCESS_ENV_LOCK.lock().await;
        let mut environment = Environment::cleared(&[
            "ANCHOR_DEPENDENCIES_PYTHON",
            "ANCHOR_DEPENDENCIES_SOURCES",
            "ANCHOR_DEPENDENCIES_UV",
            "ANCHOR_DEPENDENCIES_UV_SHA256",
            "ANCHOR_DEPENDENCIES_BUILD_ROOT",
            "ANCHOR_PILOT_PYTHON",
            "ANCHOR_PILOT_PYTHON_ENABLED",
            "ANCHOR_PILOT_RUSTUP",
            "ANCHOR_PILOT_RUSTUP_HOME",
            "ANCHOR_PILOT_CARGO_HOME",
            "ANCHOR_PILOT_PIXI",
        ]);
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let library = root.path().join("library");
        let load = |release| {
            RuntimeConfiguration::from_env_with_release(&state, &library, "bwrap".into(), release)
        };
        let missing = load(None).unwrap();
        assert_eq!(missing.status()["pixi_available"], false);
        assert_eq!(missing.status()["pixi_version"], Value::Null);
        assert!(missing.snapshot().unwrap().pixi.is_none());
        let release = root.path().join("release");
        fs::create_dir_all(release.join("bin")).unwrap();
        let pixi = release.join("bin/pixi");
        // A fixture executable is only hashed and mounted, never launched.
        fs::copy("/usr/bin/true", &pixi).unwrap();
        let digest = format!("{:x}", Sha256::digest(fs::read(&pixi).unwrap()));
        fs::write(release.join("runtime-manifest.json"), serde_json::to_vec(&json!({
            "bootstrap": {"pixi": {
                "path": "bin/pixi", "sha256": digest, "version": "0.83.0",
                "source": "https://github.com/prefix-dev/pixi/releases/download/v0.83.0/pixi-x86_64-unknown-linux-musl"
            }}
        })).unwrap()).unwrap();
        let configuration = load(Some(release.clone())).unwrap();
        assert_eq!(configuration.status()["pixi_available"], true);
        assert_eq!(configuration.status()["pixi_version"], "0.83.0");
        assert_eq!(configuration.snapshot().unwrap().pixi, Some(pixi.clone()));
        fs::write(&pixi, "changed fixture bytes").unwrap();
        // The immutable release grant is frozen at construction, not rehashed
        // on platform_status or when another runtime snapshot is prepared.
        assert_eq!(configuration.status()["pixi_available"], true);
        assert_eq!(
            configuration
                .snapshot_fields(ManagedFields::default())
                .unwrap()
                .pixi,
            Some(pixi.clone())
        );
        let mismatch = load(Some(release.clone())).unwrap();
        assert_eq!(mismatch.status()["pixi_available"], false);
        assert!(mismatch.status()["pixi_version"].is_null());
        assert!(mismatch.snapshot().unwrap().pixi.is_none());
        fs::remove_file(&pixi).unwrap();
        assert_eq!(
            load(Some(release.clone())).unwrap().status()["pixi_available"],
            false
        );

        fs::copy("/usr/bin/true", &pixi).unwrap();
        let external = root.path().join("deployment-pixi");
        fs::copy("/usr/bin/true", &external).unwrap();
        environment.set("ANCHOR_PILOT_PIXI", &external);
        for release in [None, Some(release.clone())] {
            let configuration = load(release).unwrap();
            assert_eq!(configuration.status()["pixi_available"], true);
            assert!(configuration.status()["pixi_version"].is_null());
            assert_eq!(
                configuration.snapshot().unwrap().pixi,
                Some(external.clone())
            );
            assert!(configuration.external_fields.contains("ANCHOR_PILOT_PIXI"));
            assert!(configuration.fields().unwrap() == ManagedFields::default());
        }
        let non_executable = root.path().join("non-executable");
        fs::write(&non_executable, "fixture").unwrap();
        for invalid in [
            PathBuf::from("relative-pixi"),
            root.path().join("absent"),
            release.clone(),
            non_executable,
        ] {
            environment.set("ANCHOR_PILOT_PIXI", invalid);
            let error = load(Some(release.clone()))
                .err()
                .expect("bad deployment pixi must fail startup");
            assert!(error.contains("ANCHOR_PILOT_PIXI"), "{error}");
        }
    }

    #[tokio::test]
    async fn managed_bootstrap_is_verified_probed_published_and_repeat_is_idempotent() {
        let (_root, configuration) = fixture();
        let access = DependencyAccess::default();
        let before = configuration.snapshot().unwrap();
        assert!(!before.python_enabled);
        let result = configuration
            .prepare("python", None, &access)
            .await
            .unwrap();
        assert_eq!(result["changed"], true);
        let after = configuration.snapshot().unwrap();
        assert!(after.python_enabled);
        assert!(
            after
                .python
                .unwrap()
                .starts_with(configuration.root.join("python"))
        );
        assert!(after.provisioner.is_some());
        assert!(configuration.root.join("runtime-config.json").is_file());
        assert_eq!(
            configuration
                .prepare("python", None, &access)
                .await
                .unwrap()["changed"],
            false
        );
    }

    fn configure_deployment_python(
        root: &Path,
        configuration: &mut RuntimeConfiguration,
        with_provision: bool,
    ) -> PathBuf {
        let python = root.join("deployment-python/bin/python3");
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        fs::write(&python, "#!/bin/sh\nprintf '3.12\\n'\n").unwrap();
        fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
        configuration.external_python = Some(python.clone());
        configuration.external_python_enabled = Some(true);
        configuration
            .external_fields
            .extend(["ANCHOR_DEPENDENCIES_PYTHON", "ANCHOR_PILOT_PYTHON_ENABLED"]);
        if with_provision {
            configuration.external_provision = Some(DependencyProvisionConfig {
                uv: configuration.bootstrap_tool("uv").unwrap(),
                uv_digest: configuration.bootstrap.uv.as_ref().unwrap().sha256.clone(),
                python: python.clone(),
                sources: vec![PackageSource::Index("https://pypi.org/simple".into())],
                build_root: root.join("deployment-build"),
                bwrap: configuration.bwrap.clone(),
            });
            configuration.external_fields.extend([
                "ANCHOR_DEPENDENCIES_SOURCES",
                "ANCHOR_DEPENDENCIES_UV",
                "ANCHOR_DEPENDENCIES_UV_SHA256",
                "ANCHOR_DEPENDENCIES_BUILD_ROOT",
            ]);
        }
        configuration.publish_snapshot_for_test(
            configuration
                .snapshot_fields(ManagedFields::default())
                .unwrap(),
        );
        python
    }

    #[tokio::test]
    async fn complete_external_python_preparation_does_not_require_release_bootstrap() {
        let (root, mut configuration) = fixture();
        let python = configure_deployment_python(root.path(), &mut configuration, true);
        configuration.release = None;
        configuration.bootstrap = BootstrapTools::default();
        let status = configuration.status();
        assert_eq!(status["python_bootstrap_available"], false);
        assert_eq!(status["python_prepare_available"], true);
        assert_eq!(status["python_configured"], true);
        assert_eq!(status["python_enabled"], true);
        assert_eq!(status["python_configuration_owner"], "deployment");
        let access = DependencyAccess::default();
        assert_eq!(
            configuration
                .prepare("python", None, &access)
                .await
                .unwrap()["prepared"],
            true
        );
        let snapshot = configuration.snapshot().unwrap();
        assert_eq!(snapshot.python, Some(python));
        assert!(snapshot.provisioner.is_some());
        let managed = configuration.fields().unwrap();
        assert!(managed.python.is_none());
        assert!(managed.sources.is_none());
        configuration.external_python_enabled = Some(false);
        assert_eq!(configuration.status()["python_prepare_available"], false);
        let error = configuration
            .prepare("python", None, &access)
            .await
            .unwrap_err();
        assert!(error.contains("explicitly disabled"), "{error}");
        assert_snapshot_unchanged(&configuration, &snapshot);
    }

    #[tokio::test]
    async fn external_python_stays_ready_when_preparation_tools_are_missing() {
        let (root, mut configuration) = fixture();
        let python = configure_deployment_python(root.path(), &mut configuration, false);
        assert_eq!(configuration.status()["python_prepare_available"], true);
        let bootstrap = std::mem::take(&mut configuration.bootstrap);
        let before = configuration.snapshot().unwrap();
        let status = configuration.status();
        assert_eq!(status["python_bootstrap_available"], false);
        assert_eq!(status["python_prepare_available"], false);
        assert_eq!(status["python_configured"], true);
        assert_eq!(status["python_enabled"], true);
        let access = DependencyAccess::default();
        let error = configuration
            .prepare("python", None, &access)
            .await
            .unwrap_err();
        assert!(error.contains("no native uv bootstrap tool"), "{error}");
        assert_snapshot_unchanged(&configuration, &before);
        assert!(!configuration.root.join("runtime-config.json").exists());
        configuration.bootstrap = bootstrap;
        assert_eq!(configuration.status()["python_prepare_available"], true);
        configuration
            .prepare("python", None, &access)
            .await
            .unwrap();
        let after = configuration.snapshot().unwrap();
        assert_eq!(after.python, Some(python));
        assert!(after.python_enabled);
        assert!(after.provisioner.is_some());
    }

    #[tokio::test]
    async fn python_prepare_availability_respects_external_field_conflicts_and_tool_identity() {
        let (_root, mut configuration) = fixture();
        let access = DependencyAccess::default();
        for field in [
            "ANCHOR_DEPENDENCIES_SOURCES",
            "ANCHOR_DEPENDENCIES_UV",
            "ANCHOR_DEPENDENCIES_UV_SHA256",
            "ANCHOR_DEPENDENCIES_BUILD_ROOT",
        ] {
            configuration.external_fields.insert(field);
            assert_eq!(configuration.status()["python_bootstrap_available"], true);
            assert_eq!(configuration.status()["python_prepare_available"], false);
            let error = configuration
                .prepare("python", None, &access)
                .await
                .unwrap_err();
            assert!(error.contains("externally managed"), "{error}");
            configuration.external_fields.remove(field);
        }
        assert_eq!(configuration.status()["python_prepare_available"], true);
        fs::write(
            configuration.release.as_ref().unwrap().join("uv"),
            "changed",
        )
        .unwrap();
        assert_eq!(configuration.status()["python_prepare_available"], false);
        let error = configuration
            .prepare("python", None, &access)
            .await
            .unwrap_err();
        assert!(error.contains("digest changed"), "{error}");
        assert!(!configuration.root.join("runtime-config.json").exists());
    }

    fn rust_fixture() -> (tempfile::TempDir, RuntimeConfiguration) {
        let (root, mut configuration) = fixture();
        let initializer = configuration.release.as_ref().unwrap().join("rustup-init");
        let script = r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$HOME/rustup-init-operations.log"
host=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --default-host) host="$2"; shift ;;
    esac
    shift
done
test "$host" = x86_64-unknown-linux-gnu
printf '%s\n' "$host" > "$RUSTUP_HOME/default-host"
mkdir -p "$CARGO_HOME/bin"
cat > "$CARGO_HOME/bin/rustup" <<'RUSTUP'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$CARGO_HOME/toolchain-operations.log"
case "$1" in
    run)
        if [ -f "$RUSTUP_HOME/toolchains/$2/fail-$3" ]; then
            cat "$RUSTUP_HOME/toolchains/$2/fail-$3" >&2
            exit 127
        fi
        test -f "$RUSTUP_HOME/toolchains/$2/$3"
        printf '%s fixture %s\n' "$3" "$2" ;;
    toolchain)
        case "$2" in
            list)
                test "$3" = --quiet
                for installed in "$RUSTUP_HOME"/toolchains/*; do
                    [ -d "$installed" ] || continue
                    basename "$installed"
                done ;;
            install)
                selected="$3"
                shift 3
                case " $* " in *" --no-update "*) ;; *) exit 1 ;; esac
                # The fixed static bootstrap identifies its own host as musl.
                case "$selected" in
                    *-x86_64-unknown-linux-gnu)
                        case " $* " in
                            *" --force-non-host "*) ;;
                            *) printf "toolchain '%s' may not be able to run on this system; add --force-non-host\n" "$selected" >&2; exit 1 ;;
                        esac ;;
                esac
                if [ -d "$RUSTUP_HOME/toolchains/$selected" ]; then exit 0; fi
                mkdir -p "$RUSTUP_HOME/toolchains/$selected"
                for command in rustc cargo; do
                    printf 'installed %s %s\n' "$selected" "$command" > "$RUSTUP_HOME/toolchains/$selected/$command"
                    if [ -f "$RUSTUP_HOME/install-fail-$command" ]; then
                        cp "$RUSTUP_HOME/install-fail-$command" "$RUSTUP_HOME/toolchains/$selected/fail-$command"
                    fi
                done ;;
            *) exit 1 ;;
        esac ;;
    default)
        selected="$2"
        shift 2
        case "$selected" in
            *-x86_64-unknown-linux-gnu)
                case " $* " in
                    *" --force-non-host "*) ;;
                    *) printf "toolchain '%s' may not be able to run on this system; add --force-non-host\n" "$selected" >&2; exit 1 ;;
                esac ;;
        esac
        printf '%s\n' "$selected" > "$RUSTUP_HOME/default-toolchain" ;;
    *) exit 1 ;;
esac
RUSTUP
chmod 755 "$CARGO_HOME/bin/rustup"
cp "$CARGO_HOME/bin/rustup" "$CARGO_HOME/bin/cargo"
cp "$CARGO_HOME/bin/rustup" "$CARGO_HOME/bin/rustc"
"#;
        fs::write(&initializer, script).unwrap();
        fs::set_permissions(&initializer, fs::Permissions::from_mode(0o755)).unwrap();
        configuration.bootstrap.rustup_init = Some(BootstrapTool {
            path: "rustup-init".into(),
            sha256: format!("{:x}", Sha256::digest(script.as_bytes())),
            version: "test-native-seam".into(),
            source: "https://example.com/rustup-init".into(),
        });
        for directory in ["bootstrap-home", "rust/cargo", "rust/rustup"] {
            crate::host_fs::directory(&configuration.root.join(directory)).unwrap();
        }
        (root, configuration)
    }

    fn rust_installation(configuration: &RuntimeConfiguration, selected: &str) -> RustInstallation {
        RustInstallation {
            proxy: configuration.root.join("rust/cargo/bin/rustup"),
            home: configuration.root.join("rust/rustup"),
            cargo: configuration.root.join("rust/cargo"),
            toolchain: Some(selected.into()),
        }
    }

    fn assert_snapshot_unchanged(configuration: &RuntimeConfiguration, before: &RuntimeSnapshot) {
        let after = configuration.snapshot().unwrap();
        assert_eq!(after.python, before.python);
        assert_eq!(after.python_enabled, before.python_enabled);
        assert_eq!(after.rust, before.rust);
        assert_eq!(after.pixi, before.pixi);
        match (&after.provisioner, &before.provisioner) {
            (Some(after), Some(before)) => assert!(Arc::ptr_eq(after, before)),
            (None, None) => {}
            _ => panic!("failed preparation changed the dependency provisioner"),
        }
    }

    #[tokio::test]
    async fn repeated_rust_prepare_probes_native_selection_without_updating_its_bytes() {
        let (_root, configuration) = rust_fixture();
        let access = DependencyAccess::default();
        assert_eq!(
            configuration.prepare("rust", None, &access).await.unwrap()["changed"],
            true
        );
        let first = configuration.snapshot().unwrap().rust.unwrap();
        let stable = first
            .home
            .join("toolchains/stable-x86_64-unknown-linux-gnu/rustc");
        fs::write(&stable, "active Turn's existing stable bytes").unwrap();
        assert_eq!(
            configuration.prepare("rust", None, &access).await.unwrap()["changed"],
            false
        );
        assert_eq!(
            fs::read_to_string(&stable).unwrap(),
            "active Turn's existing stable bytes"
        );
        assert_eq!(
            configuration
                .prepare("rust", Some("1.82.0"), &access)
                .await
                .unwrap()["changed"],
            true
        );
        assert_eq!(
            first.toolchain.as_deref(),
            Some("stable-x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            configuration
                .snapshot()
                .unwrap()
                .rust
                .unwrap()
                .toolchain
                .as_deref(),
            Some("1.82.0-x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            configuration
                .prepare("rust", Some("1.82.0-x86_64-unknown-linux-gnu"), &access)
                .await
                .unwrap()["changed"],
            false
        );
        assert_eq!(
            fs::read_to_string(&stable).unwrap(),
            "active Turn's existing stable bytes"
        );
        let operations = fs::read_to_string(first.cargo.join("toolchain-operations.log")).unwrap();
        assert_eq!(
            operations
                .lines()
                .filter(
                    |line| line.starts_with("toolchain install stable-x86_64-unknown-linux-gnu ")
                )
                .count(),
            1
        );
        assert_eq!(
            operations
                .lines()
                .filter(
                    |line| line.starts_with("toolchain install 1.82.0-x86_64-unknown-linux-gnu ")
                )
                .count(),
            1
        );
        assert_eq!(
            operations
                .lines()
                .filter(|line| *line == "run stable-x86_64-unknown-linux-gnu rustc --version")
                .count(),
            4
        );
        assert_eq!(
            operations
                .lines()
                .filter(|line| *line == "run stable-x86_64-unknown-linux-gnu cargo --version")
                .count(),
            2
        );
        let configuration_path = configuration.root.join("runtime-config.json");
        let published = fs::read(&configuration_path).unwrap();
        let before = configuration.snapshot().unwrap();
        let old_default = fs::read(first.home.join("default-toolchain")).unwrap();
        let error = run_native(
            &first.proxy,
            &["default", "stable-x86_64-unknown-linux-gnu"],
            &[
                ("HOME", &configuration.root.join("bootstrap-home")),
                ("CARGO_HOME", &first.cargo),
                ("RUSTUP_HOME", &first.home),
            ],
            &access,
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("may not be able to run on this system"),
            "{error}"
        );
        assert!(error.contains("--force-non-host"), "{error}");
        assert_eq!(fs::read(&configuration_path).unwrap(), published);
        assert_eq!(
            fs::read(first.home.join("default-toolchain")).unwrap(),
            old_default
        );
        assert_snapshot_unchanged(&configuration, &before);
        let initialization = fs::read_to_string(
            configuration
                .root
                .join("bootstrap-home/rustup-init-operations.log"),
        )
        .unwrap();
        assert_eq!(initialization.lines().count(), 1);
        assert!(initialization.contains("--default-host x86_64-unknown-linux-gnu"));
        assert_eq!(
            fs::read_to_string(first.home.join("default-host")).unwrap(),
            "x86_64-unknown-linux-gnu\n"
        );
    }

    #[tokio::test]
    async fn fresh_rust_requires_both_executables_before_publication_and_retries_existing_bytes() {
        for command in ["rustc", "cargo"] {
            let (_root, configuration) = rust_fixture();
            let installation = rust_installation(&configuration, "stable-x86_64-unknown-linux-gnu");
            let before = configuration.snapshot().unwrap();
            let fail_install = installation.home.join(format!("install-fail-{command}"));
            fs::write(
                &fail_install,
                format!("{command}: fixture missing ELF loader"),
            )
            .unwrap();
            let access = DependencyAccess::default();
            let error = configuration
                .prepare("rust", None, &access)
                .await
                .unwrap_err();
            assert!(
                error.contains(&format!("{command} probe failed")),
                "{error}"
            );
            assert!(error.contains("missing ELF loader"), "{error}");
            assert!(!configuration.root.join("runtime-config.json").exists());
            assert!(!installation.home.join("default-toolchain").exists());
            assert_snapshot_unchanged(&configuration, &before);
            let installed = installation
                .home
                .join("toolchains/stable-x86_64-unknown-linux-gnu");
            let rustc_bytes = fs::read(installed.join("rustc")).unwrap();
            let cargo_bytes = fs::read(installed.join("cargo")).unwrap();
            fs::remove_file(&fail_install).unwrap();
            fs::remove_file(installed.join(format!("fail-{command}"))).unwrap();
            assert_eq!(
                configuration.prepare("rust", None, &access).await.unwrap()["changed"],
                true
            );
            assert_eq!(fs::read(installed.join("rustc")).unwrap(), rustc_bytes);
            assert_eq!(fs::read(installed.join("cargo")).unwrap(), cargo_bytes);
            let operations =
                fs::read_to_string(installation.cargo.join("toolchain-operations.log")).unwrap();
            assert_eq!(
                operations
                    .lines()
                    .filter(|line| line.starts_with("toolchain install "))
                    .count(),
                1
            );
            let default = operations
                .lines()
                .position(|line| line.starts_with("default "))
                .unwrap();
            for executable in ["rustc", "cargo"] {
                assert!(operations.lines().take(default).any(|line| {
                    line == format!("run stable-x86_64-unknown-linux-gnu {executable} --version")
                }));
            }
        }
    }

    #[tokio::test]
    async fn failed_new_rust_selection_preserves_published_bytes_snapshot_and_default() {
        for command in ["rustc", "cargo"] {
            let (_root, configuration) = rust_fixture();
            let access = DependencyAccess::default();
            configuration
                .prepare("python", None, &access)
                .await
                .unwrap();
            configuration.prepare("rust", None, &access).await.unwrap();
            let before = configuration.snapshot().unwrap();
            let configuration_path = configuration.root.join("runtime-config.json");
            let published = fs::read(&configuration_path).unwrap();
            let installation = before.rust.as_ref().unwrap();
            fs::write(
                installation.home.join(format!("install-fail-{command}")),
                format!("{command}: fixture missing ELF loader"),
            )
            .unwrap();
            let error = configuration
                .prepare("rust", Some("1.82.0"), &access)
                .await
                .unwrap_err();
            assert!(
                error.contains(&format!("{command} probe failed")),
                "{error}"
            );
            assert_eq!(fs::read(&configuration_path).unwrap(), published);
            assert_snapshot_unchanged(&configuration, &before);
            assert_eq!(
                fs::read_to_string(installation.home.join("default-toolchain")).unwrap(),
                "stable-x86_64-unknown-linux-gnu\n"
            );
            let operations =
                fs::read_to_string(installation.cargo.join("toolchain-operations.log")).unwrap();
            assert!(
                !operations
                    .lines()
                    .any(|line| line.starts_with("default 1.82.0-x86_64-unknown-linux-gnu"))
            );
        }
    }

    #[tokio::test]
    async fn unusable_installed_rust_is_refused_without_reinstallation_or_configuration_changes() {
        for command in ["rustc", "cargo"] {
            let (_root, configuration) = rust_fixture();
            let access = DependencyAccess::default();
            configuration.prepare("rust", None, &access).await.unwrap();
            let before = configuration.snapshot().unwrap();
            let configuration_path = configuration.root.join("runtime-config.json");
            let published = fs::read(&configuration_path).unwrap();
            let installation = before.rust.as_ref().unwrap();
            let installed = installation
                .home
                .join("toolchains/stable-x86_64-unknown-linux-gnu");
            let executable = installed.join(command);
            fs::write(&executable, "existing Turn's exact toolchain bytes").unwrap();
            let fail_probe = installed.join(format!("fail-{command}"));
            fs::write(
                &fail_probe,
                format!("{command}: fixture missing ELF loader"),
            )
            .unwrap();
            let error = configuration
                .prepare("rust", None, &access)
                .await
                .unwrap_err();
            assert!(
                error.contains(&format!("{command} probe failed")),
                "{error}"
            );
            assert_eq!(fs::read(&configuration_path).unwrap(), published);
            assert_snapshot_unchanged(&configuration, &before);
            assert_eq!(
                fs::read_to_string(&executable).unwrap(),
                "existing Turn's exact toolchain bytes"
            );
            let operations =
                fs::read_to_string(installation.cargo.join("toolchain-operations.log")).unwrap();
            assert_eq!(
                operations
                    .lines()
                    .filter(|line| line.starts_with("toolchain install "))
                    .count(),
                1
            );
            assert_eq!(
                operations
                    .lines()
                    .filter(|line| line.starts_with("default "))
                    .count(),
                1
            );
            fs::remove_file(&fail_probe).unwrap();
            assert_eq!(
                configuration.prepare("rust", None, &access).await.unwrap()["changed"],
                false
            );
            assert_eq!(
                fs::read_to_string(&executable).unwrap(),
                "existing Turn's exact toolchain bytes"
            );
        }
    }

    #[tokio::test]
    async fn legacy_musl_default_recovers_to_explicit_gnu_without_mutating_legacy_installation() {
        let (_root, configuration) = rust_fixture();
        let legacy = rust_installation(&configuration, "stable");
        let bootstrap_home = configuration.root.join("bootstrap-home");
        let access = DependencyAccess::default();
        run_native(
            &configuration.bootstrap_tool("rustup_init").unwrap(),
            &["-y", "--default-host", RUST_TOOLCHAIN_HOST],
            &[
                ("HOME", &bootstrap_home),
                ("CARGO_HOME", &legacy.cargo),
                ("RUSTUP_HOME", &legacy.home),
            ],
            &access,
        )
        .await
        .unwrap();
        fs::write(
            legacy.home.join("default-host"),
            "x86_64-unknown-linux-musl\n",
        )
        .unwrap();
        fs::write(
            legacy.home.join("default-toolchain"),
            "stable-x86_64-unknown-linux-musl\n",
        )
        .unwrap();
        let musl = legacy
            .home
            .join("toolchains/stable-x86_64-unknown-linux-musl");
        fs::create_dir_all(&musl).unwrap();
        for command in ["rustc", "cargo"] {
            fs::write(musl.join(command), format!("legacy musl {command} bytes")).unwrap();
            fs::write(
                musl.join(format!("fail-{command}")),
                "missing musl ELF loader",
            )
            .unwrap();
        }
        let mut fields = configuration.fields().unwrap();
        fields.rust = Some(legacy.clone());
        configuration.publish(&fields).unwrap();
        configuration.publish_snapshot_for_test(configuration.snapshot_fields(fields).unwrap());
        let old_turn = configuration.snapshot().unwrap();
        let configuration_path = configuration.root.join("runtime-config.json");
        let published = fs::read(&configuration_path).unwrap();
        let error = run_native(
            &legacy.proxy,
            &[
                "toolchain",
                "install",
                "stable-x86_64-unknown-linux-gnu",
                "--profile",
                "minimal",
                "--no-self-update",
                "--no-update",
            ],
            &[
                ("HOME", &bootstrap_home),
                ("CARGO_HOME", &legacy.cargo),
                ("RUSTUP_HOME", &legacy.home),
            ],
            &access,
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("may not be able to run on this system"),
            "{error}"
        );
        assert!(error.contains("--force-non-host"), "{error}");
        assert!(
            !legacy
                .home
                .join("toolchains/stable-x86_64-unknown-linux-gnu")
                .exists()
        );
        assert_eq!(fs::read(&configuration_path).unwrap(), published);
        assert_snapshot_unchanged(&configuration, &old_turn);
        assert_eq!(
            configuration.prepare("rust", None, &access).await.unwrap()["changed"],
            true
        );
        let selected = configuration.snapshot().unwrap().rust.unwrap();
        assert_eq!(
            selected.toolchain.as_deref(),
            Some("stable-x86_64-unknown-linux-gnu")
        );
        assert_eq!(old_turn.rust.unwrap(), legacy);
        assert_eq!(
            fs::read_to_string(legacy.home.join("default-host")).unwrap(),
            "x86_64-unknown-linux-musl\n"
        );
        for command in ["rustc", "cargo"] {
            assert_eq!(
                fs::read_to_string(musl.join(command)).unwrap(),
                format!("legacy musl {command} bytes")
            );
            assert_eq!(
                fs::read_to_string(musl.join(format!("fail-{command}"))).unwrap(),
                "missing musl ELF loader"
            );
        }
        let initialization =
            fs::read_to_string(bootstrap_home.join("rustup-init-operations.log")).unwrap();
        assert_eq!(initialization.lines().count(), 1);
        let operations =
            fs::read_to_string(selected.cargo.join("toolchain-operations.log")).unwrap();
        assert!(!operations.contains("unknown-linux-musl"));
        assert!(operations.contains("run stable-x86_64-unknown-linux-gnu rustc --version\nrun stable-x86_64-unknown-linux-gnu cargo --version\ndefault stable-x86_64-unknown-linux-gnu --force-non-host"));
        assert_eq!(configuration.fields().unwrap().rust.unwrap(), selected);
    }

    #[tokio::test]
    async fn network_denial_cancellation_corrupt_tool_and_external_denial_do_not_publish() {
        let (_root, mut configuration) = fixture();
        let mut access = DependencyAccess {
            network: false,
            ..Default::default()
        };
        assert!(
            configuration
                .prepare("python", None, &access)
                .await
                .is_err()
        );
        access.network = true;
        access
            .cancellation
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(
            configuration
                .prepare("python", None, &access)
                .await
                .is_err()
        );
        access
            .cancellation
            .store(false, std::sync::atomic::Ordering::Release);
        configuration.external_python_enabled = Some(false);
        assert!(
            configuration
                .prepare("python", None, &access)
                .await
                .is_err()
        );
        configuration.external_python_enabled = None;
        fs::write(
            configuration.release.as_ref().unwrap().join("uv"),
            "changed",
        )
        .unwrap();
        assert!(
            configuration
                .prepare("python", None, &access)
                .await
                .is_err()
        );
        assert!(!configuration.root.join("runtime-config.json").exists());
    }
    #[tokio::test]
    async fn managed_config_and_lock_refuse_fifo_and_config_parent_symlink() {
        let (root, configuration) = fixture();
        let path = configuration.root.join("runtime-config.json");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &path,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        assert!(configuration.fields().is_err());
        fs::remove_file(&path).unwrap();
        let lock = configuration.root.join("runtime-prepare.lock");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &lock,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        assert!(
            configuration
                .preparation_lease(&DependencyAccess::default())
                .await
                .is_err()
        );
        let real = root.path().join("real-managed");
        fs::rename(&configuration.root, &real).unwrap();
        std::os::unix::fs::symlink(&real, &configuration.root).unwrap();
        assert!(configuration.fields().is_err());
    }
}
