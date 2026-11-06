use super::{
    MAX_CHANNEL_VALUE_BYTES, MAX_INBOUND_ID_BYTES, StoredInbound, ensure_channel_session,
    inbound_admission, read_inbound_by_turn, read_inbound_row, record_run_association,
    touch_channel_session, validate_channel_value,
};
use crate::{
    ChannelAssistant, ChannelInboundAdmission, Session, SessionError, SessionStore, store, turns,
};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::json;
use std::collections::BTreeMap;

const MAX_WAIT_KEY_BYTES: usize = 4096;

mod task_inputs;
pub(crate) use task_inputs::{is_task_turn, recoverable_task_run};

enum BindingScope {
    Current,
    Retained,
}

impl SessionStore {
    pub fn bind_channel_assistant(
        &self,
        owner: &str,
        session: &str,
        run: &str,
        wait_node: &str,
        work_node: &str,
        reply_node: &str,
    ) -> Result<ChannelAssistant, SessionError> {
        validate_assistant_lookup(owner, session, run)?;
        for (node, label) in [
            (wait_node, "assistant wait node"),
            (work_node, "assistant work node"),
            (reply_node, "assistant reply node"),
        ] {
            validate_channel_value(node, label, MAX_CHANNEL_VALUE_BYTES)?;
        }
        let assistant = ChannelAssistant {
            session_id: session.into(),
            run_id: run.into(),
            wait_node: wait_node.into(),
            work_node: work_node.into(),
            reply_node: reply_node.into(),
        };
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        if !Self::selected_channel_session(&transaction, owner, session)? {
            return Err(SessionError::Conflict(
                "channel Session is superseded by its canonical account Session".into(),
            ));
        }
        if snapshot.reply_node != work_node {
            return Err(SessionError::Conflict(
                "assistant work node does not match the channel Session result node".into(),
            ));
        }
        ensure_run_session(&transaction, session, run)?;
        if let Some(existing) = read_assistant(&transaction, session)? {
            if existing != assistant {
                return Err(SessionError::Conflict(
                    "channel Session is already bound to a different assistant".into(),
                ));
            }
            transaction.commit()?;
            return Ok(existing);
        }
        if read_assistant_for_run(&transaction, session, run)?.is_some() {
            return Err(SessionError::Conflict(
                "retired assistant Run cannot be rebound".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO channel_assistants(session_id, run_id, wait_node, work_node, reply_node)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session, run, wait_node, work_node, reply_node],
        )?;
        if !snapshot.run_ids.iter().any(|existing| existing == run) {
            snapshot.run_ids.push(run.into());
            store::append_event(
                &transaction,
                &snapshot,
                "run.attached",
                BTreeMap::from([("run".into(), json!(run))]),
            )?;
        }
        touch_channel_session(
            &transaction,
            owner,
            &mut snapshot,
            "channel.assistant_bound",
            BTreeMap::from([
                ("run".into(), json!(run)),
                ("wait_node".into(), json!(wait_node)),
                ("work_node".into(), json!(work_node)),
                ("reply_node".into(), json!(reply_node)),
            ]),
            Utc::now(),
        )?;
        transaction.commit()?;
        Ok(assistant)
    }

    pub fn get_channel_assistant(
        &self,
        owner: &str,
        session: &str,
    ) -> Result<Option<ChannelAssistant>, SessionError> {
        store::validate_lookup(owner, session)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let assistant = read_assistant(&transaction, session)?;
        transaction.commit()?;
        Ok(assistant)
    }

    pub fn retire_channel_assistant(
        &self,
        owner: &str,
        session: &str,
        run: &str,
    ) -> Result<(), SessionError> {
        validate_assistant_lookup(owner, session, run)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot =
            require_assistant(&transaction, owner, session, run, BindingScope::Retained)?;
        let (_, retired) =
            read_assistant_for_run(&transaction, session, run)?.ok_or(SessionError::Missing)?;
        if retired {
            transaction.commit()?;
            return Ok(());
        }
        turns::ensure_no_running(&transaction, session)?;
        let unfinished: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM channel_deliveries
                           WHERE session_id = ?1 AND status IN ('pending', 'sending', 'unknown'))",
            [session],
            |row| row.get(0),
        )?;
        if unfinished {
            return Err(SessionError::Conflict(
                "channel Session has unfinished deliveries".into(),
            ));
        }
        transaction.execute(
            "UPDATE channel_assistants SET retired = 1 WHERE session_id = ?1 AND run_id = ?2 AND retired = 0",
            params![session, run],
        )?;
        touch_channel_session(
            &transaction,
            owner,
            &mut snapshot,
            "channel.assistant_retired",
            BTreeMap::from([("run".into(), json!(run))]),
            Utc::now(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Hand one assistant instance over to a new Run without reviving the old
    /// one.
    ///
    /// A stopped or crashed instance keeps its Session, its Goose history and
    /// its workspace; resuming means the old Run stays terminal and a new Run
    /// takes the instance over. Everything that makes that safe happens in one
    /// transaction: the old binding is retired, the new one becomes current, and
    /// a Turn that was claimed but never committed is handed to the new Run — so
    /// the message is neither lost nor replaced by the next one. Repeating the
    /// same committed from/to handover is a no-op while its target stays
    /// current; an explicitly retired source cannot initiate a new handover.
    pub fn handover_channel_assistant(
        &self,
        owner: &str,
        session: &str,
        from_run: &str,
        to_run: &str,
        to_wait_key: &str,
    ) -> Result<ChannelAssistant, SessionError> {
        validate_assistant_lookup(owner, session, from_run)?;
        validate_assistant_lookup(owner, session, to_run)?;
        validate_channel_value(to_wait_key, "assistant wait key", MAX_WAIT_KEY_BYTES)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !Self::selected_channel_session(&transaction, owner, session)? {
            return Err(SessionError::Conflict(
                "channel Session is superseded by its canonical account Session".into(),
            ));
        }
        let mut snapshot = require_assistant(
            &transaction,
            owner,
            session,
            from_run,
            BindingScope::Retained,
        )?;
        let (source, source_retired) = read_assistant_for_run(&transaction, session, from_run)?
            .ok_or_else(|| {
                SessionError::Conflict("channel Session has no bound assistant".into())
            })?;
        if let Some((current, retired)) = read_assistant_for_run(&transaction, session, to_run)? {
            if !retired {
                let committed: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM session_events
                      WHERE session_id = ?1 AND kind = 'channel.assistant_handover'
                        AND json_extract(data, '$.from') = ?2
                        AND json_extract(data, '$.to') = ?3)",
                    params![session, from_run, to_run],
                    |row| row.get(0),
                )?;
                if source_retired && committed {
                    transaction.commit()?;
                    return Ok(current);
                }
                return Err(SessionError::Conflict(
                    "current assistant is not the target of this handover".into(),
                ));
            }
            return Err(SessionError::Conflict(
                "a retired assistant Run cannot take the instance over".into(),
            ));
        }
        if source_retired {
            return Err(SessionError::Conflict(
                "only the current assistant can hand over the instance".into(),
            ));
        }
        ensure_run_session(&transaction, session, to_run)?;
        let retired = transaction.execute(
            "UPDATE channel_assistants SET retired = 1
              WHERE session_id = ?1 AND run_id = ?2 AND retired = 0",
            params![session, from_run],
        )?;
        if retired != 1 {
            return Err(SessionError::Conflict(
                "only the current assistant can hand over the instance".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO channel_assistants(session_id, run_id, wait_node, work_node, reply_node)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session,
                to_run,
                source.wait_node,
                source.work_node,
                source.reply_node
            ],
        )?;
        // The in-flight round travels with the instance: its Turn keeps running,
        // its input row now belongs to the new Run's wait invocation, and its
        // inbound names the new Run.
        transaction.execute(
            "UPDATE channel_assistant_inputs SET run_id = ?3, key = ?4
              WHERE session_id = ?1 AND run_id = ?2
                AND turn_id IN (SELECT id FROM turns WHERE session_id = ?1 AND status = 'running')",
            params![session, from_run, to_run, to_wait_key],
        )?;
        transaction.execute(
            "UPDATE channel_inbounds SET run_id = ?3
              WHERE session_id = ?1 AND run_id = ?2
                AND turn_id IN (SELECT id FROM turns WHERE session_id = ?1 AND status = 'running')",
            params![session, from_run, to_run],
        )?;
        let carried = transaction
            .query_row(
                "SELECT inbound.inbound_id, inbound.session_id, inbound.turn_id, inbound.run_id,
                        inbound.request, inbound.superseded_by_turn_id
                   FROM channel_inbounds inbound
                   JOIN channel_assistant_inputs input ON input.turn_id = inbound.turn_id
                   JOIN turns turn ON turn.id = inbound.turn_id AND turn.session_id = inbound.session_id
                  WHERE inbound.session_id = ?1 AND input.run_id = ?2 AND turn.status = 'running'",
                params![session, to_run],
                read_inbound_row,
            )
            .optional()?;
        if let Some(inbound) = carried {
            record_run_association(&transaction, owner, &mut snapshot, &inbound, to_run, true)?;
        }
        task_inputs::handover_task_input(
            &transaction,
            owner,
            &mut snapshot,
            from_run,
            to_run,
            to_wait_key,
        )?;
        touch_channel_session(
            &transaction,
            owner,
            &mut snapshot,
            "channel.assistant_handover",
            BTreeMap::from([
                ("from".into(), json!(from_run)),
                ("to".into(), json!(to_run)),
            ]),
            Utc::now(),
        )?;
        transaction.commit()?;
        Ok(ChannelAssistant {
            session_id: session.into(),
            run_id: to_run.into(),
            wait_node: source.wait_node,
            work_node: source.work_node,
            reply_node: source.reply_node,
        })
    }

    /// Every channel Session that still owns a live assistant instance.
    ///
    /// A restart has to find those instances from durable facts only: the
    /// binding table is the authority for "this Session has an assistant", and
    /// the owner column of the channel Session scopes each binding to its
    /// private owner. Retired instances are history, not instances.
    pub fn list_channel_assistants(&self) -> Result<Vec<(String, ChannelAssistant)>, SessionError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let assistants = {
            let mut statement = transaction.prepare(
                "SELECT session.owner, assistant.session_id, assistant.run_id,
                        assistant.wait_node, assistant.work_node, assistant.reply_node
                   FROM channel_assistants assistant
                   JOIN channel_sessions session ON session.session_id = assistant.session_id
                  WHERE assistant.retired = 0
                  ORDER BY assistant.session_id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        ChannelAssistant {
                            session_id: row.get(1)?,
                            run_id: row.get(2)?,
                            wait_node: row.get(3)?,
                            work_node: row.get(4)?,
                            reply_node: row.get(5)?,
                        },
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        transaction.commit()?;
        Ok(assistants)
    }

    /// Whether this Session already retired one specific assistant Run.
    ///
    /// A handover is the only producer of a retired binding that has a current
    /// successor, so this single fact distinguishes "the instance was handed
    /// over to the current Run" from "the Run merely reused older input".
    pub fn retired_channel_assistant(
        &self,
        owner: &str,
        session: &str,
        run: &str,
    ) -> Result<bool, SessionError> {
        validate_assistant_lookup(owner, session, run)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let retired: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM channel_assistants
                            WHERE session_id = ?1 AND run_id = ?2 AND retired = 1)",
            params![session, run],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(retired)
    }

    /// The instance this Session handed over to `run`, if any.
    ///
    /// A crash between the handover and the new Run's admission leaves the new
    /// Run current with no Run facts of its own. The retired row that was
    /// current immediately before it — the newest retired row older than the
    /// current one — is what a retry must admit as its predecessor.
    pub fn channel_assistant_predecessor(
        &self,
        owner: &str,
        session: &str,
        run: &str,
    ) -> Result<Option<ChannelAssistant>, SessionError> {
        validate_assistant_lookup(owner, session, run)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let predecessor = transaction
            .query_row(
                "SELECT session_id, run_id, wait_node, work_node, reply_node
                   FROM channel_assistants
                  WHERE session_id = ?1 AND retired = 1
                    AND rowid < COALESCE((SELECT rowid FROM channel_assistants
                                           WHERE session_id = ?1 AND run_id = ?2), 0)
                  ORDER BY rowid DESC LIMIT 1",
                params![session, run],
                read_assistant_row,
            )
            .optional()?;
        transaction.commit()?;
        Ok(predecessor)
    }

    /// Record the automatic recovery of one instance.
    ///
    /// This is deliberately a separate event from the manual handover so an
    /// operator can tell a restart-driven recovery from an explicit one. It is
    /// only accepted once the target really is the current binding and the
    /// source really was retired, so it can never describe a move that did not
    /// happen.
    pub fn record_channel_assistant_auto_resume(
        &self,
        owner: &str,
        session: &str,
        from_run: &str,
        to_run: &str,
    ) -> Result<(), SessionError> {
        validate_assistant_lookup(owner, session, from_run)?;
        validate_assistant_lookup(owner, session, to_run)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let (_, to_retired) =
            read_assistant_for_run(&transaction, session, to_run)?.ok_or(SessionError::Missing)?;
        if to_retired {
            return Err(SessionError::Conflict(
                "assistant auto resume target is not the current instance".into(),
            ));
        }
        let (_, from_retired) = read_assistant_for_run(&transaction, session, from_run)?
            .ok_or(SessionError::Missing)?;
        if !from_retired {
            return Err(SessionError::Conflict(
                "assistant auto resume source was never retired".into(),
            ));
        }
        touch_channel_session(
            &transaction,
            owner,
            &mut snapshot,
            "channel.assistant_auto_resume",
            BTreeMap::from([
                ("from".into(), json!(from_run)),
                ("to".into(), json!(to_run)),
            ]),
            Utc::now(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Record that a restart continued the current instance in its own Run.
    ///
    /// Same event as a restart-driven handover, with `from == to` and
    /// `in_place`, so every automatic recovery leaves one fact of one kind. It
    /// is only accepted while the Run is the current, unretired binding.
    pub fn record_channel_assistant_resumed_in_place(
        &self,
        owner: &str,
        session: &str,
        run: &str,
    ) -> Result<(), SessionError> {
        validate_assistant_lookup(owner, session, run)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let (_, retired) =
            read_assistant_for_run(&transaction, session, run)?.ok_or(SessionError::Missing)?;
        if retired {
            return Err(SessionError::Conflict(
                "assistant resumed in place is not the current instance".into(),
            ));
        }
        touch_channel_session(
            &transaction,
            owner,
            &mut snapshot,
            "channel.assistant_auto_resume",
            BTreeMap::from([
                ("from".into(), json!(run)),
                ("to".into(), json!(run)),
                ("in_place".into(), json!(true)),
            ]),
            Utc::now(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn pending_channel_assistant_inputs(
        &self,
        owner: &str,
        session: &str,
        run: &str,
        exclude_inbound: &str,
        limit: usize,
    ) -> Result<Vec<ChannelInboundAdmission>, SessionError> {
        validate_assistant_lookup(owner, session, run)?;
        validate_channel_value(exclude_inbound, "inbound id", MAX_INBOUND_ID_BYTES)?;
        let limit = i64::try_from(limit).map_err(|_| {
            SessionError::Invalid("assistant pending input limit is too large".into())
        })?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        require_assistant(&transaction, owner, session, run, BindingScope::Current)?;
        let excluded_created_at = transaction
            .query_row(
                "SELECT inbound.created_at FROM channel_inbounds inbound
                 JOIN turns turn ON turn.id = inbound.turn_id AND turn.session_id = inbound.session_id
                 WHERE inbound.session_id = ?1 AND inbound.inbound_id = ?2",
                params![session, exclude_inbound],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(SessionError::Missing)?;
        let inbounds = {
            let mut statement = transaction.prepare(
                "SELECT recent.inbound_id, recent.session_id, recent.turn_id, recent.run_id,
                        recent.request, recent.superseded_by_turn_id
                 FROM (
                     SELECT inbound.inbound_id, inbound.session_id, inbound.turn_id, inbound.run_id,
                            inbound.request, inbound.superseded_by_turn_id, inbound.created_at
                     FROM channel_inbounds inbound
                     JOIN turns turn ON turn.id = inbound.turn_id AND turn.session_id = inbound.session_id
                     WHERE inbound.session_id = ?1 AND inbound.inbound_id != ?2 AND turn.status = 'interrupted'
                       AND inbound.created_at < ?3
                       AND inbound.created_at > COALESCE((
                           SELECT MAX(previous.created_at) FROM channel_inbounds previous
                           JOIN channel_deliveries delivery ON delivery.turn_id = previous.turn_id AND delivery.session_id = previous.session_id
                           WHERE previous.session_id = ?1 AND delivery.status = 'confirmed'), '')
                     ORDER BY inbound.created_at DESC, inbound.inbound_id DESC LIMIT ?4
                 ) recent ORDER BY recent.created_at, recent.inbound_id",
            )?;
            statement
                .query_map(
                    params![session, exclude_inbound, excluded_created_at, limit],
                    read_inbound_row,
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let admissions = inbounds
            .iter()
            .map(|inbound| inbound_admission(&transaction, inbound))
            .collect::<Result<Vec<_>, _>>()?;
        transaction.commit()?;
        Ok(admissions)
    }

    pub fn claim_channel_assistant_input(
        &self,
        owner: &str,
        session: &str,
        run: &str,
        key: &str,
    ) -> Result<Option<ChannelInboundAdmission>, SessionError> {
        self.claim_input(owner, session, run, key, false)
            .map(|input| {
                input.and_then(|input| match input {
                    crate::AssistantInput::User(input) => Some(*input),
                    crate::AssistantInput::Task(_) => None,
                })
            })
    }

    /// The wait invocation that claimed one Turn, for read-only projections.
    ///
    /// A channel progress stream has to know which round an already admitted
    /// message belongs to. That association is already persisted here, so the
    /// projection reads it instead of guessing from the Run cursor.
    pub fn channel_assistant_wait_key(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
    ) -> Result<Option<String>, SessionError> {
        turns::validate_turn_lookup(owner, session, turn)?;
        let connection = self.lock()?;
        Ok(connection
            .query_row(
                "SELECT input.key
                   FROM channel_assistant_inputs input
                   JOIN channel_sessions session ON session.session_id = input.session_id
                  WHERE input.session_id = ?1 AND input.turn_id = ?2 AND session.owner = ?3",
                params![session, turn, owner],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn get_channel_assistant_input(
        &self,
        owner: &str,
        session: &str,
        run: &str,
        key: &str,
    ) -> Result<Option<ChannelInboundAdmission>, SessionError> {
        self.get_assistant_input(owner, session, run, key)
            .map(|input| {
                input.and_then(|input| match input {
                    crate::AssistantInput::User(input) => Some(*input),
                    crate::AssistantInput::Task(_) => None,
                })
            })
    }
}

fn claim_user_input(
    connection: &Connection,
    owner: &str,
    snapshot: &mut Session,
    run: &str,
    key: &str,
    inbound: &StoredInbound,
) -> Result<ChannelInboundAdmission, SessionError> {
    if inbound
        .run_id
        .as_deref()
        .is_some_and(|existing| existing != run)
    {
        return Err(SessionError::Conflict(
            "assistant input is already bound to a different Run".into(),
        ));
    }
    connection.execute(
        "INSERT INTO channel_assistant_inputs(session_id, run_id, key, turn_id) VALUES (?1, ?2, ?3, ?4)",
        params![snapshot.id, run, key, inbound.turn_id],
    )?;
    let newly_bound = inbound.run_id.is_none();
    if newly_bound {
        connection.execute(
            "UPDATE channel_inbounds SET run_id = ?3 WHERE session_id = ?1 AND turn_id = ?2",
            params![snapshot.id, inbound.turn_id, run],
        )?;
    }
    record_run_association(connection, owner, snapshot, inbound, run, newly_bound)?;
    let updated = read_inbound_by_turn(connection, &snapshot.id, &inbound.turn_id)?
        .ok_or(SessionError::Missing)?;
    inbound_admission(connection, &updated)
}

fn validate_assistant_lookup(owner: &str, session: &str, run: &str) -> Result<(), SessionError> {
    store::validate_lookup(owner, session)?;
    store::validate_identity(run, "run id")
}

fn validate_input_lookup(
    owner: &str,
    session: &str,
    run: &str,
    key: &str,
) -> Result<(), SessionError> {
    validate_assistant_lookup(owner, session, run)?;
    validate_channel_value(key, "assistant wait key", MAX_WAIT_KEY_BYTES)
}

fn read_assistant(
    connection: &Connection,
    session: &str,
) -> Result<Option<ChannelAssistant>, SessionError> {
    connection
        .query_row(
            "SELECT session_id, run_id, wait_node, work_node, reply_node FROM channel_assistants WHERE session_id = ?1 AND retired = 0",
            [session],
            read_assistant_row,
        )
        .optional()
        .map_err(Into::into)
}

fn read_assistant_for_run(
    connection: &Connection,
    session: &str,
    run: &str,
) -> Result<Option<(ChannelAssistant, bool)>, SessionError> {
    connection
        .query_row(
            "SELECT session_id, run_id, wait_node, work_node, reply_node, retired
         FROM channel_assistants WHERE session_id = ?1 AND run_id = ?2",
            params![session, run],
            |row| Ok((read_assistant_row(row)?, row.get(5)?)),
        )
        .optional()
        .map_err(Into::into)
}

fn read_assistant_row(row: &Row<'_>) -> rusqlite::Result<ChannelAssistant> {
    Ok(ChannelAssistant {
        session_id: row.get(0)?,
        run_id: row.get(1)?,
        wait_node: row.get(2)?,
        work_node: row.get(3)?,
        reply_node: row.get(4)?,
    })
}

fn require_assistant(
    connection: &Connection,
    owner: &str,
    session: &str,
    run: &str,
    scope: BindingScope,
) -> Result<Session, SessionError> {
    let snapshot = store::read_session(connection, owner, session)?;
    ensure_channel_session(connection, owner, session)?;
    let (assistant, retired) = read_assistant_for_run(connection, session, run)?
        .ok_or_else(|| SessionError::Conflict("channel Session has no bound assistant".into()))?;
    if matches!(scope, BindingScope::Current)
        && (retired || assistant.work_node != snapshot.reply_node)
    {
        return Err(SessionError::Conflict(
            "Run does not match the channel Session assistant".into(),
        ));
    }
    ensure_run_session(connection, session, run)?;
    Ok(snapshot)
}

pub(crate) fn ensure_assistant_run_scope(
    connection: &Connection,
    session: &str,
    run: &str,
) -> Result<(), SessionError> {
    let other: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM channel_assistants WHERE run_id = ?1 AND session_id != ?2)",
        params![run, session],
        |row| row.get(0),
    )?;
    if other {
        return Err(SessionError::Conflict(
            "Run is already bound to another channel Session assistant".into(),
        ));
    }
    Ok(())
}

fn ensure_run_session(
    connection: &Connection,
    session: &str,
    run: &str,
) -> Result<(), SessionError> {
    ensure_assistant_run_scope(connection, session, run)?;
    let other: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM channel_inbounds WHERE run_id = ?1 AND session_id != ?2)
             OR EXISTS(SELECT 1 FROM turn_runs relation JOIN turns turn ON turn.id = relation.turn_id
                       WHERE relation.run_id = ?1 AND turn.session_id != ?2)
             OR EXISTS(SELECT 1 FROM sessions snapshot, json_each(snapshot.data, '$.run_ids') attached
                       WHERE snapshot.id != ?2 AND attached.value = ?1)",
        params![run, session],
        |row| row.get(0),
    )?;
    if other {
        return Err(SessionError::Conflict(
            "Run is already associated with another Session".into(),
        ));
    }
    Ok(())
}

fn read_pending_input(
    connection: &Connection,
    session: &str,
) -> Result<Option<StoredInbound>, SessionError> {
    connection
        .query_row(
            "SELECT inbound.inbound_id, inbound.session_id, inbound.turn_id, inbound.run_id,
                    inbound.request, inbound.superseded_by_turn_id
             FROM channel_inbounds inbound
             JOIN turns turn ON turn.id = inbound.turn_id AND turn.session_id = inbound.session_id
             WHERE inbound.session_id = ?1 AND turn.status = 'running'
               AND inbound.superseded_by_turn_id IS NULL
               AND NOT EXISTS(SELECT 1 FROM channel_assistant_inputs input WHERE input.turn_id = inbound.turn_id)
             ORDER BY inbound.created_at DESC, inbound.inbound_id DESC LIMIT 1",
            [session],
            read_inbound_row,
        )
        .optional()
        .map_err(Into::into)
}

fn read_input(
    connection: &Connection,
    session: &str,
    run: &str,
    key: &str,
) -> Result<Option<StoredInbound>, SessionError> {
    let turn = connection
        .query_row(
            "SELECT turn_id FROM channel_assistant_inputs WHERE session_id = ?1 AND run_id = ?2 AND key = ?3",
            params![session, run, key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(turn) = turn else {
        return Ok(None);
    };
    let inbound = read_inbound_by_turn(connection, session, &turn)?.ok_or_else(|| {
        SessionError::Storage("assistant input has no matching channel inbound".into())
    })?;
    if inbound.run_id.as_deref() != Some(run) {
        return Err(SessionError::Storage(
            "assistant input Run association is inconsistent".into(),
        ));
    }
    Ok(Some(inbound))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelIdentity, ChannelInboundRequest, TurnStatus};
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    fn inbound(conversation: &str, id: &str, text: &str) -> ChannelInboundRequest {
        ChannelInboundRequest {
            inbound_id: id.into(),
            identity: ChannelIdentity {
                conversation_kind: Default::default(),
                source: "wecom".into(),
                account: None,
                conversation_id: conversation.into(),
                sender_id: format!("user-{conversation}"),
            },
            graph: "fixture".into(),
            reply_node: "assistant".into(),
            text: Some(text.into()),
            attachments: Default::default(),
            run_id: None,
            replace_running: true,
        }
    }

    #[test]
    fn idle_assistant_input_checks_do_not_request_a_database_writer() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sessions.sqlite");
        let store = SessionStore::open(&path).unwrap();
        let admitted = store
            .admit_channel_inbound("owner", inbound("a", "one", "input"))
            .unwrap();
        let session = admitted.session.id;
        store
            .bind_channel_assistant("owner", &session, "run-a", "wait", "assistant", "reply")
            .unwrap();
        store
            .finish_turn(
                "owner",
                &session,
                &admitted.turn.id,
                TurnStatus::Completed,
                None,
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .busy_timeout(std::time::Duration::ZERO)
            .unwrap();
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let input = store.claim_channel_assistant_input("owner", &session, "run-a", "wait/2");
        let generic = store.claim_assistant_input("owner", &session, "run-a", "wait/2");
        writer.execute_batch("ROLLBACK").unwrap();
        assert!(matches!(input, Ok(None)), "{input:?}");
        assert!(matches!(generic, Ok(None)), "{generic:?}");
    }

    #[test]
    fn replaying_a_claimed_assistant_input_does_not_request_a_database_writer() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sessions.sqlite");
        let store = SessionStore::open(&path).unwrap();
        let admitted = store
            .admit_channel_inbound("owner", inbound("a", "one", "input"))
            .unwrap();
        let session = admitted.session.id;
        store
            .bind_channel_assistant("owner", &session, "run-a", "wait", "assistant", "reply")
            .unwrap();
        let claimed = store
            .claim_channel_assistant_input("owner", &session, "run-a", "wait/1")
            .unwrap()
            .unwrap();
        store
            .lock()
            .unwrap()
            .busy_timeout(std::time::Duration::ZERO)
            .unwrap();
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let replay = store.claim_channel_assistant_input("owner", &session, "run-a", "wait/1");
        let generic = store.claim_assistant_input("owner", &session, "run-a", "wait/1");
        writer.execute_batch("ROLLBACK").unwrap();
        let replay = replay.unwrap();
        assert_eq!(replay.unwrap().turn.id, claimed.turn.id);
        assert_eq!(generic.unwrap().unwrap().turn().id, claimed.turn.id);
    }

    #[test]
    fn assistant_claim_rechecks_input_key_and_binding_after_waiting_for_a_writer() {
        use std::{cell::RefCell, sync::mpsc, time::Duration};

        thread_local! {
            static WRITER_GATE: RefCell<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>> = const { RefCell::new(None) };
        }

        fn wait_for_writer(attempt: i32) -> bool {
            assert_eq!(attempt, 0);
            WRITER_GATE.with(|gate| {
                let (blocked, release) = gate.borrow_mut().take().unwrap();
                blocked.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(5)).unwrap();
            });
            true
        }

        for change in ["completed", "replaced", "same-key", "other-key", "handover"] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("sessions.sqlite");
            let store = SessionStore::open(&path).unwrap();
            let other = SessionStore::open(&path).unwrap();
            let admitted = store
                .admit_channel_inbound("owner", inbound("a", "one", "input"))
                .unwrap();
            let session = admitted.session.id;
            store
                .bind_channel_assistant("owner", &session, "run-a", "wait", "assistant", "reply")
                .unwrap();
            let writer = Connection::open(&path).unwrap();
            writer.execute_batch("BEGIN IMMEDIATE").unwrap();
            let (blocked, observed) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let worker_session = session.clone();
            let worker = thread::spawn(move || {
                WRITER_GATE.with(|gate| *gate.borrow_mut() = Some((blocked, released)));
                store
                    .lock()
                    .unwrap()
                    .busy_handler(Some(wait_for_writer))
                    .unwrap();
                store.claim_channel_assistant_input("owner", &worker_session, "run-a", "wait/1")
            });
            observed.recv_timeout(Duration::from_secs(5)).unwrap();
            writer.execute_batch("ROLLBACK").unwrap();
            let expected = match change {
                "completed" => {
                    other
                        .finish_turn(
                            "owner",
                            &session,
                            &admitted.turn.id,
                            TurnStatus::Completed,
                            None,
                        )
                        .unwrap();
                    None
                }
                "replaced" => Some(
                    other
                        .admit_channel_inbound("owner", inbound("a", "two", "replacement"))
                        .unwrap()
                        .turn
                        .id,
                ),
                "same-key" | "other-key" => {
                    let key = if change == "same-key" {
                        "wait/1"
                    } else {
                        "wait/other"
                    };
                    other
                        .claim_channel_assistant_input("owner", &session, "run-a", key)
                        .unwrap()
                        .unwrap();
                    (change == "same-key").then_some(admitted.turn.id.clone())
                }
                "handover" => {
                    other
                        .handover_channel_assistant("owner", &session, "run-a", "run-b", "wait/new")
                        .unwrap();
                    None
                }
                _ => unreachable!(),
            };
            let events = other.events("owner", &session, 0).unwrap().len();
            release.send(()).unwrap();
            let result = worker.join().unwrap();
            if change == "handover" {
                assert!(
                    matches!(result, Err(SessionError::Conflict(_))),
                    "{result:?}"
                );
            } else {
                assert_eq!(
                    result.unwrap().map(|input| input.turn.id),
                    expected,
                    "{change}"
                );
            }
            if change != "replaced" {
                assert_eq!(
                    other.events("owner", &session, 0).unwrap().len(),
                    events,
                    "{change}"
                );
            }
        }
    }

    fn retire_and_handover_in_order(retire_first: bool) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sessions.sqlite");
        let first_store = SessionStore::open(&path).unwrap();
        let second_store = SessionStore::open(&path).unwrap();
        let first = first_store
            .admit_channel_inbound("owner", inbound("a", "one", "input"))
            .unwrap();
        let session = first.session.id;
        first_store
            .bind_channel_assistant("owner", &session, "run-a", "wait", "assistant", "reply")
            .unwrap();
        first_store
            .finish_turn("owner", &session, &first.turn.id, TurnStatus::Stopped, None)
            .unwrap();
        let gate = second_store.lock().unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = barrier.clone();
        let worker_store = second_store.clone();
        let worker_session = session.clone();
        let worker = thread::spawn(move || {
            worker_barrier.wait();
            if retire_first {
                worker_store
                    .handover_channel_assistant(
                        "owner",
                        &worker_session,
                        "run-a",
                        "run-b",
                        "wait/b",
                    )
                    .map(|_| ())
            } else {
                worker_store.retire_channel_assistant("owner", &worker_session, "run-a")
            }
        });
        barrier.wait();
        let first_result = if retire_first {
            first_store.retire_channel_assistant("owner", &session, "run-a")
        } else {
            first_store
                .handover_channel_assistant("owner", &session, "run-a", "run-b", "wait/b")
                .map(|_| ())
        };
        drop(gate);
        let second_result = worker.join().unwrap();
        first_result.unwrap();
        if retire_first {
            assert!(matches!(second_result, Err(SessionError::Conflict(_))));
            assert_eq!(
                first_store
                    .get_channel_assistant("owner", &session)
                    .unwrap(),
                None
            );
        } else {
            second_result.unwrap();
            assert_eq!(
                first_store
                    .get_channel_assistant("owner", &session)
                    .unwrap()
                    .unwrap()
                    .run_id,
                "run-b"
            );
            assert_eq!(
                first_store
                    .handover_channel_assistant("owner", &session, "run-a", "run-b", "wait/b")
                    .unwrap()
                    .run_id,
                "run-b"
            );
        }
        let events = first_store.events("owner", &session, 0).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "channel.assistant_handover")
                .count(),
            usize::from(!retire_first)
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "channel.assistant_retired")
                .count(),
            usize::from(retire_first)
        );
        let connection = first_store.lock().unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM channel_assistants", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, if retire_first { 1 } else { 2 });
    }

    #[test]
    fn retirement_commits_before_a_concurrent_handover() {
        retire_and_handover_in_order(true);
    }

    #[test]
    fn handover_commits_before_a_concurrent_retirement() {
        retire_and_handover_in_order(false);
    }

    #[test]
    fn handover_successors_are_listed_and_distinguishable_from_retired_history() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path().join("sessions.sqlite")).unwrap();
        let owner = "local";
        let first = store
            .admit_channel_inbound(owner, inbound("a", "one", "第一条"))
            .unwrap();
        let session = first.session.id.clone();
        assert!(store.list_channel_assistants().unwrap().is_empty());
        store
            .bind_channel_assistant(
                owner,
                &session,
                "assistant-old",
                "wait_input",
                "assistant",
                "reply",
            )
            .unwrap();
        let listed = store.list_channel_assistants().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, owner);
        assert_eq!(listed[0].1.run_id, "assistant-old");
        assert_eq!(listed[0].1.work_node, "assistant");
        assert!(
            !store
                .retired_channel_assistant(owner, &session, "assistant-old")
                .unwrap()
        );
        assert!(
            store
                .channel_assistant_predecessor(owner, &session, "assistant-old")
                .unwrap()
                .is_none()
        );

        // A handover retires the source and makes the target current, without
        // pretending the source never existed.
        store
            .handover_channel_assistant(
                owner,
                &session,
                "assistant-old",
                "assistant-new",
                "new-key",
            )
            .unwrap();
        let listed = store.list_channel_assistants().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.run_id, "assistant-new");
        assert!(
            store
                .retired_channel_assistant(owner, &session, "assistant-old")
                .unwrap()
        );
        assert_eq!(
            store
                .channel_assistant_predecessor(owner, &session, "assistant-new")
                .unwrap()
                .unwrap()
                .run_id,
            "assistant-old"
        );
        assert!(
            store
                .channel_assistant_predecessor(owner, &session, "assistant-old")
                .unwrap()
                .is_none()
        );

