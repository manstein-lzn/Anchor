//! Host-owned, immutable file snapshots behind the shared ArtifactPort.

use anchor_runtime::{
    ReadOnlyInput,
    graph::{
        ArtifactFreezeContext, ArtifactKind, ArtifactPort, CallFileSelection, CommitRef,
        GraphError, InvocationKey, NodeCompletion,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    future::Future,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const FILE_BUFFER_BYTES: usize = 64 * 1024;

mod git;
mod links;
mod manifest;
mod previous;
mod workspace;
mod workspace_binding;
use manifest::{FileHash, Manifest, context_hash};

#[derive(Clone)]
pub(crate) struct HostArtifacts {
    root: PathBuf,
    workspace_root: PathBuf,
}

impl HostArtifacts {
    pub(crate) fn new(root: PathBuf, workspace_root: PathBuf) -> Self {
        Self {
            root,
            workspace_root,
        }
    }

    /// Returns a validated path without creating a workspace.
    pub(crate) fn workspace_path(&self, key: &InvocationKey) -> Result<PathBuf, GraphError> {
        validate_key(key)?;
        if let Some(path) = self.bound_workspace_path(key)? {
            return Ok(path);
        }
        let path = self.workspace_root.join(&key.run_id).join(key_hash(key));
        checked_path(&path)?;
        Ok(path)
    }

    pub(crate) fn input_mounts(
        &self,
        commits: &[CommitRef],
        run_id: &str,
        graph_digest: &str,
    ) -> Result<Vec<ReadOnlyInput>, GraphError> {
        let mut mounts: Vec<ReadOnlyInput> = Vec::new();
        for (path, manifest) in self.expanded_snapshots(commits, Some((run_id, graph_digest)))? {
            let destination = Path::new("/in").join(&manifest.key.node_id);
            if mounts.iter().any(|mount| {
                mount.destination.starts_with(&destination)
                    || destination.starts_with(&mount.destination)
            }) {
                return Err(corrupt("artifact mount paths overlap"));
            }
            mounts.push(ReadOnlyInput::new(
                self.git_projection(&path, &manifest)?,
                destination,
            ));
        }
        // A child admitted by Op.call sees the explicitly selected parent
        // committed files read-only at `/in/call`, alongside its own committed
        // inputs. The bundle is staged before the child runs and frozen with its
        // identity, so it is stable across a wait resume.
        let call_inputs = self.call_inputs_path(run_id)?;
        let call_manifest = self.call_inputs_manifest_path(run_id)?;
        let bundle_present = fs::symlink_metadata(&call_inputs).is_ok();
        let manifest_present = fs::symlink_metadata(&call_manifest).is_ok();
        if bundle_present || manifest_present {
            // A half-published bundle (bundle without its recorded identity, or
            // a surviving manifest whose bundle is gone) can never be proven
            // identical to the frozen selection, so it fails closed instead of
            // mounting a missing or silently rebuilt input.
            require_directory(&call_inputs)?;
            self.verify_call_inputs_bundle(run_id, &call_inputs, None, None)?;
            let destination = PathBuf::from("/in/call");
            if mounts.iter().any(|mount| mount.destination == destination) {
                return Err(corrupt("call input mount conflicts with a committed input"));
            }
            mounts.push(ReadOnlyInput::new(call_inputs, destination));
        }
        Ok(mounts)
    }

    fn call_inputs_path(&self, run_id: &str) -> Result<PathBuf, GraphError> {
        validate_component(run_id)?;
        // The bundle is a read-only sandbox source, so it lives under the
        // host-authorized artifact root rather than the writable workspace root.
        let root = self.root.join("call-inputs");
        checked_path(&root)?;
        let path = root.join(run_id);
        checked_path(&path)?;
        Ok(path)
    }

    fn call_inputs_manifest_path(&self, run_id: &str) -> Result<PathBuf, GraphError> {
        let mut path = self.call_inputs_path(run_id)?;
        path.set_file_name(format!("{run_id}.json"));
        Ok(path)
    }

    /// Re-derive the staged `/in/call` bundle and prove it is exactly the
    /// parent-keyed, child-identified, hash-recorded selection. A tampered or
    /// incomplete bundle fails closed instead of being mounted.
    fn verify_call_inputs_bundle(
        &self,
        run_id: &str,
        bundle: &Path,
        expected_parents: Option<&[CommitRef]>,
        expected_selections: Option<&[CallFileSelection]>,
    ) -> Result<(), GraphError> {
        checked_path(bundle)?;
        let manifest_path = self.call_inputs_manifest_path(run_id)?;
        require_file(&manifest_path)?;
        let manifest: CallInputManifest = serde_json::from_slice(&fs::read(&manifest_path)?)
            .map_err(|error| corrupt(format!("call input manifest is unreadable: {error}")))?;
        if manifest.format != 1 || manifest.child_run_id != run_id {
            return Err(corrupt(
                "call input manifest does not belong to this child Run",
            ));
        }
        if let Some(expected) = expected_parents
            && manifest.parents != expected
        {
            return Err(corrupt(
                "call input bundle was staged for a different parent selection",
            ));
        }
        if let Some(expected) = expected_selections {
            // The frozen selection, not just the parent commits, must match.
            // A retry that asks for different `(node, path, alias)` inputs is a
            // different request and must fail closed rather than reuse this
            // bundle or overwrite it.
            if manifest.files.len() != expected.len()
                || manifest
                    .files
                    .iter()
                    .zip(expected)
                    .any(|(file, selection)| {
                        file.node != selection.node
                            || file.path != selection.path
                            || file.alias != selection.alias
                    })
            {
                return Err(corrupt(
                    "call input bundle was staged for a different file selection",
                ));
            }
        }
        let mut expected = BTreeMap::new();
        for file in &manifest.files {
            validate_file_relative(&file.alias)?;
            if expected
                .insert(file.alias.clone(), (file.sha256.clone(), file.bytes))
                .is_some()
            {
                return Err(corrupt("call input manifest declares a duplicate alias"));
            }
        }
        let actual = scan_hash_tree(bundle)?;
        if actual != expected {
            return Err(corrupt(
                "call input bundle does not match its recorded identity and hashes",
            ));
        }
        Ok(())
    }

    /// The caller must first authorize the CommitRef through durable Run results.
    pub(crate) fn files_path(&self, commit: &CommitRef) -> Result<PathBuf, GraphError> {
        self.load_snapshot(commit)
            .map(|(path, _)| path.join("files"))
    }

    /// Lists only files declared by a fully verified immutable snapshot.
    /// Callers must first authorize the CommitRef through the Run record.
    pub(crate) fn list_files(&self, commit: &CommitRef) -> Result<Vec<(String, u64)>, GraphError> {
        let (_, manifest) = self.load_snapshot(commit)?;
        Ok(manifest
            .files
            .into_iter()
            .map(|(path, file)| (path, file.bytes))
            .collect())
    }

    fn load_snapshot(&self, commit: &CommitRef) -> Result<(PathBuf, Manifest), GraphError> {
        self.expanded_snapshots(std::slice::from_ref(commit), None)?
            .into_iter()
            .next()
            .ok_or_else(|| corrupt("artifact is missing"))
    }

    fn read_snapshot(&self, commit: &CommitRef) -> Result<(PathBuf, Manifest), GraphError> {
        if !valid_fs_id(&commit.id) {
            return Err(GraphError::Unsupported(
                "legacy artifacts have no file snapshot".into(),
            ));
        }
        validate_relative(&commit.node_id)?;
        let path = self.root.join(&commit.id);
        require_directory(&path)?;
        let manifest_path = path.join("manifest.json");
        require_file(&manifest_path)?;
        let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)
            .map_err(|error| corrupt(error.to_string()))?;
        validate_key(&manifest.key)?;
        let expected = if manifest.format == 1 {
            legacy_commit_for(&manifest.key)
        } else {
            commit_for(&manifest.key)
        };
        if expected != *commit {
            return Err(corrupt("artifact identity or format mismatch"));
        }
        manifest.validate_context()?;
        let mut entries = fs::read_dir(&path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort();
        for name in ["git-view", ".git-view.lock"] {
            if let Some(index) = entries.iter().position(|entry| entry == name) {
                if name == "git-view" {
                    require_directory(&path.join(name))?;
                } else {
                    require_file(&path.join(name))?;
                }
                entries.remove(index);
            }
        }
        if entries
            != [
                std::ffi::OsString::from("files"),
                std::ffi::OsString::from("manifest.json"),
            ]
        {
            return Err(corrupt("artifact contains undeclared resources"));
        }
        let (files, directories) = scan_tree(&path.join("files"), None)?;
        if manifest.files != files || manifest.directories != directories {
            return Err(corrupt("artifact file digest or tree mismatch"));
        }
        Ok((path, manifest))
    }

    fn freeze_snapshot(
        &self,
        key: &InvocationKey,
        completion: &NodeCompletion,
        context: &ArtifactFreezeContext,
    ) -> Result<CommitRef, GraphError> {
        validate_key(key)?;
        checked_path(&self.root)?;
        let root_existed = self.root.exists();
        fs::create_dir_all(&self.root)?;
        require_directory(&self.root)?;
        if !root_existed {
            for ancestor in self
                .root
                .ancestors()
                .filter(|path| !path.as_os_str().is_empty())
            {
                fs::File::open(ancestor)?.sync_all()?;
            }
            if !self.root.is_absolute() {
                fs::File::open(".")?.sync_all()?;
            }
        }
        self.expanded_snapshots(
            &context.input_commits,
            Some((&key.run_id, &key.graph_digest)),
        )?;
        let legacy = legacy_commit_for(key);
        let commit = if fs::symlink_metadata(self.root.join(&legacy.id)).is_ok() {
            if context.kind != ArtifactKind::Node || !context.input_commits.is_empty() {
                return Err(GraphError::Unsupported(
                    "cannot add provenance to an existing fs1 artifact".into(),
                ));
            }
            legacy
        } else {
            commit_for(key)
        };
        let published = self.root.join(&commit.id);
        if fs::symlink_metadata(&published).is_ok() {
            let (_, old) = self.load_snapshot(&commit)?;
            if old.key != *key
                || old.completion != *completion
                || old.context.as_ref().is_some_and(|old| old != context)
            {
                return Err(GraphError::RunConflict);
            }
            return Ok(commit);
        }
        let workspace = match context.kind {
            ArtifactKind::Node => {
                let workspace = self.workspace_path(key)?;
                require_directory(&workspace)?;
                Some(workspace)
            }
            // A Graph call node has no sandbox workspace of its own, but a wait
            // call copies explicitly selected child results into `<workspace>/result/`.
            // Freezing that directory is what makes them readable downstream.
            ArtifactKind::GraphCall => {
                let workspace = self.workspace_path(key)?;
                if workspace.is_dir() {
                    // Refuse to freeze a result tree that an interrupted export
                    // left half-published, otherwise an empty or partial
                    // `result/` would be committed as the call output.
                    ensure_export_complete(&workspace)?;
                    Some(workspace)
                } else {
                    None
                }
            }
            ArtifactKind::Fanout | ArtifactKind::Join | ArtifactKind::Interruption => None,
        };
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = self.root.join(format!(
            ".{}.{}.{}.tmp",
            commit.id,
            std::process::id(),
            sequence
        ));
        fs::create_dir(&temporary)?;
        let result = (|| {
            let (files, directories) = if let Some(workspace) = &workspace {
                scan_workspace(workspace, Some(&temporary.join("files")))?
            } else {
                let files_dir = temporary.join("files");
                fs::create_dir(&files_dir)?;
                let files = if context.kind != ArtifactKind::GraphCall {
                    let filename = manifest::control_filename(context.kind)
                        .ok_or_else(|| corrupt("unsupported control artifact kind"))?;
                    let bytes =
                        serde_json::to_vec(&manifest::control_output(completion, context.kind))
                            .map_err(|error| corrupt(error.to_string()))?;
                    let mut file = fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(files_dir.join(filename))?;
                    file.write_all(&bytes)?;
                    file.sync_all()?;
                    BTreeMap::from([(
                        filename.to_owned(),
                        FileHash {
                            sha256: format!("{:x}", Sha256::digest(&bytes)),
                            bytes: bytes.len() as u64,
                        },
                    )])
                } else {
                    BTreeMap::new()
                };
                fs::File::open(&files_dir)?.sync_all()?;
                (files, vec![])
            };
            let manifest = Manifest {
                format: 2,
                key: key.clone(),
                completion: completion.clone(),
                files,
                directories,
                context: Some(context.clone()),
                context_sha256: Some(context_hash(context)?),
            };
            let bytes =
                serde_json::to_vec(&manifest).map_err(|error| corrupt(error.to_string()))?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(temporary.join("manifest.json"))?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::File::open(&temporary)?.sync_all()?;
            if let Err(error) = fs::rename(&temporary, &published) {
                if fs::symlink_metadata(&published).is_err() {
                    return Err(error.into());
                }
                let (_, old) = self.load_snapshot(&commit)?;
                if old.key != *key
                    || old.completion != *completion
                    || old.context.as_ref() != Some(context)
                {
                    return Err(GraphError::RunConflict);
                }
            }
            fs::File::open(&self.root)?.sync_all()?;
            self.load_snapshot(&commit)?;
            Ok(commit)
        })();
        if temporary.exists() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result
    }
}

impl ArtifactPort for HostArtifacts {
    fn freeze<'a>(
        &'a self,
        key: &'a InvocationKey,
        completion: &'a NodeCompletion,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            self.freeze_snapshot(
                key,
                completion,
                &ArtifactFreezeContext {
                    kind: ArtifactKind::Node,
                    input_commits: vec![],
                },
            )
        })
    }

    fn freeze_with_context<'a>(
        &'a self,
        key: &'a InvocationKey,
        completion: &'a NodeCompletion,
        context: &'a ArtifactFreezeContext,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
        Box::pin(async move { self.freeze_snapshot(key, completion, context) })
    }

    fn resolve<'a>(
        &'a self,
        commit: &'a CommitRef,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let completion = if valid_fs_id(&commit.id) {
                self.load_snapshot(commit)?.1.completion
            } else {
                validate_component(&commit.id)?;
                let path = self.root.join(format!("{}.json", commit.id));
                require_file(&path)?;
                serde_json::from_slice::<NodeCompletion>(&fs::read(path)?)
                    .map_err(|error| corrupt(error.to_string()))?
            };
            serde_json::to_value(completion).map_err(|error| corrupt(error.to_string()))
        })
    }

    fn stage_call_inputs<'a>(
        &'a self,
        child_run_id: &'a str,
        parents: &'a [CommitRef],
        selections: &'a [CallFileSelection],
    ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
        Box::pin(async move {
            if selections.is_empty() {
                return Ok(());
            }
            // Reject a malformed selection before any durable side effect: a
            // duplicate alias would otherwise publish a bundle the verifier
            // must reject, leaving the child permanently unable to admit.
            let mut aliases = std::collections::BTreeSet::new();
            for selection in selections {
                validate_file_relative(&selection.path)?;
                validate_file_relative(&selection.alias)?;
                if !aliases.insert(selection.alias.clone()) {
                    return Err(corrupt("call input selection declares a duplicate alias"));
                }
            }
            let final_dir = self.call_inputs_path(child_run_id)?;
            let manifest_path = self.call_inputs_manifest_path(child_run_id)?;
            let parent_dir = final_dir
                .parent()
                .ok_or_else(|| corrupt("call input bundle has no parent"))?;
            fs::create_dir_all(parent_dir)?;
            if let Ok(metadata) = fs::symlink_metadata(&final_dir) {
                if !metadata.is_dir() {
                    return Err(corrupt("call input bundle path is not a directory"));
                }
                if !manifest_path.is_file() {
                    // A published bundle whose recorded identity vanished cannot
                    // be proven identical to the frozen selection. Rebuilding it
                    // would silently hand the child a different input tree, so
                    // this fails closed until the admission is fully reset.
                    return Err(corrupt(
                        "call input bundle exists without its recorded identity",
                    ));
                }
                // The bundle is frozen with the child identity; a resume must
                // observe exactly the inputs the first admission selected.
                return self.verify_call_inputs_bundle(
                    child_run_id,
                    &final_dir,
                    Some(parents),
                    Some(selections),
                );
            }
            // Clear temp trees this child left behind when a previous attempt
            // died before publishing. A completed bundle is never named
            // `.call-inputs-*`, so this cannot drop a committed input.
            remove_stale_call_input_temps(parent_dir, child_run_id)?;
            let snapshots = self.expanded_snapshots(parents, None)?;
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temporary = parent_dir.join(format!(
                ".call-inputs-{child_run_id}-{}-{}.tmp",
                std::process::id(),
                sequence
            ));
            let _ = fs::remove_dir_all(&temporary);
            fs::create_dir(&temporary)?;
            let result = (|| {
                let mut files = Vec::with_capacity(selections.len());
                for selection in selections {
                    let (source_root, _) = snapshots
                        .iter()
                        .find(|(_, manifest)| manifest.key.node_id == selection.node)
                        .ok_or_else(|| {
                            GraphError::InvalidSnapshot(format!(
                                "Op.call file source node `{}` is not a visible committed input",
                                selection.node
                            ))
                        })?;
                    let source = source_root.join("files").join(&selection.path);
                    checked_path(&source)?;
                    require_file(&source)?;
                    let (sha256, bytes) =
                        copy_regular_file(&source, &temporary.join(&selection.alias))?;
                    files.push(CallInputFile {
                        node: selection.node.clone(),
                        path: selection.path.clone(),
                        alias: selection.alias.clone(),
                        sha256,
                        bytes,
                    });
                }
                // Persist every directory level before the manifest records the
                // hashes, so a crash cannot leave a bundle whose nested entries
                // are missing after the rename.
                sync_tree_dirs(&temporary)?;
                // Publish the manifest before the bundle directory so a crash can
                // only leave an incomplete publish that the next attempt rebuilds,
                // never a Ready child whose inputs are missing.
                write_json_atomic(
                    &manifest_path,
                    &CallInputManifest {
                        format: 1,
                        child_run_id: child_run_id.to_owned(),
                        parents: parents.to_vec(),
                        files,
                    },
                )?;
                fs::rename(&temporary, &final_dir)?;
                fs::File::open(parent_dir)?.sync_all()?;
                self.verify_call_inputs_bundle(
                    child_run_id,
                    &final_dir,
                    Some(parents),
                    Some(selections),
                )
            })();
            if temporary.exists() {
                let _ = fs::remove_dir_all(&temporary);
            }
            result
        })
    }

    fn export_call_result_files<'a>(
        &'a self,
        call_key: &'a InvocationKey,
        commit: &'a CommitRef,
        files: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            if files.is_empty() {
                return Ok(Vec::new());
            }
            validate_key(call_key)?;
            let (snapshot, manifest) = self.load_snapshot(commit)?;
            let workspace = match self.prepare_bound_workspace(call_key, &[], None, None)? {
                Some(workspace) => workspace,
                None => self.workspace_path(call_key)?,
            };
            fs::create_dir_all(&workspace)?;
            checked_path(&workspace)?;
            remove_stale_result_dirs(&workspace)?;
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temporary =
                workspace.join(format!(".result-{}-{}.tmp", std::process::id(), sequence));
            let _ = fs::remove_dir_all(&temporary);
            fs::create_dir(&temporary)?;
            let copied = (|| {
                let mut copied = Vec::with_capacity(files.len());
                for name in files {
                    validate_file_relative(name)?;
                    let recorded = manifest.files.get(name).ok_or_else(|| {
                        corrupt(format!("result file `{name}` is not in the child commit"))
                    })?;
                    let source = snapshot.join("files").join(name);
                    checked_path(&source)?;
                    require_file(&source)?;
                    let (sha256, bytes) = copy_regular_file(&source, &temporary.join(name))?;
                    if sha256 != recorded.sha256 || bytes != recorded.bytes {
                        return Err(corrupt(format!(
                            "result file `{name}` changed after the child commit was verified"
                        )));
                    }
                    copied.push(name.clone());
                }
                sync_tree_dirs(&temporary)?;
                Ok::<_, GraphError>(copied)
            })();
            let copied = match copied {
                Ok(copied) => copied,
                Err(error) => {
                    let _ = fs::remove_dir_all(&temporary);
                    return Err(error);
                }
            };
            // Atomically replace `<workspace>/result` so a crash during transfer
            // never leaves a partial result tree for the freeze to commit.
            let target = workspace.join("result");
            let result = publish_directory(&temporary, &target);
            if temporary.exists() {
                let _ = fs::remove_dir_all(&temporary);
            }
            result?;
            fs::File::open(&workspace)?.sync_all()?;
            Ok(copied)
        })
    }
}

