use super::*;
use crate::{ChannelIdentity, ChannelInboundRequest};

fn resolve_task_result_silently(
    store: &SessionStore,
    owner: &str,
    session: &str,
    run: &str,
    key: &str,
) {
    let AssistantInput::Task(input) = store
        .get_assistant_input(owner, session, run, key)
        .unwrap()
        .unwrap()
    else {
        panic!("expected task input");
    };
    if store
        .task_result_resolution(owner, session, &input.result.id)
        .unwrap()
        .is_none()
    {
        store
            .resolve_task_result(
                owner,
                session,
                run,
                key,
                &TaskResultResolution::Silent {
                    reason: "This fixture consumes the result without a channel notification"
                        .into(),
                },
            )
            .unwrap();
    }
}

#[test]
fn explicit_resolution_is_required_durable_and_not_a_delivery_or_turn_completion() {
    let (root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    store
        .record_task_result("owner", &session, &result("first"))
        .unwrap();
    let input = store
        .claim_assistant_input("owner", &session, "run", "task")
        .unwrap()
        .unwrap();
    assert!(
        matches!(store.finish_task_result("owner", &session, "run", "task"), Err(SessionError::Conflict(reason)) if reason.contains("explicit"))
    );
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap().len(),
        1
    );
    assert!(
        store
            .resolve_task_result(
                "owner",
                &session,
                "run",
                "task",
                &TaskResultResolution::Silent { reason: " ".into() }
            )
            .is_err()
    );
    resolve_task_result_silently(&store, "owner", &session, "run", "task");
    let decision = store
        .task_result_resolution("owner", &session, "first")
        .unwrap()
        .unwrap();
    assert!(matches!(decision, TaskResultResolution::Silent { .. }));
    assert_eq!(
        store
            .get_turn("owner", &session, &input.turn().id)
            .unwrap()
            .status,
        TurnStatus::Running
    );
    let events = store.events("owner", &session, 0).unwrap();
    store
        .resolve_task_result("owner", &session, "run", "task", &decision)
        .unwrap();
    assert_eq!(store.events("owner", &session, 0).unwrap(), events);
    assert!(
        store
            .resolve_task_result(
                "owner",
                &session,
                "run",
                "task",
                &TaskResultResolution::Delivered {
                    requests: vec!["invented-send".into()]
                }
            )
            .is_err()
    );
    drop(store);
    let store = SessionStore::open(root.path().join("session.sqlite")).unwrap();
    assert_eq!(
        store
            .task_result_resolution("owner", &session, "first")
            .unwrap(),
        Some(decision.clone())
    );
    store
        .admit_channel_inbound("owner", inbound("urgent-after-resolution"))
        .unwrap();
    assert!(
        store
            .finish_task_result("owner", &session, "run", "task")
            .is_err()
    );
    complete_user(&store, &session, "urgent");
    let resumed = store
        .claim_assistant_input("owner", &session, "run", "resumed-task")
        .unwrap()
        .unwrap();
    assert_ne!(resumed.turn().id, input.turn().id);
    assert_eq!(
        store
            .task_result_resolution("owner", &session, "first")
            .unwrap(),
        Some(decision)
    );
    store
        .finish_task_result("owner", &session, "run", "resumed-task")
        .unwrap();
    assert!(
        store
            .pending_task_results("owner", &session)
            .unwrap()
            .is_empty()
    );
}

