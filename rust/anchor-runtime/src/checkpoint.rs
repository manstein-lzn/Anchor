//! Durable checkpoint storage ports and adapters.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{AgentCheckpoint, CheckpointError};

/// Storage boundary for one serialized AgentNode checkpoint.
pub trait CheckpointStore: Send + Sync {
    /// Atomically replace the checkpoint for `key`.
    fn save(&self, key: &str, checkpoint: &AgentCheckpoint) -> Result<(), CheckpointStoreError>;
    /// Load and identity-check a checkpoint. Missing checkpoints are `None`.
    fn load(
        &self,
        key: &str,
        expected_node: &str,
        expected_invocation: u32,
    ) -> Result<Option<AgentCheckpoint>, CheckpointStoreError>;
    /// Remove a checkpoint after its owning run reaches a durable terminal state.
    fn delete(&self, key: &str) -> Result<(), CheckpointStoreError>;
}

/// File-backed adapter for a standalone host or a platform-owned state root.
/// It stores only the versioned checkpoint envelope; provider clients, secrets,
/// tool handlers and host permissions remain outside the file.
#[derive(Debug, Clone)]
pub struct FileCheckpointStore {
    root: PathBuf,
}

impl FileCheckpointStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, key: &str) -> Result<PathBuf, CheckpointStoreError> {
        validate_key(key)?;
        Ok(self.root.join(format!("{key}.json")))
    }

    fn temporary_path(&self, key: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        self.root
            .join(format!(".{key}.{}.{}.tmp", std::process::id(), stamp))
    }
}

impl CheckpointStore for FileCheckpointStore {
    fn save(&self, key: &str, checkpoint: &AgentCheckpoint) -> Result<(), CheckpointStoreError> {
        let path = self.path(key)?;
        fs::create_dir_all(&self.root)?;
        let temporary = self.temporary_path(key);
        let bytes = checkpoint.encode()?;
        fs::write(&temporary, bytes)?;
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(())
    }

    fn load(
        &self,
        key: &str,
        expected_node: &str,
        expected_invocation: u32,
    ) -> Result<Option<AgentCheckpoint>, CheckpointStoreError> {
        let path = self.path(key)?;
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(AgentCheckpoint::decode(
            &bytes,
            expected_node,
            expected_invocation,
        )?))
    }

    fn delete(&self, key: &str) -> Result<(), CheckpointStoreError> {
        let path = self.path(key)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn validate_key(key: &str) -> Result<(), CheckpointStoreError> {
    if key.is_empty()
        || key == "."
        || key == ".."
        || key.chars().any(|character| {
            !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
        })
    {
        return Err(CheckpointStoreError::InvalidKey(key.to_owned()));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CheckpointStoreError {
    #[error("checkpoint key `{0}` is not a safe file name")]
    InvalidKey(String),
    #[error("checkpoint I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("checkpoint encoding failed: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("checkpoint could not be decoded: {0}")]
    Decode(#[from] CheckpointError),
}
