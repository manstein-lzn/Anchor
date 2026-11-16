use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

use rustix::fs::{
    AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, RenameFlags, mkdirat, openat,
    renameat_with, statat,
};

use crate::InstallError;

pub(crate) fn directory(path: &Path, create: bool) -> Result<(PathBuf, File), InstallError> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut file =
        File::from(openat(rustix::fs::CWD, "/", flags, Mode::empty()).map_err(io::Error::from)?);
    let mut normalized = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                let opened = match openat(&file, name, flags, Mode::empty()) {
                    Err(rustix::io::Errno::NOENT) if create => {
                        match mkdirat(&file, name, Mode::from_bits_truncate(0o700)) {
                            Ok(()) => file.sync_all()?,
                            Err(rustix::io::Errno::EXIST) => {}
                            Err(error) => return Err(io::Error::from(error).into()),
                        }
                        openat(&file, name, flags, Mode::empty())
                    }
                    result => result,
                };
                file = File::from(opened.map_err(path_error)?);
                normalized.push(name);
            }
            _ => return Err(InstallError::UnsafePath),
        }
    }
    Ok((normalized, file))
}

fn path_error(error: rustix::io::Errno) -> InstallError {
    match error {
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => InstallError::UnsafePath,
        _ => io::Error::from(error).into(),
    }
}

pub(crate) fn lease(
    plugins: &File,
    operation: FlockOperation,
) -> Result<anchor_file_lease::FileLease, InstallError> {
    lease_named(plugins, ".install.lock", operation, true)
}

