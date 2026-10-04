//! Durable per-Run data discovery and cleanup shared by single-Run deletion and
//! Graph cascade deletion.
//!
//! Artifacts and fact files are addressed by a SHA-256 of the invocation key,
//! not by a per-Run directory, so a Run's own durable record is the only
//! authority that can enumerate them. Cleanup is therefore idempotent and
//! tolerant of already-missing files: a partial delete can always be replayed
//! from the record.

use anchor_runtime_rig::graph::{GraphRunRecord, InvocationKey};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

fn key_hash(key: &InvocationKey) -> String {
    format!("{:x}", Sha256::digest(key.durable_key().as_bytes()))
}

/// Every invocation a Run may have persisted a fact, artifact, or io-harness
/// store for. Derived from the record so unrecorded crash leftovers are the only
/// things that can escape cleanup.
pub(crate) fn invocation_keys(record: &GraphRunRecord) -> Vec<InvocationKey> {
    let mut keys = Vec::new();
    for (node, count) in &record.invocations {
        for invocation in 1..=*count {
            keys.push(InvocationKey {
                run_id: record.run_id.clone(),
                graph_digest: record.graph_digest.clone(),
                node_id: node.clone(),
                invocation,
            });
        }
    }
    if let Some(cursor) = &record.cursor {
        keys.push(cursor.key.clone());
    }
    if let Some(parallel) = &record.parallel {
        for branch in &parallel.branches {
            if let Some(cursor) = &branch.cursor {
                keys.push(cursor.key.clone());
            }
        }
    }
    keys.sort_by_key(|key| key.durable_key());
    keys.dedup();
    keys
}

/// Frozen artifact locations for a Run, including legacy completion files.
/// Only ids that are a single safe path component are considered.
pub(crate) fn artifact_paths(record: &GraphRunRecord, artifacts_root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for result in record.results.values().flatten() {
        let id = result.commit.id.as_str();
        if safe_component(id) {
            paths.push(artifacts_root.join(id));
            paths.push(artifacts_root.join(format!("{id}.json")));
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn remove_io_recovery_intents(data_root: &Path, stem: &str) {
    let directory = data_root.join("io-harness").join("facts");
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let prefix = format!("{stem}.recovery-");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".json") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Delete every durable artifact, fact, io-harness store (including WAL/SHM and
/// recovery sidecars), and workspace owned by this Run. Missing files are fine.
pub(crate) fn delete_run_data(
    record: &GraphRunRecord,
    data_root: &Path,
    workspace_root: &Path,
) -> Result<(), String> {
    for key in invocation_keys(record) {
        let hash = key_hash(&key);
        let node_stem = format!("nf1-{hash}");
        let io_stem = format!("np1-{hash}");
        for suffix in ["json", "failed", "started"] {
            let _ = std::fs::remove_file(
                data_root
                    .join("facts")
                    .join(format!("{node_stem}.{suffix}")),
            );
            let _ = std::fs::remove_file(
                data_root
                    .join("io-harness")
                    .join("facts")
                    .join(format!("{io_stem}.{suffix}")),
            );
        }
        for suffix in [
            "run",
            "sqlite3",
            "sqlite3-wal",
            "sqlite3-shm",
            "sqlite3-journal",
        ] {
            let _ = std::fs::remove_file(
                data_root
                    .join("io-harness")
                    .join("store")
                    .join(format!("{io_stem}.{suffix}")),
            );
        }
        remove_io_recovery_intents(data_root, &io_stem);
    }
    for path in artifact_paths(record, &data_root.join("artifacts")) {
        if path.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|error| error.to_string())?;
        } else if path.exists() {
            std::fs::remove_file(&path).map_err(|error| error.to_string())?;
        }
    }
    if safe_component(&record.run_id) {
        let workspace = workspace_root.join(&record.run_id);
        if workspace.exists() {
            std::fs::remove_dir_all(&workspace).map_err(|error| error.to_string())?;
        }
        // Op.call stages its read-only `/in/call` bundle under the artifact
        // root; it belongs to this Run and goes with the rest of its data.
        let call_inputs = data_root
            .join("artifacts")
            .join("call-inputs")
            .join(&record.run_id);
        if call_inputs.exists() {
            std::fs::remove_dir_all(&call_inputs).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Remove the Run record and its Graph identity metadata. The Run record is the
/// last thing a caller should remove, so a crash mid-cleanup stays replayable.
pub(crate) fn remove_run_files(data_root: &Path, run_id: &str) -> Result<(), String> {
    if !safe_component(run_id) {
        return Err(format!("unsafe Run id `{run_id}`"));
    }
    let record = data_root.join("runs").join(format!("{run_id}.json"));
    if record.exists() {
        std::fs::remove_file(&record).map_err(|error| error.to_string())?;
    }
    let metadata = data_root
        .join("run-metadata")
        .join(format!("{run_id}.json"));
    if metadata.exists() {
        std::fs::remove_file(&metadata).map_err(|error| error.to_string())?;
    }
    Ok(())
}
