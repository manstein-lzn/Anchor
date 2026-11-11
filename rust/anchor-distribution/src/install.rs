//! A fresh private platform installation. Model settings belong to the Host
//! management store; release files and mutable platform facts are disjoint.
use crate::{
    GOOSE_SHA256, PIXI_SHA256, PackageError, PlatformPackageInventory, RUSTUP_INIT_SHA256, Result,
    UV_SHA256, archive,
    filesystem::{absolute, make_directories_writable, open_path},
    platform, validation,
};
use flate2::read::GzDecoder;
use rustix::fs::{RenameFlags, renameat_with};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
};

const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct PlatformInstallRequest {
    pub archive: PathBuf,
    pub sha256: String,
    pub install_root: PathBuf,
    /// Retained for API compatibility; supplying a model file is no longer supported.
    pub model_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformInstallReport {
    pub install_root: PathBuf,
    pub release_root: PathBuf,
    pub launcher: PathBuf,
    pub model_config: Option<PathBuf>,
    pub archive_sha256: String,
}

pub fn install_platform(request: &PlatformInstallRequest) -> Result<PlatformInstallReport> {
    install(
        request,
        GOOSE_SHA256,
        UV_SHA256,
        RUSTUP_INIT_SHA256,
        PIXI_SHA256,
    )
}

fn install(
    request: &PlatformInstallRequest,
    goose_sha256: &str,
    uv_sha256: &str,
    rustup_sha256: &str,
    pixi_sha256: &str,
) -> Result<PlatformInstallReport> {
    if request.sha256.len() != 64 || !request.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("archive SHA256 must be a trusted 64-digit digest"));
    }
    let install_root = absolute(&request.install_root)?;
    let parent = install_root
        .parent()
        .ok_or_else(|| invalid("installation has no parent"))?;
    let name = install_root
        .file_name()
        .ok_or_else(|| invalid("installation has no name"))?;
    let parent_fd = open_path(parent)?;
    let parent_identity = parent_fd.metadata()?;
    if !parent_identity.is_dir() {
        return Err(invalid("installation parent must be an existing directory"));
    }
    if fs::symlink_metadata(&install_root).is_ok() {
        return Err(PackageError::AlreadyExists);
    }
    if request.model_config.is_some() {
        return Err(invalid(
            "model configuration files are no longer supported; configure model services in WebUI Settings",
        ));
    }
    let pinned_parent = PathBuf::from(format!("/proc/self/fd/{}", parent_fd.as_raw_fd()));
    let staging = tempfile::Builder::new()
        .prefix(".anchor-install-")
        .tempdir_in(&pinned_parent)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    // The private copy pins the bytes checked by SHA256 even if the supplied
    // archive is concurrently replaced or modified after the read.
    let mut input = open_path(&request.archive)?;
    if !input.metadata()?.is_file() || input.metadata()?.len() > MAX_ARCHIVE_BYTES {
        return Err(invalid(
            "archive must be an ordinary file no larger than 8 GiB",
        ));
    }
    let mut snapshot = tempfile::tempfile()?;
    let bytes = std::io::copy(
        &mut Read::by_ref(&mut input).take(MAX_ARCHIVE_BYTES + 1),
        &mut snapshot,
    )?;
    if bytes > MAX_ARCHIVE_BYTES {
        return Err(invalid("archive exceeds 8 GiB"));
    }
    snapshot.seek(SeekFrom::Start(0))?;
    if archive::sha256(&mut snapshot)? != request.sha256.to_ascii_lowercase() {
        return Err(invalid("archive does not match the trusted SHA256"));
    }
    let release = staging.path().join("release");
    fs::create_dir(&release)?;
    unpack_platform(snapshot, &release)?;
    let runtime = release.join("anchor-runtime");
    validate_runtime(
        &runtime,
        goose_sha256,
        uv_sha256,
        rustup_sha256,
        pixi_sha256,
        false,
    )?;
    let config = staging.path().join("config");
    fs::create_dir(&config)?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
    for path in [
        "data",
        "data/home",
        "data/state",
        "data/catalog",
        "data/library",
        "data/workspaces",
    ] {
        let directory = staging.path().join(path);
        fs::create_dir(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        File::open(&directory)?.sync_all()?;
    }
    let mut readonly_release = ReadonlyRelease {
        root: &release,
        published: false,
    };
    make_release_readonly(&release)?;
    File::open(&config)?.sync_all()?;
    File::open(staging.path())?.sync_all()?;
    let current_parent = open_path(parent)?.metadata()?;
    if (current_parent.dev(), current_parent.ino())
        != (parent_identity.dev(), parent_identity.ino())
    {
        return Err(PackageError::Drift(parent.to_path_buf()));
    }
    if let Err(error) = renameat_with(
        &parent_fd,
        staging.path().file_name().unwrap(),
        &parent_fd,
        name,
        RenameFlags::NOREPLACE,
    ) {
        return Err(if error == rustix::io::Errno::EXIST {
            PackageError::AlreadyExists
        } else {
            PackageError::Io(error.into())
        });
    }
    readonly_release.published = true;
    let _ = staging.keep();
    parent_fd
        .sync_all()
        .map_err(|_| PackageError::PublicationUncertain(install_root.clone()))?;
    let release_root = install_root.join("release/anchor-runtime");
    Ok(PlatformInstallReport {
        launcher: release_root.join("bin/anchor-platform"),
        release_root,
        model_config: None,
        install_root,
        archive_sha256: request.sha256.to_ascii_lowercase(),
    })
}

