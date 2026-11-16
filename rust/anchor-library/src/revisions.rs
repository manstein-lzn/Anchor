//! Immutable code and solved-view references, under the existing Library lease.
//! The installed Plugin remains the only default and mutable-data authority.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

use anchor_graph_host::{FilePluginCatalog, McpServerDefinition, PluginDefinition};
use anchor_runtime::{artifact::store::valid_sha256, graph::PluginBinding};
use rustix::fs::FlockOperation;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{InstallError, InstallLease, Library, filesystem};

const REVISIONS: &str = "revisions";
const LEASES: &str = ".revision-leases";
const BINDING: &str = "binding.json";
const MAX_BINDING: usize = 512 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RevisionError {
    #[error(transparent)]
    Install(#[from] InstallError),
    #[error("Plugin revision or solved dependency identity is invalid")]
    Invalid,
    #[error("Plugin revision is not retained")]
    Missing,
    #[error("retained Plugin revision content or dependency binding changed")]
    Changed,
    #[error("Plugin revision is referenced or executing")]
    InUse,
    #[error("current installed Plugin no longer authorizes this revision")]
    AuthorizationChanged,
    #[error("Plugin default source is unavailable during pending publication")]
    DefaultUnavailable,
    #[error("Plugin revision storage is unavailable")]
    Io(#[from] io::Error),
}

/// A code identity, never a second default-install pointer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRevision {
    pub id: String,
    pub digest: String,
}

/// Identity of an actual solve, not merely a requirements declaration.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyViewIdentity {
    pub declaration_digest: String,
    pub lock: String,
    pub lock_digest: String,
    pub interpreter: String,
    pub interpreter_path: PathBuf,
    pub interpreter_prefix: PathBuf,
    pub interpreter_digest: String,
}

impl std::fmt::Debug for DependencyViewIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DependencyViewIdentity")
            .field("declaration_digest", &self.declaration_digest)
            .field("lock_digest", &self.lock_digest)
            .field("interpreter_path", &self.interpreter_path)
            .field("interpreter_digest", &self.interpreter_digest)
            .finish_non_exhaustive()
    }
}

impl DependencyViewIdentity {
    /// The existing candidate-view key; callers must not introduce another hash.
    pub fn key_for(lock_digest: &str, interpreter_path: &Path, interpreter_digest: &str) -> String {
        let bytes = serde_json::to_vec(&(
            "anchor.development-view.v1",
            lock_digest,
            interpreter_path,
            interpreter_digest,
        ))
        .expect("view identity encoding");
        format!("{:x}", Sha256::digest(bytes))
    }

    pub fn key(&self) -> String {
        Self::key_for(
            &self.lock_digest,
            &self.interpreter_path,
            &self.interpreter_digest,
        )
    }

    pub fn view_path(&self, library_root: &Path, id: &str) -> PathBuf {
        anchor_graph_host::plugin_view_path(library_root, id, &self.key())
    }

    fn validate(&self) -> Result<(), RevisionError> {
        if !valid_sha256(&self.declaration_digest)
            || !valid_sha256(&self.lock_digest)
            || !valid_sha256(&self.interpreter_digest)
            || self.lock.is_empty()
            || self.lock.len() > 256 * 1024
            || self.lock_digest != format!("{:x}", Sha256::digest(self.lock.as_bytes()))
            || self.interpreter.is_empty()
            || self.interpreter.len() > 64
            || self.interpreter.chars().any(char::is_control)
            || !normalized_absolute(&self.interpreter_path)
            || !normalized_absolute(&self.interpreter_prefix)
            || !self.interpreter_path.starts_with(&self.interpreter_prefix)
        {
            return Err(RevisionError::Invalid);
        }
        Ok(())
    }