fn inbound(id: &str) -> ChannelInboundRequest {
    ChannelInboundRequest {
        inbound_id: id.into(),
        bot_origin: None,
        platform_message_id: None,
        identity: ChannelIdentity {
            source: "wecom".into(),
            account: Some("bot".into()),
            conversation_kind: Default::default(),
            conversation_id: "alice".into(),
            sender_id: "alice".into(),
        },
        graph: "assistant".into(),
        reply_node: "agent".into(),
        text: Some(id.into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
    }
}

#[test]
fn submitted_send_survives_restart_and_cannot_be_acknowledged_as_silence_or_other_receipts() {
    let (root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    store
        .record_task_result("owner", &session, &result("queued-send"))
        .unwrap();
    store
        .claim_assistant_input("owner", &session, "run", "task")
        .unwrap()
        .unwrap();
    let requested: Vec<String> = (0..1025)
        .map(|index| format!("submitted-{index:04}"))
        .collect();
    assert!(matches!(
        store.request_task_result_delivery("owner", &session, "run", "task", &requested),
        Err(SessionError::Invalid(_))
    ));
    for batch in requested.chunks(1024) {
        store
            .request_task_result_delivery("owner", &session, "run", "task", batch)
            .unwrap();
    }
    let events = store.events("owner", &session, 0).unwrap();
    for batch in requested.chunks(1024) {
        store
            .request_task_result_delivery("owner", &session, "run", "task", batch)
            .unwrap();
    }
    assert_eq!(store.events("owner", &session, 0).unwrap(), events);
    drop(store);
    let store = SessionStore::open(root.path().join("session.sqlite")).unwrap();
    assert_eq!(
        store
            .task_result_delivery_requests("owner", &session, "queued-send")
            .unwrap(),
        requested
    );
    for resolution in [
        TaskResultResolution::Silent {
            reason: "No Gateway record visible yet".into(),
        },
        TaskResultResolution::Delivered {
            requests: vec!["another-confirmed-send".into()],
        },
        TaskResultResolution::Delivered { requests: vec![] },
        TaskResultResolution::Delivered {
            requests: [requested.clone(), vec![requested[0].clone()]].concat(),
        },
        TaskResultResolution::Delivered {
            requests: [requested.clone(), vec!["invalid/reference".into()]].concat(),
        },
    ] {
        assert!(
            store
                .resolve_task_result("owner", &session, "run", "task", &resolution)
                .is_err()
        );
    }
    assert!(
        store
            .finish_task_result("owner", &session, "run", "task")
            .is_err()
    );
    // This low-level fixture stands in for the Host's separately tested Gateway
    // check; the model-facing API never accepts these references as arguments.
    store
        .resolve_task_result(
            "owner",
            &session,
            "run",
            "task",
            &TaskResultResolution::Delivered {
                requests: requested,
            },
        )
        .unwrap();
    assert!(
        store
            .request_task_result_delivery("owner", &session, "run", "task", &["late-send".into()])
            .is_err()
    );
}

fn result(id: &str) -> TaskResult {
    TaskResult {
        id: id.into(),
        source_run: Some(format!("source-{id}")),
        outcome: "completed".into(),
        content: json!({"text":id}),
        files: vec![],
    }
}

fn fixture() -> (tempfile::TempDir, SessionStore, String) {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::open(root.path().join("session.sqlite")).unwrap();
    let admitted = store
        .admit_channel_inbound("owner", inbound("initial"))
        .unwrap();
    let session = admitted.session.id;
    store
        .bind_channel_assistant("owner", &session, "run", "wait", "agent", "reply")
        .unwrap();
    (root, store, session)
}

fn complete_user(store: &SessionStore, session: &str, key: &str) {
    let input = store
        .claim_assistant_input("owner", session, "run", key)
        .unwrap()
        .unwrap();
    assert!(input.user().is_some());
    store
        .finish_turn(
            "owner",
            session,
            &input.turn().id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
}

#[test]
fn queued_results_are_fifo_idempotent_and_never_impersonate_users() {
    let (_root, store, session) = fixture();
    let first = result("first");
    let second = result("second");
    for result in [&first, &second, &first] {
        store.record_task_result("owner", &session, result).unwrap();
    }
    assert!(matches!(
        store.record_task_result("other", &session, &first),
        Err(SessionError::Missing)
    ));
    assert!(matches!(
        store.record_task_result(
            "owner",
            &session,
            &TaskResult {
                content: json!("changed"),
                ..first.clone()
            }
        ),
        Err(SessionError::Conflict(_))
    ));
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![first.clone(), second.clone()]
    );
    // Even when both results were queued first, the live user Turn is claimed first.
    complete_user(&store, &session, "user");
    let input = store
        .claim_assistant_input("owner", &session, "run", "task-1")
        .unwrap()
        .unwrap();
    let AssistantInput::Task(task) = &input else {
        panic!("expected task")
    };
    assert_eq!(task.result, first);
    assert!(input.user().is_none());
    assert_eq!(
        store
            .claim_assistant_input("owner", &session, "run", "task-1")
            .unwrap(),
        Some(input.clone())
    );
    assert!(
        store
            .claim_assistant_input("owner", &session, "run", "task-2")
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.get_channel_inbound("owner", &session, &first.id),
        Err(SessionError::Missing)
    ));
    resolve_task_result_silently(&store, "owner", &session, "run", "task-1");
    store
        .finish_task_result("owner", &session, "run", "task-1")
        .unwrap();
    resolve_task_result_silently(&store, "owner", &session, "run", "task-1");
    store
        .finish_task_result("owner", &session, "run", "task-1")
        .unwrap();
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![second.clone()]
    );
    assert_eq!(
        store.task_result("owner", &session, "first").unwrap(),
        Some(first)
    );
    let AssistantInput::Task(next) = store
        .claim_assistant_input("owner", &session, "run", "task-2")
        .unwrap()
        .unwrap()
    else {
        panic!("expected task")
    };
    assert_eq!(next.result, second);
}

