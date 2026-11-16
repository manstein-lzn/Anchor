use rusqlite::{Connection, params};

use crate::{ChannelConversationKind, ChannelInboundRequest, SessionError};

/// Runs after replay checks and before replacing any Turn, in the admission write transaction.
pub(super) fn verify_admission(
    connection: &Connection,
    owner: &str,
    request: &ChannelInboundRequest,
) -> Result<(), SessionError> {
    let Some(origin) = &request.bot_origin else {
        return Ok(());
    };
    let identity = &request.identity;
    if identity.source != "feishu"
        || identity.conversation_kind != ChannelConversationKind::Group
        || identity.sender_id != origin.sender_bot_id
    {
        return Err(SessionError::Conflict(
            "bot origin requires a Feishu group input with the same sender".into(),
        ));
    }
    let human_root: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM channel_inbounds inbound
            JOIN channel_sessions session ON session.session_id = inbound.session_id
            WHERE session.owner = ?1 AND session.source = ?2
              AND session.conversation_kind = 'group' AND session.conversation_id = ?3
              AND inbound.inbound_id = ?4
              AND json_extract(inbound.request, '$.bot_origin') IS NULL
        )",
        params![
            owner,
            identity.source,
            identity.conversation_id,
            origin.root_inbound_id
        ],
        |row| row.get(0),
    )?;
    if !human_root {
        return Err(SessionError::Conflict(
            "bot origin has no admitted human root in this owner's Feishu group".into(),
        ));
    }
    // Count retained input facts, not current assistants: retirement or another
    // account alias must not replenish this sender-to-target pair's budget.
    let used: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM channel_inbounds inbound
            JOIN channel_sessions session ON session.session_id = inbound.session_id
            LEFT JOIN channel_account_aliases alias
              ON alias.owner = session.owner AND alias.source = session.source
             AND alias.alias = session.account
            WHERE session.owner = ?1 AND session.source = ?2
              AND COALESCE(alias.canonical, session.account) = ?3
              AND session.conversation_kind = 'group' AND session.conversation_id = ?4
              AND json_extract(inbound.request, '$.bot_origin.root_inbound_id') = ?5
              AND json_extract(inbound.request, '$.bot_origin.sender_bot_id') = ?6
        )",
        params![
            owner,
            identity.source,
            identity.account.as_deref().unwrap_or_default(),
            identity.conversation_id,
            origin.root_inbound_id,
            origin.sender_bot_id
        ],
        |row| row.get(0),
    )?;
    if used {
        return Err(SessionError::Conflict(
            "this bot account already accepted this sender for this group human root".into(),
        ));
    }
    Ok(())
}
