use std::{
    sync::{Arc, Barrier},
    thread,
};

use anchor_platform_session::{
    AttachmentManifest, AttachmentManifestEntry, ChannelAdmission, ChannelAssistant,
    ChannelDeliveryRequest, ChannelDeliveryStatus, ChannelIdentity, ChannelInboundRequest,
    SessionError, SessionStore, TurnStatus,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

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
        attachments: AttachmentManifest {
            format: 1,
            files: vec![AttachmentManifestEntry {
                name: format!("{inbound}.txt"),
                path: format!("/workspace/inbox/{inbound}.txt"),
                sha256: "a".repeat(64),
                size: 12,
                media_type: Some("text/plain".into()),
            }],
        },
        run_id: None,
        replace_running: true,
    }
}

fn fixture() -> (TempDir, SessionStore, ChannelAdmission) {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    let first = store
        .admit_channel_inbound("owner", request("message-1", "conversation-1"))
        .unwrap();
    (directory, store, first)
}

fn bind(store: &SessionStore, session: &str, run: &str) -> ChannelAssistant {
    store
        .bind_channel_assistant("owner", session, run, "wait", "assistant", "reply")
        .unwrap()
}

fn finish(store: &SessionStore, admission: &ChannelAdmission, status: TurnStatus) {
    store
        .finish_turn(
            "owner",
            &admission.session.id,
            &admission.turn.id,
            status,
            None,
        )
        .unwrap();
}

fn delivery(key: &str) -> ChannelDeliveryRequest {
    ChannelDeliveryRequest {
        key: key.into(),
        kind: "text".into(),
        content_sha256: "b".repeat(64),
    }
}

fn confirm(store: &SessionStore, admission: &ChannelAdmission, key: &str) {
    store
        .admit_completed_channel_delivery(
            "owner",
            &admission.session.id,
            &admission.turn.id,
            delivery(key),
        )
        .unwrap();
    store
        .begin_channel_delivery(
            "owner",
            &admission.session.id,
            &admission.turn.id,
            delivery(key),
        )
        .unwrap();
    store
        .settle_channel_delivery(
            "owner",
            &admission.session.id,
            delivery(key),
            ChannelDeliveryStatus::Confirmed,
            None,
        )
        .unwrap();
}

fn assert_conflict<T: std::fmt::Debug>(result: Result<T, SessionError>) {
    assert!(
        matches!(result, Err(SessionError::Conflict(_))),
        "{result:?}"
    );
}

