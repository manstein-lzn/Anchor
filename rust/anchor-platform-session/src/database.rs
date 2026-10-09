use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::SessionError;

#[cfg(test)]
mod tests;

const APPLICATION_ID: i32 = 0x414e5353;
const SCHEMA_VERSION: i32 = 7;
const SCHEMA_V1: [(&str, &str, &str); 3] = [
    (
        "table",
        "sessions",
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY NOT NULL,
            owner TEXT NOT NULL,
            data TEXT NOT NULL CHECK(json_valid(data)),
            updated_at TEXT NOT NULL
        )",
    ),
    (
        "table",
        "session_events",
        "CREATE TABLE session_events (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL CHECK(seq > 0),
            at TEXT NOT NULL,
            kind TEXT NOT NULL,
            data TEXT NOT NULL CHECK(json_valid(data)),
            PRIMARY KEY(session_id, seq)
        )",
    ),
    (
        "index",
        "sessions_owner_updated",
        "CREATE INDEX sessions_owner_updated ON sessions(owner, updated_at DESC, id)",
    ),
];

const TURN_SCHEMA: [(&str, &str, &str); 4] = [
    (
        "table",
        "turns",
        "CREATE TABLE turns (
            id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            request_id TEXT NOT NULL CHECK(length(CAST(request_id AS BLOB)) BETWEEN 1 AND 128),
            prompt TEXT CHECK(prompt IS NULL OR length(CAST(prompt AS BLOB)) BETWEEN 1 AND 65536),
            status TEXT NOT NULL CHECK(status IN ('running', 'completed', 'failed', 'stopped', 'interrupted')),
            error TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(session_id, request_id)
        )",
    ),
    (
        "table",
        "turn_events",
        "CREATE TABLE turn_events (
            turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL CHECK(seq > 0),
            data TEXT NOT NULL CHECK(json_valid(data)),
            PRIMARY KEY(turn_id, seq)
        )",
    ),
    (
        "index",
        "turns_session_created",
        "CREATE INDEX turns_session_created ON turns(session_id, created_at DESC, id)",
    ),
    (
        "index",
        "turns_session_running",
        "CREATE UNIQUE INDEX turns_session_running ON turns(session_id) WHERE status = 'running'",
    ),
];

const RELATION_SCHEMA: [(&str, &str, &str); 2] = [
    (
        "table",
        "turn_native",
        "CREATE TABLE turn_native (
            turn_id TEXT PRIMARY KEY NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            scope TEXT NOT NULL CHECK(length(scope) = 64 AND scope NOT GLOB '*[^0-9a-fA-F]*'),
            session INTEGER NOT NULL CHECK(session > 0),
            run INTEGER NOT NULL CHECK(run > 0),
            UNIQUE(scope, run)
        )",
    ),
    (
        "table",
        "turn_runs",
        "CREATE TABLE turn_runs (
            turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            run_id TEXT NOT NULL,
            seq INTEGER NOT NULL CHECK(seq > 0),
            PRIMARY KEY(turn_id, run_id),
            UNIQUE(turn_id, seq)
        )",
    ),
];

const GOOSE_SCHEMA: [(&str, &str, &str); 2] = [
    (
        "table",
        "turn_goose",
        "CREATE TABLE turn_goose (
            turn_id TEXT PRIMARY KEY NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            scope TEXT NOT NULL CHECK(length(scope) = 64 AND scope NOT GLOB '*[^0-9a-fA-F]*'),
            session TEXT NOT NULL CHECK(
                length(CAST(session AS BLOB)) BETWEEN 1 AND 1024
                AND instr(session, char(0)) = 0
                AND session NOT GLOB ('*[' || char(1) || '-' || char(31) || char(127) || '-' || char(159) || ']*')
            )
        )",
    ),
    (
        "index",
        "turn_goose_execution",
        "CREATE INDEX turn_goose_execution ON turn_goose(scope, session)",
    ),
];

