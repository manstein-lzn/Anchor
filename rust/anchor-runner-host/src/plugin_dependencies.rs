//! Landing a Plugin's declared dependencies into the shared store and its view.
//!
//! The order is deliberate. Fetch first (only the artifacts whose bytes hash to
//! the approved lock), unpack each `(package, version)` into the shared store
//! once, create the view, and *then* let the caller publish the Plugin. A failed
//! solve, fetch, unpack or verification therefore leaves no Plugin installed and
//! no existing view changed — the deployment keeps exactly the state the human
//! last approved.
//!
//! Every step that runs foreign bytes runs inside the build sandbox, which is
//! constructed without network authority: the unpacking and the `uv venv` cannot
//! reach the network even if a later edit asks them to. The only network use is
//! the fetch, on the host, against the deployment's authorized sources.

use std::path::{Path, PathBuf};

use anchor_graph_host::{
    DEPENDENCIES_DIRECTORY, plugin_view_path, resolved_library_root, store_root,
};
use anchor_runtime::ReadOnlyInput;

use crate::dependency_provision::{
    DependencyAccess, DependencyProvisioner, LockedPackage, ProvisionError, ResolvedRequirements,
    authority, fetch_wheelhouse_with_access, normalize_name, run_step_with_access,
};
use crate::plugin_view::{
    VIEW_CONFIGURATION, referenced_store_directories, site_packages_directories,
};

/// What one landed declaration produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LandedView {
    pub(crate) view: PathBuf,
    pub(crate) packages: usize,
    /// True when the view already existed and nothing was rebuilt.
    pub(crate) reused: bool,
}

/// Publish the shared store and the view for one Plugin declaration.
#[cfg(test)]
pub(crate) async fn land(
    provisioner: &DependencyProvisioner,
    library_root: &Path,
    plugin: &str,
    declaration_digest: &str,
    resolved: &ResolvedRequirements,
) -> Result<LandedView, ProvisionError> {
    land_with_access(
        provisioner,
        library_root,
        plugin,
        declaration_digest,
        resolved,
        &DependencyAccess::default(),
    )
    .await
}

pub(crate) async fn land_with_access(
    provisioner: &DependencyProvisioner,
    library_root: &Path,
    plugin: &str,
    declaration_digest: &str,
    resolved: &ResolvedRequirements,
    access: &DependencyAccess,
) -> Result<LandedView, ProvisionError> {
    let root = resolved_library_root(library_root);
    land_at(
        provisioner,
        &root,
        &store_root(&root),
        &plugin_view_path(&root, plugin, declaration_digest),
        plugin,
        resolved,
        access,
        None,
    )
    .await
}

