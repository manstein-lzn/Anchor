use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use anchor_platform_session::{
    CreateSession, NativeExecution, Session, SessionError, SessionStore, Turn, TurnStatus,
};
use rusqlite::{Connection, params};
use serde_json::json;
use tempfile::TempDir;

const TURN_SCHEMA_V2: &str = "CREATE TABLE turns (
            id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            request_id TEXT NOT NULL CHECK(length(CAST(request_id AS BLOB)) BETWEEN 1 AND 128),
            prompt TEXT CHECK(prompt IS NULL OR length(CAST(prompt AS BLOB)) BETWEEN 1 AND 65536),
            status TEXT NOT NULL CHECK(status IN ('running', 'completed', 'failed', 'stopped', 'interrupted')),
            error TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(session_id, request_id)
        );
        CREATE TABLE turn_events (
            turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL CHECK(seq > 0),
            data TEXT NOT NULL CHECK(json_valid(data)),
            PRIMARY KEY(turn_id, seq)
        );
        CREATE INDEX turns_session_created ON turns(session_id, created_at DESC, id);
        CREATE UNIQUE INDEX turns_session_running ON turns(session_id) WHERE status = 'running';";

fn fixture(version: i32) -> (TempDir, PathBuf, Session, Turn) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.sqlite");
    let store = SessionStore::open(&path).unwrap();
    store
        .create(
            "alice",
            CreateSession {
                id: Some("pilot".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let turn = store
        .create_turn("alice", "pilot", "old-request", Some("old prompt"))
        .unwrap()
        .0;
    let turn = store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
        .unwrap();
    let snapshot = store.attach_run("alice", "pilot", "rust-existing").unwrap();
    drop(store);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX channel_deliveries_unfinished;
             DROP TABLE channel_deliveries;
             DROP INDEX channel_sessions_owner;
             DROP TABLE channel_inbounds;
             DROP TABLE channel_sessions;",
        )
        .unwrap();
    connection.execute_batch("DROP TABLE questions;").unwrap();
    if version < 4 {
        connection.execute_batch("DROP TABLE turn_goose;").unwrap();
    }
    if version < 3 {
        connection
            .execute_batch("DROP TABLE turn_native; DROP TABLE turn_runs;")
            .unwrap();
    }
    if version <= 2 {
        connection
            .execute_batch("DROP TABLE turn_events; DROP TABLE turns;")
            .unwrap();
    }
    if version == 2 {
        connection.execute_batch(TURN_SCHEMA_V2).unwrap();
        connection.execute(
            "INSERT INTO turns VALUES (?1, 'pilot', 'old-request', 'old prompt', 'completed', NULL, ?2, ?3)",
            params![turn.id, turn.created_at.to_rfc3339(), turn.updated_at.to_rfc3339()],
        ).unwrap();
        let mut old_dto = serde_json::to_value(&turn).unwrap();
        old_dto.as_object_mut().unwrap().remove("native");
        old_dto.as_object_mut().unwrap().remove("goose");
        old_dto.as_object_mut().unwrap().remove("runs");
        connection
            .execute(
                "INSERT INTO turn_events VALUES (?1, 1, ?2)",
                params![
                    turn.id,
                    json!({"type": "turn.completed", "turn": old_dto}).to_string()
                ],
            )
            .unwrap();
    }
    connection
        .pragma_update(None, "user_version", version)
        .unwrap();
    (directory, path, snapshot, turn)
}

fn execution() -> NativeExecution {
    NativeExecution {
        scope: "a".repeat(64),
        session: 12,
        run: 34,
    }
}

fn schema(connection: &Connection) -> Vec<(String, String, Option<String>)> {
    connection
        .prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn exact_v2_upgrades_without_rewriting_turns_sessions_or_legacy_delivery() {
    let (_directory, path, snapshot, original) = fixture(2);
    let connection = Connection::open(&path).unwrap();
    let before = schema(&connection);
    let data: String = connection
        .query_row("SELECT data FROM sessions WHERE id = 'pilot'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let delivery: String = connection
        .query_row("SELECT data FROM turn_events", [], |row| row.get(0))
        .unwrap();
    drop(connection);
    let store = SessionStore::open(&path).unwrap();
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.get("bob", "pilot"), Err(SessionError::Missing));
    assert_eq!(
        store.get_turn("alice", "pilot", &original.id).unwrap(),
        original
    );
    assert_eq!(
        store.list_turns("alice", "pilot").unwrap(),
        vec![original.clone()]
    );
    assert_eq!(
        store
            .create_turn("alice", "pilot", "old-request", Some("old prompt"))
            .unwrap(),
        (original.clone(), false)
    );
    let events = store
        .turn_events("alice", "pilot", &original.id, 0)
        .unwrap();
    assert!(events[0].data["turn"].get("native").is_none());
    assert!(events[0].data["turn"].get("runs").is_none());
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
            .unwrap(),
        6
    );
    assert_eq!(
        connection
            .query_row("SELECT data FROM sessions WHERE id = 'pilot'", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        data
    );
    assert_eq!(
        connection
            .query_row("SELECT data FROM turn_events", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        delivery
    );
    let after = schema(&connection);
    for definition in before {
        assert!(after.contains(&definition));
    }
    let associated = store
        .associate_run("alice", "pilot", &original.id, "rust-existing")
        .unwrap();
    assert_eq!(associated.runs, vec!["rust-existing"]);
    assert_eq!(
        store
            .turn_events("alice", "pilot", &original.id, 0)
            .unwrap(),
        events
    );
    assert_eq!(store.get("alice", "pilot").unwrap().status, snapshot.status);
    let next = store
        .create_turn("alice", "pilot", "new-request", None)
        .unwrap()
        .0;
    store
        .bind_native("alice", "pilot", &next.id, execution())
        .unwrap();
    let next = store
        .associate_run("alice", "pilot", &next.id, "rust-next")
        .unwrap();
    drop(connection);
    drop(store);
    let reopened = SessionStore::open(path).unwrap();
    assert_eq!(reopened.get_turn("alice", "pilot", &next.id).unwrap(), next);
    assert_eq!(
        reopened.get_turn("alice", "pilot", &original.id).unwrap(),
        associated
    );
}

#[test]
fn exact_v3_upgrade_keeps_native_facts_and_delivery_without_manufacturing_goose() {
    let (_directory, path, snapshot, mut original) = fixture(3);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "INSERT INTO turn_native VALUES (?1, ?2, ?3, ?4)",
            params![
                original.id,
                execution().scope,
                execution().session,
                execution().run
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO turn_runs VALUES (?1, 'rust-existing', 1)",
            [&original.id],
        )
        .unwrap();
    original.native = Some(execution());
    original.runs = vec!["rust-existing".into()];
    let mut old_dto = serde_json::to_value(&original).unwrap();
    old_dto.as_object_mut().unwrap().remove("goose");
    connection
        .execute(
            "UPDATE turn_events SET data = ?1 WHERE turn_id = ?2 AND seq = 2",
            params![
                json!({"type": "turn.completed", "turn": old_dto}).to_string(),
                original.id
            ],
        )
        .unwrap();
    let before = schema(&connection);
    let deliveries: Vec<String> = connection
        .prepare("SELECT data FROM turn_events ORDER BY seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(connection);
    let store = SessionStore::open(&path).unwrap();
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(
        store.get_turn("alice", "pilot", &original.id).unwrap(),
        original
    );
    assert_eq!(
        store.list_turns("alice", "pilot").unwrap(),
        vec![original.clone()]
    );
    assert_eq!(
        store
            .create_turn("alice", "pilot", "old-request", Some("old prompt"))
            .unwrap(),
        (original.clone(), false)
    );
    assert!(matches!(
        store.bind_goose(
            "alice",
            "pilot",
            &original.id,
            anchor_platform_session::GooseExecution {
                scope: execution().scope,
                session: "not-a-legacy-session".into()
            }
        ),
        Err(SessionError::Conflict(_))
    ));
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
            .unwrap(),
        6
    );
    let after = schema(&connection);
    for definition in before {
        assert!(after.contains(&definition));
    }
    let preserved: Vec<String> = connection
        .prepare("SELECT data FROM turn_events ORDER BY seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(preserved, deliveries);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM turn_goose", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(connection);
    drop(store);
    let reopened = SessionStore::open(path).unwrap();
    assert_eq!(
        reopened.get_turn("alice", "pilot", &original.id).unwrap(),
        original
    );
}

#[test]
fn exact_v1_upgrade_adds_empty_links_without_guessing_from_session_run_ids() {
    let (_directory, path, snapshot, _) = fixture(1);
    let store = SessionStore::open(path).unwrap();
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert!(store.list_turns("alice", "pilot").unwrap().is_empty());
    let turn = store.create_turn("alice", "pilot", "new", None).unwrap().0;
    assert!(turn.native.is_none());
    assert!(turn.runs.is_empty());
    store
        .bind_native("alice", "pilot", &turn.id, execution())
        .unwrap();
    assert_eq!(
        store
            .associate_run("alice", "pilot", &turn.id, "rust-existing")
            .unwrap()
            .runs,
        vec!["rust-existing"]
    );
    assert_eq!(
        store.get("alice", "pilot").unwrap().run_ids,
        snapshot.run_ids
    );
}

#[test]
fn altered_v2_and_v3_or_future_versions_are_rejected_before_any_ddl_or_wal_change() {
    for version in [2, 3] {
        for change in [
            "PRAGMA application_id = 0",
            "PRAGMA application_id = 42",
            "PRAGMA user_version = 0",
            "PRAGMA user_version = 1",
            "PRAGMA user_version = 5",
            "PRAGMA user_version = 999",
            "DROP INDEX turns_session_running",
            "CREATE INDEX extra ON turns(session_id)",
            "ALTER TABLE turns ADD COLUMN extra TEXT",
            "CREATE TABLE extra(value TEXT)",
            "CREATE VIEW extra AS SELECT * FROM turns",
            "CREATE TRIGGER extra AFTER INSERT ON turns BEGIN SELECT 1; END",
            "CREATE TABLE turn_native(value TEXT)",
            "CREATE TABLE turn_runs(value TEXT)",
        ] {
            let (_directory, path, _, _) = fixture(version);
            let connection = Connection::open(&path).unwrap();
            if version == 3 && change.starts_with("CREATE TABLE turn_") {
                connection
                    .execute_batch("DROP TABLE turn_native; DROP TABLE turn_runs;")
                    .unwrap();
            }
            connection.execute_batch(change).unwrap();
            let before_schema = schema(&connection);
            let application: i32 = connection
                .pragma_query_value(None, "application_id", |row| row.get(0))
                .unwrap();
            let version: i32 = connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            let journal: String = connection
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .unwrap();
            drop(connection);
            let before_bytes = fs::read(&path).unwrap();
            assert!(
                matches!(SessionStore::open(&path), Err(SessionError::Storage(_))),
                "accepted {change}"
            );
            assert_eq!(
                fs::read(&path).unwrap(),
                before_bytes,
                "database changed after {change}"
            );
            let connection = Connection::open(&path).unwrap();
            assert_eq!(
                schema(&connection),
                before_schema,
                "DDL occurred after {change}"
            );
            assert_eq!(
                connection
                    .pragma_query_value(None, "application_id", |row| row.get::<_, i32>(0))
                    .unwrap(),
                application
            );
            assert_eq!(
                connection
                    .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
                    .unwrap(),
                version
            );
            assert_eq!(
                connection
                    .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                    .unwrap(),
                journal
            );
        }
    }
}

#[test]
fn tampering_with_relation_definitions_is_rejected_and_not_repaired() {
    for change in [
        "DROP TABLE turn_native",
        "DROP TABLE turn_runs",
        "ALTER TABLE turn_native ADD COLUMN extra TEXT",
        "ALTER TABLE turn_runs ADD COLUMN extra TEXT",
        "DROP TABLE turn_native; CREATE TABLE turn_native(turn_id TEXT PRIMARY KEY, scope TEXT, session INTEGER, run INTEGER)",
        "DROP TABLE turn_runs; CREATE TABLE turn_runs(turn_id TEXT, run_id TEXT, seq INTEGER)",
    ] {
        let (_directory, path, _, _) = fixture(2);
        drop(SessionStore::open(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(change).unwrap();
        let original = schema(&connection);
        drop(connection);
        let original_bytes = fs::read(&path).unwrap();
        assert!(matches!(
            SessionStore::open(&path),
            Err(SessionError::Storage(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), original_bytes);
        assert_eq!(schema(&Connection::open(path).unwrap()), original);
    }
}

#[test]
fn concurrent_v2_migration_runs_once_and_retains_the_terminal_turn() {
    let (_directory, path, snapshot, turn) = fixture(2);
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let path = path.clone();
            thread::spawn(move || {
                barrier.wait();
                SessionStore::open(path).unwrap()
            })
        })
        .collect();
    let stores: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    for store in &stores {
        assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
        assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    }
    let associated = stores[0]
        .associate_run("alice", "pilot", &turn.id, "rust-existing")
        .unwrap();
    assert_eq!(
        stores[1].get_turn("alice", "pilot", &turn.id).unwrap(),
        associated
    );
}
