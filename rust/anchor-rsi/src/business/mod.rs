pub mod rsi;
pub mod weekly;

use crate::evidence::safe_path;
use anchor_runtime::{artifact::ArtifactDescriptor, graph::CommitRef};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

pub(crate) fn read(path: &Path) -> Result<Vec<u8>, String> {
    safe_path(path)?;
    let metadata = fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} must be a regular file", path.display()));
    }
    fs::read(path).map_err(|error| error.to_string())
}

pub(crate) fn text(path: &Path) -> Result<String, String> {
    String::from_utf8(read(path)?).map_err(|error| error.to_string())
}

pub(crate) fn object(path: &Path) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(&read(path)?).map_err(|error| error.to_string())?;
    if !value.is_object() {
        return Err(format!("{} must be an object", path.display()));
    }
    Ok(value)
}

pub(crate) fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    write(
        path,
        &serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    safe_path(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(path, bytes).map_err(|error| error.to_string())
}

pub(crate) fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| format!("missing concrete {key}"))
}

pub(crate) fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{key} must be an array"))
}

pub(crate) fn relative(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = Path::new(name);
    if name.is_empty()
        || name.contains('\\')
        || name.chars().any(char::is_control)
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || name.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(format!("unsafe relative path {name:?}"));
    }
    let path = root.join(path);
    safe_path(&path)?;
    Ok(path)
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn artifact_identity(directory: &Path) -> Result<Value, String> {
    Ok(ArtifactDescriptor::read(directory)?.identity())
}

pub fn verify_artifact(
    directory: &Path,
    expected: &Value,
    node: &str,
) -> Result<ArtifactDescriptor, String> {
    let expected: CommitRef =
        serde_json::from_value(expected.clone()).map_err(|error| error.to_string())?;
    let descriptor = ArtifactDescriptor::read(directory)?;
    if expected.node_id != node || descriptor.artifact != expected {
        return Err("joined Artifact identity differs from the mounted input".into());
    }
    Ok(descriptor)
}

pub(crate) fn artifact_read(directory: &Path, name: &str) -> Result<Vec<u8>, String> {
    ArtifactDescriptor::read(directory)?.read_file(directory, name)
}

pub(crate) fn artifact_text(directory: &Path, name: &str) -> Result<String, String> {
    String::from_utf8(artifact_read(directory, name)?).map_err(|error| error.to_string())
}

pub(crate) fn artifact_object(directory: &Path, name: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(&artifact_read(directory, name)?)
        .map_err(|error| error.to_string())?;
    if !value.is_object() {
        return Err(format!("{name} must be an object"));
    }
    Ok(value)
}

pub(crate) fn copy(source: &Path, destination: &Path) -> Result<(), String> {
    write(destination, &read(source)?)
}

pub(crate) fn copy_artifact(source: &Path, name: &str, destination: &Path) -> Result<(), String> {
    write(destination, &artifact_read(source, name)?)
}

pub fn route(target: &str, reason: &str) -> Result<(), String> {
    let reason = reason.chars().take(500).collect::<String>();
    let status = Command::new("anchor-route")
        .args(["--to", target, "--reason", &reason])
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("anchor-route rejected the gate decision".into())
    }
}
