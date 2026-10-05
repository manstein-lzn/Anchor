use super::*;

pub(super) struct PreparationLock(fs::File);

impl PreparationLock {
    pub(super) fn acquire(path: &Path) -> Result<Self, GraphError> {
        checked_path(path)?;
        if fs::symlink_metadata(path).is_ok() {
            require_file(path)?;
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;
        Ok(Self(file))
    }
}

impl Drop for PreparationLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl HostArtifacts {
    /// An invocation owns its writable tree. A feedback revisit starts from
    /// its nearest committed version, while a restart preserves pending work.
    pub(crate) fn prepare_workspace(
        &self,
        key: &InvocationKey,
        inputs: &[CommitRef],
    ) -> Result<PathBuf, GraphError> {
        let workspace = self.workspace_path(key)?;
        let snapshots = self.expanded_snapshots(inputs, Some((&key.run_id, &key.graph_digest)))?;
        let previous = snapshots
            .iter()
            .find(|(_, manifest)| manifest.key.node_id == key.node_id);
        if previous.is_some_and(|(_, manifest)| manifest.key.invocation >= key.invocation) {
            return Err(corrupt("workspace seed must be an earlier node invocation"));
        }
        let parent = workspace
            .parent()
            .ok_or_else(|| corrupt("workspace has no parent"))?;
        fs::create_dir_all(parent)?;
        require_directory(parent)?;
        let lock_path = parent.join(format!(".{}.prepare.lock", key_hash(key)));
        let _lock = PreparationLock::acquire(&lock_path)?;
        match fs::symlink_metadata(&workspace) {
            Ok(_) => {
                require_directory(&workspace)?;
                return Ok(workspace);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let temporary = parent.join(format!(
            ".{}.{}.{}.prepare.tmp",
            key_hash(key),
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            if let Some((source, manifest)) = previous {
                let (files, directories) = scan_tree(&source.join("files"), Some(&temporary))?;
                if files != manifest.files || directories != manifest.directories {
                    return Err(corrupt("workspace seed changed while being copied"));
                }
            } else {
                fs::create_dir(&temporary)?;
                fs::File::open(&temporary)?.sync_all()?;
            }
            fs::rename(&temporary, &workspace)?;
            for ancestor in parent
                .ancestors()
                .filter(|path| !path.as_os_str().is_empty())
            {
                fs::File::open(ancestor)?.sync_all()?;
            }
            Ok(workspace.clone())
        })();
        if temporary.exists() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result
    }
}
