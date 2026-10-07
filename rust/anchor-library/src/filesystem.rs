use std::{
    ffi::OsStr,
    fs::{self, File},
    io,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Component, Path, PathBuf},
};

use rustix::fs::{
    AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, RenameFlags, flock, mkdirat, openat,
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

pub(crate) fn lease(plugins: &File, operation: FlockOperation) -> Result<File, InstallError> {
    let lock = File::from(
        openat(
            plugins,
            ".install.lock",
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(path_error)?,
    );
    if !lock.metadata()?.is_file() {
        return Err(InstallError::UnsafePath);
    }
    flock(&lock, operation).map_err(|failure| {
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
            validate_tree(&existing)?;
            Ok(true)
        }
        Ok(_) => Err(InstallError::UnsafePath),
    }
}

fn entries(directory: &File) -> Result<Vec<(String, FileType)>, InstallError> {
    let mut entries = Vec::new();
    for entry in Dir::read_from(directory).map_err(io::Error::from)? {
        let entry = entry.map_err(io::Error::from)?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = OsStr::from_bytes(bytes)
            .to_str()
            .filter(|name| !name.contains('\\'))
            .ok_or(InstallError::UnsafePath)?;
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

fn validate_tree(source: &File) -> Result<(), InstallError> {
    for (name, kind) in entries(source)? {
        if kind == FileType::Directory {
            validate_tree(&child_directory(source, &name)?)?;
        }
    }
    Ok(())
}

pub(crate) fn stage(source: &Path, destination: &Path) -> Result<(), InstallError> {
    let (source, source_file) = directory(source, false)?;
    if destination.starts_with(&source) {
        return Err(InstallError::UnsafePath);
    }
    validate_tree(&source_file)?;
    if let Ok(metadata) = statat(&source_file, ".codex-plugin", AtFlags::SYMLINK_NOFOLLOW)
        && FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
    {
        return Err(InstallError::UnsafePath);
    }
    let manifest = match statat(&source_file, "plugin.json", AtFlags::SYMLINK_NOFOLLOW) {
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
    fs::create_dir(destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
    copy_tree(&source_file, destination, true)?;
    let root_manifest = destination.join("plugin.json");
    copy_file(manifest, &root_manifest)?;
    let mode = (source_file.metadata()?.permissions().mode() & 0o777) | 0o700;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(test)]
#[path = "filesystem/tests.rs"]
mod tests;

fn copy_tree(source: &File, destination: &Path, root: bool) -> Result<(), InstallError> {
    for (name, kind) in entries(source)? {
        if name == ".git" || (root && (name == ".codex-plugin" || name == "plugin.json")) {
            continue;
        }
        let target = destination.join(&name);
        if kind == FileType::Directory {
            let child = child_directory(source, &name)?;
            fs::create_dir(&target)?;
            copy_tree(&child, &target, false)?;
            let mode = (child.metadata()?.permissions().mode() & 0o777) | 0o700;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode))?;
        } else {
            copy_file(regular_file(source, &name)?, &target)?;
        }
    }
    Ok(())
}

fn copy_file(mut source: File, destination: &Path) -> Result<(), InstallError> {
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    io::copy(&mut source, &mut target)?;
    let mode = source.metadata()?.permissions().mode() & 0o777;
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
