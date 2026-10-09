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
    /// same handover is a no-op.
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
        let mut snapshot = require_assistant(
            &transaction,
            owner,
            session,
            from_run,
            BindingScope::Retained,
        )?;
        let (source, _) =
            read_assistant_for_run(&transaction, session, from_run)?.ok_or_else(|| {
                SessionError::Conflict("channel Session has no bound assistant".into())
            })?;
        if let Some((current, retired)) = read_assistant_for_run(&transaction, session, to_run)? {
            if !retired {
                // The handover already happened; the same target stays current.
                transaction.commit()?;
                return Ok(current);
            }
            return Err(SessionError::Conflict(
                "a retired assistant Run cannot take the instance over".into(),
            ));
        }
        ensure_run_session(&transaction, session, to_run)?;
        transaction.execute(
            "UPDATE channel_assistants SET retired = 1
              WHERE session_id = ?1 AND run_id = ?2 AND retired = 0",
            params![session, from_run],
        )?;
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
        validate_input_lookup(owner, session, run, key)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot =
            require_assistant(&transaction, owner, session, run, BindingScope::Current)?;
        if let Some(inbound) = read_input(&transaction, session, run, key)? {
            let admission = inbound_admission(&transaction, &inbound)?;
            transaction.commit()?;
            return Ok(Some(admission));
        }
        let inbound = transaction
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
            .optional()?;
        let Some(inbound) = inbound else {
            transaction.commit()?;
            return Ok(None);
        };
        if inbound
            .run_id
            .as_deref()
            .is_some_and(|existing| existing != run)
        {
            return Err(SessionError::Conflict(
                "assistant input is already bound to a different Run".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO channel_assistant_inputs(session_id, run_id, key, turn_id) VALUES (?1, ?2, ?3, ?4)",
            params![session, run, key, inbound.turn_id],
        )?;
        let newly_bound = inbound.run_id.is_none();
        if newly_bound {
            transaction.execute(
                "UPDATE channel_inbounds SET run_id = ?3 WHERE session_id = ?1 AND turn_id = ?2",
                params![session, inbound.turn_id, run],
            )?;
        }
        record_run_association(
            &transaction,
            owner,
            &mut snapshot,
            &inbound,
            run,
            newly_bound,
        )?;
        let updated = read_inbound_by_turn(&transaction, session, &inbound.turn_id)?
            .ok_or(SessionError::Missing)?;
        let admission = inbound_admission(&transaction, &updated)?;
        transaction.commit()?;
        Ok(Some(admission))
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
        validate_input_lookup(owner, session, run, key)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        require_assistant(&transaction, owner, session, run, BindingScope::Retained)?;
        let admission = read_input(&transaction, session, run, key)?
            .as_ref()
            .map(|inbound| inbound_admission(&transaction, inbound))
            .transpose()?;
        transaction.commit()?;
        Ok(admission)
    }
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
    use crate::{ChannelIdentity, ChannelInboundRequest};

    fn inbound(conversation: &str, id: &str, text: &str) -> ChannelInboundRequest {
        ChannelInboundRequest {
            inbound_id: id.into(),
            identity: ChannelIdentity {
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
    fn handover_moves_the_instance_and_its_in_flight_round() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path().join("sessions.sqlite")).unwrap();
        let owner = "local";
        let first = store
            .admit_channel_inbound(owner, inbound("a", "one", "第一条"))
            .unwrap();
        let session = first.session.id.clone();
        let old_run = "assistant-old";
        store
            .bind_channel_assistant(owner, &session, old_run, "wait_input", "assistant", "reply")
            .unwrap();
        // The crashed window: this round was claimed by the old Run and never
        // committed.
        let old_key = format!("{old_run}:digest:wait_input:1");
        let claimed = store
            .claim_channel_assistant_input(owner, &session, old_run, &old_key)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.relation.turn_id, first.turn.id);

        // A second Session keeps its own instance.
        let other = store
            .admit_channel_inbound(owner, inbound("b", "other", "别的会话"))
            .unwrap();
        store
            .bind_channel_assistant(
                owner,
                &other.session.id,
                "assistant-other",
                "wait_input",
                "assistant",
                "reply",
            )
            .unwrap();

        let new_run = "assistant-new";
        let new_key = format!("{new_run}:digest:wait_input:1");
        let binding = store
            .handover_channel_assistant(owner, &session, old_run, new_run, &new_key)
            .unwrap();
        assert_eq!(binding.run_id, new_run);
        assert_eq!(
            store
                .get_channel_assistant(owner, &session)
                .unwrap()
                .unwrap()
                .run_id,
            new_run
        );
        // The new Run retrieves the same Turn — no message lost, no next message
        // consumed.
        let retrieved = store
            .claim_channel_assistant_input(owner, &session, new_run, &new_key)
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.relation.turn_id, first.turn.id);
        assert_eq!(retrieved.relation.inbound_id, "one");
        assert_eq!(retrieved.relation.run_id.as_deref(), Some(new_run));
        // The retired Run can no longer claim, and the same handover is a no-op.
        assert!(
            store
                .claim_channel_assistant_input(owner, &session, old_run, &old_key)
                .is_err()
        );
        let again = store
            .handover_channel_assistant(owner, &session, old_run, new_run, &new_key)
            .unwrap();
        assert_eq!(again.run_id, new_run);
        // The other Session is untouched.
        assert_eq!(
            store
                .get_channel_assistant(owner, &other.session.id)
                .unwrap()
                .unwrap()
                .run_id,
            "assistant-other"
        );
        assert_eq!(
            store
                .claim_channel_assistant_input(owner, &other.session.id, "assistant-other", "k")
                .unwrap()
                .unwrap()
                .relation
                .inbound_id,
            "other"
        );
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
