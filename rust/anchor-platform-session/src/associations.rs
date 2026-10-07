use std::collections::BTreeMap;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::json;

use crate::{
    GooseExecution, NativeExecution, SessionError, SessionStore, Turn, TurnStatus, store, turns,
};

impl SessionStore {
    pub fn bind_native(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        execution: NativeExecution,
    ) -> Result<Turn, SessionError> {
        turns::validate_turn_lookup(owner, session, turn)?;
        validate_native(&execution)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        let mut turn = turns::find_turn(&transaction, session, turn)?;
        if turn.goose.is_some() {
            return Err(SessionError::Conflict(
                "Turn is already bound to a Goose execution".into(),
            ));
        }
        if let Some(existing) = &turn.native {
            if existing != &execution {
                return Err(SessionError::Conflict(
                    "Turn is already bound to a different native execution".into(),
                ));
            }
        } else {
            if turn.status != TurnStatus::Running {
                return Err(SessionError::Conflict(
                    "only a running Turn can first bind a native execution".into(),
                ));
            }
            let claimed: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM turn_native WHERE scope = ?1 AND run = ?2)",
                params![execution.scope, execution.run],
                |row| row.get(0),
            )?;
            if claimed {
                return Err(SessionError::Conflict(
                    "native execution is already bound to another Turn".into(),
                ));
            }
            transaction.execute(
                "INSERT INTO turn_native(turn_id, scope, session, run) VALUES (?1, ?2, ?3, ?4)",
                params![turn.id, execution.scope, execution.session, execution.run],
            )?;
            snapshot.updated_at = Utc::now();
            store::write_session(&transaction, owner, &snapshot)?;
            store::append_event(
                &transaction,
                &snapshot,
                "turn.native_bound",
                BTreeMap::from([
                    ("turn".into(), json!(turn.id)),
                    ("native".into(), json!(execution)),
                ]),
            )?;
            turn.native = Some(execution);
        }
        transaction.commit()?;
        Ok(turn)
    }

    pub fn bind_goose(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        execution: GooseExecution,
    ) -> Result<Turn, SessionError> {
        turns::validate_turn_lookup(owner, session, turn)?;
        validate_goose(&execution)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        let mut turn = turns::find_turn(&transaction, session, turn)?;
        if turn.native.is_some() {
            return Err(SessionError::Conflict(
                "Turn is already bound to a native execution".into(),
            ));
        }
        if let Some(existing) = &turn.goose {
            if existing != &execution {
                return Err(SessionError::Conflict(
                    "Turn is already bound to a different Goose execution".into(),
                ));
            }
        } else {
            if turn.status != TurnStatus::Running {
                return Err(SessionError::Conflict(
                    "only a running Turn can first bind a Goose execution".into(),
                ));
            }
            let claimed: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM turn_goose
                 JOIN turns ON turns.id = turn_goose.turn_id
                 WHERE turn_goose.scope = ?1 AND turns.session_id != ?2)",
                params![execution.scope, session],
                |row| row.get(0),
            )?;
            if claimed {
                return Err(SessionError::Conflict(
                    "Goose scope is already bound to another platform Session".into(),
                ));
            }
            transaction.execute(
                "INSERT INTO turn_goose(turn_id, scope, session) VALUES (?1, ?2, ?3)",
                params![turn.id, execution.scope, execution.session],
            )?;
            snapshot.updated_at = Utc::now();
            store::write_session(&transaction, owner, &snapshot)?;
            store::append_event(
                &transaction,
                &snapshot,
                "turn.goose_bound",
                BTreeMap::from([
                    ("turn".into(), json!(turn.id)),
                    ("goose".into(), json!(execution)),
                ]),
            )?;
            turn.goose = Some(execution);
        }
        transaction.commit()?;
        Ok(turn)
    }

    pub fn associate_run(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        run: &str,
    ) -> Result<Turn, SessionError> {
        turns::validate_turn_lookup(owner, session, turn)?;
        store::validate_identity(run, "run id")?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        let mut turn = turns::find_turn(&transaction, session, turn)?;
        let associate = !turn.runs.iter().any(|existing| existing == run);
        let attach = !snapshot.run_ids.iter().any(|existing| existing == run);
        if associate || attach {
            snapshot.updated_at = Utc::now();
            if attach {
                snapshot.run_ids.push(run.into());
            }
            store::write_session(&transaction, owner, &snapshot)?;
            if attach {
                store::append_event(
                    &transaction,
                    &snapshot,
                    "run.attached",
                    BTreeMap::from([("run".into(), json!(run))]),
                )?;
            }
            if associate {
                let previous: i64 = transaction.query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM turn_runs WHERE turn_id = ?1",
                    [&turn.id],
                    |row| row.get(0),
                )?;
                let seq = previous
                    .checked_add(1)
                    .ok_or_else(|| SessionError::Storage("Turn run sequence exhausted".into()))?;
                transaction.execute(
                    "INSERT INTO turn_runs(turn_id, run_id, seq) VALUES (?1, ?2, ?3)",
                    params![turn.id, run, seq],
                )?;
                store::append_event(
                    &transaction,
                    &snapshot,
                    "turn.run_associated",
                    BTreeMap::from([("turn".into(), json!(turn.id)), ("run".into(), json!(run))]),
                )?;
                turn.runs.push(run.into());
            }
        }
        transaction.commit()?;
        Ok(turn)
    }
}

pub(crate) fn project_turn(connection: &Connection, mut turn: Turn) -> Result<Turn, SessionError> {
    turn.native = connection
        .query_row(
            "SELECT scope, session, run FROM turn_native WHERE turn_id = ?1",
            [&turn.id],
            |row| {
                Ok(NativeExecution {
                    scope: row.get(0)?,
                    session: row.get(1)?,
                    run: row.get(2)?,
                })
            },
        )
        .optional()?;
    turn.goose = connection
        .query_row(
            "SELECT scope, session FROM turn_goose WHERE turn_id = ?1",
            [&turn.id],
            |row| {
                Ok(GooseExecution {
                    scope: row.get(0)?,
                    session: row.get(1)?,
                })
            },
        )
        .optional()?;
    if turn.native.is_some() && turn.goose.is_some() {
        return Err(SessionError::Storage(
            "Turn has conflicting native and Goose executions".into(),
        ));
    }
    let mut statement =
        connection.prepare("SELECT run_id FROM turn_runs WHERE turn_id = ?1 ORDER BY seq ASC")?;
    turn.runs = statement
        .query_map([&turn.id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(turn)
}

fn validate_native(execution: &NativeExecution) -> Result<(), SessionError> {
    if execution.scope.len() != 64
        || !execution.scope.bytes().all(|byte| byte.is_ascii_hexdigit())
        || execution.session <= 0
        || execution.run <= 0
    {
        return Err(SessionError::Invalid(
            "native execution requires a 64-hex scope and positive session/run IDs".into(),
        ));
    }
    Ok(())
}

fn validate_goose(execution: &GooseExecution) -> Result<(), SessionError> {
    if execution.scope.len() != 64
        || !execution.scope.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !(1..=1024).contains(&execution.session.len())
        || execution.session.chars().any(char::is_control)
    {
        return Err(SessionError::Invalid(
            "Goose execution requires a 64-hex scope and a session ID of 1 to 1024 UTF-8 bytes without control characters".into(),
        ));
    }
    Ok(())
}
