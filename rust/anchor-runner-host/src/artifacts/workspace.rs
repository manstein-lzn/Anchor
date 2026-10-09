use super::*;
use std::cell::RefCell;

thread_local! {
    /// Preparation locks this thread already holds, so a primitive that has to
    /// run inside the same preparation (workspace seeding) can acquire the same
    /// lock again without deadlocking against itself. Cross-thread exclusion is
    /// unchanged: the outer guard still owns the file lock.
    static HELD_PREPARATIONS: RefCell<BTreeMap<PathBuf, usize>> =
        const { RefCell::new(BTreeMap::new()) };
}

fn enter_held(path: &Path) {
    HELD_PREPARATIONS.with(|held| {
        *held.borrow_mut().entry(path.to_path_buf()).or_default() += 1;
    });
}

fn held_by_this_thread(path: &Path) -> bool {
    HELD_PREPARATIONS.with(|held| held.borrow().contains_key(path))
}

fn leave_held(path: &Path) {
    HELD_PREPARATIONS.with(|held| {
        let mut held = held.borrow_mut();
        if let Some(count) = held.get_mut(path) {
            *count -= 1;
            if *count == 0 {
                held.remove(path);
            }
        }
    });
}

pub(super) enum PreparationLock {
    /// This guard owns the file lock.
    Owned(fs::File, PathBuf),
    /// This thread already owns the file lock through an outer guard.
    Nested(PathBuf),
}

impl PreparationLock {
    pub(super) fn acquire(path: &Path) -> Result<Self, GraphError> {
        checked_path(path)?;
        if fs::symlink_metadata(path).is_ok() {
            require_file(path)?;
        }
        if held_by_this_thread(path) {
            enter_held(path);
            return Ok(Self::Nested(path.to_path_buf()));
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;
        enter_held(path);
        Ok(Self::Owned(file, path.to_path_buf()))
    }
}

impl Drop for PreparationLock {
    fn drop(&mut self) {
        match self {
            Self::Owned(file, path) => {
                leave_held(path);
                let _ = file.unlock();
            }
            Self::Nested(path) => leave_held(path),
        }
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
        self.prepare_workspace_from(key, inputs, None)
    }

    /// Prepare a node workspace, optionally seeding it from the stable scene of
    /// the Run this assistant instance was handed over from.
    ///
    /// `seed` is the handed-over source key. The copy, the owner fact and the
    /// real `inputs` of this execution are written under the same
    /// `.prepare.lock`, so a resume either sees a complete workspace with the
    /// inputs it was told to use or nothing at all. Without a seed every path is
    /// exactly the unbound/committed-seed behaviour. An unbound invocation has
    /// no owner fact to seed into, so it keeps its plain workspace.
    pub(crate) fn prepare_workspace_from(
        &self,
        key: &InvocationKey,
        inputs: &[CommitRef],
        seed: Option<&InvocationKey>,
    ) -> Result<PathBuf, GraphError> {
        validate_key(key)?;
        let snapshots = self.expanded_snapshots(inputs, Some((&key.run_id, &key.graph_digest)))?;
        let previous = snapshots
            .iter()
            .find(|(_, manifest)| manifest.key.node_id == key.node_id);
        if previous.is_some_and(|(_, manifest)| manifest.key.invocation >= key.invocation) {
            return Err(corrupt("workspace seed must be an earlier node invocation"));
        }
        if let Some(workspace) = self.prepare_bound_workspace(key, inputs, previous, seed)? {
            return Ok(workspace);
        }
        let workspace = self.workspace_path(key)?;
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