fn invalid(message: impl Into<String>) -> PackageError {
    PackageError::Invalid(message.into())
}

fn unpack_platform(file: File, release: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut seen = BTreeSet::new();
    let mut total = 0_u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw = entry.path()?;
        let path = raw
            .to_str()
            .ok_or_else(|| invalid("archive path is not UTF-8"))?
            .trim_end_matches('/');
        let relative = if path == "anchor-runtime" {
            ""
        } else {
            path.strip_prefix("anchor-runtime/")
                .ok_or_else(|| invalid("archive must contain only anchor-runtime"))?
        };
        if !relative.is_empty() {
            validation::archive_path(relative)?;
        }
        if !seen.insert(path.to_owned()) {
            return Err(invalid("duplicate archive path"));
        }
        if seen.len() > 100_000 {
            return Err(invalid("platform archive exceeds 100000 entries"));
        }
        let kind = entry.header().entry_type();
        let mode = entry.header().mode()?;
        let size = entry.size();
        let destination = release.join(path);
        if kind.is_dir() && mode == 0o755 && size == 0 {
            fs::create_dir(&destination)?;
        } else if kind.is_file() && matches!(mode, 0o644 | 0o755) && size <= MAX_FILE_BYTES {
            total = total
                .checked_add(size)
                .ok_or_else(|| invalid("archive size overflow"))?;
            if total > MAX_ARCHIVE_BYTES {
                return Err(invalid("extracted platform exceeds 8 GiB"));
            }
            let mut target = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            if std::io::copy(&mut entry, &mut target)? != size {
                return Err(invalid("truncated archive file"));
            }
            target.set_permissions(fs::Permissions::from_mode(mode))?;
            target.sync_all()?;
        } else {
            return Err(invalid(
                "archive contains a link, special file, unsafe mode or oversized file",
            ));
        }
    }
    if !seen.contains("anchor-runtime") {
        return Err(invalid("platform archive root is missing"));
    }
    Ok(())
}

/// Check an installed platform against the same fixed runtime contract used at installation.
pub fn validate_platform_runtime(runtime: &Path) -> Result<()> {
    platform_plugin_inventory(runtime).map(|_| ())
}

/// Official Plugins from a release admitted by the installation validator.
/// Startup imports only these entries, never an arbitrary plugins directory.
pub fn platform_plugin_inventory(runtime: &Path) -> Result<Vec<crate::PluginInventory>> {
    open_path(runtime)?;
    validate_runtime(
        runtime,
        GOOSE_SHA256,
        UV_SHA256,
        RUSTUP_INIT_SHA256,
        PIXI_SHA256,
        true,
    )
    .map(|inventory| inventory.plugins)
}