#[test]
fn pending_inputs_preserve_full_attachments_and_filter_status_cutoff_limit_and_scope() {
    let (_directory, store, first) = fixture();
    let session = &first.session.id;
    bind(&store, session, "assistant-run");
    finish(&store, &first, TurnStatus::Interrupted);
    let answered = store
        .admit_channel_inbound("owner", request("answered", "conversation-1"))
        .unwrap();
    finish(&store, &answered, TurnStatus::Completed);
    confirm(&store, &answered, "confirmed-reply");
    let pending_first = store
        .admit_channel_inbound("owner", request("pending-first", "conversation-1"))
        .unwrap();
    store
        .claim_channel_assistant_input("owner", session, "assistant-run", "wait/pending")
        .unwrap()
        .unwrap();
    let pending_second = store
        .admit_channel_inbound("owner", request("pending-second", "conversation-1"))
        .unwrap();
    finish(&store, &pending_second, TurnStatus::Interrupted);
    for (inbound, status) in [
        ("completed", TurnStatus::Completed),
        ("failed", TurnStatus::Failed),
        ("stopped", TurnStatus::Stopped),
        ("excluded", TurnStatus::Interrupted),
    ] {
        let admission = store
            .admit_channel_inbound("owner", request(inbound, "conversation-1"))
            .unwrap();
        finish(&store, &admission, status);
    }
    store
        .admit_channel_inbound("owner", request("running", "conversation-1"))
        .unwrap();
    let other = store
        .admit_channel_inbound("owner", request("other", "conversation-2"))
        .unwrap();
    bind(&store, &other.session.id, "other-run");
    finish(&store, &other, TurnStatus::Completed);
    confirm(&store, &other, "other-confirmed");
    let snapshot = store.get("owner", session).unwrap();
    let events = store.events("owner", session, 0).unwrap();
    let expected = ["pending-first", "pending-second"]
        .into_iter()
        .map(|inbound| {
            store
                .get_channel_inbound("owner", session, inbound)
                .unwrap()
        })
        .collect::<Vec<_>>();
    let pending = store
        .pending_channel_assistant_inputs("owner", session, "assistant-run", "excluded", 8)
        .unwrap();
    assert_eq!(pending, expected);
    assert_eq!(pending[0].turn.id, pending_first.turn.id);
    assert_eq!(pending[0].relation.run_id.as_deref(), Some("assistant-run"));
    assert_eq!(pending[1].relation.run_id, None);
    assert_eq!(
        pending[0].request.attachments.files[0].name,
        "pending-first.txt"
    );
    assert_eq!(
        pending[1].request.attachments.files[0].sha256,
        "a".repeat(64)
    );
    assert_eq!(
        store
            .pending_channel_assistant_inputs("owner", session, "assistant-run", "excluded", 1)
            .unwrap(),
        vec![pending[1].clone()]
    );
    assert!(
        store
            .pending_channel_assistant_inputs("owner", session, "assistant-run", "excluded", 0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.pending_channel_assistant_inputs("other", session, "assistant-run", "excluded", 8),
        Err(SessionError::Missing)
    );
    assert_conflict(store.pending_channel_assistant_inputs(
        "owner",
        &other.session.id,
        "assistant-run",
        "excluded",
        8,
    ));
    assert_conflict(store.pending_channel_assistant_inputs(
        "owner",
        session,
        "other-run",
        "excluded",
        8,
    ));
    assert!(matches!(
        store.pending_channel_assistant_inputs("owner", session, "assistant-run", "", 8),
        Err(SessionError::Invalid(_))
    ));
    assert!(matches!(
        store.pending_channel_assistant_inputs(
            "owner",
            session,
            "assistant-run",
            "excluded",
            usize::MAX
        ),
        Err(SessionError::Invalid(_))
    ));
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
}

#[test]
fn explicit_retirement_allows_a_new_run_without_losing_history_or_pending_attachments() {
    let (directory, store, first) = fixture();
    let session = &first.session.id;
    let original = bind(&store, session, "old-run");
    let old_input = store
        .claim_channel_assistant_input("owner", session, "old-run", "wait/1")
        .unwrap()
        .unwrap();
    finish(&store, &first, TurnStatus::Interrupted);
    assert_conflict(store.bind_channel_assistant(
        "owner",
        session,
        "new-run",
        "wait",
        "assistant",
        "reply",
    ));
    store
        .retire_channel_assistant("owner", session, "old-run")
        .unwrap();
    assert_eq!(store.get_channel_assistant("owner", session).unwrap(), None);
    assert_conflict(store.bind_channel_assistant(
        "owner",
        session,
        "old-run",
        "wait",
        "assistant",
        "reply",
    ));
    assert_conflict(store.claim_channel_assistant_input("owner", session, "old-run", "wait/1"));
    assert_conflict(
        store.pending_channel_assistant_inputs("owner", session, "old-run", "excluded", 8),
    );
    let current = bind(&store, session, "new-run");
    let next = store
        .admit_channel_inbound("owner", request("message-2", "conversation-1"))
        .unwrap();
    let pending = store
        .pending_channel_assistant_inputs("owner", session, "new-run", "message-2", 8)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn.id, old_input.turn.id);
    assert_eq!(
        pending[0].request.attachments,
        old_input.request.attachments
    );
    assert_eq!(
        pending[0].relation.run_id.as_deref(),
        Some(original.run_id.as_str())
    );
    let new_input = store
        .claim_channel_assistant_input("owner", session, "new-run", "wait/1")
        .unwrap()
        .unwrap();
    assert_eq!(new_input.turn.id, next.turn.id);
    let snapshot = store.get("owner", session).unwrap();
    let events = store.events("owner", session, 0).unwrap();
    store
        .retire_channel_assistant("owner", session, "old-run")
        .unwrap();
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
    assert_eq!(snapshot.run_ids, ["old-run", "new-run"]);
    assert_conflict(store.retire_channel_assistant("owner", session, "new-run"));
    let other = store
        .admit_channel_inbound("other", request("other", "conversation-2"))
        .unwrap();
    assert_conflict(store.bind_channel_assistant(
        "other",
        &other.session.id,
        "old-run",
        "wait",
        "assistant",
        "reply",
    ));
    assert_conflict(store.attach_run("other", &other.session.id, "old-run"));
    assert_conflict(store.retire_channel_assistant("other", &other.session.id, "old-run"));
    assert_eq!(
        store.retire_channel_assistant("other", session, "old-run"),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.get_channel_assistant_input("other", session, "old-run", "wait/1"),
        Err(SessionError::Missing)
    );
    assert_conflict(store.get_channel_assistant_input(
        "other",
        &other.session.id,
        "old-run",
        "wait/1",
    ));
    drop(store);
    let reopened = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    assert_eq!(
        reopened.get_channel_assistant("owner", session).unwrap(),
        Some(current)
    );
    let historical = reopened
        .get_channel_assistant_input("owner", session, "old-run", "wait/1")
        .unwrap()
        .unwrap();
    assert_eq!(historical.turn.id, first.turn.id);
    assert_eq!(historical.turn.status, TurnStatus::Interrupted);
    finish(&reopened, &next, TurnStatus::Completed);
    reopened
        .retire_channel_assistant("owner", session, "new-run")
        .unwrap();
    assert_eq!(
        reopened.get_channel_assistant("owner", session).unwrap(),
        None
    );
    assert_eq!(
        reopened
            .get_channel_assistant_input("owner", session, "new-run", "wait/1")
            .unwrap()
            .unwrap()
            .turn
            .id,
        next.turn.id
    );
}

