use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    AttachmentManifest, ChannelAdmission, ChannelDelivery, ChannelDeliveryRequest,
    ChannelDeliveryStatus, ChannelIdentity, ChannelInboundRelation, ChannelInboundRequest,
    ChannelInboundRun, ChannelPendingMessage, Session, SessionError, SessionStatus, SessionStore,
    Turn, TurnStatus, associations, store, turns,
};

mod assistant;

pub(crate) use assistant::ensure_assistant_run_scope;

const MAX_CHANNEL_VALUE_BYTES: usize = 256;
const MAX_INBOUND_ID_BYTES: usize = 128;
const MAX_CHANNEL_TEXT_BYTES: usize = 100_000;
const MAX_ATTACHMENTS: usize = 16;
const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_ATTACHMENT_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_DELIVERY_KEY_BYTES: usize = 500;
const MAX_DELIVERY_KIND_BYTES: usize = 128;
const MAX_DELIVERY_ERROR_BYTES: usize = 4 * 1024;

type IdentityParts = (String, String, String, String);

impl SessionStore {
    pub fn admit_channel_inbound(
        &self,
        owner: &str,
        request: ChannelInboundRequest,
    ) -> Result<ChannelAdmission, SessionError> {
        store::validate_identity(owner, "owner")?;
        validate_channel_request(&request)?;
        let identity = identity_parts(&request.identity)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = match find_channel_session(&transaction, owner, &identity)? {
            Some(session_id) => {
                let session = store::read_session(&transaction, owner, &session_id)?;
                verify_channel_session(&session, &request, &identity)?;
                session
            }
            None => create_channel_session(&transaction, owner, &request, &identity)?,
        };

        if let Some(existing) = read_inbound(&transaction, &session.id, &request.inbound_id)? {
            let saved: ChannelInboundRequest = serde_json::from_str(&existing.request)?;
            if immutable_request(&saved) != immutable_request(&request) {
                return Err(SessionError::Conflict(
                    "inbound_id was already used for different channel input".into(),
                ));
            }
            if let Some(run_id) = request.run_id.as_deref() {
                associate_run(&transaction, owner, &mut session, &existing, run_id)?;
            }
            let turn = turns::find_turn(&transaction, &session.id, &existing.turn_id)?;
            let existing = read_inbound(&transaction, &session.id, &request.inbound_id)?
                .ok_or(SessionError::Missing)?;
            let relation = relation(&existing);
            transaction.commit()?;
            return Ok(ChannelAdmission {
                session,
                turn,
                relation,
            });
        }

        let previous = running_channel_turn(&transaction, &session.id)?;
        if previous.is_some() && !request.replace_running {
            return Err(SessionError::Conflict(
                "channel Session already has a running Turn".into(),
            ));
        }
        let previous = if let Some(mut previous) = previous {
            let previous_relation = read_inbound_by_turn(&transaction, &session.id, &previous.id)?
                .ok_or_else(|| {
                    SessionError::Storage(
                        "running channel Turn has no inbound relation; refusing replacement".into(),
                    )
                })?;
            turns::finish(
                &transaction,
                owner,
                &mut session,
                &mut previous,
                TurnStatus::Interrupted,
                Some("superseded by a newer channel message"),
            )?;
            Some(previous_relation)
        } else {
            None
        };

        let turn = turns::insert_running_turn(
            &transaction,
            owner,
            &mut session,
            &request.inbound_id,
            request.text.as_deref(),
        )?;
        let saved_request = immutable_request(&request);
        let encoded_request = serde_json::to_string(&saved_request)?;
        if encoded_request.len() > 262_144 {
            return Err(SessionError::Invalid(
                "channel inbound metadata is too large".into(),
            ));
        }
        let now = Utc::now();
        transaction.execute(
            "INSERT INTO channel_inbounds(
                session_id, inbound_id, turn_id, run_id, request,
                superseded_by_turn_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            params![
                session.id,
                request.inbound_id,
                turn.id,
                Option::<String>::None,
                encoded_request,
                timestamp(now),
            ],
        )?;
        if let Some(run_id) = request.run_id.as_deref() {
            let inbound = read_inbound(&transaction, &session.id, &request.inbound_id)?
                .ok_or_else(|| {
                    SessionError::Storage("channel inbound disappeared after admission".into())
                })?;
            associate_run(&transaction, owner, &mut session, &inbound, run_id)?;
        }
        if let Some(previous) = previous {
            transaction.execute(
                "UPDATE channel_inbounds SET superseded_by_turn_id = ?3
                 WHERE session_id = ?1 AND turn_id = ?2",
                params![session.id, previous.turn_id, turn.id],
            )?;
            suppress_replaced_deliveries(
                &transaction,
                owner,
                &mut session,
                &previous.turn_id,
                &turn.id,
            )?;
            touch_channel_session(
                &transaction,
                owner,
                &mut session,
                "channel.inbound.replaced",
                BTreeMap::from([
                    ("inbound".into(), json!(previous.inbound_id)),
                    ("old_turn".into(), json!(previous.turn_id)),
                    ("new_turn".into(), json!(turn.id)),
                ]),
                Utc::now(),
            )?;
        }
        touch_channel_session(
            &transaction,
            owner,
            &mut session,
            "channel.inbound.admitted",
            BTreeMap::from([
                ("inbound".into(), json!(request.inbound_id)),
                ("turn".into(), json!(turn.id)),
                ("run".into(), json!(request.run_id)),
                ("attachments".into(), json!(request.attachments.files.len())),
            ]),
            Utc::now(),
        )?;
        let relation = ChannelInboundRelation {
            inbound_id: request.inbound_id,
            session_id: session.id.clone(),
            turn_id: turn.id.clone(),
            run_id: request.run_id,
            superseded_by_turn_id: None,
        };
        transaction.commit()?;
        Ok(ChannelAdmission {
            session,
            turn,
            relation,
        })
    }

