//! Durable history is complete newline-terminated records; task costs are
//! derived from all of those records, independently of accepted versions.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anchor_kernel::*;
use serde_json::json;

const RUN: &str = "run/storage";

struct Allow;
impl Authority for Allow {
    fn permits(&self, _: &Principal, _: Action, _: &str) -> bool {
        true
    }
}

struct Control;
impl Validator for Control {
    fn judge(&self, _: &JudgeCtx, _: &Submission) -> Judgement {
        Judgement::refused(Reason::Unclassified)
    }
}

fn kernel(root: &Path) -> Kernel<FileStore> {
    let mut kernel = Kernel::new(FileStore::new(root).unwrap(), Box::new(Allow)).unwrap();
    kernel
        .install("validator/control", Arc::new(Control))
        .unwrap();
    kernel
}

fn resource() -> Resource {
    Resource::new(RUN, "validator/control", json!({})).task(100)
}

fn history_path(root: &Path) -> std::path::PathBuf {
    root.join("run%2Fstorage/history.jsonl")
}

fn record(seq: u64, cost: u64, verdict: Verdict, after: Option<Resource>) -> Record {
    // Stamp is host-only to construct through the public Rust API. Deserialize
    // a stored fact here to exercise the Store contract without a validator.
    serde_json::from_value(json!({
        "stamp": {
            "principal": {"id": "host", "kind": "code"},
            "cost": cost,
            "seq": seq,
            "knobs": "storage-regression"
        },
        "submission": {"target": RUN, "seen": 0, "action": "write"},
        "verdict": verdict,
        "after": after
    }))
    .unwrap()
}

fn rejected_record(seq: u64, cost: u64) -> Record {
    record(seq, cost, Verdict::returned(Reason::ContentInvalid), None)
}

#[test]
fn torn_tail_is_removed_before_append_and_all_complete_records_remain_readable() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path()).unwrap();
    store.create(&resource()).unwrap();
    let first = rejected_record(1, 2);
    let second = rejected_record(2, 3);
    store.commit(RUN, &first).unwrap();
    let path = history_path(dir.path());
    let complete_prefix = fs::read(&path).unwrap();
    let mut with_torn_tail = complete_prefix.clone();
    with_torn_tail.extend_from_slice(br#"{"stamp":{"princ"#);
    fs::write(&path, with_torn_tail).unwrap();

    store.commit(RUN, &second).unwrap();

    assert_eq!(store.history(RUN).unwrap(), vec![first, second.clone()]);
    let mut expected_bytes = complete_prefix;
    expected_bytes.extend(serde_json::to_vec(&second).unwrap());
    expected_bytes.push(b'\n');
    assert_eq!(fs::read(&path).unwrap(), expected_bytes);
}

/// Formal version of the preflight counterexample: stop survives an append
/// after a torn tail and a crash before its resource.json replacement.
#[test]
fn accepted_stop_after_torn_tail_recovers_on_restart_when_state_write_is_lost() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = kernel(dir.path());
    kernel.create(resource()).unwrap();
    let state_path = dir.path().join("run%2Fstorage/resource.json");
    let original_state = fs::read(&state_path).unwrap();
    fs::write(history_path(dir.path()), br#"{"torn":"#).unwrap();
    assert!(kernel.history(RUN).unwrap().is_empty());

    let owner = Principal::new("owner", PrincipalKind::Human);
    let stop = Submission::new(RUN, 0, Action::Request).content(json!({"control": "stop"}));
    assert!(kernel.submit(&owner, 7, stop).unwrap().is_accepted());
    drop(kernel);
    fs::write(&state_path, original_state).unwrap();

    let restarted = self::kernel(dir.path());
    let restored = restarted.load(RUN).unwrap().unwrap();
    assert_eq!(restored.version, 1);
    assert!(matches!(
        restored.phase(),
        Some(Phase::Waiting {
            waiter: Waiter::Human { .. }
        })
    ));
    assert_eq!(restored.task.unwrap().budget.spent, 7);
    assert_eq!(restarted.history(RUN).unwrap().len(), 1);
}

#[test]
fn malformed_complete_records_are_corrupt_in_middle_and_at_end() {
    // Empty and invalid UTF-8 newline-complete lines are corrupt, too.
    for malformed in [b"{broken}\n".as_slice(), b"\n", b"\xff\n"] {
        for has_following_record in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let store = FileStore::new(dir.path()).unwrap();
            store.create(&resource()).unwrap();
            store.commit(RUN, &rejected_record(1, 0)).unwrap();
            let path = history_path(dir.path());
            let mut bytes = fs::read(&path).unwrap();
            bytes.extend_from_slice(malformed);
            if has_following_record {
                bytes.extend(serde_json::to_vec(&rejected_record(2, 0)).unwrap());
                bytes.push(b'\n');
            }
            fs::write(&path, bytes).unwrap();

            assert!(matches!(store.history(RUN), Err(StoreError::Corrupt(_))));
            assert!(matches!(store.load(RUN), Err(StoreError::Corrupt(_))));
        }
    }
}