/// Candidate views use the same Library package bytes as a formal Plugin. Their exact references are published under the writer lock.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_candidate(
    provisioner: &DependencyProvisioner,
    library_root: &Path,
    scope: &anchor_library::DevelopmentScope,
    plugin: &str,
    declaration_digest: &str,
    resolved: &ResolvedRequirements,
    access: &DependencyAccess,
) -> Result<LandedView, ProvisionError> {
    let root = resolved_library_root(library_root);
    let view = scope
        .root()
        .join("candidates")
        .join(plugin)
        .join(declaration_digest)
        .join(resolved_view_key(resolved))
        .join("view");
    land_at(
        provisioner,
        &root,
        &store_root(&root),
        &view,
        plugin,
        resolved,
        access,
        Some(scope),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn prepare_legacy_fixture(
    provisioner: &DependencyProvisioner,
    library_root: &Path,
    scope: &anchor_library::DevelopmentScope,
    project_key: &str,
    resolved: &ResolvedRequirements,
    access: &DependencyAccess,
) -> Result<LandedView, ProvisionError> {
    let root = resolved_library_root(library_root);
    let view = scope
        .root()
        .join("projects")
        .join(project_key)
        .join(resolved_view_key(resolved))
        .join("view");
    land_at(
        provisioner,
        &root,
        &store_root(&root),
        &view,
        project_key,
        resolved,
        access,
        Some(scope),
    )
    .await
}

/// A temporary view identifies the actual solve and the shared interpreter;
/// changing the base build must not silently reuse an older Python view.
pub(crate) fn resolved_view_key(resolved: &ResolvedRequirements) -> String {
    anchor_library::DependencyViewIdentity::key_for(
        &resolved.lock_digest,
        &resolved.interpreter_path,
        &resolved.interpreter_digest,
    )
}

#[allow(clippy::too_many_arguments)]
async fn land_at(
    provisioner: &DependencyProvisioner,
    library_root: &Path,
    store: &Path,
    view: &Path,
    plugin: &str,
    resolved: &ResolvedRequirements,
    access: &DependencyAccess,
    scope: Option<&anchor_library::DevelopmentScope>,
) -> Result<LandedView, ProvisionError> {
    access.check()?;
    let config = &provisioner.config;
    let current_interpreter = std::fs::canonicalize(&config.python).map_err(failure)?;
    if current_interpreter != resolved.interpreter_path
        || file_digest(&current_interpreter)? != resolved.interpreter_digest
    {
        return Err(failure(
            "the shared Python interpreter changed after dependency resolution; prepare again with its current identity",
        ));
    }
    let library_root = library_root.to_path_buf();
    let store = store.to_path_buf();
    let view = view.to_path_buf();

    // One writer for the shared store and the views, held across the whole
    // landing so two installs cannot interleave their renames.
    let dependencies_root = library_root.join(DEPENDENCIES_DIRECTORY);
    let lock_root = dependencies_root.clone();
    let _lock = write_lock(&lock_root, access).await?;
    if let Some(scope) = scope {
        scope
            .register()
            .map_err(|error| ProvisionError::Failed(error.to_string()))?;
    }
    // Another install may have landed the same declaration while we waited.
    if let Some(landed) =
        reuse_existing(provisioner, plugin, &view, &store, resolved, access).await?
    {
        if let Some(scope) = scope {
            scope
                .record_view(&view)
                .map_err(|error| ProvisionError::Failed(error.to_string()))?;
        }
        return Ok(landed);
    }

    let staging_parent = config.build_root.join("plugin-dependencies");
    std::fs::create_dir_all(&staging_parent)
        .map_err(|error| ProvisionError::Failed(error.to_string()))?;
    let staging_directory = tempfile::Builder::new()
        .prefix("prepare-")
        .tempdir_in(&staging_parent)
        .map_err(|error| ProvisionError::Failed(error.to_string()))?;
    let staging = staging_directory.path();
    let workspace = staging.join("run");
    let input = workspace.join("input");
    let wheelhouse = staging.join("wheelhouse");
    std::fs::create_dir_all(&input)
        .and_then(|_| std::fs::create_dir_all(&workspace))
        .map_err(|error| ProvisionError::Failed(format!("dependency staging failed: {error}")))?;

    // A verified stored artifact needs neither another download nor another
    // unpack. Older stores without provenance are compared after a real build
    // before they acquire this marker; their bytes are never silently trusted.
    let mut needed = Vec::new();
    for package in &resolved.packages {
        if !stored_artifact_matches(&store, package, resolved)? {
            needed.push(package.clone());
        }
    }
    let artifacts = fetch_wheelhouse_with_access(config, &needed, &wheelhouse, access).await?;
    for package in &needed {
        write_requirement(&input, package)?;
    }

    let uv = config.verified_uv()?;
    let uv_dir = uv
        .parent()
        .ok_or_else(|| ProvisionError::Failed("the uv binary has no directory".to_owned()))?
        .to_path_buf();
    let interpreter = std::fs::canonicalize(&config.python).map_err(|error| {
        ProvisionError::Failed(format!(
            "the configured interpreter {} is unavailable: {error}",
            config.python.display()
        ))
    })?;
    // The wheelhouse is a delivery mechanism, not an authority: it is mounted
    // read-only and the install still runs with `--require-hashes`.
    let mut grants = vec![
        ReadOnlyInput::new(&uv, &uv),
        ReadOnlyInput::new(&wheelhouse, &wheelhouse),
    ];
    grants.extend(
        crate::plugin_view::interpreter_prefix_grants(&resolved.interpreter_prefix)
            .map_err(ProvisionError::Failed)?,
    );
    let build = authority(&provisioner.sandbox, grants, &[uv_dir])?;

    let uv_arg = uv.to_string_lossy().into_owned();
    let python_arg = interpreter.to_string_lossy().into_owned();
    let wheelhouse_arg = wheelhouse.to_string_lossy().into_owned();
    for package in &needed {
        let name = normalize_name(&package.name);
        run_step_with_access(
            config,
            &build,
            &workspace,
            &[
                uv_arg.clone(),
                "pip".into(),
                "install".into(),
                "--no-deps".into(),
                "--no-index".into(),
                "--find-links".into(),
                wheelhouse_arg.clone(),
                "--require-hashes".into(),
                "--python".into(),
                python_arg.clone(),
                "--target".into(),
                format!("/workspace/store/{name}/{}", package.version),
                "--requirement".into(),
                format!("/workspace/input/{name}.txt"),
            ],
            &format!("unpacking `{}=={}`", package.name, package.version),
            access,
        )
        .await?;
    }
    // A view is created here and published at its final path, so the interpreter
    // it names is the deployment's own — one Python, many views.
    run_step_with_access(
        config,
        &build,
        &workspace,
        &[
            uv_arg,
            "venv".into(),
            "--python".into(),
            python_arg,
            "/workspace/view".into(),
        ],
        "creating the Plugin dependency view",
        access,
    )
    .await?;

    assert_interpreter(&workspace.join("view"), &interpreter)?;
    pointer_into_store(&workspace, &store, &view, &resolved.packages)?;
    publish_store(&workspace, &store, &library_root, &needed)?;
    for package in &needed {
        let artifact = artifacts
            .iter()
            .find(|artifact| {
                normalize_name(&artifact.name) == normalize_name(&package.name)
                    && artifact.version == package.version
            })
            .ok_or_else(|| {
                ProvisionError::Failed("landed package has no verified source artifact".into())
            })?;
        record_stored_artifact(&store, package, &artifact.sha256, resolved)?;
    }
    publish_view(&workspace, &view)?;

    // The view is only proven where it now lives: a Python environment can carry
    // absolute paths that break the moment it moves.
    if let Err(error) = verify_view(provisioner, &store, &view, resolved, access).await {
        // Nothing else references the view yet — the Plugin is published after
        // this returns — so an unusable one is removed rather than left to look
        // installed.
        let _ = std::fs::remove_dir_all(&view);
        return Err(error);
    }
    if let Some(scope) = scope {
        scope
            .record_view(&view)
            .map_err(|error| ProvisionError::Failed(error.to_string()))?;
    }
    Ok(LandedView {
        view,
        packages: resolved.packages.len(),
        reused: false,
    })
}

/// Reuse the view this declaration already has, or say why it cannot be reused.
///
/// A view is content-addressed by the *declaration*, and a declaration need not
/// pin versions (`idna>=3`). What a human approves, and what the form and the
/// answer report, is the *resolved set* — so an existing view is only reused when
/// it really holds that set. It is never rewritten in place: that could change an
/// environment a Run is already using, and switching views is a later slice.
async fn reuse_existing(
    provisioner: &DependencyProvisioner,
    plugin: &str,
    view: &Path,
    store: &Path,
    resolved: &ResolvedRequirements,
    access: &DependencyAccess,
) -> Result<Option<LandedView>, ProvisionError> {
    if !view.exists() {
        return Ok(None);
    }
    if !complete_view(view, store) {
        // The view is published by a single rename, so it is either the whole
        // environment or a leftover somebody has to look at.
        return Err(ProvisionError::Failed(format!(
            "Plugin `{plugin}` view {} exists but is not a usable environment; remove it deliberately and install again",
            view.display()
        )));
    }
    for package in &resolved.packages {
        let provenance = store
            .join(normalize_name(&package.name))
            .join(&package.version)
            .join(STORED_ARTIFACT);
        if std::fs::symlink_metadata(&provenance).is_ok()
            && !stored_artifact_matches(store, package, resolved)?
        {
            return Err(failure(format!(
                "Plugin `{plugin}` view references a package that does not match the resolved artifact or interpreter"
            )));
        }
    }
    verify_view(provisioner, store, view, resolved, access)
        .await
        .map_err(|error| {
            ProvisionError::Failed(format!(
                "Plugin `{plugin}` already has the view {} for this declaration, but it does not match the set this declaration resolves to: {error}. The platform does not switch an existing view in place — pin the versions in the declaration, or remove {} deliberately and install again",
                view.display(),
                view.display()
            ))
        })?;
    Ok(Some(LandedView {
        view: view.to_path_buf(),
        packages: resolved.packages.len(),
        reused: true,
    }))
}

/// True when the view is complete: a real environment whose pointers all resolve.
fn complete_view(view: &Path, store: &Path) -> bool {
    view.join(VIEW_CONFIGURATION).is_file()
        && view.join("bin/python").exists()
        && store.exists()
        && referenced_store_directories(view, store).is_ok()
}

/// A view must run on the deployment's one interpreter, not on a copy of it.
///
/// This is invariant 3 of the standard: many views, one Python. A `uv venv` that
/// somehow produced its own interpreter would look like a working view while
/// quietly doubling every deployment's Python.
fn assert_interpreter(view: &Path, interpreter: &Path) -> Result<(), ProvisionError> {
    let python = view.join("bin/python");
    let resolved = python.canonicalize().map_err(|error| {
        ProvisionError::Failed(format!(
            "the created view has no usable interpreter at {}: {error}",
            python.display()
        ))
    })?;
    if resolved != interpreter {
        return Err(ProvisionError::Failed(format!(
            "the created view runs on {} instead of the deployment interpreter {}",
            resolved.display(),
            interpreter.display()
        )));
    }
    Ok(())
}

/// Write one package's pinned requirement, in the shape `uv` reads hashes from.
///
/// `uv` (like pip) only accepts `--hash` options on a continuation line, so the
/// requirement line ends with a backslash exactly as `uv pip compile` writes it.
fn write_requirement(input: &Path, package: &LockedPackage) -> Result<(), ProvisionError> {
    let name = normalize_name(&package.name);
    let path = input.join(format!("{name}.txt"));
    let mut hashes = package.hashes.clone();
    hashes.sort();
    let mut text = format!("{}=={}", package.name, package.version);
    for hash in &hashes {
        text.push_str(&format!(" \\\n    --hash=sha256:{hash}"));
    }
    text.push('\n');
    std::fs::write(&path, text).map_err(|error| {
        ProvisionError::Failed(format!("{} could not be written: {error}", path.display()))
    })
}

/// Point every installed entry at its final `(package, version)` directory.
///
/// The target is the final store path, not the staging one: the view is renamed
/// into place afterwards, so the pointers must already be right when it lands.
fn pointer_into_store(
    workspace: &Path,
    store: &Path,
    final_view: &Path,
    packages: &[LockedPackage],
) -> Result<(), ProvisionError> {
    let staged_view = workspace.join("view");
    let site_packages = site_packages_directories(&staged_view)
        .map_err(ProvisionError::Failed)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            ProvisionError::Failed("the created view has no site-packages directory".to_owned())
        })?;
    for package in packages {
        let name = normalize_name(&package.name);
        let staged = workspace.join("store").join(&name).join(&package.version);
        let content = if staged.is_dir() {
            staged.clone()
        } else {
            store.join(&name).join(&package.version)
        };
        // `uv pip install --target` leaves its own lock file behind; it is not
        // part of the distribution.
        let _ = std::fs::remove_file(staged.join(".lock"));
        let entries = std::fs::read_dir(&content).map_err(|error| {
            ProvisionError::Failed(format!("{} is not readable: {error}", content.display()))
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                ProvisionError::Failed(format!("{} is not readable: {error}", staged.display()))
            })?;
            let name_of_entry = entry.file_name();
            if name_of_entry == STORED_ARTIFACT || name_of_entry == ".lock" {
                continue;
            }
            let target = store
                .join(&name)
                .join(&package.version)
                .join(&name_of_entry);
            if name_of_entry == "bin" && entry.path().is_dir() {
                install_console_scripts(
                    &entry.path(),
                    &target,
                    &staged_view.join("bin"),
                    &final_view.join("bin/python"),
                )?;
                continue;
            }
            link_entry(
                workspace,
                store,
                &entry.path(),
                &target,
                &site_packages.join(&name_of_entry),
            )?;
        }
    }
    Ok(())
}

fn failure(error: impl std::fmt::Display) -> ProvisionError {
    ProvisionError::Failed(error.to_string())
}

/// Keep normal package directories as one pointer. When two distributions share
/// a namespace directory, merge only its pointers; conflicting file bytes fail.
fn link_entry(
    workspace: &Path,
    store: &Path,
    source: &Path,
    target: &Path,
    link: &Path,
) -> Result<(), ProvisionError> {
    let existing = match std::fs::symlink_metadata(link) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return std::os::unix::fs::symlink(target, link).map_err(failure);
        }
        Err(error) => return Err(failure(error)),
    };
    let previous = if existing.file_type().is_symlink() {
        let destination = std::fs::read_link(link).map_err(failure)?;
        if destination == target {
            return Ok(());
        }
        if destination.exists() {
            destination
        } else {
            workspace
                .join("store")
                .join(destination.strip_prefix(store).map_err(failure)?)
        }
    } else {
        link.to_path_buf()
    };
    if !source.is_dir() || !previous.is_dir() {
        if source.is_file()
            && previous.is_file()
            && std::fs::read(source).map_err(failure)?
                == std::fs::read(&previous).map_err(failure)?
        {
            return Ok(());
        }
        return Err(failure(format!(
            "distributions contain conflicting entries at {}",
            link.display()
        )));
    }
    if existing.file_type().is_symlink() {
        let old_target = std::fs::read_link(link).map_err(failure)?;
        std::fs::remove_file(link).map_err(failure)?;
        std::fs::create_dir(link).map_err(failure)?;
        for entry in std::fs::read_dir(&previous).map_err(failure)? {
            let entry = entry.map_err(failure)?;
            std::os::unix::fs::symlink(
                old_target.join(entry.file_name()),
                link.join(entry.file_name()),
            )
            .map_err(failure)?;
        }
    }
    for entry in std::fs::read_dir(source).map_err(failure)? {
        let entry = entry.map_err(failure)?;
        link_entry(
            workspace,
            store,
            &entry.path(),
            &target.join(entry.file_name()),
            &link.join(entry.file_name()),
        )?;
    }
    Ok(())
}

