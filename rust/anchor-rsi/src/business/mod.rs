pub mod rsi;
pub mod weekly;

use crate::evidence::safe_path;
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
    safe_path(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
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

fn git(directory: &Path, arguments: &[&str]) -> Result<Vec<u8>, String> {
    safe_path(&directory.join(".git"))?;
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .args(["--no-replace-objects", "-c", "core.fsmonitor=false"])
        .arg(format!("--git-dir={}", directory.join(".git").display()))
        .args(arguments)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Git projection check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output.stdout)
}

pub fn git_head(directory: &Path) -> Result<String, String> {
    let head = String::from_utf8(git(directory, &["rev-parse", "HEAD"])?)
        .map_err(|error| error.to_string())?;
    let head = head.trim();
    if !hex(head, 40) {
        return Err("artifact Git HEAD must be a SHA-1 object".into());
    }
    Ok(head.into())
}

fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn verify_commit(directory: &Path, expected: &Value, node: &str) -> Result<(), String> {
    let head = git_head(directory)?;
    if let Some(expected) = expected.as_str() {
        return if head == expected {
            Ok(())
        } else {
            Err("joined commit differs from branch Git HEAD".into())
        };
    }
    let fields = expected
        .as_object()
        .ok_or("joined commit must be a CommitRef object")?;
    let identity = required(expected, "id")?;
    if fields.len() != 3
        || !identity
            .strip_prefix("fs1-")
            .or_else(|| identity.strip_prefix("fs2-"))
            .is_some_and(|hash| hex(hash, 64))
        || expected["node_id"] != node
        || expected["invocation"]
            .as_u64()
            .is_none_or(|number| number == 0)
    {
        return Err("invalid branch CommitRef identity".into());
    }
    let commit = String::from_utf8(git(directory, &["cat-file", "commit", "HEAD"])?)
        .map_err(|error| error.to_string())?;
    let message = commit.split_once("\n\n").ok_or("invalid Git commit")?.1;
    let prefix = format!(
        "Anchor Artifact {identity}\nArtifact-Node: {node}\nArtifact-Invocation: {}\nManifest-SHA256: ",
        expected["invocation"]
    );
    let manifest = message
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix('\n'))
        .filter(|hash| hex(hash, 64))
        .ok_or("Git projection is not bound to the branch artifact")?;
    for projection_path in [
        directory.join("projection.json"),
        directory
            .parent()
            .unwrap_or(directory)
            .join("projection.json"),
    ] {
        if projection_path.is_file() {
            let projection = object(&projection_path)?;
            if projection["format"] != 1
                || projection["artifact"] != *expected
                || projection["head"] != head
                || projection["manifest_sha256"] != manifest
            {
                return Err("Git projection identity differs from its artifact".into());
            }
            let inventory = projection["git_files"]
                .as_object()
                .ok_or("missing Git projection file inventory")?;
            for (name, hash) in inventory {
                if digest(&read(&relative(&directory.join(".git"), name)?)?)
                    != hash.as_str().ok_or("invalid Git file hash")?
                {
                    return Err("Git projection metadata differs from its inventory".into());
                }
            }
        }
    }
    for entry in git(directory, &["ls-tree", "-rz", "HEAD"])?
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = std::str::from_utf8(entry).map_err(|error| error.to_string())?;
        let (metadata, name) = entry.split_once('\t').ok_or("invalid Git tree entry")?;
        let parts = metadata.split(' ').collect::<Vec<_>>();
        if parts.len() != 3
            || parts[0] != "100644"
            || parts[1] != "blob"
            || !hex(parts[2], 40)
            || name == ".git"
            || name.starts_with(".git/")
        {
            return Err("unsafe artifact Git tree entry".into());
        }
        let path = relative(directory, name)?;
        read(&path)?;
        let observed = String::from_utf8(git(
            directory,
            &[
                "hash-object",
                "--no-filters",
                path.to_str().ok_or("non-UTF8 file path")?,
            ],
        )?)
        .map_err(|error| error.to_string())?;
        if observed.trim() != parts[2] {
            return Err("mounted artifact bytes differ from the Git projection".into());
        }
    }
    if !git(directory, &["fsck", "--full", "--strict", "--no-reflogs"])?.is_empty() {
        return Err("artifact Git projection contains invalid objects".into());
    }
    Ok(())
}

pub(crate) fn copy(source: &Path, destination: &Path) -> Result<(), String> {
    let bytes = read(source)?;
    safe_path(destination)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(destination, bytes).map_err(|error| error.to_string())
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