#[test]
fn retirement_requires_no_running_turn_or_unsettled_delivery_in_the_whole_session() {
    let (_directory, store, first) = fixture();
    let session = &first.session.id;
    let assistant = bind(&store, session, "assistant-run");
    assert_conflict(store.retire_channel_assistant("owner", session, "assistant-run"));
    finish(&store, &first, TurnStatus::Completed);
    for (key, status, terminal) in [
        (
            "pending",
            ChannelDeliveryStatus::Pending,
            ChannelDeliveryStatus::Confirmed,
        ),
        (
            "sending",
            ChannelDeliveryStatus::Sending,
            ChannelDeliveryStatus::Failed,
        ),
        (
            "unknown",
            ChannelDeliveryStatus::Unknown,
            ChannelDeliveryStatus::Confirmed,
        ),
    ] {
        store
            .admit_completed_channel_delivery("owner", session, &first.turn.id, delivery(key))
            .unwrap();
        if status != ChannelDeliveryStatus::Pending {
            store
                .begin_channel_delivery("owner", session, &first.turn.id, delivery(key))
                .unwrap();
        }
        if status == ChannelDeliveryStatus::Unknown {
            store
                .settle_channel_delivery(
                    "owner",
                    session,
                    delivery(key),
                    status,
                    Some("unconfirmed"),
                )
                .unwrap();
        }
        let snapshot = store.get("owner", session).unwrap();
        let events = store.events("owner", session, 0).unwrap();
        assert_conflict(store.retire_channel_assistant("owner", session, "assistant-run"));
        assert_eq!(
            store.get_channel_assistant("owner", session).unwrap(),
            Some(assistant.clone())
        );
        assert_eq!(store.get("owner", session).unwrap(), snapshot);
        assert_eq!(store.events("owner", session, 0).unwrap(), events);
        let error = (terminal == ChannelDeliveryStatus::Failed).then_some("refused");
        store
            .settle_channel_delivery("owner", session, delivery(key), terminal, error)
            .unwrap();
    }
    assert_conflict(store.retire_channel_assistant("owner", session, "missing-run"));
    store
        .retire_channel_assistant("owner", session, "assistant-run")
        .unwrap();
    assert_eq!(store.get_channel_assistant("owner", session).unwrap(), None);
}

