//! Shared immutable Artifact manifests and bounded reads under the Host store lease.

use super::{ArtifactDescriptor, ArtifactFile as FileHash};
use crate::graph::{
    ArtifactFreezeContext, ArtifactKind, CommitRef, GraphError, InvocationKey, NodeCompletion,
};
use anchor_file_lease::{FileLease, FlockOperation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub key: InvocationKey,
    pub completion: NodeCompletion,
    pub files: BTreeMap<String, FileHash>,
    pub directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ArtifactFreezeContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_sha256: Option<String>,
}

pub fn context_hash(context: &ArtifactFreezeContext) -> Result<String, GraphError> {
    let bytes = serde_json::to_vec(context).map_err(|error| corrupt(error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn control_output(completion: &NodeCompletion, kind: ArtifactKind) -> Value {
    let mut output = completion.output.clone();
    if kind == ArtifactKind::Join {
        // Kernel has no filesystem inventory. Its historical empty `files`
        // placeholders must not be presented as an authoritative file list.
        if let Some(branches) = output.get_mut("branches").and_then(Value::as_array_mut) {
            for branch in branches {
                if let Some(nodes) = branch.get_mut("nodes").and_then(Value::as_array_mut) {
                    for node in nodes {
                        if let Some(fields) = node.as_object_mut() {
                            fields.remove("files");
                        }
                    }
                }
            }
        }
    }
    output
}

pub fn control_filename(kind: ArtifactKind) -> Option<&'static str> {
    match kind {
        ArtifactKind::Fanout => Some("fanout.json"),
        ArtifactKind::Join => Some("join.json"),
        ArtifactKind::Interruption => Some("interruption.json"),
        ArtifactKind::Node | ArtifactKind::GraphCall => None,
    }
}

impl Manifest {
    pub fn validate_context(&self) -> Result<(), GraphError> {
        for (path, file) in &self.files {
            validate_file_relative(path)?;
            if !valid_sha256(&file.sha256) {
                return Err(corrupt("invalid artifact file metadata"));
            }
            if Path::new(path).ancestors().skip(1).any(|ancestor| {
                ancestor
                    .to_str()
                    .is_some_and(|ancestor| self.files.contains_key(ancestor))
            }) {
                return Err(corrupt("artifact file paths overlap"));
            }
        }
        let mut previous: Option<&str> = None;
        for directory in &self.directories {
            validate_file_relative(directory)?;
            if previous.is_some_and(|previous| previous >= directory.as_str())
                || Path::new(directory).ancestors().any(|ancestor| {
                    ancestor
                        .to_str()
                        .is_some_and(|ancestor| self.files.contains_key(ancestor))
                })
            {
                return Err(corrupt("invalid artifact directory metadata"));
            }
            previous = Some(directory);
        }
        match (self.format, &self.context, &self.context_sha256) {
            (3, Some(context), Some(hash)) if context_hash(context)? == *hash => {
                let mut ids = std::collections::BTreeSet::new();
                for input in &context.input_commits {
                    validate_relative(&input.node_id)?;
                    if !valid_fs_id(&input.id) || input.invocation == 0 || !ids.insert(&input.id) {
                        return Err(corrupt("invalid or duplicate artifact parent reference"));
                    }
                }
                match context.kind {
                    ArtifactKind::Node => Ok(()),
                    // Only the explicitly exported child result selection is
                    // published for a Graph call.
                    ArtifactKind::GraphCall => Ok(()),
                    ArtifactKind::Fanout | ArtifactKind::Join | ArtifactKind::Interruption => {
                        let filename = control_filename(context.kind).unwrap();
                        if self.files.len() != 1
                            || !self.files.contains_key(filename)
                            || !self.directories.is_empty()
                        {
                            return Err(corrupt("control artifact contains unexpected files"));
                        }
                        let bytes =
                            serde_json::to_vec(&control_output(&self.completion, context.kind))
                                .map_err(|error| corrupt(error.to_string()))?;
                        let expected = FileHash {
                            sha256: format!("{:x}", Sha256::digest(&bytes)),
                            bytes: bytes.len() as u64,
                            executable: false,
                        };
                        if self.files[filename] != expected {
                            return Err(corrupt(
                                "control file does not match Coordinator completion",
                            ));
                        }
                        Ok(())
                    }
                }
            }
            _ => Err(corrupt("artifact context, checksum or format mismatch")),
        }
    }
}

/// The existing lowercase SHA-256 filename stem, without each caller's prefix.
pub fn invocation_stem(key: &InvocationKey) -> String {
    format!("{:x}", Sha256::digest(key.durable_key().as_bytes()))
}

pub fn validate_component(value: &str) -> Result<(), GraphError> {
    if !crate::identifier::is_name(value) {
        return Err(GraphError::CorruptRun(
            "unsafe artifact identity component".into(),
        ));
    }
    Ok(())
}

pub fn validate_relative(value: &str) -> Result<(), GraphError> {
    if value.is_empty() || value.contains('\\') || value.split('/').any(|part| part.is_empty()) {
        return Err(GraphError::CorruptRun(
            "unsafe artifact relative path".into(),
        ));
    }
    for part in value.split('/') {
        validate_component(part)?;
    }
    Ok(())
}

pub fn validate_key(key: &InvocationKey) -> Result<(), GraphError> {
    validate_component(&key.run_id)?;
    validate_component(&key.graph_digest)?;
    validate_relative(&key.node_id)?;
    if key.invocation == 0 {
        return Err(GraphError::CorruptRun(
            "artifact invocation must be positive".into(),
        ));
    }
    Ok(())
}

pub fn corrupt(message: impl Into<String>) -> GraphError {
    GraphError::CorruptRun(message.into())
}
pub fn manifest_reference(id: &str, manifest: &Manifest) -> Result<CommitRef, GraphError> {
    validate_key(&manifest.key)?;
    manifest.validate_context()?;
    let expected = commit_for(&manifest.key);
    if expected.id != id {
        return Err(corrupt("artifact identity mismatch"));
    }
    Ok(expected)
}
pub fn commit_for(key: &InvocationKey) -> CommitRef {
    CommitRef {
        id: format!("fs2-{}", invocation_stem(key)),
        node_id: key.node_id.clone(),
        invocation: key.invocation,
    }
}
pub fn valid_fs_id(id: &str) -> bool {
    id.strip_prefix("fs2-").is_some_and(valid_sha256)
}
pub fn validate_file_relative(value: &str) -> Result<(), GraphError> {
    if !super::is_delivery_path(value) {
        return Err(corrupt("unsafe artifact file path"));
    }
    Ok(())
}
pub fn checked_path(path: &Path) -> Result<(), GraphError> {
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
pub fn require_directory(path: &Path) -> Result<(), GraphError> {
    checked_path(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(corrupt("artifact directory required"));
    }
    Ok(())
}
pub fn require_file(path: &Path) -> Result<(), GraphError> {
    checked_path(path)?;
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(corrupt("artifact regular file required"));
    }
    Ok(())
}

pub fn read_snapshot(root: &Path, commit: &CommitRef) -> Result<(PathBuf, Manifest), GraphError> {
    if !valid_fs_id(&commit.id) {
        return Err(GraphError::Unsupported(
            "artifact has no published file manifest".into(),
        ));
    }
    validate_relative(&commit.node_id)?;
    let path = root.join(&commit.id);
    require_directory(&path)?;
    let manifest_path = path.join("manifest.json");
    require_file(&manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)
        .map_err(|error| corrupt(error.to_string()))?;
    if manifest_reference(&commit.id, &manifest)? != *commit {
        return Err(corrupt("artifact identity mismatch"));
    }
    Ok((path, manifest))
}

pub fn require_not_expired(snapshot: &Path) -> Result<(), GraphError> {
    let expired = snapshot.join("expired.json");
    if expired.exists() {
        require_file(&expired)?;
        return Err(GraphError::Unsupported(
            "artifact delivery has expired".into(),
        ));
    }
    Ok(())
}

pub fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

impl Manifest {
    pub fn descriptor(&self, snapshot: &Path) -> Result<ArtifactDescriptor, GraphError> {
        Ok(ArtifactDescriptor {
            format: 1,
            artifact: commit_for(&self.key),
            manifest_sha256: format!(
                "{:x}",
                Sha256::digest(fs::read(snapshot.join("manifest.json"))?)
            ),
            files: self.files.clone(),
            directories: self.directories.clone(),
        })
    }
}

/// Finite readers share the publication/collection lock without creating or
/// modifying anything, including when the store is mounted read-only.
pub struct ArtifactStoreReader<'a> {
    root: &'a Path,
    lease: FileLease,
}

impl<'a> ArtifactStoreReader<'a> {
    pub fn open(root: &'a Path) -> Result<Self, GraphError> {
        let path = root.join(".store.lock");
        require_file(&path)?;
        let lease = FileLease::acquire(fs::File::open(path)?, FlockOperation::LockShared)
            .map_err(std::io::Error::from)?;
        Ok(Self { root, lease })
    }

    pub fn read_view(&self, commit: &CommitRef) -> Result<ArtifactView<'_>, GraphError> {
        let (snapshot, manifest) = read_snapshot(self.root, commit)?;
        require_not_expired(&snapshot)?;
        let root = snapshot.join("view");
        let descriptor = manifest.descriptor(&snapshot)?;
        if ArtifactDescriptor::read(&root).map_err(corrupt)? != descriptor {
            return Err(corrupt("artifact view descriptor changed"));
        }
        Ok(ArtifactView {
            root,
            descriptor,
            _lease: &self.lease,
        })
    }
}

pub struct ArtifactView<'a> {
    pub descriptor: ArtifactDescriptor,
    root: PathBuf,
    _lease: &'a FileLease,
}

impl ArtifactView<'_> {
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        self.descriptor.read_file(&self.root, path)
    }
}
