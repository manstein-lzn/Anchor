//! Durable FIFO results share Session events; a claim alone creates a system Turn.
use super::*;
use crate::{AssistantInput, TaskResult, TaskResultAdmission, TurnStatus, associations};
use serde::{Deserialize, Serialize};

const QUEUED: &str = "task.result.queued";
const CLAIMED: &str = "task.result.claimed";
const HANDLED: &str = "task.result.handled";

#[derive(Deserialize, Serialize)]
struct Claim {
    id: String,
    turn: String,
    run: String,
    key: String,
}

impl SessionStore {
    /// Result/status delivery is idempotent and never starts or revives an assistant.
    pub fn record_task_result(
        &self,
        owner: &str,
        session: &str,
        result: &TaskResult,
    ) -> Result<(), SessionError> {
        store::validate_lookup(owner, session)?;
        store::validate_identity(&result.id, "task result id")?;
        if let Some(run) = &result.source_run {
            store::validate_identity(run, "task source Run")?;
        } else if result.outcome != "blocked" || !result.files.is_empty() {
            return Err(SessionError::Invalid(
                "only a blocked task without files may have no source Run".into(),
            ));
        }
        validate_channel_value(&result.outcome, "task outcome", MAX_CHANNEL_VALUE_BYTES)?;
        let mut names = std::collections::BTreeSet::new();
        for file in &result.files {
            super::super::validate_file_name(&file.name)?;
            store::validate_identity(&file.artifact_id, "task Artifact id")?;
            if !names.insert(&file.name)
                || file.path.is_empty()
                || file.path.contains('\\')
                || file.path.chars().any(char::is_control)
                || !std::path::Path::new(&file.path)
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            {
                return Err(SessionError::Invalid(
                    "invalid task result file selection".into(),
                ));
            }
        }
        let encoded = serde_json::to_value(result)?;
        if serde_json::to_vec(&encoded)?.len() > 262_144 {
            return Err(SessionError::Invalid(
                "task result metadata is too large".into(),
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        if let Some(existing) = read_result(&transaction, session, &result.id)? {
            if existing != *result {
                return Err(SessionError::Conflict(
                    "task result identity changed".into(),
                ));
            }
        } else {
            touch_channel_session(
                &transaction,
                owner,
                &mut snapshot,
                QUEUED,
                serde_json::from_value(encoded)?,
                Utc::now(),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn pending_task_results(
        &self,
        owner: &str,
        session: &str,
    ) -> Result<Vec<TaskResult>, SessionError> {
        store::validate_lookup(owner, session)?;
        let connection = self.lock()?;
        store::read_session(&connection, owner, session)?;
        ensure_channel_session(&connection, owner, session)?;
        Self::pending_results(&connection, session)
    }

    /// Delivery identity remains queryable after handling; callers do not need
    /// another enqueue marker beside the Session's authoritative result fact.
    pub fn task_result(
        &self,
        owner: &str,
        session: &str,
        id: &str,
    ) -> Result<Option<TaskResult>, SessionError> {
        store::validate_lookup(owner, session)?;
        store::validate_identity(id, "task result id")?;
        let connection = self.lock()?;
        store::read_session(&connection, owner, session)?;
        ensure_channel_session(&connection, owner, session)?;
        read_result(&connection, session, id)
    }

    pub fn get_assistant_input(
        &self,
        owner: &str,
        session: &str,
        run: &str,
        key: &str,
    ) -> Result<Option<AssistantInput>, SessionError> {
        validate_input_lookup(owner, session, run, key)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        require_assistant(&transaction, owner, session, run, BindingScope::Retained)?;
        let input = read_assistant_input(&transaction, session, run, key, true)?;
        transaction.commit()?;
        Ok(input)
    }

    /// User input wins over queued results. Existing keys always keep their
    /// original input, including interrupted claims and completed rounds.
    pub fn claim_assistant_input(
        &self,
        owner: &str,
        session: &str,
        run: &str,
        key: &str,
    ) -> Result<Option<AssistantInput>, SessionError> {
        self.claim_input(owner, session, run, key, true)
    }

    pub(super) fn claim_input(
        &self,
        owner: &str,
        session: &str,
        run: &str,
        key: &str,
        include_tasks: bool,
    ) -> Result<Option<AssistantInput>, SessionError> {
        validate_input_lookup(owner, session, run, key)?;
        let mut connection = self.lock()?;
        {
            let transaction = connection.transaction()?;
            require_assistant(&transaction, owner, session, run, BindingScope::Current)?;
            if let Some(input) =
                read_assistant_input(&transaction, session, run, key, include_tasks)?
            {
                transaction.commit()?;
                return Ok(Some(input));
            }
            let pending = read_pending_input(&transaction, session)?.is_some()
                || (include_tasks
                    && super::super::running_channel_turn(&transaction, session)?.is_none()
                    && !Self::pending_results(&transaction, session)?.is_empty());
            transaction.commit()?;
            if !pending {
                return Ok(None);
            }
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot =
            require_assistant(&transaction, owner, session, run, BindingScope::Current)?;
        if let Some(input) = read_assistant_input(&transaction, session, run, key, include_tasks)? {
            transaction.commit()?;
            return Ok(Some(input));
        }
        let input = if let Some(inbound) = read_pending_input(&transaction, session)? {
            Some(AssistantInput::User(Box::new(claim_user_input(
                &transaction,
                owner,
                &mut snapshot,
                run,
                key,
                &inbound,
            )?)))
        } else if include_tasks
            && super::super::running_channel_turn(&transaction, session)?.is_none()
        {
            if let Some(result) = Self::pending_results(&transaction, session)?
                .into_iter()
                .next()
            {
                // A new attempt has its own Turn, but never reruns the source Graph.
                let mut turn = turns::insert_running_turn(
                    &transaction,
                    owner,
                    &mut snapshot,
                    &format!("task-{}", uuid::Uuid::new_v4()),
                    None,
                )?;
                associations::associate_run(&transaction, owner, &mut snapshot, &mut turn, run)?;
                append_claim(
                    &transaction,
                    owner,
                    &mut snapshot,
                    &Claim {
                        id: result.id.clone(),
                        turn: turn.id.clone(),
                        run: run.into(),
                        key: key.into(),
                    },
                )?;
                Some(AssistantInput::Task(Box::new(TaskResultAdmission {
                    result,
                    turn,
                    run_id: run.into(),
                })))
            } else {
                None
            }
        } else {
            None
        };
        transaction.commit()?;
        Ok(input)
    }

    /// Called only after the reply node's durable completion was saved. Its
    /// retry closes the same Turn and ACK in one transaction.
    pub fn finish_task_result(
        &self,
        owner: &str,
        session: &str,
        run: &str,
        key: &str,
    ) -> Result<(), SessionError> {
        validate_input_lookup(owner, session, run, key)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot =
            require_assistant(&transaction, owner, session, run, BindingScope::Retained)?;
        let claim = read_claim(&transaction, session, run, key)?.ok_or(SessionError::Missing)?;
        let mut turn = turns::find_turn(&transaction, session, &claim.turn)?;
        if turn.status == TurnStatus::Running {
            if recoverable_task_run(&transaction, session, &claim.turn)?.as_deref() != Some(run) {
                return Err(SessionError::Conflict(
                    "task result claim was handed over to another assistant".into(),
                ));
            }
            turns::finish(
                &transaction,
                owner,
                &mut snapshot,
                &mut turn,
                TurnStatus::Completed,
                None,
            )?;
        } else if turn.status != TurnStatus::Completed {
            return Err(SessionError::Conflict(
                "interrupted task result cannot be acknowledged".into(),
            ));
        }
        let handled: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_events WHERE session_id = ?1 AND kind = ?2
             AND json_extract(data, '$.id') = ?3)",
            params![session, HANDLED, claim.id],
            |row| row.get(0),
        )?;
        if !handled {
            touch_channel_session(
                &transaction,
                owner,
                &mut snapshot,
                HANDLED,
                serde_json::from_value(serde_json::to_value(&claim)?)?,
                Utc::now(),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
    pub(in crate::channel) fn pending_results(
        connection: &Connection,
        session: &str,
    ) -> Result<Vec<TaskResult>, SessionError> {
        let mut statement = connection.prepare(
            "SELECT queued.data FROM session_events queued
         WHERE queued.session_id = ?1 AND queued.kind = ?2
           AND NOT EXISTS(SELECT 1 FROM session_events handled
             WHERE handled.session_id = queued.session_id AND handled.kind = ?3
               AND json_extract(handled.data, '$.id') = json_extract(queued.data, '$.id'))
         ORDER BY queued.seq",
        )?;
        let raw = statement
            .query_map(params![session, QUEUED, HANDLED], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|raw| serde_json::from_str(&raw).map_err(Into::into))
            .collect()
    }
}

fn read_result(
    connection: &Connection,
    session: &str,
    id: &str,
) -> Result<Option<TaskResult>, SessionError> {
    let raw: Option<String> = connection
        .query_row(
            "SELECT data FROM session_events WHERE session_id = ?1 AND kind = ?2
         AND json_extract(data, '$.id') = ?3 ORDER BY seq LIMIT 1",
            params![session, QUEUED, id],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
        .transpose()
}

fn read_claim(
    connection: &Connection,
    session: &str,
    run: &str,
    key: &str,
) -> Result<Option<Claim>, SessionError> {
    let raw: Option<String> = connection
        .query_row(
            "SELECT data FROM session_events WHERE session_id = ?1 AND kind = ?2
         AND json_extract(data, '$.run') = ?3 AND json_extract(data, '$.key') = ?4
         ORDER BY seq DESC LIMIT 1",
            params![session, CLAIMED, run, key],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
        .transpose()
}

fn read_assistant_input(
    connection: &Connection,
    session: &str,
    run: &str,
    key: &str,
    include_tasks: bool,
) -> Result<Option<AssistantInput>, SessionError> {
    if let Some(user) = read_input(connection, session, run, key)? {
        return Ok(Some(AssistantInput::User(Box::new(inbound_admission(
            connection, &user,
        )?))));
    }
    let Some(claim) = read_claim(connection, session, run, key)? else {
        return Ok(None);
    };
    if !include_tasks {
        return Err(SessionError::Conflict(
            "assistant wait key already belongs to a task result".into(),
        ));
    }
    let result = read_result(connection, session, &claim.id)?
        .ok_or_else(|| SessionError::Storage("claimed task result is missing".into()))?;
    Ok(Some(AssistantInput::Task(Box::new(TaskResultAdmission {
        result,
        turn: turns::find_turn(connection, session, &claim.turn)?,
        run_id: run.into(),
    }))))
}

fn append_claim(
    connection: &Connection,
    owner: &str,
    snapshot: &mut Session,
    claim: &Claim,
) -> Result<(), SessionError> {
    touch_channel_session(
        connection,
        owner,
        snapshot,
        CLAIMED,
        serde_json::from_value(serde_json::to_value(claim)?)?,
        Utc::now(),
    )
}

pub(super) fn handover_task_input(
    connection: &Connection,
    owner: &str,
    snapshot: &mut Session,
    from: &str,
    to: &str,
    key: &str,
) -> Result<(), SessionError> {
    let raw: Option<String> = connection
        .query_row(
            "SELECT event.data FROM session_events event JOIN turns turn
           ON turn.session_id = event.session_id AND turn.id = json_extract(event.data, '$.turn')
         WHERE event.session_id = ?1 AND event.kind = ?2 AND json_extract(event.data, '$.run') = ?3
           AND turn.status = 'running' ORDER BY event.seq DESC LIMIT 1",
            params![snapshot.id, CLAIMED, from],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(raw) = raw {
        let mut claim: Claim = serde_json::from_str(&raw)?;
        claim.run = to.into();
        claim.key = key.into();
        let mut turn = turns::find_turn(connection, &snapshot.id, &claim.turn)?;
        associations::associate_run(connection, owner, snapshot, &mut turn, to)?;
        append_claim(connection, owner, snapshot, &claim)?;
    }
    Ok(())
}

pub(crate) fn is_task_turn(
    connection: &Connection,
    session: &str,
    turn: &str,
) -> Result<bool, SessionError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM session_events WHERE session_id = ?1 AND kind = ?2
         AND json_extract(data, '$.turn') = ?3)",
        params![session, CLAIMED, turn],
        |row| row.get(0),
    )?)
}

pub(crate) fn recoverable_task_run(
    connection: &Connection,
    session: &str,
    turn: &str,
) -> Result<Option<String>, SessionError> {
    Ok(connection.query_row(
        "SELECT assistant.run_id FROM session_events event JOIN channel_assistants assistant
           ON assistant.session_id = event.session_id AND assistant.run_id = json_extract(event.data, '$.run')
         WHERE event.session_id = ?1 AND event.kind = ?2 AND json_extract(event.data, '$.turn') = ?3
           AND assistant.retired = 0 ORDER BY event.seq DESC LIMIT 1",
        params![session, CLAIMED, turn], |row| row.get(0),
    ).optional()?)
}

#[cfg(test)]
mod tests;
