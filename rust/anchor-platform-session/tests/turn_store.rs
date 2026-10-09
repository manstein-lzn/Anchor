use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use anchor_platform_session::{
    CreateSession, Session, SessionError, SessionStatus, SessionStore, Turn, TurnEvent, TurnStatus,
};
use chrono::Utc;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf, SessionStore) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.sqlite");
    let store = SessionStore::open(&path).unwrap();
    create(&store, "alice", "pilot");
    (directory, path, store)
}

fn create(store: &SessionStore, owner: &str, session: &str) -> Session {
    store
        .create(
            owner,
            CreateSession {
                id: Some(session.into()),
                ..Default::default()
            },
        )
        .unwrap()
}

fn start(store: &SessionStore, request: &str) -> Turn {
    let (turn, created) = store
        .create_turn("alice", "pilot", request, Some("hello"))
        .unwrap();
    assert!(created);
    turn
}

fn assert_conflict<ResultValue>(result: Result<ResultValue, SessionError>) {
    assert!(matches!(result, Err(SessionError::Conflict(_))));
}

fn assert_invalid<ResultValue>(result: Result<ResultValue, SessionError>) {
    assert!(matches!(result, Err(SessionError::Invalid(_))));
}

fn assert_storage<ResultValue>(result: Result<ResultValue, SessionError>) {
    assert!(matches!(result, Err(SessionError::Storage(_))));
}

#[test]
fn turn_dto_and_statuses_have_the_fixed_public_shape() {
    for (status, text) in [
        (TurnStatus::Running, "running"),
        (TurnStatus::Completed, "completed"),
        (TurnStatus::Failed, "failed"),
        (TurnStatus::Stopped, "stopped"),
        (TurnStatus::Interrupted, "interrupted"),
    ] {
        assert_eq!(serde_json::to_value(status).unwrap(), text);
        assert_eq!(
            serde_json::from_value::<TurnStatus>(json!(text)).unwrap(),
            status
        );
    }
    let (_directory, _, store) = fixture();
    let turn = start(&store, "req-1");
    uuid::Uuid::parse_str(&turn.id).unwrap();
    assert_eq!(turn.created_at, turn.updated_at);
    let dto = serde_json::to_value(&turn).unwrap();
    assert_eq!(
        dto,
        json!({
            "id": turn.id, "session": "pilot", "request_id": "req-1", "prompt": "hello",
            "status": "running", "error": null, "created_at": turn.created_at, "updated_at": turn.updated_at,
            "native": null, "goose": null, "runs": [],
        })
    );
    assert_eq!(serde_json::from_value::<Turn>(dto).unwrap(), turn);
    assert_eq!(
        serde_json::to_value(TurnEvent {
            seq: 2,
            data: json!(null)
        })
        .unwrap(),
        json!({"seq": 2, "data": null})
    );
}

#[test]
fn admission_reactivates_session_and_writes_durable_lifecycle_events() {
    let (_directory, _, store) = fixture();
    store
        .set_status(
            "alice",
            "pilot",
            SessionStatus::WaitingUser,
            "answer needed",
        )
        .unwrap();
    let turn = start(&store, "req-1");
    let session = store.get("alice", "pilot").unwrap();
    assert_eq!(session.status, SessionStatus::Active);
    assert!(session.waiting_reason.is_empty());
    assert_eq!(session.updated_at, turn.created_at);
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    assert_eq!(
        events,
        vec![TurnEvent {
            seq: 1,
            data: json!({"type": "turn.started", "turn": turn})
        }]
    );
    let activity = store.events("alice", "pilot", 2).unwrap();
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].kind, "turn.started");
    assert_eq!(activity[0].data["turn"], turn.id);
    assert_eq!(activity[0].at, session.updated_at);
}

#[test]
fn same_request_and_exact_input_return_the_original_turn_without_events() {
    let (_directory, _, store) = fixture();
    let turn = start(&store, "req-1");
    let snapshot = store.get("alice", "pilot").unwrap();
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    assert_eq!(
        store
            .create_turn("alice", "pilot", "req-1", Some("hello"))
            .unwrap(),
        (turn.clone(), false)
    );
    for prompt in [None, Some("Hello"), Some("hello ")] {
        assert_conflict(store.create_turn("alice", "pilot", "req-1", prompt));
    }
    assert_conflict(store.create_turn("alice", "pilot", "req-2", Some("new")));
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(
        store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        events
    );
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    assert_eq!(store.list_turns("alice", "pilot").unwrap(), vec![turn]);
}