const QUESTION_SCHEMA: [(&str, &str, &str); 2] = [
    (
        "table",
        "questions",
        "CREATE TABLE questions (
            id TEXT PRIMARY KEY NOT NULL,
            turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL CHECK(seq > 0),
            message TEXT NOT NULL CHECK(length(CAST(message AS BLOB)) BETWEEN 1 AND 65536),
            requested_schema TEXT NOT NULL CHECK(json_valid(requested_schema) AND length(CAST(requested_schema AS BLOB)) <= 65536),
            status TEXT NOT NULL CHECK(status IN ('pending', 'answered', 'interrupted')),
            answer TEXT CHECK(answer IS NULL OR (json_valid(answer) AND length(CAST(answer AS BLOB)) <= 65536)),
            CHECK((status = 'answered' AND answer IS NOT NULL) OR (status != 'answered' AND answer IS NULL)),
            UNIQUE(turn_id, seq)
        )",
    ),
    (
        "index",
        "questions_turn_pending",
        "CREATE UNIQUE INDEX questions_turn_pending ON questions(turn_id) WHERE status = 'pending'",
    ),
];

const CHANNEL_SCHEMA: [(&str, &str, &str); 6] = [
    (
        "table",
        "channel_sessions",
        "CREATE TABLE channel_sessions (
            session_id TEXT PRIMARY KEY NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            owner TEXT NOT NULL,
            source TEXT NOT NULL CHECK(length(CAST(source AS BLOB)) BETWEEN 1 AND 256),
            account TEXT NOT NULL CHECK(length(CAST(account AS BLOB)) <= 256),
            conversation_id TEXT NOT NULL CHECK(length(CAST(conversation_id AS BLOB)) BETWEEN 1 AND 256),
            sender_id TEXT NOT NULL CHECK(length(CAST(sender_id AS BLOB)) BETWEEN 1 AND 256),
            UNIQUE(owner, source, account, conversation_id, sender_id)
        )",
    ),
    (
        "index",
        "channel_sessions_owner",
        "CREATE INDEX channel_sessions_owner ON channel_sessions(owner, session_id)",
    ),
    (
        "table",
        "channel_inbounds",
        "CREATE TABLE channel_inbounds (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            inbound_id TEXT NOT NULL CHECK(length(CAST(inbound_id AS BLOB)) BETWEEN 1 AND 128),
            turn_id TEXT NOT NULL UNIQUE REFERENCES turns(id) ON DELETE CASCADE,
            run_id TEXT UNIQUE,
            request TEXT NOT NULL CHECK(json_valid(request) AND length(CAST(request AS BLOB)) <= 262144),
            superseded_by_turn_id TEXT REFERENCES turns(id) ON DELETE SET NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY(session_id, inbound_id)
        )",
    ),
    (
        "index",
        "channel_inbounds_session_created",
        "CREATE INDEX channel_inbounds_session_created ON channel_inbounds(session_id, created_at, inbound_id)",
    ),
    (
        "table",
        "channel_deliveries",
        "CREATE TABLE channel_deliveries (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            key TEXT NOT NULL CHECK(length(CAST(key AS BLOB)) BETWEEN 1 AND 500),
            turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            kind TEXT NOT NULL CHECK(length(CAST(kind AS BLOB)) BETWEEN 1 AND 128),
            content_sha256 TEXT NOT NULL CHECK(length(content_sha256) = 64 AND content_sha256 NOT GLOB '*[^0-9a-f]*'),
            status TEXT NOT NULL CHECK(status IN ('pending', 'sending', 'confirmed', 'failed', 'unknown', 'suppressed')),
            error TEXT,
            superseded_by_turn_id TEXT REFERENCES turns(id) ON DELETE SET NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(session_id, key)
        )",
    ),
    (
        "index",
        "channel_deliveries_unfinished",
        "CREATE INDEX channel_deliveries_unfinished ON channel_deliveries(session_id, status, updated_at)",
    ),
];

