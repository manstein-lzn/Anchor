use std::{
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags, mkdirat, openat, renameat_with, statat};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::assets::open_directory;

const MANIFEST: &str = include_str!("../../../plugins/docmost/plugin.json");
const SKILL: &[u8] = include_bytes!("../../../plugins/docmost/skills/docmost/SKILL.md");

#[derive(Debug, Error)]
pub enum PackageError {
    #[error("plugin destination must not exist or be a symlink")]
    Exists,
    #[error("plugin destination parent changed or is not an existing directory without symlinks")]
    Path,
    #[error("package executable identity changed or copied bytes are incomplete")]
    Identity,
    #[error("could not create native Docmost plugin package")]
    Write,
}

pub fn package_plugin(destination: impl AsRef<Path>) -> Result<(), PackageError> {
    let executable = std::env::current_exe().map_err(|_| PackageError::Identity)?;
    let running = File::open("/proc/self/exe").map_err(|_| PackageError::Identity)?;
    let identity = FileIdentity::from(&running.metadata().map_err(|_| PackageError::Identity)?);
    package_checked(destination.as_ref(), &executable, Some(identity), |_, _| {})
}

#[derive(Clone, Copy)]
enum Checkpoint {
    ParentOpened,
    Copied,
    BeforeRename,
    Published,
}

fn package_checked(
    destination: &Path,
    executable: &Path,
    running_identity: Option<FileIdentity>,
    mut checkpoint: impl FnMut(Checkpoint, &File),
) -> Result<(), PackageError> {
    let absolute = std::path::absolute(destination).map_err(|_| PackageError::Path)?;
    let parent = absolute.parent().ok_or(PackageError::Path)?;
    let destination_name = absolute.file_name().ok_or(PackageError::Path)?;
    let parent_fd = File::from(open_directory(parent).map_err(|_| PackageError::Path)?);
    match statat(&parent_fd, destination_name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => return Err(PackageError::Exists),
        Err(rustix::io::Errno::NOENT) => {}
        Err(_) => return Err(PackageError::Path),
    }
    let mut source = open_executable(executable)?;
    let source_metadata = source.metadata().map_err(|_| PackageError::Identity)?;
    let source_identity = FileIdentity::from(&source_metadata);
    if running_identity.is_some_and(|running| running != source_identity) {
        return Err(PackageError::Identity);
    }
    checkpoint(Checkpoint::ParentOpened, &parent_fd);
    let pinned_parent = descriptor_path(&parent_fd);
    let staging = tempfile::Builder::new()
        .prefix(".anchor-docmost-")
        .tempdir_in(&pinned_parent)
        .map_err(|_| PackageError::Write)?;
    let staging_name = staging.path().file_name().ok_or(PackageError::Write)?;
    let staging_fd = open_child_directory(&parent_fd, staging_name)?;
    let mut manifest: serde_json::Value =
        serde_json::from_str(MANIFEST).map_err(|_| PackageError::Write)?;
    let attachments = &mut manifest["mcpServers"]["attachments"];
    attachments["command"] = "bin/anchor-docmost-tools".into();
    attachments["args"] = serde_json::json!([]);
    attachments["cwd"] = ".".into();
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|_| PackageError::Write)?;
    write_file(&staging_fd, "plugin.json", &bytes)?;
    let skills = create_directory(&staging_fd, "skills")?;
    let skill = create_directory(&skills, "docmost")?;
    write_file(&skill, "SKILL.md", SKILL)?;
    let binary_directory = create_directory(&staging_fd, "bin")?;
    let mut binary = new_file(&binary_directory, "anchor-docmost-tools")?;
    let expected_digest = copy_executable(&mut source, &mut binary, source_metadata.len())?;
    binary
        .set_permissions(fs::Permissions::from_mode(source_metadata.mode() & 0o777))
        .map_err(|_| PackageError::Write)?;
    binary.sync_all().map_err(|_| PackageError::Write)?;
    checkpoint(Checkpoint::Copied, &binary);
    verify_copy(&mut binary, source_metadata.len(), &expected_digest)?;
    let copied_identity =
        FileIdentity::from(&binary.metadata().map_err(|_| PackageError::Identity)?);
    for directory in [&skill, &skills, &binary_directory, &staging_fd] {
        directory.sync_all().map_err(|_| PackageError::Write)?;
    }
    validate_parent(parent, &parent_fd)?;
    validate_source(executable, &source, source_identity)?;
    validate_entry(&parent_fd, staging_name, &staging_fd)?;
    validate_binary(&binary_directory, &binary, copied_identity)?;
    checkpoint(Checkpoint::BeforeRename, &parent_fd);
    renameat_with(
        &parent_fd,
        staging_name,
        &parent_fd,
        destination_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            PackageError::Exists
        } else {
            PackageError::Write
        }
    })?;
    checkpoint(Checkpoint::Published, &parent_fd);
    let published = (|| {
        validate_parent(parent, &parent_fd)?;
        validate_source(executable, &source, source_identity)?;
        validate_entry(&parent_fd, destination_name, &staging_fd)?;
        validate_binary(&binary_directory, &binary, copied_identity)?;
        parent_fd.sync_all().map_err(|_| PackageError::Write)
    })();
    if let Err(error) = published {
        if validate_entry(&parent_fd, destination_name, &staging_fd).is_ok() {
            fs::remove_dir_all(pinned_parent.join(destination_name))
                .map_err(|_| PackageError::Write)?;
        }
        return Err(error);
    }
    let _ = staging.keep();
    Ok(())
}