/// Durable, immutable record binding one child `/in/call` bundle to the exact
/// parent commits and file hashes that produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallInputManifest {
    format: u32,
    child_run_id: String,
    parents: Vec<CommitRef>,
    files: Vec<CallInputFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallInputFile {
    node: String,
    path: String,
    alias: String,
    sha256: String,
    bytes: u64,
}

fn copy_regular_file(source: &Path, destination: &Path) -> Result<(String, u64), GraphError> {
    if let Some(parent) = destination.parent() {
        checked_path(parent)?;
        fs::create_dir_all(parent)?;
    }
    checked_path(destination)?;
    let mut input = fs::File::open(source)?;
    let mut output = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(destination)?;
    let mut buffer = vec![0_u8; FILE_BUFFER_BYTES];
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    loop {
        let count = match input.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        if count == 0 {
            break;
        }
        let chunk = &buffer[..count];
        hash.update(chunk);
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| corrupt("file size overflow"))?;
        output.write_all(chunk)?;
    }
    output.sync_all()?;
    Ok((format!("{:x}", hash.finalize()), bytes))
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), GraphError> {
    let bytes = serde_json::to_vec(value).map_err(|error| corrupt(error.to_string()))?;
    let parent = path
        .parent()
        .ok_or_else(|| corrupt("manifest path has no parent"))?;
    checked_path(parent)?;
    fs::create_dir_all(parent)?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("manifest"),
        std::process::id(),
        sequence
    ));
    let _ = fs::remove_file(&temporary);
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn publish_directory(temporary: &Path, target: &Path) -> Result<(), GraphError> {
    match fs::symlink_metadata(target) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(corrupt("result destination is not a directory"));
            }
            checked_path(target)?;
            let old = target.with_file_name(format!(
                ".result-old-{}-{}.tmp",
                std::process::id(),
                TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&old);
            fs::rename(target, &old)?;
            fs::rename(temporary, target)?;
            let _ = fs::remove_dir_all(&old);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::rename(temporary, target)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn remove_stale_result_dirs(workspace: &Path) -> Result<(), GraphError> {
    for entry in fs::read_dir(workspace)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".result") {
            let path = entry.path();
            checked_path(&path)?;
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

/// A Graph call workspace is only mutated by `export_call_result_files`, which
/// publishes `<workspace>/result` through a temporary `.result-*` tree. Any
/// surviving `.result-*` entry means an export was interrupted, so the freeze
/// fails closed instead of committing a partial result tree.
fn ensure_export_complete(workspace: &Path) -> Result<(), GraphError> {
    checked_path(workspace)?;
    for entry in fs::read_dir(workspace)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(".result") {
            return Err(corrupt(
                "result export was interrupted; refusing to freeze a partial call result",
            ));
        }
    }
    Ok(())
}

/// Remove the temporary bundle trees a previous `stage_call_inputs` attempt for
/// `child_run_id` left behind when it crashed before publishing. Scoped to this
/// child so an in-flight stage for a different child is never disturbed.
fn remove_stale_call_input_temps(parent: &Path, child_run_id: &str) -> Result<(), GraphError> {
    let prefix = format!(".call-inputs-{child_run_id}-");
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".tmp") {
            let path = entry.path();
            checked_path(&path)?;
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

/// fsync every directory in a freshly built tree. File contents are already
/// synced per file, but a nested directory entry is only durable once its own
/// directory is synced, not just the top-level parent.
fn sync_tree_dirs(root: &Path) -> Result<(), GraphError> {
    checked_path(root)?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        checked_path(&path)?;
        if entry.file_type()?.is_dir() {
            sync_tree_dirs(&path)?;
        }
    }
    fs::File::open(root)?.sync_all()?;
    Ok(())
}

fn scan_hash_tree(root: &Path) -> Result<BTreeMap<String, (String, u64)>, GraphError> {
    let mut files = BTreeMap::new();
    scan_hash_directory(root, root, &mut files)?;
    Ok(files)
}

fn scan_hash_directory(
    root: &Path,
    current: &Path,
    files: &mut BTreeMap<String, (String, u64)>,
) -> Result<(), GraphError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        checked_path(&path)?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| corrupt("call input path escapes root"))?
            .to_str()
            .ok_or_else(|| corrupt("call input paths must be UTF-8"))?
            .to_owned();
        validate_file_relative(&relative)?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            scan_hash_directory(root, &path, files)?;
        } else if kind.is_file() {
            let mut buffer = vec![0_u8; FILE_BUFFER_BYTES];
            let mut file = fs::File::open(&path)?;
            let mut hash = Sha256::new();
            let mut bytes = 0_u64;
            loop {
                let count = match file.read(&mut buffer) {
                    Ok(count) => count,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                };
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
                bytes = bytes
                    .checked_add(count as u64)
                    .ok_or_else(|| corrupt("call input size overflow"))?;
            }
            if files
                .insert(relative, (format!("{:x}", hash.finalize()), bytes))
                .is_some()
            {
                return Err(corrupt("duplicate call input file"));
            }
        } else {
            return Err(corrupt(
                "call input symlinks and special files are not supported",
            ));
        }
    }
    Ok(())
}

