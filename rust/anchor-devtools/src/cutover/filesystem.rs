use std::{
    collections::BTreeSet,
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use rustix::fs::{
    AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, Stat, flock, fstat, mkdirat, openat,
    statat,
};
use serde::Serialize;
use serde_json::Value;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

pub(super) fn absolute(value: &str) -> Result<PathBuf, String> {
    let path = if value == "~" || value.starts_with("~/") {
        let home =
            std::env::var_os("HOME").ok_or("HOME is missing; supply an absolute legacy root")?;
        PathBuf::from(home).join(value.strip_prefix("~/").unwrap_or(""))
    } else {
        PathBuf::from(value)
    };
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err("cutover paths must not contain NUL".into());
    }
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let mut normalized = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            Component::Normal(name) => normalized.push(name),
            Component::ParentDir => {
                directory(&normalized, false)
                    .map_err(|_| "parent traversal must not cross an unsafe directory")?
                    .ok_or("parent traversal must not cross a missing directory")?;
                normalized.pop();
            }
            Component::RootDir | Component::CurDir => {}
            _ => return Err("unsupported cutover path".into()),
        }
    }
    Ok(normalized)
}

pub(super) fn overlap(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

fn unsafe_entry(message: &str) -> io::Error {
    io::Error::other(message)
}

pub(super) fn directory(path: &Path, create: bool) -> io::Result<Option<File>> {
    let root = File::from(openat(
        rustix::fs::CWD,
        "/",
        DIRECTORY_FLAGS,
        Mode::empty(),
    )?);
    descend(root, path, create)
}

fn descend(mut parent: File, path: &Path, create: bool) -> io::Result<Option<File>> {
    for component in path.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            _ => return Err(unsafe_entry("unsafe directory component")),
        };
        let metadata = match statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(metadata) => metadata,
            Err(rustix::io::Errno::NOENT) if create => {
                match mkdirat(&parent, name, Mode::RWXU) {
                    Ok(()) => parent.sync_all()?,
                    Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
                statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)?
            }
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory => {}
            FileType::Symlink => return Err(unsafe_entry("directory traverses a symlink")),
            _ => return Err(unsafe_entry("root or ancestor is not a directory")),
        }
        let child = File::from(openat(&parent, name, DIRECTORY_FLAGS, Mode::empty())?);
        // Only a *replaced* component is a swap race. Comparing whole stamps also
        // treats a component that merely changed as replaced, which makes every
        // walk through a shared ancestor such as `/tmp` fail whenever a sibling
        // process or test thread creates or removes an unrelated entry.
        if !same_object(&metadata, &fstat(&child)?) {
            return Err(unsafe_entry("directory was replaced during inspection"));
        }
        parent = child;
    }
    Ok(Some(parent))
}

pub(super) fn names(directory: &File) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in Dir::read_from(directory)? {
        let entry = entry?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let name = std::str::from_utf8(bytes)
            .map_err(|_| unsafe_entry("filesystem entry is not UTF-8; inventory is incomplete"))?;
        names.push(name.to_owned());
    }
    names.sort();
    Ok(names)
}

pub(super) fn regular(directory: &File, name: &str) -> io::Result<File> {
    let metadata = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return Err(unsafe_entry("refusing symlink or non-regular file"));
    }
    let file = File::from(openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    // This file's bytes are read or hashed right after opening, so a file that
    // was replaced *or modified* between the two calls must fail closed here.
    // That is why this check keeps the whole stamp while the traversal check in
    // `descend` only compares identity.
    if Stamp::of(&metadata) != Stamp::of(&fstat(&file)?) {
        return Err(unsafe_entry("file changed during inspection"));
    }
    Ok(file)
}

pub(super) fn relative_file(root: &File, path: &str) -> io::Result<File> {
    let path = Path::new(path);
    let parent = descend(
        root.try_clone()?,
        path.parent().unwrap_or(Path::new("")),
        false,
    )?
    .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| unsafe_entry("invalid file name"))?;
    regular(&parent, name)
}

pub(super) fn json_file(file: File) -> io::Result<Value> {
    let before = Stamp::of(&fstat(&file)?);
    let mut content = Vec::new();
    (&file)
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut content)?;
    if content.len() > 16 * 1024 * 1024 || before != Stamp::of(&fstat(&file)?) {
        return Err(unsafe_entry(
            "JSON record is too large or changed during inspection",
        ));
    }
    let value: Value =
        serde_json::from_slice(&content).map_err(|_| unsafe_entry("invalid JSON record"))?;
    if !value.is_object() {
        return Err(unsafe_entry("expected JSON object"));
    }
    Ok(value)
}

pub(super) fn lock_status(path: &Path) -> &'static str {
    let result = (|| -> io::Result<Option<File>> {
        let Some(parent) = directory(path.parent().unwrap(), false)? else {
            return Ok(None);
        };
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| unsafe_entry("invalid lock name"))?;
        match statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(error) => Err(error.into()),
            Ok(_) => regular(&parent, name).map(Some),
        }
    })();
    match result {
        Ok(None) => "absent",
        Ok(Some(file)) => match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => "available",
            Err(rustix::io::Errno::WOULDBLOCK) => "held",
            Err(_) => "unknown",
        },
        Err(_) => "unknown",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    device: u64,
    inode: u64,
    size: i128,
    mode: u32,
    links: u64,
    modified: i128,
    changed: i128,
}