fn validate_runtime(
    root: &Path,
    goose_sha256: &str,
    uv_sha256: &str,
    rustup_sha256: &str,
    pixi_sha256: &str,
    read_only: bool,
) -> Result<PlatformPackageInventory> {
    // Installed releases use this same validator. Reject links and special entries before reading files.
    let mut actual_files = BTreeSet::new();
    let mut actual_directories = BTreeSet::new();
    enumerate(root, root, &mut actual_files, &mut actual_directories)?;
    let manifest_file = File::open(root.join("runtime-manifest.json"))?;
    if !manifest_file.metadata()?.is_file() || manifest_file.metadata()?.len() > 16 * 1024 * 1024 {
        return Err(invalid(
            "runtime manifest must be an ordinary file no larger than 16 MiB",
        ));
    }
    let manifest: serde_json::Value = serde_json::from_reader(manifest_file)?;
    // Reject obsolete archives before decoding the required bootstrap inventory.
    if manifest.get("format").and_then(serde_json::Value::as_u64) != Some(3) {
        return Err(invalid(
            "installation requires the pinned format-3 platform archive",
        ));
    }
    let manifest: PlatformPackageInventory = serde_json::from_value(manifest)?;
    if manifest.kind != "platform"
        || manifest.goose_sha256 != goose_sha256
        || manifest.goose_version != crate::GOOSE_VERSION
    {
        return Err(invalid(
            "installation requires the pinned format-3 platform archive",
        ));
    }
    for (tool, path, digest, version, source) in [
        (
            &manifest.bootstrap.uv,
            "bin/uv",
            uv_sha256,
            crate::UV_VERSION,
            crate::UV_SOURCE,
        ),
        (
            &manifest.bootstrap.rustup_init,
            "bin/rustup-init",
            rustup_sha256,
            crate::RUSTUP_INIT_VERSION,
            crate::RUSTUP_INIT_SOURCE,
        ),
        (
            &manifest.bootstrap.pixi,
            "bin/pixi",
            pixi_sha256,
            crate::PIXI_VERSION,
            crate::PIXI_SOURCE,
        ),
    ] {
        if tool.path != path
            || tool.sha256 != digest
            || tool.version != version
            || tool.source != source
        {
            return Err(invalid(
                "platform bootstrap identity differs from the fixed pin",
            ));
        }
    }
    let mut declared = BTreeSet::from(["runtime-manifest.json".to_owned()]);
    let mut required_directories = BTreeSet::from([String::new()]);
    for resource in &manifest.files {
        validation::archive_path(&resource.path)?;
        for component in resource.path.split('/') {
            validation::resource_component(component)?;
        }
        if !matches!(
            resource.path.as_str(),
            "README.md"
                | "bin/anchor-runner-host"
                | "bin/anchor-net-relay"
                | "bin/anchor-sandbox-network"
                | "bin/anchor-platform"
                | "bin/goose"
                | "bin/uv"
                | "bin/rustup-init"
                | "bin/pixi"
        ) && platform::release_plugin_resource_mode(&resource.path).is_none()
            && !resource.path.starts_with("web/")
        {
            return Err(invalid(
                "platform inventory contains a non-platform resource",
            ));
        }
        if platform::release_plugin_resource_mode(&resource.path)
            .is_some_and(|mode| resource.mode != mode)
        {
            return Err(invalid("platform Plugin resource has an unexpected mode"));
        }
        if !declared.insert(resource.path.clone()) || !matches!(resource.mode, 0o644 | 0o755) {
            return Err(invalid("invalid or duplicate platform inventory file"));
        }
        let file = File::open(root.join(&resource.path))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.len() != resource.size
            || metadata.permissions().mode() & 0o7777
                != if read_only {
                    resource.mode & 0o555
                } else {
                    resource.mode
                }
        {
            return Err(invalid(
                "platform file type, mode or size differs from inventory",
            ));
        }
        let mut file = file;
        if archive::sha256(&mut file)? != resource.sha256 {
            return Err(invalid("platform file differs from inventory SHA256"));
        }
        if resource.mode == 0o755 {
            if validation::elf(&root.join(&resource.path))? != manifest.platform {
                return Err(invalid("platform ELF identity differs from inventory"));
            }
            let runtime = validation::elf_runtime(&root.join(&resource.path))?;
            if matches!(
                resource.path.as_str(),
                "bin/goose" | "bin/uv" | "bin/rustup-init" | "bin/pixi"
            ) && (runtime.interpreter.is_some() || !runtime.needed.is_empty())
            {
                return Err(invalid(
                    "Goose and bootstrap must be static musl executables",
                ));
            }
            if !manifest.executables.iter().any(|executable| {
                executable.path == resource.path
                    && executable.sha256 == resource.sha256
                    && executable.elf == manifest.platform
                    && executable.runtime == runtime
            }) {
                return Err(invalid(
                    "platform executable metadata differs from inventory",
                ));
            }
        } else {
            validation::resource_extension(&resource.path)?;
            validation::resource_content(&root.join(&resource.path), &resource.path)?;
        }
        let mut path = resource.path.as_str();
        while let Some((parent, _)) = path.rsplit_once('/') {
            required_directories.insert(parent.to_owned());
            path = parent;
        }
    }
    for (path, digest) in [
        ("bin/goose", goose_sha256),
        ("bin/uv", uv_sha256),
        ("bin/rustup-init", rustup_sha256),
        ("bin/pixi", pixi_sha256),
    ] {
        if !manifest
            .files
            .iter()
            .any(|file| file.path == path && file.sha256 == digest && file.mode == 0o755)
        {
            return Err(invalid(
                "required fixed platform executable is absent from inventory",
            ));
        }
    }
    for path in [
        "bin/anchor-runner-host",
        "bin/anchor-net-relay",
        "bin/anchor-sandbox-network",
        "bin/anchor-platform",
    ] {
        if !manifest
            .files
            .iter()
            .any(|file| file.path == path && file.mode == 0o755)
        {
            return Err(invalid("required platform executable is absent"));
        }
    }
    if !declared.contains("web/index.html") {
        return Err(invalid("compiled platform WebUI is missing"));
    }
    if manifest.executables.len()
        != manifest
            .files
            .iter()
            .filter(|file| file.mode == 0o755)
            .count()
    {
        return Err(invalid(
            "platform executable inventory has unexpected entries",
        ));
    }
    for (path, mode) in platform::release_plugin_resources() {
        if !manifest
            .files
            .iter()
            .any(|file| file.path == path && file.mode == mode)
        {
            return Err(invalid("required official Plugin resource is absent"));
        }
    }
    if manifest.plugins != platform::release_plugin_inventory(root)? {
        return Err(invalid("platform Plugins differ from admitted inventory"));
    }
    if actual_files != declared || actual_directories != required_directories {
        return Err(invalid(
            "platform archive contains undeclared files or directories",
        ));
    }
    Ok(manifest)
}

