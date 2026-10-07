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
}

impl Snapshot {
    pub(crate) fn file(source: &Path, staging: &Path, relative: &str) -> Result<Self> {
        let source_path = absolute(source)?;
        let mut source = open_path(&source_path)?;
        let metadata = source.metadata()?;
        require_regular(&metadata)?;
        require_executable(&metadata)?;
        let before = Stamp::from(&metadata);
        let destination = staging.join(relative);
        let mut target = File::create(&destination)?;
        let (sha256, size) = hash_copy(&mut source, &mut target, metadata.len())?;
        if Stamp::from(&source.metadata()?) != before || size != metadata.len() {
            return Err(PackageError::Drift(source_path));
        }
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))?;
        let resource = ResourceFile {
            path: relative.into(),
            sha256,
            size,
            mode: 0o755,
        };
        Ok(Self {
            source: source_path,
            prefix: relative.into(),
            stamps: BTreeMap::from([(relative.into(), before)]),
            files: BTreeMap::from([(relative.into(), resource)]),
            tree: false,
        })
    }

    pub(crate) fn tree(source: &Path, staging: &Path, prefix: &str) -> Result<Self> {
        let mut snapshot = Self {
            source: absolute(source)?,
            prefix: prefix.into(),
            stamps: BTreeMap::new(),
            files: BTreeMap::new(),
            tree: true,
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
        let current = if self.tree {
            let mut current = Self {
                source: self.source.clone(),
                prefix: self.prefix.clone(),
                stamps: BTreeMap::new(),
                files: BTreeMap::new(),
                tree: true,
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
                        mode: 0o755,
                    },
                )]),
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
        let path = absolute(path)?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| PackageError::Invalid("output must name a new tar.gz file".into()))?;
        validation::safe_component(name)?;
        if !name.ends_with(".tar.gz") {
            return Err(PackageError::Invalid("output must end in .tar.gz".into()));
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
