//! Temporary dependency references owned by a Host Pilot scope.
//!
//! The scope path is supplied by the trusted Host, which already derives it from
//! owner, Session and Session incarnation. A binding remains live across Host
//! restarts; absence of the source directory is never evidence for reclamation.

use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use rustix::fs::{FlockOperation, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Library;

pub(crate) const DEVELOPMENT_DIRECTORY: &str = "development";
const LEASES_DIRECTORY: &str = ".development-leases";
const BINDING: &str = "binding.json";

fn present(path: &Path) -> Result<bool, DevelopmentError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DevelopmentError {
    #[error("development dependencies are in use or being updated")]
    Busy,
    #[error("development dependency ownership is unavailable: {0}")]
    Invalid(String),
    #[error("development dependency storage failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeBinding {
    format: u32,
    scope: PathBuf,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewReference {
    format: u32,
    view: PathBuf,
}

/// Shared lease held while a scope prepares or executes with its dependencies.
/// Dropping it allows Session cleanup to acquire an exclusive lease.
#[derive(Debug)]
pub struct DevelopmentScope {
    root: PathBuf,
    ledger: PathBuf,
    released: PathBuf,
    binding: ScopeBinding,
    _lease: anchor_file_lease::FileLease,
}

fn invalid(error: impl std::fmt::Display) -> DevelopmentError {
    DevelopmentError::Invalid(error.to_string())
}

fn scope_binding(scope: &Path) -> Result<ScopeBinding, DevelopmentError> {
    let scope = std::path::absolute(scope)?;
    if scope
        .components()
        .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid("scope must be an absolute normalized Host path"));
    }
    Ok(ScopeBinding { format: 1, scope })
}

fn scope_id(binding: &ScopeBinding) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut digest = Sha256::new();
    digest.update(b"anchor.development-scope.v1\0");
    digest.update(binding.scope.as_os_str().as_bytes());
    format!("{:x}", digest.finalize())
}

fn lock(
    directory: &File,
    name: &str,
    operation: FlockOperation,
    create: bool,
) -> Result<anchor_file_lease::FileLease, DevelopmentError> {
    crate::filesystem::lease_named(directory, name, operation, create).map_err(
        |error| match error {
            crate::InstallError::CatalogBusy => DevelopmentError::Busy,
            crate::InstallError::Io(error) => DevelopmentError::Io(error),
            error => invalid(error),
        },
    )
}

fn read_binding(path: &Path) -> Result<Option<ScopeBinding>, DevelopmentError> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("binding has no parent"))?;
    let (_, directory) = match crate::filesystem::directory(parent, false) {
        Ok(value) => value,
        Err(crate::InstallError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(invalid(error)),
    };
    let file = match rustix::fs::openat(
        &directory,
        path.file_name().expect("binding name"),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(file) => File::from(file),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(invalid(error)),
    };
    if !file.metadata()?.is_file() || file.metadata()?.len() > 16 * 1024 {
        return Err(invalid("binding is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err(invalid("binding exceeds its limit"));
    }
    let value: ScopeBinding = serde_json::from_slice(&bytes).map_err(invalid)?;
    if value.format != 1 || scope_binding(&value.scope)? != value {
        return Err(invalid("binding identity changed"));
    }
    Ok(Some(value))
}

fn save_binding(path: &Path, binding: &ScopeBinding) -> Result<(), DevelopmentError> {
    if let Some(retained) = read_binding(path)? {
        if &retained != binding {
            return Err(invalid("binding belongs to another Host scope"));
        }
        return Ok(());
    }
    let parent = path.parent().expect("binding directory");
    let (_, directory) = crate::filesystem::directory(parent, true).map_err(invalid)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&serde_json::to_vec(binding).map_err(invalid)?)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| DevelopmentError::Io(error.error))?;
    directory.sync_all()?;
    Ok(())
}

fn matching_binding(
    path: &Path,
    binding: &ScopeBinding,
) -> Result<Option<ScopeBinding>, DevelopmentError> {
    let retained = read_binding(path)?;
    if retained.as_ref().is_some_and(|value| value != binding) {
        return Err(invalid("binding belongs to another Host scope"));
    }
    Ok(retained)
}

fn checked_live_scope(
    root: &Path,
    ledger: &Path,
    released: &Path,
    binding: &ScopeBinding,
) -> Result<bool, DevelopmentError> {
    if matching_binding(released, binding)?.is_some() {
        return Err(invalid(
            "Host scope was released; a deleted Session cannot prepare dependencies",
        ));
    }
    let registered = matching_binding(ledger, binding)?;
    let retained = matching_binding(&root.join(BINDING), binding)?;
    let root_present = present(root)?;
    if root_present && registered.is_none() {
        return Err(invalid(
            "scope reference has no durable ownership ledger; it was retained",
        ));
    }
    if registered.is_some() && (!root_present || retained.is_none()) {
        return Err(invalid(
            "live scope reference directory or ownership binding is missing; it was retained",
        ));
    }
    Ok(registered.is_some())
}

