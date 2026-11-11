use crate::{PackageError, ResourceFile, Result, validation};
use rustix::fs::{Dir, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::MetadataExt, fs::PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl From<&Metadata> for Stamp {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

pub(crate) fn absolute(path: &Path) -> Result<PathBuf> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(PackageError::Invalid(
            "parent traversal is forbidden".into(),
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(absolute.components().collect())
}

pub(crate) fn open_path(path: &Path) -> Result<File> {
    let path = absolute(path)?;
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut directory = File::from(rustix::fs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    for (index, component) in components.iter().enumerate() {
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        if index + 1 < components.len() {
            flags |= OFlags::DIRECTORY;
        }
        directory = File::from(
            rustix::fs::openat(&directory, *component, flags, Mode::empty()).map_err(|error| {
                PackageError::Invalid(format!(
                    "cannot open {} without symlinks or special components: {error}",
                    path.display()
                ))
            })?,
        );
    }
    Ok(directory)
}

fn open_child(directory: &File, name: &str) -> Result<File> {
    Ok(File::from(
        rustix::fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| {
            PackageError::Invalid(format!(
                "cannot open resource {name} without symlinks: {error}"
            ))
        })?,
    ))
}

fn names(directory: &File) -> Result<Vec<String>> {
    let mut reader = Dir::read_from(directory)?;
    let mut names = Vec::new();
    while let Some(entry) = reader.read() {
        let entry = entry?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = std::str::from_utf8(bytes)
            .map_err(|_| PackageError::Invalid("non-UTF-8 resource name".into()))?;
        validation::resource_component(name)?;
        names.push(name.to_owned());
    }
    names.sort();
    Ok(names)
}

fn hash_copy(
    source: &mut File,
    destination: &mut dyn Write,
    expected_size: u64,
) -> Result<(String, u64)> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut length = 0;
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if length + count as u64 > expected_size {
            return Err(PackageError::Invalid(
                "input grew during snapshotting".into(),
            ));
        }
        digest.update(&buffer[..count]);
        destination.write_all(&buffer[..count])?;
        length += count as u64;
    }
    Ok((format!("{:x}", digest.finalize()), length))
}

#[derive(Debug)]
pub(crate) struct Snapshot {
    source: PathBuf,
    prefix: String,
    stamps: BTreeMap<String, Stamp>,
    files: BTreeMap<String, ResourceFile>,
    tree: bool,
    /// The mode a single-file snapshot was created with (unused for a tree).
    mode: u32,
    /// A shallow listing of an input directory: children only, for verification.
    shallow: bool,
}

impl Snapshot {
    pub(crate) fn file(source: &Path, staging: &Path, relative: &str) -> Result<Self> {
        Self::single(source, staging, relative, true)
    }

    /// A single input file that is not an executable: the Graph and its manifest.
    pub(crate) fn resource(source: &Path, staging: &Path, relative: &str) -> Result<Self> {
        Self::single(source, staging, relative, false)
    }

    fn single(source: &Path, staging: &Path, relative: &str, executable: bool) -> Result<Self> {
        let source_path = absolute(source)?;
        let mut source = open_path(&source_path)?;
        let metadata = source.metadata()?;
        require_regular(&metadata)?;
        if executable {
            require_executable(&metadata)?;
        }
        let before = Stamp::from(&metadata);
        let destination = staging.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut target = File::create(&destination)?;
        let (sha256, size) = hash_copy(&mut source, &mut target, metadata.len())?;
        if Stamp::from(&source.metadata()?) != before || size != metadata.len() {
            return Err(PackageError::Drift(source_path));
        }
        let mode = if executable { 0o755 } else { 0o644 };
        fs::set_permissions(&destination, fs::Permissions::from_mode(mode))?;
        let resource = ResourceFile {
            path: relative.into(),
            sha256,
            size,
            mode,
        };
        Ok(Self {
            source: source_path,
            prefix: relative.into(),
            stamps: BTreeMap::from([(relative.into(), before)]),
            files: BTreeMap::from([(relative.into(), resource)]),
            tree: false,
            mode,
            shallow: false,
        })
    }

    /// The immediate children of one input directory, for verification only.
    ///
    /// Nothing is staged and no archive entry is contributed: this is how an
    /// addition to the input between preparation and publication is a drift
    /// rather than a silent copy. Contents are covered by the snapshots of what is
    /// actually shipped.
    pub(crate) fn listing(source: &Path, prefix: &str) -> Result<Self> {
        let source = absolute(source)?;
        let metadata = open_path(&source)?.metadata()?;
        if !metadata.is_dir() {
            return Err(PackageError::Invalid(
                "an input directory was expected".into(),
            ));
        }
        let mut snapshot = Self {
            source,
            prefix: prefix.into(),
            stamps: BTreeMap::new(),
            files: BTreeMap::new(),
            tree: false,
            mode: 0o644,
            shallow: true,
        };
        snapshot.record_children()?;
        Ok(snapshot)
    }

    fn record_children(&mut self) -> Result<()> {
        self.stamps.insert(
            self.prefix.clone(),
            Stamp::from(&open_path(&self.source)?.metadata()?),
        );
        for entry in fs::read_dir(&self.source)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            self.stamps.insert(
                format!("{}/{name}", self.prefix),
                Stamp::from(&entry.metadata()?),
            );
        }
        Ok(())
    }

    pub(crate) fn tree(source: &Path, staging: &Path, prefix: &str) -> Result<Self> {
        let mut snapshot = Self {
            source: absolute(source)?,
            prefix: prefix.into(),
            stamps: BTreeMap::new(),
            files: BTreeMap::new(),
            tree: true,
            mode: 0o755,
            shallow: false,
        };
        let root = open_path(&snapshot.source)?;
        snapshot.walk(root, prefix, Some(staging))?;
        Ok(snapshot)
    }

    fn walk(&mut self, mut source: File, relative: &str, staging: Option<&Path>) -> Result<()> {
        validation::archive_path(relative)?;
        let metadata = source.metadata()?;
        let before = Stamp::from(&metadata);
        self.stamps.insert(relative.into(), before.clone());
        if metadata.is_dir() {
            if let Some(staging) = staging {
                fs::create_dir(staging.join(relative))?;
                fs::set_permissions(staging.join(relative), fs::Permissions::from_mode(0o755))?;
            }
            let children = names(&source)?;
            for name in children {
                let child = open_child(&source, &name)?;
                self.walk(child, &format!("{relative}/{name}"), staging)?;
            }
        } else {
            require_regular(&metadata)?;
            validation::resource_extension(relative)?;
            let executable = validation::native_resource(relative);
            if executable {
                require_executable(&metadata)?;
            }
            let mode = if executable { 0o755 } else { 0o644 };
            let (sha256, size) = if let Some(staging) = staging {
                let destination = staging.join(relative);
                let result = hash_copy(
                    &mut source,
                    &mut File::create(&destination)?,
                    metadata.len(),
                )?;
                fs::set_permissions(&destination, fs::Permissions::from_mode(mode))?;
                validation::resource_content(&destination, relative)?;
                if executable {
                    validation::elf(&destination)?;
                }
                result
            } else {
                hash_copy(&mut source, &mut io::sink(), metadata.len())?
            };
            if size != metadata.len() {
                return Err(PackageError::Drift(self.source.clone()));
            }
            self.files.insert(
                relative.into(),
                ResourceFile {
                    path: relative.into(),
                    sha256,
                    size,
                    mode,
                },
            );
        }
        if Stamp::from(&source.metadata()?) != before {
            return Err(PackageError::Drift(self.source.clone()));
        }
        Ok(())
    }

    pub(crate) fn verify(&self) -> Result<()> {
        if self.shallow {
            let mut current = Self {
                source: self.source.clone(),
                prefix: self.prefix.clone(),
                stamps: BTreeMap::new(),
                files: BTreeMap::new(),
                tree: false,
                mode: self.mode,
                shallow: true,
            };
            current.record_children()?;
            if current.stamps != self.stamps {
                return Err(PackageError::Drift(self.source.clone()));
            }
            return Ok(());
        }
        let current = if self.tree {
            let mut current = Self {
                source: self.source.clone(),
                prefix: self.prefix.clone(),
                stamps: BTreeMap::new(),
                files: BTreeMap::new(),
                tree: true,
                mode: self.mode,
                shallow: false,
            };
            current.walk(open_path(&self.source)?, &self.prefix, None)?;
            current
        } else {
            let mut source = open_path(&self.source)?;
            let metadata = source.metadata()?;
            require_regular(&metadata)?;
            let stamp = Stamp::from(&metadata);
            let (sha256, size) = hash_copy(&mut source, &mut io::sink(), metadata.len())?;
            if Stamp::from(&source.metadata()?) != stamp {
                return Err(PackageError::Drift(self.source.clone()));
            }
            Self {
                source: self.source.clone(),
                prefix: self.prefix.clone(),
                tree: false,
                stamps: BTreeMap::from([(self.prefix.clone(), stamp)]),
                files: BTreeMap::from([(
                    self.prefix.clone(),
                    ResourceFile {
                        path: self.prefix.clone(),
                        sha256,
                        size,
                        mode: self.mode,
                    },
                )]),
                mode: self.mode,
                shallow: false,
            }
        };
        if current.stamps != self.stamps || current.files != self.files {
            return Err(PackageError::Drift(self.source.clone()));
        }
        Ok(())
    }

    pub(crate) fn resources(&self) -> impl Iterator<Item = &ResourceFile> {
        self.files.values()
    }

    pub(crate) fn directories(&self) -> impl Iterator<Item = &String> {
        self.stamps
            .keys()
            .filter(|path| !self.files.contains_key(*path))
    }
}

