use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use rusqlite::Connection;
use serde_json::{Value, json};

use super::{filesystem, run};

struct Fixture {
    temporary: tempfile::TempDir,
    legacy: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let legacy = temporary.path().join("legacy");
        fs::create_dir_all(legacy.join("workspaces/graph/runs/run-1")).unwrap();
        fs::create_dir_all(legacy.join("sessions/session-1")).unwrap();
        fs::create_dir_all(legacy.join("state")).unwrap();
        fs::write(legacy.join("workspaces/graph/graph.json"), "{}").unwrap();
        write_json(
            &legacy.join("workspaces/graph/runs/run-1/run.json"),
            &json!({
                "status": "finished", "cursor": null, "active": {}, "parallel": null,
            }),
        );
        write_json(
            &legacy.join("sessions/session-1/session.json"),
            &json!({
                "id": "session-1", "status": "active", "run_ids": ["run-1"],
            }),
        );
        let database = Connection::open(legacy.join("state/pilot-turns.sqlite")).unwrap();
        database.execute_batch("CREATE TABLE turns (id TEXT, status TEXT); INSERT INTO turns VALUES ('turn-1', 'completed');").unwrap();
        drop(database);
        Self { temporary, legacy }
    }

    fn root(&self) -> &Path {
        self.temporary.path()
    }

    fn arguments(&self, extra: &[&str]) -> Vec<String> {
        let mut arguments = vec![
            "--legacy-root".into(),
            self.legacy.to_str().unwrap().into(),
            "--rust-state-root".into(),
            self.root().join("rust-state").to_str().unwrap().into(),
            "--rust-workspace-root".into(),
            self.root().join("rust-work").to_str().unwrap().into(),
            "--rust-catalog-root".into(),
            self.root().join("rust-catalog").to_str().unwrap().into(),
        ];
        arguments.extend(extra.iter().map(|value| (*value).to_owned()));
        arguments
    }

    fn attested(&self, extra: &[&str]) -> Vec<String> {
        let mut arguments = self.arguments(&[
            "--legacy-writer-stopped",
            "--legacy-read-only-confirmed",
            "--credentials-reviewed",
        ]);
        arguments.extend(extra.iter().map(|value| (*value).to_owned()));
        arguments
    }

    fn report(&self) -> Value {
        run(&self.attested(&[])).unwrap()
    }

    fn prepare(&self, output: &Path) -> Value {
        run(&self.attested(&["--prepare", "--output-dir", output.to_str().unwrap()])).unwrap()
    }

    fn state_database(&self) -> Connection {
        fs::create_dir_all(self.root().join("rust-state/platform")).unwrap();
        let database =
            Connection::open(self.root().join("rust-state/platform/sessions.sqlite")).unwrap();
        database
            .execute_batch("CREATE TABLE sessions (id TEXT); CREATE TABLE turns (id TEXT);")
            .unwrap();
        database
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fn writable(path: &Path) {
            if let Ok(metadata) = fs::symlink_metadata(path) {
                if metadata.file_type().is_symlink() {
                    return;
                }
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
                if metadata.is_dir()
                    && let Ok(children) = fs::read_dir(path)
                {
                    for child in children.flatten() {
                        writable(&child.path());
                    }
                }
            }
        }
        writable(self.root());
    }
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn issues_contain(report: &Value, fragment: &str) -> bool {
    report["inventory_issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue.as_str().unwrap().contains(fragment))
}

fn blockers_contain(report: &Value, fragment: &str) -> bool {
    report["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue.as_str().unwrap().contains(fragment))
}

#[test]
fn argument_errors_are_clear_and_do_not_inventory_default_roots() {
    assert!(run(&[]).unwrap_err().contains("--rust-state-root"));
    assert!(run(&["--help".into()]).unwrap_err().contains("--help"));
    let fixture = Fixture::new();
    assert!(
        run(&fixture.arguments(&["--prepare"]))
            .unwrap_err()
            .contains("--output-dir")
    );
    assert!(
        run(&fixture.arguments(&["--credential-path"]))
            .unwrap_err()
            .contains("missing path")
    );
    assert!(
        run(&fixture.arguments(&["--no-such-flag"]))
            .unwrap_err()
            .contains("unknown")
    );
    assert!(
        run(&fixture.arguments(&["--prepare=true"]))
            .unwrap_err()
            .contains("does not take a value")
    );
}

#[test]
fn default_inventory_is_read_only_and_requires_attestations() {
    let fixture = Fixture::new();
    let before = fs::read(fixture.legacy.join("state/pilot-turns.sqlite")).unwrap();
    let mut issues = BTreeSet::new();
    let snapshot = filesystem::Snapshot::inspect(&fixture.legacy, "legacy", &mut issues);
    let report = run(&fixture.arguments(&[])).unwrap();
    assert_eq!(report["decision"], "blocked");
    assert!(blockers_contain(&report, "service stop"));
    assert!(blockers_contain(&report, "root is writable"));
    assert!(report["inventory"]["file_count"].as_u64().unwrap() >= 4);
    assert_eq!(report["inventory"]["legacy_turn_ids"], json!(["turn-1"]));
    assert_eq!(
        fs::read(fixture.legacy.join("state/pilot-turns.sqlite")).unwrap(),
        before
    );
    assert!(!fixture.root().join("rust-state").exists());
    assert!(!fixture.root().join("rust-work").exists());
    snapshot.verify().unwrap();
}

#[test]
fn ready_inventory_retains_original_report_and_rollback_contract() {
    let fixture = Fixture::new();
    let report = fixture.report();
    assert_eq!(report["decision"], "legacy_read_only");
    assert_eq!(report["inventory_issues"], json!([]));
    assert_eq!(report["inventory"]["runs"][0]["id"], "run-1");
    assert_eq!(report["inventory"]["sessions"][0]["id"], "session-1");
    assert_eq!(
        report["policy"]["data_migration"],
        "not_performed; inspect/import only in a separately authorized operation"
    );
    assert_eq!(
        report["policy"]["rust_library_root"],
        report["policy"]["rust_catalog_root"]
    );
    assert_eq!(
        report["rollback_preconditions"].as_array().unwrap().len(),
        5
    );
}

#[test]
fn missing_legacy_root_blocks_even_with_prepare_and_attestations() {
    let fixture = Fixture::new();
    let missing = fixture.root().join("missing");
    let output = fixture.root().join("output");
    let report = run(&fixture.attested(&[
        "--legacy-root",
        missing.to_str().unwrap(),
        "--prepare",
        "--output-dir",
        output.to_str().unwrap(),
    ]))
    .unwrap();
    assert_eq!(report["decision"], "blocked");
    assert!(issues_contain(&report, "missing root"));
    assert!(!output.exists());
}

#[test]
fn empty_legacy_root_is_only_a_migration_candidate() {
    let fixture = Fixture::new();
    let empty = fixture.root().join("empty");
    fs::create_dir(&empty).unwrap();
    let report = run(&fixture.arguments(&[
        "--legacy-root",
        empty.to_str().unwrap(),
        "--legacy-read-only-confirmed",
    ]))
    .unwrap();
    assert_eq!(report["decision"], "migration_candidate");
    assert_eq!(report["inventory"]["entry_count"], 0);
    assert_eq!(report["policy"]["legacy_writer_stopped_confirmed"], true);
    assert!(!fixture.root().join("rust-state").exists());
}

#[test]
fn unfinished_or_unreadable_runs_block_and_leave_records_unchanged() {
    let fixture = Fixture::new();
    let run_file = fixture.legacy.join("workspaces/graph/runs/run-1/run.json");
    for record in [
        json!({"status": "running", "cursor": {"node": "n"}}),
        json!({"status": "paused"}),
        json!({"status": "finished", "active": {"n": {}}}),
        json!({"status": "finished", "parallel": {"entry": "n"}}),
        json!({"status": "finished", "cursor": {"node": "n"}}),
        json!({"status": 2}),
        json!({"status": "finished", "id": "different"}),
        json!([]),
    ] {
        write_json(&run_file, &record);
        let before = fs::read(&run_file).unwrap();
        let report = fixture.report();
        assert_eq!(report["decision"], "blocked", "{record}");
        assert_eq!(report["unfinished_runs"][0]["id"], "run-1");
        assert_eq!(fs::read(&run_file).unwrap(), before);
    }
    fs::write(&run_file, "{broken").unwrap();
    assert!(issues_contain(&fixture.report(), "invalid JSON"));
}

#[test]
fn all_known_terminal_run_statuses_are_quiescent() {
    let fixture = Fixture::new();
    for status in ["finished", "failed", "stopped"] {
        write_json(
            &fixture.legacy.join("workspaces/graph/runs/run-1/run.json"),
            &json!({"status": status}),
        );
        let report = fixture.report();
        assert_eq!(report["decision"], "legacy_read_only", "{status}: {report}");
    }
}

#[test]
fn pending_admissions_and_orphan_run_directories_block() {
    let fixture = Fixture::new();
    let pending = fixture.legacy.join("workspaces/graph/runs/pending");
    fs::create_dir(&pending).unwrap();
    write_json(&pending.join("admission.json"), &json!({}));
    let report = fixture.report();
    assert_eq!(report["decision"], "blocked");
    assert!(
        report["unfinished_runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["status"] == "pending_admission")
    );
    fs::create_dir(fixture.legacy.join("workspaces/graph/runs/orphan")).unwrap();
    assert!(
        fixture.report()["unfinished_runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["id"] == "orphan")
    );
}

#[test]
fn duplicate_legacy_run_ids_are_reported() {
    let fixture = Fixture::new();
    let second = fixture.legacy.join("workspaces/other/runs/run-1");
    fs::create_dir_all(&second).unwrap();
    write_json(&second.join("run.json"), &json!({"status": "finished"}));
    let report = fixture.report();
    assert_eq!(report["decision"], "blocked");
    assert_eq!(
        report["id_conflicts"]["legacy_duplicate_run_ids"]["run-1"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn malformed_session_identity_status_and_run_references_fail_closed() {
    let fixture = Fixture::new();
    let session_file = fixture.legacy.join("sessions/session-1/session.json");
    for record in [
        json!({"id": "wrong", "status": "active"}),
        json!({"id": "session-1"}),
        json!({"id": "session-1", "status": "unknown"}),
        json!({"id": "session-1", "status": "active", "run_ids": "wrong"}),
        json!({"id": "session-1", "status": "active", "run_ids": [1]}),
    ] {
        write_json(&session_file, &record);
        assert_eq!(fixture.report()["decision"], "blocked", "{record}");
    }
}

#[test]
fn running_turn_and_all_rust_id_collisions_are_detected() {
    let fixture = Fixture::new();
    let database = Connection::open(fixture.legacy.join("state/pilot-turns.sqlite")).unwrap();
    database
        .execute("UPDATE turns SET status='running'", [])
        .unwrap();
    drop(database);
    fs::create_dir_all(fixture.root().join("rust-state/runs")).unwrap();
    fs::write(fixture.root().join("rust-state/runs/run-1.json"), "{}").unwrap();
    let database = fixture.state_database();
    database
        .execute_batch(
            "INSERT INTO sessions VALUES ('session-1'); INSERT INTO turns VALUES ('turn-1');",
        )
        .unwrap();
    drop(database);
    let report = fixture.report();
    assert_eq!(report["decision"], "blocked");
    assert_eq!(report["running_turn_ids"], json!(["turn-1"]));
    assert_eq!(report["id_conflicts"]["rust_run_ids"], json!(["run-1"]));
    assert_eq!(
        report["id_conflicts"]["rust_session_ids"],
        json!(["session-1"])
    );
    assert_eq!(report["id_conflicts"]["rust_turn_ids"], json!(["turn-1"]));
}

#[test]
fn referenced_run_ids_collide_with_rust_metadata_even_without_legacy_run() {
    let fixture = Fixture::new();
    write_json(
        &fixture.legacy.join("sessions/session-1/session.json"),
        &json!({"id": "session-1", "status": "active", "run_ids": ["run-only-in-session"]}),
    );
    fs::create_dir_all(fixture.root().join("rust-state/run-metadata")).unwrap();
    fs::write(
        fixture
            .root()
            .join("rust-state/run-metadata/run-only-in-session.json"),
        "{}",
    )
    .unwrap();
    assert_eq!(
        fixture.report()["id_conflicts"]["rust_run_ids"],
        json!(["run-only-in-session"])
    );
}

#[test]
fn sqlite_sidecars_fail_closed_for_legacy_and_rust_including_dangling_links() {
    for rust in [false, true] {
        for suffix in ["-wal", "-shm", "-journal"] {
            for linked in [false, true] {
                let fixture = Fixture::new();
                let database = if rust {
                    drop(fixture.state_database());
                    fixture.root().join("rust-state/platform/sessions.sqlite")
                } else {
                    fixture.legacy.join("state/pilot-turns.sqlite")
                };
                let sidecar = PathBuf::from(format!("{}{suffix}", database.display()));
                if linked {
                    symlink(fixture.root().join("absent"), &sidecar).unwrap();
                } else {
                    fs::write(&sidecar, "unfinished").unwrap();
                }
                let report = fixture.report();
                assert_eq!(report["decision"], "blocked");
                assert!(issues_contain(&report, "sidecar present"));
            }
        }
    }
}

#[test]
fn immutable_sqlite_handles_uri_characters_and_does_not_create_sidecars() {
    let fixture = Fixture::new();
    let renamed = fixture.root().join("legacy ?#%中文");
    fs::rename(&fixture.legacy, &renamed).unwrap();
    let database = renamed.join("state/pilot-turns.sqlite");
    let before = fs::read(&database).unwrap();
    let report = run(&fixture.attested(&["--legacy-root", renamed.to_str().unwrap()])).unwrap();
    assert_eq!(report["decision"], "legacy_read_only");
    assert_eq!(report["inventory"]["legacy_turn_ids"], json!(["turn-1"]));
    assert_eq!(fs::read(&database).unwrap(), before);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!PathBuf::from(format!("{}{suffix}", database.display())).exists());
    }
}

#[test]
fn corrupt_sqlite_and_invalid_turn_ids_statuses_fail_closed() {
    for sql in [
        "DROP TABLE turns;",
        "INSERT INTO turns VALUES ('turn-1', 'completed');",
        "UPDATE turns SET id=NULL;",
        "UPDATE turns SET id='';",
        "UPDATE turns SET status=NULL;",
        "UPDATE turns SET status='unexpected';",
    ] {
        let fixture = Fixture::new();
        let database = Connection::open(fixture.legacy.join("state/pilot-turns.sqlite")).unwrap();
        database.execute_batch(sql).unwrap();
        drop(database);
        assert_eq!(fixture.report()["decision"], "blocked", "{sql}");
    }
    let fixture = Fixture::new();
    fs::write(
        fixture.legacy.join("state/pilot-turns.sqlite"),
        "not a database",
    )
    .unwrap();
    assert_eq!(fixture.report()["decision"], "blocked");
}

#[test]
fn credentials_are_only_listed_and_review_is_required() {
    let fixture = Fixture::new();
    let secret = fixture.legacy.join("credentials.json");
    fs::write(&secret, "SECRET-CONTENT-MUST-NOT-APPEAR").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o0)).unwrap();
    let report =
        run(&fixture.arguments(&["--legacy-writer-stopped", "--legacy-read-only-confirmed"]))
            .unwrap();
    assert_eq!(report["decision"], "blocked");
    assert!(blockers_contain(&report, "explicit review"));
    assert!(
        report["credential_paths"]
            .as_array()
            .unwrap()
            .contains(&json!(secret))
    );
    assert!(
        !report
            .to_string()
            .contains("SECRET-CONTENT-MUST-NOT-APPEAR")
    );
    let ready = fixture.report();
    assert_eq!(ready["decision"], "legacy_read_only");
}

#[test]
fn configured_and_repeatable_explicit_credential_paths_are_resolved_without_values() {
    let fixture = Fixture::new();
    let config = fixture.root().join("config.json");
    write_json(
        &config,
        &json!({"providers": [{"SECRET_FILE": "provider.key", "token": "INLINE-SECRET-MUST-NOT-APPEAR"}]}),
    );
    let first = fixture.root().join("one");
    let second = fixture.root().join("two");
    let report = run(&fixture.attested(&[
        "--config",
        config.to_str().unwrap(),
        "--credential-path",
        first.to_str().unwrap(),
        "--credential-path",
        second.to_str().unwrap(),
    ]))
    .unwrap();
    assert_eq!(report["decision"], "legacy_read_only", "{report}");
    assert_eq!(report["credential_paths"].as_array().unwrap().len(), 3);
    assert!(
        report["credential_paths"]
            .as_array()
            .unwrap()
            .contains(&json!(fixture.root().join("provider.key")))
    );
    assert!(!report.to_string().contains("INLINE-SECRET-MUST-NOT-APPEAR"));
}

#[test]
fn explicit_credentials_are_never_read_even_when_named_as_records() {
    let fixture = Fixture::new();
    let credential = fixture.legacy.join("workspaces/graph/runs/run-1/run.json");
    fs::write(&credential, "SECRET-CONTENT").unwrap();
    let report =
        run(&fixture.attested(&["--credential-path", credential.to_str().unwrap()])).unwrap();
    assert_eq!(report["decision"], "blocked");
    assert!(issues_contain(&report, "refusing to read"));
    assert!(!report.to_string().contains("SECRET-CONTENT"));
}

#[test]
fn missing_malformed_and_symlink_config_fail_closed() {
    let fixture = Fixture::new();
    let config = fixture.root().join("config.json");
    assert_eq!(
        run(&fixture.attested(&["--config", config.to_str().unwrap()])).unwrap()["decision"],
        "blocked"
    );
    fs::write(&config, "invalid").unwrap();
    assert_eq!(
        run(&fixture.attested(&["--config", config.to_str().unwrap()])).unwrap()["decision"],
        "blocked"
    );
    let linked = fixture.root().join("config-link.json");
    symlink(&config, &linked).unwrap();
    assert_eq!(
        run(&fixture.attested(&["--config", linked.to_str().unwrap()])).unwrap()["decision"],
        "blocked"
    );
}

#[test]
fn symlink_roots_ancestors_and_entries_fail_closed() {
    let fixture = Fixture::new();
    let linked = fixture.root().join("legacy-link");
    symlink(&fixture.legacy, &linked).unwrap();
    let report = run(&fixture.attested(&["--legacy-root", linked.to_str().unwrap()])).unwrap();
    assert_eq!(report["decision"], "blocked");
    assert!(issues_contain(&report, "symlink"));
    let ancestor = fixture.root().join("ancestor");
    symlink(fixture.root(), &ancestor).unwrap();
    let report = run(&fixture.attested(&[
        "--rust-state-root",
        ancestor.join("missing-state").to_str().unwrap(),
    ]))
    .unwrap();
    assert_eq!(report["decision"], "blocked");
    assert!(issues_contain(&report, "symlink"));
    symlink(
        fixture.root().join("absent"),
        fixture.legacy.join("dangling"),
    )
    .unwrap();
    assert_eq!(fixture.report()["decision"], "blocked");
}

#[test]
fn non_regular_roots_and_special_entries_fail_closed_without_blocking_reads() {
    let fixture = Fixture::new();
    let file_root = fixture.root().join("file-root");
    fs::write(&file_root, "{}").unwrap();
    assert_eq!(
        run(&fixture.attested(&["--rust-state-root", file_root.to_str().unwrap()])).unwrap()["decision"],
        "blocked"
    );
    let fifo = fixture.legacy.join("state/fifo");
    rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RWXU).unwrap();
    let report = fixture.report();
    assert_eq!(report["decision"], "blocked");
    assert!(issues_contain(&report, "special file"));
}

#[test]
fn unsafe_rust_run_and_metadata_entries_fail_closed() {
    for directory in ["runs", "run-metadata"] {
        let fixture = Fixture::new();
        let state_directory = fixture.root().join("rust-state").join(directory);
        fs::create_dir_all(&state_directory).unwrap();
        symlink(
            fixture.root().join("outside.json"),
            state_directory.join("linked.json"),
        )
        .unwrap();
        let report = fixture.report();
        assert_eq!(report["decision"], "blocked");
        assert!(issues_contain(&report, "symlink in Rust state directory"));
    }
}

#[test]
fn root_overlaps_and_output_overlaps_block_preparation() {
    let fixture = Fixture::new();
    let report = run(&fixture.attested(&[
        "--rust-state-root",
        fixture.legacy.join("state").to_str().unwrap(),
    ]))
    .unwrap();
    assert_eq!(report["decision"], "blocked");
    assert!(blockers_contain(&report, "roots overlap"));
    let report = run(&fixture.attested(&[
        "--rust-workspace-root",
        fixture.root().join("rust-state/child").to_str().unwrap(),
    ]))
    .unwrap();
    assert_eq!(report["decision"], "blocked");
    let output = fixture.legacy.join("output");
    let report = fixture.prepare(&output);
    assert_eq!(report["decision"], "blocked");
    assert!(!output.exists());
    let ancestor_output = fixture.root().to_path_buf();
    assert!(blockers_contain(
        &fixture.prepare(&ancestor_output),
        "overlaps a data root"
    ));
}

#[test]
fn held_and_unknown_writer_locks_block_and_are_not_created() {
    for relative in [
        "legacy/library/plugins/.install.lock",
        "rust-state/deployment-locks/.deployment-writer.lock",
        "rust-state/runs/.run-1.lock",
    ] {
        let fixture = Fixture::new();
        let path = fixture.root().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
        let file = fs::File::open(&path).unwrap();
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).unwrap();
        assert_eq!(fixture.report()["decision"], "blocked", "{relative}");
        drop(file);
        assert_eq!(
            fixture.report()["decision"],
            "legacy_read_only",
            "{relative}"
        );
    }
    let fixture = Fixture::new();
    let lock = fixture.legacy.join("library/plugins/.install.lock");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    symlink(fixture.root().join("missing"), &lock).unwrap();
    let report = fixture.report();
    assert_eq!(
        report["locks"]["legacy_library_install"]["status"],
        "unknown"
    );
    assert_eq!(report["decision"], "blocked");
}

#[test]
fn observed_readonly_mode_does_not_replace_service_stop_attestation() {
    let fixture = Fixture::new();
    let mut issues = BTreeSet::new();
    let snapshot = filesystem::Snapshot::inspect(&fixture.legacy, "legacy", &mut issues);
    for entry in &snapshot.entries {
        fs::set_permissions(
            fixture.legacy.join(&entry.path),
            fs::Permissions::from_mode(if entry.kind == "directory" {
                0o555
            } else {
                0o444
            }),
        )
        .unwrap();
    }
    fs::set_permissions(&fixture.legacy, fs::Permissions::from_mode(0o555)).unwrap();
    let report = run(&fixture.arguments(&[])).unwrap();
    assert_eq!(report["policy"]["legacy_read_only_observed"], true);
    assert_eq!(report["decision"], "blocked");
    assert!(blockers_contain(&report, "service stop"));
    assert_eq!(
        run(&fixture.arguments(&["--legacy-writer-stopped"])).unwrap()["decision"],
        "legacy_read_only"
    );
}

#[test]
fn prepare_writes_only_private_atomic_manifest_and_index_and_is_idempotent() {
    let fixture = Fixture::new();
    let output = fixture.root().join("cutover");
    let report = fixture.prepare(&output);
    assert_eq!(report["decision"], "legacy_read_only", "{report}");
    assert_eq!(report["prepared_files"].as_array().unwrap().len(), 2);
    let manifest_file = output.join("cutover-manifest.json");
    let index_file = output.join("backup-index.json");
    let manifest_before = fs::read(&manifest_file).unwrap();
    let index_before = fs::read(&index_file).unwrap();
    let manifest: Value = serde_json::from_slice(&manifest_before).unwrap();
    let index: Value = serde_json::from_slice(&index_before).unwrap();
    assert_eq!(manifest["preparation"]["data_copied"], false);
    assert_eq!(manifest["preparation"]["secrets_copied"], false);
    assert!(index["kind"].as_str().unwrap().contains("no data backup"));
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for file in [&manifest_file, &index_file] {
        assert_eq!(
            fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let modified = fs::metadata(&manifest_file).unwrap().modified().unwrap();
    let repeated = fixture.prepare(&output);
    assert_eq!(repeated["decision"], "legacy_read_only", "{repeated}");
    assert_eq!(report["prepared_files"], repeated["prepared_files"]);
    assert_eq!(fs::read(&manifest_file).unwrap(), manifest_before);
    assert_eq!(fs::read(&index_file).unwrap(), index_before);
    assert_eq!(
        fs::metadata(&manifest_file).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(fs::read_dir(&output).unwrap().count(), 2);
    assert!(!fixture.root().join("rust-state").exists());
}

#[test]
fn output_dir_without_prepare_is_read_only() {
    let fixture = Fixture::new();
    let output = fixture.root().join("output");
    let report = run(&fixture.attested(&["--output-dir", output.to_str().unwrap()])).unwrap();
    assert_eq!(report["decision"], "legacy_read_only");
    assert!(!output.exists());
}

#[test]
fn prepare_accepts_private_empty_directory_and_nested_new_directory() {
    let fixture = Fixture::new();
    let output = fixture.root().join("empty");
    fs::create_dir(&output).unwrap();
    fs::set_permissions(&output, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(fixture.prepare(&output)["decision"], "legacy_read_only");
    let nested = fixture.root().join("new-parent/new-output");
    assert_eq!(fixture.prepare(&nested)["decision"], "legacy_read_only");
}

#[test]
fn prepare_rejects_non_private_or_nonempty_output_without_chmod_or_overwrite() {
    for unrelated in [false, true] {
        let fixture = Fixture::new();
        let output = fixture.root().join("existing-output");
        fs::create_dir(&output).unwrap();
        fs::set_permissions(&output, fs::Permissions::from_mode(0o755)).unwrap();
        if unrelated {
            fs::write(output.join("unrelated"), "preserve").unwrap();
        }
        let report = fixture.prepare(&output);
        assert_eq!(report["decision"], "blocked");
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o755
        );
        if unrelated {
            assert_eq!(
                fs::read_to_string(output.join("unrelated")).unwrap(),
                "preserve"
            );
        }
        assert!(!output.join("cutover-manifest.json").exists());
    }
}

#[test]
fn prepare_refuses_changed_partial_linked_and_non_private_prior_outputs() {
    for case in ["different", "partial", "symlink", "hardlink", "public"] {
        let fixture = Fixture::new();
        let output = fixture.root().join("output");
        assert_eq!(fixture.prepare(&output)["decision"], "legacy_read_only");
        let manifest = output.join("cutover-manifest.json");
        match case {
            "different" => fs::write(&manifest, "preserve-different-content").unwrap(),
            "partial" => fs::rename(&manifest, fixture.root().join("retained-manifest")).unwrap(),
            "symlink" => {
                fs::rename(&manifest, fixture.root().join("retained-manifest")).unwrap();
                symlink(fixture.root().join("retained-manifest"), &manifest).unwrap();
            }
            "hardlink" => fs::hard_link(&manifest, fixture.root().join("linked-manifest")).unwrap(),
            "public" => fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644)).unwrap(),
            _ => unreachable!(),
        }
        let before = fs::read(&manifest).ok();
        assert_eq!(fixture.prepare(&output)["decision"], "blocked", "{case}");
        assert_eq!(fs::read(&manifest).ok(), before);
    }
}

#[test]
fn prepare_rejects_symlink_output_and_ancestors_without_following_them() {
    let fixture = Fixture::new();
    let external = fixture.root().join("external");
    fs::create_dir(&external).unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o700)).unwrap();
    let linked = fixture.root().join("linked");
    symlink(&external, &linked).unwrap();
    assert_eq!(fixture.prepare(&linked)["decision"], "blocked");
    assert_eq!(fixture.prepare(&linked.join("new"))["decision"], "blocked");
    assert_eq!(fs::read_dir(&external).unwrap().count(), 0);
}