    /// Which channel session, turn and Run an admitted inbound message belongs
    /// to. Read-only: the live progress projection resolves its subject through
    /// this lookup instead of trusting a caller-supplied Run.
    pub fn channel_run_for_inbound(
        &self,
        owner: &str,
        inbound_id: &str,
    ) -> Result<Option<ChannelInboundRun>, SessionError> {
        store::validate_identity(owner, "owner")?;
        validate_channel_value(inbound_id, "inbound id", MAX_INBOUND_ID_BYTES)?;
        let connection = self.lock()?;
        let run = connection
            .query_row(
                "SELECT inbound.session_id, inbound.turn_id, inbound.run_id
                   FROM channel_inbounds inbound
                   JOIN channel_sessions session ON session.session_id = inbound.session_id
                  WHERE inbound.inbound_id = ?1 AND session.owner = ?2",
                params![inbound_id, owner],
                |row| {
                    Ok(ChannelInboundRun {
                        session_id: row.get(0)?,
                        turn_id: row.get(1)?,
                        run_id: row.get(2)?,
                    })
                },
            )
            .optional()?;
        Ok(run)
    }

    /// User messages this session has not been answered for: everything newer
    /// than the newest turn whose reply the platform confirmed. A new message
    /// folds these into its Run so interrupted work continues with the new
    /// information instead of disappearing.
    pub fn pending_channel_messages(
        &self,
        owner: &str,
        session: &str,
        exclude_inbound: &str,
        limit: usize,
    ) -> Result<Vec<ChannelPendingMessage>, SessionError> {
        validate_channel_lookup(owner, session, exclude_inbound)?;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT json_extract(request,'$.text'),
                    json_array_length(json_extract(request,'$.attachments.files'))
             FROM channel_inbounds
             WHERE session_id = ?1 AND inbound_id != ?2
               AND created_at > COALESCE((
                   SELECT MAX(previous.created_at) FROM channel_inbounds previous
                    JOIN channel_deliveries delivery ON delivery.turn_id = previous.turn_id
                   WHERE previous.session_id = ?1 AND delivery.status = 'confirmed'), '')
             ORDER BY created_at LIMIT ?3",
        )?;
        let rows = statement.query_map(params![session, exclude_inbound, limit as i64], |row| {
            Ok(ChannelPendingMessage {
                text: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                attachments: row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as usize,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn get_channel_relation(
        &self,
        owner: &str,
        session: &str,
        inbound_id: &str,
    ) -> Result<ChannelInboundRelation, SessionError> {
        validate_channel_lookup(owner, session, inbound_id)?;
        let connection = self.lock()?;
        ensure_channel_session(&connection, owner, session)?;
        let inbound =
            read_inbound(&connection, session, inbound_id)?.ok_or(SessionError::Missing)?;
        Ok(relation(&inbound))
    }

    pub fn get_channel_inbound(
        &self,
        owner: &str,
        session: &str,
        inbound_id: &str,
    ) -> Result<crate::ChannelInboundAdmission, SessionError> {
        validate_channel_lookup(owner, session, inbound_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let inbound =
            read_inbound(&transaction, session, inbound_id)?.ok_or(SessionError::Missing)?;
        let admission = inbound_admission(&transaction, &inbound)?;
        transaction.commit()?;
        Ok(admission)
    }

    pub fn associate_channel_run(
        &self,
        owner: &str,
        session: &str,
        inbound_id: &str,
        run_id: &str,
    ) -> Result<ChannelInboundRelation, SessionError> {
        validate_channel_lookup(owner, session, inbound_id)?;
        store::validate_identity(run_id, "run id")?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let inbound =
            read_inbound(&transaction, session, inbound_id)?.ok_or(SessionError::Missing)?;
        associate_run(&transaction, owner, &mut snapshot, &inbound, run_id)?;
        let updated =
            read_inbound(&transaction, session, inbound_id)?.ok_or(SessionError::Missing)?;
        transaction.commit()?;
        Ok(relation(&updated))
    }

    pub fn delete_channel_session(&self, owner: &str, session: &str) -> Result<(), SessionError> {
        store::validate_lookup(owner, session)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let current_assistant: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM channel_assistants WHERE session_id = ?1 AND retired = 0)",
            [session],
            |row| row.get(0),
        )?;
        if current_assistant {
            return Err(SessionError::Conflict(
                "channel Session has a current assistant; retire it before deletion".into(),
            ));
        }
        turns::ensure_no_running(&transaction, session)?;
        if !snapshot.run_ids.is_empty() {
            return Err(SessionError::Conflict(
                "channel Sessions with retained runs cannot be deleted".into(),
            ));
        }
        let unfinished: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM channel_deliveries
                 WHERE session_id = ?1
                   AND status NOT IN ('confirmed', 'failed', 'suppressed')
             )",
            [session],
            |row| row.get(0),
        )?;
        if unfinished {
            return Err(SessionError::Conflict(
                "channel Session has unfinished deliveries".into(),
            ));
        }
        transaction.execute(
            "DELETE FROM sessions WHERE owner = ?1 AND id = ?2",
            params![owner, session],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn admit_channel_delivery(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        request: ChannelDeliveryRequest,
    ) -> Result<ChannelDelivery, SessionError> {
        validate_delivery_request(&request)?;
        turns::validate_turn_lookup(owner, session, turn)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        ensure_channel_turn(&transaction, session, turn)?;
        if let Some(existing) = read_delivery(&transaction, session, &request.key)? {
            verify_delivery_request(&existing, turn, &request)?;
            transaction.commit()?;
            return Ok(existing);
        }
        let now = Utc::now();
        transaction.execute(
            "INSERT INTO channel_deliveries(
                session_id, key, turn_id, kind, content_sha256, status, error,
                superseded_by_turn_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', NULL, NULL, ?6, ?6)",
            params![
                session,
                request.key,
                turn,
                request.kind,
                request.content_sha256,
                timestamp(now),
            ],
        )?;
        touch_channel_session(
            &transaction,
            owner,
            &mut snapshot,
            "channel.delivery.pending",
            delivery_event_data(&request, turn, None),
            now,
        )?;
        let delivery = read_delivery(&transaction, session, &request.key)?.ok_or_else(|| {
            SessionError::Storage("channel delivery disappeared after admission".into())
        })?;
        transaction.commit()?;
        Ok(delivery)
    }

    pub fn admit_completed_channel_delivery(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        request: ChannelDeliveryRequest,
    ) -> Result<ChannelDelivery, SessionError> {
        validate_delivery_request(&request)?;
        turns::validate_turn_lookup(owner, session, turn)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        ensure_channel_turn(&transaction, session, turn)?;
        let inbound = read_inbound_by_turn(&transaction, session, turn)?.ok_or_else(|| {
            SessionError::Storage("completed channel Turn has no inbound relation".into())
        })?;
        if inbound.superseded_by_turn_id.is_some() {
            return Err(SessionError::Conflict(
                "superseded channel Turn cannot create a reply delivery".into(),
            ));
        }
        if turns::find_turn(&transaction, session, turn)?.status != TurnStatus::Completed {
            return Err(SessionError::Conflict(
                "channel reply delivery requires a completed Turn".into(),
            ));
        }
        if let Some(existing) = read_delivery(&transaction, session, &request.key)? {
            verify_delivery_request(&existing, turn, &request)?;
            transaction.commit()?;
            return Ok(existing);
        }
        let now = Utc::now();
        transaction.execute(
            "INSERT INTO channel_deliveries(
                session_id, key, turn_id, kind, content_sha256, status, error,
                superseded_by_turn_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', NULL, NULL, ?6, ?6)",
            params![
                session,
                request.key,
                turn,
                request.kind,
                request.content_sha256,
                timestamp(now),
            ],
        )?;
        touch_channel_session(
            &transaction,
            owner,
            &mut snapshot,
            "channel.delivery.pending",
            delivery_event_data(&request, turn, None),
            now,
        )?;
        let delivery = read_delivery(&transaction, session, &request.key)?.ok_or_else(|| {
            SessionError::Storage("channel delivery disappeared after admission".into())
        })?;
        transaction.commit()?;
        Ok(delivery)
    }

    pub fn begin_channel_delivery(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        request: ChannelDeliveryRequest,
    ) -> Result<ChannelDelivery, SessionError> {
        validate_delivery_request(&request)?;
        turns::validate_turn_lookup(owner, session, turn)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        ensure_channel_turn(&transaction, session, turn)?;
        let existing = read_delivery(&transaction, session, &request.key)?.ok_or_else(|| {
            SessionError::Conflict("channel delivery must be admitted before it is claimed".into())
        })?;
        verify_delivery_request(&existing, turn, &request)?;
        if matches!(
            existing.status,
            ChannelDeliveryStatus::Pending | ChannelDeliveryStatus::Failed
        ) {
            let now = Utc::now();
            transaction.execute(
                "UPDATE channel_deliveries SET status = 'sending', error = NULL, updated_at = ?3
                 WHERE session_id = ?1 AND key = ?2",
                params![session, request.key, timestamp(now)],
            )?;
            touch_channel_session(
                &transaction,
                owner,
                &mut snapshot,
                "channel.delivery.sending",
                delivery_event_data(&request, turn, None),
                now,
            )?;
        }
        let delivery = read_delivery(&transaction, session, &request.key)?.ok_or_else(|| {
            SessionError::Storage("channel delivery disappeared after claim".into())
        })?;
        transaction.commit()?;
        Ok(delivery)
    }

    pub fn settle_channel_delivery(
        &self,
        owner: &str,
        session: &str,
        request: ChannelDeliveryRequest,
        status: ChannelDeliveryStatus,
        error: Option<&str>,
    ) -> Result<ChannelDelivery, SessionError> {
        validate_delivery_request(&request)?;
        validate_delivery_settlement(status, error)?;
        store::validate_lookup(owner, session)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let existing =
            read_delivery(&transaction, session, &request.key)?.ok_or(SessionError::Missing)?;
        verify_delivery_request(&existing, &existing.turn_id, &request)?;
        let normalized_error = error.map(str::to_owned);
        if existing.status == status && existing.error == normalized_error {
            transaction.commit()?;
            return Ok(existing);
        }
        if existing.status.is_terminal() {
            return Err(SessionError::Conflict(
                "terminal channel delivery cannot be changed".into(),
            ));
        }
        if existing.status == ChannelDeliveryStatus::Unknown
            && status == ChannelDeliveryStatus::Suppressed
        {
            return Err(SessionError::Conflict(
                "unknown channel delivery must be reconciled before suppression".into(),
            ));
        }
        let now = Utc::now();
        transaction.execute(
            "UPDATE channel_deliveries SET status = ?3, error = ?4, updated_at = ?5
             WHERE session_id = ?1 AND key = ?2",
            params![
                session,
                request.key,
                status.as_str(),
                normalized_error,
                timestamp(now)
            ],
        )?;
        touch_channel_session(
            &transaction,
            owner,
            &mut snapshot,
            delivery_event_kind(status),
            delivery_event_data(&request, &existing.turn_id, error),
            now,
        )?;
        let delivery = read_delivery(&transaction, session, &request.key)?.ok_or_else(|| {
            SessionError::Storage("channel delivery disappeared after settlement".into())
        })?;
        transaction.commit()?;
        Ok(delivery)
    }

    pub fn settle_channel_delivery_from_gateway(
        &self,
        owner: &str,
        session: &str,
        inbound_id: &str,
        request: ChannelDeliveryRequest,
        status: ChannelDeliveryStatus,
    ) -> Result<ChannelDelivery, SessionError> {
        validate_delivery_request(&request)?;
        if !matches!(
            status,
            ChannelDeliveryStatus::Confirmed
                | ChannelDeliveryStatus::Unknown
                | ChannelDeliveryStatus::Suppressed
        ) {
            return Err(SessionError::Invalid(
                "gateway settlement has an unsupported status".into(),
            ));
        }
        let error = (status == ChannelDeliveryStatus::Unknown)
            .then_some("delivery outcome unknown at channel Gateway");
        validate_delivery_settlement(status, error)?;
        validate_channel_lookup(owner, session, inbound_id)?;

        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let inbound =
            read_inbound(&transaction, session, inbound_id)?.ok_or(SessionError::Missing)?;
        let existing =
            read_delivery(&transaction, session, &request.key)?.ok_or(SessionError::Missing)?;
        verify_delivery_request(&existing, &inbound.turn_id, &request)?;

        if existing.status == status {
            transaction.commit()?;
            return Ok(existing);
        }
        if existing.status.is_terminal() {
            return Err(SessionError::Conflict(
                "terminal channel delivery cannot be changed".into(),
            ));
        }
        let allowed = match status {
            ChannelDeliveryStatus::Confirmed => matches!(
                existing.status,
                ChannelDeliveryStatus::Sending | ChannelDeliveryStatus::Unknown
            ),
            ChannelDeliveryStatus::Unknown => existing.status == ChannelDeliveryStatus::Sending,
            ChannelDeliveryStatus::Suppressed => matches!(
                existing.status,
                ChannelDeliveryStatus::Sending | ChannelDeliveryStatus::Unknown
            ),
            ChannelDeliveryStatus::Pending
            | ChannelDeliveryStatus::Sending
            | ChannelDeliveryStatus::Failed => false,
        };
        if !allowed {
            return Err(SessionError::Conflict(
                "channel delivery is not in a settleable state".into(),
            ));
        }

        let now = Utc::now();
        transaction.execute(
            "UPDATE channel_deliveries SET status = ?3, error = ?4, updated_at = ?5
             WHERE session_id = ?1 AND key = ?2",
            params![session, request.key, status.as_str(), error, timestamp(now)],
        )?;
        touch_channel_session(
            &transaction,
            owner,
            &mut snapshot,
            delivery_event_kind(status),
            delivery_event_data(&request, &existing.turn_id, error),
            now,
        )?;
        let delivery = read_delivery(&transaction, session, &request.key)?.ok_or_else(|| {
            SessionError::Storage("channel delivery disappeared after settlement".into())
        })?;
        transaction.commit()?;
        Ok(delivery)
    }

    pub fn recover_channel_deliveries(&self) -> Result<usize, SessionError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let unfinished = {
            let mut statement = transaction.prepare(
                "SELECT channel_sessions.owner, channel_deliveries.session_id,
                        channel_deliveries.key, channel_deliveries.turn_id
                 FROM channel_deliveries
                 JOIN channel_sessions ON channel_sessions.session_id = channel_deliveries.session_id
                 WHERE channel_deliveries.status = 'sending'
                 ORDER BY channel_deliveries.session_id, channel_deliveries.key",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (owner, session, key, turn) in &unfinished {
            let mut snapshot = store::read_session(&transaction, owner, session)?;
            let now = Utc::now();
            transaction.execute(
                "UPDATE channel_deliveries
                 SET status = 'unknown', error = 'delivery outcome unknown after restart', updated_at = ?3
                 WHERE session_id = ?1 AND key = ?2 AND status = 'sending'",
                params![session, key, timestamp(now)],
            )?;
            touch_channel_session(
                &transaction,
                owner,
                &mut snapshot,
                "channel.delivery.unknown",
                BTreeMap::from([
                    ("key".into(), json!(key)),
                    ("turn".into(), json!(turn)),
                    ("reason".into(), json!("restart")),
                ]),
                now,
            )?;
        }
        transaction.commit()?;
        Ok(unfinished.len())
    }

    pub fn list_unfinished_channel_deliveries(
        &self,
        owner: &str,
        session: &str,
    ) -> Result<Vec<ChannelDelivery>, SessionError> {
        store::validate_lookup(owner, session)?;
        let connection = self.lock()?;
        ensure_channel_session(&connection, owner, session)?;
        let mut statement = connection.prepare(
            "SELECT session_id, key, turn_id, kind, content_sha256, status, error,
                    superseded_by_turn_id, created_at, updated_at
             FROM channel_deliveries
             WHERE session_id = ?1 AND status NOT IN ('confirmed', 'failed', 'suppressed')
             ORDER BY created_at, key",
        )?;
        statement
            .query_map([session], read_delivery_row)?
            .map(|row| {
                row.map_err(SessionError::from)
                    .and_then(|delivery| delivery)
            })
            .collect()
    }
}

fn validate_channel_request(request: &ChannelInboundRequest) -> Result<(), SessionError> {
    validate_channel_value(&request.inbound_id, "inbound id", MAX_INBOUND_ID_BYTES)?;
    let _ = identity_parts(&request.identity)?;
    validate_channel_value(&request.graph, "channel graph", MAX_CHANNEL_VALUE_BYTES)?;
    validate_channel_value(
        &request.reply_node,
        "channel reply node",
        MAX_CHANNEL_VALUE_BYTES,
    )?;
    if let Some(text) = request.text.as_deref()
        && (text.len() > MAX_CHANNEL_TEXT_BYTES || text.chars().any(|character| character == '\0'))
    {
        return Err(SessionError::Invalid(
            "channel text is too large or contains NUL".into(),
        ));
    }
    validate_manifest(&request.attachments)?;
    if request
        .text
        .as_deref()
        .is_none_or(|text| text.trim().is_empty())
        && request.attachments.files.is_empty()
    {
        return Err(SessionError::Invalid(
            "channel inbound requires text or an attachment".into(),
        ));
    }
    if let Some(run_id) = request.run_id.as_deref() {
        store::validate_identity(run_id, "run id")?;
    }
    Ok(())
}

fn validate_channel_value(value: &str, label: &str, limit: usize) -> Result<(), SessionError> {
    if value.trim().is_empty()
        || value.len() > limit
        || value.chars().any(char::is_control)
        || value == "."
        || value == ".."
    {
        return Err(SessionError::Invalid(format!("invalid {label}")));
    }
    Ok(())
}

fn identity_parts(identity: &ChannelIdentity) -> Result<IdentityParts, SessionError> {
    validate_channel_value(&identity.source, "channel source", MAX_CHANNEL_VALUE_BYTES)?;
    validate_channel_value(
        &identity.conversation_id,
        "channel conversation id",
        MAX_CHANNEL_VALUE_BYTES,
    )?;
    validate_channel_value(
        &identity.sender_id,
        "channel sender id",
        MAX_CHANNEL_VALUE_BYTES,
    )?;
    let account = match identity.account.as_deref() {
        Some(account) => {
            validate_channel_value(account, "channel account", MAX_CHANNEL_VALUE_BYTES)?;
            account.to_owned()
        }
        None => String::new(),
    };
    Ok((
        identity.source.clone(),
        account,
        identity.conversation_id.clone(),
        identity.sender_id.clone(),
    ))
}

fn validate_manifest(manifest: &AttachmentManifest) -> Result<(), SessionError> {
    if manifest.format != 1 || manifest.files.len() > MAX_ATTACHMENTS {
        return Err(SessionError::Invalid(
            "unsupported or oversized channel attachment manifest".into(),
        ));
    }
    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut total = 0u64;
    for file in &manifest.files {
        if file.name.is_empty()
            || file.name.len() > 200
            || file.name == "."
            || file.name == ".."
            || Path::new(&file.name)
                .file_name()
                .and_then(|name| name.to_str())
                != Some(file.name.as_str())
            || file.name.contains('\\')
            || file.name.chars().any(char::is_control)
            || !names.insert(file.name.clone())
        {
            return Err(SessionError::Invalid(
                "channel attachment names must be unique safe file names".into(),
            ));
        }
        if !file.path.starts_with('/')
            || file.path.chars().any(char::is_control)
            || Path::new(&file.path)
                .components()
                .any(|component| component == Component::ParentDir)
            || !paths.insert(file.path.clone())
        {
            return Err(SessionError::Invalid(
                "channel attachment paths must be absolute, unique and traversal-free".into(),
            ));
        }
        if file.size > MAX_ATTACHMENT_BYTES {
            return Err(SessionError::Invalid(
                "channel attachment exceeds the per-file limit".into(),
            ));
        }
        total = total
            .checked_add(file.size)
            .ok_or_else(|| SessionError::Invalid("channel attachment size overflow".into()))?;
        if file.sha256.len() != 64
            || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || file.sha256.bytes().any(|byte| byte.is_ascii_uppercase())
        {
            return Err(SessionError::Invalid(
                "channel attachment hash must be lowercase SHA-256 hex".into(),
            ));
        }
        if let Some(media_type) = file.media_type.as_deref() {
            validate_media_type(media_type)?;
        }
    }
    if total > MAX_ATTACHMENT_TOTAL_BYTES {
        return Err(SessionError::Invalid(
            "channel attachments exceed the aggregate size limit".into(),
        ));
    }
    let encoded = serde_json::to_vec(manifest)?;
    if encoded.len() > MAX_MANIFEST_BYTES {
        return Err(SessionError::Invalid(
            "channel attachment manifest is too large".into(),
        ));
    }
    Ok(())
}

fn validate_media_type(media_type: &str) -> Result<(), SessionError> {
    if media_type.len() > 128 || media_type.matches('/').count() != 1 {
        return Err(SessionError::Invalid(
            "channel attachment media type is invalid".into(),
        ));
    }
    if media_type.split('/').any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&byte))
    }) {
        return Err(SessionError::Invalid(
            "channel attachment media type is invalid".into(),
        ));
    }
    Ok(())
}

