use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

use anchor_platform_session::{CreateSession, Session, SessionError, SessionStatus, SessionStore};
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf, SessionStore) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("platform-sessions.sqlite");
    let store = SessionStore::open(&path).unwrap();
    (directory, path, store)
}

fn request(id: &str) -> CreateSession {
    CreateSession {
        id: Some(id.into()),
        ..Default::default()
    }
}

fn create(store: &SessionStore, owner: &str, id: &str) -> Session {
    store.create(owner, request(id)).unwrap()
}

fn assert_invalid<T>(result: Result<T, SessionError>) {
    assert!(matches!(result, Err(SessionError::Invalid(_))));
}

fn assert_storage<T>(result: Result<T, SessionError>) {
    assert!(matches!(result, Err(SessionError::Storage(_))));
}

#[test]
fn public_types_and_default_dto_match_legacy_initial_shape() {
    fn assert_thread_safe<Store: Clone + Send + Sync>() {}
    assert_thread_safe::<SessionStore>();
    let (_directory, _, store) = fixture();
    let request: CreateSession = serde_json::from_value(json!({})).unwrap();
    assert_eq!(request, CreateSession::default());
    let session = store.create("api:key-1", request).unwrap();
    uuid::Uuid::parse_str(&session.id).unwrap();
    assert_eq!(session.id, session.conversation_id);
    assert_eq!(session.created_at, session.updated_at);
    let dto = serde_json::to_value(&session).unwrap();
    let actual: BTreeSet<_> = dto
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let expected = BTreeSet::from([
        "id",
        "conversation_id",
        "title",
        "status",
        "waiting_reason",
        "run_ids",
        "graph",
        "reply_node",
        "channel",
        "approval",
        "approvals",
        "questions",
        "operation",
        "operations",
        "created_at",
        "updated_at",
    ]);
    assert_eq!(actual, expected);
    for field in ["title", "waiting_reason", "graph", "reply_node"] {
        assert_eq!(dto[field], "");
    }
    for field in ["approval", "operation"] {
        assert_eq!(dto[field], json!(null));
    }
    for field in ["approvals", "questions", "run_ids"] {
        assert_eq!(dto[field], json!([]));
    }
    for field in ["channel", "operations"] {
        assert_eq!(dto[field], json!({}));
    }
    assert_eq!(dto["status"], "active");
    assert_eq!(serde_json::from_value::<Session>(dto).unwrap(), session);
    for (status, serialized) in [
        (SessionStatus::Active, "active"),
        (SessionStatus::WaitingUser, "waiting_user"),
        (SessionStatus::Interrupted, "interrupted"),
        (SessionStatus::Archived, "archived"),
    ] {
        assert_eq!(serde_json::to_value(status).unwrap(), serialized);
        assert_eq!(
            serde_json::from_value::<SessionStatus>(json!(serialized)).unwrap(),
            status
        );
    }
    assert!(serde_json::from_value::<SessionStatus>(json!("running")).is_err());
}