const CHANNEL_INBOUND_SCHEMA_V7: (&str, &str, &str) = (
    "table",
    "channel_inbounds",
    "CREATE TABLE channel_inbounds (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            inbound_id TEXT NOT NULL CHECK(length(CAST(inbound_id AS BLOB)) BETWEEN 1 AND 128),
            turn_id TEXT NOT NULL UNIQUE REFERENCES turns(id) ON DELETE CASCADE,
            run_id TEXT,
            request TEXT NOT NULL CHECK(json_valid(request) AND length(CAST(request AS BLOB)) <= 262144),
            superseded_by_turn_id TEXT REFERENCES turns(id) ON DELETE SET NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY(session_id, inbound_id)
        )",
);

const CHANNEL_ASSISTANT_SCHEMA: [(&str, &str, &str); 4] = [
    (
        "index",
        "channel_inbounds_run",
        "CREATE INDEX channel_inbounds_run ON channel_inbounds(run_id, session_id) WHERE run_id IS NOT NULL",
    ),
    (
        "table",
        "channel_assistants",
        "CREATE TABLE channel_assistants (
            session_id TEXT NOT NULL REFERENCES channel_sessions(session_id) ON DELETE CASCADE,
            run_id TEXT PRIMARY KEY NOT NULL,
            wait_node TEXT NOT NULL CHECK(length(CAST(wait_node AS BLOB)) BETWEEN 1 AND 256),
            work_node TEXT NOT NULL CHECK(length(CAST(work_node AS BLOB)) BETWEEN 1 AND 256),
            reply_node TEXT NOT NULL CHECK(length(CAST(reply_node AS BLOB)) BETWEEN 1 AND 256),
            retired INTEGER NOT NULL DEFAULT 0 CHECK(retired IN (0, 1)),
            UNIQUE(session_id, run_id)
        )",
    ),
    (
        "index",
        "channel_assistants_current_session",
        "CREATE UNIQUE INDEX channel_assistants_current_session ON channel_assistants(session_id) WHERE retired = 0",
    ),
    (
        "table",
        "channel_assistant_inputs",
        "CREATE TABLE channel_assistant_inputs (
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            key TEXT NOT NULL CHECK(length(CAST(key AS BLOB)) BETWEEN 1 AND 4096),
            turn_id TEXT NOT NULL UNIQUE REFERENCES channel_inbounds(turn_id) ON DELETE CASCADE,
            PRIMARY KEY(run_id, key),
            FOREIGN KEY(session_id, run_id) REFERENCES channel_assistants(session_id, run_id) ON DELETE CASCADE
        )",
    ),
];

pub(crate) fn open(path: &Path) -> Result<Connection, SessionError> {
    let path = validate_path(path)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let mut connection = Connection::open_with_flags(path, flags)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    migrate(&mut connection)?;
    let journal: String =
        connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(SessionError::Storage(
            "Session database requires WAL".into(),
        ));
    }
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(connection)
}

fn migrate(connection: &mut Connection) -> Result<(), SessionError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let application: i32 =
        transaction.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i32 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let existing = schema(&transaction)?;
    let empty = application == 0 && version == 0 && existing.is_empty();
    let compatible = application == APPLICATION_ID
        && (1..=SCHEMA_VERSION).contains(&version)
        && existing == expected_schema(version);
    if !empty && !compatible {
        return Err(SessionError::Storage(
            "incompatible database: expected exact Anchor platform Session schema version 1, 2, 3, 4, 5, 6 or 7"
                .into(),
        ));
    }
    for (_, _, statement) in SCHEMA_V1
        .iter()
        .filter(|_| version < 1)
        .chain(TURN_SCHEMA.iter().filter(|_| version < 2))
        .chain(RELATION_SCHEMA.iter().filter(|_| version < 3))
        .chain(GOOSE_SCHEMA.iter().filter(|_| version < 4))
        .chain(QUESTION_SCHEMA.iter().filter(|_| version < 5))
        .chain(CHANNEL_SCHEMA.iter().filter(|_| version < 6))
    {
        transaction.execute(statement, [])?;
    }
    if version < 7 {
        migrate_channel_assistants(&transaction)?;
    }
    if version < SCHEMA_VERSION {
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    transaction.commit()?;
    Ok(())
}