#[test]
fn cancellation_is_durable_idempotent_and_preserves_the_queued_result() {
    let (root, store, session) = fixture();
    let cancelled = result("cancelled");
    let retained = result("retained");
    assert!(
        !store
            .task_result_cancelled("owner", &session, &cancelled.id)
            .unwrap()
    );
    assert!(matches!(
        store.cancel_task_result("owner", &session, &cancelled.id, "subscription removed"),
        Err(SessionError::Missing)
    ));
    for result in [&cancelled, &retained] {
        store.record_task_result("owner", &session, result).unwrap();
    }
    let before = store.events("owner", &session, 0).unwrap();
    assert!(matches!(
        store.cancel_task_result("other", &session, &cancelled.id, "subscription removed"),
        Err(SessionError::Missing)
    ));
    assert!(matches!(
        store.task_result_cancelled("other", &session, &cancelled.id),
        Err(SessionError::Missing)
    ));
    assert!(matches!(
        store.cancel_task_result("owner", &session, &cancelled.id, " "),
        Err(SessionError::Invalid(_))
    ));
    assert_eq!(store.events("owner", &session, 0).unwrap(), before);
    store
        .cancel_task_result("owner", &session, &cancelled.id, "subscription removed")
        .unwrap();
    let after = store.events("owner", &session, 0).unwrap();
    assert_eq!(&after[..before.len()], before);
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(after.last().unwrap().kind, CANCELLED);
    assert_eq!(after.last().unwrap().data["reason"], "subscription removed");
    assert!(
        !after
            .iter()
            .any(|event| event.kind == HANDLED || event.kind == CLAIMED)
    );
    drop(store);

    let store = SessionStore::open(root.path().join("session.sqlite")).unwrap();
    store
        .record_task_result("owner", &session, &cancelled)
        .unwrap();
    store
        .cancel_task_result(
            "owner",
            &session,
            &cancelled.id,
            "retry with new explanation",
        )
        .unwrap();
    assert_eq!(store.events("owner", &session, 0).unwrap(), after);
    assert!(
        store
            .task_result_cancelled("owner", &session, &cancelled.id)
            .unwrap()
    );
    assert_eq!(
        store.task_result("owner", &session, &cancelled.id).unwrap(),
        Some(cancelled)
    );
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![retained.clone()]
    );
    complete_user(&store, &session, "initial");
    let AssistantInput::Task(next) = store
        .claim_assistant_input("owner", &session, "run", "next")
        .unwrap()
        .unwrap()
    else {
        panic!("expected the uncancelled result")
    };
    assert_eq!(next.result, retained);
}

