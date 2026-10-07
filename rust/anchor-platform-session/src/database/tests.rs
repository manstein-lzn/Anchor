use rusqlite::Connection;

use super::{
    APPLICATION_ID, CHANNEL_SCHEMA, GOOSE_SCHEMA, QUESTION_SCHEMA, RELATION_SCHEMA, SCHEMA_V1,
    TURN_SCHEMA, migrate, schema,
};
use crate::SessionError;

fn assert_failed_migration_rolls_back(version: i32) {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.pragma_update(None, "page_size", 512).unwrap();
    for (_, _, statement) in SCHEMA_V1
        .iter()
        .chain(TURN_SCHEMA.iter().filter(|_| version >= 2))
        .chain(RELATION_SCHEMA.iter().filter(|_| version >= 3))
        .chain(GOOSE_SCHEMA.iter().filter(|_| version >= 4))
        .chain(QUESTION_SCHEMA.iter().filter(|_| version >= 5))
        .chain(CHANNEL_SCHEMA.iter().filter(|_| version >= 6))
    {
        connection.execute(statement, []).unwrap();
    }
    connection
        .pragma_update(None, "application_id", APPLICATION_ID)
        .unwrap();
    connection
        .pragma_update(None, "user_version", version)
        .unwrap();
    connection
        .execute(
            "INSERT INTO sessions VALUES ('preserved', 'alice', '{}', 'unchanged')",
            [],
        )
        .unwrap();
    let original_schema = schema(&connection).unwrap();
    let transaction = connection.transaction().unwrap();
    if version == 1 {
        for (_, _, statement) in TURN_SCHEMA {
            transaction.execute(statement, []).unwrap();
        }
    }
    let additions: &[(&str, &str, &str)] = match version {
        5 => &CHANNEL_SCHEMA,
        4 => &QUESTION_SCHEMA,
        3 => &GOOSE_SCHEMA,
        _ => &RELATION_SCHEMA,
    };
    transaction.execute(additions[0].2, []).unwrap();
    let budget: i64 = transaction
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    transaction.execute(additions[1].2, []).unwrap();
    if version < 3 {
        for (_, _, statement) in GOOSE_SCHEMA {
            transaction.execute(statement, []).unwrap();
        }
    }
    if version < 4 {
        for (_, _, statement) in QUESTION_SCHEMA {
            transaction.execute(statement, []).unwrap();
        }
    }
    if version < 5 {
        for (_, _, statement) in CHANNEL_SCHEMA {
            transaction.execute(statement, []).unwrap();
        }
    }
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
        "unexpected migration result: {result:?}"
    );
    assert_eq!(schema(&connection).unwrap(), original_schema);
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
        APPLICATION_ID
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT owner, data, updated_at FROM sessions WHERE id = 'preserved'",
                [],
                |row| Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?
                ))
            )
            .unwrap(),
        ("alice".into(), "{}".into(), "unchanged".into())
    );
    connection
        .pragma_update(None, "max_page_count", required)
        .unwrap();
    migrate(&mut connection).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
            .unwrap(),
        6
    );
}

#[test]
fn v1_migration_rolls_back_every_ddl_when_the_second_relation_cannot_be_created() {
    assert_failed_migration_rolls_back(1);
}

#[test]
fn v2_migration_rolls_back_every_ddl_when_the_second_relation_cannot_be_created() {
    assert_failed_migration_rolls_back(2);
}

#[test]
fn v3_migration_rolls_back_the_goose_table_when_the_index_cannot_be_created() {
    assert_failed_migration_rolls_back(3);
}

#[test]
fn v4_migration_rolls_back_questions_when_the_pending_index_cannot_be_created() {
    assert_failed_migration_rolls_back(4);
}