#[test]
fn valid_json_without_terminating_newline_is_not_a_record_and_is_trimmed_on_append() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path()).unwrap();
    store.create(&resource()).unwrap();
    let path = history_path(dir.path());
    let incomplete = rejected_record(1, 99);
    fs::write(&path, serde_json::to_vec(&incomplete).unwrap()).unwrap();

    assert!(store.history(RUN).unwrap().is_empty());
    assert_eq!(
        store.load(RUN).unwrap().unwrap().task.unwrap().budget.spent,
        0
    );
    let complete = rejected_record(2, 3);
    store.commit(RUN, &complete).unwrap();
    assert_eq!(store.history(RUN).unwrap(), vec![complete]);
    assert_eq!(
        store.load(RUN).unwrap().unwrap().task.unwrap().budget.spent,
        3
    );
}

#[test]
fn existing_complete_record_bytes_are_preserved_including_json_whitespace() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path()).unwrap();
    store.create(&resource()).unwrap();
    let first = rejected_record(1, 2);
    let mut complete_prefix = b"  ".to_vec();
    complete_prefix.extend(serde_json::to_vec(&first).unwrap());
    complete_prefix.extend_from_slice(b" \r\n");
    let path = history_path(dir.path());
    fs::write(&path, &complete_prefix).unwrap();

    store.commit(RUN, &rejected_record(2, 3)).unwrap();

    assert!(fs::read(&path).unwrap().starts_with(&complete_prefix));
    assert_eq!(store.history(RUN).unwrap().len(), 2);
}

fn assert_costs_derive_from_all_verdicts(store: &dyn Store) {
    store.create(&resource()).unwrap();
    let mut after = resource();
    after.version = 1;
    after.task.as_mut().unwrap().budget.spent = 999; // Not authoritative.
    let verdicts = [
        Verdict::returned(Reason::ContentInvalid),
        Verdict::refused(Reason::Unauthorized),
        Verdict::returned(Reason::StaleVersion),
        Verdict {
            effect: Effect::Pending,
            verification: Verification::NotApplicable,
            reason: Reason::AwaitingHuman,
        },
        Verdict {
            effect: Effect::Accepted,
            verification: Verification::Mechanical,
            reason: Reason::Ok,
        },
    ];
    for (index, verdict) in verdicts.into_iter().enumerate() {
        let after = verdict.is_accepted().then(|| after.clone());
        store
            .commit(RUN, &record(index as u64 + 1, 2, verdict, after))
            .unwrap();
        let loaded = store.load(RUN).unwrap().unwrap();
        assert_eq!(loaded.version, u64::from(verdict.is_accepted()));
        assert_eq!(loaded.task.unwrap().budget.spent, (index as u64 + 1) * 2);
    }
    // A later versionless refusal still contributes after an accepted record.
    store.commit(RUN, &rejected_record(6, 3)).unwrap();
    let loaded = store.load(RUN).unwrap().unwrap();
    assert_eq!(loaded.version, 1);
    assert_eq!(loaded.task.unwrap().budget.spent, 13);
}

#[test]
fn memory_store_derives_spent_from_every_record_without_advancing_versions() {
    assert_costs_derive_from_all_verdicts(&MemoryStore::default());
}

#[test]
fn file_store_derives_spent_from_every_record_including_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    assert_costs_derive_from_all_verdicts(&FileStore::new(dir.path()).unwrap());
    let restarted = FileStore::new(dir.path()).unwrap();
    let restored = restarted.load(RUN).unwrap().unwrap();
    assert_eq!(restored.version, 1);
    assert_eq!(restored.task.unwrap().budget.spent, 13);
}

fn assert_costs_saturate(store: &dyn Store) {
    store.create(&resource()).unwrap();
    store.commit(RUN, &rejected_record(1, u64::MAX)).unwrap();
    store.commit(RUN, &rejected_record(2, 1)).unwrap();
    let restored = store.load(RUN).unwrap().unwrap();
    assert_eq!(restored.version, 0);
    assert_eq!(restored.task.unwrap().budget.spent, u64::MAX);
}

#[test]
fn store_cost_sum_saturates_instead_of_overflowing() {
    assert_costs_saturate(&MemoryStore::default());
    let dir = tempfile::tempdir().unwrap();
    assert_costs_saturate(&FileStore::new(dir.path()).unwrap());
}