/// uv's target install writes script files with the base Python shebang. A view
/// launcher chooses its own interpreter while keeping the distribution script
/// bytes only in the shared store.
fn install_console_scripts(
    source: &Path,
    target: &Path,
    bin: &Path,
    python: &Path,
) -> Result<(), ProvisionError> {
    use std::os::unix::fs::PermissionsExt;
    fn quoted(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }
    for entry in std::fs::read_dir(source).map_err(failure)? {
        let entry = entry.map_err(failure)?;
        if !entry.file_type().map_err(failure)?.is_file() {
            return Err(failure("distribution scripts must be regular files"));
        }
        let name = entry.file_name();
        let destination = bin.join(&name);
        if std::fs::symlink_metadata(&destination).is_ok() {
            return Err(failure(format!(
                "distributions contain conflicting console scripts at {}",
                destination.display()
            )));
        }
        let bytes = std::fs::read(entry.path()).map_err(failure)?;
        let shebang = bytes
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        if (shebang.starts_with(b"#!") && String::from_utf8_lossy(shebang).contains("python"))
            || generated_console_script_body(&bytes).is_some()
        {
            std::fs::write(
                &destination,
                format!(
                    "#!/bin/sh\nexec {} {} \"$@\"\n",
                    quoted(python),
                    quoted(&target.join(name))
                ),
            )
            .map_err(failure)?;
            std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755))
                .map_err(failure)?;
        } else {
            std::os::unix::fs::symlink(target.join(name), destination).map_err(failure)?;
        }
    }
    Ok(())
}

/// Move each staged `(package, version)` into the shared store exactly once.
fn publish_store(
    workspace: &Path,
    store: &Path,
    library_root: &Path,
    packages: &[LockedPackage],
) -> Result<(), ProvisionError> {
    for package in packages {
        let name = normalize_name(&package.name);
        let staged = workspace.join("store").join(&name).join(&package.version);
        let destination = store.join(&name).join(&package.version);
        if destination.exists() {
            // Already stored. The layout says one (package, version) is one byte
            // set, so a different set of bytes under the same name and version is
            // refused rather than silently kept.
            if !same_tree(&staged, &destination)? {
                return Err(ProvisionError::Failed(format!(
                    "the shared store already holds `{}=={}` with different bytes than this source delivered; the store is keyed by (package, version), so it cannot hold both",
                    package.name, package.version
                )));
            }
            continue;
        }
        let parent = destination
            .parent()
            .expect("a package directory has a parent");
        std::fs::create_dir_all(parent).map_err(|error| {
            ProvisionError::Failed(format!("{} is unusable: {error}", parent.display()))
        })?;
        std::fs::rename(&staged, &destination).map_err(|error| {
            ProvisionError::Failed(format!(
                "`{}=={}` could not be moved into the shared store: {error} (the build root {} and the library root {} must be on one filesystem)",
                package.name,
                package.version,
                workspace.display(),
                library_root.display()
            ))
        })?;
    }
    Ok(())
}

/// Metadata a package installer writes about the installation rather than from
/// the distribution: `uv` records a timestamp in `uv_cache.json`, and `RECORD`
/// lists the files it just wrote. Comparing them would refuse a legitimate
/// rebuild of the same wheel over a difference that carries no package bytes.
const INSTALLER_METADATA: &[&str] = &[
    ".lock",
    "RECORD",
    "INSTALLER",
    "REQUESTED",
    "uv_cache.json",
    "direct_url.json",
    STORED_ARTIFACT,
];

fn is_installer_metadata(relative: &Path) -> bool {
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !INSTALLER_METADATA.contains(&name) {
        return false;
    }
    name == ".lock"
        || (name == STORED_ARTIFACT && relative.parent() == Some(Path::new("")))
        || relative
            .parent()
            .and_then(Path::extension)
            .is_some_and(|extension| extension == "dist-info")
}

const STORED_ARTIFACT: &str = ".anchor-package.json";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredArtifact {
    format: u32,
    name: String,
    version: String,
    artifact_digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    verified_artifact_digests: Vec<String>,
    interpreter_digest: String,
    bytes_digest: String,
}

fn read_stored_artifact(path: &Path) -> Result<Option<StoredArtifact>, ProvisionError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ProvisionError::Failed(error.to_string())),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16 * 1024 {
        return Err(ProvisionError::Failed(
            "stored package provenance is not a bounded regular file".into(),
        ));
    }
    let fact = serde_json::from_slice(
        &std::fs::read(path).map_err(|error| ProvisionError::Failed(error.to_string()))?,
    )
    .map_err(|error| {
        ProvisionError::Failed(format!("stored package provenance is unreadable: {error}"))
    })?;
    Ok(Some(fact))
}

fn stored_artifact_matches(
    store: &Path,
    package: &LockedPackage,
    resolved: &ResolvedRequirements,
) -> Result<bool, ProvisionError> {
    let path = store
        .join(normalize_name(&package.name))
        .join(&package.version)
        .join(STORED_ARTIFACT);
    let Some(fact) = read_stored_artifact(&path)? else {
        return Ok(false);
    };
    if fact.format != 1
        || fact.name != normalize_name(&package.name)
        || fact.version != package.version
    {
        return Err(ProvisionError::Failed(
            "stored package provenance identity changed".into(),
        ));
    }
    if fact.bytes_digest != package_bytes_digest(path.parent().expect("stored package directory"))?
    {
        return Err(failure(
            "shared package bytes changed after their verified publication",
        ));
    }
    Ok(resolved.interpreter_digest == fact.interpreter_digest
        && (package.hashes.contains(&fact.artifact_digest)
            || fact
                .verified_artifact_digests
                .iter()
                .any(|hash| package.hashes.contains(hash))))
}

fn record_stored_artifact(
    store: &Path,
    package: &LockedPackage,
    artifact_digest: &str,
    resolved: &ResolvedRequirements,
) -> Result<(), ProvisionError> {
    use std::io::Write;
    let directory = store
        .join(normalize_name(&package.name))
        .join(&package.version);
    let bytes_digest = package_bytes_digest(&directory)?;
    let mut verified_artifact_digests = Vec::new();
    if let Some(previous) = read_stored_artifact(&directory.join(STORED_ARTIFACT))? {
        if previous.format != 1
            || previous.name != normalize_name(&package.name)
            || previous.version != package.version
            || previous.bytes_digest != bytes_digest
        {
            return Err(failure(
                "shared package provenance changed before publication",
            ));
        }
        if previous.interpreter_digest == resolved.interpreter_digest {
            verified_artifact_digests.push(previous.artifact_digest);
            verified_artifact_digests.extend(previous.verified_artifact_digests);
        }
    }
    verified_artifact_digests.sort();
    verified_artifact_digests.dedup();
    verified_artifact_digests.retain(|digest| digest != artifact_digest);
    let fact = StoredArtifact {
        format: 1,
        name: normalize_name(&package.name),
        version: package.version.clone(),
        artifact_digest: artifact_digest.into(),
        verified_artifact_digests,
        interpreter_digest: resolved.interpreter_digest.clone(),
        bytes_digest,
    };
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)
        .map_err(|error| ProvisionError::Failed(error.to_string()))?;
    temporary
        .write_all(
            &serde_json::to_vec(&fact)
                .map_err(|error| ProvisionError::Failed(error.to_string()))?,
        )
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| ProvisionError::Failed(error.to_string()))?;
    temporary
        .persist(directory.join(STORED_ARTIFACT))
        .map_err(|error| ProvisionError::Failed(error.to_string()))?;
    std::fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| ProvisionError::Failed(error.to_string()))
}

