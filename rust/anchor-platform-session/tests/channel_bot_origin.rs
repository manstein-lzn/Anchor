mod support;

use std::{
    sync::{Arc, Barrier},
    thread,
};

use anchor_platform_session::{
    ChannelBotOrigin, ChannelConversationKind, ChannelInboundRequest, SessionError, SessionStore,
    TurnStatus,
};

fn human(account: &str, id: &str) -> ChannelInboundRequest {
    let mut request = support::channel_request(id, "feishu", Some(account), "oc_group", "ou_human");
    request.identity.conversation_kind = ChannelConversationKind::Group;
    request.graph = format!("graph-{account}");
    request.platform_message_id = Some(format!("om_{id}"));
    request.replace_running = true;
    request
}

fn bot(account: &str, id: &str, root: &str) -> ChannelInboundRequest {
    let mut request = human(account, id);
    request.identity.sender_id = "ou_peer_bot".into();
    request.bot_origin = Some(ChannelBotOrigin {
        root_inbound_id: root.into(),
        sender_bot_id: request.identity.sender_id.clone(),
    });
    request
}

#[test]
fn bot_admission_requires_an_existing_human_root_in_the_same_owner_feishu_group() {
    let (_directory, _path, store) = support::store_fixture();
    let mut invalid_roots = vec![];
    for (id, owner) in [
        ("other-owner", "other"),
        ("other-source", "owner"),
        ("other-group", "owner"),
        ("private", "owner"),
    ] {
        let mut root = human("a", id);
        match id {
            "other-source" => root.identity.source = "wecom".into(),
            "other-group" => root.identity.conversation_id = "oc_other".into(),
            "private" => root.identity.conversation_kind = ChannelConversationKind::Single,
            _ => {}
        }
        store.admit_channel_inbound(owner, root).unwrap();
        invalid_roots.push(bot("b", &format!("reply-{id}"), id));
    }
    invalid_roots.push(bot("b", "missing", "fabricated-root"));
    store
        .admit_channel_inbound("owner", human("a", "human-root"))
        .unwrap();
    let mut mismatch = bot("b", "sender-mismatch", "human-root");
    mismatch.identity.sender_id = "ou_different_bot".into();
    invalid_roots.push(mismatch);
    let mut private = bot("b", "private-bot", "human-root");
    private.identity.conversation_kind = ChannelConversationKind::Single;
    invalid_roots.push(private);
    let mut other_source = bot("b", "other-source-bot", "human-root");
    other_source.identity.source = "wecom".into();
    invalid_roots.push(other_source);
    let before = store.list("owner").unwrap();
    for request in invalid_roots {
        support::assert_conflict(store.admit_channel_inbound("owner", request));
        assert_eq!(store.list("owner").unwrap(), before);
    }

    let accepted = store
        .admit_channel_inbound("owner", bot("b", "request", "human-root"))
        .unwrap();
    support::assert_conflict(
        store.admit_channel_inbound("owner", bot("c", "bot-cannot-be-root", "request")),
    );
    store
        .bind_channel_assistant(
            "owner",
            &accepted.session.id,
            "run-b",
            "wait",
            "assistant",
            "reply",
        )
        .unwrap();
    let claimed = store
        .claim_channel_assistant_input("owner", &accepted.session.id, "run-b", "wait/1")
        .unwrap()
        .unwrap();
    assert_eq!(
        claimed.request.platform_message_id.as_deref(),
        Some("om_request")
    );
    assert_eq!(
        claimed.request.bot_origin,
        bot("b", "request", "human-root").bot_origin
    );
}