fn immutable_request(request: &ChannelInboundRequest) -> ChannelInboundRequest {
    let mut request = request.clone();
    request.run_id = None;
    request.replace_running = false;
    request
}

fn channel_map(identity: &ChannelIdentity) -> BTreeMap<String, String> {
    let mut channel = BTreeMap::from([
        ("conversation_id".into(), identity.conversation_id.clone()),
        ("sender_id".into(), identity.sender_id.clone()),
        ("source".into(), identity.source.clone()),
    ]);
    if let Some(account) = identity.account.as_deref() {
        channel.insert("account".into(), account.into());
    }
    channel
}

fn create_channel_session(
    connection: &Connection,
    owner: &str,
    request: &ChannelInboundRequest,
    identity: &IdentityParts,
) -> Result<Session, SessionError> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let session = Session {
        id: id.clone(),
        conversation_id: id.clone(),
        title: String::new(),
        status: SessionStatus::Active,
        waiting_reason: String::new(),
        run_ids: Vec::new(),
        graph: request.graph.clone(),
        reply_node: request.reply_node.clone(),
        channel: channel_map(&request.identity),
        approval: None,
        approvals: Vec::new(),
        questions: Vec::new(),
        operation: None,
        operations: BTreeMap::new(),
        created_at: now,
        updated_at: now,
    };
    connection.execute(
        "INSERT INTO sessions(id, owner, data, updated_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            session.id,
            owner,
            serde_json::to_string(&session)?,
            timestamp(now)
        ],
    )?;
    store::append_event(connection, &session, "session.created", BTreeMap::new())?;
    connection.execute(
        "INSERT INTO channel_sessions(
            session_id, owner, source, account, conversation_id, sender_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session.id, owner, identity.0, identity.1, identity.2, identity.3,
        ],
    )?;
    store::append_event(
        connection,
        &session,
        "channel.bound",
        BTreeMap::from([("channel".into(), json!(&session.channel))]),
    )?;
    Ok(session)
}