#[test]
fn cancelling_a_claim_preserves_its_running_turn_and_normal_completion() {
    let (_root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    let result = result("claimed");
    store
        .record_task_result("owner", &session, &result)
        .unwrap();
    let claimed = store
        .claim_assistant_input("owner", &session, "run", "task")
        .unwrap()
        .unwrap();
    let events = store.events("owner", &session, 0).unwrap();
    store
        .cancel_task_result("owner", &session, &result.id, "permission withdrawn")
        .unwrap();
    assert!(
        store
            .pending_task_results("owner", &session)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .claim_assistant_input("owner", &session, "run", "task")
            .unwrap(),
        Some(claimed.clone())
    );
    assert_eq!(
        store
            .get_assistant_input("owner", &session, "run", "task")
            .unwrap(),
        Some(claimed)
    );
    assert_eq!(
        &store.events("owner", &session, 0).unwrap()[..events.len()],
        events
    );
    resolve_task_result_silently(&store, "owner", &session, "run", "task");
    store
        .finish_task_result("owner", &session, "run", "task")
        .unwrap();
    let finished = store
        .get_assistant_input("owner", &session, "run", "task")
        .unwrap()
        .unwrap();
    assert_eq!(finished.turn().status, TurnStatus::Completed);
    let events = store.events("owner", &session, 0).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == CANCELLED)
            .count(),
        1
    );
    assert_eq!(
        events.iter().filter(|event| event.kind == CLAIMED).count(),
        1
    );
    assert_eq!(
        events.iter().filter(|event| event.kind == HANDLED).count(),
        1
    );
    store
        .cancel_task_result("owner", &session, &result.id, "duplicate")
        .unwrap();
    resolve_task_result_silently(&store, "owner", &session, "run", "task");
    store
        .finish_task_result("owner", &session, "run", "task")
        .unwrap();
    assert_eq!(store.events("owner", &session, 0).unwrap(), events);
    assert!(
        store
            .task_result_cancelled("owner", &session, &result.id)
            .unwrap()
    );
    assert_eq!(
        store.task_result("owner", &session, &result.id).unwrap(),
        Some(result)
    );
}