fn descriptor_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

fn new_file(directory: &File, name: &str) -> Result<File, PackageError> {
    openat(
        directory,
        name,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
    )
    .map(File::from)
    .map_err(|_| PackageError::Write)
}

fn write_file(directory: &File, name: &str, bytes: &[u8]) -> Result<(), PackageError> {
    let mut file = new_file(directory, name)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| PackageError::Write)
}

fn open_child_directory(directory: &File, name: impl AsRef<Path>) -> Result<File, PackageError> {
    openat(
        directory,
        name.as_ref(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| PackageError::Write)
}

fn create_directory(directory: &File, name: &str) -> Result<File, PackageError> {
    mkdirat(directory, name, Mode::from_raw_mode(0o755)).map_err(|_| PackageError::Write)?;
    open_child_directory(directory, name)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    mode: u32,
}

impl From<&Metadata> for FileIdentity {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            mode: metadata.mode(),
        }
    }
}

fn open_executable(path: &Path) -> Result<File, PackageError> {
    let parent = path.parent().ok_or(PackageError::Identity)?;
    let name = path.file_name().ok_or(PackageError::Identity)?;
    let directory = open_directory(parent).map_err(|_| PackageError::Identity)?;
    let file = openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| PackageError::Identity)?;
    let metadata = file.metadata().map_err(|_| PackageError::Identity)?;
    if !metadata.is_file() || metadata.mode() & 0o111 == 0 || metadata.len() == 0 {
        return Err(PackageError::Identity);
    }
    Ok(file)
}

fn validate_source(path: &Path, file: &File, expected: FileIdentity) -> Result<(), PackageError> {
    let current = open_executable(path)?;
    for source in [file, &current] {
        if FileIdentity::from(&source.metadata().map_err(|_| PackageError::Identity)?) != expected {
            return Err(PackageError::Identity);
        }
    }
    Ok(())
}

fn validate_parent(path: &Path, directory: &File) -> Result<(), PackageError> {
    let current = File::from(open_directory(path).map_err(|_| PackageError::Path)?);
    let before = directory.metadata().map_err(|_| PackageError::Path)?;
    let after = current.metadata().map_err(|_| PackageError::Path)?;
    if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
        return Err(PackageError::Path);
    }
    Ok(())
}

fn validate_entry(
    directory: &File,
    name: impl AsRef<Path>,
    file: &File,
) -> Result<(), PackageError> {
    let entry = statat(directory, name.as_ref(), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| PackageError::Identity)?;
    let opened = file.metadata().map_err(|_| PackageError::Identity)?;
    if (entry.st_dev, entry.st_ino, entry.st_mode) != (opened.dev(), opened.ino(), opened.mode()) {
        return Err(PackageError::Identity);
    }
    Ok(())
}

fn validate_binary(
    directory: &File,
    file: &File,
    expected: FileIdentity,
) -> Result<(), PackageError> {
    validate_entry(directory, "anchor-docmost-tools", file)?;
    if FileIdentity::from(&file.metadata().map_err(|_| PackageError::Identity)?) != expected {
        return Err(PackageError::Identity);
    }
    Ok(())
}

fn copy_executable(
    source: &mut File,
    output: &mut File,
    length: u64,
) -> Result<[u8; 32], PackageError> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65536];
    let mut reader = source.take(length.saturating_add(1));
    let mut copied = 0_u64;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| PackageError::Identity)?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|_| PackageError::Write)?;
        digest.update(&buffer[..count]);
        copied += count as u64;
    }
    if copied != length {
        return Err(PackageError::Identity);
    }
    Ok(digest.finalize().into())
}

fn verify_copy(file: &mut File, length: u64, expected: &[u8; 32]) -> Result<(), PackageError> {
    if file.metadata().map_err(|_| PackageError::Identity)?.len() != length {
        return Err(PackageError::Identity);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| PackageError::Write)?;
    let mut digest = Sha256::new();
    let mut reader = file.take(length.saturating_add(1));
    let mut buffer = [0_u8; 65536];
    loop {
        let count = reader.read(&mut buffer).map_err(|_| PackageError::Write)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if <[u8; 32]>::from(digest.finalize()) != *expected {
        return Err(PackageError::Identity);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