#[test]
fn retirement_failure_rolls_back_the_current_pointer_snapshot_and_events() {
    let (directory, store, first) = fixture();
    let session = &first.session.id;
    let assistant = bind(&store, session, "assistant-run");
    store
        .claim_channel_assistant_input("owner", session, "assistant-run", "wait/1")
        .unwrap()
        .unwrap();
    finish(&store, &first, TurnStatus::Stopped);
    let connection = Connection::open(directory.path().join("sessions.sqlite")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_retirement BEFORE INSERT ON session_events
        WHEN NEW.kind = 'channel.assistant_retired' BEGIN SELECT RAISE(FAIL, 'injected retirement failure'); END;").unwrap();
    let snapshot = store.get("owner", session).unwrap();
    let events = store.events("owner", session, 0).unwrap();
    assert!(matches!(
        store.retire_channel_assistant("owner", session, "assistant-run"),
        Err(SessionError::Storage(_))
    ));
    assert_eq!(
        store.get_channel_assistant("owner", session).unwrap(),
        Some(assistant)
    );
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
    assert_eq!(
        store
            .get_channel_assistant_input("owner", session, "assistant-run", "wait/1")
            .unwrap()
            .unwrap()
            .turn
            .id,
        first.turn.id
    );
    connection
        .execute_batch("DROP TRIGGER fail_retirement")
        .unwrap();
    store
        .retire_channel_assistant("owner", session, "assistant-run")
        .unwrap();
    drop(store);
    let reopened = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    assert_eq!(
        reopened.get_channel_assistant("owner", session).unwrap(),
        None
    );
    assert_eq!(
        reopened
            .get_channel_assistant_input("owner", session, "assistant-run", "wait/1")
            .unwrap()
            .unwrap()
            .turn
            .id,
        first.turn.id
    );
}

#[test]
fn independent_connections_retire_once_and_allow_only_one_current_replacement() {
    let (directory, store, first) = fixture();
    let session = &first.session.id;
    bind(&store, session, "old-run");
    finish(&store, &first, TurnStatus::Stopped);
    let stores = (0..3)
        .map(|_| SessionStore::open(directory.path().join("sessions.sqlite")).unwrap())
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(stores.len()));
    let workers = stores
        .into_iter()
        .map(|store| {
            let barrier = barrier.clone();
            let session = session.clone();
            thread::spawn(move || {
                barrier.wait();
                store
                    .retire_channel_assistant("owner", &session, "old-run")
                    .unwrap();
                bind(&store, &session, "new-run")
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        assert_eq!(worker.join().unwrap().run_id, "new-run");
    }
    let events = store.events("owner", session, 0).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "channel.assistant_retired")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "channel.assistant_bound")
            .count(),
        2
    );
    store
        .retire_channel_assistant("owner", session, "new-run")
        .unwrap();
    let stores = (0..2)
        .map(|_| SessionStore::open(directory.path().join("sessions.sqlite")).unwrap())
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(stores.len()));
    let workers = stores
        .into_iter()
        .enumerate()
        .map(|(index, store)| {
            let barrier = barrier.clone();
            let session = session.clone();
            thread::spawn(move || {
                barrier.wait();
                store.bind_channel_assistant(
                    "owner",
                    &session,
                    &format!("contender-{index}"),
                    "wait",
                    "assistant",
                    "reply",
                )
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(SessionError::Conflict(_))))
            .count(),
        1
    );
    let connection = Connection::open(directory.path().join("sessions.sqlite")).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM channel_assistants WHERE session_id = ?1",
                [session],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        3
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM channel_assistants WHERE session_id = ?1 AND retired = 0",
                [session],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(store.get("owner", session).unwrap().run_ids.len(), 3);
}