    fn verify_view(&self, root: &Path, id: &str) -> Result<String, RevisionError> {
        self.validate()?;
        let view = self.view_path(root, id);
        filesystem::directory(&view, false)?;
        anchor_file_lease::read_regular(&view.join("pyvenv.cfg"), 16 * 1024)?;
        filesystem::directory(&view.join("bin"), false)?;
        if view.join("bin/python").canonicalize()? != self.interpreter_path
            || format!(
                "{:x}",
                Sha256::digest(anchor_file_lease::read_regular(
                    &self.interpreter_path,
                    128 * 1024 * 1024,
                )?)
            ) != self.interpreter_digest
        {
            return Err(RevisionError::Changed);
        }
        let store = anchor_graph_host::store_root(root);
        filesystem::directory(&store, false)?;
        if crate::reclaim::store_references(&view, &store)?.is_empty() {
            return Err(RevisionError::Invalid);
        }
        view_digest(&view)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionBinding {
    format: u32,
    revision: PluginRevision,
    authorization_digest: anchor_runtime::graph::AuthorizationDigest,
    dependencies: Option<DependencyViewIdentity>,
    /// Detect edits to the actual view after its verified, immutable admission.
    view_digest: Option<String>,
}

/// One preparation's code, data and dependency authority. Exact revisions retain
/// this through execution; live defaults release it before OAuth/MCP startup.
pub struct ResolvedPluginSource {
    source: SourceRead,
    _lease: anchor_file_lease::FileLease,
}

struct SourceRead {
    library_root: PathBuf,
    definition: PluginDefinition,
    mutable_root: PathBuf,
    current_authority: PluginBinding,
    revision: Option<String>,
    dependency_view: Option<PathBuf>,
    dependencies: Option<DependencyViewIdentity>,
}

impl std::fmt::Debug for ResolvedPluginSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedPluginSource")
            .field("binding", &self.binding())
            .field("code_root", &self.code_root())
            .field("mutable_root", &self.mutable_root())
            .field("revision", &self.revision())
            .field("dependencies", &self.dependency_identity())
            .finish_non_exhaustive()
    }
}

impl ResolvedPluginSource {
    pub fn library_root(&self) -> &Path {
        &self.source.library_root
    }

    pub fn definition(&self) -> &PluginDefinition {
        &self.source.definition
    }
    pub fn code_root(&self) -> &Path {
        &self.source.definition.directory
    }
    pub fn mutable_root(&self) -> &Path {
        &self.source.mutable_root
    }
    pub fn binding(&self) -> PluginBinding {
        self.source.definition.binding()
    }
    pub fn current_authority(&self) -> &PluginBinding {
        &self.source.current_authority
    }
    pub fn revision(&self) -> Option<&str> {
        self.source.revision.as_deref()
    }
    pub fn dependency_view(&self) -> Option<&Path> {
        self.source.dependency_view.as_deref()
    }
    pub fn dependency_identity(&self) -> Option<&DependencyViewIdentity> {
        self.source.dependencies.as_ref()
    }

    pub fn mutable_paths(&self) -> Result<Vec<PathBuf>, RevisionError> {
        self.source.mutable_paths()
    }