#[test]
fn terminal_requests_remain_idempotent_even_after_a_new_turn_and_archiving() {
    let (_directory, _, store) = fixture();
    let first = start(&store, "req-1");
    let completed = store
        .finish_turn("alice", "pilot", &first.id, TurnStatus::Completed, None)
        .unwrap();
    let second = start(&store, "req-2");
    assert_eq!(
        store
            .create_turn("alice", "pilot", "req-1", Some("hello"))
            .unwrap(),
        (completed.clone(), false)
    );
    assert_conflict(store.create_turn("alice", "pilot", "req-1", Some("different")));
    store
        .finish_turn("alice", "pilot", &second.id, TurnStatus::Stopped, None)
        .unwrap();
    let archived = store
        .set_status("alice", "pilot", SessionStatus::Archived, "saved")
        .unwrap();
    let events = store.events("alice", "pilot", 0).unwrap();
    assert_eq!(
        store
            .create_turn("alice", "pilot", "req-1", Some("hello"))
            .unwrap(),
        (completed, false)
    );
    assert_conflict(store.create_turn("alice", "pilot", "new", None));
    assert_eq!(store.get("alice", "pilot").unwrap(), archived);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), events);
}

#[test]
fn validation_counts_utf8_bytes_and_preserves_input_while_none_allows_resume() {
    let (_directory, _, store) = fixture();
    for request in [
        "".to_owned(),
        " ".to_owned(),
        "x".repeat(129),
        "界".repeat(43),
    ] {
        assert_invalid(store.create_turn("alice", "pilot", &request, None));
    }
    for prompt in [
        "".to_owned(),
        " \n\t".to_owned(),
        "x".repeat(65537),
        "界".repeat(21846),
    ] {
        assert_invalid(store.create_turn("alice", "pilot", "invalid-prompt", Some(&prompt)));
    }
    assert!(store.list_turns("alice", "pilot").unwrap().is_empty());
    let request = format!("{}ab", "界".repeat(42));
    let prompt = "x".repeat(65536);
    let (turn, created) = store
        .create_turn("alice", "pilot", &request, Some(&prompt))
        .unwrap();
    assert!(created);
    assert_eq!(turn.request_id, request);
    assert_eq!(turn.prompt.as_deref(), Some(prompt.as_str()));
    store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Stopped, None)
        .unwrap();
    let (resume, created) = store.create_turn("alice", "pilot", "resume", None).unwrap();
    assert!(created);
    assert_eq!(resume.prompt, None);
    assert_eq!(
        store.create_turn("alice", "pilot", "resume", None).unwrap(),
        (resume.clone(), false)
    );
    assert_conflict(store.create_turn("alice", "pilot", "resume", Some("hello")));
    store
        .finish_turn("alice", "pilot", &resume.id, TurnStatus::Completed, None)
        .unwrap();
    let (_, created) = store
        .create_turn("alice", "pilot", "preserve", Some("  hello  "))
        .unwrap();
    assert!(created);
    assert_eq!(
        store.list_turns("alice", "pilot").unwrap()[0]
            .prompt
            .as_deref(),
        Some("  hello  ")
    );
}