fn file_digest(path: &Path) -> Result<String, ProvisionError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(failure)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let bytes = file.read(&mut buffer).map_err(failure)?;
        if bytes == 0 {
            break;
        }
        digest.update(&buffer[..bytes]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Verification metadata only: it does not name an environment or create
/// another storage key. Package directories remain `(package, version)`.
fn package_bytes_digest(root: &Path) -> Result<String, ProvisionError> {
    use sha2::{Digest, Sha256};
    let mut pending = vec![root.to_owned()];
    let mut files = std::collections::BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(failure)? {
            let entry = entry.map_err(failure)?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("stored package child")
                .to_owned();
            if is_installer_metadata(&relative) {
                continue;
            }
            let kind = entry.file_type().map_err(failure)?;
            if kind.is_dir() {
                pending.push(path);
                files.insert(relative, None);
            } else if kind.is_file() {
                files.insert(relative, Some(file_digest(&path)?));
            } else {
                return Err(failure("shared package contains a symlink or special file"));
            }
        }
    }
    let mut digest = Sha256::new();
    for (relative, content) in files {
        let path = relative.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path);
        match content {
            Some(content) => {
                digest.update(b"file\0");
                digest.update(content.as_bytes());
            }
            None => digest.update(b"directory\0"),
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Whether two stored trees hold the same distribution bytes.
///
/// The installer's own metadata is ignored on both sides (see
/// [`INSTALLER_METADATA`]). A uv-generated entry-point script may also carry a
/// different base Python shebang: its view launcher selects the interpreter,
/// and its declared entry point and complete script body must still match.
fn same_tree(left: &Path, right: &Path) -> Result<bool, ProvisionError> {
    fn entries(
        root: &Path,
    ) -> Result<std::collections::BTreeMap<PathBuf, PathBuf>, ProvisionError> {
        let mut found = std::collections::BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let listing = std::fs::read_dir(&directory).map_err(|error| {
                ProvisionError::Failed(format!("{} is not readable: {error}", directory.display()))
            })?;
            for entry in listing {
                let entry = entry.map_err(|error| {
                    ProvisionError::Failed(format!(
                        "{} is not readable: {error}",
                        directory.display()
                    ))
                })?;
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .expect("a walked path is under its root")
                    .to_path_buf();
                if is_installer_metadata(&relative) {
                    continue;
                }
                let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                    ProvisionError::Failed(format!("{} is not readable: {error}", path.display()))
                })?;
                if metadata.is_dir() {
                    pending.push(path);
                    found.insert(relative, PathBuf::new());
                } else {
                    found.insert(relative, path);
                }
            }
        }
        Ok(found)
    }
    let left_scripts = generated_console_scripts(left)?;
    let right_scripts = generated_console_scripts(right)?;
    let left = entries(left)?;
    let right = entries(right)?;
    if left.keys().ne(right.keys()) {
        return Ok(false);
    }
    for (relative, left_path) in &left {
        let right_path = right.get(relative).expect("the key sets are equal");
        if left_path.as_os_str().is_empty() || right_path.as_os_str().is_empty() {
            continue;
        }
        let left_bytes = std::fs::read(left_path).map_err(|error| {
            ProvisionError::Failed(format!("{} is not readable: {error}", left_path.display()))
        })?;
        let right_bytes = std::fs::read(right_path).map_err(|error| {
            ProvisionError::Failed(format!("{} is not readable: {error}", right_path.display()))
        })?;
        if left_bytes != right_bytes {
            if !left_scripts.contains(relative) || !right_scripts.contains(relative) {
                return Ok(false);
            }
            match (
                generated_console_script_body(&left_bytes),
                generated_console_script_body(&right_bytes),
            ) {
                (Some(left), Some(right)) if left == right => {}
                _ => return Ok(false),
            }
        }
    }
    Ok(true)
}

fn generated_console_scripts(
    root: &Path,
) -> Result<std::collections::BTreeSet<PathBuf>, ProvisionError> {
    let mut scripts = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(root).map_err(failure)? {
        let path = entry.map_err(failure)?.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "dist-info")
        {
            continue;
        }
        let text = match std::fs::read_to_string(path.join("entry_points.txt")) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(failure(error)),
        };
        let mut console = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') && line.ends_with(']') {
                console = matches!(line, "[console_scripts]" | "[gui_scripts]");
            } else if console
                && !line.starts_with(['#', ';'])
                && let Some((name, _)) = line.split_once('=')
            {
                let name = name.trim();
                if Path::new(name).file_name() == Some(std::ffi::OsStr::new(name)) {
                    scripts.insert(Path::new("bin").join(name));
                }
            }
        }
    }
    Ok(scripts)
}

fn generated_console_script_body(bytes: &[u8]) -> Option<&[u8]> {
    let newline = bytes.iter().position(|byte| *byte == b'\n')?;
    let shebang = std::str::from_utf8(&bytes[..newline]).ok()?;
    let mut body = &bytes[newline + 1..];
    // uv uses this exact shell/Python wrapper when an interpreter path is too
    // long for an OS shebang. The view must still launch its own Python.
    let python = if shebang == "#!/bin/sh" {
        let newline = body.iter().position(|byte| *byte == b'\n')?;
        let command = std::str::from_utf8(&body[..newline])
            .ok()?
            .strip_prefix("'''exec' ")?
            .strip_suffix(" \"$0\" \"$@\"")?;
        let arguments = shlex::split(command)?;
        if arguments.len() != 1 {
            return None;
        }
        body = body[newline + 1..].strip_prefix(b"' '''\n")?;
        arguments.into_iter().next()?
    } else {
        shebang.strip_prefix("#!")?.to_owned()
    };
    let path = Path::new(&python);
    let suffix = path.file_name()?.to_str()?.strip_prefix("python")?;
    if !path.is_absolute()
        || !suffix
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
    {
        return None;
    }
    body.starts_with(b"# -*- coding: utf-8 -*-\nimport sys\nfrom ")
        .then_some(body)
}

fn publish_view(workspace: &Path, view: &Path) -> Result<(), ProvisionError> {
    let parent = view.parent().expect("a view directory has a parent");
    std::fs::create_dir_all(parent).map_err(|error| {
        ProvisionError::Failed(format!("{} is unusable: {error}", parent.display()))
    })?;
    std::fs::rename(workspace.join("view"), view).map_err(|error| {
        ProvisionError::Failed(format!(
            "the view could not be published at {}: {error}",
            view.display()
        ))
    })
}

/// Run the published view at its final path and report what it actually holds.
async fn verify_view(
    provisioner: &DependencyProvisioner,
    store: &Path,
    view: &Path,
    resolved: &ResolvedRequirements,
    access: &DependencyAccess,
) -> Result<(), ProvisionError> {
    let config = &provisioner.config;
    let mut grants = vec![ReadOnlyInput::new(view, view)];
    grants.extend(
        crate::plugin_view::interpreter_prefix_grants(&resolved.interpreter_prefix)
            .map_err(ProvisionError::Failed)?,
    );
    for directory in referenced_store_directories(view, store).map_err(ProvisionError::Failed)? {
        grants.push(ReadOnlyInput::new(&directory, &directory));
    }
    let bin = view.join("bin");
    let build = authority(&provisioner.sandbox, grants, std::slice::from_ref(&bin))?;
    let workspace = config.build_root.join("plugin-dependencies/verify");
    std::fs::create_dir_all(&workspace).map_err(|error| {
        ProvisionError::Failed(format!(
            "the dependency verification workspace is unavailable: {error}"
        ))
    })?;
    let probe = "import json, sys\n\
         report = {'prefix': sys.prefix, 'exe': sys.executable, 'distributions': []}\n\
         try:\n\
         \x20   import importlib.metadata as metadata\n\
         \x20   report['distributions'] = sorted(\n\
         \x20       [d.metadata['Name'], d.version] for d in metadata.distributions())\n\
         except Exception as error:\n\
         \x20   report['error'] = repr(error)\n\
         print(json.dumps(report))";

    let output = run_step_with_access(
        config,
        &build,
        &workspace,
        &[
            view.join("bin/python").to_string_lossy().into_owned(),
            "-c".into(),
            probe.to_owned(),
        ],
        "running the Plugin dependency view",
        access,
    )
    .await?;
    let report: serde_json::Value = serde_json::from_str(output.trim()).map_err(|error| {
        ProvisionError::Failed(format!("the view reported no usable result: {error}"))
    })?;
    let prefix = report
        .get("prefix")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if prefix != view.to_string_lossy() {
        return Err(ProvisionError::Failed(format!(
            "the view runs with prefix `{prefix}` instead of `{}`, so it is not usable where it was published",
            view.display()
        )));
    }
    if let Some(error) = report.get("error").and_then(serde_json::Value::as_str) {
        return Err(ProvisionError::Failed(format!(
            "the view cannot read its own installed set ({error})"
        )));
    }
    // A view carries `lib64 -> lib`, so the same `site-packages` is reachable
    // under two path strings and importlib.metadata reports each distribution
    // once per alias. The installed *set* is what the lock pins.
    let observed: std::collections::BTreeSet<(String, String)> = report
        .get("distributions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let pair = entry.as_array()?;
            Some((
                pair.first()?.as_str()?.to_owned(),
                pair.get(1)?.as_str()?.to_owned(),
            ))
        })
        .map(|(name, version)| (normalize_name(&name), version))
        .collect();
    let planned: std::collections::BTreeSet<(String, String)> = resolved
        .packages
        .iter()
        .map(|package| (normalize_name(&package.name), package.version.clone()))
        .collect();
    if observed != planned {
        return Err(ProvisionError::Failed(format!(
            "the view holds {observed:?}, not the approved {planned:?}"
        )));
    }
    Ok(())
}