pub(crate) fn lease_named(
    directory: &File,
    name: &str,
    operation: FlockOperation,
    create: bool,
) -> Result<anchor_file_lease::FileLease, InstallError> {
    let lock = File::from(
        openat(
            directory,
            name,
            OFlags::RDWR
                | if create {
                    OFlags::CREATE
                } else {
                    OFlags::empty()
                }
                | OFlags::CLOEXEC
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(path_error)?,
    );
    let metadata = lock.metadata()?;
    if !metadata.is_file() || metadata.len() != 0 || metadata.nlink() != 1 {
        return Err(InstallError::UnsafePath);
    }
    let lock = anchor_file_lease::FileLease::acquire(lock, operation).map_err(|failure| {
        if failure == rustix::io::Errno::WOULDBLOCK {
            InstallError::CatalogBusy
        } else {
            InstallError::Io(io::Error::from(failure))
        }
    })?;
    Ok(lock)
}

pub(crate) fn existing_destination(
    destination: &Path,
    replace_existing: bool,
) -> Result<bool, InstallError> {
    match fs::symlink_metadata(destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
        Ok(metadata) if metadata.is_dir() => {
            if !replace_existing {
                return Err(InstallError::AlreadyExists);
            }
            let (_, existing) = directory(destination, false)?;
            // Only the *declared* mutable paths are pruned: everything else, and
            // everything when there is no declaration, stays under the platform's
            // checks. The declaration read here is the installed one — it is the
            // only authority for what the tree that exists means.
            validate_tree(&existing, "", &installed_mutable_paths(destination)?)?;
            Ok(true)
        }
        Ok(_) => Err(InstallError::UnsafePath),
    }
}

fn entries(
    directory: &File,
    prefix: &str,
    mutable: &anchor_graph_host::MutablePaths,
) -> Result<Vec<(String, FileType)>, InstallError> {
    let mut entries = Vec::new();
    for entry in Dir::read_from(directory).map_err(io::Error::from)? {
        let entry = entry.map_err(io::Error::from)?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = OsStr::from_bytes(bytes)
            .to_str()
            .ok_or(InstallError::UnsafePath)?;
        let relative = relative_path(prefix, name);
        // The declaration owns this location. Do not even inspect its type:
        // Plugin data can include links, credentials, FIFOs, or live files.
        if mutable.covers(&relative) {
            continue;
        }
        if name.contains('\\') {
            return Err(InstallError::UnsafePath);
        }
        let metadata =
            statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
        let kind = FileType::from_raw_mode(metadata.st_mode);
        if kind != FileType::RegularFile && kind != FileType::Directory {
            return Err(InstallError::UnsafePath);
        }
        entries.push((name.to_owned(), kind));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

fn relative_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

fn child_directory(parent: &File, name: &str) -> Result<File, InstallError> {
    Ok(File::from(
        openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(path_error)?,
    ))
}

fn regular_file(parent: &File, name: &str) -> Result<File, InstallError> {
    let file = File::from(
        openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(path_error)?,
    );
    if !file.metadata()?.is_file() {
        return Err(InstallError::UnsafePath);
    }
    Ok(file)
}

/// The mutable paths the *installed* Plugin declared, if it is readable.
///
/// An unreadable or missing manifest means "no declaration": the platform then
/// validates the whole tree, which is the fail-closed direction.
fn installed_mutable_paths(
    destination: &Path,
) -> Result<anchor_graph_host::MutablePaths, InstallError> {
    let manifest = match fs::read(destination.join("plugin.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(anchor_graph_host::MutablePaths::default());
        }
        Err(error) => return Err(error.into()),
    };
    let value: serde_json::Value = match serde_json::from_slice(&manifest) {
        Ok(value) => value,
        Err(_) => return Ok(anchor_graph_host::MutablePaths::default()),
    };
    // A declaration that cannot be parsed is *no* declaration here: the tree is
    // then validated in full (fail-closed), and the catalog reports the malformed
    // manifest with its own error instead of this check preempting it.
    Ok(anchor_graph_host::mutable_paths_from_manifest(&value).unwrap_or_default())
}

/// Check one installed tree, skipping the declared mutable paths.
fn validate_tree(
    source: &File,
    prefix: &str,
    mutable: &anchor_graph_host::MutablePaths,
) -> Result<(), InstallError> {
    for (name, kind) in entries(source, prefix, mutable)? {
        let relative = relative_path(prefix, &name);
        if kind == FileType::Directory {
            validate_tree(&child_directory(source, &name)?, &relative, mutable)?;
        }
    }
    Ok(())
}

pub(crate) fn stage(source: &Path, destination: &Path) -> Result<(), InstallError> {
    let (source, source_file) = directory(source, false)?;
    if destination.starts_with(&source) {
        return Err(InstallError::UnsafePath);
    }
    if let Ok(metadata) = statat(&source_file, ".codex-plugin", AtFlags::SYMLINK_NOFOLLOW)
        && FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
    {
        return Err(InstallError::UnsafePath);
    }
    let mut manifest = match statat(&source_file, "plugin.json", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) if FileType::from_raw_mode(metadata.st_mode) == FileType::RegularFile => {
            regular_file(&source_file, "plugin.json")?
        }
        Ok(_) => return Err(InstallError::InvalidPlugin),
        Err(rustix::io::Errno::NOENT) => {
            let hidden =
                child_directory(&source_file, ".codex-plugin").map_err(|error| match error {
                    InstallError::Io(ref io_error)
                        if io_error.kind() == io::ErrorKind::NotFound =>
                    {
                        InstallError::MissingManifest
                    }
                    error => error,
                })?;
            regular_file(&hidden, "plugin.json").map_err(|error| match error {
                InstallError::Io(ref io_error) if io_error.kind() == io::ErrorKind::NotFound => {
                    InstallError::MissingManifest
                }
                error => error,
            })?
        }
        Err(error) => return Err(io::Error::from(error).into()),
    };
    // Use the manifest actually selected above, including the community layout
    // with `.codex-plugin/plugin.json`. Invalid declarations provide no pruning;
    // the catalog still reports them through each caller's existing error map.
    // The workspace can still be changing while it is staged. Read the selected
    // manifest once so the declaration used for pruning is also the one we publish.
    let mut manifest_bytes = Vec::new();
    manifest.read_to_end(&mut manifest_bytes)?;
    let manifest_mode = manifest.metadata()?.permissions().mode() & 0o777;
    let mutable = serde_json::from_slice::<serde_json::Value>(&manifest_bytes)
        .ok()
        .and_then(|value| anchor_graph_host::mutable_paths_from_manifest(&value).ok())
        .unwrap_or_default();
    validate_tree(&source_file, "", &mutable)?;
    fs::create_dir(destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
    copy_tree(&source_file, destination, "", &mutable)?;
    let root_manifest = destination.join("plugin.json");
    write_file(manifest_bytes.as_slice(), manifest_mode, &root_manifest)?;
    let mode = (source_file.metadata()?.permissions().mode() & 0o777) | 0o700;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    // Staging serves review and candidate execution as well as publication.
    // Each gets empty data locations; no source data enters any of these trees.
    crate::provide_mutable_paths(destination, &mutable)?;
    Ok(())
}

#[cfg(test)]
#[path = "filesystem/tests.rs"]
mod tests;

fn copy_tree(
    source: &File,
    destination: &Path,
    prefix: &str,
    mutable: &anchor_graph_host::MutablePaths,
) -> Result<(), InstallError> {
    for (name, kind) in entries(source, prefix, mutable)? {
        if name == ".git"
            || (prefix.is_empty() && (name == ".codex-plugin" || name == "plugin.json"))
        {
            continue;
        }
        let target = destination.join(&name);
        if kind == FileType::Directory {
            let child = child_directory(source, &name)?;
            fs::create_dir(&target)?;
            copy_tree(&child, &target, &relative_path(prefix, &name), mutable)?;
            let mode = (child.metadata()?.permissions().mode() & 0o777) | 0o700;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode))?;
        } else {
            copy_file(regular_file(source, &name)?, &target)?;
        }
    }
    Ok(())
}

fn copy_file(source: File, destination: &Path) -> Result<(), InstallError> {
    let mode = source.metadata()?.permissions().mode() & 0o777;
    write_file(source, mode, destination)
}

fn write_file(mut source: impl Read, mode: u32, destination: &Path) -> Result<(), InstallError> {
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    io::copy(&mut source, &mut target)?;
    target.set_permissions(fs::Permissions::from_mode(mode))?;
    target.sync_all()?;
    Ok(())
}

pub(crate) fn sync_tree(path: &Path) -> Result<(), InstallError> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            File::open(entry.path())?.sync_all()?;
        }
    }
    File::open(path)?.sync_all()?;
    Ok(())
}

