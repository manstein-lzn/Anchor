use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
    thread,
};

use anchor_platform_session::{
    AttachmentManifest, AttachmentManifestEntry, ChannelAdmission, ChannelAssistant,
    ChannelIdentity, ChannelInboundRequest, CreateSession, SessionError, SessionStore, TurnStatus,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn fixture() -> (TempDir, SessionStore, ChannelAdmission) {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    let admission = store
        .admit_channel_inbound("owner", request("message-1", "conversation-1"))
        .unwrap();
    (directory, store, admission)
}

fn request(inbound: &str, conversation: &str) -> ChannelInboundRequest {
    ChannelInboundRequest {
        inbound_id: inbound.into(),
        identity: ChannelIdentity {
            source: "wecom".into(),
            account: Some("corp-a".into()),
            conversation_id: conversation.into(),
            sender_id: "user-1".into(),
        },
        graph: "support-graph".into(),
        reply_node: "assistant".into(),
        text: Some(format!("input for {inbound}")),
        attachments: AttachmentManifest::default(),
        run_id: None,
        replace_running: false,
    }
}

fn bind(store: &SessionStore, session: &str) -> ChannelAssistant {
    store
        .bind_channel_assistant(
            "owner",
            session,
            "assistant-run",
            "wait",
            "assistant",
            "reply",
        )
        .unwrap()
}

fn replace(store: &SessionStore, inbound: &str) -> ChannelAdmission {
    let mut replacement = request(inbound, "conversation-1");
    replacement.replace_running = true;
    store.admit_channel_inbound("owner", replacement).unwrap()
}

fn assert_conflict<T: std::fmt::Debug>(result: Result<T, SessionError>) {
    assert!(
        matches!(result, Err(SessionError::Conflict(_))),
        "{result:?}"
    );
}

#[test]
fn binding_is_durable_idempotent_and_keeps_work_and_reply_nodes_distinct() {
    let (directory, store, first) = fixture();
    let session = &first.session.id;
    assert_eq!(store.get_channel_assistant("owner", session).unwrap(), None);
    let assistant = bind(&store, session);
    assert_eq!(assistant.session_id, *session);
    assert_eq!(assistant.run_id, "assistant-run");
    assert_eq!(assistant.wait_node, "wait");
    assert_eq!(assistant.work_node, "assistant");
    assert_eq!(assistant.reply_node, "reply");
    assert_eq!(store.get("owner", session).unwrap().reply_node, "assistant");
    assert_eq!(
        store.get("owner", session).unwrap().run_ids,
        ["assistant-run"]
    );
    assert_eq!(
        store
            .get_channel_relation("owner", session, "message-1")
            .unwrap()
            .run_id,
        None
    );
    assert!(
        store
            .get_turn("owner", session, &first.turn.id)
            .unwrap()
            .runs
            .is_empty()
    );
    let snapshot = store.get("owner", session).unwrap();
    let events = store.events("owner", session, 0).unwrap();
    assert_eq!(bind(&store, session), assistant);
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
    for (run, wait, work, reply) in [
        ("other-run", "wait", "assistant", "reply"),
        ("assistant-run", "other-wait", "assistant", "reply"),
        ("assistant-run", "wait", "other-work", "reply"),
        ("assistant-run", "wait", "assistant", "other-reply"),
    ] {
        assert_conflict(store.bind_channel_assistant("owner", session, run, wait, work, reply));
    }
    drop(store);
    let reopened = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    assert_eq!(
        reopened.get_channel_assistant("owner", session).unwrap(),
        Some(assistant)
    );
    assert_eq!(reopened.events("owner", session, 0).unwrap(), events);
}

#[test]
fn two_turns_share_one_run_only_through_first_input_claims() {
    let (_directory, store, first) = fixture();
    let session = &first.session.id;
    bind(&store, session);
    assert_eq!(
        store
            .get_channel_assistant_input("owner", session, "assistant-run", "wait/1")
            .unwrap(),
        None
    );
    let claimed = store
        .claim_channel_assistant_input("owner", session, "assistant-run", "wait/1")
        .unwrap()
        .unwrap();
    assert_eq!(claimed.turn.id, first.turn.id);
    assert_eq!(claimed.turn.runs, ["assistant-run"]);
    assert_eq!(claimed.relation.run_id.as_deref(), Some("assistant-run"));
    store
        .finish_turn(
            "owner",
            session,
            &first.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    let mut next = request("message-2", "conversation-1");
    next.attachments.files.push(AttachmentManifestEntry {
        name: "report.txt".into(),
        path: "/workspace/inbox/report.txt".into(),
        sha256: "a".repeat(64),
        size: 12,
        media_type: Some("text/plain".into()),
    });
    let second = store.admit_channel_inbound("owner", next.clone()).unwrap();
    assert_eq!(second.relation.run_id, None);
    assert_conflict(store.associate_channel_run("owner", session, "message-2", "assistant-run"));
    let claimed = store
        .claim_channel_assistant_input("owner", session, "assistant-run", "wait/2")
        .unwrap()
        .unwrap();
    assert_eq!(claimed.turn.id, second.turn.id);
    assert_eq!(claimed.turn.runs, ["assistant-run"]);
    assert_eq!(claimed.request, next);
    assert_eq!(
        store.get("owner", session).unwrap().run_ids,
        ["assistant-run"]
    );
    let snapshot = store.get("owner", session).unwrap();
    let events = store.events("owner", session, 0).unwrap();
    for _ in 0..3 {
        let retry = store
            .claim_channel_assistant_input("owner", session, "assistant-run", "wait/1")
            .unwrap()
            .unwrap();
        assert_eq!(retry.turn.id, first.turn.id);
        assert_eq!(retry.turn.status, TurnStatus::Completed);
        assert_eq!(
            store
                .claim_channel_assistant_input("owner", session, "assistant-run", "wait/2")
                .unwrap(),
            Some(claimed.clone())
        );
        assert_eq!(
            store
                .claim_channel_assistant_input("owner", session, "assistant-run", "wait/3")
                .unwrap(),
            None
        );
    }
    assert_conflict(store.associate_channel_run("owner", session, "message-1", "assistant-run"));
    assert_conflict(store.associate_channel_run("owner", session, "message-2", "assistant-run"));
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
}

#[test]
fn replacement_skips_unclaimed_superseded_turns_but_restores_claimed_turns() {
    let (directory, store, first) = fixture();
    let session = &first.session.id;
    bind(&store, session);
    let second = replace(&store, "message-2");
    let key = r#" {"node":"wait/input","round":2} "#;
    let claimed = store
        .claim_channel_assistant_input("owner", session, "assistant-run", key)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.turn.id, second.turn.id);
    let third = replace(&store, "message-3");
    assert_eq!(replace(&store, "message-3").turn, third.turn);
    drop(store);
    let reopened = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    let snapshot = reopened.get("owner", session).unwrap();
    let events = reopened.events("owner", session, 0).unwrap();
    for _ in 0..3 {
        let retry = reopened
            .claim_channel_assistant_input("owner", session, "assistant-run", key)
            .unwrap()
            .unwrap();
        assert_eq!(retry.turn.id, second.turn.id);
        assert_eq!(retry.turn.status, TurnStatus::Interrupted);
        assert_eq!(
            retry.relation.superseded_by_turn_id.as_deref(),
            Some(third.turn.id.as_str())
        );
        assert_eq!(
            reopened
                .get_channel_assistant_input("owner", session, "assistant-run", key)
                .unwrap(),
            Some(retry)
        );
    }
    assert_eq!(reopened.get("owner", session).unwrap(), snapshot);
    assert_eq!(reopened.events("owner", session, 0).unwrap(), events);
    let next = reopened
        .claim_channel_assistant_input("owner", session, "assistant-run", key.trim())
        .unwrap()
        .unwrap();
    assert_eq!(next.turn.id, third.turn.id);
    assert_eq!(
        reopened
            .claim_channel_assistant_input("owner", session, "assistant-run", "another-key")
            .unwrap(),
        None
    );
    assert_eq!(
        reopened
            .get_channel_relation("owner", session, "message-1")
            .unwrap()
            .run_id,
        None
    );
    reopened
        .finish_turn(
            "owner",
            session,
            &third.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    assert_eq!(
        reopened
            .claim_channel_assistant_input("owner", session, "assistant-run", "after-completion")
            .unwrap(),
        None
    );
    assert_eq!(
        reopened
            .claim_channel_assistant_input("owner", session, "assistant-run", key)
            .unwrap()
            .unwrap()
            .turn
            .id,
        second.turn.id
    );
}

#[test]
fn assistants_and_inputs_cannot_cross_owners_sessions_or_untrusted_projections() {
    let (_directory, store, first) = fixture();
    let session = &first.session.id;
    assert_conflict(store.claim_channel_assistant_input(
        "owner",
        session,
        "assistant-run",
        "wait/1",
    ));
    assert_conflict(store.get_channel_assistant_input("owner", session, "assistant-run", "wait/1"));
    bind(&store, session);
    let same_owner = store
        .admit_channel_inbound("owner", request("other-message", "conversation-2"))
        .unwrap();
    let other_owner = store
        .admit_channel_inbound("other", request("message-1", "conversation-1"))
        .unwrap();
    for (owner, other_session, inbound, turn) in [
        (
            "owner",
            &same_owner.session.id,
            "other-message",
            &same_owner.turn.id,
        ),
        (
            "other",
            &other_owner.session.id,
            "message-1",
            &other_owner.turn.id,
        ),
    ] {
        assert_conflict(store.bind_channel_assistant(
            owner,
            other_session,
            "assistant-run",
            "wait",
            "assistant",
            "reply",
        ));
        assert_conflict(store.associate_channel_run(
            owner,
            other_session,
            inbound,
            "assistant-run",
        ));
        assert_conflict(store.attach_run(owner, other_session, "assistant-run"));
        assert_conflict(store.associate_run(owner, other_session, turn, "assistant-run"));
        assert_conflict(store.claim_channel_assistant_input(
            owner,
            other_session,
            "assistant-run",
            "wait/1",
        ));
    }
    assert_eq!(
        store.get_channel_assistant("other", session),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.bind_channel_assistant(
            "other",
            session,
            "assistant-run",
            "wait",
            "assistant",
            "reply"
        ),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.claim_channel_assistant_input("other", session, "assistant-run", "wait/1"),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.get_channel_assistant_input("other", session, "assistant-run", "wait/1"),
        Err(SessionError::Missing)
    );
    store
        .bind_channel_assistant(
            "owner",
            &same_owner.session.id,
            "second-run",
            "wait",
            "assistant",
            "reply",
        )
        .unwrap();
    assert_conflict(store.get_channel_assistant_input(
        "owner",
        &same_owner.session.id,
        "assistant-run",
        "wait/1",
    ));
    assert_conflict(store.claim_channel_assistant_input("owner", session, "second-run", "wait/1"));
    let first_input = store
        .claim_channel_assistant_input("owner", session, "assistant-run", "same-key")
        .unwrap()
        .unwrap();
    let second_input = store
        .claim_channel_assistant_input("owner", &same_owner.session.id, "second-run", "same-key")
        .unwrap()
        .unwrap();
    assert_ne!(first_input.turn.id, second_input.turn.id);
    let ordinary = store
        .create(
            "owner",
            CreateSession {
                reply_node: "assistant".into(),
                channel: BTreeMap::from([("source".into(), "wecom".into())]),
                ..Default::default()
            },
        )
        .unwrap();
    assert_conflict(store.bind_channel_assistant(
        "owner",
        &ordinary.id,
        "fake-run",
        "wait",
        "assistant",
        "reply",
    ));
}

#[test]
fn prior_run_facts_in_other_sessions_and_incompatible_input_runs_are_rejected() {
    let (_directory, store, first) = fixture();
    let ordinary = store.create("other", CreateSession::default()).unwrap();
    store
        .attach_run("other", &ordinary.id, "retained-run")
        .unwrap();
    assert_conflict(store.bind_channel_assistant(
        "owner",
        &first.session.id,
        "retained-run",
        "wait",
        "assistant",
        "reply",
    ));
    let turn = store
        .create_turn("other", &ordinary.id, "turn", None)
        .unwrap()
        .0;
    store
        .associate_run("other", &ordinary.id, &turn.id, "turn-run")
        .unwrap();
    assert_conflict(store.bind_channel_assistant(
        "owner",
        &first.session.id,
        "turn-run",
        "wait",
        "assistant",
        "reply",
    ));
    let mut legacy = request("legacy", "conversation-2");
    legacy.run_id = Some("legacy-run".into());
    store.admit_channel_inbound("other", legacy).unwrap();
    assert_conflict(store.bind_channel_assistant(
        "owner",
        &first.session.id,
        "legacy-run",
        "wait",
        "assistant",
        "reply",
    ));
    store
        .associate_channel_run("owner", &first.session.id, "message-1", "old-run")
        .unwrap();
    bind(&store, &first.session.id);
    let events = store.events("owner", &first.session.id, 0).unwrap();
    assert_conflict(store.claim_channel_assistant_input(
        "owner",
        &first.session.id,
        "assistant-run",
        "wait/1",
    ));
    assert_eq!(
        store
            .get_channel_assistant_input("owner", &first.session.id, "assistant-run", "wait/1")
            .unwrap(),
        None
    );
    assert_eq!(store.events("owner", &first.session.id, 0).unwrap(), events);
}

#[test]
fn independent_connections_serialize_duplicate_and_competing_wait_keys() {
    let (directory, store, first) = fixture();
    let session = &first.session.id;
    let assistants = (0..4)
        .map(|_| SessionStore::open(directory.path().join("sessions.sqlite")).unwrap())
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(assistants.len()));
    let workers = assistants
        .into_iter()
        .map(|store| {
            let barrier = barrier.clone();
            let session = session.clone();
            thread::spawn(move || {
                barrier.wait();
                bind(&store, &session);
                store
                    .claim_channel_assistant_input("owner", &session, "assistant-run", "wait/1")
                    .unwrap()
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        assert_eq!(worker.join().unwrap().turn.id, first.turn.id);
    }
    let events = store.events("owner", session, 0).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "channel.assistant_bound")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "turn.run_associated")
            .count(),
        1
    );
    let second = replace(&store, "message-2");
    let contenders = (0..2)
        .map(|_| SessionStore::open(directory.path().join("sessions.sqlite")).unwrap())
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(contenders.len()));
    let workers = contenders
        .into_iter()
        .enumerate()
        .map(|(index, store)| {
            let barrier = barrier.clone();
            let session = session.clone();
            thread::spawn(move || {
                barrier.wait();
                store
                    .claim_channel_assistant_input(
                        "owner",
                        &session,
                        "assistant-run",
                        &format!("competing/{index}"),
                    )
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let claims = workers
        .into_iter()
        .filter_map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].turn.id, second.turn.id);
}

#[test]
fn binding_and_input_claim_failures_roll_back_all_facts_and_events() {
    let (directory, store, first) = fixture();
    let session = &first.session.id;
    let connection = Connection::open(directory.path().join("sessions.sqlite")).unwrap();
    let snapshot = store.get("owner", session).unwrap();
    let events = store.events("owner", session, 0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_assistant_bind BEFORE INSERT ON session_events
        WHEN NEW.kind = 'channel.assistant_bound' BEGIN SELECT RAISE(FAIL, 'injected bind failure'); END;").unwrap();
    assert!(matches!(
        store.bind_channel_assistant(
            "owner",
            session,
            "assistant-run",
            "wait",
            "assistant",
            "reply"
        ),
        Err(SessionError::Storage(_))
    ));
    assert_eq!(store.get_channel_assistant("owner", session).unwrap(), None);
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
    connection
        .execute_batch("DROP TRIGGER fail_assistant_bind;")
        .unwrap();
    bind(&store, session);
    let snapshot = store.get("owner", session).unwrap();
    let events = store.events("owner", session, 0).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_assistant_claim BEFORE INSERT ON turn_runs
        BEGIN SELECT RAISE(FAIL, 'injected claim failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        store.claim_channel_assistant_input("owner", session, "assistant-run", "wait/1"),
        Err(SessionError::Storage(_))
    ));
    assert_eq!(
        store
            .get_channel_assistant_input("owner", session, "assistant-run", "wait/1")
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .get_channel_relation("owner", session, "message-1")
            .unwrap()
            .run_id,
        None
    );
    assert!(
        store
            .get_turn("owner", session, &first.turn.id)
            .unwrap()
            .runs
            .is_empty()
    );
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
    connection
        .execute_batch("DROP TRIGGER fail_assistant_claim;")
        .unwrap();
    assert_eq!(
        store
            .claim_channel_assistant_input("owner", session, "assistant-run", "wait/1")
            .unwrap()
            .unwrap()
            .turn
            .id,
        first.turn.id
    );
}

#[test]
fn opaque_wait_keys_are_validated_without_runtime_parsing_or_normalization() {
    let (_directory, store, first) = fixture();
    let session = &first.session.id;
    bind(&store, session);
    for key in [
        "".to_owned(),
        " ".to_owned(),
        "bad\nkey".to_owned(),
        "bad\0key".to_owned(),
        "x".repeat(4097),
    ] {
        assert!(matches!(
            store.claim_channel_assistant_input("owner", session, "assistant-run", &key),
            Err(SessionError::Invalid(_))
        ));
        assert!(matches!(
            store.get_channel_assistant_input("owner", session, "assistant-run", &key),
            Err(SessionError::Invalid(_))
        ));
    }
    let key = "界".repeat(1365) + "!";
    let claimed = store
        .claim_channel_assistant_input("owner", session, "assistant-run", &key)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.turn.id, first.turn.id);
    assert_eq!(
        store
            .get_channel_assistant_input("owner", session, "assistant-run", &key)
            .unwrap(),
        Some(claimed)
    );
}

#[test]
fn legacy_admission_and_association_keep_one_turn_per_run_without_an_assistant() {
    let (_directory, store, first) = fixture();
    let session = &first.session.id;
    store
        .associate_channel_run("owner", session, "message-1", "legacy-run")
        .unwrap();
    store
        .finish_turn(
            "owner",
            session,
            &first.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    let snapshot = store.get("owner", session).unwrap();
    let events = store.events("owner", session, 0).unwrap();
    let mut second = request("message-2", "conversation-1");
    second.run_id = Some("legacy-run".into());
    assert_conflict(store.admit_channel_inbound("owner", second));
    assert_eq!(store.list_turns("owner", session).unwrap().len(), 1);
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
    let second = store
        .admit_channel_inbound("owner", request("message-2", "conversation-1"))
        .unwrap();
    assert_conflict(store.associate_channel_run("owner", session, "message-2", "legacy-run"));
    assert_eq!(
        store
            .get_channel_relation("owner", session, "message-2")
            .unwrap()
            .run_id,
        None
    );
    assert!(
        store
            .get_turn("owner", session, &second.turn.id)
            .unwrap()
            .runs
            .is_empty()
    );
    assert_eq!(store.get_channel_assistant("owner", session).unwrap(), None);
}
