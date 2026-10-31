use super::*;
use anchor_file_lease::{FileLease, FlockOperation};
use std::collections::BTreeSet;

pub(super) struct SelectedPaths {
    pub files: BTreeMap<String, PathBuf>,
    pub directories: Vec<String>,
}

/// Completion validation and publication share exactly this selection walk.
/// Unselected workspace entries are never enumerated or opened.
pub(crate) fn validate_selection(
    workspace: &Path,
    selection: &ArtifactSelection,
    writes: &[String],
) -> Result<(), String> {
    selection.covers(writes)?;
    selected_paths(workspace, selection)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(super) fn selected_paths(
    workspace: &Path,
    selection: &ArtifactSelection,
) -> Result<SelectedPaths, GraphError> {
    let selection = ArtifactSelection::new(selection.paths.clone()).map_err(corrupt)?;
    let mut files = BTreeMap::new();
    let mut directories = BTreeSet::new();
    for path in &selection.paths {
        select_path(workspace, path, &mut files, &mut directories)?;
    }
    Ok(SelectedPaths {
        files,
        directories: directories.into_iter().collect(),
    })
}

fn select_path(
    workspace: &Path,
    name: &str,
    files: &mut BTreeMap<String, PathBuf>,
    directories: &mut BTreeSet<String>,
) -> Result<(), GraphError> {
    validate_file_relative(name)?;
    if files.contains_key(name) || directories.contains(name) {
        return Ok(());
    }
    let path = workspace.join(name);
    checked_path(&path)?;
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            corrupt(format!("selected delivery path does not exist: {name}"))
        } else {
            error.into()
        }
    })?;
    if metadata.is_file() {
        files.insert(name.to_owned(), path);
    } else if metadata.is_dir() {
        directories.insert(name.to_owned());
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let child = entry
                .file_name()
                .into_string()
                .map_err(|_| corrupt("delivery paths must be UTF-8"))?;
            select_path(workspace, &format!("{name}/{child}"), files, directories)?;
        }
    } else {
        return Err(corrupt(
            "selected delivery paths must be regular files or directories",
        ));
    }
    // Include the directory structure of individually selected nested files.
    for parent in Path::new(name)
        .ancestors()
        .skip(1)
        .filter(|path| !path.as_os_str().is_empty())
    {
        directories.insert(parent.to_str().unwrap().to_owned());
    }
    Ok(())
}

impl HostArtifacts {
    pub(super) fn store_lease(&self, exclusive: bool) -> Result<FileLease, GraphError> {
        crate::host_fs::directory(&self.root).map_err(corrupt)?;
        let path = self.root.join(".store.lock");
        checked_path(&path)?;
        let file = fs::File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        FileLease::acquire(
            file,
            if exclusive {
                FlockOperation::LockExclusive
            } else {
                FlockOperation::LockShared
            },
        )
        .map_err(|error| std::io::Error::from(error).into())
    }

    fn object_directory(&self, sha256: &str) -> Result<PathBuf, GraphError> {
        if !valid_sha256(sha256) {
            return Err(corrupt("invalid content object hash"));
        }
        let path = self.root.join("objects").join(sha256);
        checked_path(&path)?;
        Ok(path)
    }

    pub(super) fn object_path(&self, file: &FileHash) -> Result<PathBuf, GraphError> {
        Ok(self
            .object_directory(&file.sha256)?
            .join(if file.executable {
                "executable"
            } else {
                "data"
            }))
    }

