//! Durable per-Run data discovery and cleanup shared by single-Run deletion and
//! Graph cascade deletion.
//!
//! Artifacts and fact files are addressed by a SHA-256 of the invocation key,
//! not by a per-Run directory, so a Run's own durable record is the only
//! authority that can enumerate them. Cleanup is therefore idempotent and
//! tolerant of already-missing files: a partial delete can always be replayed
//! from the record.

use anchor_runtime::graph::{GraphRunRecord, InvocationKey};
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

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn remove_directory_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn legacy_invocation_exists(
    data_root: &Path,
    key: &InvocationKey,
) -> Result<bool, String> {
    let stem = format!("np1-{}", key_hash(key));
    let root = data_root.join("io-harness");
    for (directory, suffixes) in [
        ("facts", &["json", "started", "failed", "model.json"][..]),
        (
            "store",
            &[
                "run",
                "sqlite3",
                "sqlite3-wal",
                "sqlite3-shm",
                "sqlite3-journal",
                "conversation.json",
                "recordings",
                "call-ids.json",
            ][..],
        ),
    ] {
        for suffix in suffixes {
            if root
                .join(directory)
                .join(format!("{stem}.{suffix}"))
                .try_exists()
                .map_err(|error| error.to_string())?
            {
                return Ok(true);
            }
        }
    }
    let entries = match std::fs::read_dir(root.join("facts")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let prefix = format!("{stem}.recovery-");
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".json") {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn remove_legacy_conversation(data_root: &Path, hint: &str) -> Result<(), String> {
    let root = data_root.join("io-harness/store");
    if !root.try_exists().map_err(|error| error.to_string())? {
        return Ok(());
    }
    let scope = format!("nc1-{:x}", Sha256::digest(hint.as_bytes()));
    let locks = root.join("conversation-locks");
    std::fs::create_dir_all(&locks).map_err(|error| error.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(locks.join(format!("{scope}.lock")))
        .map_err(|error| error.to_string())?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            format!("native conversation {scope} is already active")
        }
        std::fs::TryLockError::Error(error) => error.to_string(),
    })?;
    let mut stems = Vec::new();
    for entry in std::fs::read_dir(&root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".conversation.json") else {
            continue;
        };
        if !stem.starts_with("np1-") || !safe_component(stem) {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(entry.path()).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let saved_scope = value
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .ok_or("native conversation scope is missing")?;
        if !saved_scope.starts_with("nc1-")
            || saved_scope.len() != 68
            || !saved_scope[4..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("native conversation scope is not a key digest".into());
        }
        if saved_scope == scope {
            stems.push(stem.to_owned());
        }
    }
    for path in [
        root.join("conversations").join(&scope),
        root.join("conversation-roots").join(&scope),
    ] {
        remove_directory_if_exists(&path)?;
        if let Some(parent) = path.parent()
            && parent.exists()
        {
            std::fs::File::open(parent)
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
        }
    }
    for stem in stems {
        for suffix in ["conversation.json", "run", "call-ids.json"] {
            remove_file_if_exists(&root.join(format!("{stem}.{suffix}")))?;
        }
        remove_directory_if_exists(&root.join(format!("{stem}.recordings")))?;
    }
    std::fs::File::open(&root)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
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
        let store = data_root.join("io-harness/store");
        remove_directory_if_exists(&store.join(format!("{io_stem}.recordings")))?;
        remove_file_if_exists(&store.join(format!("{io_stem}.call-ids.json")))?;
        for runtime in ["goose-acp", "goose-acp-spike"] {
            for suffix in ["json", "tmp", "evidence.json"] {
                remove_file_if_exists(&data_root.join(runtime).join(format!("{hash}.{suffix}")))?;
            }
        }
        remove_directory_if_exists(&workspace_root.join(".goose-process").join(&hash))?;
        for suffix in ["json", "failed", "started", "model.json"] {
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
        crate::channel_inputs::remove(data_root, &record.run_id)?;
        let local_inputs = crate::local_inputs::fact_path(data_root, &record.run_id);
        match std::fs::remove_file(local_inputs) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.to_string()),
        }
        let channel_reply = data_root
            .join("channel-replies")
            .join(format!("{}.json", record.run_id));
        for path in [&channel_reply, &channel_reply.with_extension("tmp")] {
            match std::fs::remove_file(path) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.to_string()),
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_runtime::graph::GraphSnapshot;
    use serde_json::json;

    fn record(run_id: &str) -> GraphRunRecord {
        let snapshot = GraphSnapshot::admit(json!({
            "objective":"cleanup fixture", "entry":"agent",
            "agents":{"worker":{"model":"fixture"}},
            "nodes":[{"id":"agent", "agent":"worker"}], "edges":[],
        }))
        .unwrap();
        let mut record = GraphRunRecord::create_with_id(snapshot, json!({}), run_id).unwrap();
        record.invocations.insert("agent".into(), 1);
        record
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn cleanup_removes_owned_goose_and_legacy_files_but_not_other_runs() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let work = root.path().join("work");
        let target = record("target");
        let other = record("other");
        let mut target_paths = Vec::new();
        let mut other_paths = Vec::new();
        for (record, paths) in [(&target, &mut target_paths), (&other, &mut other_paths)] {
            let hash = key_hash(&invocation_keys(record)[0]);
            for runtime in ["goose-acp", "goose-acp-spike"] {
                for suffix in ["json", "tmp", "evidence.json"] {
                    paths.push(data.join(runtime).join(format!("{hash}.{suffix}")));
                }
            }
            paths.push(
                work.join(".goose-process")
                    .join(&hash)
                    .join("data/history.sqlite"),
            );
            paths.push(work.join(&record.run_id).join("agent/work.txt"));
            let store = data.join("io-harness/store");
            paths.push(store.join(format!("np1-{hash}.recordings/1/recording.json")));
            for suffix in [
                "call-ids.json",
                "run",
                "sqlite3",
                "sqlite3-wal",
                "sqlite3-shm",
                "sqlite3-journal",
            ] {
                paths.push(store.join(format!("np1-{hash}.{suffix}")));
            }
            for suffix in ["json", "started", "failed", "model.json", "recovery-3.json"] {
                paths.push(
                    data.join("io-harness/facts")
                        .join(format!("np1-{hash}.{suffix}")),
                );
            }
            for suffix in ["json", "started", "failed", "model.json"] {
                paths.push(data.join("facts").join(format!("nf1-{hash}.{suffix}")));
            }
            for path in paths.iter() {
                write(path, b"owned");
            }
        }
        delete_run_data(&target, &data, &work).unwrap();
        delete_run_data(&target, &data, &work).unwrap();
        assert!(target_paths.iter().all(|path| !path.exists()));
        assert!(
            other_paths
                .iter()
                .all(|path| std::fs::read(path).unwrap() == b"owned")
        );
    }

    #[test]
    fn conversation_cleanup_keeps_other_scopes_and_obeys_native_lock() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("io-harness/store");
        let target_scope = format!("nc1-{:x}", Sha256::digest(b"target"));
        let other_scope = format!("nc1-{:x}", Sha256::digest(b"other"));
        for (scope, stem) in [(&target_scope, "np1-target"), (&other_scope, "np1-other")] {
            write(
                &store.join(format!("{stem}.conversation.json")),
                &serde_json::to_vec(&json!({"scope":scope})).unwrap(),
            );
            write(
                &store
                    .join("conversations")
                    .join(scope)
                    .join("framework.sqlite3"),
                b"history",
            );
            write(
                &store
                    .join("conversation-roots")
                    .join(scope)
                    .join("file.txt"),
                b"work",
            );
            write(
                &store.join(format!("{stem}.recordings/1/recording.json")),
                b"recording",
            );
            for suffix in ["run", "call-ids.json"] {
                write(&store.join(format!("{stem}.{suffix}")), b"sidecar");
            }
        }
        let lock_path = store
            .join("conversation-locks")
            .join(format!("{target_scope}.lock"));
        write(&lock_path, b"");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        lock.try_lock().unwrap();
        assert!(
            remove_legacy_conversation(root.path(), "target")
                .unwrap_err()
                .contains("already active")
        );
        assert!(store.join("conversations").join(&target_scope).exists());
        drop(lock);
        remove_legacy_conversation(root.path(), "target").unwrap();
        remove_legacy_conversation(root.path(), "target").unwrap();
        assert!(!store.join("conversations").join(&target_scope).exists());
        assert!(
            !store
                .join("conversation-roots")
                .join(&target_scope)
                .exists()
        );
        for suffix in ["conversation.json", "run", "call-ids.json", "recordings"] {
            assert!(!store.join(format!("np1-target.{suffix}")).exists());
            assert!(store.join(format!("np1-other.{suffix}")).exists());
        }
        assert!(store.join("conversations").join(&other_scope).exists());
        assert!(store.join("conversation-roots").join(&other_scope).exists());
    }

    #[test]
    fn invalid_conversation_pointer_does_not_delete_owned_state() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("io-harness/store");
        let scope = format!("nc1-{:x}", Sha256::digest(b"target"));
        write(
            &store.join("np1-invalid.conversation.json"),
            b"{\"scope\":\"../outside\"}",
        );
        let history = store
            .join("conversations")
            .join(&scope)
            .join("framework.sqlite3");
        write(&history, b"preserve");
        assert!(remove_legacy_conversation(root.path(), "target").is_err());
        assert_eq!(std::fs::read(history).unwrap(), b"preserve");
    }

    #[test]
    fn legacy_detection_includes_unfinished_sidecars_and_never_goose_facts() {
        let root = tempfile::tempdir().unwrap();
        let key = invocation_keys(&record("target"))[0].clone();
        let hash = key_hash(&key);
        write(
            &root.path().join("goose-acp").join(format!("{hash}.json")),
            b"native",
        );
        assert!(!legacy_invocation_exists(root.path(), &key).unwrap());
        for relative in [
            format!("io-harness/facts/np1-{hash}.started"),
            format!("io-harness/facts/np1-{hash}.recovery-9.json"),
            format!("io-harness/store/np1-{hash}.conversation.json"),
            format!("io-harness/store/np1-{hash}.sqlite3-wal"),
        ] {
            let path = root.path().join(relative);
            write(&path, b"legacy");
            assert!(legacy_invocation_exists(root.path(), &key).unwrap());
            std::fs::remove_file(path).unwrap();
        }
        assert!(!legacy_invocation_exists(root.path(), &key).unwrap());
    }
}
