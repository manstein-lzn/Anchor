mod support;

use anchor_platform_session::{
    ChannelConversationKind, ChannelIdentity, ChannelInboundRequest, SessionError, SessionStore,
    TurnStatus,
};

fn inbound(id: &str, actor: &str) -> ChannelInboundRequest {
    let mut request = support::channel_request(id, "wecom", Some("corp"), "shared-group", actor);
    request.identity.conversation_kind = ChannelConversationKind::Group;
    request.graph = "assistant".into();
    request.reply_node = "work".into();
    request.text = Some(format!("message from {actor}"));
    request.replace_running = true;
    request
}

#[test]
fn group_members_share_current_assistant_and_preserve_real_actor_per_turn() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.sqlite");
    let store = SessionStore::open(&path).unwrap();
    let alice = store
        .admit_channel_inbound("owner", inbound("alice-1", "alice"))
        .unwrap();
    let session = &alice.session.id;
    let identity = store.channel_identity("owner", session).unwrap();
    assert_eq!(identity.conversation_kind, ChannelConversationKind::Group);
    assert_eq!(identity.conversation_id, "shared-group");
    assert!(identity.sender_id.is_empty());
    assert_eq!(
        alice
            .session
            .channel
            .get("conversation_kind")
            .map(String::as_str),
        Some("group")
    );
    assert!(!alice.session.channel.contains_key("sender_id"));
    store
        .bind_channel_assistant("owner", session, "shared-run", "wait", "work", "reply")
        .unwrap();
    let first = store
        .claim_channel_assistant_input("owner", session, "shared-run", "wait/1")
        .unwrap()
        .unwrap();
    assert_eq!(first.request.identity.sender_id, "alice");
    let bob = store
        .admit_channel_inbound("owner", inbound("bob-1", "bob"))
        .unwrap();
    assert_eq!(bob.session.id, *session);
    assert_eq!(
        store
            .get_turn("owner", session, &alice.turn.id)
            .unwrap()
            .status,
        TurnStatus::Interrupted
    );
    assert_eq!(
        store
            .get_channel_relation("owner", session, "alice-1")
            .unwrap()
            .superseded_by_turn_id,
        Some(bob.turn.id.clone())
    );
    assert_eq!(
        store
            .get_channel_assistant("owner", session)
            .unwrap()
            .unwrap()
            .run_id,
        "shared-run"
    );
    let next = store
        .claim_channel_assistant_input("owner", session, "shared-run", "wait/2")
        .unwrap()
        .unwrap();
    assert_eq!(next.turn.id, bob.turn.id);
    assert_eq!(next.request.identity.sender_id, "bob");
    assert_eq!(
        next.request.identity.conversation_kind,
        ChannelConversationKind::Group
    );
    assert_eq!(store.get("owner", session).unwrap().run_ids, ["shared-run"]);
    assert!(matches!(
        store.admit_channel_inbound("owner", inbound("bob-1", "alice")),
        Err(SessionError::Conflict(_))
    ));
    drop(store);
    let store = SessionStore::open(path).unwrap();
    let alice_again = store
        .admit_channel_inbound("owner", inbound("alice-2", "alice"))
        .unwrap();
    assert_eq!(alice_again.session.id, *session);
    assert_eq!(
        store
            .get_channel_assistant("owner", session)
            .unwrap()
            .unwrap()
            .run_id,
        "shared-run"
    );
}

#[test]
fn group_scope_isolates_private_conversations_groups_sources_accounts_and_owners() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    let first = store
        .admit_channel_inbound("owner", inbound("event", "alice"))
        .unwrap();
    let mut scopes = vec![];
    let mut private = inbound("event", "alice");
    private.identity.conversation_kind = ChannelConversationKind::Single;
    scopes.push(("owner", private.clone()));
    private.identity.sender_id = "bob".into();
    scopes.push(("owner", private));
    let mut other = inbound("event", "alice");
    other.identity.conversation_id = "another-group".into();
    scopes.push(("owner", other));
    let mut other = inbound("event", "alice");
    other.identity.source = "another-source".into();
    scopes.push(("owner", other));
    let mut other = inbound("event", "alice");
    other.identity.account = Some("another-account".into());
    scopes.push(("owner", other));
    scopes.push(("another-owner", inbound("event", "alice")));
    let mut sessions = std::collections::BTreeSet::from([first.session.id]);
    for (owner, request) in scopes {
        assert!(
            sessions.insert(
                store
                    .admit_channel_inbound(owner, request)
                    .unwrap()
                    .session
                    .id
            )
        );
    }
    let mut invalid = inbound("invalid", "");
    assert!(matches!(
        store.admit_channel_inbound("owner", invalid.clone()),
        Err(SessionError::Invalid(_))
    ));
    invalid.identity.sender_id = "alice".into();
    invalid.identity.conversation_id = String::new();
    assert!(matches!(
        store.admit_channel_inbound("owner", invalid),
        Err(SessionError::Invalid(_))
    ));
}

#[test]
fn absent_kind_keeps_legacy_private_identity_json_unchanged() {
    let encoded =
        r#"{"source":"wecom","account":"corp","conversation_id":"private","sender_id":"alice"}"#;
    let identity: ChannelIdentity = serde_json::from_str(encoded).unwrap();
    assert_eq!(identity.conversation_kind, ChannelConversationKind::Single);
    assert_eq!(serde_json::to_string(&identity).unwrap(), encoded);
    assert_eq!(identity.participant_id(), "alice");
}