    pub(super) fn store_object(&self, source: &Path) -> Result<FileHash, GraphError> {
        require_file(source)?;
        let executable = is_executable(&fs::metadata(source)?);
        let objects = self.root.join("objects");
        fs::create_dir_all(&objects)?;
        require_directory(&objects)?;
        let temporary = objects.join(format!(
            ".{}-{}.tmp",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let (sha256, bytes) = copy_regular_file(source, &temporary)?;
            self.publish_object(
                &temporary,
                FileHash {
                    sha256,
                    bytes,
                    executable,
                },
            )
        })();
        if temporary.exists() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    pub(super) fn store_bytes(&self, bytes: &[u8]) -> Result<FileHash, GraphError> {
        let file = FileHash {
            sha256: format!("{:x}", Sha256::digest(bytes)),
            bytes: bytes.len() as u64,
            executable: false,
        };
        let objects = self.root.join("objects");
        fs::create_dir_all(&objects)?;
        let temporary = objects.join(format!(
            ".{}-{}.tmp",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut output = fs::File::options()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        output.write_all(bytes)?;
        output.sync_all()?;
        let result = self.publish_object(&temporary, file);
        if temporary.exists() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn publish_object(&self, temporary: &Path, file: FileHash) -> Result<FileHash, GraphError> {
        let directory = self.object_directory(&file.sha256)?;
        fs::create_dir_all(&directory)?;
        let data = directory.join("data");
        if data.exists() {
            verify_file(&data, &file)?;
        } else {
            set_executable(temporary, false, true)?;
            fs::rename(temporary, &data)?;
        }
        if file.executable {
            // Mode belongs to the manifest. A separate derived inode lets two
            // views expose the same bytes with different executable flags.
            let executable = directory.join("executable");
            if !executable.exists() {
                let temporary = directory.join(format!(
                    ".executable-{}-{}.tmp",
                    std::process::id(),
                    TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                ));
                copy_regular_file(&data, &temporary)?;
                set_executable(&temporary, true, true)?;
                fs::rename(temporary, &executable)?;
            } else {
                verify_file(&executable, &file)?;
            }
        }
        fs::File::open(&directory)?.sync_all()?;
        fs::File::open(directory.parent().unwrap())?.sync_all()?;
        fs::File::open(&self.root)?.sync_all()?;
        Ok(file)
    }

    pub(super) fn verified_object(&self, file: &FileHash) -> Result<PathBuf, GraphError> {
        let path = self.object_path(file)?;
        verify_file(&path, file)?;
        if is_executable(&fs::metadata(&path)?) != file.executable {
            return Err(corrupt("content object executable metadata changed"));
        }
        Ok(path)
    }

    pub(super) fn artifact_view(
        &self,
        snapshot: &Path,
        manifest: &Manifest,
    ) -> Result<PathBuf, GraphError> {
        let commit = commit_for(&manifest.key);
        self.require_available(&commit)?;
        let lock = snapshot.join(".view.lock");
        checked_path(&lock)?;
        let _view_lease = FileLease::acquire(
            fs::File::options()
                .write(true)
                .create(true)
                .truncate(false)
                .open(lock)?,
            FlockOperation::LockExclusive,
        )
        .map_err(std::io::Error::from)?;
        let view = snapshot.join("view");
        let descriptor = manifest.descriptor(snapshot)?;
        if view.exists() {
            require_directory(&view)?;
            let recorded: ArtifactDescriptor =
                serde_json::from_slice(&fs::read(view.join(ARTIFACT_DESCRIPTOR_FILE))?)
                    .map_err(|error| corrupt(error.to_string()))?;
            if recorded != descriptor {
                return Err(corrupt("artifact view descriptor changed"));
            }
            for (name, file) in &manifest.files {
                let source = self.verified_object(file)?;
                let path = view.join(name);
                require_file(&path)?;
                use std::os::unix::fs::MetadataExt;
                let actual = fs::metadata(path)?;
                let expected = fs::metadata(source)?;
                if (actual.dev(), actual.ino()) != (expected.dev(), expected.ino()) {
                    return Err(corrupt("artifact view file changed"));
                }
            }
            for directory in &manifest.directories {
                require_directory(&view.join(directory))?;
            }
            return Ok(view);
        }
        let temporary = snapshot.join(format!(
            ".view-{}-{}.tmp",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&temporary)?;
        let result = (|| {
            for directory in &manifest.directories {
                fs::create_dir_all(temporary.join(directory))?;
            }
            for (name, file) in &manifest.files {
                let source = self.verified_object(file)?;
                if let Some(parent) = temporary.join(name).parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::hard_link(source, temporary.join(name))?;
            }
            write_json_atomic(&temporary.join(ARTIFACT_DESCRIPTOR_FILE), &descriptor)?;
            set_executable(&temporary.join(ARTIFACT_DESCRIPTOR_FILE), false, true)?;
            sync_tree_dirs(&temporary)?;
            fs::rename(&temporary, &view)?;
            fs::File::open(snapshot)?.sync_all()?;
            Ok(view)
        })();
        if temporary.exists() {
            let _ = fs::remove_dir_all(temporary);
        }
        result
    }
}

pub(super) fn verify_file(path: &Path, expected: &FileHash) -> Result<(), GraphError> {
    require_file(path)?;
    let (sha256, bytes) = hash_copy(fs::File::open(path)?, std::io::sink())?;
    if sha256 != expected.sha256 || bytes != expected.bytes {
        return Err(corrupt("artifact content digest mismatch"));
    }
    Ok(())
}

pub(super) fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

pub(super) fn set_executable(
    path: &Path,
    executable: bool,
    readonly: bool,
) -> Result<(), GraphError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match (executable, readonly) {
        (false, false) => 0o644,
        (true, false) => 0o755,
        (false, true) => 0o444,
        (true, true) => 0o555,
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    fs::File::open(path)?.sync_all()?;
    Ok(())
}
