//! Physical checkpoints are disposable read optimizations, never new facts.
use anchor_kernel::*;
use serde_json::json;
use std::{fs, io::Write, path::Path};

const ID: &str = "run/cursor";

fn initial() -> Resource {
    Resource::new(ID, "validator/cursor", json!({"payload":"initial"})).task(1_000_000)
}

fn record(version: u64) -> Record {
    let mut after = initial();
    after.version = version;
    after.state = json!({"payload":format!("version-{version}")});
    serde_json::from_value(json!({
        "stamp":{"principal":{"id":"host","kind":"code"},"cost":3},
        "submission":{"target":ID,"seen":version-1,"action":"write"},
        "verdict":{"effect":"accepted","verification":"mechanical","reason":"ok"},
        "after":after
    }))
    .unwrap()
}

fn store(root: &Path, count: u64) -> FileStore {
    let store = FileStore::new(root).unwrap();
    store.create(&initial()).unwrap();
    for version in 1..=count {
        store.commit(ID, &record(version)).unwrap();
    }
    store
}

fn history(root: &Path) -> std::path::PathBuf {
    root.join("run%2Fcursor/history.jsonl")
}

#[test]
fn serialized_checkpoint_restarts_at_the_physical_suffix_and_recovers_the_bounded_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path(), 30);
    let reader = HistoryReader::open(root.path()).unwrap();
    let first = reader.history_since(ID, None).unwrap();
    assert_eq!(first.records.len(), 30);
    assert_eq!(first.metrics.records_parsed, 30);
    assert_eq!(
        first.cursor.offset(),
        fs::metadata(history(root.path())).unwrap().len()
    );
    let cursor: HistoryCursor =
        serde_json::from_slice(&serde_json::to_vec(&first.cursor).unwrap()).unwrap();
    drop(reader);
    let reader = HistoryReader::open(root.path()).unwrap();
    let before = fs::metadata(history(root.path())).unwrap().len();
    store.commit(ID, &record(31)).unwrap();
    let next = reader.history_since(ID, Some(&cursor)).unwrap();
    assert_eq!(next.records.len(), 1);
    assert_eq!(next.cursor.place(), 31);
    assert_eq!(next.metrics.records_parsed, 1);
    assert_eq!(
        next.metrics.history_bytes_read,
        next.cursor.offset() - before
    );
    assert_eq!(next.resource.version, 31);
    assert_eq!(next.resource.task.as_ref().unwrap().budget.spent, 93);
    assert_eq!(next.metrics.snapshot_records_parsed, 0);
    // A crash after the history fsync but before replacing resource.json must
    // still recover the retained snapshot without scanning older records.
    fs::write(
        root.path().join("run%2Fcursor/resource.json"),
        serde_json::to_vec(&initial()).unwrap(),
    )
    .unwrap();
    let idle = reader.history_since(ID, Some(&next.cursor)).unwrap();
    assert!(idle.records.is_empty());
    assert_eq!(idle.metrics.history_bytes_read, 0);
    assert_eq!(idle.metrics.records_parsed, 0);
    assert_eq!(idle.metrics.snapshot_records_parsed, 1);
    assert!(idle.metrics.anchor_bytes_read <= 8192);
    assert_eq!(idle.resource, next.resource);
    assert_eq!(idle.cursor, next.cursor);
}

#[test]
fn only_newline_complete_records_advance_and_malformed_complete_lines_never_disappear() {
    let root = tempfile::tempdir().unwrap();
    let _store = store(root.path(), 1);
    let reader = HistoryReader::open(root.path()).unwrap();
    let first = reader.history_since(ID, None).unwrap();
    let bytes = serde_json::to_vec(&record(2)).unwrap();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(history(root.path()))
        .unwrap();
    file.write_all(&bytes).unwrap();
    let partial = reader.history_since(ID, Some(&first.cursor)).unwrap();
    assert!(partial.records.is_empty());
    assert_eq!(partial.cursor, first.cursor);
    assert_eq!(partial.metrics.history_bytes_read, bytes.len() as u64);
    file.write_all(b"\n").unwrap();
    let complete = reader.history_since(ID, Some(&partial.cursor)).unwrap();
    assert_eq!(complete.records, vec![record(2)]);
    assert_eq!(complete.cursor.place(), 2);
    file.write_all(b"{invalid}\n").unwrap();
    assert!(matches!(
        reader.history_since(ID, Some(&complete.cursor)),
        Err(StoreError::Corrupt(_))
    ));
    let replay = reader
        .history_range(ID, None, Some(&complete.cursor))
        .unwrap();
    assert_eq!(replay.cursor, complete.cursor);
    assert_eq!(replay.resource, complete.resource);
    assert_eq!(replay.records, vec![record(1), record(2)]);
}

#[test]
fn cursors_never_silently_reset_after_truncation_replacement_boundary_edits_or_removal() {
    for scenario in [
        "truncate", "replace", "boundary", "remove", "resource", "root", "forged",
    ] {
        let root = tempfile::tempdir().unwrap();
        let _store = store(root.path(), 2);
        let reader = HistoryReader::open(root.path()).unwrap();
        let mut first = reader.history_since(ID, None).unwrap();
        let path = history(root.path());
        match scenario {
            "truncate" => fs::write(&path, []).unwrap(),
            "replace" => {
                let bytes = fs::read(&path).unwrap();
                fs::rename(&path, path.with_extension("old")).unwrap();
                fs::write(&path, bytes).unwrap();
            }
            "boundary" => {
                let mut bytes = fs::read(&path).unwrap();
                let last = bytes.len() - 2;
                bytes[last] ^= 1;
                fs::write(&path, bytes).unwrap();
            }
            "remove" => fs::remove_file(&path).unwrap(),
            "resource" => fs::rename(path.parent().unwrap(), root.path().join("detached")).unwrap(),
            "root" => {
                let other = tempfile::tempdir().unwrap();
                let _store = store(other.path(), 2);
                first.cursor = HistoryReader::open(other.path())
                    .unwrap()
                    .history_since(ID, None)
                    .unwrap()
                    .cursor;
            }
            "forged" => {
                let mut cursor = serde_json::to_value(&first.cursor).unwrap();
                cursor["place"] = json!(1000);
                first.cursor = serde_json::from_value(cursor).unwrap();
                assert!(reader.history_range(ID, None, Some(&first.cursor)).is_err());
                continue;
            }
            _ => unreachable!(),
        }
        assert!(
            reader.history_since(ID, Some(&first.cursor)).is_err(),
            "{scenario}"
        );
    }
}

#[test]
fn strict_replay_checks_the_entire_prefix_not_only_the_incremental_anchor() {
    let root = tempfile::tempdir().unwrap();
    let _store = store(root.path(), 40);
    let reader = HistoryReader::open(root.path()).unwrap();
    let frozen = reader.history_since(ID, None).unwrap();
    assert!(frozen.cursor.offset() > 8192);
    let path = history(root.path());
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] = b'!';
    fs::write(path, bytes).unwrap();
    assert!(matches!(
        reader.history_range(ID, None, Some(&frozen.cursor)),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn a_pre_history_empty_checkpoint_can_advance_when_the_writer_creates_the_log() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path(), 0);
    let reader = HistoryReader::open(root.path()).unwrap();
    let empty = reader.history_since(ID, None).unwrap();
    assert_eq!(empty.cursor.place(), 0);
    assert_eq!(empty.resource, initial());
    store.commit(ID, &record(1)).unwrap();
    assert_eq!(
        reader
            .history_since(ID, Some(&empty.cursor))
            .unwrap()
            .records
            .len(),
        1
    );
}