/// Take the one write lock that store and view publication share.
pub(crate) async fn write_lock(
    root: &Path,
    access: &DependencyAccess,
) -> Result<anchor_file_lease::FileLease, ProvisionError> {
    use std::os::unix::fs::OpenOptionsExt;
    crate::host_fs::directory(root).map_err(ProvisionError::Failed)?;
    let path = root.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .custom_flags(
            rustix::fs::OFlags::NOFOLLOW.bits() as i32 | rustix::fs::OFlags::NONBLOCK.bits() as i32,
        )
        .open(&path)
        .map_err(|error| {
            ProvisionError::Failed(format!("{} is unusable: {error}", path.display()))
        })?;
    if !file.metadata().map_err(failure)?.is_file() {
        return Err(failure("dependency writer lease is not a regular file"));
    }
    let file = anchor_file_lease::FileLease::new(file);
    loop {
        access.check()?;
        match file.lock(rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => break,
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {},
                _ = access.cancelled() => return Err(ProvisionError::Failed("dependency preparation was cancelled while waiting for its store lock".into())),
            },
            Err(error) => {
                return Err(ProvisionError::Failed(format!(
                    "the dependency store lock failed: {error}"
                )));
            }
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::RunApplication;
    use crate::dependency_provision::test_support::{
        builder_required, config, fixture_wheel, toolchain,
    };
    use crate::plugin_library::Catalog;
    use crate::tool_host::{McpToolConfig, SandboxLaunch};
    use anchor_graph_host::{PluginCatalog, PluginDependencies};
    use anchor_runtime::ToolPort;
    use anchor_sandbox_bwrap::{BubblewrapPolicy, BubblewrapSandbox};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    #[test]
    fn only_generated_entry_point_interpreter_bindings_are_ignored_when_comparing_store_bytes() {
        let root = tempfile::tempdir().unwrap();
        let left = root.path().join("left");
        let right = root.path().join("right");
        for tree in [&left, &right] {
            std::fs::create_dir_all(tree.join("bin")).unwrap();
            std::fs::create_dir_all(tree.join("fixture-1.dist-info")).unwrap();
            std::fs::write(
                tree.join("fixture-1.dist-info/entry_points.txt"),
                "[console_scripts]\ntool = fixture.cli:main\n",
            )
            .unwrap();
        }
        let body = "# -*- coding: utf-8 -*-\nimport sys\nfrom fixture.cli import main\nif __name__ == \"__main__\":\n    sys.exit(main())\n";
        let short = format!("#!/usr/bin/python3.12\n{body}");
        let long = format!(
            "#!/bin/sh\n'''exec' '/a long managed prefix/bin/python3.12' \"$0\" \"$@\"\n' '''\n{body}"
        );
        std::fs::write(left.join("bin/tool"), &short).unwrap();
        std::fs::write(right.join("bin/tool"), &long).unwrap();
        assert!(same_tree(&left, &right).unwrap());
        assert_ne!(
            package_bytes_digest(&left).unwrap(),
            package_bytes_digest(&right).unwrap(),
            "provenance continues to check every stored byte"
        );

        std::fs::write(right.join("bin/tool"), long.replace("main()", "other()")).unwrap();
        assert!(
            !same_tree(&left, &right).unwrap(),
            "script bodies must match"
        );
        std::fs::write(
            right.join("bin/tool"),
            long.replace("' \"$0\"", "' -O \"$0\""),
        )
        .unwrap();
        assert!(
            !same_tree(&left, &right).unwrap(),
            "interpreter arguments cannot be ignored"
        );
        std::fs::write(right.join("bin/tool"), &long).unwrap();

        std::fs::write(left.join("bin/undeclared"), &short).unwrap();
        std::fs::write(right.join("bin/undeclared"), &long).unwrap();
        assert!(
            !same_tree(&left, &right).unwrap(),
            "arbitrary bin files remain payload"
        );
        std::fs::remove_file(left.join("bin/undeclared")).unwrap();
        std::fs::remove_file(right.join("bin/undeclared")).unwrap();

        std::fs::write(
            left.join("bin/tool"),
            "#!/usr/bin/python3.12\nprint('wheel script')\n",
        )
        .unwrap();
        std::fs::write(
            right.join("bin/tool"),
            "#!/a/bin/python3.12\nprint('wheel script')\n",
        )
        .unwrap();
        assert!(
            !same_tree(&left, &right).unwrap(),
            "only the uv-generated script format is normalized"
        );
        std::fs::write(left.join("bin/tool"), &short).unwrap();
        std::fs::write(right.join("bin/tool"), &long).unwrap();
        std::fs::write(
            right.join("fixture-1.dist-info/entry_points.txt"),
            "[other]\ntool = fixture.cli:main\n",
        )
        .unwrap();
        assert!(
            !same_tree(&left, &right).unwrap(),
            "entry-point declarations remain payload"
        );
    }

    #[tokio::test]
    async fn store_lease_wait_cancels_and_a_later_preparation_can_continue() {
        let root = tempfile::tempdir().unwrap();
        let active = DependencyAccess::default();
        let held = write_lock(root.path(), &active).await.unwrap();
        let access = DependencyAccess::default();
        let cancel = async {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            access
                .cancellation
                .store(true, std::sync::atomic::Ordering::Release);
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(write_lock(root.path(), &access), cancel)
        })
        .await
        .expect("waiting for the package-store lease must be cancellable");
        assert!(result.unwrap_err().to_string().contains("cancel"));
        drop(held);
        let next = write_lock(root.path(), &DependencyAccess::default())
            .await
            .unwrap();
        drop(next);
    }

    /// The built-in Plugin this slice ships, in the repository next to the others.
    fn built_in_probe() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/python-probe")
    }

    /// The built-in probe Plugin, with one test-only addition: the address this
    /// case listens on, so the server can report whether it can reach the network.
    /// Its manifest declaration and its server are the committed ones.
    fn write_candidate(directory: &Path, probe_address: &str) {
        let source = built_in_probe();
        std::fs::create_dir_all(directory).unwrap();
        std::fs::copy(source.join("server.py"), directory.join("server.py")).unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(source.join("plugin.json")).unwrap()).unwrap();
        manifest["mcpServers"]["probe"]["env"] =
            serde_json::json!({"ANCHOR_PROBE_ADDRESS": probe_address});
        std::fs::write(directory.join("plugin.json"), manifest.to_string()).unwrap();
    }

    fn sandbox(workspace: &Path) -> Option<BubblewrapSandbox> {
        // The deployment's allowlist deliberately does not name `python`: the
        // view's interpreter must be authorized by the invocation's executable
        // grant, so this case fails if that grant ever stops being applied.
        let policy = BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(workspace);
        match BubblewrapSandbox::new(policy) {
            Ok(sandbox) => Some(sandbox),
            Err(error) => {
                let reason = format!("Bubblewrap is unavailable: {error}");
                if builder_required() {
                    panic!("ANCHOR_REQUIRE_ENV_BUILDER=1 but {reason}");
                }
                eprintln!("SKIPPED (no verification): {reason}");
                None
            }
        }
    }

    /// The whole slice in one case: the built-in Python Plugin declares a package,
    /// the host solves and lands it through uv, a view is generated, and the
    /// Plugin's tool imports the package inside a no-network sandbox — from the
    /// view, not from the deployment's interpreter.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_plugin_tool_imports_its_declared_package_from_its_view_without_network() {
        let Some(deployment) = deploy().await else {
            return;
        };
        let root = &deployment.root;
        let library = deployment.library.clone();
        let workspace = &deployment.workspace;
        let sandbox = &deployment.sandbox;
        let provisioner = &deployment.provisioner;
        let application = &deployment.application;

        // The deployment interpreter is the control, but whether this particular
        // host happens to have the package is not the proof: the proof is where
        // the tool's import came from.
        let control = std::process::Command::new(&provisioner.config.python)
            .arg("-c")
            .arg("import idna")
            .output()
            .unwrap();
        if control.status.success() {
            eprintln!("NOTE: this host already imports idna; the view path below is the evidence");
        }

        // 1. The declaration is solved before a human is asked anything.
        let precondition = application
            .plugin_install_precondition(
                library.clone(),
                deployment.candidate.clone(),
                "python-probe".into(),
            )
            .await
            .unwrap();
        let plan = precondition
            .dependencies
            .clone()
            .expect("the declaration resolved to a plan");
        assert_eq!(
            plan.packages
                .iter()
                .map(|package| (package.name.as_str(), package.version.as_str()))
                .collect::<Vec<_>>(),
            vec![("idna", "3.6")]
        );
        assert_eq!(
            plan.interpreter,
            resolved_interpreter(&provisioner.config.python)
        );
        assert!(plan.view.starts_with(&library), "{}", plan.view.display());
        assert_eq!(plan.sources.len(), 1);

        let mut stale = resolved_of(&plan);
        stale.interpreter_digest = "obsolete interpreter digest".into();
        let refused = land(
            provisioner,
            &library,
            "python-probe",
            &plan.declaration_digest,
            &stale,
        )
        .await
        .unwrap_err();
        assert!(
            refused.to_string().contains("interpreter changed"),
            "{refused}"
        );
        assert!(
            !plan.view.exists(),
            "a stale interpreter cannot publish a view"
        );

        let outcome = application
            .install_plugin_if_unchanged(library.clone(), precondition)
            .await
            .unwrap();
        assert_eq!(outcome.id, "python-probe");

        // 2. The view is a real environment whose pointer names the shared store.
        let catalog = Catalog::new(&library);
        let view = catalog.view_path("python-probe").unwrap().expect("a view");
        assert!(view.join("pyvenv.cfg").is_file(), "{}", view.display());
        assert!(
            view.join("bin/python").is_symlink(),
            "the view must point at the deployment's one interpreter"
        );
        let store = store_root(&library.canonicalize().unwrap());
        let site_packages = site_packages_directories(&view).unwrap().remove(0);
        let pointer = site_packages.join("idna");
        assert!(pointer.is_symlink(), "{}", pointer.display());
        let resolved = pointer.canonicalize().unwrap();
        assert!(
            resolved.starts_with(store.canonicalize().unwrap()),
            "{} is not in the shared store",
            resolved.display()
        );
        // The store holds exactly one copy of the (package, version).
        assert_eq!(std::fs::read_dir(&store).unwrap().count(), 1);
        let reused = land(
            provisioner,
            &library,
            "python-probe",
            &plan.declaration_digest,
            &resolved_of(&plan),
        )
        .await
        .unwrap();
        assert!(reused.reused, "a landed declaration is not rebuilt");
        assert_eq!(reused.view, view);

        // 3. The Plugin's tool runs on the view's interpreter, in a sandbox with
        //    no network, and reports what that interpreter imported.
        let binding = catalog.resolve(&["python-probe".into()]).unwrap().remove(0);
        let report = report_at(
            &library,
            &library,
            &binding,
            sandbox,
            workspace,
            "python-probe-probe_report",
            &view,
        )
        .await;
        assert_eq!(report["version"], "3.6", "{report}");
        assert_eq!(
            report["prefix"],
            view.to_string_lossy().as_ref(),
            "{report}"
        );
        assert_eq!(
            report["exe"],
            view.join("bin/python").to_string_lossy().as_ref(),
            "{report}"
        );
        assert!(
            report["file"]
                .as_str()
                .unwrap()
                .starts_with(view.to_string_lossy().as_ref()),
            "the imported package did not come from the view: {report}"
        );
        assert!(
            report["network"].as_str().unwrap().starts_with("denied"),
            "the sandbox shares the host network: {report}"
        );
        drop(deployment.listener);

        // 4. A declaration that cannot be solved is refused before anything is
        //    written, and the view that already exists is untouched.
        let broken = root.path().join("broken-candidate");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(
            broken.join("plugin.json"),
            serde_json::json!({
                "name": "Broken",
                "dependencies": {"python": ["idna==9.9.9"]},
            })
            .to_string(),
        )
        .unwrap();
        let failure = application
            .plugin_install_precondition(library.clone(), broken, "broken".into())
            .await
            .unwrap_err();
        let crate::application::ApplicationError::Invalid(reason) = failure else {
            panic!("a declaration that cannot be solved is an invalid ask: {failure:?}");
        };
        assert!(
            reason.contains("dependency resolution failed"),
            "the refusal must carry uv's own reason: {reason}"
        );
        assert!(!library.join("plugins/broken").exists());
        assert_eq!(catalog.view_path("python-probe").unwrap(), Some(view));
        assert_eq!(
            catalog.definition("python-probe").unwrap().dependencies,
            PluginDependencies {
                python: vec!["idna==3.6".to_owned()]
            }
        );
    }

    /// A Plugin with a declared mutable path, whose tool writes into it and into a
    /// path it never declared, and reports the package its view provides.
    const WRITING_SERVER: &str = r#"import json, socket, sys