#[test]
fn changed_inventory_blocks_preparation_before_creating_output() {
    let fixture = Fixture::new();
    let mut issues = BTreeSet::new();
    let snapshot = filesystem::Snapshot::inspect(&fixture.legacy, "legacy", &mut issues);
    let report = fixture.report();
    fs::write(fixture.legacy.join("new-fact"), "new").unwrap();
    let output = fixture.root().join("output");
    assert!(
        super::prepare::write(&report, &output, &[snapshot])
            .unwrap_err()
            .contains("changed")
    );
    assert!(!output.exists());
}

#[test]
fn inline_flag_values_preserve_paths_and_repeatable_credentials() {
    let fixture = Fixture::new();
    let extra = format!(
        "--credential-path={}",
        fixture.root().join("secret").display()
    );
    let report = run(&fixture.attested(&[&extra])).unwrap();
    assert_eq!(
        report["credential_paths"],
        json!([fixture.root().join("secret")])
    );
}

#[test]
fn malformed_rust_fact_directories_fail_closed_instead_of_hiding_ids() {
    for directory in ["runs", "run-metadata", "platform"] {
        let fixture = Fixture::new();
        fs::create_dir(fixture.root().join("rust-state")).unwrap();
        fs::write(fixture.root().join("rust-state").join(directory), "{}").unwrap();
        let report = fixture.report();
        assert_eq!(report["decision"], "blocked", "{directory}");
        assert!(issues_contain(&report, "state path is not a directory"));
    }
}

