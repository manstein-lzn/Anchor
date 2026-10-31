use super::*;
use anchor_file_lease::{FileLease, FlockOperation};
use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactAvailability {
    Available,
    Expired,
    Missing,
}

pub(crate) struct ArtifactRead {
    pub(crate) file: fs::File,
    pub(crate) bytes: u64,
    _lease: FileLease,
}

impl ArtifactRead {
    pub(crate) fn into_parts(self) -> (fs::File, FileLease) {
        (self.file, self._lease)
    }
}

#[derive(Debug, Default)]
pub(crate) struct ArtifactGcReport {
    pub(crate) expired: Vec<String>,
    pub(crate) busy: Vec<String>,
    pub(crate) removed_objects: usize,
    pub(crate) freed_bytes: u64,
    pub(crate) remaining_bytes: u64,
}

#[derive(Debug)]
pub(crate) struct ArtifactInventoryEntry {
    pub(crate) commit: CommitRef,
    pub(crate) run_id: String,
    pub(crate) published_at: i64,
    pub(crate) availability: ArtifactAvailability,
}

impl HostArtifacts {
    fn usage_lease(&self, id: &str, operation: FlockOperation) -> Result<FileLease, GraphError> {
        if !valid_fs_id(id) {
            return Err(corrupt("invalid artifact usage identity"));
        }
        let directory = self.root.join(".usage");
        crate::host_fs::directory(&directory).map_err(corrupt)?;
        let path = directory.join(format!("{id}.lock"));
        checked_path(&path)?;
        let file = fs::File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        FileLease::acquire(file, operation).map_err(|error| std::io::Error::from(error).into())
    }

    /// Resolve under the store lease, then pin only actual mounted deliveries.
    /// Long-running execution never blocks unrelated publication.
    pub(crate) fn lease_inputs(&self, commits: &[CommitRef]) -> Result<Vec<FileLease>, GraphError> {
        let _store = self.store_lease(false)?;
        self.visible_inputs(commits)?
            .into_iter()
            .map(|commit| {
                self.require_available(&commit)?;
                self.usage_lease(&commit.id, FlockOperation::LockShared)
            })
            .collect()
    }

    /// Availability is distinct from file integrity: access hashes the relevant
    /// object, while this projection checks metadata and object presence only.
    pub(crate) fn availability(
        &self,
        commit: &CommitRef,
    ) -> Result<ArtifactAvailability, GraphError> {
        let _lease = self.store_lease(false)?;
        self.availability_unlocked(commit)
    }