#[test]
fn graph_and_channel_sessions_cannot_admit_pilot_turns() {
    let (_directory, _, store) = fixture();
    for (id, graph, channel) in [
        ("graph", "graph-id", BTreeMap::new()),
        (
            "channel",
            "",
            BTreeMap::from([("source".into(), "web".into())]),
        ),
    ] {
        let original = store
            .create(
                "alice",
                CreateSession {
                    id: Some(id.into()),
                    graph: graph.into(),
                    channel,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_invalid(store.create_turn("alice", id, "req-1", Some("hello")));
        assert_eq!(store.get("alice", id).unwrap(), original);
        assert!(store.list_turns("alice", id).unwrap().is_empty());
        assert_eq!(store.events("alice", id, 0).unwrap().len(), 1);
    }
}

#[test]
fn every_turn_api_enforces_owner_session_and_turn_identity() {
    let (_directory, _, store) = fixture();
    let turn = start(&store, "req-1");
    create(&store, "bob", "bob-session");
    create(&store, "alice", "other-session");
    for (owner, session) in [
        ("bob", "pilot"),
        ("alice", "missing"),
        ("bob", "bob-session"),
        ("alice", "other-session"),
    ] {
        assert_eq!(
            store.get_turn(owner, session, &turn.id),
            Err(SessionError::Missing)
        );
        assert_eq!(
            store.turn_events(owner, session, &turn.id, u64::MAX),
            Err(SessionError::Missing)
        );
        assert_eq!(
            store.append_turn_event(owner, session, &turn.id, json!({})),
            Err(SessionError::Missing)
        );
        assert_eq!(
            store.finish_turn(owner, session, &turn.id, TurnStatus::Stopped, None),
            Err(SessionError::Missing)
        );
    }
    assert_eq!(
        store.create_turn("bob", "pilot", "req-1", Some("hello")),
        Err(SessionError::Missing)
    );
    assert_eq!(store.list_turns("bob", "pilot"), Err(SessionError::Missing));
    assert_eq!(
        store.get_turn("alice", "pilot", "missing"),
        Err(SessionError::Missing)
    );
    assert_invalid(store.get_turn("alice", "pilot", "../turn"));
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(
        store
            .turn_events("alice", "pilot", &turn.id, 0)
            .unwrap()
            .len(),
        1
    );
    let (_, created) = store
        .create_turn("bob", "bob-session", "req-1", Some("other input"))
        .unwrap();
    assert!(created);
}

#[test]
fn status_changes_and_deletion_cannot_break_a_running_turn_but_rename_can() {
    let (_directory, _, store) = fixture();
    let turn = start(&store, "req-1");
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    for status in [
        SessionStatus::Active,
        SessionStatus::WaitingUser,
        SessionStatus::Interrupted,
        SessionStatus::Archived,
    ] {
        assert_conflict(store.set_status("alice", "pilot", status, "mutation"));
    }
    assert_conflict(store.delete("alice", "pilot"));
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    let renamed = store
        .rename("alice", "pilot", "Renamed while running")
        .unwrap();
    assert_eq!(renamed.status, SessionStatus::Active);
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
}

#[test]
fn finishing_is_atomic_terminal_idempotent_and_stop_allows_resume() {
    let (_directory, _, store) = fixture();
    for (iteration, status) in [
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Stopped,
        TurnStatus::Interrupted,
    ]
    .into_iter()
    .enumerate()
    {
        let turn = start(&store, &format!("req-{iteration}"));
        let error = (status == TurnStatus::Failed).then_some("provider failed");
        assert_invalid(store.finish_turn("alice", "pilot", &turn.id, TurnStatus::Running, None));
        let finished = store
            .finish_turn("alice", "pilot", &turn.id, status, error)
            .unwrap();
        assert_eq!(finished.status, status);
        assert_eq!(finished.error.as_deref(), error);
        assert_eq!(finished.created_at, turn.created_at);
        assert!(finished.updated_at >= turn.updated_at);
        let snapshot = store.get("alice", "pilot").unwrap();
        assert_eq!(
            snapshot.status,
            if status == TurnStatus::Completed {
                SessionStatus::Active
            } else {
                SessionStatus::Interrupted
            }
        );
        assert_eq!(snapshot.updated_at, finished.updated_at);
        let activity = store.events("alice", "pilot", 0).unwrap();
        let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[1].data["turn"],
            serde_json::to_value(&finished).unwrap()
        );
        assert_eq!(
            store
                .finish_turn("alice", "pilot", &turn.id, status, error)
                .unwrap(),
            finished
        );
        assert_conflict(store.finish_turn("alice", "pilot", &turn.id, status, Some("changed")));
        let different_status = if status == TurnStatus::Completed {
            TurnStatus::Stopped
        } else {
            TurnStatus::Completed
        };
        assert_conflict(store.finish_turn("alice", "pilot", &turn.id, different_status, error));
        assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
        assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
        assert_eq!(
            store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
            events
        );
    }
}

#[test]
fn late_idempotent_finish_does_not_mutate_the_next_running_turn_or_archived_session() {
    let (_directory, _, store) = fixture();
    let first = start(&store, "req-1");
    let stopped = store
        .finish_turn("alice", "pilot", &first.id, TurnStatus::Stopped, None)
        .unwrap();
    let next = start(&store, "req-2");
    let snapshot = store.get("alice", "pilot").unwrap();
    assert_eq!(
        store
            .finish_turn("alice", "pilot", &first.id, TurnStatus::Stopped, None)
            .unwrap(),
        stopped
    );
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.get_turn("alice", "pilot", &next.id).unwrap(), next);
    store
        .finish_turn("alice", "pilot", &next.id, TurnStatus::Completed, None)
        .unwrap();
    let archived = store
        .set_status("alice", "pilot", SessionStatus::Archived, "retained")
        .unwrap();
    store
        .finish_turn("alice", "pilot", &first.id, TurnStatus::Stopped, None)
        .unwrap();
    assert_eq!(store.get("alice", "pilot").unwrap(), archived);
}

