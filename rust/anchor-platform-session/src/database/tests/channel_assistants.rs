use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use rusqlite::{Connection, types::Value};
use serde_json::json;
use tempfile::TempDir;

use super::super::{
    APPLICATION_ID, CHANNEL_SCHEMA, GOOSE_SCHEMA, QUESTION_SCHEMA, RELATION_SCHEMA, SCHEMA_V1,
    SCHEMA_VERSION, TURN_SCHEMA, expected_schema, migrate, migrate_channel_assistants, schema,
};
use crate::{
    AttachmentManifest, ChannelDeliveryRequest, ChannelIdentity, ChannelInboundRequest,
    CreateSession, GooseExecution, NativeExecution, SessionError, SessionStore, TurnStatus,
};

fn inbound(id: &str, run: Option<&str>) -> ChannelInboundRequest {
    ChannelInboundRequest {
        inbound_id: id.into(),
        identity: ChannelIdentity {
            source: "wecom".into(),
            account: Some("corp".into()),
            conversation_id: "conversation".into(),
            sender_id: "sender".into(),
        },
        graph: "graph".into(),
        reply_node: "assistant".into(),
        text: Some(format!("message {id}")),
        attachments: AttachmentManifest::default(),
        run_id: run.map(str::to_owned),
        replace_running: true,
    }
}

fn v6_fixture() -> (TempDir, PathBuf, String) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.sqlite");
    let store = SessionStore::open(&path).unwrap();
    let pilot = store.create("owner", CreateSession::default()).unwrap();
    let first = store
        .create_turn("owner", &pilot.id, "native-turn", Some("old prompt"))
        .unwrap()
        .0;
    store
        .bind_native(
            "owner",
            &pilot.id,
            &first.id,
            NativeExecution {
                scope: "a".repeat(64),
                session: 1,
                run: 1,
            },
        )
        .unwrap();
    store
        .associate_run("owner", &pilot.id, &first.id, "pilot-run")
        .unwrap();
    store
        .append_turn_event(
            "owner",
            &pilot.id,
            &first.id,
            json!({"type":"text-delta", "delta":"old event"}),
        )
        .unwrap();
    store
        .create_question(
            "owner",
            &pilot.id,
            &first.id,
            "Confirm?",
            json!({"type":"object", "properties":{"confirm":{"type":"boolean"}}}),
        )
        .unwrap();
    store
        .finish_turn("owner", &pilot.id, &first.id, TurnStatus::Completed, None)
        .unwrap();
    let second = store
        .create_turn("owner", &pilot.id, "goose-turn", None)
        .unwrap()
        .0;
    store
        .bind_goose(
            "owner",
            &pilot.id,
            &second.id,
            GooseExecution {
                scope: "b".repeat(64),
                session: "opaque-native-session".into(),
            },
        )
        .unwrap();
    store
        .finish_turn("owner", &pilot.id, &second.id, TurnStatus::Completed, None)
        .unwrap();
    let first = store
        .admit_channel_inbound("owner", inbound("old-1", Some("old-run-1")))
        .unwrap();
    let delivery = ChannelDeliveryRequest {
        key: "old-delivery".into(),
        kind: "text".into(),
        content_sha256: "c".repeat(64),
    };
    store
        .admit_channel_delivery("owner", &first.session.id, &first.turn.id, delivery.clone())
        .unwrap();
    store
        .begin_channel_delivery("owner", &first.session.id, &first.turn.id, delivery)
        .unwrap();
    let second = store
        .admit_channel_inbound("owner", inbound("old-2", Some("old-run-2")))
        .unwrap();
    drop(store);
    let mut connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    let transaction = connection.transaction().unwrap();
    transaction
        .execute("DROP TABLE channel_assistant_inputs", [])
        .unwrap();
    transaction
        .execute("DROP TABLE channel_assistants", [])
        .unwrap();
    transaction
        .execute(
            "CREATE TEMP TABLE preserved_inbounds AS SELECT * FROM channel_inbounds",
            [],
        )
        .unwrap();
    transaction
        .execute("DROP TABLE channel_inbounds", [])
        .unwrap();
    for (_, name, statement) in CHANNEL_SCHEMA {
        if name == "channel_inbounds" || name == "channel_inbounds_session_created" {
            transaction.execute(statement, []).unwrap();
        }
    }
    transaction
        .execute(
            "INSERT INTO channel_inbounds SELECT * FROM temp.preserved_inbounds",
            [],
        )
        .unwrap();
    transaction
        .execute("DROP TABLE temp.preserved_inbounds", [])
        .unwrap();
    transaction.pragma_update(None, "user_version", 6).unwrap();
    transaction.commit().unwrap();
    connection.execute_batch("VACUUM").unwrap();
    assert_eq!(schema(&connection).unwrap(), expected_schema(6));
    (directory, path, second.session.id)
}

fn old_facts(connection: &Connection) -> BTreeMap<String, Vec<Vec<Value>>> {
    SCHEMA_V1
        .iter()
        .chain(TURN_SCHEMA.iter())
        .chain(RELATION_SCHEMA.iter())
        .chain(GOOSE_SCHEMA.iter())
        .chain(QUESTION_SCHEMA.iter())
        .chain(CHANNEL_SCHEMA.iter())
        .filter(|(kind, _, _)| *kind == "table")
        .map(|(_, name, _)| {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {name} ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get(column))
                        .collect::<rusqlite::Result<Vec<Value>>>()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (name.to_string(), rows)
        })
        .collect()
}