fn find_channel_session(
    connection: &Connection,
    owner: &str,
    identity: &IdentityParts,
) -> Result<Option<String>, SessionError> {
    connection
        .query_row(
            "SELECT session_id FROM channel_sessions
             WHERE owner = ?1 AND source = ?2 AND account = ?3
               AND conversation_id = ?4 AND sender_id = ?5",
            params![owner, identity.0, identity.1, identity.2, identity.3],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn verify_channel_session(
    session: &Session,
    request: &ChannelInboundRequest,
    identity: &IdentityParts,
) -> Result<(), SessionError> {
    if session.graph != request.graph || session.reply_node != request.reply_node {
        return Err(SessionError::Conflict(
            "channel identity is already bound to a different Graph or reply node".into(),
        ));
    }
    if session.channel != channel_map(&request.identity) {
        return Err(SessionError::Storage(
            "channel Session identity projection is inconsistent".into(),
        ));
    }
    if identity.0.is_empty() || identity.2.is_empty() || identity.3.is_empty() {
        return Err(SessionError::Storage(
            "channel Session identity is incomplete".into(),
        ));
    }
    Ok(())
}

fn ensure_channel_session(
    connection: &Connection,
    owner: &str,
    session: &str,
) -> Result<(), SessionError> {
    let found: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM channel_sessions WHERE owner = ?1 AND session_id = ?2)",
        params![owner, session],
        |row| row.get(0),
    )?;
    if found {
        Ok(())
    } else {
        Err(SessionError::Conflict(
            "operation requires a channel Session".into(),
        ))
    }
}