#[test]
fn create_get_list_rename_status_and_events_form_one_lifecycle() {
    let (_directory, _, store) = fixture();
    let first = store
        .create(
            "local",
            CreateSession {
                id: Some("first".into()),
                title: "  首个会话  ".into(),
                graph: "assistant".into(),
                reply_node: "reply".into(),
                channel: BTreeMap::from([("userid".into(), "trusted-user".into())]),
            },
        )
        .unwrap();
    assert_eq!(first.title, "首个会话");
    assert_eq!(first.graph, "assistant");
    assert_eq!(first.reply_node, "reply");
    assert_eq!(first.channel["userid"], "trusted-user");
    assert_eq!(store.get("local", "first").unwrap(), first);
    create(&store, "local", "second");
    assert_eq!(store.list("local").unwrap()[0].id, "second");
    let renamed = store.rename("local", "first", "\u{2003}更新 👋\n").unwrap();
    assert_eq!(renamed.title, "更新 👋");
    assert_eq!(renamed.created_at, first.created_at);
    assert!(renamed.updated_at >= first.updated_at);
    assert_eq!(store.list("local").unwrap()[0].id, "first");
    let waiting = store
        .set_status("local", "first", SessionStatus::WaitingUser, "需要回复")
        .unwrap();
    assert_eq!(waiting.waiting_reason, "需要回复");
    store
        .set_status("local", "first", SessionStatus::Interrupted, "宿主重启")
        .unwrap();
    let active = store
        .set_status("local", "first", SessionStatus::Active, "")
        .unwrap();
    assert_eq!(active.waiting_reason, "");
    let events = store.events("local", "first", 0).unwrap();
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "session.created",
            "session.renamed",
            "session.waiting_user",
            "session.interrupted",
            "session.active",
        ]
    );
    assert_eq!(events[1].data["title"], "更新 👋");
    assert_eq!(events[2].data["reason"], "需要回复");
    assert!(events[4].data.is_empty());
    assert_eq!(events[4].at, active.updated_at);
    assert_eq!(store.events("local", "first", 2).unwrap(), events[2..]);
    assert!(store.events("local", "first", 5).unwrap().is_empty());
    assert!(store.events("local", "first", u64::MAX).unwrap().is_empty());
    assert_eq!(store.events("local", "second", 0).unwrap()[0].seq, 1);
}

#[test]
fn owner_isolation_applies_to_every_operation_and_prevents_claiming_an_id() {
    let (_directory, _, store) = fixture();
    let original = create(&store, "alice", "private");
    assert_eq!(store.get("bob", "private"), Err(SessionError::Missing));
    assert_eq!(
        store.rename("bob", "private", "new"),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.rename("bob", "private", ""),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.set_status("bob", "private", SessionStatus::Archived, ""),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.attach_run("bob", "private", "run"),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.events("bob", "private", 0),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.events("bob", "private", u64::MAX),
        Err(SessionError::Missing)
    );
    assert_eq!(store.delete("bob", "private"), Err(SessionError::Missing));
    assert_eq!(store.get("bob", "absent"), Err(SessionError::Missing));
    assert!(store.list("bob").unwrap().is_empty());
    for owner in ["alice", "bob"] {
        assert!(matches!(
            store.create(owner, request("private")),
            Err(SessionError::Conflict(_))
        ));
    }
    assert_eq!(store.get("alice", "private").unwrap(), original);
    assert_eq!(store.events("alice", "private", 0).unwrap().len(), 1);
    create(&store, "bob", "public-to-bob");
    assert_eq!(store.list("alice").unwrap(), vec![original]);
    assert_eq!(store.list("bob").unwrap()[0].id, "public-to-bob");
}

#[test]
fn titles_are_trimmed_and_count_unicode_characters_not_bytes() {
    let (_directory, _, store) = fixture();
    let original = create(&store, "local", "title");
    for title in [String::new(), "\n\u{2003} ".into(), "界".repeat(121)] {
        assert_invalid(store.rename("local", "title", &title));
    }
    assert_eq!(store.get("local", "title").unwrap(), original);
    assert_eq!(store.events("local", "title", 0).unwrap().len(), 1);
    let title = "界".repeat(120);
    assert_eq!(
        store
            .rename("local", "title", &format!("  {title}\t"))
            .unwrap()
            .title,
        title
    );
    assert_eq!(store.rename("local", "title", "👋").unwrap().title, "👋");
    assert_invalid(store.create(
        "local",
        CreateSession {
            title: "界".repeat(121),
            ..request("invalid-title")
        },
    ));
    assert_eq!(
        store.get("local", "invalid-title"),
        Err(SessionError::Missing)
    );
}