    /// Skill paths, MCP declarations and the view all belong to this same read.
    /// As in the ordinary catalog, the view token stays for the authorized
    /// launcher to bind from `dependency_view`, before credential injection.
    pub fn resolve_mcp_servers_with(
        &self,
        environment: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Vec<McpServerDefinition>, anchor_runtime::graph::GraphError> {
        FilePluginCatalog::new(&self.source.library_root)
            .resolve_mcp_servers_with(&self.source.definition, environment)
    }
}

impl SourceRead {
    fn mutable_paths(&self) -> Result<Vec<PathBuf>, RevisionError> {
        self.definition
            .mutable
            .resolve(&self.mutable_root)
            .map_err(|reason| RevisionError::Install(InstallError::UnsafeMutablePath(reason)))
    }
}

impl Library {
    /// Admit through the same staging/catalog rules as installation. Call before
    /// replacing the default while the caller holds its complete live-reference
    /// set and the Library installation lease. Business data is never staged.
    pub fn retain_revision_locked(
        &self,
        id: &str,
        source: impl AsRef<Path>,
        expected_digest: &str,
        dependencies: Option<DependencyViewIdentity>,
        lease: &InstallLease,
    ) -> Result<PluginRevision, RevisionError> {
        Self::validate_extension_install(id)?;
        if !valid_sha256(expected_digest) {
            return Err(RevisionError::Invalid);
        }
        let root = self.leased_root(lease)?;
        filesystem::directory(&root.join("plugins").join(id), false)?;
        let (parent, parent_file) = filesystem::directory(&root.join(REVISIONS).join(id), true)?;
        let transaction = tempfile::Builder::new()
            .prefix(".anchor-revision-")
            .tempdir_in(&parent)?;
        let result = (|| {
            let (staged_root, _, _) =
                crate::stage_candidate(id, source.as_ref(), transaction.path())?;
            let definition = FilePluginCatalog::new(&staged_root)
                .definition(id)
                .map_err(|_| RevisionError::Invalid)?;
            if definition.digest != expected_digest {
                return Err(RevisionError::Changed);
            }
            let view_digest = match &dependencies {
                Some(identity)
                    if !definition.dependencies.is_empty()
                        && identity.declaration_digest
                            == definition.dependencies.declaration_digest() =>
                {
                    Some(identity.verify_view(&root, id)?)
                }
                None if definition.dependencies.is_empty() => None,
                _ => return Err(RevisionError::Invalid),
            };
            let revision = PluginRevision {
                id: id.into(),
                digest: expected_digest.into(),
            };
            let binding = RevisionBinding {
                format: 1,
                revision: revision.clone(),
                authorization_digest: definition.authorization_digest,
                dependencies,
                view_digest,
            };
            let destination = parent.join(expected_digest);
            match fs::symlink_metadata(&destination) {
                Ok(_) => {
                    let _held = revision_lease(
                        &root,
                        &revision,
                        FlockOperation::NonBlockingLockShared,
                        false,
                    )?;
                    if load_binding(&root, &revision)? != binding {
                        return Err(RevisionError::Changed);
                    }
                    // A crash after rename may leave only the outer directory
                    // writable; all code and the binding were synced beforehand.
                    let directory = anchor_file_lease::open_directory(&destination)?;
                    directory.set_permissions(fs::Permissions::from_mode(0o555))?;
                    directory.sync_all()?;
                    read_definition(&root, &binding)?;
                    return Ok(revision);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            // Persist the reservation outside the asset tree. Losing code must
            // not masquerade as explicitly releasing its dependency references.
            let _revision = revision_lease(
                &root,
                &revision,
                FlockOperation::NonBlockingLockExclusive,
                true,
            )?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(staged_root.join(BINDING))?;
            file.write_all(&serde_json::to_vec(&binding).map_err(|_| RevisionError::Invalid)?)?;
            file.sync_all()?;
            immutable_tree(&staged_root, true)?;
            filesystem::sync_tree(&staged_root)?;
            // Moving a directory across parents updates `..` and requires write
            // access to that outer directory. Readers remain behind the Library
            // lease until its final mode and parent entry are durable.
            let staged_directory = anchor_file_lease::open_directory(&staged_root)?;
            staged_directory.set_permissions(fs::Permissions::from_mode(0o700))?;
            fs::rename(&staged_root, transaction.path().join(expected_digest))?;
            let (_, staged_parent) = filesystem::directory(transaction.path(), false)?;
            filesystem::publish(
                &staged_parent,
                &parent_file,
                expected_digest,
                false,
                filesystem::sync_publication,
            )?;
            staged_directory.set_permissions(fs::Permissions::from_mode(0o555))?;
            staged_directory.sync_all()?;
            Ok(revision)
        })();
        if result.is_err() {
            let _ = removable_directories(transaction.path());
        }
        result
    }

    /// Resolve once. Current installed declarations are rechecked even for old
    /// code; Host must additionally check its current grants for this invocation.
    pub fn resolve_source(
        &self,
        id: &str,
        revision: Option<&str>,
    ) -> Result<ResolvedPluginSource, RevisionError> {
        let root = if revision.is_none() {
            // Match the existing Catalog's live-read capability without giving
            // installers or retained revisions authority through an alias.
            self.root.canonicalize()?
        } else {
            filesystem::directory(&self.root, false)?.0
        };
        let guard = self.catalog_read_guard()?.ok_or(RevisionError::Missing)?;
        if revision.is_none() {
            crate::source_fence::ensure_default_available(&root, id)?;
        }
        let (source, revision_lease) = read_source(root, id, revision)?;
        Ok(ResolvedPluginSource {
            source,
            _lease: revision_lease.unwrap_or(guard),
        })
    }

    /// Inspect under an existing installation lease without reacquiring its
    /// shared lock. This deliberately bypasses the default publication outage
    /// for owner reconciliation; it does not authorize later execution.
    pub fn inspect_binding_locked(
        &self,
        id: &str,
        revision: Option<&str>,
        lease: &InstallLease,
    ) -> Result<PluginBinding, RevisionError> {
        let root = self.leased_root(lease)?;
        let (source, _held) = read_source(root, id, revision)?;
        Ok(source.definition.binding())
    }

    /// Trusted Host cleanup supplies the complete Graph, change, experiment and
    /// recovery live set while holding the same lease as install/reclamation.
    /// There is no TTL and no deletion inferred from missing runtime processes.
    pub fn release_revision_locked(
        &self,
        revision: &PluginRevision,
        live: &BTreeSet<PluginRevision>,
        lease: &InstallLease,
    ) -> Result<bool, RevisionError> {
        Self::validate_extension_install(&revision.id)?;
        if !valid_sha256(&revision.digest) {
            return Err(RevisionError::Invalid);
        }
        let root = self.leased_root(lease)?;
        if live.contains(revision) {
            return Err(RevisionError::InUse);
        }
        let directory = revision_root(&root, revision);
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if fs::symlink_metadata(revision_lease_path(&root, revision)).is_ok() {
                    let _held = revision_lease(
                        &root,
                        revision,
                        FlockOperation::NonBlockingLockExclusive,
                        false,
                    )?;
                    remove_revision_lease(&root, revision)?;
                }
                remove_empty_plugin_directory(&root.join(REVISIONS).join(&revision.id))?;
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let _revision = revision_lease(
            &root,
            revision,
            FlockOperation::NonBlockingLockExclusive,
            false,
        )?;
        load_binding(&root, revision)?;
        immutable_tree(&directory, false)?;
        removable_directories(&directory)?;
        fs::remove_dir_all(&directory)?;
        File::open(directory.parent().expect("revision parent"))?.sync_all()?;
        remove_empty_plugin_directory(&root.join(REVISIONS).join(&revision.id))?;
        remove_revision_lease(&root, revision)?;
        Ok(true)
    }

    /// Retained revision digests for one Plugin, read-only and fail-closed.
    pub fn retained_revisions_for(&self, id: &str) -> Result<Vec<String>, RevisionError> {
        crate::validate_id(id)?;
        if crate::SystemPlugin::from_id(id).is_some() {
            return Ok(Vec::new());
        }
        let root = match self.root.canonicalize() {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut digests = BTreeSet::new();
        let leases = root
            .join(anchor_graph_host::DEPENDENCIES_DIRECTORY)
            .join(LEASES)
            .join(id);
        for entry in entries_if_present(&leases)? {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| RevisionError::Invalid)?;
            let digest = name
                .strip_suffix(".lock")
                .filter(|digest| valid_sha256(digest))
                .ok_or(RevisionError::Invalid)?
                .to_owned();
            digests.insert(digest);
        }
        for entry in entries_if_present(&root.join(REVISIONS).join(id))? {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| RevisionError::Invalid)?;
            if !name.starts_with(".anchor-revision-") {
                if !valid_sha256(&name) {
                    return Err(RevisionError::Invalid);
                }
                digests.insert(name);
            }
        }
        Ok(digests.into_iter().collect())
    }

    /// All retained revisions across the Library while holding its install lease.
    pub fn retained_revisions_locked(
        &self,
        lease: &InstallLease,
    ) -> Result<BTreeSet<PluginRevision>, RevisionError> {
        let root = self.leased_root(lease)?;
        let mut retained = BTreeSet::new();
        let leases = root
            .join(anchor_graph_host::DEPENDENCIES_DIRECTORY)
            .join(LEASES);
        for plugin in directories_if_present(&leases)? {
            let id = plugin
                .file_name()
                .into_string()
                .map_err(|_| RevisionError::Invalid)?;
            Self::validate_extension_install(&id)?;
            for entry in entries_if_present(&plugin.path())? {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| RevisionError::Invalid)?;
                let digest = name
                    .strip_suffix(".lock")
                    .filter(|digest| valid_sha256(digest))
                    .ok_or(RevisionError::Invalid)?
                    .to_owned();
                retained.insert(PluginRevision {
                    id: id.clone(),
                    digest,
                });
            }
        }
        for plugin in directories_if_present(&root.join(REVISIONS))? {
            let id = plugin
                .file_name()
                .into_string()
                .map_err(|_| RevisionError::Invalid)?;
            Self::validate_extension_install(&id)?;
            for entry in entries_if_present(&plugin.path())? {
                let digest = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| RevisionError::Invalid)?;
                if !digest.starts_with(".anchor-revision-") {
                    if !valid_sha256(&digest) {
                        return Err(RevisionError::Invalid);
                    }
                    retained.insert(PluginRevision {
                        id: id.clone(),
                        digest,
                    });
                }
            }
        }
        Ok(retained)
    }
}