fn validate_channel_lookup(
    owner: &str,
    session: &str,
    inbound_id: &str,
) -> Result<(), SessionError> {
    store::validate_lookup(owner, session)?;
    validate_channel_value(inbound_id, "inbound id", MAX_INBOUND_ID_BYTES)
}

struct StoredInbound {
    inbound_id: String,
    session_id: String,
    turn_id: String,
    run_id: Option<String>,
    request: String,
    superseded_by_turn_id: Option<String>,
}

fn read_inbound(
    connection: &Connection,
    session: &str,
    inbound_id: &str,
) -> Result<Option<StoredInbound>, SessionError> {
    connection
        .query_row(
            "SELECT inbound_id, session_id, turn_id, run_id, request, superseded_by_turn_id
             FROM channel_inbounds WHERE session_id = ?1 AND inbound_id = ?2",
            params![session, inbound_id],
            read_inbound_row,
        )
        .optional()
        .map_err(Into::into)
}

fn read_inbound_by_turn(
    connection: &Connection,
    session: &str,
    turn: &str,
) -> Result<Option<StoredInbound>, SessionError> {
    connection
        .query_row(
            "SELECT inbound_id, session_id, turn_id, run_id, request, superseded_by_turn_id
             FROM channel_inbounds WHERE session_id = ?1 AND turn_id = ?2",
            params![session, turn],
            read_inbound_row,
        )
        .optional()
        .map_err(Into::into)
}

