//! Filesystem primitives shared by Host storage and runtime services.

pub use anchor_runtime::artifact::store::{invocation_stem, validate_component, validate_key};
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Open an existing directory without following links in any component.
pub(crate) fn open_directory(path: &Path) -> io::Result<fs::File> {
    anchor_file_lease::open_directory(&std::path::absolute(path)?)
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), String> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

/// Publish an immutable directory or file without replacing a concurrent winner.
pub(crate) fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        target,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(Into::into)
}

/// Resolve an author-selected candidate without granting host filesystem paths.
pub(crate) fn workspace_directory(
    workspace: &Path,
    relative: &str,
) -> Result<PathBuf, anchor_runtime::ToolError> {
    let rejected = |reason: &str| anchor_runtime::ToolError::Failed(reason.into());
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.contains('\0')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(rejected(
            "directory must be a workspace-relative path without `.` or `..` components",
        ));
    }
    let root = workspace
        .canonicalize()
        .map_err(|_| rejected("the assistant workspace is unavailable"))?;
    let candidate = root
        .join(path)
        .canonicalize()
        .map_err(|_| rejected("the candidate directory does not exist"))?;
    if !candidate.starts_with(&root) || !candidate.is_dir() {
        return Err(rejected("the candidate directory escapes the workspace"));
    }
    Ok(candidate)
}

/// Resolve a deployment executable without launching it.
pub(crate) fn executable(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("must be an absolute path".into());
    }
    let path = path
        .canonicalize()
        .map_err(|error| format!("is unavailable: {error}"))?;
    let metadata = fs::metadata(&path).map_err(|error| format!("is unavailable: {error}"))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err("must be an executable regular file".into());
    }
    Ok(path)
}

/// Create durable private directories without following symlinks in the path.
/// Error text is retained from the original Pilot helper for existing callers.
pub(crate) fn directory(root: &Path) -> Result<(), String> {
    use rustix::fs::{Mode, OFlags, fsync, mkdirat, open, openat};
    use std::path::Component;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut parent = open("/", flags, Mode::empty()).map_err(|_| "Pilot directory unavailable")?;
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                match mkdirat(&parent, name, Mode::RWXU) {
                    Ok(()) => {
                        fsync(&parent).map_err(|_| "Pilot directory could not be persisted")?
                    }
                    Err(error) if error == rustix::io::Errno::EXIST => {}
                    Err(_) => return Err("Pilot directory unavailable".into()),
                }
                parent = openat(&parent, name, flags, Mode::empty())
                    .map_err(|_| "Pilot directory must not contain symlinks")?;
            }
            _ => return Err("Pilot directory must be an absolute host path".into()),
        }
    }
    Ok(())
}