#[test]
fn invalid_identities_are_rejected_by_all_lookups() {
    let (_directory, _, store) = fixture();
    create(&store, "local", "valid");
    let too_long = "x".repeat(257);
    for invalid in [
        "",
        ".",
        "..",
        "../escape",
        "/absolute",
        "bad\\id",
        "nul\0",
        " ",
        "a b",
        &too_long,
    ] {
        assert_invalid(store.create("local", request(invalid)));
        assert_invalid(store.create(invalid, request("new")));
        assert_invalid(store.get("local", invalid));
        assert_invalid(store.get(invalid, "valid"));
        assert_invalid(store.list(invalid));
        assert_invalid(store.rename(invalid, "valid", "title"));
        assert_invalid(store.rename("local", invalid, "title"));
        assert_invalid(store.set_status(invalid, "valid", SessionStatus::Active, ""));
        assert_invalid(store.set_status("local", invalid, SessionStatus::Active, ""));
        assert_invalid(store.events(invalid, "valid", 0));
        assert_invalid(store.events("local", invalid, 0));
        assert_invalid(store.attach_run(invalid, "valid", "run"));
        assert_invalid(store.attach_run("local", invalid, "run"));
        assert_invalid(store.attach_run("local", "valid", invalid));
        assert_invalid(store.delete(invalid, "valid"));
        assert_invalid(store.delete("local", invalid));
    }
    assert_eq!(store.list("local").unwrap().len(), 1);
    assert_eq!(store.events("local", "valid", 0).unwrap().len(), 1);
}

#[test]
fn archived_sessions_cannot_transition_to_any_non_archived_status() {
    let (_directory, _, store) = fixture();
    create(&store, "local", "archive");
    let archived = store
        .set_status("local", "archive", SessionStatus::Archived, "done")
        .unwrap();
    for status in [
        SessionStatus::Active,
        SessionStatus::WaitingUser,
        SessionStatus::Interrupted,
    ] {
        assert!(matches!(
            store.set_status("local", "archive", status, "resume"),
            Err(SessionError::Conflict(_))
        ));
    }
    assert_eq!(store.get("local", "archive").unwrap(), archived);
    assert_eq!(store.events("local", "archive", 0).unwrap().len(), 2);
    assert_eq!(
        store
            .set_status("local", "archive", SessionStatus::Archived, "final")
            .unwrap()
            .waiting_reason,
        "final"
    );
}

#[test]
fn attachments_are_idempotent_and_retained_runs_block_deletion() {
    let (_directory, _, store) = fixture();
    create(&store, "alice", "runs");
    let attached = store.attach_run("alice", "runs", "run-1").unwrap();
    assert_eq!(
        store.clone().attach_run("alice", "runs", "run-1").unwrap(),
        attached
    );
    assert_eq!(attached.run_ids, vec!["run-1"]);
    let events = store.events("alice", "runs", 0).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, "run.attached");
    assert_eq!(events[1].data["run"], "run-1");
    assert!(matches!(
        store.delete("alice", "runs"),
        Err(SessionError::Conflict(_))
    ));
    assert_eq!(store.delete("bob", "runs"), Err(SessionError::Missing));
    assert_eq!(store.get("alice", "runs").unwrap(), attached);
    assert_eq!(store.events("alice", "runs", 0).unwrap(), events);
}

