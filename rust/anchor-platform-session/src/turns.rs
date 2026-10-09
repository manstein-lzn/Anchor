use std::collections::BTreeMap;

use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    Session, SessionError, SessionStatus, SessionStore, Turn, TurnEvent, TurnStatus, TurnWindow,
    associations, store,
};

const TURN_COLUMNS: &str =
    "id, session_id, request_id, prompt, status, error, created_at, updated_at";

impl SessionStore {
    pub fn create_turn(
        &self,
        owner: &str,
        session: &str,
        request_id: &str,
        prompt: Option<&str>,
    ) -> Result<(Turn, bool), SessionError> {
        store::validate_lookup(owner, session)?;
        validate_input(request_id, prompt)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        let existing = transaction
            .query_row(
                &format!(
                    "SELECT {TURN_COLUMNS} FROM turns WHERE session_id = ?1 AND request_id = ?2"
                ),
                params![session, request_id],
                read_turn,
            )
            .optional()?;
        if let Some(existing) = existing {
            let existing = associations::project_turn(&transaction, existing?)?;
            if existing.prompt.as_deref() != prompt {
                return Err(SessionError::Conflict(
                    "request_id was already used for different input".into(),
                ));
            }
            transaction.commit()?;
            return Ok((existing, false));
        }
        if snapshot.status == SessionStatus::Archived {
            return Err(SessionError::Conflict(
                "archived sessions cannot accept turns".into(),
            ));
        }
        if !snapshot.graph.is_empty() || !snapshot.channel.is_empty() {
            return Err(SessionError::Invalid(
                "native turns only support Pilot sessions".into(),
            ));
        }
        let turn = insert_running_turn(&transaction, owner, &mut snapshot, request_id, prompt)?;
        transaction.commit()?;
        Ok((turn, true))
    }

    pub fn get_turn(&self, owner: &str, session: &str, turn: &str) -> Result<Turn, SessionError> {
        validate_turn_lookup(owner, session, turn)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        store::read_session(&transaction, owner, session)?;
        let turn = find_turn(&transaction, session, turn)?;
        transaction.commit()?;
        Ok(turn)
    }

    pub fn list_turns(&self, owner: &str, session: &str) -> Result<Vec<Turn>, SessionError> {
        store::validate_lookup(owner, session)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        store::read_session(&transaction, owner, session)?;
        let turns = {
            let mut statement = transaction.prepare(&format!(
                "SELECT {TURN_COLUMNS} FROM turns WHERE session_id = ?1 ORDER BY created_at DESC, id ASC"
            ))?;
            statement
                .query_map([session], read_turn)?
                .map(|row| row.map_err(SessionError::from).and_then(|turn| turn))
                .collect::<Result<Vec<_>, _>>()?
        };
        let turns = turns
            .into_iter()
            .map(|turn| associations::project_turn(&transaction, turn))
            .collect::<Result<Vec<_>, _>>()?;
        transaction.commit()?;
        Ok(turns)
    }

    /// The execution windows this Run was associated with, oldest first.
    ///
    /// A resident assistant keeps one Run across many Turns, so a single
    /// `started`..`updated` span would report every idle wait as work. The
    /// association written when a Turn is claimed is the durable record of
    /// which Turns that Run actually executed, and it survives handover and
    /// restart. Read-only projections use it; an ordinary one-Turn Run gets the
    /// same interval either way.
    ///
    /// It takes no owner on purpose, unlike every other public Turn lookup: the
    /// Run board projects Runs across owners, and this result carries no prompt,
    /// error or delivery content. An owner-scoped caller must not reuse it.
    pub fn turn_windows_for_run(&self, run: &str) -> Result<Vec<TurnWindow>, SessionError> {
        store::validate_identity(run, "run id")?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let windows = {
            let mut statement = transaction.prepare(
                "SELECT turns.created_at, turns.updated_at, turns.status FROM turns
                 JOIN turn_runs ON turn_runs.turn_id = turns.id
                 WHERE turn_runs.run_id = ?1
                 ORDER BY turns.created_at ASC, turns.id ASC",
            )?;
            statement
                .query_map([run], read_window)?
                .map(|row| row.map_err(SessionError::from).and_then(|window| window))
                .collect::<Result<Vec<_>, _>>()?
        };
        transaction.commit()?;
        Ok(windows)
    }