fn read_source(
    root: PathBuf,
    id: &str,
    revision: Option<&str>,
) -> Result<(SourceRead, Option<anchor_file_lease::FileLease>), RevisionError> {
    crate::validate_id(id)?;
    let current = FilePluginCatalog::new(&root)
        .definition(id)
        .map_err(|_| RevisionError::Invalid)?;
    let mutable_root = if revision.is_some() {
        filesystem::directory(&root.join("plugins").join(id), false)?.0
    } else {
        current.directory.clone()
    };
    let authority = current.binding();
    let (definition, dependencies, dependency_view, held) = match revision {
        Some(digest) => {
            Library::validate_extension_install(id)?;
            if !valid_sha256(digest) {
                return Err(RevisionError::Invalid);
            }
            let revision = PluginRevision {
                id: id.into(),
                digest: digest.into(),
            };
            let held = revision_lease(
                &root,
                &revision,
                FlockOperation::NonBlockingLockShared,
                false,
            )?;
            let binding = load_binding(&root, &revision)?;
            let definition = read_definition(&root, &binding)?;
            if definition.authorization_digest != current.authorization_digest {
                return Err(RevisionError::AuthorizationChanged);
            }
            let view = binding
                .dependencies
                .as_ref()
                .map(|identity| identity.view_path(&root, id));
            (definition, binding.dependencies, view, Some(held))
        }
        None => {
            // Legacy default views retain their established declaration path.
            // Exact solves are mandatory for new immutable revisions.
            let view = (!current.dependencies.is_empty()).then(|| {
                anchor_graph_host::plugin_view_path(
                    &root,
                    id,
                    &current.dependencies.declaration_digest(),
                )
            });
            (current, None, view, None)
        }
    };
    let source = SourceRead {
        library_root: root,
        definition,
        mutable_root,
        current_authority: authority,
        revision: revision.map(str::to_owned),
        dependency_view,
        dependencies,
    };
    source.mutable_paths()?;
    Ok((source, held))
}

