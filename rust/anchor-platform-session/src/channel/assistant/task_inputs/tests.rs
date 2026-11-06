use super::*;
use crate::{ChannelIdentity, ChannelInboundRequest};

fn inbound(id: &str) -> ChannelInboundRequest {
    ChannelInboundRequest {
        inbound_id: id.into(),
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
    store
        .finish_task_result("owner", &session, "run", "task-1")
        .unwrap();
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
