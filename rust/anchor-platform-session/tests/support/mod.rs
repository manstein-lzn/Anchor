#![allow(dead_code)]

use std::path::PathBuf;

use anchor_platform_session::{
    ChannelIdentity, ChannelInboundRequest, CreateSession, Session, SessionError, SessionStore,
    Turn,
};
use tempfile::TempDir;

pub fn store_fixture() -> (TempDir, PathBuf, SessionStore) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.sqlite");
    let store = SessionStore::open(&path).unwrap();
    (directory, path, store)
}

pub fn create_session(store: &SessionStore, owner: &str, id: &str) -> Session {
    store
        .create(
            owner,
            CreateSession {
                id: Some(id.into()),
                ..Default::default()
            },
        )
        .unwrap()
}

pub fn session_fixture() -> (TempDir, PathBuf, SessionStore) {
    let fixture = store_fixture();
    create_session(&fixture.2, "alice", "pilot");
    fixture
}

pub fn create_turn(store: &SessionStore, owner: &str, session: &str) -> Turn {
    create_session(store, owner, session);
    store
        .create_turn(owner, session, "request", Some("hello"))
        .unwrap()
        .0
}

pub fn turn_fixture() -> (TempDir, PathBuf, SessionStore, Turn) {
    let (directory, path, store) = store_fixture();
    let turn = create_turn(&store, "alice", "pilot");
    (directory, path, store, turn)
}

pub fn channel_request(
    inbound: &str,
    source: &str,
    account: Option<&str>,
    conversation: &str,
    sender: &str,
) -> ChannelInboundRequest {
    ChannelInboundRequest {
        inbound_id: inbound.into(),
        bot_origin: None,
        platform_message_id: None,
        identity: ChannelIdentity {
            source: source.into(),
            account: account.map(str::to_owned),
            conversation_kind: Default::default(),
            conversation_id: conversation.into(),
            sender_id: sender.into(),
        },
        graph: "fixture".into(),
        reply_node: "assistant".into(),
        text: Some("input".into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: false,
    }
}

pub fn assert_conflict<T>(result: Result<T, SessionError>) {
    let error = result.err();
    assert!(
        matches!(error, Some(SessionError::Conflict(_))),
        "{error:?}"
    );
}

pub fn assert_invalid<T>(result: Result<T, SessionError>) {
    let error = result.err();
    assert!(matches!(error, Some(SessionError::Invalid(_))), "{error:?}");
}

pub fn assert_storage<T>(result: Result<T, SessionError>) {
    let error = result.err();
    assert!(matches!(error, Some(SessionError::Storage(_))), "{error:?}");
}