fn revision_root(root: &Path, revision: &PluginRevision) -> PathBuf {
    root.join(REVISIONS)
        .join(&revision.id)
        .join(&revision.digest)
}

fn revision_lease(
    root: &Path,
    revision: &PluginRevision,
    operation: FlockOperation,
    create: bool,
) -> Result<anchor_file_lease::FileLease, RevisionError> {
    let path = revision_lease_path(root, revision);
    let (_, directory) =
        filesystem::directory(path.parent().expect("revision lease parent"), create)?;
    let lease = filesystem::lease_named(
        &directory,
        &format!("{}.lock", revision.digest),
        operation,
        create,
    )
    .map_err(|error| match error {
        InstallError::CatalogBusy => RevisionError::InUse,
        error => error.into(),
    })?;
    if create {
        lease.sync_all()?;
        directory.sync_all()?;
    }
    Ok(lease)
}

fn revision_lease_path(root: &Path, revision: &PluginRevision) -> PathBuf {
    root.join(anchor_graph_host::DEPENDENCIES_DIRECTORY)
        .join(LEASES)
        .join(&revision.id)
        .join(format!("{}.lock", revision.digest))
}

fn remove_revision_lease(root: &Path, revision: &PluginRevision) -> Result<(), RevisionError> {
    let path = revision_lease_path(root, revision);
    let parent = path.parent().expect("revision lease parent");
    // Every acquisition first takes the Library lease. With its exclusive
    // lease and this revision's exclusive lease, no old-inode reader can wait.
    fs::remove_file(&path)?;
    File::open(parent)?.sync_all()?;
    remove_empty_plugin_directory(parent)?;
    Ok(())
}