#[test]
fn finishing_a_preexisting_archived_snapshot_never_reactivates_it() {
    let (_directory, path, store) = fixture();
    let turn = start(&store, "req-1");
    let mut snapshot = store.get("alice", "pilot").unwrap();
    snapshot.status = SessionStatus::Archived;
    snapshot.waiting_reason = "archived externally".into();
    Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE sessions SET data = ?1 WHERE id = 'pilot'",
            [serde_json::to_string(&snapshot).unwrap()],
        )
        .unwrap();
    store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Stopped, None)
        .unwrap();
    let finished = store.get("alice", "pilot").unwrap();
    assert_eq!(finished.status, SessionStatus::Archived);
    assert_eq!(finished.waiting_reason, snapshot.waiting_reason);
    assert_conflict(store.create_turn("alice", "pilot", "new", None));
}

#[test]
fn ui_events_are_opaque_durable_and_have_exclusive_monotonic_cursors() {
    let (_directory, path, store) = fixture();
    let turn = start(&store, "req-1");
    for (index, data) in [
        json!({"type": "delta", "text": "hello"}),
        json!([1, "two"]),
        Value::Null,
    ]
    .into_iter()
    .enumerate()
    {
        let event = store
            .append_turn_event("alice", "pilot", &turn.id, data.clone())
            .unwrap();
        assert_eq!(event.seq, index as u64 + 2);
        assert_eq!(event.data, data);
    }
    let completed = store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
        .unwrap();
    assert_conflict(store.append_turn_event("alice", "pilot", &turn.id, json!({"type": "done"})));
    let all = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    assert_eq!(
        all.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    assert_eq!(
        store.turn_events("alice", "pilot", &turn.id, 2).unwrap(),
        all[2..]
    );
    for cursor in [5, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
        assert!(
            store
                .turn_events("alice", "pilot", &turn.id, cursor)
                .unwrap()
                .is_empty()
        );
    }
    drop(store);
    let reopened = SessionStore::open(&path).unwrap();
    assert_eq!(
        reopened.get_turn("alice", "pilot", &turn.id).unwrap(),
        completed
    );
    assert_eq!(
        reopened.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        all
    );
    assert_conflict(reopened.append_turn_event("alice", "pilot", &turn.id, json!({})));
}

#[test]
fn turn_listing_is_newest_first_and_late_terminal_events_are_rejected() {
    let (_directory, _, store) = fixture();
    let mut turns = Vec::new();
    for request in ["one", "two", "three"] {
        let turn = start(&store, request);
        turns.push(
            store
                .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
                .unwrap(),
        );
    }
    assert_conflict(store.append_turn_event(
        "alice",
        "pilot",
        &turns[0].id,
        json!({"type": "late"}),
    ));
    turns.reverse();
    assert_eq!(store.list_turns("alice", "pilot").unwrap(), turns);
}

#[test]
fn reopen_does_not_interrupt_and_explicit_startup_interruption_is_durable_and_idempotent() {
    let (_directory, path, store) = fixture();
    let pilot = start(&store, "pilot-request");
    create(&store, "bob", "other");
    let other = store
        .create_turn("bob", "other", "other-request", None)
        .unwrap()
        .0;
    create(&store, "alice", "completed");
    let completed = store
        .create_turn("alice", "completed", "completed-request", None)
        .unwrap()
        .0;
    let completed = store
        .finish_turn(
            "alice",
            "completed",
            &completed.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    drop(store);
    let reopened = SessionStore::open(&path).unwrap();
    assert_eq!(
        reopened
            .get_turn("alice", "pilot", &pilot.id)
            .unwrap()
            .status,
        TurnStatus::Running
    );
    assert_conflict(reopened.create_turn("alice", "pilot", "new", Some("hello")));
    assert_eq!(reopened.interrupt_running().unwrap(), 2);
    let pilot_events = reopened
        .turn_events("alice", "pilot", &pilot.id, 0)
        .unwrap();
    assert_eq!(pilot_events.len(), 2);
    assert_eq!(pilot_events[1].data["type"], "turn.interrupted");
    assert_eq!(
        reopened.get("alice", "pilot").unwrap().status,
        SessionStatus::Interrupted
    );
    assert_eq!(
        reopened.get_turn("bob", "other", &other.id).unwrap().status,
        TurnStatus::Interrupted
    );
    assert_eq!(
        reopened
            .get_turn("alice", "completed", &completed.id)
            .unwrap(),
        completed
    );
    assert_eq!(reopened.interrupt_running().unwrap(), 0);
    assert_eq!(
        reopened
            .turn_events("alice", "pilot", &pilot.id, 0)
            .unwrap(),
        pilot_events
    );
    let interrupted = reopened.get_turn("alice", "pilot", &pilot.id).unwrap();
    assert_eq!(
        reopened
            .create_turn("alice", "pilot", "pilot-request", Some("hello"))
            .unwrap(),
        (interrupted, false)
    );
    drop(reopened);
    let reopened = SessionStore::open(&path).unwrap();
    assert_eq!(reopened.interrupt_running().unwrap(), 0);
    assert_eq!(
        reopened
            .turn_events("alice", "pilot", &pilot.id, 1)
            .unwrap(),
        pilot_events[1..]
    );
    let (_, created) = reopened
        .create_turn("alice", "pilot", "resume", None)
        .unwrap();
    assert!(created);
    assert_eq!(
        reopened.get("alice", "pilot").unwrap().status,
        SessionStatus::Active
    );
}

fn concurrent_create(
    path: &PathBuf,
    first_request: &str,
    second_request: &str,
    second_prompt: &str,
) -> Vec<Result<(Turn, bool), SessionError>> {
    let stores = [
        SessionStore::open(path).unwrap(),
        SessionStore::open(path).unwrap(),
    ];
    let start = Arc::new(Barrier::new(2));
    let workers: Vec<_> = stores
        .into_iter()
        .zip([
            (first_request.to_owned(), "hello".to_owned()),
            (second_request.to_owned(), second_prompt.to_owned()),
        ])
        .map(|(store, (request, prompt))| {
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                store.create_turn("alice", "pilot", &request, Some(&prompt))
            })
        })
        .collect();
    workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect()
}