fn read_inbound_row(row: &Row<'_>) -> rusqlite::Result<StoredInbound> {
    Ok(StoredInbound {
        inbound_id: row.get(0)?,
        session_id: row.get(1)?,
        turn_id: row.get(2)?,
        run_id: row.get(3)?,
        request: row.get(4)?,
        superseded_by_turn_id: row.get(5)?,
    })
}

fn relation(inbound: &StoredInbound) -> ChannelInboundRelation {
    ChannelInboundRelation {
        inbound_id: inbound.inbound_id.clone(),
        session_id: inbound.session_id.clone(),
        turn_id: inbound.turn_id.clone(),
        run_id: inbound.run_id.clone(),
        superseded_by_turn_id: inbound.superseded_by_turn_id.clone(),
    }
}

fn inbound_admission(
    connection: &Connection,
    inbound: &StoredInbound,
) -> Result<crate::ChannelInboundAdmission, SessionError> {
    let request = serde_json::from_str(&inbound.request)?;
    let turn = turns::find_turn(connection, &inbound.session_id, &inbound.turn_id)?;
    let previous_run = connection
        .query_row(
            "SELECT run_id FROM channel_inbounds
             WHERE session_id = ?1 AND turn_id != ?2 AND run_id IS NOT NULL
             ORDER BY created_at DESC, inbound_id DESC LIMIT 1",
            params![inbound.session_id, inbound.turn_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(crate::ChannelInboundAdmission {
        request,
        relation: relation(inbound),
        turn,
        previous_run,
    })
}

fn associate_run(
    connection: &Connection,
    owner: &str,
    snapshot: &mut Session,
    inbound: &StoredInbound,
    run_id: &str,
) -> Result<(), SessionError> {
    assistant::ensure_assistant_run_scope(connection, &snapshot.id, run_id)?;
    let newly_bound = match inbound.run_id.as_deref() {
        Some(existing) if existing == run_id => false,
        Some(_) => {
            return Err(SessionError::Conflict(
                "inbound relation is already bound to a different Run".into(),
            ));
        }
        None => {
            let claimed: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM channel_inbounds WHERE run_id = ?1)",
                [run_id],
                |row| row.get(0),
            )?;
            if claimed {
                return Err(SessionError::Conflict(
                    "Run is already bound to another channel inbound".into(),
                ));
            }
            connection.execute(
                "UPDATE channel_inbounds SET run_id = ?3
                 WHERE session_id = ?1 AND inbound_id = ?2 AND run_id IS NULL",
                params![inbound.session_id, inbound.inbound_id, run_id],
            )?;
            true
        }
    };
    let turn_claimed: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM turn_runs WHERE run_id = ?1 AND turn_id != ?2)",
        params![run_id, inbound.turn_id],
        |row| row.get(0),
    )?;
    if turn_claimed {
        return Err(SessionError::Conflict(
            "Run is already associated with another channel Turn".into(),
        ));
    }
    record_run_association(connection, owner, snapshot, inbound, run_id, newly_bound)
}