fn corrupt(message: impl Into<String>) -> GraphError {
    GraphError::CorruptRun(message.into())
}
fn key_hash(key: &InvocationKey) -> String {
    format!("{:x}", Sha256::digest(key.durable_key().as_bytes()))
}
fn commit_for(key: &InvocationKey) -> CommitRef {
    CommitRef {
        id: format!("fs2-{}", key_hash(key)),
        node_id: key.node_id.clone(),
        invocation: key.invocation,
    }
}
fn legacy_commit_for(key: &InvocationKey) -> CommitRef {
    CommitRef {
        id: format!("fs1-{}", key_hash(key)),
        node_id: key.node_id.clone(),
        invocation: key.invocation,
    }
}
fn valid_fs_id(id: &str) -> bool {
    id.strip_prefix("fs1-")
        .or_else(|| id.strip_prefix("fs2-"))
        .is_some_and(|suffix| {
            suffix.len() == 64
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
}
fn validate_component(value: &str) -> Result<(), GraphError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    {
        return Err(corrupt("unsafe artifact identity component"));
    }
    Ok(())
}
fn validate_relative(value: &str) -> Result<(), GraphError> {
    if value.is_empty() || value.contains('\\') || value.split('/').any(|part| part.is_empty()) {
        return Err(corrupt("unsafe artifact relative path"));
    }
    for part in value.split('/') {
        validate_component(part)?;
    }
    Ok(())
}
fn validate_file_relative(value: &str) -> Result<(), GraphError> {
    if value.is_empty()
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(corrupt("unsafe artifact file path"));
    }
    Ok(())
}
fn validate_key(key: &InvocationKey) -> Result<(), GraphError> {
    validate_component(&key.run_id)?;
    validate_component(&key.graph_digest)?;
    validate_relative(&key.node_id)?;
    if key.invocation == 0 {
        return Err(corrupt("artifact invocation must be positive"));
    }
    Ok(())
}
fn checked_path(path: &Path) -> Result<(), GraphError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            _ => return Err(corrupt("unsafe host artifact path")),
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(corrupt("artifact symlinks are not supported"));
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn require_directory(path: &Path) -> Result<(), GraphError> {
    checked_path(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(corrupt("artifact directory required"));
    }
    Ok(())
}
fn require_file(path: &Path) -> Result<(), GraphError> {
    checked_path(path)?;
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(corrupt("artifact regular file required"));
    }
    Ok(())
}

