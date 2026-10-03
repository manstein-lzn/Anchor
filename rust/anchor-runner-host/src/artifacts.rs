//! Host-owned, immutable file snapshots behind the shared ArtifactPort.

use anchor_runtime_rig::{
    ReadOnlyInput,
    graph::{
        ArtifactFreezeContext, ArtifactKind, ArtifactPort, CommitRef, GraphError, InvocationKey,
        NodeCompletion,
    },
};
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

mod links;
mod manifest;
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
            mounts.push(ReadOnlyInput::new(path.join("files"), destination));
        }
        Ok(mounts)
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
        let workspace = if context.kind == ArtifactKind::Node {
            let workspace = self.workspace_path(key)?;
            require_directory(&workspace)?;
            Some(workspace)
        } else {
            None
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
                scan_tree(workspace, Some(&temporary.join("files")))?
            } else {
                let files_dir = temporary.join("files");
                fs::create_dir(&files_dir)?;
                let files = if context.kind != ArtifactKind::GraphCall {
                    let filename = if context.kind == ArtifactKind::Fanout {
                        "fanout.json"
                    } else {
                        "join.json"
                    };
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
    scan_directory(root, root, destination, &mut files, &mut directories)?;
    directories.sort();
    Ok((files, directories))
}
fn scan_directory(
    root: &Path,
    current: &Path,
    destination: Option<&Path>,
    files: &mut BTreeMap<String, FileHash>,
    directories: &mut Vec<String>,
) -> Result<(), GraphError> {
    if let Some(destination) = destination {
        fs::create_dir(destination)?;
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
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
            scan_directory(root, &path, next.as_deref(), files, directories)?;
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