import idna

import os

data = os.path.join(os.getcwd(), "data")
os.makedirs(data, exist_ok=True)
with open(os.path.join(data, "kept.txt"), "w") as handle:
    handle.write("written by " + idna.__version__)
try:
    with open(os.path.join(os.getcwd(), "code.txt"), "w") as handle:
        handle.write("must not be writable")
    undeclared = "writable"
except OSError as error:
    undeclared = "denied: " + type(error).__name__


def report():
    return json.dumps({
        "version": idna.__version__,
        "prefix": sys.prefix,
        "undeclared_write": undeclared,
    })


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": request["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "anchor-probe", "version": "1.0.0"},
        }
    elif method == "tools/list":
        result = {"tools": [{
            "name": "version",
            "description": "The declared package this view provides",
            "inputSchema": {"type": "object", "properties": {}},
        }]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": report()}]}
    else:
        continue
    send({"jsonrpc": "2.0", "id": request.get("id"), "result": result})
"#;

    /// The same candidate, declaring a mutable path and a server that writes.
    fn write_writing_candidate(directory: &Path, declaration: &[&str]) {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(
            directory.join("plugin.json"),
            serde_json::json!({
                "name": "Probe",
                "dependencies": {"python": declaration},
                "mutable": ["data/"],
                "mcpServers": {
                    "probe": {
                        "command": "${ANCHOR_PLUGIN_VIEW}/bin/python",
                        "args": ["server.py"]
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(directory.join("server.py"), WRITING_SERVER).unwrap();
    }

    async fn report_at(
        library: &Path,
        catalog: &Path,
        binding: &anchor_runtime::graph::PluginBinding,
        sandbox: &BubblewrapSandbox,
        workspace: &Path,
        name: &str,
        expected_view: &Path,
    ) -> serde_json::Value {
        let prepared = crate::plugin_view::prepare(library, binding)
            .unwrap()
            .expect("the installed Plugin has a view");
        assert_eq!(prepared.view(), expected_view);
        let mcp = McpToolConfig::from_catalog(catalog, std::slice::from_ref(binding)).unwrap();
        let plugin_views = BTreeMap::from([(
            binding.id.clone(),
            crate::plugin_view::PreparedPlugin::for_candidate(
                &Catalog::new(library),
                &binding.id,
                Some(prepared),
            )
            .unwrap(),
        )]);
        let tools = mcp
            .bind_with_node_inputs(
                std::slice::from_ref(binding),
                false,
                SandboxLaunch {
                    sandbox,
                    workspace,
                    readonly_inputs: &[],
                    plugin_views: &plugin_views,
                },
            )
            .await
            .unwrap_or_else(|error| panic!("the Plugin server did not bind: {error}"));
        assert!(
            tools
                .definitions()
                .iter()
                .any(|definition| definition.name == name),
            "{name}: {:?}",
            tools.definitions()
        );
        let result = tools.call(name, serde_json::json!({})).await.unwrap();
        serde_json::from_str(result[0].as_text().unwrap()).unwrap()
    }

    /// Bind the installed Plugin's server and ask its one tool.
    async fn report_from(
        deployment: &Deployment,
        binding: &anchor_runtime::graph::PluginBinding,
    ) -> serde_json::Value {
        report_at(
            &deployment.library,
            &deployment.library,
            binding,
            &deployment.sandbox,
            &deployment.workspace,
            "python-probe-probe_version",
            &deployment.view(),
        )
        .await
    }

    /// The same Run uses the **current** Plugin: its first call reads the 3.6 its
    /// declaration resolved to, and after the declaration is upgraded to 3.7 the
    /// same frozen binding's next call reads 3.7 — without a refusal — while the
    /// data the Plugin wrote into its declared mutable path is still there.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_upgrade_takes_effect_at_the_next_call_and_keeps_the_plugins_data() {
        let Some(deployment) = deploy().await else {
            return;
        };
        write_writing_candidate(&deployment.candidate, &["idna==3.6"]);
        deployment.install().await.unwrap();

        // The Run froze this binding when the declaration was 3.6.
        let binding = Catalog::new(&deployment.library)
            .resolve(&["python-probe".into()])
            .unwrap()
            .remove(0);
        let first = report_from(&deployment, &binding).await;
        assert_eq!(first["version"], "3.6", "{first}");
        assert!(
            first["undeclared_write"]
                .as_str()
                .unwrap()
                .starts_with("denied"),
            "a path the declaration never named must not be writable: {first}"
        );
        let data = deployment
            .library
            .join("plugins/python-probe/data/kept.txt");
        assert_eq!(
            std::fs::read_to_string(&data).unwrap(),
            "written by 3.6",
            "the Plugin wrote into its declared mutable path"
        );

        // Upgrade: the same Plugin, a new declaration and a new resolution.
        write_writing_candidate(&deployment.candidate, &["idna==3.7"]);
        let wheels = deployment.root.path().join("wheels");
        std::fs::remove_file(wheels.join("idna-3.6-py3-none-any.whl")).unwrap();
        fixture_wheel(&wheels, "idna", "3.7");
        let precondition = deployment.precondition().await.unwrap();
        deployment
            .application
            .install_plugin_if_unchanged(deployment.library.clone(), precondition)
            .await
            .expect("an upgrade is always allowed");

        // The platform replaced code and left the declared mutable path alone: the
        // data is still exactly what the previous version wrote.
        assert_eq!(
            std::fs::read_to_string(&data).unwrap(),
            "written by 3.6",
            "an upgrade replaces code, never data"
        );

        // The *same* binding, the next call: the current Plugin, not a refusal.
        let second = report_from(&deployment, &binding).await;
        assert_eq!(second["version"], "3.7", "{second}");
        assert!(
            second["prefix"]
                .as_str()
                .unwrap()
                .contains(deployment.view().file_name().unwrap().to_str().unwrap()),
            "the new call used the new view: {second}"
        );
        // The Plugin still writes into the same declared path, in place.
        assert_eq!(
            std::fs::read_to_string(&data).unwrap(),
            "written by 3.7",
            "the new version keeps using the same data file"
        );
    }

    /// The built-in Python Plugin this slice ships stays well formed: it declares
    /// exactly the package its server imports and launches on its own view's
    /// interpreter, so a deployment can install it as it stands.
    #[test]
    fn the_built_in_python_probe_is_a_valid_declaration() {
        let directory = built_in_probe();
        let root = directory.parent().unwrap().parent().unwrap();
        let catalog = Catalog::new(root);
        let definition = catalog.definition("python-probe").unwrap();
        assert_eq!(definition.dependencies.python, vec!["idna==3.6"]);
        // The view exists, and the manifest names it as a deployment path: the
        // catalog keeps it as written, because only the host that *grants* the
        // view may substitute it (see the launcher's `named_view`).
        let view = catalog.view_path("python-probe").unwrap().unwrap();
        assert!(!view.ends_with("bin/python"));
        let server = catalog.mcp_servers("python-probe", true).unwrap().remove(0);
        assert_eq!(
            server
                .config
                .get("command")
                .and_then(serde_json::Value::as_str),
            Some("${ANCHOR_PLUGIN_VIEW}/bin/python")
        );
        assert_eq!(
            server.config.get("args"),
            Some(&serde_json::json!(["server.py"]))
        );
        let source = std::fs::read_to_string(directory.join("server.py")).unwrap();
        assert!(
            source.contains("import idna"),
            "the probe must import the package its manifest declares"
        );
    }

    /// One deployment of the built-in probe Plugin against a local wheelhouse.
    struct Deployment {
        root: tempfile::TempDir,
        library: PathBuf,
        workspace: PathBuf,
        sandbox: BubblewrapSandbox,
        provisioner: Arc<crate::dependency_provision::DependencyProvisioner>,
        application: RunApplication,
        candidate: PathBuf,
        /// Kept alive so the address the server is told to reach answers.
        listener: std::net::TcpListener,
    }

    async fn deploy() -> Option<Deployment> {
        let toolchain = toolchain()?;
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("library");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let sandbox = sandbox(&workspace)?;
        let wheels = root.path().join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        fixture_wheel(&wheels, "idna", "3.6");
        let provisioner = Arc::new(
            crate::dependency_provision::DependencyProvisioner::new(config(
                &root.path().join("environment-build"),
                &toolchain,
                wheels,
            ))
            .unwrap(),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let candidate = root.path().join("probe-candidate");
        write_candidate(&candidate, &listener.local_addr().unwrap().to_string());
        let application = RunApplication::new(root.path().join("state"), root.path().to_path_buf())
            .with_dependency_provisioner(Arc::clone(&provisioner));
        Some(Deployment {
            root,
            library,
            workspace,
            sandbox,
            provisioner,
            application,
            candidate,
            listener,
        })
    }

    impl Deployment {
        /// The view the declaration maps to, whether or not it exists.
        fn view(&self) -> PathBuf {
            Catalog::new(&self.library)
                .view_path("python-probe")
                .unwrap()
                .expect("the Plugin declares dependencies")
        }

        async fn precondition(
            &self,
        ) -> Result<
            crate::application::plugins::PluginInstallPrecondition,
            crate::application::ApplicationError,
        > {
            self.application
                .plugin_install_precondition(
                    self.library.clone(),
                    self.candidate.clone(),
                    "python-probe".into(),
                )
                .await
        }

        async fn install(
            &self,
        ) -> Result<anchor_library::InstallOutcome, crate::application::ApplicationError> {
            let precondition = self.precondition().await?;
            self.application
                .install_plugin_if_unchanged(self.library.clone(), precondition)
                .await
        }
    }

    /// The same candidate with a different declaration, so a case can use a
    /// requirement that does not pin a version.
    fn write_candidate_with(directory: &Path, probe_address: &str, declaration: &[&str]) {
        write_candidate(directory, probe_address);
        let path = directory.join("plugin.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["dependencies"]["python"] = serde_json::json!(declaration);
        std::fs::write(&path, manifest.to_string()).unwrap();
    }

    fn resolved_of(
        plan: &crate::application::plugins::PluginDependencyPlan,
    ) -> ResolvedRequirements {
        ResolvedRequirements {
            interpreter: plan.interpreter.clone(),
            interpreter_path: plan.interpreter_path.clone(),
            interpreter_prefix: PathBuf::from("/usr"),
            interpreter_digest: plan.interpreter_digest.clone(),
            lock: String::new(),
            lock_digest: plan.lock_digest.clone(),
            packages: plan.packages.clone(),
        }
    }

    /// A wheel for the same (package, version) with different bytes.
    fn write_other_wheel(directory: &Path, name: &str, version: &str) -> PathBuf {
        use crate::dependency_provision::test_support::write_zip;
        use base64::Engine;
        use sha2::Digest;
        let dist = format!("{name}-{version}");
        let mut files = vec![
            (
                format!("{name}/__init__.py"),
                format!("__version__ = {version:?}\nOTHER = True\n").into_bytes(),
            ),
            (
                format!("{dist}.dist-info/METADATA"),
                format!("Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n").into_bytes(),
            ),
            (
                format!("{dist}.dist-info/WHEEL"),
                b"Wheel-Version: 1.0\nGenerator: other\nRoot-Is-Purelib: true\nTag: py3-none-any\n"
                    .to_vec(),
            ),
        ];
        // A wheel is a valid wheel: without a RECORD the installer refuses it, and
        // then the case would prove nothing about the store.
        let mut record = String::new();
        for (path, data) in &files {
            let digest =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(data));
            record.push_str(&format!("{path},sha256={digest},{}\n", data.len()));
        }
        record.push_str(&format!("{dist}.dist-info/RECORD,,\n"));
        files.push((format!("{dist}.dist-info/RECORD"), record.into_bytes()));
        let path = directory.join(format!("{dist}-py3-none-any.whl"));
        std::fs::write(&path, write_zip(&files)).unwrap();
        path
    }

    /// A view that is gone must be repairable through the one installation gate:
    /// the same Plugin digest is not "nothing to do" while its view is missing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_deleted_view_is_rebuilt_by_installing_the_same_plugin_again() {
        let Some(deployment) = deploy().await else {
            return;
        };
        deployment.install().await.unwrap();
        let view = deployment.view();
        assert!(view.join("pyvenv.cfg").is_file(), "{}", view.display());
        std::fs::remove_dir_all(&view).unwrap();
        assert!(!view.exists());

        // The one gate must see work to do rather than reporting "already current".
        assert!(
            crate::plugin_view::view_needs_repair(&deployment.library, "python-probe"),
            "a missing view is exactly what a repair has to fix"
        );
        let precondition = deployment.precondition().await.unwrap();
        assert!(
            precondition.already_installed(),
            "the Plugin bytes themselves are unchanged"
        );
        let outcome = deployment
            .application
            .install_plugin_if_unchanged(deployment.library.clone(), precondition)
            .await
            .expect("a missing view is rebuilt through the one gate");
        assert_eq!(outcome.id, "python-probe");
        assert!(view.join("pyvenv.cfg").is_file(), "the view is back");
        assert!(
            crate::plugin_view::prepare_for_plugin(&deployment.library, "python-probe")
                .unwrap()
                .is_some(),
            "the rebuilt view is usable"
        );
        // The repair published no second copy of the Plugin.
        assert!(deployment.library.join("plugins/python-probe").is_dir());
    }

    /// A view is keyed by the declaration, and a declaration need not pin
    /// versions. What a human approves is the *resolved set*, so a view holding a
    /// different set must never be reported as this declaration's answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unpinned_declaration_never_disagrees_with_the_view_it_reuses() {
        let Some(deployment) = deploy().await else {
            return;
        };
        let declaration = ["idna>=3"];
        write_candidate_with(
            &deployment.candidate,
            &deployment.listener.local_addr().unwrap().to_string(),
            &declaration,
        );
        // First resolution: the authorized directory holds 3.6.
        deployment.install().await.unwrap();
        let view = deployment.view();
        let first = deployment.precondition().await.unwrap();
        let first_plan = first.dependencies.clone().unwrap();
        assert_eq!(first_plan.packages[0].version, "3.6");
        verify_view(
            &deployment.provisioner,
            &store_root(&resolved_library_root(&deployment.library)),
            &view,
            &resolved_of(&first_plan),
            &DependencyAccess::default(),
        )
        .await
        .expect("the landed view holds the first resolution");

        // The authorized directory now only holds 3.7, so the *same* declaration
        // resolves differently while the view stays where it is.
        let wheels = deployment.root.path().join("wheels");
        std::fs::remove_file(wheels.join("idna-3.6-py3-none-any.whl")).unwrap();
        fixture_wheel(&wheels, "idna", "3.7");
        let precondition = deployment.precondition().await.unwrap();
        let plan = precondition.dependencies.clone().unwrap();
        assert_eq!(
            plan.packages[0].version, "3.7",
            "resolution moved on while the view did not"
        );

        match deployment
            .application
            .install_plugin_if_unchanged(deployment.library.clone(), precondition)
            .await
        {
            Ok(_) => {
                // If reuse was allowed, the view must really hold the new set.
                verify_view(
                    &deployment.provisioner,
                    &store_root(&resolved_library_root(&deployment.library)),
                    &view,
                    &resolved_of(&plan),
                    &DependencyAccess::default(),
                )
                .await
                .expect("a reused view must hold the set this declaration resolved to");
            }
            Err(crate::application::ApplicationError::Invalid(reason)) => {
                assert!(
                    reason.contains("3.7") && reason.contains("3.6"),
                    "a refusal must name both sets: {reason}"
                );
            }
            Err(other) => panic!("unexpected failure: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identical_package_bytes_from_two_artifacts_keep_both_locks_reusable_offline() {
        let Some(deployment) = deploy().await else {
            return;
        };
        deployment.install().await.unwrap();
        let first = resolved_of(
            &deployment
                .precondition()
                .await
                .unwrap()
                .dependencies
                .unwrap(),
        );
        let wheel = deployment
            .root
            .path()
            .join("wheels/idna-3.6-py3-none-any.whl");
        let original_hash = file_digest(&wheel).unwrap();
        let mut archive = std::fs::read(&wheel).unwrap();
        assert_eq!(
            &archive[archive.len() - 22..archive.len() - 18],
            b"PK\x05\x06"
        );
        assert_eq!(&archive[archive.len() - 2..], &[0, 0]);
        // A ZIP comment changes the approved artifact hash while delivering
        // exactly the same distribution members and installed package bytes.
        let comment = b"second authorized artifact";
        let end = archive.len();
        archive[end - 2..].copy_from_slice(&(comment.len() as u16).to_le_bytes());
        archive.extend_from_slice(comment);
        std::fs::write(&wheel, archive).unwrap();
        let second_hash = file_digest(&wheel).unwrap();
        assert_ne!(original_hash, second_hash);
        let mut second = first.clone();
        second.packages[0].hashes = vec![second_hash.clone()];
        second.lock = second.lock.replace(&original_hash, &second_hash);
        second.lock_digest = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(second.lock.as_bytes()))
        };
        let offline = DependencyAccess {
            network: false,
            ..Default::default()
        };
        land_with_access(
            &deployment.provisioner,
            &deployment.library,
            "second",
            "second",
            &second,
            &offline,
        )
        .await
        .unwrap();
        std::fs::remove_dir_all(deployment.root.path().join("wheels")).unwrap();
        std::fs::remove_dir_all(deployment.provisioner.config.build_root.join("artifacts"))
            .unwrap();
        let existing = land_with_access(
            &deployment.provisioner,
            &deployment.library,
            "python-probe",
            &Catalog::new(&deployment.library)
                .definition("python-probe")
                .unwrap()
                .dependencies
                .declaration_digest(),
            &first,
            &offline,
        )
        .await
        .unwrap();
        assert!(
            existing.reused,
            "the first lock remains valid after verifying an equivalent artifact"
        );
        land_with_access(
            &deployment.provisioner,
            &deployment.library,
            "third",
            "third",
            &first,
            &offline,
        )
        .await
        .expect("a new view can reuse either verified artifact's shared bytes without downloads");
        assert_eq!(
            std::fs::read_dir(store_root(&deployment.library).join("idna"))
                .unwrap()
                .count(),
            1
        );
    }

    /// The shared store is keyed by (package, version), so two different byte sets
    /// under one name and version are refused rather than silently merged.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_byte_set_for_one_package_version_is_refused() {
        let Some(deployment) = deploy().await else {
            return;
        };
        deployment.install().await.unwrap();
        let view = deployment.view();
        assert!(view.join("pyvenv.cfg").is_file());
        // Rebuild from a different wheel that claims the same name and version.
        std::fs::remove_dir_all(&view).unwrap();
        let wheels = deployment.root.path().join("wheels");
        std::fs::remove_file(wheels.join("idna-3.6-py3-none-any.whl")).unwrap();
        write_other_wheel(&wheels, "idna", "3.6");

        let failure = deployment.install().await.unwrap_err();
        let crate::application::ApplicationError::Invalid(reason) = failure else {
            panic!("a byte conflict is an invalid ask: {failure:?}");
        };
        assert!(reason.contains("different bytes"), "{reason}");
        assert!(
            !view.exists(),
            "a view built from bytes the store refused is not published"
        );
    }

    /// A view must run on the deployment's one interpreter, not on a copy.
    #[test]
    fn a_view_that_does_not_point_at_the_one_interpreter_is_refused() {
        let Some(toolchain) = toolchain() else {
            return;
        };
        let interpreter = std::fs::canonicalize(&toolchain.python).unwrap();
        let root = tempfile::tempdir().unwrap();
        let view = root.path().join("view");
        std::fs::create_dir_all(view.join("bin")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", view.join("bin/python")).unwrap();
        let error = assert_interpreter(&view, &interpreter).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("instead of the deployment interpreter"),
            "{error}"
        );
        std::fs::remove_file(view.join("bin/python")).unwrap();
        std::os::unix::fs::symlink(&interpreter, view.join("bin/python")).unwrap();
        assert!(assert_interpreter(&view, &interpreter).is_ok());
    }

    /// The real deployment layout: the Plugin is installed in the Library, while
    /// the Graph catalog's bundle carries its own copy of the same tree. The
    /// command the launcher builds must name the *granted* view, so a Plugin
    /// resolved through the catalog root still runs on its Library view.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_plugin_resolved_from_a_bundle_root_still_runs_on_its_library_view() {
        let Some(deployment) = deploy().await else {
            return;
        };
        deployment.install().await.unwrap();
        let library_view = deployment.view();
        assert!(library_view.join("pyvenv.cfg").is_file());

        // Three separate roots, as the deployment data-root contract describes:
        // the Library holds what was installed, the catalog holds the bundle, and
        // the bundle carries its own copy of the Plugin resources.
        let catalog = deployment.root.path().join("catalog");
        let bundle = catalog.join("demo-bundle");
        let bundled_plugin = bundle.join("plugins/python-probe");
        std::fs::create_dir_all(&bundled_plugin).unwrap();
        for name in ["plugin.json", "server.py"] {
            std::fs::copy(
                deployment.library.join("plugins/python-probe").join(name),
                bundled_plugin.join(name),
            )
            .unwrap();
        }
        assert!(!library_view.starts_with(&catalog));

        // The binding is resolved through the *catalog*, exactly as the node host
        // does when a bundle serves the Plugin.
        let catalog_source = Catalog::new(&bundle);
        assert_eq!(
            catalog_source.definition("python-probe").unwrap().digest,
            Catalog::new(&deployment.library)
                .definition("python-probe")
                .unwrap()
                .digest,
            "the bundle copy is the same Plugin the Run froze"
        );
        let binding = catalog_source
            .resolve(&["python-probe".into()])
            .unwrap()
            .remove(0);
        let report = report_at(
            &deployment.library,
            &bundle,
            &binding,
            &deployment.sandbox,
            &deployment.workspace,
            "python-probe-probe_report",
            &library_view,
        )
        .await;
        assert_eq!(report["version"], "3.6", "{report}");
        assert!(
            report["file"]
                .as_str()
                .unwrap()
                .starts_with(library_view.to_string_lossy().as_ref()),
            "the interpreter must be the one the host granted: {report}"
        );
    }

    /// The interpreter version the fixture wheel was built for, as the plan
    /// reports it.
    fn resolved_interpreter(python: &Path) -> String {
        let output = std::process::Command::new(python)
            .arg("-c")
            .arg("import sys; print('%d.%d.%d' % sys.version_info[:3])")
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
}