fn scan_tree(
    root: &Path,
    destination: Option<&Path>,
) -> Result<(BTreeMap<String, FileHash>, Vec<String>), GraphError> {
    require_directory(root)?;
    let mut files = BTreeMap::new();
    let mut directories = Vec::new();
    scan_directory(root, root, destination, &mut files, &mut directories, false)?;
    directories.sort();
    Ok((files, directories))
}
fn scan_workspace(
    root: &Path,
    destination: Option<&Path>,
) -> Result<(BTreeMap<String, FileHash>, Vec<String>), GraphError> {
    require_directory(root)?;
    let mut files = BTreeMap::new();
    let mut directories = Vec::new();
    scan_directory(root, root, destination, &mut files, &mut directories, true)?;
    directories.sort();
    Ok((files, directories))
}
fn scan_directory(
    root: &Path,
    current: &Path,
    destination: Option<&Path>,
    files: &mut BTreeMap<String, FileHash>,
    directories: &mut Vec<String>,
    exclude_workspace_git: bool,
) -> Result<(), GraphError> {
    if let Some(destination) = destination {
        fs::create_dir(destination)?;
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        // Git metadata belongs to the host projection, never the Agent's
        // writable files or its configuration and hooks.
        if exclude_workspace_git && current == root && entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        checked_path(&path)?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| corrupt("artifact path escapes root"))?
            .to_str()
            .ok_or_else(|| corrupt("artifact paths must be UTF-8"))?
            .to_owned();
        validate_file_relative(&relative)?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            directories.push(relative);
            let next = destination.map(|destination| destination.join(entry.file_name()));
            scan_directory(
                root,
                &path,
                next.as_deref(),
                files,
                directories,
                exclude_workspace_git,
            )?;
        } else if kind.is_file() {
            let mut source = fs::File::open(&path)?;
            let mut copy = destination
                .map(|destination| {
                    fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(destination.join(entry.file_name()))
                })
                .transpose()?;
            let mut buffer = vec![0_u8; FILE_BUFFER_BYTES];
            let mut hash = Sha256::new();
            let mut bytes = 0_u64;
            loop {
                let count = match source.read(&mut buffer) {
                    Ok(count) => count,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                };
                if count == 0 {
                    break;
                }
                let chunk = &buffer[..count];
                hash.update(chunk);
                bytes = bytes
                    .checked_add(count as u64)
                    .ok_or_else(|| corrupt("artifact file size overflow"))?;
                if let Some(copy) = &mut copy {
                    copy.write_all(chunk)?;
                }
            }
            if let Some(copy) = copy {
                copy.sync_all()?;
            }
            files.insert(
                relative,
                FileHash {
                    sha256: format!("{:x}", hash.finalize()),
                    bytes,
                },
            );
        } else {
            return Err(corrupt(
                "artifact symlinks and special files are not supported",
            ));
        }
    }
    if let Some(destination) = destination {
        fs::File::open(destination)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "artifacts/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "artifacts/provenance_tests.rs"]
mod provenance_tests;

#[cfg(test)]
#[path = "artifacts/coordinator_tests.rs"]
mod coordinator_tests;

#[cfg(test)]
#[path = "artifacts/parity_tests.rs"]
mod parity_tests;
