use rustix::fs::{Mode, OFlags, RenameFlags, open, renameat_with};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

const MANIFEST: &str = include_str!("../../../plugins/wecom/plugin.json");
const SKILL: &str = include_str!("../../../plugins/wecom/skills/wecom/SKILL.md");

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("package destination must be new with an existing non-symlink parent")]
    Destination,
    #[error("package binary identity changed or is not a regular executable")]
    Identity,
    #[error("package filesystem operation failed")]
    Filesystem,
    #[error("embedded Plugin manifest is invalid")]
    Manifest,
}

pub fn package_plugin(destination: impl AsRef<Path>) -> Result<(), PackageError> {
    let executable = std::env::current_exe().map_err(|_| PackageError::Identity)?;
    let running = fs::metadata("/proc/self/exe").map_err(|_| PackageError::Identity)?;
    package_from(destination.as_ref(), &executable, Some(running))
}

fn destination_path(destination: &Path) -> Result<PathBuf, PackageError> {
    let absolute = if destination.is_absolute() {
        destination.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| PackageError::Filesystem)?
            .join(destination)
    };
    if absolute.file_name().is_none()
        || absolute
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        || fs::symlink_metadata(&absolute).is_ok()
    {
        return Err(PackageError::Destination);
    }
    let parent = absolute.parent().ok_or(PackageError::Destination)?;
    let mut prefix = PathBuf::new();
    for component in parent.components() {
        prefix.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&prefix).map_err(|_| PackageError::Destination)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(PackageError::Destination);
        }
    }
    Ok(absolute)
}

fn identity(metadata: &Metadata) -> (u64, u64, u64, i64, i64, i64, i64, u32) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
        metadata.mode(),
    )
}

fn new_file(path: &Path, bytes: &[u8]) -> Result<(), PackageError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| PackageError::Filesystem)?;
    file.write_all(bytes)
        .map_err(|_| PackageError::Filesystem)?;
    file.sync_all().map_err(|_| PackageError::Filesystem)
}

fn package_from(
    destination: &Path,
    executable: &Path,
    running: Option<Metadata>,
) -> Result<(), PackageError> {
    package_checked(destination, executable, running, || {})
}