fn record_run_association(
    connection: &Connection,
    owner: &str,
    snapshot: &mut Session,
    inbound: &StoredInbound,
    run_id: &str,
    newly_bound: bool,
) -> Result<(), SessionError> {
    let turn_associated: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM turn_runs WHERE run_id = ?1 AND turn_id = ?2)",
        params![run_id, inbound.turn_id],
        |row| row.get(0),
    )?;
    if !turn_associated {
        let sequence: i64 = connection.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM turn_runs WHERE turn_id = ?1",
            [&inbound.turn_id],
            |row| row.get(0),
        )?;
        let sequence = sequence
            .checked_add(1)
            .ok_or_else(|| SessionError::Storage("Turn run sequence exhausted".into()))?;
        connection.execute(
            "INSERT INTO turn_runs(turn_id, run_id, seq) VALUES (?1, ?2, ?3)",
            params![inbound.turn_id, run_id, sequence],
        )?;
    }
    let session_attached = snapshot.run_ids.iter().any(|existing| existing == run_id);
    if !session_attached {
        snapshot.run_ids.push(run_id.into());
    }
    let now = Utc::now();
    if newly_bound || !turn_associated || !session_attached {
        snapshot.updated_at = now;
        store::write_session(connection, owner, snapshot)?;
        if newly_bound || !session_attached {
            store::append_event(
                connection,
                snapshot,
                "run.attached",
                BTreeMap::from([("run".into(), json!(run_id))]),
            )?;
        }
        if !turn_associated {
            store::append_event(
                connection,
                snapshot,
                "turn.run_associated",
                BTreeMap::from([
                    ("turn".into(), json!(&inbound.turn_id)),
                    ("run".into(), json!(run_id)),
                ]),
            )?;
        }
    }
    touch_channel_session(
        connection,
        owner,
        snapshot,
        "channel.run_associated",
        BTreeMap::from([
            ("inbound".into(), json!(inbound.inbound_id)),
            ("turn".into(), json!(inbound.turn_id)),
            ("run".into(), json!(run_id)),
        ]),
        now,
    )
}

fn running_channel_turn(
    connection: &Connection,
    session: &str,
) -> Result<Option<Turn>, SessionError> {
    let turn = connection
        .query_row(
            "SELECT id, session_id, request_id, prompt, status, error, created_at, updated_at
             FROM turns WHERE session_id = ?1 AND status = 'running'",
            [session],
            turns::read_turn,
        )
        .optional()?;
    match turn {
        Some(Ok(turn)) => associations::project_turn(connection, turn).map(Some),
        Some(Err(error)) => Err(error),
        None => Ok(None),
    }
}

fn ensure_channel_turn(
    connection: &Connection,
    session: &str,
    turn: &str,
) -> Result<(), SessionError> {
    if read_inbound_by_turn(connection, session, turn)?.is_some() {
        Ok(())
    } else {
        Err(SessionError::Conflict(
            "operation requires a channel Turn".into(),
        ))
    }
}

fn touch_channel_session(
    connection: &Connection,
    owner: &str,
    session: &mut Session,
    kind: &'static str,
    data: BTreeMap<String, Value>,
    now: DateTime<Utc>,
) -> Result<(), SessionError> {
    session.updated_at = now;
    store::write_session(connection, owner, session)?;
    store::append_event(connection, session, kind, data)
}

fn suppress_replaced_deliveries(
    connection: &Connection,
    owner: &str,
    session: &mut Session,
    old_turn: &str,
    new_turn: &str,
) -> Result<(), SessionError> {
    let count: usize = connection.execute(
        "UPDATE channel_deliveries
         SET status = CASE
             WHEN status IN ('pending', 'failed') THEN 'suppressed'
             WHEN status = 'sending' THEN 'unknown'
             ELSE status END,
             error = CASE
                 WHEN status IN ('pending', 'failed') THEN 'superseded by a newer channel message'
                 WHEN status = 'sending' THEN 'delivery outcome unknown after replacement'
                 ELSE error END,
             superseded_by_turn_id = ?3,
             updated_at = ?4
         WHERE session_id = ?1 AND turn_id = ?2
           AND status NOT IN ('confirmed', 'suppressed')",
        params![session.id, old_turn, new_turn, timestamp(Utc::now())],
    )?;
    if count == 0 {
        return Ok(());
    }
    let now = Utc::now();
    touch_channel_session(
        connection,
        owner,
        session,
        "channel.delivery.replaced",
        BTreeMap::from([
            ("old_turn".into(), json!(old_turn)),
            ("new_turn".into(), json!(new_turn)),
            ("count".into(), json!(count)),
        ]),
        now,
    )
}