#[test]
fn pair_budget_survives_replay_restart_retirement_and_account_aliases() {
    let (_directory, path, store) = support::store_fixture();
    let human_root = store
        .admit_channel_inbound("owner", human("a", "root"))
        .unwrap();
    let request = bot("legacy-b", "request", "root");
    let accepted = store
        .admit_channel_inbound("owner", request.clone())
        .unwrap();
    let events = store.events("owner", &accepted.session.id, 0).unwrap();
    assert_eq!(
        store
            .admit_channel_inbound("owner", request.clone())
            .unwrap(),
        accepted
    );
    assert_eq!(
        store.events("owner", &accepted.session.id, 0).unwrap(),
        events
    );

    // A can receive one report for the same human root; it replaces the Running
    // human input normally rather than creating a separate bot queue.
    let report = store
        .admit_channel_inbound("owner", bot("a", "report", "root"))
        .unwrap();
    assert_eq!(report.session.id, human_root.session.id);
    assert_eq!(
        store
            .get_channel_relation("owner", &report.session.id, "root")
            .unwrap()
            .superseded_by_turn_id,
        Some(report.turn.id.clone())
    );
    let before = store.list_turns("owner", &report.session.id).unwrap();
    support::assert_conflict(store.admit_channel_inbound("owner", bot("a", "loop", "root")));
    assert_eq!(
        store.list_turns("owner", &report.session.id).unwrap(),
        before
    );

    store
        .bind_channel_assistant(
            "owner",
            &accepted.session.id,
            "old-b",
            "wait",
            "assistant",
            "reply",
        )
        .unwrap();
    store
        .finish_turn(
            "owner",
            &accepted.session.id,
            &accepted.turn.id,
            TurnStatus::Stopped,
            None,
        )
        .unwrap();
    store
        .retire_channel_assistant("owner", &accepted.session.id, "old-b")
        .unwrap();
    drop(store);
    let store = SessionStore::open(&path).unwrap();
    assert_eq!(
        store
            .admit_channel_inbound("owner", request)
            .unwrap()
            .turn
            .id,
        accepted.turn.id
    );
    support::assert_conflict(
        store.admit_channel_inbound("owner", bot("legacy-b", "after-stop", "root")),
    );

    // A distinct retained historical Session for an alias still consumes the
    // canonical target's budget after the canonical Session becomes selected.
    let current = store
        .admit_channel_inbound("owner", human("b", "current-human"))
        .unwrap();
    store
        .register_channel_account("owner", "feishu", "b", &["legacy-b".into()])
        .unwrap();
    assert!(
        !store
            .channel_session_is_selected("owner", &accepted.session.id)
            .unwrap()
    );
    let before = store.list_turns("owner", &current.session.id).unwrap();
    let events = store.events("owner", &current.session.id, 0).unwrap();
    support::assert_conflict(
        store.admit_channel_inbound("owner", bot("b", "same-peer-same-root", "root")),
    );
    assert_eq!(
        store.list_turns("owner", &current.session.id).unwrap(),
        before
    );
    assert_eq!(
        store.events("owner", &current.session.id, 0).unwrap(),
        events
    );

    let mut changed_sender = bot("b", "new-peer-same-root", "root");
    changed_sender.identity.sender_id = "ou_another_peer".into();
    changed_sender.bot_origin.as_mut().unwrap().sender_bot_id = "ou_another_peer".into();
    let peer = store
        .admit_channel_inbound("owner", changed_sender.clone())
        .unwrap();
    assert_eq!(peer.session.id, current.session.id);
    assert_eq!(
        store
            .get_turn("owner", &current.session.id, &current.turn.id)
            .unwrap()
            .status,
        TurnStatus::Interrupted
    );
    changed_sender.inbound_id = "another-message-from-new-peer".into();
    changed_sender.platform_message_id = Some("om_another_message".into());
    let before = store.list_turns("owner", &current.session.id).unwrap();
    let events = store.events("owner", &current.session.id, 0).unwrap();
    support::assert_conflict(store.admit_channel_inbound("owner", changed_sender));
    assert_eq!(
        store.list_turns("owner", &current.session.id).unwrap(),
        before
    );
    assert_eq!(
        store.events("owner", &current.session.id, 0).unwrap(),
        events
    );

    let next = store
        .admit_channel_inbound("owner", bot("b", "new-root-request", "current-human"))
        .unwrap();
    assert_eq!(next.session.id, current.session.id);
    assert_eq!(
        store
            .get_turn("owner", &current.session.id, &peer.turn.id)
            .unwrap()
            .status,
        TurnStatus::Interrupted
    );
}

#[test]
fn concurrent_different_messages_share_one_durable_pair_budget() {
    let (_directory, path, store) = support::store_fixture();
    store
        .admit_channel_inbound("owner", human("a", "root"))
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers = ["first", "second"].map(|id| {
        let connection = SessionStore::open(&path).unwrap();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            connection.admit_channel_inbound("owner", bot("b", id, "root"))
        })
    });
    let results = workers.map(|worker| worker.join().unwrap());
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(SessionError::Conflict(_))))
            .count(),
        1
    );
    let accepted = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(
        store
            .list_turns("owner", &accepted.session.id)
            .unwrap()
            .len(),
        1
    );
    let replay = store
        .admit_channel_inbound("owner", bot("b", &accepted.relation.inbound_id, "root"))
        .unwrap();
    assert_eq!(replay, accepted);
}

#[test]
fn optional_platform_id_preserves_old_replays_but_new_identity_and_origin_are_immutable() {
    let (_directory, _path, store) = support::store_fixture();
    let mut old = human("a", "old");
    old.platform_message_id = None;
    let json = serde_json::to_value(&old).unwrap();
    assert!(json.get("platform_message_id").is_none());
    assert!(json.get("bot_origin").is_none());
    let decoded: ChannelInboundRequest = serde_json::from_value(json).unwrap();
    assert_eq!(decoded, old);
    let admitted = store.admit_channel_inbound("owner", old).unwrap();
    let events = store.events("owner", &admitted.session.id, 0).unwrap();
    assert_eq!(
        store
            .admit_channel_inbound("owner", human("a", "old"))
            .unwrap(),
        admitted
    );
    assert_eq!(
        store.events("owner", &admitted.session.id, 0).unwrap(),
        events
    );
    assert!(
        store
            .get_channel_inbound("owner", &admitted.session.id, "old")
            .unwrap()
            .request
            .platform_message_id
            .is_none()
    );

    let request = bot("b", "request", "old");
    let accepted = store
        .admit_channel_inbound("owner", request.clone())
        .unwrap();
    let events = store.events("owner", &accepted.session.id, 0).unwrap();
    let mut changed = request.clone();
    changed.platform_message_id = Some("om_other".into());
    support::assert_conflict(store.admit_channel_inbound("owner", changed));
    let mut changed = request.clone();
    changed.platform_message_id = None;
    support::assert_conflict(store.admit_channel_inbound("owner", changed));
    let mut changed = request.clone();
    changed.bot_origin = None;
    support::assert_conflict(store.admit_channel_inbound("owner", changed));
    let mut changed = request.clone();
    changed.bot_origin.as_mut().unwrap().root_inbound_id = "other-root".into();
    support::assert_conflict(store.admit_channel_inbound("owner", changed));
    assert_eq!(
        store.events("owner", &accepted.session.id, 0).unwrap(),
        events
    );
    assert_eq!(
        store.admit_channel_inbound("owner", request).unwrap(),
        accepted
    );
}