impl Library {
    /// Acquire a shared scope lease. Registration is a separate operation under
    /// the dependency writer lock, keeping the order scope lease -> writer lock.
    pub fn development_scope(
        &self,
        scope: &Path,
        create: bool,
    ) -> Result<Option<DevelopmentScope>, DevelopmentError> {
        let binding = scope_binding(scope)?;
        let id = scope_id(&binding);
        let dependencies = anchor_graph_host::resolved_library_root(&self.root)
            .join(anchor_graph_host::DEPENDENCIES_DIRECTORY);
        let leases = dependencies.join(LEASES_DIRECTORY);
        let root = dependencies.join(DEVELOPMENT_DIRECTORY).join(&id);
        if !create && !present(&leases)? {
            if present(&root)? {
                return Err(invalid(
                    "scope reference has no durable ownership registry; it was retained",
                ));
            }
            return Ok(None);
        }
        let (_, directory) = crate::filesystem::directory(&leases, create).map_err(invalid)?;
        let ledger = leases.join(format!("{id}.scope.json"));
        let released = leases.join(format!("{id}.released.json"));
        let registered = checked_live_scope(&root, &ledger, &released, &binding)?;
        if !create && !registered {
            return Ok(None);
        }
        // A registered lease must retain its inode. Recreating a missing file
        // could bypass a command still holding a lock on the removed inode.
        let lease = lock(
            &directory,
            &format!("{id}.lock"),
            FlockOperation::NonBlockingLockShared,
            create && !registered,
        )?;
        checked_live_scope(&root, &ledger, &released, &binding)?;
        Ok(Some(DevelopmentScope {
            root,
            ledger,
            released,
            binding,
            _lease: lease,
        }))
    }

    /// Release only the trusted Host scope, rejecting cleanup while a preparation
    /// or command still holds its shared lease. Tombstones prevent a stale caller
    /// recreating references between this return and filesystem Session cleanup.
    pub fn release_development_scope(&self, scope: &Path) -> Result<(), DevelopmentError> {
        let binding = scope_binding(scope)?;
        let id = scope_id(&binding);
        let dependencies = anchor_graph_host::resolved_library_root(&self.root)
            .join(anchor_graph_host::DEPENDENCIES_DIRECTORY);
        if !present(&dependencies)? {
            return Ok(());
        }
        let leases = dependencies.join(LEASES_DIRECTORY);
        let root = dependencies.join(DEVELOPMENT_DIRECTORY).join(&id);
        if !present(&leases)? {
            if present(&root)? {
                return Err(invalid(
                    "scope reference has no durable ownership registry; it was retained",
                ));
            }
            return Ok(());
        }
        let (_, lease_directory) = crate::filesystem::directory(&leases, false).map_err(invalid)?;
        let ledger = leases.join(format!("{id}.scope.json"));
        let released = leases.join(format!("{id}.released.json"));
        let lease_path = leases.join(format!("{id}.lock"));
        let registered = matching_binding(&ledger, &binding)?;
        let tombstone = matching_binding(&released, &binding)?;
        if !present(&root)? && registered.is_none() && tombstone.is_none() && !present(&lease_path)?
        {
            return Ok(());
        }
        let _scope = lock(
            &lease_directory,
            &format!("{id}.lock"),
            FlockOperation::NonBlockingLockExclusive,
            false,
        )?;
        let (_, dependencies_directory) =
            crate::filesystem::directory(&dependencies, false).map_err(invalid)?;
        let _writer = lock(
            &dependencies_directory,
            ".lock",
            FlockOperation::NonBlockingLockExclusive,
            true,
        )?;
        let registered = matching_binding(&ledger, &binding)?;
        let retained = matching_binding(&root.join(BINDING), &binding)?;
        let tombstone = matching_binding(&released, &binding)?;
        let root_present = present(&root)?;
        if registered.is_none() && root_present {
            return Err(invalid(
                "scope reference has no durable ownership ledger; it was retained",
            ));
        }
        if root_present && retained.is_none() && tombstone.is_none() {
            return Err(invalid(
                "scope reference has no readable ownership binding; it was retained",
            ));
        }
        // The Host's explicit Session cleanup is the release authority. A first
        // preparation cancelled before registration may have only its lease;
        // record its identity and release rather than trapping Session deletion.
        // A lost live tree remains protected until this explicit release.
        save_binding(&ledger, &binding)?;
        save_binding(&released, &binding)?;
        if root_present {
            crate::filesystem::directory(&root, false).map_err(invalid)?;
            std::fs::remove_dir_all(&root)?;
            if let Some(parent) = root.parent() {
                File::open(parent)?.sync_all()?;
            }
        }
        Ok(())
    }
}