#[test]
fn protected_results_retain_cancelled_running_claims_until_finished() {
    let (_root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    let first = result("first");
    let second = result("second");
    for result in [&first, &second] {
        store.record_task_result("owner", &session, result).unwrap();
    }
    assert!(matches!(
        store.protected_task_results("other", &session),
        Err(SessionError::Missing)
    ));
    assert_eq!(
        store.protected_task_results("owner", &session).unwrap(),
        vec![first.clone(), second.clone()]
    );
    store
        .claim_assistant_input("owner", &session, "run", "task")
        .unwrap()
        .unwrap();
    // Handover adds another claim fact for the same Turn, not another result.
    store
        .handover_channel_assistant("owner", &session, "run", "successor", "new-wait")
        .unwrap();
    let before = store.events("owner", &session, 0).unwrap();
    assert_eq!(
        store.protected_task_results("owner", &session).unwrap(),
        vec![first.clone(), second.clone()]
    );
    assert_eq!(store.events("owner", &session, 0).unwrap(), before);
    store
        .cancel_task_result("owner", &session, &first.id, "unsubscribed")
        .unwrap();
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![second.clone()]
    );
    assert_eq!(
        store.protected_task_results("owner", &session).unwrap(),
        vec![first, second.clone()]
    );
    resolve_task_result_silently(&store, "owner", &session, "successor", "new-wait");
    store
        .finish_task_result("owner", &session, "successor", "new-wait")
        .unwrap();
    assert_eq!(
        store.protected_task_results("owner", &session).unwrap(),
        vec![second.clone()]
    );
    store
        .cancel_task_result("owner", &session, &second.id, "unsubscribed")
        .unwrap();
    assert!(
        store
            .protected_task_results("owner", &session)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cancelled_interrupted_result_is_not_claimed_by_a_new_wait() {
    let (_root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    let result = result("interrupted");
    store
        .record_task_result("owner", &session, &result)
        .unwrap();
    let original = store
        .claim_assistant_input("owner", &session, "run", "task")
        .unwrap()
        .unwrap();
    store
        .admit_channel_inbound("owner", inbound("urgent"))
        .unwrap();
    store
        .cancel_task_result("owner", &session, &result.id, "unsubscribed")
        .unwrap();
    assert!(
        store
            .protected_task_results("owner", &session)
            .unwrap()
            .is_empty()
    );
    complete_user(&store, &session, "urgent");
    assert!(
        store
            .claim_assistant_input("owner", &session, "run", "next")
            .unwrap()
            .is_none()
    );
    let retained = store
        .get_assistant_input("owner", &session, "run", "task")
        .unwrap()
        .unwrap();
    assert_eq!(retained.turn().id, original.turn().id);
    assert_eq!(retained.turn().status, TurnStatus::Interrupted);
    assert!(matches!(
        store.finish_task_result("owner", &session, "run", "task"),
        Err(SessionError::Conflict(_))
    ));
    assert!(
        !store
            .events("owner", &session, 0)
            .unwrap()
            .iter()
            .any(|event| event.kind == HANDLED)
    );
}

#[test]
fn concurrent_cancellation_replay_and_claim_keep_one_cancellation_fact() {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };
    let (root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    store
        .record_task_result("owner", &session, &result("same"))
        .unwrap();
    let barrier = Arc::new(Barrier::new(4));
    let workers = (0..3)
        .map(|index| {
            let store = SessionStore::open(root.path().join("session.sqlite")).unwrap();
            let session = session.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                if index == 2 {
                    store
                        .claim_assistant_input("owner", &session, "run", "task")
                        .unwrap()
                } else {
                    store
                        .record_task_result("owner", &session, &result("same"))
                        .unwrap();
                    store
                        .cancel_task_result("owner", &session, "same", "unsubscribed")
                        .unwrap();
                    None
                }
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let claims = workers
        .into_iter()
        .filter_map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    let events = store.events("owner", &session, 0).unwrap();
    assert_eq!(
        events.iter().filter(|event| event.kind == QUEUED).count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == CANCELLED)
            .count(),
        1
    );
    assert_eq!(
        events.iter().filter(|event| event.kind == CLAIMED).count(),
        claims.len()
    );
    assert!(!events.iter().any(|event| event.kind == HANDLED));
    assert!(
        store
            .pending_task_results("owner", &session)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .claim_assistant_input("owner", &session, "run", "later")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .task_result_cancelled("owner", &session, "same")
            .unwrap()
    );
    assert_eq!(
        store.task_result("owner", &session, "same").unwrap(),
        Some(result("same"))
    );
}

#[test]
fn shared_result_is_consumed_independently_by_sessions_across_restart() {
    let (root, store, alice) = fixture();
    let mut request = inbound("bob-initial");
    request.graph = "feishu-assistant".into();
    request.identity.source = "feishu".into();
    request.identity.account = Some("feishu-bot".into());
    request.identity.conversation_id = "bob".into();
    request.identity.sender_id = "bob".into();
    let bob = store
        .admit_channel_inbound("owner", request)
        .unwrap()
        .session
        .id;
    store
        .bind_channel_assistant("owner", &bob, "bob-run", "wait", "agent", "reply")
        .unwrap();
    let mut shared = result("shared-result");
    shared.files.push(crate::ChannelNotificationFile {
        artifact_id: "shared-artifact".into(),
        path: "published/report.pdf".into(),
        name: "report.pdf".into(),
    });
    for (session, run) in [(&alice, "run"), (&bob, "bob-run")] {
        let user = store
            .claim_assistant_input("owner", session, run, "initial")
            .unwrap()
            .unwrap();
        store
            .finish_turn(
                "owner",
                session,
                &user.turn().id,
                TurnStatus::Completed,
                None,
            )
            .unwrap();
        store.record_task_result("owner", session, &shared).unwrap();
        let input = store
            .claim_assistant_input("owner", session, run, "shared-wait")
            .unwrap()
            .unwrap();
        let AssistantInput::Task(task) = input else {
            panic!("expected task result, not a user message")
        };
        assert_eq!(task.result, shared);
    }
    resolve_task_result_silently(&store, "owner", &alice, "run", "shared-wait");
    store
        .finish_task_result("owner", &alice, "run", "shared-wait")
        .unwrap();
    assert_eq!(
        store.pending_task_results("owner", &bob).unwrap(),
        vec![shared.clone()]
    );
    drop(store);

    let store = SessionStore::open(root.path().join("session.sqlite")).unwrap();
    for session in [&alice, &bob] {
        store.record_task_result("owner", session, &shared).unwrap();
    }
    assert!(
        store
            .pending_task_results("owner", &alice)
            .unwrap()
            .is_empty()
    );
    let AssistantInput::Task(pending) = store
        .claim_assistant_input("owner", &bob, "bob-run", "shared-wait")
        .unwrap()
        .unwrap()
    else {
        panic!("expected Bob's retained result claim")
    };
    assert_eq!(pending.result, shared);
    resolve_task_result_silently(&store, "owner", &bob, "bob-run", "shared-wait");
    store
        .finish_task_result("owner", &bob, "bob-run", "shared-wait")
        .unwrap();
    assert!(
        store
            .pending_task_results("owner", &bob)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn user_interrupt_preserves_result_until_a_later_attempt_handles_it() {
    let (_root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    store
        .record_task_result("owner", &session, &result("first"))
        .unwrap();
    let first = store
        .claim_assistant_input("owner", &session, "run", "task-1")
        .unwrap()
        .unwrap();
    store
        .admit_channel_inbound("owner", inbound("urgent"))
        .unwrap();
    assert!(matches!(
        store.claim_channel_assistant_input("owner", &session, "run", "task-1"),
        Err(SessionError::Conflict(_))
    ));
    let old = store
        .get_assistant_input("owner", &session, "run", "task-1")
        .unwrap()
        .unwrap();
    assert_eq!(old.turn().id, first.turn().id);
    assert_eq!(old.turn().status, TurnStatus::Interrupted);
    assert!(matches!(
        store.finish_task_result("owner", &session, "run", "task-1"),
        Err(SessionError::Conflict(_))
    ));
    complete_user(&store, &session, "urgent");
    let next = store
        .claim_assistant_input("owner", &session, "run", "task-2")
        .unwrap()
        .unwrap();
    assert_ne!(next.turn().id, first.turn().id);
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![result("first")]
    );
    resolve_task_result_silently(&store, "owner", &session, "run", "task-2");
    store
        .finish_task_result("owner", &session, "run", "task-2")
        .unwrap();
    assert!(
        store
            .pending_task_results("owner", &session)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn channel_session_deletion_preserves_unclaimed_results_until_handled() {
    let (_root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    store
        .retire_channel_assistant("owner", &session, "run")
        .unwrap();
    {
        let connection = store.lock().unwrap();
        let mut snapshot = store::read_session(&connection, "owner", &session).unwrap();
        snapshot.run_ids.clear();
        store::write_session(&connection, "owner", &snapshot).unwrap();
    }
    let result = result("unclaimed");
    store
        .record_task_result("owner", &session, &result)
        .unwrap();
    let snapshot = store.get("owner", &session).unwrap();
    let events = store.events("owner", &session, 0).unwrap();
    assert!(matches!(
        store.delete_channel_session("owner", &session),
        Err(SessionError::Conflict(reason)) if reason.contains("unhandled task results")
    ));
    assert_eq!(store.get("owner", &session).unwrap(), snapshot);
    assert_eq!(store.events("owner", &session, 0).unwrap(), events);
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![result]
    );

    store
        .bind_channel_assistant("owner", &session, "successor", "wait", "agent", "reply")
        .unwrap();
    store
        .claim_assistant_input("owner", &session, "successor", "task-wait")
        .unwrap()
        .unwrap();
    resolve_task_result_silently(&store, "owner", &session, "successor", "task-wait");
    store
        .finish_task_result("owner", &session, "successor", "task-wait")
        .unwrap();
    store
        .retire_channel_assistant("owner", &session, "successor")
        .unwrap();
    {
        let connection = store.lock().unwrap();
        let mut snapshot = store::read_session(&connection, "owner", &session).unwrap();
        snapshot.run_ids.clear();
        store::write_session(&connection, "owner", &snapshot).unwrap();
    }
    store.delete_channel_session("owner", &session).unwrap();
    assert_eq!(store.get("owner", &session), Err(SessionError::Missing));
}

#[test]
fn independent_connections_queue_once_and_competing_waits_claim_one_result() {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };
    let (root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    let barrier = Arc::new(Barrier::new(3));
    let workers = (0..2)
        .map(|index| {
            let store = SessionStore::open(root.path().join("session.sqlite")).unwrap();
            let session = session.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                store
                    .record_task_result("owner", &session, &result("same"))
                    .unwrap();
                store
                    .claim_assistant_input("owner", &session, "run", &format!("wait-{index}"))
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let claims = workers
        .into_iter()
        .filter_map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![result("same")]
    );
    let events = store.events("owner", &session, 0).unwrap();
    assert_eq!(
        events.iter().filter(|event| event.kind == QUEUED).count(),
        1
    );
    assert_eq!(
        events.iter().filter(|event| event.kind == CLAIMED).count(),
        1
    );
    store
        .lock()
        .unwrap()
        .busy_timeout(std::time::Duration::ZERO)
        .unwrap();
    let writer = Connection::open(root.path().join("session.sqlite")).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let key = events
        .iter()
        .find(|event| event.kind == CLAIMED)
        .unwrap()
        .data["key"]
        .as_str()
        .unwrap();
    let replay = store.claim_assistant_input("owner", &session, "run", key);
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(replay.unwrap().unwrap().turn().id, claims[0].turn().id);
}

#[test]
fn restart_and_handover_preserve_task_claim_without_reviving_retired_assistants() {
    let (root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    store
        .record_task_result("owner", &session, &result("first"))
        .unwrap();
    let input = store
        .claim_assistant_input("owner", &session, "run", "task-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .interrupt_running_except_assistants(&["run".into()])
            .unwrap(),
        0
    );
    store
        .handover_channel_assistant("owner", &session, "run", "successor", "new-wait")
        .unwrap();
    assert!(matches!(
        store.finish_task_result("owner", &session, "run", "task-1"),
        Err(SessionError::Conflict(_))
    ));
    drop(store);
    let store = SessionStore::open(root.path().join("session.sqlite")).unwrap();
    let carried = store
        .claim_assistant_input("owner", &session, "successor", "new-wait")
        .unwrap()
        .unwrap();
    assert_eq!(carried.turn().id, input.turn().id);
    assert_eq!(carried.turn().runs, vec!["run", "successor"]);
    assert_eq!(
        store
            .interrupt_running_except_assistants(&["successor".into()])
            .unwrap(),
        0
    );
    resolve_task_result_silently(&store, "owner", &session, "successor", "new-wait");
    store
        .finish_task_result("owner", &session, "successor", "new-wait")
        .unwrap();
    store
        .retire_channel_assistant("owner", &session, "successor")
        .unwrap();
    store
        .record_task_result("owner", &session, &result("later"))
        .unwrap();
    assert!(matches!(
        store.claim_assistant_input("owner", &session, "successor", "later"),
        Err(SessionError::Conflict(_))
    ));
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![result("later")]
    );
}

#[test]
fn result_file_selection_must_stay_inside_the_selected_artifact() {
    let (_root, store, session) = fixture();
    for (name, path) in [
        ("../escape", "report.md"),
        ("report.md", "../other"),
        ("report.md", "/absolute"),
        ("report.md", "a\\b"),
    ] {
        let mut selected = result("bad");
        selected.files.push(crate::ChannelNotificationFile {
            artifact_id: "artifact".into(),
            name: name.into(),
            path: path.into(),
        });
        assert!(matches!(
            store.record_task_result("owner", &session, &selected),
            Err(SessionError::Invalid(_))
        ));
    }
}

#[test]
fn admission_blocked_status_uses_no_fabricated_run_or_participant_message() {
    let (_root, store, session) = fixture();
    complete_user(&store, &session, "initial");
    let mut notice = result("schedule-blocked");
    notice.source_run = None;
    assert!(matches!(
        store.record_task_result("owner", &session, &notice),
        Err(SessionError::Invalid(_))
    ));
    notice.outcome = "blocked".into();
    notice.content = json!({"event":"schedule.blocked","schedule":"existing-plan"});
    store
        .record_task_result("owner", &session, &notice)
        .unwrap();
    store
        .record_task_result("owner", &session, &notice)
        .unwrap();
    let AssistantInput::Task(input) = store
        .claim_assistant_input("owner", &session, "run", "notice")
        .unwrap()
        .unwrap()
    else {
        panic!("expected system input")
    };
    assert_eq!(input.result, notice);
    assert!(input.turn.prompt.is_none());
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![notice]
    );
}