fn package_checked(
    destination: &Path,
    executable: &Path,
    running: Option<Metadata>,
    before_publish: impl FnOnce(),
) -> Result<(), PackageError> {
    let destination = destination_path(destination)?;
    let parent = destination.parent().ok_or(PackageError::Destination)?;
    let parent_fd = File::from(
        open(
            parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| PackageError::Destination)?,
    );
    let descriptor = open(
        executable,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| PackageError::Identity)?;
    let mut source = File::from(descriptor);
    let before = source.metadata().map_err(|_| PackageError::Identity)?;
    if !before.is_file() || before.mode() & 0o111 == 0 {
        return Err(PackageError::Identity);
    }
    if running.is_some_and(|running| identity(&running) != identity(&before)) {
        return Err(PackageError::Identity);
    }
    let staging = tempfile::Builder::new()
        .prefix(".anchor-wecom-package-")
        .tempdir_in(parent)
        .map_err(|_| PackageError::Filesystem)?;
    let bin = staging.path().join("bin");
    fs::create_dir(&bin).map_err(|_| PackageError::Filesystem)?;
    let binary = bin.join("anchor-wecom-tools");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&binary)
        .map_err(|_| PackageError::Filesystem)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|_| PackageError::Identity)?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|_| PackageError::Filesystem)?;
        digest.update(&buffer[..count]);
    }
    output
        .set_permissions(fs::Permissions::from_mode(0o755))
        .map_err(|_| PackageError::Filesystem)?;
    output.sync_all().map_err(|_| PackageError::Filesystem)?;
    let mut copied = File::open(&binary).map_err(|_| PackageError::Filesystem)?;
    let mut copied_digest = Sha256::new();
    loop {
        let count = copied
            .read(&mut buffer)
            .map_err(|_| PackageError::Filesystem)?;
        if count == 0 {
            break;
        }
        copied_digest.update(&buffer[..count]);
    }
    if digest.finalize() != copied_digest.finalize() {
        return Err(PackageError::Identity);
    }
    let mut manifest: serde_json::Value =
        serde_json::from_str(MANIFEST).map_err(|_| PackageError::Manifest)?;
    let server = manifest
        .get_mut("mcpServers")
        .and_then(|servers| servers.get_mut("wecom"))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(PackageError::Manifest)?;
    server.insert("command".into(), "bin/anchor-wecom-tools".into());
    server.insert("args".into(), serde_json::json!([]));
    server.insert("cwd".into(), ".".into());
    let manifest = serde_json::to_vec_pretty(&manifest).map_err(|_| PackageError::Manifest)?;
    new_file(&staging.path().join("plugin.json"), &manifest)?;
    let skill = staging.path().join("skills/wecom");
    fs::create_dir_all(&skill).map_err(|_| PackageError::Filesystem)?;
    new_file(&skill.join("SKILL.md"), SKILL.as_bytes())?;
    before_publish();
    let after = source.metadata().map_err(|_| PackageError::Identity)?;
    let current = fs::symlink_metadata(executable).map_err(|_| PackageError::Identity)?;
    if identity(&before) != identity(&after) || identity(&before) != identity(&current) {
        return Err(PackageError::Identity);
    }
    let current_parent = fs::symlink_metadata(parent).map_err(|_| PackageError::Destination)?;
    let opened_parent = parent_fd
        .metadata()
        .map_err(|_| PackageError::Destination)?;
    if current_parent.dev() != opened_parent.dev() || current_parent.ino() != opened_parent.ino() {
        return Err(PackageError::Destination);
    }
    renameat_with(
        &parent_fd,
        staging
            .path()
            .file_name()
            .ok_or(PackageError::Destination)?,
        &parent_fd,
        destination.file_name().ok_or(PackageError::Destination)?,
        RenameFlags::NOREPLACE,
    )
    .map_err(|_| PackageError::Destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::write(&source, b"fixture executable").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        (temporary, source)
    }

    #[test]
    fn changed_source_identity_never_publishes_partial_output() {
        for replace in [false, true] {
            let (temporary, source) = fixture();
            let destination = temporary.path().join("native");
            let result = package_checked(&destination, &source, None, || {
                if replace {
                    let replacement = temporary.path().join("replacement");
                    fs::write(&replacement, b"replacement executable").unwrap();
                    fs::rename(replacement, &source).unwrap();
                } else {
                    fs::write(&source, b"mutated executable").unwrap();
                }
            });
            assert!(matches!(result, Err(PackageError::Identity)));
            assert!(!destination.exists());
            assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn publication_collision_preserves_other_writer_and_removes_staging() {
        let (temporary, source) = fixture();
        let destination = temporary.path().join("native");
        let result = package_checked(&destination, &source, None, || {
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("sentinel"), b"other writer").unwrap();
        });
        assert!(matches!(result, Err(PackageError::Destination)));
        assert_eq!(
            fs::read(destination.join("sentinel")).unwrap(),
            b"other writer"
        );
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 2);
    }

    #[test]
    fn source_symlinks_and_nonexecutables_are_rejected() {
        let (temporary, source) = fixture();
        let destination = temporary.path().join("native");
        let link = temporary.path().join("link");
        symlink(&source, &link).unwrap();
        assert!(matches!(
            package_from(&destination, &link, None),
            Err(PackageError::Identity)
        ));
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            package_from(&destination, &source, None),
            Err(PackageError::Identity)
        ));
        assert!(!destination.exists());
    }

    #[test]
    fn executable_path_must_still_identify_the_running_image() {
        let (temporary, source) = fixture();
        let other = temporary.path().join("running-image");
        fs::copy(&source, &other).unwrap();
        let destination = temporary.path().join("native");
        let result = package_from(&destination, &source, Some(fs::metadata(other).unwrap()));
        assert!(matches!(result, Err(PackageError::Identity)));
        assert!(!destination.exists());
    }
}
