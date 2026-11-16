//! Run in its own process so descriptor and read counters cover this fixture.
#![cfg(target_os = "linux")]

use anchor_kernel::*;
use serde_json::json;
use std::{fs, path::Path};

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn watches() -> usize {
    fs::read_dir("/proc/self/fd")
        .unwrap()
        .flatten()
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .filter(|path| path == Path::new("anon_inode:inotify"))
        .count()
}

fn read_bytes() -> u64 {
    fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("rchar: ")?.trim().parse().ok())
        .unwrap()
}

fn add(store: &FileStore, id: &str, count: usize) {
    store
        .create(&Resource::new(id, "validator/test", json!({})))
        .unwrap();
    let record = Record {
        stamp: serde_json::from_value(json!({
            "principal": {"id": "host", "kind": "code"},
            "cost": 1
        }))
        .unwrap(),
        submission: Submission::new(id, 0, Action::Write)
            .content(json!({"fixture": "x".repeat(1024)})),
        verdict: Verdict::returned(Reason::ContentInvalid),
        after: None,
    };
    for _ in 0..count {
        store.commit(id, &record).unwrap();
    }
}

#[test]
fn many_stores_share_one_instance_and_validate_all_histories() {
    let _test = TEST_LOCK.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let baseline = watches();
    let hot = FileStore::new(root.path().join("hot")).unwrap();
    add(&hot, "run/hot", 300);
    let mut stores = Vec::new();
    for index in 0..64 {
        let store = FileStore::new(root.path().join(index.to_string())).unwrap();
        add(&store, "run/old", 1);
        stores.push(store);
        assert!(watches() <= baseline + 1);
        assert_eq!(hot.history_len("run/hot").unwrap(), 300);
    }
    assert_eq!(watches(), baseline + 1);
    let before = read_bytes();
    for _ in 0..20 {
        assert_eq!(hot.history_len("run/hot").unwrap(), 300);
    }
    // A cold scan reads over 300 KiB; a warm history_len only opens/stats it.
    assert!(read_bytes() - before < 8192, "recent history was rescanned");

    let path = root.path().join("0/run%2Fold/history.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] = b'!';
    fs::write(&path, bytes).unwrap();
    assert!(matches!(
        stores[0].history_len("run/old"),
        Err(StoreError::Corrupt(_))
    ));
    assert!(watches() <= baseline + 1);
    drop(stores);
    drop(hot);
    assert_eq!(watches(), baseline);
}

#[test]
fn repeated_working_set_larger_than_sixteen_does_not_rescan_histories() {
    let _test = TEST_LOCK.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = FileStore::new(root.path()).unwrap();
    for index in 0..262 {
        add(&store, &format!("run/{index}"), 100);
    }
    // Startup and periodic scheduler/list passes rotate across many runs;
    // repeatedly touching one hot run does not represent this workload.
    for index in 0..262 {
        assert_eq!(store.history_len(&format!("run/{index}")).unwrap(), 100);
    }
    let before = read_bytes();
    for _ in 0..4 {
        for index in 0..262 {
            assert_eq!(store.history_len(&format!("run/{index}")).unwrap(), 100);
        }
    }
    let bytes = read_bytes() - before;
    eprintln!("262 histories x 4 warm scans: {bytes} read bytes");
    assert!(bytes < 8192, "working set was reread: {bytes} bytes");
}