#[test]
fn deleting_an_empty_session_removes_its_events_and_nothing_else() {
    let (_directory, path, store) = fixture();
    create(&store, "alice", "empty");
    store.rename("alice", "empty", "name").unwrap();
    let other = create(&store, "bob", "other");
    store.delete("alice", "empty").unwrap();
    assert_eq!(store.get("alice", "empty"), Err(SessionError::Missing));
    assert_eq!(
        store.events("alice", "empty", 0),
        Err(SessionError::Missing)
    );
    assert_eq!(store.delete("alice", "empty"), Err(SessionError::Missing));
    assert!(store.list("alice").unwrap().is_empty());
    assert_eq!(store.get("bob", "other").unwrap(), other);
    let connection = Connection::open(path).unwrap();
    let remaining: i64 = connection
        .query_row("SELECT COUNT(*) FROM session_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 1);
}

#[test]
fn reopening_preserves_snapshots_owner_and_event_cursors() {
    let (_directory, path, store) = fixture();
    create(&store, "api:key", "persistent");
    store.rename("api:key", "persistent", "saved").unwrap();
    store.attach_run("api:key", "persistent", "run-1").unwrap();
    let snapshot = store
        .set_status(
            "api:key",
            "persistent",
            SessionStatus::Interrupted,
            "restart",
        )
        .unwrap();
    let events = store.events("api:key", "persistent", 0).unwrap();
    drop(store);
    let reopened = SessionStore::open(&path).unwrap();
    assert_eq!(reopened.get("api:key", "persistent").unwrap(), snapshot);
    assert_eq!(reopened.events("api:key", "persistent", 0).unwrap(), events);
    assert_eq!(
        reopened.get("other", "persistent"),
        Err(SessionError::Missing)
    );
    assert_eq!(
        reopened
            .attach_run("api:key", "persistent", "run-1")
            .unwrap(),
        snapshot
    );
    reopened
        .rename("api:key", "persistent", "after-restart")
        .unwrap();
    assert_eq!(
        reopened.events("api:key", "persistent", 4).unwrap()[0].seq,
        5
    );
    let connection = Connection::open(path).unwrap();
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

#[test]
fn mutation_and_event_insertion_rollback_together() {
    let (_directory, path, store) = fixture();
    let original = create(&store, "local", "atomic");
    let initial_events = store.events("local", "atomic", 0).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_activity BEFORE INSERT ON session_events
         BEGIN SELECT RAISE(ABORT, 'fixture event failure'); END;",
        )
        .unwrap();
    assert_storage(store.rename("local", "atomic", "not-saved"));
    assert_storage(store.set_status("local", "atomic", SessionStatus::Archived, "not-saved"));
    assert_storage(store.attach_run("local", "atomic", "not-saved"));
    assert_storage(store.create("other-owner", request("not-created")));
    assert_eq!(store.get("local", "atomic").unwrap(), original);
    assert_eq!(store.events("local", "atomic", 0).unwrap(), initial_events);
    assert_eq!(
        store.get("other-owner", "not-created"),
        Err(SessionError::Missing)
    );
    let orphan_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM session_events WHERE session_id = 'not-created'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphan_events, 0);
    connection
        .execute_batch("DROP TRIGGER fail_activity")
        .unwrap();
    store.rename("local", "atomic", "saved").unwrap();
    assert_eq!(store.events("local", "atomic", 1).unwrap()[0].seq, 2);
}

#[test]
fn cascading_delete_is_atomic_when_event_deletion_fails() {
    let (_directory, path, store) = fixture();
    let original = create(&store, "local", "atomic-delete");
    let events = store.events("local", "atomic-delete", 0).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_delete BEFORE DELETE ON session_events
         BEGIN SELECT RAISE(ABORT, 'fixture cascade failure'); END;",
        )
        .unwrap();
    assert_storage(store.delete("local", "atomic-delete"));
    assert_eq!(store.get("local", "atomic-delete").unwrap(), original);
    assert_eq!(store.events("local", "atomic-delete", 0).unwrap(), events);
    connection
        .execute_batch("DROP TRIGGER fail_delete")
        .unwrap();
    store.delete("local", "atomic-delete").unwrap();
    assert_eq!(
        store.get("local", "atomic-delete"),
        Err(SessionError::Missing)
    );
}

#[test]
fn exhausted_sequence_rolls_back_the_snapshot_update() {
    let (_directory, path, store) = fixture();
    let original = create(&store, "local", "exhausted");
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "INSERT INTO session_events(session_id, seq, at, kind, data)
         VALUES ('exhausted', ?1, ?2, 'fixture', '{}')",
            rusqlite::params![i64::MAX, original.created_at.to_rfc3339()],
        )
        .unwrap();
    assert_storage(store.rename("local", "exhausted", "not-saved"));
    assert_eq!(store.get("local", "exhausted").unwrap(), original);
    assert_eq!(store.events("local", "exhausted", 0).unwrap().len(), 2);
}