fn remove_empty_plugin_directory(directory: &Path) -> Result<(), RevisionError> {
    match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    filesystem::directory(directory, false)?;
    if fs::read_dir(directory)?.next().is_none() {
        fs::remove_dir(directory)?;
        if let Some(parent) = directory.parent() {
            File::open(parent)?.sync_all()?;
        }
    }
    Ok(())
}

fn entries_if_present(path: &Path) -> Result<Vec<fs::DirEntry>, RevisionError> {
    directories_if_present(path)
}

fn load_binding(root: &Path, revision: &PluginRevision) -> Result<RevisionBinding, RevisionError> {
    let bytes = match anchor_file_lease::read_regular(
        &revision_root(root, revision).join(BINDING),
        MAX_BINDING,
    ) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(RevisionError::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    let binding: RevisionBinding =
        serde_json::from_slice(&bytes).map_err(|_| RevisionError::Invalid)?;
    if binding.format != 1
        || &binding.revision != revision
        || !valid_sha256(&revision.digest)
        || binding.dependencies.is_some() != binding.view_digest.is_some()
        || binding
            .view_digest
            .as_deref()
            .is_some_and(|digest| !valid_sha256(digest))
    {
        return Err(RevisionError::Invalid);
    }
    if let Some(identity) = &binding.dependencies {
        identity.validate()?;
    }
    Ok(binding)
}

fn read_definition(
    root: &Path,
    binding: &RevisionBinding,
) -> Result<PluginDefinition, RevisionError> {
    let directory = revision_root(root, &binding.revision);
    immutable_tree(&directory, false)?;
    let definition = FilePluginCatalog::new(&directory)
        .definition(&binding.revision.id)
        .map_err(|_| RevisionError::Invalid)?;
    if definition.digest != binding.revision.digest
        || definition.authorization_digest != binding.authorization_digest
        || definition.dependencies.is_empty() != binding.dependencies.is_none()
    {
        return Err(RevisionError::Changed);
    }
    // Bubblewrap cannot create a mount destination inside read-only code. The
    // shared staging path supplies empty mountpoints, never business contents.
    for relative in definition.mutable.paths() {
        let path = definition.directory.join(relative);
        let empty = if relative.ends_with('/') {
            anchor_file_lease::open_directory(&path)?;
            fs::read_dir(&path)?.next().is_none()
        } else {
            anchor_file_lease::open_regular(&path)?.metadata()?.len() == 0
        };
        if !empty {
            return Err(RevisionError::Changed);
        }
    }
    if let Some(identity) = &binding.dependencies
        && (identity.declaration_digest != definition.dependencies.declaration_digest()
            || Some(identity.verify_view(root, &binding.revision.id)?) != binding.view_digest)
    {
        return Err(RevisionError::Changed);
    }
    Ok(definition)
}

/// Reclamation consumes these references before deleting *any* view. Corrupt
/// or missing bytes are uncertainty, never evidence that a revision was freed.
pub(crate) fn revision_views(root: &Path) -> Result<Vec<PathBuf>, RevisionError> {
    let leases = root
        .join(anchor_graph_host::DEPENDENCIES_DIRECTORY)
        .join(LEASES);
    let mut retained = BTreeSet::new();
    let mut views = Vec::new();
    for plugin in directories_if_present(&leases)? {
        let id = plugin
            .file_name()
            .into_string()
            .map_err(|_| RevisionError::Invalid)?;
        Library::validate_extension_install(&id)?;
        filesystem::directory(&plugin.path(), false)?;
        for entry in fs::read_dir(plugin.path())? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| RevisionError::Invalid)?;
            let digest = name
                .strip_suffix(".lock")
                .filter(|digest| valid_sha256(digest))
                .ok_or(RevisionError::Invalid)?
                .to_owned();
            let file = anchor_file_lease::open_regular(&entry.path())?;
            if file.metadata()?.len() != 0 || file.metadata()?.nlink() != 1 {
                return Err(RevisionError::Invalid);
            }
            let revision = PluginRevision {
                id: id.clone(),
                digest,
            };
            let binding = load_binding(root, &revision)?;
            filesystem::directory(
                &revision_root(root, &revision).join("plugins").join(&id),
                false,
            )?;
            if let Some(identity) = binding.dependencies {
                let view = identity.view_path(root, &id);
                filesystem::directory(&view, false)?;
                if Some(view_digest(&view)?) != binding.view_digest {
                    return Err(RevisionError::Changed);
                }
                views.push(view);
            }
            retained.insert(revision);
        }
    }
    // Assets with missing leases are also uncertainty. This check never decides
    // that a Graph/change reference died from the absence of its code directory.
    for plugin in directories_if_present(&root.join(REVISIONS))? {
        let id = plugin
            .file_name()
            .into_string()
            .map_err(|_| RevisionError::Invalid)?;
        Library::validate_extension_install(&id)?;
        filesystem::directory(&plugin.path(), false)?;
        for entry in fs::read_dir(plugin.path())? {
            let entry = entry?;
            let digest = entry
                .file_name()
                .into_string()
                .map_err(|_| RevisionError::Invalid)?;
            if digest.starts_with(".anchor-revision-") {
                filesystem::directory(&entry.path(), false)?;
            } else if !retained.contains(&PluginRevision {
                id: id.clone(),
                digest,
            }) {
                return Err(RevisionError::Missing);
            }
        }
    }
    Ok(views)
}