#[test]
fn borrowed_record_projection_refreshes_after_own_append_and_external_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path()).unwrap();
    store.create(&resource()).unwrap();
    let project = || {
        store
            .with_record_entries(RUN, |entries| {
                entries
                    .iter()
                    .map(|entry| {
                        entry.submission.content["recovery"]
                            .as_u64()
                            .ok_or("missing recovery fact")
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
            .unwrap()
    };
    assert!(project().is_empty());

    let mut appended = rejected_record(1, 2);
    appended.submission.content = json!({"recovery": 11});
    store.commit(RUN, &appended).unwrap();
    assert_eq!(project(), vec![11]);

    let mut replacement = rejected_record(2, 7);
    replacement.submission.content = json!({"recovery": 22});
    let path = history_path(dir.path());
    let mut bytes = serde_json::to_vec(&replacement).unwrap();
    bytes.push(b'\n');
    let tmp = path.with_extension("replacement");
    fs::write(&tmp, bytes).unwrap();
    fs::rename(&tmp, &path).unwrap();

    assert_eq!(project(), vec![22]);
    assert_eq!(
        store.record_entries(RUN).unwrap(),
        vec![RecordEntry::from(replacement)]
    );
}

#[test]
fn cached_facts_follow_other_writers_replacement_truncation_and_same_length_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path()).unwrap();
    store.create(&resource()).unwrap();
    store.commit(RUN, &rejected_record(1, 2)).unwrap();
    assert_eq!(store.history_len(RUN).unwrap(), 1);
    assert_eq!(store.record_entries(RUN).unwrap()[0].stamp.cost(), 2);

    let other = FileStore::new(dir.path()).unwrap();
    other.commit(RUN, &rejected_record(2, 3)).unwrap();
    assert_eq!(store.history_len(RUN).unwrap(), 2);
    assert_eq!(
        store.load(RUN).unwrap().unwrap().task.unwrap().budget.spent,
        5
    );

    let path = history_path(dir.path());
    let mut replacement = serde_json::to_vec(&rejected_record(3, 7)).unwrap();
    replacement.push(b'\n');
    let tmp = path.with_extension("replacement");
    fs::write(&tmp, &replacement).unwrap();
    fs::rename(&tmp, &path).unwrap();
    assert_eq!(store.history_len(RUN).unwrap(), 1);
    assert_eq!(
        store.load(RUN).unwrap().unwrap().task.unwrap().budget.spent,
        7
    );

    replacement[0] = b'!';
    fs::write(&path, replacement).unwrap();
    assert!(matches!(
        store.record_entries(RUN),
        Err(StoreError::Corrupt(_))
    ));
    assert!(matches!(store.load(RUN), Err(StoreError::Corrupt(_))));
    fs::write(&path, []).unwrap();
    assert_eq!(store.history_len(RUN).unwrap(), 0);
    assert_eq!(
        store.load(RUN).unwrap().unwrap().task.unwrap().budget.spent,
        0
    );
}

#[test]
fn cached_load_recovers_lost_state_without_trusting_snapshot_cost_or_counting_a_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path()).unwrap();
    let original = resource();
    store.create(&original).unwrap();
    let mut after = original.clone();
    after.version = 1;
    after.state = json!({"changed": true});
    after.task.as_mut().unwrap().budget.spent = 999;
    let accepted = Verdict {
        effect: Effect::Accepted,
        verification: Verification::Mechanical,
        reason: Reason::Ok,
    };
    store
        .commit(RUN, &record(1, 2, accepted, Some(after)))
        .unwrap();
    store.commit(RUN, &rejected_record(2, 3)).unwrap();
    fs::write(
        dir.path().join("run%2Fstorage/resource.json"),
        serde_json::to_vec(&original).unwrap(),
    )
    .unwrap();
    let mut bytes = fs::read(history_path(dir.path())).unwrap();
    bytes.extend(serde_json::to_vec(&rejected_record(3, 99)).unwrap());
    fs::write(history_path(dir.path()), bytes).unwrap();
    for reader in [&store, &FileStore::new(dir.path()).unwrap()] {
        let restored = reader.load(RUN).unwrap().unwrap();
        assert_eq!(restored.version, 1);
        assert_eq!(restored.state, json!({"changed": true}));
        assert_eq!(restored.task.unwrap().budget.spent, 5);
        assert_eq!(reader.history_len(RUN).unwrap(), 2);
        let entries: Vec<_> = reader
            .history(RUN)
            .unwrap()
            .into_iter()
            .map(RecordEntry::from)
            .collect();
        assert_eq!(reader.record_entries(RUN).unwrap(), entries);
    }
    store.commit(RUN, &rejected_record(3, 4)).unwrap();
    assert_eq!(store.history_len(RUN).unwrap(), 3);
    assert_eq!(
        store.load(RUN).unwrap().unwrap().task.unwrap().budget.spent,
        9
    );
}

#[test]
fn locally_appended_facts_obey_the_same_decoder_limits_as_cold_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path()).unwrap();
    store.create(&resource()).unwrap();
    let mut record = rejected_record(1, 0);
    let mut content = json!(null);
    for _ in 0..140 {
        content = json!([content]);
    }
    record.submission.content = content;
    let encoded = serde_json::to_vec(&record).unwrap();
    // A discarded partial tail can have exactly the appended line's length.
    fs::write(history_path(dir.path()), vec![b'x'; encoded.len() + 1]).unwrap();
    assert_eq!(store.history_len(RUN).unwrap(), 0);
    store.commit(RUN, &record).unwrap();
    for reader in [&store, &FileStore::new(dir.path()).unwrap()] {
        assert!(matches!(reader.history(RUN), Err(StoreError::Corrupt(_))));
        assert!(matches!(reader.load(RUN), Err(StoreError::Corrupt(_))));
        assert!(matches!(
            reader.record_entries(RUN),
            Err(StoreError::Corrupt(_))
        ));
    }
}
