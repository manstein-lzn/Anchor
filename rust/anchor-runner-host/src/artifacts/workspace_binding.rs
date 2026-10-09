use super::*;
use std::fs;
use workspace::PreparationLock;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceBinding {
    format: u32,
    run_id: String,
    graph_digest: String,
    node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceOwner {
    format: u32,
    key: InvocationKey,
    initialized: bool,
    input_commits: Vec<CommitRef>,
    previous_owner: Option<InvocationKey>,
    files: BTreeMap<String, FileHash>,
    directories: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InterruptedWorkspace {
    format: u32,
    key: InvocationKey,
    files: BTreeMap<String, FileHash>,
    directories: Vec<String>,
}

impl HostArtifacts {
    pub(crate) fn bind_node_workspace(
        &self,
        run_id: &str,
        graph_digest: &str,
        node_id: &str,
    ) -> Result<(), GraphError> {
        let key = InvocationKey {
            run_id: run_id.into(),
            graph_digest: graph_digest.into(),
            node_id: node_id.into(),
            invocation: 1,
        };
        validate_key(&key)?;
        let state = self.binding_state_path(&key);
        create_private_directory(state.parent().unwrap())?;
        create_private_directory(&state)?;
        let _lock = PreparationLock::acquire(&state.join(".prepare.lock"))?;
        if self.read_binding(&key)?.is_some() {
            let owner = self.read_workspace_owner(&key)?;
            let workspace = self.stable_workspace_path(&key);
            if owner.as_ref().is_some_and(|owner| owner.initialized) {
                require_directory(&workspace)?;
            } else if owner.is_none() && path_exists(&workspace)? {
                return Err(corrupt("node workspace has no initialization fact"));
            }
            return Ok(());
        }
        if path_exists(&state.join("owner.json"))?
            || path_exists(&self.stable_workspace_path(&key))?
        {
            return Err(corrupt("cannot bind an unowned existing node workspace"));
        }
        write_json_atomic(
            &state.join("binding.json"),
            &WorkspaceBinding {
                format: 1,
                run_id: key.run_id,
                graph_digest: key.graph_digest,
                node_id: key.node_id,
            },
        )?;
        sync_ancestors(&state)?;
        Ok(())
    }

    pub(super) fn bound_workspace_path(
        &self,
        key: &InvocationKey,
    ) -> Result<Option<PathBuf>, GraphError> {
        let state = self.binding_state_path(key);
        if !path_exists(&state)? {
            return Ok(None);
        }
        self.require_binding(key)?;
        let workspace = self.stable_workspace_path(key);
        checked_path(&workspace)?;
        match self.read_workspace_owner(key)? {
            Some(owner) if owner.key != *key || !owner.initialized => {
                return Err(GraphError::RunConflict);
            }
            Some(_) => require_directory(&workspace)?,
            None if path_exists(&workspace)? => {
                return Err(corrupt("node workspace has no initialization fact"));
            }
            None => (),
        }
        Ok(Some(workspace))
    }

    pub(super) fn prepare_bound_workspace(
        &self,
        key: &InvocationKey,
        inputs: &[CommitRef],
        previous: Option<&(PathBuf, Manifest)>,
        seed: Option<&InvocationKey>,
    ) -> Result<Option<PathBuf>, GraphError> {
        let state = self.binding_state_path(key);
        if !path_exists(&state)? {
            return Ok(None);
        }
        self.require_binding(key)?;
        let _lock = PreparationLock::acquire(&state.join(".prepare.lock"))?;
        self.require_binding(key)?;
        let workspace = self.stable_workspace_path(key);
        checked_path(&workspace)?;
        if let Some(mut owner) = self.read_workspace_owner(key)? {
            if owner.key == *key {
                if owner.input_commits != inputs {
                    return Err(corrupt("node workspace initialization inputs changed"));
                }
                if owner.initialized {
                    require_directory(&workspace)?;
                    return Ok(Some(workspace));
                }
                self.finish_workspace_initialization(&workspace, &mut owner, previous)?;
                return Ok(Some(workspace));
            }
            if !owner.initialized || key.invocation <= owner.key.invocation {
                return Err(GraphError::RunConflict);
            }
            require_directory(&workspace)?;
            let (files, directories) = self.workspace_release_fact(&owner.key, &workspace)?;
            self.write_workspace_owner(&WorkspaceOwner {
                format: 1,
                key: key.clone(),
                initialized: true,
                input_commits: inputs.to_vec(),
                previous_owner: Some(owner.key),
                files,
                directories,
            })?;
            return Ok(Some(workspace));
        }
        // A seeded workspace may already exist: a crash between renaming the
        // staged scene into place and recording the owner fact leaves exactly
        // that state, and the seed itself is the evidence that explains it. Any
        // other existing directory without an owner fact stays unowned and fails
        // closed, because nothing proves where its files came from.
        if path_exists(&workspace)? && seed.is_none() {
            return Err(corrupt("node workspace has no initialization fact"));
        }
        let seeded = match seed {
            Some(source) => self.seed_stable_workspace(source, key)?,
            None => None,
        };
        let (files, directories, previous_owner) = match seeded {
            Some(scene) => (scene.files, scene.directories, seed.cloned()),
            None => {
                if path_exists(&workspace)? {
                    return Err(corrupt("node workspace has no initialization fact"));
                }
                let (files, directories) = previous
                    .map(|(_, manifest)| (manifest.files.clone(), manifest.directories.clone()))
                    .unwrap_or_default();
                (files, directories, None)
            }
        };
        let mut owner = WorkspaceOwner {
            format: 1,
            key: key.clone(),
            initialized: false,
            input_commits: inputs.to_vec(),
            previous_owner,
            files,
            directories,
        };
        self.write_workspace_owner(&owner)?;
        self.finish_workspace_initialization(&workspace, &mut owner, previous)?;
        Ok(Some(workspace))
    }

    pub(crate) fn retain_interrupted_workspace(
        &self,
        key: &InvocationKey,
    ) -> Result<(), GraphError> {
        validate_key(key)?;
        let state = self.binding_state_path(key);
        let lock_path = if path_exists(&state)? {
            self.require_binding(key)?;
            state.join(".prepare.lock")
        } else {
            self.workspace_root
                .join(&key.run_id)
                .join(format!(".{}.prepare.lock", key_hash(key)))
        };
        let _lock = PreparationLock::acquire(&lock_path)?;
        let workspace = self.workspace_path(key)?;
        require_directory(&workspace)?;
        let checkpoint = self.interrupted_workspace_path(key);
        if path_exists(&checkpoint)? {
            let saved = self.read_interrupted_workspace(key)?;
            if scan_workspace(&workspace, None)? != (saved.files, saved.directories) {
                return Err(corrupt("interrupted workspace changed after retention"));
            }
            return Ok(());
        }
        let parent = checkpoint.parent().unwrap();
        create_private_directory(parent)?;
        let temporary = parent.join(format!(
            ".{}.{}.{}.tmp",
            key_hash(key),
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&temporary)?;
        let result = (|| {
            let (files, directories) = scan_workspace(&workspace, Some(&temporary.join("files")))?;
            if scan_workspace(&workspace, None)? != (files.clone(), directories.clone()) {
                return Err(corrupt("interrupted workspace changed during retention"));
            }
            write_json_atomic(
                &temporary.join("checkpoint.json"),
                &InterruptedWorkspace {
                    format: 1,
                    key: key.clone(),
                    files,
                    directories,
                },
            )?;
            fs::rename(&temporary, &checkpoint)?;
            sync_ancestors(parent)?;
            self.read_interrupted_workspace(key)?;
            Ok(())
        })();
        if temporary.exists() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result
    }

    fn finish_workspace_initialization(
        &self,
        workspace: &Path,
        owner: &mut WorkspaceOwner,
        previous: Option<&(PathBuf, Manifest)>,
    ) -> Result<(), GraphError> {
        if !path_exists(workspace)? {
            let parent = workspace.parent().unwrap();
            checked_path(parent)?;
            fs::create_dir_all(parent)?;
            require_directory(parent)?;
            let temporary = self.binding_state_path(&owner.key).join(format!(
                ".{}.{}.{}.prepare.tmp",
                key_hash(&owner.key),
                std::process::id(),
                TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let result = (|| {
                let (files, directories) = if let Some((source, _)) = previous {
                    scan_tree(&source.join("files"), Some(&temporary))?
                } else {
                    fs::create_dir(&temporary)?;
                    fs::File::open(&temporary)?.sync_all()?;
                    (BTreeMap::new(), vec![])
                };
                if files != owner.files || directories != owner.directories {
                    return Err(corrupt("node workspace initialization seed changed"));
                }
                fs::rename(&temporary, workspace)?;
                sync_ancestors(parent)
            })();
            if temporary.exists() {
                let _ = fs::remove_dir_all(&temporary);
            }
            result?;
        }
        if scan_workspace(workspace, None)? != (owner.files.clone(), owner.directories.clone()) {
            return Err(corrupt(
                "node workspace does not match its initialization fact",
            ));
        }
        owner.initialized = true;
        self.write_workspace_owner(owner)
    }

    fn binding_state_path(&self, key: &InvocationKey) -> PathBuf {
        self.workspace_root
            .join(&key.run_id)
            .join(".node-workspaces")
            .join(node_hash(&key.node_id))
    }

    fn stable_workspace_path(&self, key: &InvocationKey) -> PathBuf {
        self.workspace_root
            .join(&key.run_id)
            .join("nodes")
            .join(node_hash(&key.node_id))
    }

    fn read_binding(&self, key: &InvocationKey) -> Result<Option<WorkspaceBinding>, GraphError> {
        let path = self.binding_state_path(key).join("binding.json");
        if !path_exists(&path)? {
            return Ok(None);
        }
        let binding: WorkspaceBinding = read_private_json(&path)?;
        if binding.format != 1
            || binding.run_id != key.run_id
            || binding.graph_digest != key.graph_digest
            || binding.node_id != key.node_id
        {
            return Err(corrupt("node workspace binding identity mismatch"));
        }
        Ok(Some(binding))
    }

    fn require_binding(&self, key: &InvocationKey) -> Result<(), GraphError> {
        let state = self.binding_state_path(key);
        require_private_directory(state.parent().unwrap())?;
        require_private_directory(&state)?;
        self.read_binding(key)?
            .ok_or_else(|| corrupt("node workspace binding is missing"))?;
        Ok(())
    }

    fn read_workspace_owner(
        &self,
        key: &InvocationKey,
    ) -> Result<Option<WorkspaceOwner>, GraphError> {
        let path = self.binding_state_path(key).join("owner.json");
        if !path_exists(&path)? {
            return Ok(None);
        }
        let owner: WorkspaceOwner = read_private_json(&path)?;
        validate_key(&owner.key)?;
        if owner.format != 1
            || owner.key.run_id != key.run_id
            || owner.key.graph_digest != key.graph_digest
            || owner.key.node_id != key.node_id
        {
            return Err(corrupt("node workspace owner identity mismatch"));
        }
        if let Some(previous) = &owner.previous_owner {
            validate_key(previous)?;
            // The scene a Run owns can come from two places: an earlier
            // invocation of the same Run reusing its directory, or the stable
            // scene of the Run it took an assistant instance over from. Both are
            // recorded, so a previous owner may name another Run — but only for
            // the same Graph and node, and only an initialized owner may claim
            // one. Within one Run the invocation must still strictly increase.
            let same_run = previous.run_id == key.run_id;
            if previous.graph_digest != key.graph_digest
                || previous.node_id != key.node_id
                || (same_run && previous.invocation >= owner.key.invocation)
                || !owner.initialized
            {
                return Err(corrupt("node workspace previous owner mismatch"));
            }
        }
        Ok(Some(owner))
    }

    fn write_workspace_owner(&self, owner: &WorkspaceOwner) -> Result<(), GraphError> {
        write_json_atomic(
            &self.binding_state_path(&owner.key).join("owner.json"),
            owner,
        )
    }

    fn workspace_release_fact(
        &self,
        key: &InvocationKey,
        workspace: &Path,
    ) -> Result<(BTreeMap<String, FileHash>, Vec<String>), GraphError> {
        let actual = scan_workspace(workspace, None)?;
        let commit = commit_for(key);
        if path_exists(&self.root.join(&commit.id))? {
            let (_, manifest) = self.load_snapshot(&commit)?;
            if manifest.key != *key {
                return Err(corrupt("node workspace commit owner mismatch"));
            }
            if matches!(
                manifest.context.as_ref().map(|context| context.kind),
                Some(ArtifactKind::Node | ArtifactKind::GraphCall)
            ) && actual == (manifest.files, manifest.directories)
            {
                return Ok(actual);
            }
        }
        if path_exists(&self.interrupted_workspace_path(key))? {
            let saved = self.read_interrupted_workspace(key)?;
            if actual == (saved.files, saved.directories) {
                return Ok(actual);
            }
        }
        Err(corrupt(
            "node workspace owner has no saved execution outcome",
        ))
    }

    fn interrupted_workspace_path(&self, key: &InvocationKey) -> PathBuf {
        self.workspace_root
            .join(&key.run_id)
            .join(".interrupted-workspaces")
            .join(key_hash(key))
    }

    fn read_interrupted_workspace(
        &self,
        key: &InvocationKey,
    ) -> Result<InterruptedWorkspace, GraphError> {
        let checkpoint = self.interrupted_workspace_path(key);
        require_private_directory(checkpoint.parent().unwrap())?;
        require_private_directory(&checkpoint)?;
        let saved: InterruptedWorkspace = read_private_json(&checkpoint.join("checkpoint.json"))?;
        validate_key(&saved.key)?;
        let mut entries = fs::read_dir(&checkpoint)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort();
        if saved.format != 1
            || saved.key != *key
            || entries
                != [
                    std::ffi::OsString::from("checkpoint.json"),
                    std::ffi::OsString::from("files"),
                ]
            || scan_tree(&checkpoint.join("files"), None)?
                != (saved.files.clone(), saved.directories.clone())
        {
            return Err(corrupt("interrupted workspace identity or content changed"));
        }
        Ok(saved)
    }
}

fn node_hash(node_id: &str) -> String {
    format!("{:x}", Sha256::digest(node_id.as_bytes()))
}

fn path_exists(path: &Path) -> Result<bool, GraphError> {
    checked_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn create_private_directory(path: &Path) -> Result<(), GraphError> {
    checked_path(path)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    require_private_directory(path)
}

fn require_private_directory(path: &Path) -> Result<(), GraphError> {
    require_directory(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::symlink_metadata(path)?.permissions().mode() & 0o077 != 0 {
            return Err(corrupt(
                "node workspace facts require a private host directory",
            ));
        }
    }
    Ok(())
}

fn read_private_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, GraphError> {
    require_file(path)?;
    serde_json::from_slice(&fs::read(path)?).map_err(|error| corrupt(error.to_string()))
}

fn sync_ancestors(path: &Path) -> Result<(), GraphError> {
    for ancestor in path.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        fs::File::open(ancestor)?.sync_all()?;
    }
    if !path.is_absolute() {
        fs::File::open(".")?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "workspace_binding_tests.rs"]
mod tests;

/// One assistant instance's writable scene copied onto the next Run of the same
/// node. The facts are returned so the caller records them with the real
/// initialization inputs it is about to use.
pub(super) struct SeededScene {
    pub(super) files: BTreeMap<String, FileHash>,
    pub(super) directories: Vec<String>,
    pub(super) bytes: u64,
    pub(super) source_run: String,
}

impl HostArtifacts {
    /// Copy the writable scene of one Run of a node onto the next Run of the
    /// same node.
    ///
    /// Only the scene's own files travel: preparation locks, owner facts and
    /// prepared inputs live outside the stable workspace, so they are never
    /// copied, and symlinked components are rejected by the existing scan. The
    /// copy is staged in a temporary directory and renamed into place, so a
    /// failure leaves no half-initialized scene and a retry can succeed. An
    /// already-existing target is verified against the source instead of being
    /// copied twice, and the source scene is never modified.
    ///
    /// The caller decides whether a handover really happened; this only proves
    /// the two keys describe the same scene of the same node and Graph.
    ///
    /// The preparation lock is re-entrant inside one thread, so the node
    /// preparation path can call this while it already holds the same target
    /// lock — which is what makes the staged scene and the owner fact that
    /// records it one atomic decision.
    pub(super) fn seed_stable_workspace(
        &self,
        source: &InvocationKey,
        target: &InvocationKey,
    ) -> Result<Option<SeededScene>, GraphError> {
        validate_key(source)?;
        validate_key(target)?;
        if source.node_id != target.node_id || source.graph_digest != target.graph_digest {
            return Err(corrupt(
                "workspace seeding requires the same node and Graph",
            ));
        }
        if source.run_id == target.run_id {
            return Err(corrupt("workspace seeding requires a different Run"));
        }
        let source_root = self.stable_workspace_path(source);
        checked_path(&source_root)?;
        if !path_exists(&source_root)? {
            return Ok(None);
        }
        let target_root = self.stable_workspace_path(target);
        checked_path(&target_root)?;
        let state = self.binding_state_path(target);
        create_private_directory(&state)?;
        let parent = target_root
            .parent()
            .ok_or_else(|| corrupt("node workspace has no parent"))?;
        checked_path(parent)?;
        fs::create_dir_all(parent)?;
        require_directory(parent)?;
        let _lock = PreparationLock::acquire(&state.join(".prepare.lock"))?;
        if path_exists(&target_root)? {
            // The scene was already seeded: the second handover must not copy
            // again, and it must not silently accept a different scene.
            let (files, directories) = scan_tree(&target_root, None)?;
            let (expected, expected_directories) = scan_tree(&source_root, None)?;
            if files != expected || directories != expected_directories {
                return Err(corrupt("seeded node workspace differs from its source"));
            }
            return Ok(Some(SeededScene {
                bytes: files.values().map(|file| file.bytes).sum(),
                files,
                directories,
                source_run: source.run_id.clone(),
            }));
        }
        let temporary = state.join("seed.tmp");
        if path_exists(&temporary)? {
            fs::remove_dir_all(&temporary)?;
        }
        let staged = (|| -> Result<SeededScene, GraphError> {
            // `scan_tree` creates the staging directory itself.
            let (files, directories) = scan_tree(&source_root, Some(&temporary))?;
            fs::rename(&temporary, &target_root)?;
            Ok(SeededScene {
                bytes: files.values().map(|file| file.bytes).sum(),
                files,
                directories,
                source_run: source.run_id.clone(),
            })
        })();
        match staged {
            Ok(scene) => {
                sync_ancestors(&state)?;
                eprintln!(
                    "anchor-runner-host: seeded assistant workspace from {} ({} files, {} bytes)",
                    scene.source_run,
                    scene.files.len(),
                    scene.bytes
                );
                Ok(Some(scene))
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&temporary);
                Err(error)
            }
        }
    }
}