fn migrate_channel_assistants(connection: &Connection) -> Result<(), SessionError> {
    connection.execute(
        "CREATE TEMP TABLE channel_inbounds_migration AS SELECT * FROM channel_inbounds",
        [],
    )?;
    connection.execute("DROP TABLE channel_inbounds", [])?;
    connection.execute(CHANNEL_INBOUND_SCHEMA_V7.2, [])?;
    connection.execute(
        "INSERT INTO channel_inbounds(
            session_id, inbound_id, turn_id, run_id, request, superseded_by_turn_id, created_at
         ) SELECT session_id, inbound_id, turn_id, run_id, request, superseded_by_turn_id, created_at
           FROM temp.channel_inbounds_migration",
        [],
    )?;
    connection.execute("DROP TABLE temp.channel_inbounds_migration", [])?;
    for (_, _, statement) in CHANNEL_SCHEMA
        .iter()
        .filter(|(_, name, _)| *name == "channel_inbounds_session_created")
        .chain(CHANNEL_ASSISTANT_SCHEMA.iter())
    {
        connection.execute(statement, [])?;
    }
    Ok(())
}

fn expected_schema(version: i32) -> Vec<(String, String, String)> {
    let mut expected: Vec<_> = SCHEMA_V1
        .iter()
        .chain(TURN_SCHEMA.iter().filter(|_| version >= 2))
        .chain(RELATION_SCHEMA.iter().filter(|_| version >= 3))
        .chain(GOOSE_SCHEMA.iter().filter(|_| version >= 4))
        .chain(QUESTION_SCHEMA.iter().filter(|_| version >= 5))
        .chain(CHANNEL_SCHEMA.iter().filter(|_| version >= 6))
        .chain(CHANNEL_ASSISTANT_SCHEMA.iter().filter(|_| version >= 7))
        .map(|definition| {
            if version >= 7 && definition.1 == "channel_inbounds" {
                &CHANNEL_INBOUND_SCHEMA_V7
            } else {
                definition
            }
        })
        .map(|(kind, name, statement)| (kind.to_string(), name.to_string(), statement.to_string()))
        .collect();
    expected.sort();
    expected
}

fn schema(connection: &Connection) -> Result<Vec<(String, String, String)>, SessionError> {
    let mut statement = connection.prepare(
        "SELECT type, name, sql FROM sqlite_schema
         WHERE sql IS NOT NULL ORDER BY type, name",
    )?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn validate_path(path: &Path) -> Result<PathBuf, SessionError> {
    if path.as_os_str().is_empty()
        || path.file_name().is_none()
        || path.file_name().is_some_and(|name| name == ":memory:")
        || path.to_string_lossy().starts_with("file:")
    {
        return Err(SessionError::Invalid(
            "expected a database file path".into(),
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| SessionError::Storage(error.to_string()))?
            .join(path)
    };
    let mut checked = PathBuf::new();
    for component in absolute.components() {
        if component == Component::ParentDir {
            return Err(SessionError::Invalid(
                "database path cannot contain parent traversal".into(),
            ));
        }
        checked.push(component);
        let is_database = checked == absolute;
        match fs::symlink_metadata(&checked) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || (is_database && !metadata.is_file())
                    || (!is_database && !metadata.is_dir())
                {
                    return Err(SessionError::Invalid(
                        "database path requires a regular file and non-symlink directories".into(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && is_database => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionError::Invalid(
                    "database parent directory must already exist".into(),
                ));
            }
            Err(error) => return Err(SessionError::Storage(error.to_string())),
        }
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut companion = absolute.as_os_str().to_owned();
        companion.push(suffix);
        match fs::symlink_metadata(Path::new(&companion)) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(SessionError::Invalid(
                    "database sidecars must be regular non-symlink files".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(SessionError::Storage(error.to_string())),
        }
    }
    Ok(absolute)
}
