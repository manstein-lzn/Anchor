//! Host-owned immutable deliveries, content objects and read-only input views.

use crate::host_fs::{invocation_stem as key_hash, validate_component, validate_key};
use anchor_runtime::{
    ReadOnlyInput,
    artifact::{
        ARTIFACT_DESCRIPTOR_FILE, ArtifactDescriptor, ArtifactSelection,
        store::{
            checked_path, commit_for, corrupt, manifest_reference, require_directory, require_file,
            valid_fs_id, valid_sha256, validate_file_relative,
        },
    },
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
    path::{Path, PathBuf},
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const FILE_BUFFER_BYTES: usize = 64 * 1024;

mod lifecycle;
mod links;
mod manifest;
mod objects;
mod workspace;
mod workspace_binding;
pub(crate) use lifecycle::{ArtifactAvailability, ArtifactGcReport, ArtifactRead};
use manifest::{FileHash, Manifest, context_hash};
pub(crate) use objects::validate_selection;
use objects::{selected_paths, set_executable};

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

    pub(crate) fn publish_owned(
        &self,
        key: &InvocationKey,
        completion: &NodeCompletion,
        context: &ArtifactFreezeContext,
    ) -> Result<CommitRef, GraphError> {
        self.freeze_snapshot(key, completion, context)
    }

    pub(crate) fn input_lease(&self) -> Result<anchor_file_lease::FileLease, GraphError> {
        self.store_lease(false)
    }

    pub(crate) fn visible_inputs(
        &self,
        commits: &[CommitRef],
    ) -> Result<Vec<CommitRef>, GraphError> {
        Ok(self
            .expanded_snapshots(commits, None)?
            .into_iter()
            .map(|(path, manifest)| CommitRef {
                id: path.file_name().unwrap().to_str().unwrap().to_owned(),
                node_id: manifest.key.node_id,
                invocation: manifest.key.invocation,
            })
            .collect())
    }

    pub(crate) fn reference(&self, id: &str) -> Result<Option<CommitRef>, GraphError> {
        if !valid_fs_id(id) {
            return Err(corrupt("invalid artifact reference"));
        }
        let path = self.root.join(id).join("manifest.json");
        match fs::symlink_metadata(&path) {
            Ok(_) => require_file(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(path)?).map_err(|error| corrupt(error.to_string()))?;
        Ok(Some(manifest_reference(id, &manifest)?))
    }

    fn published_commit(&self, key: &InvocationKey) -> Option<CommitRef> {
        let commit = commit_for(key);
        self.root.join(&commit.id).exists().then_some(commit)
    }

    fn freeze_with_workspace(
        &self,
        key: &InvocationKey,
        completion: &NodeCompletion,
        context: &ArtifactFreezeContext,
    ) -> Result<CommitRef, GraphError> {
        if self.published_commit(key).is_some()
            || !matches!(context.kind, ArtifactKind::Node | ArtifactKind::GraphCall)
        {
            return self.freeze_snapshot(key, completion, context);
        }
        self.prepare_workspace(key, &context.input_commits)?;
        let lease = self.workspaces().acquire(key)?;
        let commit = self.publish_owned(key, completion, context)?;
        lease.finish_published()?;
        Ok(commit)
    }

    pub(crate) fn input_mounts(
        &self,
        commits: &[CommitRef],
        run_id: &str,
        graph_digest: &str,
    ) -> Result<Vec<ReadOnlyInput>, GraphError> {
        let _lease = self.store_lease(false)?;
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
                self.artifact_view(&path, &manifest)?,
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

    fn call_result_staging(&self, key: &InvocationKey) -> Result<PathBuf, GraphError> {
        Ok(self
            .workspaces()
            .staging_directory(key)?
            .join(format!("call-{}", key_hash(key))))
    }

    fn ensure_export_complete(&self, key: &InvocationKey) -> Result<(), GraphError> {
        let pending = self.call_result_staging(key)?.join("pending.json");
        checked_path(&pending)?;
        if pending.exists() {
            return Err(corrupt(
                "result export was interrupted; refusing to publish a partial call result",
            ));
        }
        Ok(())
    }

    /// The caller must first authorize this fixed reference. The view is a
    /// rebuildable projection, never the authority for delivered bytes.
    pub(crate) fn files_path(&self, commit: &CommitRef) -> Result<PathBuf, GraphError> {
        let _lease = self.store_lease(false)?;
        let (path, manifest) = self.read_snapshot(commit)?;
        self.artifact_view(&path, &manifest)
    }

    /// Listing and result resolution read metadata only, including after expiry.
    pub(crate) fn list_files(&self, commit: &CommitRef) -> Result<Vec<(String, u64)>, GraphError> {
        let (_, manifest) = self.read_snapshot(commit)?;
        Ok(manifest
            .files
            .into_iter()
            .map(|(path, file)| (path, file.bytes))
            .collect())
    }

    #[cfg(test)]
    fn load_snapshot(&self, commit: &CommitRef) -> Result<(PathBuf, Manifest), GraphError> {
        self.read_snapshot(commit)
    }

    fn read_snapshot(&self, commit: &CommitRef) -> Result<(PathBuf, Manifest), GraphError> {
        anchor_runtime::artifact::store::read_snapshot(&self.root, commit)
    }

    fn freeze_snapshot(
        &self,
        key: &InvocationKey,
        completion: &NodeCompletion,
        context: &ArtifactFreezeContext,
    ) -> Result<CommitRef, GraphError> {
        validate_key(key)?;
        let _lease = self.store_lease(true)?;
        let commit = self
            .published_commit(key)
            .unwrap_or_else(|| commit_for(key));
        let published = self.root.join(&commit.id);
        // A published invocation is authoritative even if its old workspace or
        // input bytes changed or expired before Kernel accepted its result.
        if published.exists() {
            let (path, old) = self.read_snapshot(&commit)?;
            if old.key != *key
                || old.completion != *completion
                || old.context.as_ref() != Some(context)
            {
                return Err(GraphError::RunConflict);
            }
            self.artifact_view(&path, &old)?;
            return Ok(commit);
        }
        self.expanded_snapshots(
            &context.input_commits,
            Some((&key.run_id, &key.graph_digest)),
        )?;
        let (files, directories) = match context.kind {
            ArtifactKind::Node | ArtifactKind::GraphCall => {
                let selection =
                    ArtifactSelection::from_output(&completion.output).map_err(corrupt)?;
                if context.kind == ArtifactKind::GraphCall {
                    self.ensure_export_complete(key)?;
                }
                if selection.paths.is_empty() {
                    (BTreeMap::new(), vec![])
                } else {
                    let workspace = self.workspace_path(key)?;
                    let selected = selected_paths(&workspace, &selection)?;
                    let mut files = BTreeMap::new();
                    for (name, source) in selected.files {
                        files.insert(name, self.store_object(&source)?);
                    }
                    (files, selected.directories)
                }
            }
            ArtifactKind::Fanout | ArtifactKind::Join | ArtifactKind::Interruption => {
                let filename = manifest::control_filename(context.kind).unwrap();
                let bytes = serde_json::to_vec(&manifest::control_output(completion, context.kind))
                    .map_err(|error| corrupt(error.to_string()))?;
                (
                    BTreeMap::from([(filename.to_owned(), self.store_bytes(&bytes)?)]),
                    vec![],
                )
            }
        };
        let manifest = Manifest {
            format: 3,
            key: key.clone(),
            completion: completion.clone(),
            files,
            directories,
            context: Some(context.clone()),
            context_sha256: Some(context_hash(context)?),
        };
        manifest.validate_context()?;
        let temporary = self.root.join(format!(
            ".{}.{}.{}.tmp",
            commit.id,
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&temporary)?;
        let result = (|| {
            write_json_atomic(&temporary.join("manifest.json"), &manifest)?;
            fs::File::open(&temporary)?.sync_all()?;
            fs::rename(&temporary, &published)?;
            fs::File::open(&self.root)?.sync_all()?;
            self.artifact_view(&published, &manifest)?;
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
            self.freeze_with_workspace(
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
        Box::pin(async move { self.freeze_with_workspace(key, completion, context) })
    }

    fn resolve<'a>(
        &'a self,
        commit: &'a CommitRef,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let completion = self.read_snapshot(commit)?.1.completion;
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
                let bundle = self.call_inputs_path(child_run_id)?;
                if bundle.exists() || self.call_inputs_manifest_path(child_run_id)?.exists() {
                    return self.verify_call_inputs_bundle(
                        child_run_id,
                        &bundle,
                        Some(parents),
                        Some(selections),
                    );
                }
                return Ok(());
            }
            let _lease = self.store_lease(false)?;
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
                    let (source_root, manifest) = snapshots
                        .iter()
                        .find(|(_, manifest)| manifest.key.node_id == selection.node)
                        .ok_or_else(|| {
                            GraphError::InvalidSnapshot(format!(
                                "Op.call file source node `{}` is not a visible committed input",
                                selection.node
                            ))
                        })?;
                    self.require_not_expired(&CommitRef {
                        id: source_root
                            .file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned(),
                        node_id: manifest.key.node_id.clone(),
                        invocation: manifest.key.invocation,
                    })?;
                    let recorded = manifest
                        .files
                        .get(&selection.path)
                        .ok_or_else(|| corrupt("Op.call source is not a delivered file"))?;
                    let source = self.verified_object(recorded)?;
                    let destination = temporary.join(&selection.alias);
                    if let Some(parent) = destination.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::hard_link(&source, &destination)?;
                    let (sha256, bytes) = (recorded.sha256.clone(), recorded.bytes);
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
            validate_key(call_key)?;
            {
                let _lease = self.store_lease(false)?;
                if let Some(published) = self.published_commit(call_key) {
                    let (_, old) = self.read_snapshot(&published)?;
                    let requested = files
                        .iter()
                        .map(|name| format!("result/{name}"))
                        .collect::<std::collections::BTreeSet<_>>();
                    let delivered = old
                        .files
                        .keys()
                        .cloned()
                        .collect::<std::collections::BTreeSet<_>>();
                    let child =
                        serde_json::to_value(commit).map_err(|error| corrupt(error.to_string()))?;
                    let requested_files =
                        serde_json::to_value(files).map_err(|error| corrupt(error.to_string()))?;
                    if old
                        .context
                        .as_ref()
                        .is_none_or(|context| context.kind != ArtifactKind::GraphCall)
                        || old.completion.output.pointer("/result/commit") != Some(&child)
                        || old.completion.output.pointer("/result/files") != Some(&requested_files)
                        || requested != delivered
                    {
                        return Err(GraphError::RunConflict);
                    }
                    return Ok(files.to_vec());
                }
            }
            if files.is_empty() {
                return Ok(Vec::new());
            }
            let workspace = self.prepare_workspace(call_key, &[])?;
            // Dropping this process lock keeps the pending invocation owner.
            // Only publication (or an explicit interruption fact) releases it.
            let _workspace_lease = self.workspaces().acquire(call_key)?;
            let _lease = self.store_lease(false)?;
            let (_, manifest) = self.read_snapshot(commit)?;
            self.require_not_expired(commit)?;
            fs::create_dir_all(&workspace)?;
            checked_path(&workspace)?;
            let staging = self.call_result_staging(call_key)?;
            crate::host_fs::directory(&staging).map_err(corrupt)?;
            let request = CallResultExport {
                format: 1,
                child: commit.clone(),
                files: files.to_vec(),
            };
            let request_path = staging.join("request.json");
            if request_path.exists() {
                require_file(&request_path)?;
                let previous: CallResultExport = serde_json::from_slice(&fs::read(&request_path)?)
                    .map_err(|error| corrupt(error.to_string()))?;
                if previous != request {
                    return Err(GraphError::RunConflict);
                }
            } else {
                write_json_atomic(&request_path, &request)?;
            }
            let pending = staging.join("pending.json");
            write_json_atomic(&pending, &request)?;
            let temporary = staging.join("next");
            if temporary.exists() {
                require_directory(&temporary)?;
                fs::remove_dir_all(&temporary)?;
            }
            fs::create_dir(&temporary)?;
            let copied = (|| {
                let mut copied = Vec::with_capacity(files.len());
                for name in files {
                    validate_file_relative(name)?;
                    let recorded = manifest.files.get(name).ok_or_else(|| {
                        corrupt(format!("result file `{name}` is not in the child commit"))
                    })?;
                    let source = self.verified_object(recorded)?;
                    let (sha256, bytes) = copy_regular_file(&source, &temporary.join(name))?;
                    if sha256 != recorded.sha256 || bytes != recorded.bytes {
                        return Err(corrupt(format!(
                            "result file `{name}` changed after the child commit was verified"
                        )));
                    }
                    set_executable(&temporary.join(name), recorded.executable, false)?;
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
            // All staging and the pending marker live beside the Workspace,
            // in Host-owned state on the same filesystem. No project scan is
            // needed to detect an incomplete transfer or recover a swap.
            let target = workspace.join("result");
            let old = staging.join("old");
            if old.exists() {
                require_directory(&old)?;
                fs::remove_dir_all(&old)?;
            }
            if target.exists() {
                require_directory(&target)?;
                fs::rename(&target, &old)?;
            }
            fs::rename(&temporary, &target)?;
            fs::File::open(&workspace)?.sync_all()?;
            fs::File::open(&staging)?.sync_all()?;
            fs::remove_file(pending)?;
            if old.exists() {
                fs::remove_dir_all(old)?;
            }
            fs::File::open(staging)?.sync_all()?;
            Ok(copied)
        })
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallResultExport {
    format: u32,
    child: CommitRef,
    files: Vec<String>,
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
    let result = hash_copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(result)
}

fn hash_copy(mut input: impl Read, mut output: impl Write) -> Result<(String, u64), GraphError> {
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
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| corrupt("file size overflow"))?;
    }
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
            let (sha256, bytes) = hash_copy(fs::File::open(&path)?, std::io::sink())?;
            if files.insert(relative, (sha256, bytes)).is_some() {
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