    fn availability_unlocked(
        &self,
        commit: &CommitRef,
    ) -> Result<ArtifactAvailability, GraphError> {
        if !valid_fs_id(&commit.id) {
            return Err(corrupt("invalid artifact reference"));
        }
        let path = self.root.join(&commit.id);
        if !path.exists() {
            return Ok(ArtifactAvailability::Missing);
        }
        let (_, manifest) = self.read_snapshot(commit)?;
        let expired = path.join("expired.json");
        if expired.exists() {
            require_file(&expired)?;
            return Ok(ArtifactAvailability::Expired);
        }
        for file in manifest.files.values() {
            let object = self.object_path(file)?;
            match fs::symlink_metadata(&object) {
                Ok(metadata) if metadata.is_file() && metadata.len() == file.bytes => {}
                Ok(_) => return Err(corrupt("artifact content object metadata mismatch")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(ArtifactAvailability::Missing);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(ArtifactAvailability::Available)
    }

    pub(super) fn require_available(&self, commit: &CommitRef) -> Result<(), GraphError> {
        match self.availability_unlocked(commit)? {
            ArtifactAvailability::Available => Ok(()),
            ArtifactAvailability::Expired => Err(GraphError::Unsupported(
                "artifact delivery has expired".into(),
            )),
            ArtifactAvailability::Missing => Err(GraphError::Unsupported(
                "artifact delivery bytes are missing".into(),
            )),
        }
    }

    pub(super) fn require_not_expired(&self, commit: &CommitRef) -> Result<(), GraphError> {
        let (path, _) = self.read_snapshot(commit)?;
        anchor_runtime::artifact::store::require_not_expired(&path)
    }

    /// Keep this usage guard for the entire preview/download/stream. The store
    /// lease covers lookup only; collection skips an in-use delivery.
    pub(crate) fn open_file(
        &self,
        commit: &CommitRef,
        relative: &str,
    ) -> Result<ArtifactRead, GraphError> {
        validate_file_relative(relative)?;
        let _store = self.store_lease(false)?;
        self.require_not_expired(commit)?;
        let lease = self.usage_lease(&commit.id, FlockOperation::LockShared)?;
        let (_, manifest) = self.read_snapshot(commit)?;
        let recorded = manifest
            .files
            .get(relative)
            .ok_or_else(|| corrupt("file is not in this delivery"))?;
        let file = fs::File::open(self.verified_object(recorded)?)?;
        Ok(ArtifactRead {
            file,
            bytes: recorded.bytes,
            _lease: lease,
        })
    }

    pub(crate) fn inventory(&self) -> Result<Vec<ArtifactInventoryEntry>, GraphError> {
        let _lease = self.store_lease(false)?;
        let mut inventory = Vec::new();
        for (path, manifest) in self.published_manifests()? {
            let commit = commit_for(&manifest.key);
            inventory.push(ArtifactInventoryEntry {
                published_at: fs::metadata(path.join("manifest.json"))?.mtime(),
                availability: self.availability_unlocked(&commit)?,
                run_id: manifest.key.run_id,
                commit,
            });
        }
        inventory
            .sort_by(|a, b| (a.published_at, &a.commit.id).cmp(&(b.published_at, &b.commit.id)));
        Ok(inventory)
    }

    /// The application supplies retained and execution/recovery references from
    /// Kernel facts. Historical provenance alone does not retain content bytes.
    pub(crate) fn collect(
        &self,
        protected: &BTreeSet<String>,
        expire: &[String],
    ) -> Result<ArtifactGcReport, GraphError> {
        let _lease = self.store_lease(true)?;
        let before = self.storage_bytes_unlocked()?;
        let manifests = self.published_manifests()?;
        let mut report = ArtifactGcReport::default();
        for id in expire {
            if protected.contains(id) {
                continue;
            }
            if !valid_fs_id(id) {
                return Err(corrupt("invalid artifact expiry reference"));
            }
            if let Some((path, _)) = manifests
                .iter()
                .find(|(path, _)| path.file_name().is_some_and(|name| name == id.as_str()))
                && !path.join("expired.json").exists()
            {
                let _usage = match self.usage_lease(id, FlockOperation::NonBlockingLockExclusive) {
                    Ok(lease) => lease,
                    Err(GraphError::Io(error))
                        if error.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        report.busy.push(id.clone());
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                // Tombstone first: an interrupted cleanup cannot resurrect
                // a reference when some shared bytes are still present.
                write_json_atomic(
                    &path.join("expired.json"),
                    &serde_json::json!({"format":1,"artifact":id}),
                )?;
                report.expired.push(id.clone());
            }
        }
        let mut reachable = BTreeSet::new();
        for (path, manifest) in manifests {
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(".view-") && name.ends_with(".tmp") {
                    require_directory(&entry.path())?;
                    fs::remove_dir_all(entry.path())?;
                }
            }
            if path.join("expired.json").exists() {
                let view = path.join("view");
                if view.exists() {
                    require_directory(&view)?;
                    fs::remove_dir_all(view)?;
                }
            } else {
                reachable.extend(manifest.files.values().map(|file| file.sha256.clone()));
            }
        }
        let objects = self.root.join("objects");
        if objects.exists() {
            require_directory(&objects)?;
            for entry in fs::read_dir(&objects)? {
                let entry = entry?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| corrupt("invalid content object name"))?;
                let path = entry.path();
                checked_path(&path)?;
                if name.starts_with('.') && name.ends_with(".tmp") {
                    require_file(&path)?;
                    fs::remove_file(path)?;
                } else if valid_sha256(&name) && !reachable.contains(&name) {
                    require_directory(&path)?;
                    fs::remove_dir_all(path)?;
                    report.removed_objects += 1;
                } else if valid_sha256(&name) {
                    for entry in fs::read_dir(&path)? {
                        let entry = entry?;
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with(".executable-") && name.ends_with(".tmp") {
                            require_file(&entry.path())?;
                            fs::remove_file(entry.path())?;
                        }
                    }
                    fs::File::open(path)?.sync_all()?;
                }
            }
            fs::File::open(objects)?.sync_all()?;
        }
        // Publication uses the same exclusive lease, so only abandoned
        // manifest staging directories can remain at this point.
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".fs2-") && name.ends_with(".tmp") {
                require_directory(&entry.path())?;
                fs::remove_dir_all(entry.path())?;
            }
        }
        let call_inputs = self.root.join("call-inputs");
        if call_inputs.exists() {
            for entry in fs::read_dir(&call_inputs)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(".call-inputs-") && name.ends_with(".tmp") {
                    require_directory(&entry.path())?;
                    fs::remove_dir_all(entry.path())?;
                }
            }
            fs::File::open(call_inputs)?.sync_all()?;
        }
        fs::File::open(&self.root)?.sync_all()?;
        report.remaining_bytes = self.storage_bytes_unlocked()?;
        report.freed_bytes = before.saturating_sub(report.remaining_bytes);
        Ok(report)
    }

    pub(crate) fn storage_bytes(&self) -> Result<u64, GraphError> {
        let _lease = self.store_lease(false)?;
        self.storage_bytes_unlocked()
    }

    fn storage_bytes_unlocked(&self) -> Result<u64, GraphError> {
        fn visit(path: &Path, seen: &mut BTreeSet<(u64, u64)>) -> Result<u64, GraphError> {
            checked_path(path)?;
            let metadata = fs::symlink_metadata(path)?;
            if metadata.is_file() {
                return Ok(if seen.insert((metadata.dev(), metadata.ino())) {
                    metadata.len()
                } else {
                    0
                });
            }
            if !metadata.is_dir() {
                return Err(corrupt("unexpected artifact storage entry"));
            }
            fs::read_dir(path)?.try_fold(0_u64, |bytes, entry| {
                Ok(bytes.saturating_add(visit(&entry?.path(), seen)?))
            })
        }
        visit(&self.root, &mut BTreeSet::new())
    }

    fn published_manifests(&self) -> Result<Vec<(PathBuf, Manifest)>, GraphError> {
        let mut manifests = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_fs_id(&id) {
                continue;
            }
            let path = entry.path();
            require_file(&path.join("manifest.json"))?;
            let manifest: Manifest = serde_json::from_slice(&fs::read(path.join("manifest.json"))?)
                .map_err(|error| corrupt(error.to_string()))?;
            let commit = CommitRef {
                id,
                node_id: manifest.key.node_id.clone(),
                invocation: manifest.key.invocation,
            };
            manifests.push(self.read_snapshot(&commit)?);
        }
        Ok(manifests)
    }
}
