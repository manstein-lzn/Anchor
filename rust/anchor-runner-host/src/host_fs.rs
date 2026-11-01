//! Filesystem primitives shared by Host storage and runtime services.

pub use anchor_runtime::artifact::store::{invocation_stem, validate_component, validate_key};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

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