fn require_regular(metadata: &Metadata) -> Result<()> {
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(PackageError::Invalid(
            "only regular, non-hardlinked files are supported".into(),
        ));
    }
    if metadata.len() > 1024 * 1024 * 1024 {
        return Err(PackageError::Invalid("input file exceeds 1 GiB".into()));
    }
    Ok(())
}

fn require_executable(metadata: &Metadata) -> Result<()> {
    if metadata.mode() & 0o111 == 0 {
        return Err(PackageError::Invalid(
            "binary input must be executable".into(),
        ));
    }
    Ok(())
}

pub(crate) struct Output {
    directory: File,
    name: String,
    pub(crate) path: PathBuf,
}

impl Output {
    pub(crate) fn new(path: &Path) -> Result<Self> {
        Self::with_extension(path, ".tar.gz")
    }

    pub(crate) fn with_extension(path: &Path, extension: &str) -> Result<Self> {
        let path = absolute(path)?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| PackageError::Invalid("output must name a new file".into()))?;
        validation::safe_component(name)?;
        if !name.ends_with(extension) {
            return Err(PackageError::Invalid(format!(
                "output must end in {extension}"
            )));
        }
        let directory = open_path(
            path.parent()
                .ok_or_else(|| PackageError::Invalid("output has no parent".into()))?,
        )?;
        if !directory.metadata()?.is_dir() {
            return Err(PackageError::Invalid(
                "output parent must be a directory".into(),
            ));
        }
        match rustix::fs::statat(&directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => return Err(PackageError::AlreadyExists),
            Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Self {
            directory,
            name: name.into(),
            path,
        })
    }

    fn verify_directory(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| PackageError::Invalid("output has no parent".into()))?;
        let current = open_path(parent)?.metadata()?;
        let pinned = self.directory.metadata()?;
        if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
            return Err(PackageError::Drift(parent.into()));
        }
        Ok(())
    }

    pub(crate) fn publish(&self, archive: &mut impl Read) -> Result<()> {
        self.verify_directory()?;
        let pinned = PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()));
        let mut candidate = tempfile::NamedTempFile::new_in(pinned)?;
        io::copy(archive, &mut candidate)?;
        candidate
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))?;
        candidate.as_file().sync_all()?;
        self.verify_directory()?;
        let candidate_name = candidate
            .path()
            .file_name()
            .ok_or_else(|| PackageError::Invalid("temporary archive has no name".into()))?;
        rustix::fs::renameat_with(
            &self.directory,
            candidate_name,
            &self.directory,
            self.name.as_str(),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                PackageError::AlreadyExists
            } else {
                error.into()
            }
        })?;
        if let Err(error) = self.verify_directory() {
            let published = rustix::fs::statat(
                &self.directory,
                self.name.as_str(),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )?;
            let expected = candidate.as_file().metadata()?;
            if published.st_ino as u64 != expected.ino()
                || published.st_dev as u64 != expected.dev()
            {
                return Err(PackageError::PublicationUncertain(self.path.clone()));
            }
            rustix::fs::unlinkat(
                &self.directory,
                self.name.as_str(),
                rustix::fs::AtFlags::empty(),
            )
            .map_err(|_| PackageError::PublicationUncertain(self.path.clone()))?;
            self.directory
                .sync_all()
                .map_err(|_| PackageError::PublicationUncertain(self.path.clone()))?;
            return Err(error);
        }
        self.directory
            .sync_all()
            .map_err(|_| PackageError::PublicationUncertain(self.path.clone()))?;
        Ok(())
    }
}

pub(crate) fn make_directories_writable(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(path)? {
            make_directories_writable(&entry?.path())?;
        }
    }
    Ok(())
}