    pub fn turn_events(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        after: u64,
    ) -> Result<Vec<TurnEvent>, SessionError> {
        validate_turn_lookup(owner, session, turn)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        store::read_session(&transaction, owner, session)?;
        find_turn(&transaction, session, turn)?;
        let events = {
            let mut statement = transaction.prepare(
                "SELECT seq, data FROM turn_events WHERE turn_id = ?1 AND seq > ?2 ORDER BY seq ASC",
            )?;
            statement
                .query_map(
                    params![turn, i64::try_from(after).unwrap_or(i64::MAX)],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )?
                .map(|row| {
                    let (seq, data) = row?;
                    Ok(TurnEvent {
                        seq: u64::try_from(seq)
                            .map_err(|error| SessionError::Storage(error.to_string()))?,
                        data: serde_json::from_str(&data)?,
                    })
                })
                .collect::<Result<Vec<_>, SessionError>>()?
        };
        transaction.commit()?;
        Ok(events)
    }

    pub fn append_turn_event(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        data: Value,
    ) -> Result<TurnEvent, SessionError> {
        validate_turn_lookup(owner, session, turn)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        store::read_session(&transaction, owner, session)?;
        if find_turn(&transaction, session, turn)?.status != TurnStatus::Running {
            return Err(SessionError::Conflict(
                "terminal Turn delivery is immutable".into(),
            ));
        }
        let event = append_ui_event(&transaction, turn, data)?;
        transaction.commit()?;
        Ok(event)
    }

