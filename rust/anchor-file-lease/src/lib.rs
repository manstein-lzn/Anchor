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
    path::{Component, Path},
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

#[cfg(test)]
mod tests {
    use super::*;
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