fn directories_if_present(path: &Path) -> Result<Vec<fs::DirEntry>, RevisionError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
        Ok(_) => {
            filesystem::directory(path, false)?;
            Ok(fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?)
        }
    }
}

fn normalized_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

fn immutable_tree(root: &Path, seal: bool) -> Result<(), RevisionError> {
    let directory = anchor_file_lease::open_directory(root)?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            immutable_tree(&entry.path(), seal)?;
        } else {
            let file = anchor_file_lease::open_regular(&entry.path())?;
            let metadata = file.metadata()?;
            if metadata.nlink() != 1 {
                return Err(RevisionError::Changed);
            }
            if seal {
                file.set_permissions(fs::Permissions::from_mode(
                    0o444 | (metadata.mode() & 0o111),
                ))?;
                file.sync_all()?;
            } else if metadata.mode() & 0o222 != 0 {
                return Err(RevisionError::Changed);
            }
        }
    }
    if seal {
        directory.set_permissions(fs::Permissions::from_mode(0o555))?;
        directory.sync_all()?;
    } else if directory.metadata()?.mode() & 0o222 != 0 {
        return Err(RevisionError::Changed);
    }
    Ok(())
}

fn removable_directories(root: &Path) -> Result<(), RevisionError> {
    let directory = anchor_file_lease::open_directory(root)?;
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            removable_directories(&entry.path())?;
        }
    }
    Ok(())
}

fn view_digest(root: &Path) -> Result<String, RevisionError> {
    let mut pending = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        filesystem::directory(&directory, false)?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    let mut digest = Sha256::new();
    digest.update(b"anchor.plugin-solved-view.v1\0");
    for file in files {
        let relative = file.strip_prefix(root).expect("view child");
        let metadata = fs::symlink_metadata(&file)?;
        let bytes = if metadata.file_type().is_symlink() {
            fs::read_link(&file)?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            anchor_file_lease::read_regular(&file, 16 * 1024 * 1024)?
        };
        let entry = serde_json::to_vec(&(
            relative,
            metadata.file_type().is_symlink(),
            metadata.mode() & 0o111,
            format!("{:x}", Sha256::digest(bytes)),
        ))
        .map_err(|_| RevisionError::Invalid)?;
        digest.update(entry);
        digest.update(b"\n");
    }
    Ok(format!("{:x}", digest.finalize()))
}