    pub fn finish_turn(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        status: TurnStatus,
        error: Option<&str>,
    ) -> Result<Turn, SessionError> {
        validate_turn_lookup(owner, session, turn)?;
        if status == TurnStatus::Running {
            return Err(SessionError::Invalid(
                "finish requires a terminal turn status".into(),
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        let mut turn = find_turn(&transaction, session, turn)?;
        if turn.status != TurnStatus::Running {
            if turn.status != status || turn.error.as_deref() != error {
                return Err(SessionError::Conflict(
                    "terminal turn cannot be changed".into(),
                ));
            }
        } else {
            finish(&transaction, owner, &mut snapshot, &mut turn, status, error)?;
        }
        transaction.commit()?;
        Ok(turn)
    }

    pub fn interrupt_running(&self) -> Result<usize, SessionError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running = {
            let mut statement = transaction.prepare(
                "SELECT sessions.owner, turns.session_id, turns.id FROM turns
                 JOIN sessions ON sessions.id = turns.session_id WHERE turns.status = 'running'
                 ORDER BY turns.session_id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (owner, session, turn) in &running {
            let mut snapshot = store::read_session(&transaction, owner, session)?;
            let mut turn = find_turn(&transaction, session, turn)?;
            finish(
                &transaction,
                owner,
                &mut snapshot,
                &mut turn,
                TurnStatus::Interrupted,
                None,
            )?;
        }
        transaction.commit()?;
        Ok(running.len())
    }
}

pub(crate) fn insert_running_turn(
    connection: &Connection,
    owner: &str,
    snapshot: &mut Session,
    request_id: &str,
    prompt: Option<&str>,
) -> Result<Turn, SessionError> {
    validate_input(request_id, prompt)?;
    if snapshot.status == SessionStatus::Archived {
        return Err(SessionError::Conflict(
            "archived sessions cannot accept turns".into(),
        ));
    }
    ensure_no_running(connection, &snapshot.id)?;
    let now = Utc::now();
    let turn = Turn {
        id: Uuid::new_v4().to_string(),
        session: snapshot.id.clone(),
        request_id: request_id.into(),
        prompt: prompt.map(str::to_owned),
        status: TurnStatus::Running,
        error: None,
        native: None,
        goose: None,
        runs: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    connection.execute(
        "INSERT INTO turns(id, session_id, request_id, prompt, status, error, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'running', NULL, ?5, ?5)",
        params![
            turn.id,
            snapshot.id,
            request_id,
            prompt,
            now.to_rfc3339_opts(SecondsFormat::Nanos, true)
        ],
    )?;
    snapshot.status = SessionStatus::Active;
    snapshot.waiting_reason.clear();
    snapshot.updated_at = now;
    store::write_session(connection, owner, snapshot)?;
    append_lifecycle(connection, snapshot, &turn)?;
    Ok(turn)
}

pub(crate) fn ensure_no_running(
    connection: &Connection,
    session: &str,
) -> Result<(), SessionError> {
    let running: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM turns WHERE session_id = ?1 AND status = 'running')",
        [session],
        |row| row.get(0),
    )?;
    if running {
        return Err(SessionError::Conflict(
            "session already has a running turn".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_input(request_id: &str, prompt: Option<&str>) -> Result<(), SessionError> {
    if !(1..=128).contains(&request_id.len()) || request_id.trim().is_empty() {
        return Err(SessionError::Invalid(
            "request_id must contain 1 to 128 UTF-8 bytes and not be blank".into(),
        ));
    }
    if prompt.is_some_and(|prompt| prompt.trim().is_empty() || prompt.len() > 65536) {
        return Err(SessionError::Invalid(
            "prompt must be nonblank and at most 64 KiB in UTF-8 bytes".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_turn_lookup(
    owner: &str,
    session: &str,
    turn: &str,
) -> Result<(), SessionError> {
    store::validate_lookup(owner, session)?;
    store::validate_identity(turn, "turn id")
}

/// Read one Turn's execution window, whose columns are `created_at`,
/// `updated_at` and `status`. An unrecognized status reads as not running: a
/// projection must not fail the board over one unexpected row.
fn read_window(row: &Row<'_>) -> rusqlite::Result<Result<TurnWindow, SessionError>> {
    let created_at: String = row.get(0)?;
    let updated_at: String = row.get(1)?;
    let status: String = row.get(2)?;
    let times = created_at.parse().and_then(|created_at| {
        updated_at
            .parse()
            .map(|updated_at| (created_at, updated_at))
    });
    Ok(match times {
        Ok((created_at, updated_at)) => Ok(TurnWindow {
            created_at,
            updated_at,
            running: status == "running",
        }),
        Err(error) => Err(SessionError::Storage(format!(
            "invalid turn timestamp: {error}"
        ))),
    })
}

pub(crate) fn read_turn(row: &Row<'_>) -> rusqlite::Result<Result<Turn, SessionError>> {
    let status: String = row.get(4)?;
    let created_at: String = row.get(6)?;
    let updated_at: String = row.get(7)?;
    let status = match status.as_str() {
        "running" => TurnStatus::Running,
        "completed" => TurnStatus::Completed,
        "failed" => TurnStatus::Failed,
        "stopped" => TurnStatus::Stopped,
        "interrupted" => TurnStatus::Interrupted,
        _ => {
            return Ok(Err(SessionError::Storage(
                "invalid stored turn status".into(),
            )));
        }
    };
    let times = created_at.parse().and_then(|created_at| {
        updated_at
            .parse()
            .map(|updated_at| (created_at, updated_at))
    });
    Ok(match times {
        Ok((created_at, updated_at)) => Ok(Turn {
            id: row.get(0)?,
            session: row.get(1)?,
            request_id: row.get(2)?,
            prompt: row.get(3)?,
            status,
            error: row.get(5)?,
            native: None,
            goose: None,
            runs: Vec::new(),
            created_at,
            updated_at,
        }),
        Err(error) => Err(SessionError::Storage(format!(
            "invalid turn timestamp: {error}"
        ))),
    })
}

pub(crate) fn find_turn(
    connection: &Connection,
    session: &str,
    turn: &str,
) -> Result<Turn, SessionError> {
    let turn = connection
        .query_row(
            &format!("SELECT {TURN_COLUMNS} FROM turns WHERE session_id = ?1 AND id = ?2"),
            params![session, turn],
            read_turn,
        )
        .optional()?
        .ok_or(SessionError::Missing)??;
    associations::project_turn(connection, turn)
}

pub(crate) fn append_ui_event(
    connection: &Connection,
    turn: &str,
    data: Value,
) -> Result<TurnEvent, SessionError> {
    let previous: i64 = connection.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM turn_events WHERE turn_id = ?1",
        [turn],
        |row| row.get(0),
    )?;
    let seq = previous
        .checked_add(1)
        .ok_or_else(|| SessionError::Storage("Turn event sequence exhausted".into()))?;
    connection.execute(
        "INSERT INTO turn_events(turn_id, seq, data) VALUES (?1, ?2, ?3)",
        params![turn, seq, serde_json::to_string(&data)?],
    )?;
    Ok(TurnEvent {
        seq: seq as u64,
        data,
    })
}

fn append_lifecycle(
    connection: &Connection,
    session: &Session,
    turn: &Turn,
) -> Result<(), SessionError> {
    let kind = turn.status.event_kind();
    append_ui_event(connection, &turn.id, json!({"type": kind, "turn": turn}))?;
    store::append_event(
        connection,
        session,
        kind,
        BTreeMap::from([("turn".into(), json!(turn.id))]),
    )
}

pub(crate) fn finish(
    connection: &Connection,
    owner: &str,
    session: &mut Session,
    turn: &mut Turn,
    status: TurnStatus,
    error: Option<&str>,
) -> Result<(), SessionError> {
    turn.status = status;
    turn.error = error.map(str::to_owned);
    turn.updated_at = Utc::now();
    connection.execute(
        "UPDATE turns SET status = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
        params![
            turn.id,
            status.as_str(),
            error,
            turn.updated_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
        ],
    )?;
    if session.status != SessionStatus::Archived {
        session.status = match status {
            TurnStatus::Completed => SessionStatus::Active,
            _ => SessionStatus::Interrupted,
        };
        session.waiting_reason = error.unwrap_or_default().into();
    }
    session.updated_at = turn.updated_at;
    store::write_session(connection, owner, session)?;
    crate::questions::interrupt_pending(connection, session, turn)?;
    append_lifecycle(connection, session, turn)
}