impl Stamp {
    pub(super) fn of(metadata: &Stat) -> Self {
        Self {
            device: metadata.st_dev,
            inode: metadata.st_ino,
            size: metadata.st_size as i128,
            mode: metadata.st_mode,
            links: metadata.st_nlink,
            modified: metadata.st_mtime as i128 * 1_000_000_000 + metadata.st_mtime_nsec as i128,
            changed: metadata.st_ctime as i128 * 1_000_000_000 + metadata.st_ctime_nsec as i128,
        }
    }
}

/// True while both stats still describe the same filesystem object.
///
/// Walking a path and opening each component must not be a swap race, and that
/// is all a traversal check has to prove: an ancestor that merely changed — a
/// sibling entry created or removed in it — is not a replaced ancestor.
pub(super) fn same_object(first: &Stat, second: &Stat) -> bool {
    first.st_dev == second.st_dev && first.st_ino == second.st_ino
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct Entry {
    root: String,
    pub(super) path: String,
    pub(super) kind: &'static str,
    pub(super) bytes: u64,
    pub(super) mode: u32,
    mtime_ns: i128,
    #[serde(skip)]
    stamp: Stamp,
}

pub(super) struct Snapshot {
    pub(super) path: PathBuf,
    label: String,
    pub(super) directory: Option<File>,
    pub(super) entries: Vec<Entry>,
    stamp: Option<Stamp>,
}

impl Snapshot {
    pub(super) fn inspect(path: &Path, label: &str, issues: &mut BTreeSet<String>) -> Self {
        let directory = match directory(path, false) {
            Ok(directory) => directory,
            Err(error) => {
                issues.insert(format!("unsafe {label} root {}: {error}", path.display()));
                None
            }
        };
        let mut snapshot = Self {
            path: path.to_owned(),
            label: label.to_owned(),
            directory,
            entries: Vec::new(),
            stamp: None,
        };
        if let Some(directory) = &snapshot.directory {
            match fstat(directory) {
                Ok(metadata) => snapshot.stamp = Some(Stamp::of(&metadata)),
                Err(error) => {
                    issues.insert(format!("cannot stat {label} root: {error}"));
                }
            }
            snapshot.entries = tree(directory, path, label, issues);
        }
        snapshot
    }

    pub(super) fn readonly(&self) -> bool {
        self.stamp
            .as_ref()
            .is_some_and(|stamp| stamp.mode & 0o222 == 0)
            && self.entries.iter().all(|entry| entry.mode & 0o222 == 0)
    }

    pub(super) fn verify(&self) -> Result<(), String> {
        let mut issues = BTreeSet::new();
        let current = Self::inspect(&self.path, &self.label, &mut issues);
        if !issues.is_empty() || self.stamp != current.stamp || self.entries != current.entries {
            return Err(format!(
                "{} root changed or became unsafe during inventory: {}",
                self.label,
                self.path.display()
            ));
        }
        Ok(())
    }

    pub(super) fn contains(&self, path: &str) -> bool {
        self.entries.iter().any(|entry| entry.path == path)
    }

    pub(super) fn require_directory(&self, path: &str, issues: &mut BTreeSet<String>) {
        if self
            .entries
            .iter()
            .any(|entry| entry.path == path && entry.kind != "directory")
        {
            issues.insert(format!(
                "state path is not a directory: {}",
                self.path.join(path).display()
            ));
        }
    }
}

fn tree(root: &File, root_path: &Path, label: &str, issues: &mut BTreeSet<String>) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut pending = vec![(PathBuf::new(), root.try_clone())];
    while let Some((relative, directory)) = pending.pop() {
        let result = (|| -> io::Result<()> {
            let directory = directory?;
            for name in names(&directory)? {
                let path = relative.join(&name);
                let full = root_path.join(&path);
                let metadata = match statat(&directory, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        issues.insert(format!("cannot stat {}: {error}", full.display()));
                        continue;
                    }
                };
                let kind = match FileType::from_raw_mode(metadata.st_mode) {
                    FileType::Directory => {
                        let child =
                            openat(&directory, name.as_str(), DIRECTORY_FLAGS, Mode::empty())
                                .map(File::from)
                                .map_err(io::Error::from);
                        pending.push((path.clone(), child));
                        "directory"
                    }
                    FileType::RegularFile => "file",
                    FileType::Symlink => {
                        issues.insert(format!("symlink entry: {}", full.display()));
                        "symlink"
                    }
                    _ => {
                        issues.insert(format!("special file: {}", full.display()));
                        "special"
                    }
                };
                if let Some(suffix) = ["-wal", "-shm", "-journal"]
                    .into_iter()
                    .find(|suffix| name.ends_with(suffix))
                {
                    issues.insert(format!(
                        "SQLite {} sidecar present; refusing an incomplete immutable snapshot: {}",
                        suffix.to_uppercase(),
                        full.display()
                    ));
                }
                entries.push(Entry {
                    root: label.to_owned(),
                    path: path.to_string_lossy().into_owned(),
                    kind,
                    bytes: if kind == "file" {
                        metadata.st_size.try_into().unwrap_or(0)
                    } else {
                        0
                    },
                    mode: metadata.st_mode & 0o7777,
                    mtime_ns: Stamp::of(&metadata).modified,
                    stamp: Stamp::of(&metadata),
                });
            }
            Ok(())
        })();
        if let Err(error) = result {
            issues.insert(format!(
                "cannot inventory {}: {error}",
                root_path.join(relative).display()
            ));
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries
}
