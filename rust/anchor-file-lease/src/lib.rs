#![forbid(unsafe_code)]
//! Release an advisory lock before closing its descriptor. A concurrent fork
//! can retain the same open file description until exec; close alone would let
//! that inherited descriptor prolong the parent's lock.
pub use rustix::fs::FlockOperation;
use std::{
    fs::File,
    io::{self, Read},
    ops::Deref,
    os::fd::{AsFd, BorrowedFd},
    path::{Component, Path, PathBuf},
};

/// Walk an absolute registered path without following a link in any component.
/// Opening nonblocking prevents a FIFO supplied in place of a file from hanging.
pub fn open_regular(path: &Path) -> io::Result<File> {
    let file = open_path(path, false)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("regular file required"));
    }
    Ok(file)
}

pub fn open_directory(path: &Path) -> io::Result<File> {
    open_path(path, true)
}

fn open_path(path: &Path, directory_only: bool) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    if !path.is_absolute() {
        return Err(io::Error::other("registered file path must be absolute"));
    }
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let mut directory = open("/", flags | OFlags::DIRECTORY, Mode::empty())?;
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            if component == Component::RootDir {
                continue;
            }
            return Err(io::Error::other("registered file path is not canonical"));
        };
        let last = components.peek().is_none();
        let descriptor = openat(
            &directory,
            Path::new(component),
            if last && !directory_only {
                flags
            } else {
                flags | OFlags::DIRECTORY
            },
            Mode::empty(),
        )?;
        if last {
            return Ok(File::from(descriptor));
        }
        directory = descriptor;
    }
    if directory_only {
        Ok(directory.into())
    } else {
        Err(io::Error::other("registered path must name a file"))
    }
}

pub fn read_regular(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let file = open_regular(path)?;
    let mut bytes = Vec::new();
    file.take((maximum as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(io::Error::other("registered file exceeds its size limit"));
    }
    Ok(bytes)
}

#[derive(Debug)]
pub struct FileLease(File);

impl FileLease {
    /// Own the descriptor before a cancellable acquisition loop. Drop also
    /// covers errors and unwinding between acquiring the lock and returning.
    pub fn new(file: File) -> Self {
        Self(file)
    }

    pub fn acquire(file: File, operation: FlockOperation) -> rustix::io::Result<Self> {
        let lease = Self::new(file);
        lease.lock(operation)?;
        Ok(lease)
    }

    pub fn release(self) -> rustix::io::Result<()> {
        self.lock(FlockOperation::Unlock)
    }

    /// Preserve the caller's shared/exclusive and blocking/nonblocking policy.
    pub fn lock(&self, operation: FlockOperation) -> rustix::io::Result<()> {
        rustix::fs::flock(&self.0, operation)
    }
}
impl Deref for FileLease {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}
impl AsFd for FileLease {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Drop for FileLease {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.0, FlockOperation::Unlock);
    }
}

/// Cooperative, nonblocking single-writer ownership of an existing directory.
/// Shared ancestor locks conflict with an exclusive lease on any enclosing
/// directory, while sibling directories (including worktrees) remain independent.
/// This is not a filesystem permission boundary and does not fence other tools.
#[derive(Debug)]
pub struct ProjectLease {
    path: PathBuf,
    _leases: Vec<FileLease>,
}