#[test]
fn pending_selects_the_most_recent_eight_before_the_fixed_admission_in_forward_order() {
    let (directory, store, first) = fixture();
    let session = &first.session.id;
    bind(&store, session, "assistant-run");
    finish(&store, &first, TurnStatus::Interrupted);
    for index in 1..=12 {
        let admission = store
            .admit_channel_inbound(
                "owner",
                request(&format!("past-{index:02}"), "conversation-1"),
            )
            .unwrap();
        finish(&store, &admission, TurnStatus::Interrupted);
    }
    let current = store
        .admit_channel_inbound("owner", request("current", "conversation-1"))
        .unwrap();
    let concurrent = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    for index in 1..=3 {
        let admission = concurrent
            .admit_channel_inbound(
                "owner",
                request(&format!("future-{index}"), "conversation-1"),
            )
            .unwrap();
        finish(&concurrent, &admission, TurnStatus::Interrupted);
    }
    let snapshot = store.get("owner", session).unwrap();
    let events = store.events("owner", session, 0).unwrap();
    let pending = store
        .pending_channel_assistant_inputs("owner", session, "assistant-run", "current", 8)
        .unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|inbound| inbound.request.inbound_id.clone())
            .collect::<Vec<_>>(),
        (5..=12)
            .map(|index| format!("past-{index:02}"))
            .collect::<Vec<_>>()
    );
    assert!(
        pending
            .iter()
            .all(|inbound| inbound.turn.id != current.turn.id && inbound.relation.run_id.is_none())
    );
    assert_eq!(pending[0].request.attachments.files[0].name, "past-05.txt");
    assert_eq!(pending[7].request.attachments.files[0].name, "past-12.txt");
    assert_eq!(
        store
            .get_channel_inbound("owner", session, "past-01")
            .unwrap()
            .request
            .attachments
            .files[0]
            .name,
        "past-01.txt"
    );
    assert_eq!(store.list_turns("owner", session).unwrap().len(), 17);
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
}

#[test]
fn pending_requires_an_existing_excluded_admission_in_the_exact_owned_session() {
    let (_directory, store, first) = fixture();
    let session = &first.session.id;
    bind(&store, session, "assistant-run");
    finish(&store, &first, TurnStatus::Interrupted);
    let current = store
        .admit_channel_inbound("owner", request("current", "conversation-1"))
        .unwrap();
    let foreign = store
        .admit_channel_inbound("owner", request("foreign", "conversation-2"))
        .unwrap();
    let snapshot = store.get("owner", session).unwrap();
    let events = store.events("owner", session, 0).unwrap();
    for exclude in ["missing", foreign.relation.inbound_id.as_str()] {
        for limit in [0, 8] {
            assert_eq!(
                store.pending_channel_assistant_inputs(
                    "owner",
                    session,
                    "assistant-run",
                    exclude,
                    limit
                ),
                Err(SessionError::Missing)
            );
        }
    }
    assert_eq!(
        store
            .pending_channel_assistant_inputs(
                "owner",
                session,
                "assistant-run",
                &current.relation.inbound_id,
                8
            )
            .unwrap()[0]
            .turn
            .id,
        first.turn.id
    );
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
}

#[test]
fn current_binding_blocks_session_deletion_until_retired_then_history_cascades() {
    let (directory, store, first) = fixture();
    let session = &first.session.id;
    let assistant = bind(&store, session, "assistant-run");
    store
        .claim_channel_assistant_input("owner", session, "assistant-run", "wait/1")
        .unwrap()
        .unwrap();
    finish(&store, &first, TurnStatus::Stopped);
    let connection = Connection::open(directory.path().join("sessions.sqlite")).unwrap();
    let mut snapshot = store.get("owner", session).unwrap();
    snapshot.run_ids.clear();
    connection
        .execute(
            "UPDATE sessions SET data = ?3 WHERE owner = ?1 AND id = ?2",
            params!["owner", session, serde_json::to_string(&snapshot).unwrap()],
        )
        .unwrap();
    let events = store.events("owner", session, 0).unwrap();
    assert_eq!(
        store.delete_channel_session("other", session),
        Err(SessionError::Missing)
    );
    assert_conflict(store.delete_channel_session("owner", session));
    assert_eq!(
        store.get_channel_assistant("owner", session).unwrap(),
        Some(assistant)
    );
    assert_eq!(store.get("owner", session).unwrap(), snapshot);
    assert_eq!(store.events("owner", session, 0).unwrap(), events);
    store
        .retire_channel_assistant("owner", session, "assistant-run")
        .unwrap();
    assert_eq!(
        store
            .get_channel_assistant_input("owner", session, "assistant-run", "wait/1")
            .unwrap()
            .unwrap()
            .turn
            .id,
        first.turn.id
    );
    store.delete_channel_session("owner", session).unwrap();
    assert_eq!(store.get("owner", session), Err(SessionError::Missing));
    for table in [
        "channel_assistants",
        "channel_assistant_inputs",
        "channel_inbounds",
        "channel_sessions",
        "turns",
        "session_events",
    ] {
        assert_eq!(
            connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
                    [session],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
}