impl DevelopmentScope {
    /// Call while holding the shared dependency writer lock.
    pub fn register(&self) -> Result<(), DevelopmentError> {
        // The ledger lives outside the temporary tree. Losing the whole view
        // directory can then never masquerade as releasing a live reference.
        checked_live_scope(&self.root, &self.ledger, &self.released, &self.binding)?;
        save_binding(&self.ledger, &self.binding)?;
        save_binding(&self.root.join(BINDING), &self.binding)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Publish the exact temporary view reference under the dependency writer
    /// lock, after its content has landed and before the writer releases it.
    pub fn record_view(&self, view: &Path) -> Result<(), DevelopmentError> {
        if matching_binding(&self.ledger, &self.binding)?.is_none()
            || matching_binding(&self.root.join(BINDING), &self.binding)?.is_none()
        {
            return Err(invalid("view reference has no durable ownership binding"));
        }
        let relative = view
            .strip_prefix(&self.root)
            .map_err(invalid)?
            .to_path_buf();
        if !valid_view_relative(&relative) {
            return Err(invalid("view is outside its Host scope"));
        }
        let reference = ViewReference {
            format: 1,
            view: relative,
        };
        let references = self.root.join("references");
        let (_, directory) = crate::filesystem::directory(&references, true).map_err(invalid)?;
        let filename = format!(
            "{:x}.json",
            Sha256::digest(reference.view.as_os_str().as_encoded_bytes())
        );
        let path = references.join(filename);
        let bytes = serde_json::to_vec(&reference).map_err(invalid)?;
        if present(&path)? {
            let retained = bounded_file(&path)?;
            if retained != bytes {
                return Err(invalid("view reference identity changed"));
            }
            return Ok(());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&references)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(&path)
            .map_err(|error| DevelopmentError::Io(error.error))?;
        directory.sync_all()?;
        Ok(())
    }
}

fn valid_view_relative(path: &Path) -> bool {
    !path.is_absolute()
        && path.file_name().is_some_and(|name| name == "view")
        && path.components().count() >= 2
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn bounded_file(path: &Path) -> Result<Vec<u8>, DevelopmentError> {
    let (_, parent) = crate::filesystem::directory(path.parent().expect("reference parent"), false)
        .map_err(invalid)?;
    let file = File::from(
        rustix::fs::openat(
            &parent,
            path.file_name().expect("reference name"),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    if !file.metadata()?.is_file() || file.metadata()?.len() > 16 * 1024 {
        return Err(invalid("reference is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err(invalid("reference exceeds its limit"));
    }
    Ok(bytes)
}

/// All temporary views whose durable Host ownership can be established. The
/// caller holds the dependency writer lock, so publication and release cannot
/// change the scanned reference set. Unknown shapes refuse package reclamation.
pub(crate) fn development_views(root: &Path) -> Result<Vec<PathBuf>, DevelopmentError> {
    let registry = registry(root)?;
    let directory = root
        .join(anchor_graph_host::DEPENDENCIES_DIRECTORY)
        .join(DEVELOPMENT_DIRECTORY);
    if !present(&directory)? {
        if registry.values().any(|(_, released)| !released) {
            return Err(invalid(
                "live development reference directory is missing; package bytes were retained",
            ));
        }
        return Ok(Vec::new());
    }
    crate::filesystem::directory(&directory, false).map_err(invalid)?;
    let mut views = Vec::new();
    for (id, (_, released)) in &registry {
        if !released && !present(&directory.join(id))? {
            return Err(invalid(
                "live development scope directory is missing; package bytes were retained",
            ));
        }
    }
    for entry in std::fs::read_dir(&directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            return Err(invalid("unknown scope reference was retained"));
        }
        let scope = entry.path();
        let binding = read_binding(&scope.join(BINDING))?
            .ok_or_else(|| invalid("scope reference has no ownership binding"))?;
        if entry.file_name() != std::ffi::OsStr::new(&scope_id(&binding)) {
            return Err(invalid("scope reference identity changed"));
        }
        if registry
            .get(&scope_id(&binding))
            .is_none_or(|(registered, _)| registered != &binding)
        {
            return Err(invalid(
                "development scope has no matching durable ledger ownership",
            ));
        }
        let references = scope.join("references");
        let mut recorded = std::collections::BTreeSet::new();
        if present(&references)? {
            crate::filesystem::directory(&references, false).map_err(invalid)?;
            for reference in std::fs::read_dir(&references)? {
                let reference = reference?;
                let fact: ViewReference =
                    serde_json::from_slice(&bounded_file(&reference.path())?).map_err(invalid)?;
                let name = format!(
                    "{:x}.json",
                    Sha256::digest(fact.view.as_os_str().as_encoded_bytes())
                );
                if fact.format != 1
                    || !valid_view_relative(&fact.view)
                    || reference.file_name() != std::ffi::OsStr::new(&name)
                {
                    return Err(invalid("temporary view reference identity changed"));
                }
                let view = scope.join(fact.view);
                crate::filesystem::directory(&view, false).map_err(invalid)?;
                let config = std::fs::symlink_metadata(view.join("pyvenv.cfg"))?;
                if !config.is_file() || config.file_type().is_symlink() {
                    return Err(invalid("temporary dependency view is incomplete"));
                }
                if !view.join("bin/python").is_file() {
                    return Err(invalid(
                        "temporary dependency view has no usable interpreter",
                    ));
                }
                recorded.insert(view);
            }
        }
        let mut observed = std::collections::BTreeSet::new();
        let mut pending = vec![scope];
        while let Some(current) = pending.pop() {
            for child in std::fs::read_dir(&current)? {
                let child = child?;
                let kind = child.file_type()?;
                if kind.is_symlink() {
                    return Err(invalid("scope reference contains an unexpected symlink"));
                }
                if kind.is_dir() {
                    if child.file_name() == "view" {
                        if !child.path().join("pyvenv.cfg").is_file() {
                            return Err(invalid("temporary dependency view is incomplete"));
                        }
                        observed.insert(child.path());
                    } else {
                        pending.push(child.path());
                    }
                } else if !kind.is_file() {
                    return Err(invalid("scope reference contains a special file"));
                }
            }
        }
        if observed != recorded {
            return Err(invalid(
                "temporary views and their durable references disagree; all package bytes were retained",
            ));
        }
        views.extend(recorded);
    }
    views.sort();
    Ok(views)
}

/// A released Host-owned scope makes a dependency-only Library an intentional
/// state, rather than a possibly mistyped/missing Plugin root. Legacy stores
/// without that fact keep the old conservative no-Plugin guard.
pub(crate) fn has_released_development(root: &Path) -> Result<bool, DevelopmentError> {
    Ok(registry(root)?.values().any(|(_, released)| *released))
}

fn registry(
    root: &Path,
) -> Result<std::collections::BTreeMap<String, (ScopeBinding, bool)>, DevelopmentError> {
    let leases = root
        .join(anchor_graph_host::DEPENDENCIES_DIRECTORY)
        .join(LEASES_DIRECTORY);
    if !present(&leases)? {
        return Ok(std::collections::BTreeMap::new());
    }
    crate::filesystem::directory(&leases, false).map_err(invalid)?;
    let mut found = std::collections::BTreeMap::new();
    let mut identities = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(&leases)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| invalid("unknown development registry entry"))?;
        let (id, kind) = if let Some(id) = name.strip_suffix(".scope.json") {
            (id, "scope")
        } else if let Some(id) = name.strip_suffix(".released.json") {
            (id, "released")
        } else if let Some(id) = name.strip_suffix(".lock") {
            (id, "lease")
        } else {
            return Err(invalid("unknown development registry entry was retained"));
        };
        if id.len() != 64
            || !id
                .bytes()
                .all(|value| value.is_ascii_hexdigit() && !value.is_ascii_uppercase())
        {
            return Err(invalid("development registry identity changed"));
        }
        identities.insert(id.to_owned());
        if kind == "lease" {
            if !bounded_file(&entry.path())?.is_empty() {
                return Err(invalid("development lease is not an empty regular file"));
            }
            continue;
        }
        let binding = read_binding(&entry.path())?
            .ok_or_else(|| invalid("development scope fact vanished"))?;
        if id != scope_id(&binding) {
            return Err(invalid("development scope identity changed"));
        }
        if kind == "released" {
            continue;
        }
        let released = read_binding(&leases.join(format!("{id}.released.json")))?;
        if released.as_ref().is_some_and(|value| value != &binding) {
            return Err(invalid("released scope identity changed"));
        }
        if !present(&leases.join(format!("{id}.lock")))? {
            return Err(invalid(
                "development scope lease is missing; package bytes were retained",
            ));
        }
        found.insert(id.to_owned(), (binding, released.is_some()));
    }
    if identities.iter().any(|id| !found.contains_key(id)) {
        return Err(invalid(
            "development scope has no durable ownership ledger; package bytes were retained",
        ));
    }
    Ok(found)
}