#[test]
fn two_connections_preserve_every_event_and_deduplicate_run_attachments() {
    let (_directory, path, first) = fixture();
    create(&first, "local", "concurrent");
    let second = SessionStore::open(path).unwrap();
    let start = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [first.clone(), second]
        .into_iter()
        .enumerate()
        .map(|(worker, store)| {
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                for iteration in 0..25 {
                    store
                        .attach_run("local", "concurrent", &format!("run-{iteration}"))
                        .unwrap();
                    store
                        .rename(
                            "local",
                            "concurrent",
                            &format!("worker-{worker}-{iteration}"),
                        )
                        .unwrap();
                    store
                        .set_status(
                            "local",
                            "concurrent",
                            SessionStatus::WaitingUser,
                            &format!("worker-{worker}"),
                        )
                        .unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let session = first.get("local", "concurrent").unwrap();
    let runs: BTreeSet<_> = session.run_ids.iter().cloned().collect();
    assert_eq!(session.run_ids.len(), 25);
    assert_eq!(
        runs,
        (0..25)
            .map(|iteration| format!("run-{iteration}"))
            .collect()
    );
    let events = first.events("local", "concurrent", 0).unwrap();
    assert_eq!(events.len(), 126);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (1..=126).collect::<Vec<_>>()
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "run.attached")
            .count(),
        25
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.renamed")
            .count(),
        50
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.waiting_user")
            .count(),
        50
    );
}

#[test]
fn concurrent_creation_cannot_overwrite_or_claim_a_session() {
    let (_directory, path, first) = fixture();
    let second = SessionStore::open(&path).unwrap();
    let start = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [("alice", first.clone()), ("bob", second)]
        .into_iter()
        .map(|(owner, store)| {
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                (owner, store.create(owner, request("racing-id")))
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|(_, result)| matches!(result, Err(SessionError::Conflict(_))))
            .count(),
        1
    );
    let (owner, result) = results.iter().find(|(_, result)| result.is_ok()).unwrap();
    assert_eq!(
        first.get(owner, "racing-id").unwrap(),
        *result.as_ref().unwrap()
    );
    assert_eq!(first.events(owner, "racing-id", 0).unwrap().len(), 1);
    let other = if *owner == "alice" { "bob" } else { "alice" };
    assert_eq!(first.get(other, "racing-id"), Err(SessionError::Missing));
}

#[test]
fn concurrent_open_initializes_one_compatible_schema() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("new.sqlite");
    let start = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|worker| {
            let start = Arc::clone(&start);
            let path = path.clone();
            thread::spawn(move || {
                start.wait();
                let store = SessionStore::open(path).unwrap();
                create(&store, "local", &format!("session-{worker}"));
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(
        SessionStore::open(path)
            .unwrap()
            .list("local")
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn busy_timeout_allows_a_writer_to_wait_for_another_connection() {
    let (_directory, path, store) = fixture();
    create(&store, "local", "waiting-writer");
    let connection = Connection::open(path).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (started, received) = std::sync::mpsc::sync_channel(0);
    let writer = thread::spawn(move || {
        started.send(()).unwrap();
        store.rename("local", "waiting-writer", "after-lock")
    });
    received.recv().unwrap();
    thread::sleep(Duration::from_millis(100));
    connection.execute_batch("COMMIT").unwrap();
    assert_eq!(writer.join().unwrap().unwrap().title, "after-lock");
}

#[test]
fn invalid_database_paths_do_not_create_directories_or_special_databases() {
    let directory = tempfile::tempdir().unwrap();
    assert_invalid(SessionStore::open(""));
    assert_invalid(SessionStore::open(":memory:"));
    assert_invalid(SessionStore::open("file:memory?mode=memory"));
    assert_invalid(SessionStore::open(directory.path()));
    let absent = directory.path().join("absent");
    assert_invalid(SessionStore::open(absent.join("new.sqlite")));
    assert!(!absent.exists());
    let file = directory.path().join("regular-file");
    fs::write(&file, "unchanged").unwrap();
    assert_invalid(SessionStore::open(file.join("new.sqlite")));
    assert_invalid(SessionStore::open(
        directory.path().join("../escape.sqlite"),
    ));
    assert_eq!(fs::read_to_string(file).unwrap(), "unchanged");
}

#[cfg(unix)]
#[test]
fn database_and_ancestor_symlinks_and_non_regular_files_are_rejected() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let actual = directory.path().join("actual");
    fs::create_dir(&actual).unwrap();
    let alias = directory.path().join("alias");
    symlink(&actual, &alias).unwrap();
    assert_invalid(SessionStore::open(alias.join("new.sqlite")));
    assert!(!actual.join("new.sqlite").exists());
    let target = actual.join("data.sqlite");
    fs::write(&target, "unchanged").unwrap();
    let linked = directory.path().join("linked.sqlite");
    symlink(&target, &linked).unwrap();
    assert_invalid(SessionStore::open(linked));
    assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
    let dangling = directory.path().join("dangling.sqlite");
    symlink(actual.join("absent.sqlite"), &dangling).unwrap();
    assert_invalid(SessionStore::open(dangling));
    assert!(!actual.join("absent.sqlite").exists());
    assert_invalid(SessionStore::open("/dev/null"));
    let socket = directory.path().join("socket.sqlite");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    assert_invalid(SessionStore::open(socket));
}

#[cfg(unix)]
#[test]
fn unsafe_database_sidecars_are_rejected_before_creating_a_database() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        let database = directory.path().join(format!("database{suffix}.sqlite"));
        let outside = directory.path().join(format!("outside{suffix}"));
        fs::write(&outside, "protected").unwrap();
        let companion = PathBuf::from(format!("{}{suffix}", database.display()));
        symlink(&outside, &companion).unwrap();
        assert_invalid(SessionStore::open(&database));
        assert!(!database.exists());
        assert_eq!(fs::read_to_string(outside).unwrap(), "protected");
    }
    let database = directory.path().join("directory-sidecar.sqlite");
    fs::create_dir(directory.path().join("directory-sidecar.sqlite-wal")).unwrap();
    assert_invalid(SessionStore::open(&database));
    assert!(!database.exists());
}

#[test]
fn foreign_and_corrupt_databases_are_rejected_without_replacing_their_data() {
    let directory = tempfile::tempdir().unwrap();
    let foreign = directory.path().join("framework.sqlite");
    let connection = Connection::open(&foreign).unwrap();
    connection.execute_batch("CREATE TABLE conversations(id TEXT PRIMARY KEY, messages TEXT); INSERT INTO conversations VALUES ('old', 'protected');").unwrap();
    drop(connection);
    let original = fs::read(&foreign).unwrap();
    assert_storage(SessionStore::open(&foreign));
    assert_eq!(fs::read(&foreign).unwrap(), original);
    assert!(!directory.path().join("framework.sqlite-wal").exists());
    let connection = Connection::open(&foreign).unwrap();
    let message: String = connection
        .query_row(
            "SELECT messages FROM conversations WHERE id = 'old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(message, "protected");
    let corrupt = directory.path().join("corrupt.sqlite");
    fs::write(&corrupt, "not a sqlite database").unwrap();
    assert_storage(SessionStore::open(&corrupt));
    assert_eq!(
        fs::read_to_string(corrupt).unwrap(),
        "not a sqlite database"
    );
}

#[test]
fn incompatible_schema_version_identity_or_shape_is_not_overwritten() {
    for change in [
        "PRAGMA user_version = 7",
        "PRAGMA application_id = 42",
        "CREATE TABLE unexpected(value TEXT)",
        "DROP INDEX sessions_owner_updated",
        "DROP TABLE session_events; CREATE TABLE session_events(session_id TEXT, seq INTEGER)",
        "ALTER TABLE sessions ADD COLUMN unexpected TEXT",
    ] {
        let (_directory, path, store) = fixture();
        create(&store, "local", "protected");
        drop(store);
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(change).unwrap();
        drop(connection);
        let original = fs::read(&path).unwrap();
        assert_storage(SessionStore::open(&path));
        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "schema was changed after {change}"
        );
    }
}
