//! Explicit file delivery and the Host-owned read-only view consumed by nodes.

use crate::graph::CommitRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

pub mod store;

pub const ARTIFACT_DESCRIPTOR_FILE: &str = ".anchor-artifact.json";

pub fn is_delivery_path(value: &str) -> bool {
    !value.trim().is_empty()
        && !value.chars().any(char::is_control)
        && !value.contains(['\\', '*', '?', '[', ']'])
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".." | ARTIFACT_DESCRIPTOR_FILE))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSelection {
    pub paths: Vec<String>,
}

impl ArtifactSelection {
    pub fn new(paths: Vec<String>) -> Result<Self, String> {
        if let Some(path) = paths.iter().find(|path| !is_delivery_path(path)) {
            return Err(format!("invalid delivery path {path:?}"));
        }
        let mut paths = paths;
        paths.sort();
        paths.dedup();
        Ok(Self { paths })
    }

    pub fn from_output(output: &Value) -> Result<Self, String> {
        let paths = output
            .get("files")
            .and_then(Value::as_array)
            .ok_or("completion requires files: an explicit list, or [] for text only")?
            .iter()
            .map(|path| {
                path.as_str()
                    .map(str::to_owned)
                    .ok_or("files must contain paths")
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(paths)
    }

    pub fn covers(&self, required: &[String]) -> Result<(), String> {
        let missing: Vec<_> = required
            .iter()
            .filter(|path| {
                !self.paths.iter().any(|selected| {
                    *path == selected
                        || path
                            .strip_prefix(selected)
                            .is_some_and(|tail| tail.starts_with('/'))
                })
            })
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "delivery does not include declared outputs: {missing:?}"
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactFile {
    pub sha256: String,
    pub bytes: u64,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDescriptor {
    pub format: u32,
    pub artifact: CommitRef,
    pub manifest_sha256: String,
    pub files: BTreeMap<String, ArtifactFile>,
    pub directories: Vec<String>,
}

impl ArtifactDescriptor {
    pub fn read(root: &Path) -> Result<Self, String> {
        let path = root.join(ARTIFACT_DESCRIPTOR_FILE);
        store::require_file(&path).map_err(|error| error.to_string())?;
        let descriptor: Self =
            serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        if descriptor.format != 1 {
            return Err("unsupported artifact view format".into());
        }
        Ok(descriptor)
    }

    pub fn read_file(&self, root: &Path, relative: &str) -> Result<Vec<u8>, String> {
        if !is_delivery_path(relative) {
            return Err("invalid artifact file path".into());
        }
        let expected = self
            .files
            .get(relative)
            .ok_or("file is not in this delivery")?;
        let path = root.join(relative);
        store::require_file(&path).map_err(|error| error.to_string())?;
        if std::fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .len()
            != expected.bytes
        {
            return Err("artifact file differs from its delivery".into());
        }
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        if bytes.len() as u64 != expected.bytes
            || format!("{:x}", Sha256::digest(&bytes)) != expected.sha256
        {
            return Err("artifact file differs from its delivery".into());
        }
        Ok(bytes)
    }

    pub fn identity(&self) -> Value {
        serde_json::json!({"artifact":self.artifact,"manifest_sha256":self.manifest_sha256})
    }
}