        // The automatic event only describes a move that really happened.
        assert!(
            store
                .record_channel_assistant_auto_resume(
                    owner,
                    &session,
                    "assistant-new",
                    "assistant-old"
                )
                .is_err()
        );
        assert!(
            store
                .record_channel_assistant_auto_resume(
                    owner,
                    &session,
                    "assistant-missing",
                    "assistant-new"
                )
                .is_err()
        );
        store
            .record_channel_assistant_auto_resume(owner, &session, "assistant-old", "assistant-new")
            .unwrap();
        let events = store.events(owner, &session, 0).unwrap();
        let resumed = events
            .iter()
            .filter(|event| event.kind == "channel.assistant_auto_resume")
            .collect::<Vec<_>>();
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].data["from"], json!("assistant-old"));
        assert_eq!(resumed[0].data["to"], json!("assistant-new"));

        // An in-place resume names only the current instance.
        assert!(
            store
                .record_channel_assistant_resumed_in_place(owner, &session, "assistant-old")
                .is_err()
        );
        store
            .record_channel_assistant_resumed_in_place(owner, &session, "assistant-new")
            .unwrap();
        let in_place = store
            .events(owner, &session, 0)
            .unwrap()
            .into_iter()
            .rfind(|event| event.kind == "channel.assistant_auto_resume")
            .unwrap();
        assert_eq!(
            in_place.data,
            BTreeMap::from([
                ("from".into(), json!("assistant-new")),
                ("to".into(), json!("assistant-new")),
                ("in_place".into(), json!(true)),
            ])
        );

        // A second handover chains the predecessors in binding order.
        store
            .handover_channel_assistant(
                owner,
                &session,
                "assistant-new",
                "assistant-later",
                "later-key",
            )
            .unwrap();
        assert_eq!(
            store
                .channel_assistant_predecessor(owner, &session, "assistant-later")
                .unwrap()
                .unwrap()
                .run_id,
            "assistant-new"
        );
        assert_eq!(
            store
                .channel_assistant_predecessor(owner, &session, "assistant-new")
                .unwrap()
                .unwrap()
                .run_id,
            "assistant-old"
        );

        // Another owner's instance is listed with its own owner and stays out of
        // this Session's history.
        let other = store
            .admit_channel_inbound("another-owner", inbound("b", "other", "别的会话"))
            .unwrap();
        store
            .bind_channel_assistant(
                "another-owner",
                &other.session.id,
                "assistant-other",
                "wait_input",
                "assistant",
                "reply",
            )
            .unwrap();
        let listed = store.list_channel_assistants().unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|(listed_owner, assistant)| {
            listed_owner == "another-owner" && assistant.session_id == other.session.id
        }));
        assert!(
            !store
                .retired_channel_assistant(owner, &session, "assistant-other")
                .unwrap()
        );
        assert!(
            store
                .channel_assistant_predecessor(owner, &session, "assistant-other")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .record_channel_assistant_auto_resume(
                    owner,
                    &session,
                    "assistant-new",
                    "assistant-other"
                )
                .is_err()
        );
    }
}