fn validate_delivery_request(request: &ChannelDeliveryRequest) -> Result<(), SessionError> {
    validate_channel_value(&request.key, "delivery key", MAX_DELIVERY_KEY_BYTES)?;
    validate_channel_value(&request.kind, "delivery kind", MAX_DELIVERY_KIND_BYTES)?;
    if request.content_sha256.len() != 64
        || !request
            .content_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || request
            .content_sha256
            .bytes()
            .any(|byte| byte.is_ascii_uppercase())
    {
        return Err(SessionError::Invalid(
            "delivery content hash must be lowercase SHA-256 hex".into(),
        ));
    }
    Ok(())
}

fn validate_delivery_settlement(
    status: ChannelDeliveryStatus,
    error: Option<&str>,
) -> Result<(), SessionError> {
    if matches!(
        status,
        ChannelDeliveryStatus::Pending | ChannelDeliveryStatus::Sending
    ) {
        return Err(SessionError::Invalid(
            "delivery settlement requires a terminal or unknown status".into(),
        ));
    }
    if error.is_some_and(|value| value.len() > MAX_DELIVERY_ERROR_BYTES || value.contains('\0')) {
        return Err(SessionError::Invalid(
            "delivery error is too large or contains NUL".into(),
        ));
    }
    if matches!(
        status,
        ChannelDeliveryStatus::Failed | ChannelDeliveryStatus::Unknown
    ) && error.is_none_or(str::is_empty)
    {
        return Err(SessionError::Invalid(
            "failed or unknown delivery requires an error".into(),
        ));
    }
    if status == ChannelDeliveryStatus::Confirmed && error.is_some() {
        return Err(SessionError::Invalid(
            "confirmed delivery cannot carry an error".into(),
        ));
    }
    Ok(())
}

fn delivery_event_kind(status: ChannelDeliveryStatus) -> &'static str {
    match status {
        ChannelDeliveryStatus::Pending => "channel.delivery.pending",
        ChannelDeliveryStatus::Sending => "channel.delivery.sending",
        ChannelDeliveryStatus::Confirmed => "channel.delivery.confirmed",
        ChannelDeliveryStatus::Failed => "channel.delivery.failed",
        ChannelDeliveryStatus::Unknown => "channel.delivery.unknown",
        ChannelDeliveryStatus::Suppressed => "channel.delivery.suppressed",
    }
}

fn delivery_event_data(
    request: &ChannelDeliveryRequest,
    turn: &str,
    error: Option<&str>,
) -> BTreeMap<String, Value> {
    let mut data = BTreeMap::from([
        ("key".into(), json!(request.key)),
        ("kind".into(), json!(request.kind)),
        ("turn".into(), json!(turn)),
        ("content_sha256".into(), json!(request.content_sha256)),
    ]);
    if let Some(error) = error {
        data.insert("error".into(), json!(error));
    }
    data
}

fn verify_delivery_request(
    delivery: &ChannelDelivery,
    turn: &str,
    request: &ChannelDeliveryRequest,
) -> Result<(), SessionError> {
    if delivery.turn_id != turn
        || delivery.kind != request.kind
        || delivery.content_sha256 != request.content_sha256
    {
        return Err(SessionError::Conflict(
            "delivery key was already used for different content or Turn".into(),
        ));
    }
    Ok(())
}

fn read_delivery(
    connection: &Connection,
    session: &str,
    key: &str,
) -> Result<Option<ChannelDelivery>, SessionError> {
    connection
        .query_row(
            "SELECT session_id, key, turn_id, kind, content_sha256, status, error,
                    superseded_by_turn_id, created_at, updated_at
             FROM channel_deliveries WHERE session_id = ?1 AND key = ?2",
            params![session, key],
            read_delivery_row,
        )
        .optional()?
        .transpose()
}

fn read_delivery_row(row: &Row<'_>) -> rusqlite::Result<Result<ChannelDelivery, SessionError>> {
    let status: String = row.get(5)?;
    let status = match status.as_str() {
        "pending" => ChannelDeliveryStatus::Pending,
        "sending" => ChannelDeliveryStatus::Sending,
        "confirmed" => ChannelDeliveryStatus::Confirmed,
        "failed" => ChannelDeliveryStatus::Failed,
        "unknown" => ChannelDeliveryStatus::Unknown,
        "suppressed" => ChannelDeliveryStatus::Suppressed,
        _ => {
            return Ok(Err(SessionError::Storage(
                "invalid channel delivery status".into(),
            )));
        }
    };
    let created_at: String = row.get(8)?;
    let updated_at: String = row.get(9)?;
    let times = created_at.parse().and_then(|created_at| {
        updated_at
            .parse()
            .map(|updated_at| (created_at, updated_at))
    });
    Ok(match times {
        Ok((created_at, updated_at)) => Ok(ChannelDelivery {
            session_id: row.get(0)?,
            key: row.get(1)?,
            turn_id: row.get(2)?,
            kind: row.get(3)?,
            content_sha256: row.get(4)?,
            status,
            error: row.get(6)?,
            superseded_by_turn_id: row.get(7)?,
            created_at,
            updated_at,
        }),
        Err(error) => Err(SessionError::Storage(format!(
            "invalid channel delivery timestamp: {error}"
        ))),
    })
}

fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Nanos, true)
}