pub(crate) fn sync_publication(staged_parent: &File, plugins: &File) -> Result<(), InstallError> {
    plugins.sync_all()?;
    staged_parent.sync_all()?;
    Ok(())
}

pub(crate) fn publish(
    staged_parent: &File,
    plugins: &File,
    id: &str,
    replacing: bool,
    mut sync: impl FnMut(&File, &File) -> Result<(), InstallError>,
) -> Result<(), InstallError> {
    let flags = if replacing {
        RenameFlags::EXCHANGE
    } else {
        RenameFlags::NOREPLACE
    };
    renameat_with(staged_parent, id, plugins, id, flags).map_err(|error| match error {
        rustix::io::Errno::EXIST => InstallError::AlreadyExists,
        error => io::Error::from(error).into(),
    })?;
    if let Err(error) = sync(staged_parent, plugins) {
        let restored = if replacing {
            renameat_with(staged_parent, id, plugins, id, RenameFlags::EXCHANGE)
        } else {
            renameat_with(plugins, id, staged_parent, id, RenameFlags::NOREPLACE)
        };
        if restored.is_err() || sync(staged_parent, plugins).is_err() {
            return Err(InstallError::PublicationUncertain);
        }
        return Err(error);
    }
    Ok(())
}

/// One top-most entry of a Plugin tree that is not a declared mutable path.
///
/// A code replacement works on these, never on a file inside a mutable subtree:
/// the platform replaces code and leaves everything the Plugin declared as its own
/// data exactly where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodeEntry {
    pub(crate) relative: PathBuf,
}

/// The top-most entries of one Plugin tree that are not mutable.
///
/// "Top-most" means an entry is returned instead of its children, except where a
/// declared mutable path lives *under* it: then the directory is descended into,
/// because exchanging it whole would move the Plugin's data. An entry that is
/// itself mutable is never returned.
pub(crate) fn code_entries(
    root: &Path,
    mutable: &anchor_graph_host::MutablePaths,
) -> Result<Vec<CodeEntry>, InstallError> {
    let mut entries = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let directory = root.join(&relative);
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let child = relative.join(&name);
            let relative_text = child.to_string_lossy().into_owned();
            if mutable.covers(&relative_text) {
                continue;
            }
            if entry.file_type()?.is_dir() && mutable_under(mutable, &relative_text) {
                pending.push(child);
                continue;
            }
            entries.push(CodeEntry { relative: child });
        }
    }
    entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(entries)
}

