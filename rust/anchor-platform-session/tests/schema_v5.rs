use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use anchor_platform_session::{
    CreateSession, GooseExecution, QuestionStatus, Session, SessionError, SessionStore, Turn,
    TurnEvent, TurnStatus,
};
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

fn v4_fixture() -> (TempDir, PathBuf, Session, Turn, Vec<TurnEvent>) {
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
        .create_turn("alice", "pilot", "old-request", Some("hello"))
        .unwrap()
        .0;
    store
        .bind_goose(
            "alice",
            "pilot",
            &turn.id,
            GooseExecution {
                scope: "a".repeat(64),
                session: "goose/原生".into(),
            },
        )
        .unwrap();
    let turn = store
        .associate_run("alice", "pilot", &turn.id, "retained-run")
        .unwrap();
    let turn = store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
        .unwrap();
    let session = store.get("alice", "pilot").unwrap();
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    drop(store);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE channel_assistant_inputs;
             DROP TABLE channel_assistants;
             DROP INDEX channel_deliveries_unfinished;
             DROP TABLE channel_deliveries;
             DROP INDEX channel_sessions_owner;
             DROP TABLE channel_inbounds;
             DROP TABLE channel_sessions;
             DROP TABLE questions;
             PRAGMA user_version = 4;",
        )
        .unwrap();
    (directory, path, session, turn, events)
}

fn old_facts(connection: &Connection) -> Vec<(String, Vec<String>)> {
    [
        "sessions",
        "session_events",
        "turns",
        "turn_events",
        "turn_goose",
        "turn_runs",
    ]
    .into_iter()
    .map(|table| {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                let fields = (0..row.as_ref().column_count())
                    .map(|column| format!("{:?}", row.get_ref(column).unwrap()))
                    .collect::<Vec<_>>();
                Ok(fields.join("|"))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        (table.into(), rows)
    })
    .collect()
}

#[test]
fn exact_v4_migrates_to_v5_without_rewriting_session_turn_goose_or_delivery_facts() {
    let (_directory, path, session, turn, events) = v4_fixture();
    let connection = Connection::open(&path).unwrap();
    let before = old_facts(&connection);
    let definitions = connection
        .prepare(
            "SELECT type, name, sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type, name",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(connection);
    let store = SessionStore::open(&path).unwrap();
    assert_eq!(store.get("alice", "pilot").unwrap(), session);
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(
        store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        events
    );
    assert!(
        store
            .list_questions("alice", "pilot", &turn.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.get("bob", "pilot"), Err(SessionError::Missing));
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
            .unwrap(),
        7
    );
    assert_eq!(old_facts(&connection), before);
    let after = connection
        .prepare(
            "SELECT type, name, sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type, name",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for definition in definitions {
        assert!(after.contains(&definition));
    }
    let next = store.create_turn("alice", "pilot", "next", None).unwrap().0;
    let question = store
        .create_question(
            "alice",
            "pilot",
            &next.id,
            "Confirm?",
            json!({"type":"object", "properties":{"confirm":{"type":"boolean"}}}),
        )
        .unwrap();
    assert_eq!(question.status, QuestionStatus::Pending);
    drop(store);
    drop(connection);
    let reopened = SessionStore::open(path).unwrap();
    assert_eq!(
        reopened
            .get_question("alice", "pilot", &next.id, &question.id)
            .unwrap(),
        question
    );
}

#[test]
fn concurrent_v4_migration_runs_once_and_retains_all_old_facts() {
    let (_directory, path, session, turn, events) = v4_fixture();
    let barrier = Arc::new(Barrier::new(3));
    let workers: Vec<_> = (0..3)
        .map(|_| {
            let barrier = barrier.clone();
            let path = path.clone();
            thread::spawn(move || {
                barrier.wait();
                SessionStore::open(path).unwrap()
            })
        })
        .collect();
    for worker in workers {
        let store = worker.join().unwrap();
        assert_eq!(store.get("alice", "pilot").unwrap(), session);
        assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
        assert_eq!(
            store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
            events
        );
        assert!(
            store
                .list_questions("alice", "pilot", &turn.id)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn malformed_v4_or_v5_schema_is_rejected_without_migrating_or_changing_facts() {
    for change in [
        "DROP INDEX turn_goose_execution",
        "ALTER TABLE turn_goose ADD COLUMN unexpected TEXT",
        "PRAGMA user_version = 5",
    ] {
        let (_directory, path, _, _, _) = v4_fixture();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(change).unwrap();
        let before = old_facts(&connection);
        let version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
            .unwrap();
        assert!(matches!(
            SessionStore::open(&path),
            Err(SessionError::Storage(_))
        ));
        assert_eq!(old_facts(&connection), before);
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
                .unwrap(),
            version
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'questions'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    let (_directory, path, _, _, _) = v4_fixture();
    let store = SessionStore::open(&path).unwrap();
    drop(store);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP INDEX questions_turn_pending;")
        .unwrap();
    assert!(matches!(
        SessionStore::open(&path),
        Err(SessionError::Storage(_))
    ));
}