#[test]
fn v6_migration_preserves_every_old_table_index_and_legacy_run_rejection() {
    let (_directory, path, session) = v6_fixture();
    let connection = Connection::open(&path).unwrap();
    let before = old_facts(&connection);
    let definitions = schema(&connection).unwrap();
    let store = SessionStore::open(&path).unwrap();
    assert_eq!(old_facts(&connection), before);
    let after = schema(&connection).unwrap();
    assert_eq!(after, expected_schema(SCHEMA_VERSION));
    for definition in definitions {
        if definition.1 != "channel_inbounds" {
            assert!(after.contains(&definition));
        }
    }
    assert_eq!(
        connection
            .pragma_query_value(None, "application_id", |row| row.get::<_, i32>(0))
            .unwrap(),
        APPLICATION_ID
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    assert!(
        !connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        store.get_channel_assistant("owner", &session).unwrap(),
        None
    );
    store
        .bind_channel_assistant("owner", &session, "old-run-2", "wait", "assistant", "reply")
        .unwrap();
    let first = store
        .claim_channel_assistant_input("owner", &session, "old-run-2", "wait/1")
        .unwrap()
        .unwrap();
    assert_eq!(first.request.inbound_id, "old-2");
    let next = store
        .admit_channel_inbound("owner", inbound("new-3", None))
        .unwrap();
    assert!(matches!(
        store.associate_channel_run("owner", &session, "new-3", "old-run-2"),
        Err(SessionError::Conflict(_))
    ));
    assert_eq!(
        store
            .get_channel_relation("owner", &session, "new-3")
            .unwrap()
            .run_id,
        None
    );
    let second = store
        .claim_channel_assistant_input("owner", &session, "old-run-2", "wait/2")
        .unwrap()
        .unwrap();
    assert_eq!(second.turn.id, next.turn.id);
    assert_eq!(second.turn.runs, ["old-run-2"]);
    assert_eq!(
        store
            .claim_channel_assistant_input("owner", &session, "old-run-2", "wait/1")
            .unwrap()
            .unwrap()
            .turn
            .id,
        first.turn.id
    );
    drop(store);
    let reopened = SessionStore::open(path).unwrap();
    assert_eq!(
        reopened
            .get_channel_assistant_input("owner", &session, "old-run-2", "wait/2")
            .unwrap(),
        Some(second)
    );
}

#[test]
fn v6_failed_rebuild_rolls_back_schema_version_data_and_indexes() {
    let (_directory, path, _session) = v6_fixture();
    let mut connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    let original_schema = schema(&connection).unwrap();
    let before = old_facts(&connection);
    let budget: i64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    let transaction = connection.transaction().unwrap();
    migrate_channel_assistants(&transaction).unwrap();
    let required: i64 = transaction
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    assert!(required > budget);
    transaction.rollback().unwrap();
    connection
        .pragma_update(None, "max_page_count", budget)
        .unwrap();
    let result = migrate(&mut connection);
    assert!(
        matches!(&result, Err(SessionError::Storage(message)) if message.contains("full")),
        "{result:?}"
    );
    assert_eq!(schema(&connection).unwrap(), original_schema);
    assert_eq!(old_facts(&connection), before);
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
            .unwrap(),
        6
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "application_id", |row| row.get::<_, i32>(0))
            .unwrap(),
        APPLICATION_ID
    );
    connection
        .pragma_update(None, "max_page_count", required)
        .unwrap();
    migrate(&mut connection).unwrap();
    assert_eq!(old_facts(&connection), before);
    assert_eq!(
        schema(&connection).unwrap(),
        expected_schema(SCHEMA_VERSION)
    );
}

#[test]
fn concurrent_v6_migrations_preserve_all_old_facts() {
    let (_directory, path, session) = v6_fixture();
    let before = old_facts(&Connection::open(&path).unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let workers = (0..3)
        .map(|_| {
            let barrier = barrier.clone();
            let path = path.clone();
            thread::spawn(move || {
                barrier.wait();
                SessionStore::open(path).unwrap()
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        assert_eq!(
            worker
                .join()
                .unwrap()
                .get_channel_assistant("owner", &session)
                .unwrap(),
            None
        );
    }
    assert_eq!(old_facts(&Connection::open(&path).unwrap()), before);
}

#[test]
fn altered_v6_v7_and_future_schemas_are_rejected_before_any_mutation() {
    for (version, alteration) in [
        (6, "DROP INDEX channel_inbounds_session_created"),
        (7, "DROP INDEX channel_inbounds_run"),
        (7, "PRAGMA user_version = 8"),
        (7, "PRAGMA application_id = 0"),
    ] {
        let (_directory, path, _session) = v6_fixture();
        let mut connection = Connection::open(&path).unwrap();
        if version == 7 {
            migrate(&mut connection).unwrap();
        }
        connection.execute_batch(alteration).unwrap();
        let original_schema = schema(&connection).unwrap();
        let before = old_facts(&connection);
        let version: i32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let application: i32 = connection
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .unwrap();
        assert!(matches!(
            SessionStore::open(&path),
            Err(SessionError::Storage(_))
        ));
        assert_eq!(schema(&connection).unwrap(), original_schema);
        assert_eq!(old_facts(&connection), before);
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
                .unwrap(),
            version
        );
        assert_eq!(
            connection
                .pragma_query_value(None, "application_id", |row| row.get::<_, i32>(0))
                .unwrap(),
            application
        );
    }
}
