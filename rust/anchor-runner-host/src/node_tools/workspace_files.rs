//! Workspace file IO uses directory capabilities, never a checked host pathname.

use rustix::fs::{Mode, OFlags, openat, renameat};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    os::fd::AsRawFd,
    path::{Component, Path, PathBuf},
};

pub(super) struct WorkspaceFiles {
    directory: File,
    canonical: PathBuf,
    excluded: Vec<PathBuf>,
}

impl WorkspaceFiles {
    pub(super) fn open(workspace: &Path) -> io::Result<Self> {
        let canonical = workspace.canonicalize()?;
        let relative = canonical.strip_prefix("/").map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace root must be absolute",
            )
        })?;
        let directory = open_beneath(&File::open("/")?, relative, true)?;
        Ok(Self {
            directory,
            canonical,
            excluded: Vec::new(),
        })
    }

    pub(super) fn with_excluded_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.excluded = paths;
        self
    }

    pub(super) fn read(&self, relative: &Path) -> io::Result<Vec<u8>> {
        // Preserve reads through internal symlinks (including absolute internal
        // links). The canonical path selects a relative name, not an IO path:
        // each component is reopened under the pinned root with NOFOLLOW, so a
        // concurrent directory/symlink substitution cannot escape the workspace.
        let resolved = self.canonical.join(relative).canonicalize()?;
        let relative = self.relative(&resolved)?;
        read_regular(open_beneath(&self.directory, relative, false)?)
    }

    pub(super) fn edit_target(&self, relative: &Path) -> io::Result<EditTarget> {
        let name = relative
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path must name a file"))?;
        let parent = self
            .canonical
            .join(relative.parent().unwrap_or(Path::new("")));
        let resolved = parent.canonicalize().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("its directory does not exist in /workspace ({error})"),
            )
        })?;
        let directory = open_beneath(&self.directory, self.relative(&resolved)?, true)?;
        Ok(EditTarget {
            directory,
            name: name.to_owned(),
        })
    }

    fn relative<'a>(&self, resolved: &'a Path) -> io::Result<&'a Path> {
        let relative = resolved.strip_prefix(&self.canonical).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "path resolves outside /workspace",
            )
        })?;
        if self.excluded.iter().any(|path| relative.starts_with(path)) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "project path is excluded",
            ));
        }
        Ok(relative)
    }
}

pub(super) struct EditTarget {
    directory: File,
    name: OsString,
}

impl EditTarget {
    pub(super) fn read_existing(&self) -> io::Result<Option<Vec<u8>>> {
        match open_beneath(&self.directory, Path::new(&self.name), false) {
            Ok(file) => read_regular(file).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn replace(&self, bytes: &[u8]) -> io::Result<()> {
        // This Host-side capability path keeps tempfile creation and cleanup
        // attached to the open parent even if its workspace name is replaced.
        let directory_path = PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()));
        let mut temporary = tempfile::Builder::new()
            .prefix(".anchor-edit-")
            .rand_bytes(16)
            .make_in(directory_path, |path| {
                let name = path.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid temporary file name")
                })?;
                openat(
                    &self.directory,
                    name,
                    OFlags::RDWR
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_bits_truncate(0o666),
                )
                .map(File::from)
                .map_err(io::Error::from)
            })?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        let temporary_name = temporary.path().file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid temporary file name")
        })?;
        renameat(&self.directory, temporary_name, &self.directory, &self.name)?;
        Ok(())
    }
}

/// Like Plugin resource reads, never follow symlinks while opening a capability.
fn open_beneath(directory: &File, relative: &Path, is_directory: bool) -> io::Result<File> {
    let components = relative
        .components()
        .filter(|part| *part != Component::CurDir)
        .collect::<Vec<_>>();
    let mut opened = directory.try_clone()?;
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid workspace file path",
            ));
        };
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        if is_directory || index + 1 < components.len() {
            flags |= OFlags::DIRECTORY;
        }
        opened = File::from(openat(&opened, *name, flags, Mode::empty())?);
    }
    Ok(opened)
}

fn read_regular(mut file: File) -> io::Result<Vec<u8>> {
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file in /workspace",
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}