#[test]
fn two_connections_admit_exactly_one_turn_for_a_repeated_request() {
    let (_directory, path, store) = fixture();
    let results = concurrent_create(&path, "same", "same", "hello");
    let results: Vec<_> = results.into_iter().map(Result::unwrap).collect();
    assert_eq!(results.iter().filter(|(_, created)| *created).count(), 1);
    assert_eq!(results[0].0, results[1].0);
    assert_eq!(store.list_turns("alice", "pilot").unwrap().len(), 1);
    assert_eq!(
        store
            .turn_events("alice", "pilot", &results[0].0.id, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.events("alice", "pilot", 0).unwrap().len(), 2);
}

#[test]
fn concurrent_mismatched_requests_or_inputs_conflict_without_orphans() {
    for (second_request, second_prompt) in [("second", "hello"), ("first", "changed")] {
        let (_directory, path, store) = fixture();
        let results = concurrent_create(&path, "first", second_request, second_prompt);
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(SessionError::Conflict(_))))
                .count(),
            1
        );
        assert_eq!(store.list_turns("alice", "pilot").unwrap().len(), 1);
        assert_eq!(store.events("alice", "pilot", 0).unwrap().len(), 2);
    }
}

#[test]
fn separate_connections_append_events_without_duplicate_or_missing_sequences() {
    let (_directory, path, store) = fixture();
    let turn = start(&store, "req-1");
    let start = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [store.clone(), SessionStore::open(&path).unwrap()]
        .into_iter()
        .enumerate()
        .map(|(worker, store)| {
            let start = Arc::clone(&start);
            let turn = turn.id.clone();
            thread::spawn(move || {
                start.wait();
                for iteration in 0..25 {
                    store
                        .append_turn_event(
                            "alice",
                            "pilot",
                            &turn,
                            json!({"worker": worker, "iteration": iteration}),
                        )
                        .unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    assert_eq!(events.len(), 51);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (1..=51).collect::<Vec<_>>()
    );
    for worker in 0..2 {
        assert_eq!(
            events
                .iter()
                .filter(|event| event.data["worker"] == worker)
                .count(),
            25
        );
    }
}

#[test]
fn concurrent_finish_and_archive_never_produce_a_running_archived_session() {
    let (_directory, path, store) = fixture();
    let turn = start(&store, "req-1");
    let archiver = SessionStore::open(path).unwrap();
    let finisher = store.clone();
    let start = Arc::new(Barrier::new(2));
    let archive_start = Arc::clone(&start);
    let archive = thread::spawn(move || {
        archive_start.wait();
        archiver.set_status("alice", "pilot", SessionStatus::Archived, "saved")
    });
    let finish = thread::spawn(move || {
        start.wait();
        finisher.finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
    });
    assert_eq!(
        finish.join().unwrap().unwrap().status,
        TurnStatus::Completed
    );
    let archived = archive.join().unwrap();
    match archived {
        Ok(_) => assert_eq!(
            store.get("alice", "pilot").unwrap().status,
            SessionStatus::Archived
        ),
        Err(SessionError::Conflict(_)) => assert_eq!(
            store.get("alice", "pilot").unwrap().status,
            SessionStatus::Active
        ),
        result => panic!("unexpected archive result: {result:?}"),
    }
}

#[test]
fn admission_and_finish_roll_back_snapshots_turns_and_both_event_streams() {
    for table in ["turn_events", "session_events"] {
        let (_directory, path, store) = fixture();
        let original = store
            .set_status("alice", "pilot", SessionStatus::Interrupted, "paused")
            .unwrap();
        let activity = store.events("alice", "pilot", 0).unwrap();
        let connection = Connection::open(path).unwrap();
        connection.execute_batch(&format!("CREATE TRIGGER fail_event BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT, 'event failed'); END;")).unwrap();
        assert_storage(store.create_turn("alice", "pilot", "req-1", Some("hello")));
        assert_eq!(store.get("alice", "pilot").unwrap(), original);
        assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
        assert!(store.list_turns("alice", "pilot").unwrap().is_empty());
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM turn_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        connection.execute_batch("DROP TRIGGER fail_event").unwrap();
        let turn = start(&store, "req-1");
        let original = store.get("alice", "pilot").unwrap();
        let activity = store.events("alice", "pilot", 0).unwrap();
        let ui_events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
        connection.execute_batch(&format!("CREATE TRIGGER fail_event BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT, 'event failed'); END;")).unwrap();
        assert_storage(store.finish_turn(
            "alice",
            "pilot",
            &turn.id,
            TurnStatus::Failed,
            Some("failed"),
        ));
        assert_eq!(store.get("alice", "pilot").unwrap(), original);
        assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
        assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
        assert_eq!(
            store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
            ui_events
        );
        connection.execute_batch("DROP TRIGGER fail_event").unwrap();
        assert_eq!(
            store
                .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
                .unwrap()
                .status,
            TurnStatus::Completed
        );
        assert_eq!(
            store
                .turn_events("alice", "pilot", &turn.id, 0)
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
fn startup_interruption_rolls_back_all_sessions_if_any_event_fails() {
    let (_directory, path, store) = fixture();
    let pilot = start(&store, "pilot-request");
    create(&store, "bob", "second");
    let second = store
        .create_turn("bob", "second", "second-request", None)
        .unwrap()
        .0;
    let first_snapshot = store.get("alice", "pilot").unwrap();
    let second_snapshot = store.get("bob", "second").unwrap();
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(&format!("CREATE TRIGGER fail_event BEFORE INSERT ON turn_events WHEN NEW.turn_id = '{}' BEGIN SELECT RAISE(ABORT, 'second event failed'); END;", second.id)).unwrap();
    assert_storage(store.interrupt_running());
    assert_eq!(store.get("alice", "pilot").unwrap(), first_snapshot);
    assert_eq!(store.get("bob", "second").unwrap(), second_snapshot);
    assert_eq!(store.get_turn("alice", "pilot", &pilot.id).unwrap(), pilot);
    assert_eq!(store.get_turn("bob", "second", &second.id).unwrap(), second);
    assert_eq!(
        store
            .turn_events("alice", "pilot", &pilot.id, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .turn_events("bob", "second", &second.id, 0)
            .unwrap()
            .len(),
        1
    );
    connection.execute_batch("DROP TRIGGER fail_event").unwrap();
    assert_eq!(store.interrupt_running().unwrap(), 2);
}

#[test]
fn event_sequence_exhaustion_is_reported_without_changing_turn_or_session() {
    let (_directory, path, store) = fixture();
    let turn = start(&store, "req-1");
    let snapshot = store.get("alice", "pilot").unwrap();
    Connection::open(path)
        .unwrap()
        .execute(
            "INSERT INTO turn_events(turn_id, seq, data) VALUES (?1, ?2, '{}')",
            params![turn.id, i64::MAX],
        )
        .unwrap();
    assert_storage(store.append_turn_event("alice", "pilot", &turn.id, json!({})));
    assert_storage(store.finish_turn("alice", "pilot", &turn.id, TurnStatus::Stopped, None));
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(
        store
            .turn_events("alice", "pilot", &turn.id, 0)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn deleting_terminal_turns_cascades_business_events_without_touching_other_sessions() {
    let (_directory, path, store) = fixture();
    let turn = start(&store, "req-1");
    assert_conflict(store.delete("alice", "pilot"));
    store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Stopped, None)
        .unwrap();
    create(&store, "bob", "other");
    let other = store.create_turn("bob", "other", "req-1", None).unwrap().0;
    assert_eq!(store.delete("bob", "pilot"), Err(SessionError::Missing));
    store.delete("alice", "pilot").unwrap();
    assert_eq!(store.get("alice", "pilot"), Err(SessionError::Missing));
    assert_eq!(
        store.get_turn("alice", "pilot", &turn.id),
        Err(SessionError::Missing)
    );
    assert_eq!(store.get_turn("bob", "other", &other.id).unwrap(), other);
    let connection = Connection::open(path).unwrap();
    for table in ["sessions", "turns", "turn_events"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM session_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn cascading_turn_event_deletion_failure_restores_all_facts() {
    let (_directory, path, store) = fixture();
    let turn = start(&store, "req-1");
    let turn = store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
        .unwrap();
    let snapshot = store.get("alice", "pilot").unwrap();
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    let connection = Connection::open(path).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_delete BEFORE DELETE ON turn_events BEGIN SELECT RAISE(ABORT, 'cascade failed'); END;").unwrap();
    assert_storage(store.delete("alice", "pilot"));
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(
        store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        events
    );
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    connection
        .execute_batch("DROP TRIGGER fail_delete")
        .unwrap();
    store.delete("alice", "pilot").unwrap();
}

const V1_SCHEMA: &str = "CREATE TABLE sessions (
            id TEXT PRIMARY KEY NOT NULL,
            owner TEXT NOT NULL,
            data TEXT NOT NULL CHECK(json_valid(data)),
            updated_at TEXT NOT NULL
        );
        CREATE TABLE session_events (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL CHECK(seq > 0),
            at TEXT NOT NULL,
            kind TEXT NOT NULL,
            data TEXT NOT NULL CHECK(json_valid(data)),
            PRIMARY KEY(session_id, seq)
        );
        CREATE INDEX sessions_owner_updated ON sessions(owner, updated_at DESC, id);
        PRAGMA application_id = 1095652179;
        PRAGMA user_version = 1;";

fn v1_fixture() -> (TempDir, PathBuf, Session) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("v1.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch(V1_SCHEMA).unwrap();
    let now = Utc::now();
    let snapshot = Session {
        id: "pilot".into(),
        conversation_id: "pilot".into(),
        title: "v1 preserved".into(),
        status: SessionStatus::Interrupted,
        waiting_reason: "restart".into(),
        run_ids: vec![],
        graph: String::new(),
        reply_node: String::new(),
        channel: BTreeMap::new(),
        approval: None,
        approvals: vec![],
        questions: vec![],
        operation: None,
        operations: BTreeMap::new(),
        created_at: now,
        updated_at: now,
    };
    connection
        .execute(
            "INSERT INTO sessions VALUES (?1, 'alice', ?2, ?3)",
            params![
                snapshot.id,
                serde_json::to_string(&snapshot).unwrap(),
                now.to_rfc3339()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_events VALUES ('pilot', 1, ?1, 'session.created', '{}')",
            [now.to_rfc3339()],
        )
        .unwrap();
    (directory, path, snapshot)
}

#[test]
fn exact_v1_migrates_transactionally_and_preserves_sessions_events_and_ownership() {
    let (_directory, path, original) = v1_fixture();
    let store = SessionStore::open(&path).unwrap();
    assert_eq!(store.get("alice", "pilot").unwrap(), original);
    assert_eq!(store.get("bob", "pilot"), Err(SessionError::Missing));
    assert!(store.list_turns("alice", "pilot").unwrap().is_empty());
    assert_eq!(store.events("alice", "pilot", 0).unwrap()[0].seq, 1);
    let turn = start(&store, "v2-request");
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    let tables = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        tables,
        vec![
            "channel_assistant_inputs",
            "channel_assistants",
            "channel_deliveries",
            "channel_inbounds",
            "channel_sessions",
            "questions",
            "session_events",
            "sessions",
            "turn_events",
            "turn_goose",
            "turn_native",
            "turn_runs",
            "turns"
        ]
    );
    drop(connection);
    drop(store);
    let reopened = SessionStore::open(path).unwrap();
    assert_eq!(reopened.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(reopened.events("alice", "pilot", 1).unwrap().len(), 1);
}

#[test]
fn unknown_v1_schema_identity_or_future_version_is_unchanged_without_partial_migration() {
    for change in [
        "PRAGMA application_id = 0",
        "PRAGMA application_id = 42",
        "PRAGMA user_version = 0",
        "PRAGMA user_version = 2",
        "PRAGMA user_version = 3",
        "PRAGMA user_version = 999",
        "DROP INDEX sessions_owner_updated",
        "CREATE TABLE unexpected(value TEXT)",
        "CREATE TABLE turns(value TEXT)",
        "CREATE VIEW unexpected AS SELECT * FROM sessions",
        "CREATE TRIGGER unexpected AFTER INSERT ON sessions BEGIN SELECT 1; END",
        "ALTER TABLE sessions ADD COLUMN extra TEXT",
        "DROP TABLE session_events; CREATE TABLE session_events(session_id TEXT, seq INTEGER)",
    ] {
        let (_directory, path, _) = v1_fixture();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(change).unwrap();
        let schema: Vec<_> = connection
            .prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(connection);
        let bytes = fs::read(&path).unwrap();
        assert_storage(SessionStore::open(&path));
        assert_eq!(
            fs::read(&path).unwrap(),
            bytes,
            "database was changed after {change}"
        );
        assert!(!path.with_extension("sqlite-wal").exists());
        let connection = Connection::open(&path).unwrap();
        let after: Vec<_> = connection
            .prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(after, schema, "DDL occurred before rejecting {change}");
    }
}

#[test]
fn two_connections_can_open_and_migrate_v1_once_without_losing_facts() {
    let (_directory, path, original) = v1_fixture();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let start = Arc::clone(&barrier);
            thread::spawn(move || {
                start.wait();
                SessionStore::open(path).unwrap()
            })
        })
        .collect();
    let stores: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    for store in &stores {
        assert_eq!(store.get("alice", "pilot").unwrap(), original);
    }
    let turn = start(&stores[0], "migrated");
    assert_eq!(
        stores[1].get_turn("alice", "pilot", &turn.id).unwrap(),
        turn
    );
}