fn enumerate(
    root: &Path,
    path: &Path,
    files: &mut BTreeSet<String>,
    directories: &mut BTreeSet<String>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let relative = path
        .strip_prefix(root)
        .unwrap()
        .to_str()
        .ok_or_else(|| invalid("non-UTF8 platform path"))?
        .to_owned();
    if metadata.is_dir() {
        directories.insert(relative);
        for entry in fs::read_dir(path)? {
            enumerate(root, &entry?.path(), files, directories)?;
        }
    } else if metadata.is_file() {
        files.insert(relative);
    } else {
        return Err(invalid("platform contains a special file or link"));
    }
    Ok(())
}

fn make_release_readonly(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            make_release_readonly(&entry?.path())?;
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o555))?;
    } else {
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(metadata.permissions().mode() & 0o555),
        )?;
    }
    File::open(path)?.sync_all()?;
    Ok(())
}

struct ReadonlyRelease<'a> {
    root: &'a Path,
    published: bool,
}

impl Drop for ReadonlyRelease<'_> {
    fn drop(&mut self) {
        if !self.published {
            // TempDir cleanup must remain possible if publication or a
            // durability operation fails after the tree becomes read-only.
            let _ = make_directories_writable(self.root);
        }
    }
}

/// Build a Host command from installation-owned roots. Model and runtime
/// settings live in the Host store; no caller configuration is inherited.
pub fn platform_command(launcher: &Path) -> Result<Command> {
    platform_command_with_listen(launcher, "127.0.0.1:8077")
}

pub fn platform_command_with_listen(launcher: &Path, listen: &str) -> Result<Command> {
    let address = listen
        .parse::<std::net::SocketAddr>()
        .map_err(|_| invalid("platform listen address must be a loopback IP and port"))?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(invalid(
            "platform listen address must be a loopback IP and nonzero port",
        ));
    }
    let launcher = absolute(launcher)?;
    let runtime = launcher
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("launcher has no release root"))?;
    if launcher
        .file_name()
        .is_none_or(|name| name != "anchor-platform")
        || runtime
            .file_name()
            .is_none_or(|name| name != "anchor-runtime")
    {
        return Err(invalid(
            "launcher must be release/anchor-runtime/bin/anchor-platform",
        ));
    }
    let install_root = runtime
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("launcher has no install root"))?;
    if runtime
        .parent()
        .and_then(Path::file_name)
        .is_none_or(|name| name != "release")
    {
        return Err(invalid(
            "launcher must be inside the installed release directory",
        ));
    }
    open_path(runtime)?;
    // Old installations may retain config/model.env. Leave its bytes untouched;
    // only the Host-managed model store supplies model services and credentials.
    let mut command = Command::new(runtime.join("bin/anchor-runner-host"));
    command
        .arg("serve")
        .current_dir(runtime)
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", install_root.join("data/home"))
        .env("GOOSE_TELEMETRY_OFF", "1")
        .env("ANCHOR_GOOSE_BINARY", runtime.join("bin/goose"))
        .env("ANCHOR_GOOSE_BINARY_SHA256", GOOSE_SHA256)
        .env("ANCHOR_GOOSE_LOCAL_NETWORK", "1")
        .env("ANCHOR_RUNNER_LISTEN", address.to_string())
        .env("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh,git")
        .env("ANCHOR_MODEL_DIRECT", "0");
    for (key, path) in [
        ("ANCHOR_RUNNER_STATE_ROOT", "data/state"),
        ("ANCHOR_RUNNER_CATALOG_ROOT", "data/catalog"),
        ("ANCHOR_RUNNER_LIBRARY_ROOT", "data/library"),
        ("ANCHOR_RUNNER_WORKSPACE_ROOT", "data/workspaces"),
    ] {
        command.env(key, install_root.join(path));
    }
    command.env("ANCHOR_RUNNER_WEB_ROOT", runtime.join("web"));
    Ok(command)
}

#[cfg(test)]
mod tests;