/// Whether a declared mutable path lives strictly under this relative path.
fn mutable_under(mutable: &anchor_graph_host::MutablePaths, relative: &str) -> bool {
    let prefix = format!("{relative}/");
    mutable
        .paths()
        .iter()
        .any(|declared| declared.starts_with(&prefix))
}

/// Replace a Plugin's code in place, one entry at a time.
///
/// Mutable paths are neither listed, renamed, copied nor verified: they stay
/// exactly as the Plugin left them. Each code entry is exchanged atomically
/// (`RENAME_EXCHANGE`) or created/removed, so a crash can leave a mix of old and
/// new code entries — the standard records that as an accepted cost — but never a
/// touched mutable path.
pub(crate) fn replace_code(
    staged_root: &Path,
    destination: &Path,
    old_mutable: &anchor_graph_host::MutablePaths,
    new_mutable: &anchor_graph_host::MutablePaths,
) -> Result<(), InstallError> {
    let staged = code_entries(staged_root, new_mutable)?;
    let installed = code_entries(destination, old_mutable)?;
    let mut plan = Vec::new();
    for entry in &staged {
        let relative_text = entry.relative.to_string_lossy().into_owned();
        // A code entry may not land inside a mutable path *either* declaration
        // names: the older declaration's data would be overwritten, or the newer
        // declaration's data would be replaced by code the author no longer
        // declares as code. That is a decision for a human, not for a rename.
        if old_mutable.covers(&relative_text) {
            // New code wants a path the *previous* declaration designated as the
            // Plugin's own data. Replacing it would destroy data, so a human
            // decides instead.
            return Err(InstallError::MutableConflict {
                entry: relative_text,
                changed: String::new(),
            });
        }
        plan.push(CodeStep::Place(entry.relative.clone()));
    }
    let staged_relative = staged
        .iter()
        .map(|entry| entry.relative.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for entry in &installed {
        if staged_relative.contains(&entry.relative) {
            continue;
        }
        let relative_text = entry.relative.to_string_lossy().into_owned();
        if new_mutable.covers(&relative_text) {
            // It was code and is now the Plugin's own data: leave it where it is.
            continue;
        }
        plan.push(CodeStep::Remove(entry.relative.clone()));
    }

    let mut changed = Vec::new();
    for step in plan {
        let outcome = match &step {
            CodeStep::Place(relative) => place(staged_root, destination, relative),
            CodeStep::Remove(relative) => remove(destination, relative),
        };
        if let Err(error) = outcome {
            return Err(InstallError::PartialReplacement {
                changed: changed.join(", "),
                reason: describe(&error),
            });
        }
        changed.push(step.relative().to_string_lossy().into_owned());
    }
    Ok(())
}

/// A step failure's reason, with the filesystem detail that the public error text
/// deliberately keeps out of its own message.
fn describe(error: &InstallError) -> String {
    match error {
        InstallError::Io(io) => format!("filesystem operation failed: {io}"),
        other => other.to_string(),
    }
}

enum CodeStep {
    Place(PathBuf),
    Remove(PathBuf),
}

impl CodeStep {
    fn relative(&self) -> &Path {
        match self {
            Self::Place(relative) | Self::Remove(relative) => relative,
        }
    }
}

/// Put one staged code entry in place, exchanging an existing one atomically.
fn place(staged_root: &Path, destination: &Path, relative: &Path) -> Result<(), InstallError> {
    let source = staged_root.join(relative);
    let target = destination.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let paired = if target.symlink_metadata().is_ok() {
        RenameFlags::EXCHANGE
    } else {
        RenameFlags::NOREPLACE
    };
    rustix::fs::renameat_with(rustix::fs::CWD, &source, rustix::fs::CWD, &target, paired).map_err(
        |error| match error {
            rustix::io::Errno::EXIST => InstallError::AlreadyExists,
            error => io::Error::from(error).into(),
        },
    )
}

/// Move one obsolete code entry out of the Plugin tree.
fn remove(destination: &Path, relative: &Path) -> Result<(), InstallError> {
    let target = destination.join(relative);
    let discarded = destination.join(format!(
        ".removed-{}-{}",
        std::process::id(),
        relative
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    fs::rename(&target, &discarded)?;
    if discarded.is_dir() {
        fs::remove_dir_all(&discarded)?;
    } else {
        fs::remove_file(&discarded)?;
    }
    Ok(())
}
