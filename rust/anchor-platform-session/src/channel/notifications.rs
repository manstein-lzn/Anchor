//! Confirmed outgoing messages are Session facts, not synthetic user Turns.
use super::*;
use crate::ChannelNotification;

const KIND: &str = "channel.notification.confirmed";

impl SessionStore {
    pub fn channel_session_for_identity(
        &self,
        owner: &str,
        identity: &ChannelIdentity,
    ) -> Result<Option<Session>, SessionError> {
        store::validate_identity(owner, "owner")?;
        let connection = self.lock()?;
        find_channel_session(&connection, owner, &identity_parts(identity)?)?
            .map(|id| store::read_session(&connection, owner, &id))
            .transpose()
    }

    /// Only the Host calls this after the platform acknowledged the delivery.
    /// A repeated ACK cannot move an older notification ahead of a newer one.
    pub fn record_channel_notification(
        &self,
        owner: &str,
        session: &str,
        notification: &ChannelNotification,
    ) -> Result<(), SessionError> {
        store::validate_lookup(owner, session)?;
        validate_channel_value(&notification.id, "notification id", MAX_DELIVERY_KEY_BYTES)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_channel_session(&transaction, owner, session)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT data FROM session_events WHERE session_id = ?1 AND kind = ?2
             AND json_extract(data, '$.id') = ?3 ORDER BY seq DESC LIMIT 1",
                params![session, KIND, notification.id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if serde_json::from_str::<ChannelNotification>(&existing)? != *notification {
                return Err(SessionError::Conflict(
                    "notification identity changed".into(),
                ));
            }
        } else {
            let data = serde_json::from_value(serde_json::to_value(notification)?)?;
            touch_channel_session(&transaction, owner, &mut snapshot, KIND, data, Utc::now())?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Bounded projection: no conversation-history scan or replay is required.
    pub fn latest_channel_notification(
        &self,
        owner: &str,
        session: &str,
    ) -> Result<Option<ChannelNotification>, SessionError> {
        store::validate_lookup(owner, session)?;
        let connection = self.lock()?;
        store::read_session(&connection, owner, session)?;
        let raw: Option<String> = connection
            .query_row(
                "SELECT data FROM session_events WHERE session_id = ?1 AND kind = ?2
             ORDER BY seq DESC LIMIT 1",
                params![session, KIND],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replayed_ack_cannot_replace_a_newer_notification_or_cross_owners() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path().join("sessions.sqlite")).unwrap();
        let identity = ChannelIdentity {
            source: "wecom".into(),
            account: Some("bot".into()),
            conversation_kind: ChannelConversationKind::Single,
            conversation_id: "alice".into(),
            sender_id: "alice".into(),
        };
        let admitted = store
            .admit_channel_inbound(
                "owner",
                ChannelInboundRequest {
                    inbound_id: "first".into(),
                    identity: identity.clone(),
                    graph: "assistant".into(),
                    reply_node: "reply".into(),
                    text: Some("hello".into()),
                    attachments: Default::default(),
                    run_id: None,
                    replace_running: false,
                },
            )
            .unwrap();
        let session = &admitted.session.id;
        assert_eq!(
            store
                .channel_session_for_identity("owner", &identity)
                .unwrap()
                .unwrap()
                .id,
            *session
        );
        assert!(
            store
                .channel_session_for_identity("other", &identity)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .latest_channel_notification("owner", session)
                .unwrap()
                .is_none()
        );
        let first = ChannelNotification {
            id: "first-ack".into(),
            source_run: "production".into(),
            content: "first edition".into(),
            files: vec![],
        };
        let second = ChannelNotification {
            id: "second-ack".into(),
            content: "second edition".into(),
            ..first.clone()
        };
        for notification in [&first, &second, &first] {
            store
                .record_channel_notification("owner", session, notification)
                .unwrap();
        }
        assert_eq!(
            store.latest_channel_notification("owner", session).unwrap(),
            Some(second)
        );
        assert!(matches!(
            store.record_channel_notification(
                "owner",
                session,
                &ChannelNotification {
                    content: "different bytes".into(),
                    ..first.clone()
                }
            ),
            Err(SessionError::Conflict(_))
        ));
        assert!(matches!(
            store.latest_channel_notification("other", session),
            Err(SessionError::Missing)
        ));
        assert!(matches!(
            store.record_channel_notification("other", session, &first),
            Err(SessionError::Missing)
        ));
    }
}