impl ProjectLease {
    /// Resolve aliases once, then lock directory inodes without creating files.
    /// The caller must authorize the path and retain this guard until every
    /// process capable of writing through the operation has stopped. Contention
    /// returns `io::ErrorKind::WouldBlock`; partially acquired locks are released.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory lease requires an absolute path without `..`",
            ));
        }
        let path = path.canonicalize()?;
        let mut leases = Vec::new();
        for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let operation = if ancestor == path {
                FlockOperation::NonBlockingLockExclusive
            } else {
                FlockOperation::NonBlockingLockShared
            };
            leases.push(FileLease::acquire(open_directory(ancestor)?, operation)?);
        }
        Ok(Self {
            path,
            _leases: leases,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_lease_coordinates_independent_processes() {
        if let Some(path) = std::env::var_os("ANCHOR_LEASE_TEST_PATH") {
            let result = ProjectLease::acquire(Path::new(&path));
            if std::env::var_os("ANCHOR_LEASE_TEST_BUSY").is_some() {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
            } else {
                result.unwrap();
            }
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let lease = ProjectLease::acquire(root.path()).unwrap();
        let child = |busy: bool| {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "tests::project_lease_coordinates_independent_processes",
                ])
                .env("ANCHOR_LEASE_TEST_PATH", root.path());
            if busy {
                command.env("ANCHOR_LEASE_TEST_BUSY", "1");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        child(true);
        drop(lease);
        child(false);
    }
    #[test]
    fn directory_leases_fence_aliases_and_ancestors_but_not_siblings() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let child = project.join("src");
        let sibling = root.path().join("worktree");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        let guard = ProjectLease::acquire(&alias).unwrap();
        assert_eq!(guard.path(), project.canonicalize().unwrap());
        for path in [&project, &alias, &child, root.path()] {
            assert_eq!(
                ProjectLease::acquire(path).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
        let sibling_guard = ProjectLease::acquire(&sibling).unwrap();
        drop(guard);
        let child_guard = ProjectLease::acquire(&child).unwrap();
        assert_eq!(
            ProjectLease::acquire(&project).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(child_guard);
        drop(sibling_guard);
        ProjectLease::acquire(root.path()).unwrap();
    }

    #[test]
    fn directory_lease_rejects_invalid_paths_and_releases_on_unwind() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file");
        std::fs::write(&file, b"fixture").unwrap();
        for path in [Path::new("."), &root.path().join("missing"), &file] {
            assert!(ProjectLease::acquire(path).is_err());
        }
        assert!(
            std::panic::catch_unwind(|| {
                let _guard = ProjectLease::acquire(root.path()).unwrap();
                panic!("fixture");
            })
            .is_err()
        );
        ProjectLease::acquire(root.path()).unwrap();
    }
    fn open(path: &std::path::Path) -> File {
        File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap()
    }
    #[test]
    fn bounded_registered_reads_reject_links_directories_fifos_and_oversize() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("input");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(read_regular(&path, 3).unwrap(), b"abc");
        assert_eq!(read_regular(&path, usize::MAX).unwrap(), b"abc");
        assert!(read_regular(&path, 2).is_err());
        assert!(read_regular(root.path(), 3).is_err());
        let link = root.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read_regular(&link, 3).is_err());
        let parent = root.path().join("parent");
        symlink(root.path(), &parent).unwrap();
        assert!(read_regular(&parent.join("input"), 3).is_err());
        let fifo = root.path().join("fifo");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RWXU).unwrap();
        assert!(read_regular(&fifo, 3).is_err());
        assert!(read_regular(Path::new("relative"), 3).is_err());
    }
    #[test]
    fn early_return_releases_even_with_an_inherited_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lease");
        let inherited = (|| -> Result<(), File> {
            let lease =
                FileLease::acquire(open(&path), FlockOperation::NonBlockingLockExclusive).unwrap();
            let inherited = lease.try_clone().unwrap();
            Err(inherited)?;
            Ok(())
        })()
        .unwrap_err();
        let next =
            FileLease::acquire(open(&path), FlockOperation::NonBlockingLockExclusive).unwrap();
        drop(inherited);
        drop(next);
    }
    #[test]
    fn unwinding_releases_a_lease_without_waiting_for_its_duplicate() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lease");
        let lease = FileLease::acquire(open(&path), FlockOperation::LockExclusive).unwrap();
        let inherited = lease.try_clone().unwrap();
        assert!(
            std::panic::catch_unwind(move || {
                let _lease = lease;
                panic!("fixture");
            })
            .is_err()
        );
        let _next =
            FileLease::acquire(open(&path), FlockOperation::NonBlockingLockExclusive).unwrap();
        drop(inherited);
    }
    #[test]
    fn shared_and_exclusive_policies_and_failed_acquisition_preserve_ownership() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lease");
        let first = FileLease::acquire(open(&path), FlockOperation::NonBlockingLockShared).unwrap();
        let second =
            FileLease::acquire(open(&path), FlockOperation::NonBlockingLockShared).unwrap();
        for _ in 0..2 {
            assert_eq!(
                FileLease::acquire(open(&path), FlockOperation::NonBlockingLockExclusive)
                    .unwrap_err(),
                rustix::io::Errno::WOULDBLOCK
            );
        }
        drop(first);
        drop(second);
        let _exclusive = FileLease::acquire(open(&path), FlockOperation::LockExclusive).unwrap();
        assert!(FileLease::acquire(open(&path), FlockOperation::NonBlockingLockShared).is_err());
    }
}
