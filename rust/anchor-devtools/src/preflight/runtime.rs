use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use rustix::fs::Access;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{GOOSE_VERSION, absolute_path, accessible, elf, metadata, reject_symlinks, required};

fn platform() -> Value {
    json!({"architecture": "X86_64", "bits": 64, "little_endian": true})
}

fn packaged_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.is_empty()
        || relative
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
        || Path::new(relative)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("runtime manifest contains an unsafe path".into());
    }
    let path = root.join(relative);
    let metadata = metadata(&path, "runtime file")?;
    if !metadata.is_file() {
        return Err("runtime file must be a regular file".into());
    }
    if ![0, rustix::process::geteuid().as_raw()].contains(&metadata.uid())
        || metadata.mode() & 0o7022 != 0
    {
        return Err(
            "runtime file must be root- or service-owned and not group- or world-writable".into(),
        );
    }
    Ok(path)
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|_| "runtime file is unreadable")?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65_536];
    loop {
        let length = file
            .read(&mut buffer)
            .map_err(|_| "runtime file is unreadable")?;
        if length == 0 {
            return Ok(format!("{:x}", digest.finalize()));
        }
        digest.update(&buffer[..length]);
    }
}

pub(super) fn check(env: &BTreeMap<String, String>, goose_sha256: &str) -> Result<PathBuf, String> {
    let root = absolute_path(
        required(env, "ANCHOR_DEPLOYMENT_ROOT")?,
        "ANCHOR_DEPLOYMENT_ROOT",
    )?;
    let root_metadata = metadata(&root, "ANCHOR_DEPLOYMENT_ROOT")?;
    if !root_metadata.is_dir() || root_metadata.mode() & 0o022 != 0 {
        return Err("ANCHOR_DEPLOYMENT_ROOT must be a non-writable runtime directory".into());
    }
    let root = fs::canonicalize(root).map_err(|_| "ANCHOR_DEPLOYMENT_ROOT is unavailable")?;
    let manifest_path = packaged_file(&root, "runtime-manifest.json")?;
    let manifest: Value = serde_json::from_slice(
        &fs::read(manifest_path).map_err(|_| "runtime-manifest.json is missing or invalid")?,
    )
    .map_err(|_| "runtime-manifest.json is missing or invalid")?;
    if !manifest.is_object() || manifest.get("format").and_then(Value::as_u64) != Some(1) {
        return Err("runtime-manifest.json must use format 1".into());
    }
    if manifest.get("goose_version").and_then(Value::as_str) != Some(GOOSE_VERSION) {
        return Err(format!(
            "runtime must contain pinned Goose v{GOOSE_VERSION}"
        ));
    }
    if manifest.get("goose_sha256").and_then(Value::as_str) != Some(goose_sha256) {
        return Err("runtime-manifest Goose digest does not match the fixed v1.53.0 pin".into());
    }
    if manifest.get("platform") != Some(&platform()) {
        return Err("runtime platform must be 64-bit little-endian x86_64".into());
    }
    let files = manifest
        .get("files")
        .and_then(Value::as_array)
        .ok_or("runtime-manifest.json has no file inventory")?;
    let mut file_entries = BTreeMap::new();
    for entry in files {
        let relative = entry
            .get("path")
            .and_then(Value::as_str)
            .ok_or("runtime-manifest file inventory is invalid")?;
        if file_entries.insert(relative, entry).is_some() {
            return Err("runtime-manifest contains duplicate file paths".into());
        }
        let path = packaged_file(&root, relative)?;
        let metadata = fs::metadata(&path).map_err(|_| "runtime file is unavailable")?;
        if entry.get("size").and_then(Value::as_u64) != Some(metadata.len())
            || entry.get("sha256").and_then(Value::as_str) != Some(sha256(&path)?.as_str())
        {
            return Err("runtime file digest or size mismatch".into());
        }
        if entry.get("mode").and_then(Value::as_u64) != Some(u64::from(metadata.mode() & 0o7777)) {
            return Err("runtime file mode does not match its inventory".into());
        }
    }
    for required_file in [
        "bin/anchor-runner-host",
        "bin/goose",
        "bundle/graph.json",
        "bundle/manifest.json",
        "web/index.html",
    ] {
        if !file_entries.contains_key(required_file) {
            return Err(format!("runtime package is missing {required_file}"));
        }
    }
    if file_entries["bin/goose"]
        .get("sha256")
        .and_then(Value::as_str)
        != Some(goose_sha256)
    {
        return Err("packaged Goose file digest does not match the fixed v1.53.0 pin".into());
    }
    let executables = manifest
        .get("executables")
        .and_then(Value::as_array)
        .ok_or("runtime-manifest executable inventory is invalid")?;
    let mut identities = BTreeSet::new();
    for identity in executables {
        let relative = identity
            .get("path")
            .and_then(Value::as_str)
            .ok_or("runtime manifest executable inventory is invalid")?;
        if !identities.insert(relative) {
            return Err("runtime manifest contains duplicate executable paths".into());
        }
        let entry = file_entries
            .get(relative)
            .ok_or("runtime executable is missing from the file inventory")?;
        if identity.get("sha256").and_then(Value::as_str).is_none()
            || identity.get("sha256") != entry.get("sha256")
        {
            return Err("executable identity does not match the runtime file inventory".into());
        }
        if identity.get("elf") != Some(&platform()) {
            return Err("runtime executable platform does not match Goose".into());
        }
        let path = packaged_file(&root, relative)?;
        if !accessible(&path, Access::EXEC_OK) {
            return Err("runtime executable is not executable".into());
        }
        let declared: elf::Runtime =
            serde_json::from_value(identity.get("runtime").cloned().unwrap_or(Value::Null))
                .map_err(|_| "runtime ELF runtime metadata is missing or invalid")?;
        if declared.needed.iter().any(String::is_empty) {
            return Err("runtime ELF runtime metadata is missing or invalid".into());
        }
        if relative == "bin/goose"
            && (declared.interpreter.is_some() || !declared.needed.is_empty())
        {
            return Err("pinned Goose must be the packaged static musl executable".into());
        }
        if elf::inspect(&path)? != declared {
            return Err("runtime ELF runtime metadata does not match the executable".into());
        }
        if let Some(interpreter) = &declared.interpreter {
            let path = Path::new(interpreter);
            if !path.is_absolute() || !path.is_file() || !accessible(path, Access::EXEC_OK) {
                return Err("runtime ELF interpreter from runtime manifest is unavailable".into());
            }
        }
    }
    if !identities.contains("bin/anchor-runner-host") || !identities.contains("bin/goose") {
        return Err("runtime manifest must describe both Host and Goose executables".into());
    }
    for (name, packaged) in [
        ("ANCHOR_GOOSE_BINARY", "bin/goose"),
        ("ANCHOR_RUNNER_BUNDLE_ROOT", "bundle"),
        ("ANCHOR_RUNNER_WEB_ROOT", "web"),
    ] {
        let path = absolute_path(required(env, name)?, name)?;
        reject_symlinks(&path, name)?;
        if fs::canonicalize(path).map_err(|_| format!("{name} is unavailable"))?
            != root.join(packaged)
        {
            return Err(format!(
                "{name} must point into the selected source-free runtime"
            ));
        }
    }
    let host_binary = absolute_path(
        required(env, "ANCHOR_RUNNER_HOST_BINARY")?,
        "ANCHOR_RUNNER_HOST_BINARY",
    )?;
    if fs::canonicalize(host_binary).map_err(|_| "ANCHOR_RUNNER_HOST_BINARY is unavailable")?
        != root.join("bin/anchor-runner-host")
    {
        return Err("ANCHOR_RUNNER_HOST_BINARY must resolve to the selected runtime Host".into());
    }
    if required(env, "ANCHOR_GOOSE_BINARY_SHA256")? != goose_sha256 {
        return Err("ANCHOR_GOOSE_BINARY_SHA256 must equal the fixed Goose v1.53.0 pin".into());
    }
    Ok(root)
}