#[test]
fn unread_sqlite_databases_with_sidecars_still_block_the_backup_index() {
    let fixture = Fixture::new();
    fs::write(
        fixture.legacy.join("state/pilot-conversations.sqlite-wal"),
        "uncheckpointed",
    )
    .unwrap();
    let output = fixture.root().join("output");
    let report = fixture.prepare(&output);
    assert_eq!(report["decision"], "blocked");
    assert!(issues_contain(&report, "WAL sidecar present"));
    assert!(!output.exists());
}

#[test]
fn changed_legacy_facts_do_not_overwrite_a_previous_preparation() {
    let fixture = Fixture::new();
    let output = fixture.root().join("output");
    assert_eq!(fixture.prepare(&output)["decision"], "legacy_read_only");
    let manifest = fs::read(output.join("cutover-manifest.json")).unwrap();
    let index = fs::read(output.join("backup-index.json")).unwrap();
    fs::write(fixture.legacy.join("new-fact"), "retain").unwrap();
    assert_eq!(fixture.prepare(&output)["decision"], "blocked");
    assert_eq!(
        fs::read(output.join("cutover-manifest.json")).unwrap(),
        manifest
    );
    assert_eq!(fs::read(output.join("backup-index.json")).unwrap(), index);
}

#[test]
fn held_output_directory_lock_blocks_without_writing_files() {
    let fixture = Fixture::new();
    let output = fixture.root().join("output");
    fs::create_dir(&output).unwrap();
    fs::set_permissions(&output, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fs::File::open(&output).unwrap();
    rustix::fs::flock(&directory, rustix::fs::FlockOperation::LockExclusive).unwrap();
    assert_eq!(fixture.prepare(&output)["decision"], "blocked");
    assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
}

#[test]
fn blocked_credential_review_does_not_create_preparation_output() {
    let fixture = Fixture::new();
    let credential = fixture.root().join("external-credential");
    let output = fixture.root().join("output");
    let report = run(&fixture.arguments(&[
        "--legacy-writer-stopped",
        "--legacy-read-only-confirmed",
        "--credential-path",
        credential.to_str().unwrap(),
        "--prepare",
        "--output-dir",
        output.to_str().unwrap(),
    ]))
    .unwrap();
    assert_eq!(report["decision"], "blocked");
    assert!(blockers_contain(&report, "explicit review"));
    assert!(!output.exists());
}
