use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use anchor_platform_session::{
    CreateSession, NativeExecution, SessionError, SessionStatus, SessionStore, Turn, TurnStatus,
};
use rusqlite::{Connection, params};
use serde_json::json;
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf, SessionStore, Turn) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.sqlite");
    let store = SessionStore::open(&path).unwrap();
    let turn = create_turn(&store, "alice", "pilot");
    (directory, path, store, turn)
}

fn create_turn(store: &SessionStore, owner: &str, session: &str) -> Turn {
    store
        .create(
            owner,
            CreateSession {
                id: Some(session.into()),
                ..Default::default()
            },
        )
        .unwrap();
    store
        .create_turn(owner, session, "request", Some("hello"))
        .unwrap()
        .0
}

fn execution(run: i64) -> NativeExecution {
    NativeExecution {
        scope: "0123456789abcdef".repeat(4),
        session: 1,
        run,
    }
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
fn old_turn_json_defaults_links_and_native_execution_round_trips() {
    let (_directory, _, _, turn) = fixture();
    let mut value = serde_json::to_value(&turn).unwrap();
    value.as_object_mut().unwrap().remove("native");
    value.as_object_mut().unwrap().remove("goose");
    value.as_object_mut().unwrap().remove("runs");
    assert_eq!(serde_json::from_value::<Turn>(value).unwrap(), turn);
    let native = execution(i64::MAX);
    assert_eq!(
        serde_json::to_value(&native).unwrap(),
        json!({"scope": native.scope, "session": 1, "run": i64::MAX})
    );
    assert_eq!(
        serde_json::from_value::<NativeExecution>(serde_json::to_value(&native).unwrap()).unwrap(),
        native
    );
}

#[test]
fn binding_is_durable_idempotent_and_visible_in_every_turn_projection() {
    let (_directory, path, store, initial) = fixture();
    let delivery = store.turn_events("alice", "pilot", &initial.id, 0).unwrap();
    let bound = store
        .bind_native("alice", "pilot", &initial.id, execution(3))
        .unwrap();
    assert_eq!(bound.native, Some(execution(3)));
    assert!(bound.runs.is_empty());
    assert_eq!(bound.updated_at, initial.updated_at);
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    let event = activity.last().unwrap();
    assert_eq!(event.kind, "turn.native_bound");
    assert_eq!(event.data["turn"], initial.id);
    assert_eq!(event.data["native"], json!(execution(3)));
    assert_eq!(event.at, snapshot.updated_at);
    assert_eq!(snapshot.status, SessionStatus::Active);
    assert!(snapshot.run_ids.is_empty());
    assert_eq!(
        store
            .bind_native("alice", "pilot", &initial.id, execution(3))
            .unwrap(),
        bound
    );
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    assert_eq!(
        store.turn_events("alice", "pilot", &initial.id, 0).unwrap(),
        delivery
    );
    assert_eq!(
        store.list_turns("alice", "pilot").unwrap(),
        vec![bound.clone()]
    );
    assert_eq!(
        store
            .create_turn("alice", "pilot", "request", Some("hello"))
            .unwrap(),
        (bound.clone(), false)
    );
    drop(store);
    let reopened = SessionStore::open(path).unwrap();
    assert_eq!(
        reopened.get_turn("alice", "pilot", &initial.id).unwrap(),
        bound
    );
    let completed = reopened
        .finish_turn("alice", "pilot", &initial.id, TurnStatus::Completed, None)
        .unwrap();
    assert_eq!(completed.native, bound.native);
    let terminal_delivery = reopened
        .turn_events("alice", "pilot", &initial.id, 0)
        .unwrap();
    assert_eq!(
        terminal_delivery.last().unwrap().data["turn"]["native"],
        json!(execution(3))
    );
}

#[test]
fn native_bindings_conflict_on_any_changed_field_and_cannot_be_stolen() {
    let (_directory, _, store, turn) = fixture();
    let bound = store
        .bind_native("alice", "pilot", &turn.id, execution(1))
        .unwrap();
    for conflicting in [
        execution(2),
        NativeExecution {
            session: 2,
            ..execution(1)
        },
        NativeExecution {
            scope: "f".repeat(64),
            ..execution(1)
        },
    ] {
        assert_conflict(store.bind_native("alice", "pilot", &turn.id, conflicting));
    }
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), bound);
    store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
        .unwrap();
    let second = store
        .create_turn("alice", "pilot", "second", None)
        .unwrap()
        .0;
    let foreign = create_turn(&store, "bob", "foreign");
    for (owner, session, target) in [
        ("alice", "pilot", &second.id),
        ("bob", "foreign", &foreign.id),
    ] {
        assert_conflict(store.bind_native(owner, session, target, execution(1)));
        assert_conflict(store.bind_native(
            owner,
            session,
            target,
            NativeExecution {
                session: 99,
                ..execution(1)
            },
        ));
        assert!(
            store
                .get_turn(owner, session, target)
                .unwrap()
                .native
                .is_none()
        );
    }
    store
        .bind_native("alice", "pilot", &second.id, execution(2))
        .unwrap();
    let other_scope = NativeExecution {
        scope: "A".repeat(64),
        ..execution(1)
    };
    store
        .bind_native("bob", "foreign", &foreign.id, other_scope.clone())
        .unwrap();
    assert_eq!(
        store
            .get_turn("bob", "foreign", &foreign.id)
            .unwrap()
            .native,
        Some(other_scope)
    );
}

#[test]
fn every_terminal_status_rejects_first_bind_but_preserves_existing_idempotent_bind() {
    for status in [
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Stopped,
        TurnStatus::Interrupted,
    ] {
        for bind_first in [false, true] {
            let (_directory, _, store, turn) = fixture();
            if bind_first {
                store
                    .bind_native("alice", "pilot", &turn.id, execution(1))
                    .unwrap();
            }
            let terminal = store
                .finish_turn("alice", "pilot", &turn.id, status, Some("finished"))
                .unwrap();
            let snapshot = store.get("alice", "pilot").unwrap();
            let activity = store.events("alice", "pilot", 0).unwrap();
            let delivery = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
            if bind_first {
                assert_eq!(
                    store
                        .bind_native("alice", "pilot", &turn.id, execution(1))
                        .unwrap(),
                    terminal
                );
            } else {
                assert_conflict(store.bind_native("alice", "pilot", &turn.id, execution(1)));
            }
            assert_conflict(store.bind_native("alice", "pilot", &turn.id, execution(2)));
            assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
            assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
            assert_eq!(
                store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
                delivery
            );
        }
    }
}

#[test]
fn both_link_apis_require_the_owner_and_exact_session_turn_identity() {
    let (_directory, _, store, turn) = fixture();
    let same_owner = create_turn(&store, "alice", "other");
    let foreign = create_turn(&store, "bob", "foreign");
    let snapshot = store.get("alice", "pilot").unwrap();
    for (owner, session, target) in [
        ("bob", "pilot", turn.id.as_str()),
        ("alice", "foreign", foreign.id.as_str()),
        ("alice", "other", turn.id.as_str()),
        ("alice", "pilot", same_owner.id.as_str()),
        ("alice", "pilot", foreign.id.as_str()),
        ("alice", "pilot", "missing"),
        ("alice", "missing", turn.id.as_str()),
    ] {
        assert_eq!(
            store.bind_native(owner, session, target, execution(1)),
            Err(SessionError::Missing)
        );
        assert_eq!(
            store.associate_run(owner, session, target, "rust-owner-check"),
            Err(SessionError::Missing)
        );
    }
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    for (owner, session, target) in [
        ("", "pilot", turn.id.as_str()),
        ("alice", "../pilot", turn.id.as_str()),
        ("alice", "pilot", "../turn"),
    ] {
        assert_invalid(store.bind_native(owner, session, target, execution(1)));
        assert_invalid(store.associate_run(owner, session, target, "rust-valid"));
    }
}

#[test]
fn invalid_native_facts_and_unsafe_run_ids_leave_every_fact_unchanged() {
    let (_directory, _, store, turn) = fixture();
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    for scope in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "g".repeat(64),
        "é".repeat(32),
        format!("{}\0", "a".repeat(63)),
        format!("{} /", "a".repeat(62)),
    ] {
        assert_invalid(store.bind_native(
            "alice",
            "pilot",
            &turn.id,
            NativeExecution {
                scope,
                ..execution(1)
            },
        ));
    }
    for native in [
        execution(0),
        execution(-1),
        NativeExecution {
            session: 0,
            ..execution(1)
        },
        NativeExecution {
            session: -1,
            ..execution(1)
        },
    ] {
        assert_invalid(store.bind_native("alice", "pilot", &turn.id, native));
    }
    for run in [
        "",
        ".",
        "..",
        "../rust-run",
        "/rust-run",
        "rust/run",
        "rust\\run",
        "rust run",
        "rust\0run",
        "rust%2frun",
    ] {
        assert_invalid(store.associate_run("alice", "pilot", &turn.id, run));
    }
    assert_invalid(store.associate_run("alice", "pilot", &turn.id, &"a".repeat(257)));
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    let valid = NativeExecution {
        session: i64::MAX,
        ..execution(i64::MAX)
    };
    assert_eq!(
        store
            .bind_native("alice", "pilot", &turn.id, valid.clone())
            .unwrap()
            .native,
        Some(valid)
    );
    let run = "rust-00000000-0000-0000-0000-000000000001";
    assert_eq!(
        store
            .associate_run("alice", "pilot", &turn.id, run)
            .unwrap()
            .runs,
        vec![run]
    );
}

#[test]
fn serial_turn_associations_preserve_insertion_order_and_the_session_union() {
    let (_directory, path, store, first) = fixture();
    store
        .bind_native("alice", "pilot", &first.id, execution(1))
        .unwrap();
    store
        .attach_run("alice", "pilot", "rust-preexisting")
        .unwrap();
    let activity_count = store.events("alice", "pilot", 0).unwrap().len();
    store
        .associate_run("alice", "pilot", &first.id, "rust-preexisting")
        .unwrap();
    assert_eq!(
        store.events("alice", "pilot", 0).unwrap().len(),
        activity_count + 1
    );
    let first = store
        .associate_run("alice", "pilot", &first.id, "rust-second")
        .unwrap();
    assert_eq!(first.runs, vec!["rust-preexisting", "rust-second"]);
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    assert_eq!(
        store
            .associate_run("alice", "pilot", &first.id, "rust-preexisting")
            .unwrap(),
        first
    );
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    let first = store
        .finish_turn("alice", "pilot", &first.id, TurnStatus::Completed, None)
        .unwrap();
    let second = store
        .create_turn("alice", "pilot", "second", Some("new"))
        .unwrap()
        .0;
    store
        .bind_native("alice", "pilot", &second.id, execution(2))
        .unwrap();
    store
        .associate_run("alice", "pilot", &second.id, "rust-second")
        .unwrap();
    let second = store
        .associate_run("alice", "pilot", &second.id, "rust-third")
        .unwrap();
    assert_eq!(second.runs, vec!["rust-second", "rust-third"]);
    assert_eq!(
        store.get("alice", "pilot").unwrap().run_ids,
        vec!["rust-preexisting", "rust-second", "rust-third"]
    );
    assert_eq!(
        store.list_turns("alice", "pilot").unwrap(),
        vec![second.clone(), first.clone()]
    );
    assert_eq!(
        store
            .create_turn("alice", "pilot", "request", Some("hello"))
            .unwrap(),
        (first.clone(), false)
    );
    assert_eq!(
        store
            .create_turn("alice", "pilot", "second", Some("new"))
            .unwrap(),
        (second.clone(), false)
    );
    drop(store);
    let reopened = SessionStore::open(path).unwrap();
    assert_eq!(
        reopened.get_turn("alice", "pilot", &first.id).unwrap(),
        first
    );
    assert_eq!(
        reopened.get_turn("alice", "pilot", &second.id).unwrap(),
        second
    );
    assert_eq!(reopened.interrupt_running().unwrap(), 1);
    let interrupted = reopened.get_turn("alice", "pilot", &second.id).unwrap();
    assert_eq!(interrupted.native, second.native);
    assert_eq!(interrupted.runs, second.runs);
    assert_eq!(
        reopened
            .turn_events("alice", "pilot", &second.id, 0)
            .unwrap()
            .last()
            .unwrap()
            .data["turn"],
        json!(interrupted)
    );
}

#[test]
fn terminal_reconciliation_preserves_lifecycle_and_rejects_late_delivery() {
    for status in [
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Stopped,
        TurnStatus::Interrupted,
    ] {
        for session_status in [
            SessionStatus::Active,
            SessionStatus::WaitingUser,
            SessionStatus::Interrupted,
            SessionStatus::Archived,
        ] {
            let (_directory, path, store, turn) = fixture();
            store
                .bind_native("alice", "pilot", &turn.id, execution(1))
                .unwrap();
            let terminal = store
                .finish_turn("alice", "pilot", &turn.id, status, Some("original outcome"))
                .unwrap();
            store
                .set_status("alice", "pilot", session_status, "keep reason")
                .unwrap();
            let snapshot = store.get("alice", "pilot").unwrap();
            let delivery = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
            drop(store);
            let reopened = SessionStore::open(path).unwrap();
            let associated = reopened
                .associate_run("alice", "pilot", &turn.id, "rust-reconciled")
                .unwrap();
            assert_eq!(
                associated,
                Turn {
                    runs: vec!["rust-reconciled".into()],
                    ..terminal
                }
            );
            let updated = reopened.get("alice", "pilot").unwrap();
            assert_eq!(
                updated,
                anchor_platform_session::Session {
                    run_ids: vec!["rust-reconciled".into()],
                    updated_at: updated.updated_at,
                    ..snapshot
                }
            );
            assert_eq!(
                reopened.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
                delivery
            );
            let after = delivery.last().unwrap().seq;
            assert!(
                reopened
                    .turn_events("alice", "pilot", &turn.id, after)
                    .unwrap()
                    .is_empty()
            );
            assert_conflict(reopened.append_turn_event(
                "alice",
                "pilot",
                &turn.id,
                json!({"type": "late"}),
            ));
            assert_eq!(
                reopened
                    .finish_turn("alice", "pilot", &turn.id, status, Some("original outcome"))
                    .unwrap(),
                associated
            );
            let activity = reopened.events("alice", "pilot", 0).unwrap();
            assert_eq!(
                reopened
                    .associate_run("alice", "pilot", &turn.id, "rust-reconciled")
                    .unwrap(),
                associated
            );
            assert_eq!(reopened.events("alice", "pilot", 0).unwrap(), activity);
            assert_eq!(reopened.get("alice", "pilot").unwrap(), updated);
        }
    }
}

#[test]
fn startup_reconciliation_of_old_turn_does_not_touch_the_next_running_turn() {
    let (_directory, _, store, old) = fixture();
    store
        .bind_native("alice", "pilot", &old.id, execution(1))
        .unwrap();
    assert_eq!(store.interrupt_running().unwrap(), 1);
    let old = store.get_turn("alice", "pilot", &old.id).unwrap();
    let current = store.create_turn("alice", "pilot", "next", None).unwrap().0;
    let delivery = store.turn_events("alice", "pilot", &old.id, 0).unwrap();
    let associated = store
        .associate_run("alice", "pilot", &old.id, "rust-discovered")
        .unwrap();
    assert_eq!(associated.status, TurnStatus::Interrupted);
    assert_eq!(associated.updated_at, old.updated_at);
    assert_eq!(
        store.get_turn("alice", "pilot", &current.id).unwrap(),
        current
    );
    assert_eq!(
        store.get("alice", "pilot").unwrap().status,
        SessionStatus::Active
    );
    assert_eq!(
        store.turn_events("alice", "pilot", &old.id, 0).unwrap(),
        delivery
    );
    assert_conflict(store.append_turn_event("alice", "pilot", &old.id, json!("late")));
}

#[test]
fn binding_failures_roll_back_relations_session_snapshots_and_activity() {
    for trigger in [
        "CREATE TRIGGER fail BEFORE INSERT ON turn_native BEGIN SELECT RAISE(ABORT, 'native failed'); END",
        "CREATE TRIGGER fail BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT, 'snapshot failed'); END",
        "CREATE TRIGGER fail BEFORE INSERT ON session_events WHEN NEW.kind = 'turn.native_bound' BEGIN SELECT RAISE(ABORT, 'event failed'); END",
    ] {
        let (_directory, path, store, turn) = fixture();
        let snapshot = store.get("alice", "pilot").unwrap();
        let activity = store.events("alice", "pilot", 0).unwrap();
        let connection = Connection::open(path).unwrap();
        connection.execute_batch(trigger).unwrap();
        assert_storage(store.bind_native("alice", "pilot", &turn.id, execution(1)));
        assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
        assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
        assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
        connection.execute_batch("DROP TRIGGER fail").unwrap();
        store
            .bind_native("alice", "pilot", &turn.id, execution(1))
            .unwrap();
    }
}

#[test]
fn association_failures_roll_back_both_links_and_all_session_events() {
    for trigger in [
        "CREATE TRIGGER fail BEFORE INSERT ON turn_runs BEGIN SELECT RAISE(ABORT, 'association failed'); END",
        "CREATE TRIGGER fail BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT, 'snapshot failed'); END",
        "CREATE TRIGGER fail BEFORE INSERT ON session_events WHEN NEW.kind = 'run.attached' BEGIN SELECT RAISE(ABORT, 'attachment event failed'); END",
        "CREATE TRIGGER fail BEFORE INSERT ON session_events WHEN NEW.kind = 'turn.run_associated' BEGIN SELECT RAISE(ABORT, 'association event failed'); END",
    ] {
        let (_directory, path, store, turn) = fixture();
        let snapshot = store.get("alice", "pilot").unwrap();
        let activity = store.events("alice", "pilot", 0).unwrap();
        let delivery = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
        let connection = Connection::open(path).unwrap();
        connection.execute_batch(trigger).unwrap();
        assert_storage(store.associate_run("alice", "pilot", &turn.id, "rust-failed"));
        assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
        assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
        assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
        assert_eq!(
            store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
            delivery
        );
        connection.execute_batch("DROP TRIGGER fail").unwrap();
        store
            .associate_run("alice", "pilot", &turn.id, "rust-failed")
            .unwrap();
    }
}

#[test]
fn two_connections_bind_a_repeated_execution_once() {
    let (_directory, path, store, turn) = fixture();
    let second = SessionStore::open(path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [store.clone(), second]
        .into_iter()
        .map(|store| {
            let barrier = Arc::clone(&barrier);
            let turn = turn.id.clone();
            thread::spawn(move || {
                barrier.wait();
                store
                    .bind_native("alice", "pilot", &turn, execution(1))
                    .unwrap()
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results[0], results[1]);
    assert_eq!(store.events("alice", "pilot", 0).unwrap().len(), 3);
}

#[test]
fn concurrent_native_claims_from_different_owners_have_exactly_one_winner() {
    let (_directory, path, store, turn) = fixture();
    let foreign = create_turn(&store, "bob", "foreign");
    let second = SessionStore::open(path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [
        (store.clone(), "alice", "pilot", turn.id),
        (second, "bob", "foreign", foreign.id),
    ]
    .into_iter()
    .map(|(store, owner, session, turn)| {
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            store.bind_native(owner, session, &turn, execution(1))
        })
    })
    .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(SessionError::Conflict(_))))
            .count(),
        1
    );
    let count = store.events("alice", "pilot", 0).unwrap().len()
        + store.events("bob", "foreign", 0).unwrap().len();
    assert_eq!(count, 5);
}

#[test]
fn concurrent_associations_deduplicate_and_preserve_atomic_ordered_links() {
    let (_directory, path, store, turn) = fixture();
    let second = SessionStore::open(path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [store.clone(), second]
        .into_iter()
        .map(|store| {
            let barrier = Arc::clone(&barrier);
            let turn = turn.id.clone();
            thread::spawn(move || {
                barrier.wait();
                for index in 0..20 {
                    store
                        .associate_run("alice", "pilot", &turn, &format!("rust-{index}"))
                        .unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let associated = store.get_turn("alice", "pilot", &turn.id).unwrap();
    assert_eq!(associated.runs.len(), 20);
    assert_eq!(
        associated.runs,
        store.get("alice", "pilot").unwrap().run_ids
    );
    assert_eq!(
        associated.runs.iter().cloned().collect::<BTreeSet<_>>(),
        (0..20).map(|index| format!("rust-{index}")).collect()
    );
    let activity = store.events("alice", "pilot", 0).unwrap();
    assert_eq!(activity.len(), 42);
    assert_eq!(
        activity.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (1..=42).collect::<Vec<_>>()
    );
    for pair in activity[2..].chunks_exact(2) {
        assert_eq!(pair[0].kind, "run.attached");
        assert_eq!(pair[1].kind, "turn.run_associated");
        assert_eq!(pair[0].data["run"], pair[1].data["run"]);
        assert_eq!(pair[0].at, pair[1].at);
    }
}

#[test]
fn business_delete_cascades_relations_but_retained_graph_runs_still_block_delete() {
    let (_directory, path, store, turn) = fixture();
    let foreign = create_turn(&store, "bob", "foreign");
    store
        .bind_native("alice", "pilot", &turn.id, execution(1))
        .unwrap();
    store
        .bind_native("bob", "foreign", &foreign.id, execution(2))
        .unwrap();
    store
        .associate_run("alice", "pilot", &turn.id, "rust-retained")
        .unwrap();
    assert_conflict(store.delete("alice", "pilot"));
    store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
        .unwrap();
    assert_conflict(store.delete("alice", "pilot"));
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE sessions SET data = json_set(data, '$.run_ids', json('[]')) WHERE id = 'pilot'",
            [],
        )
        .unwrap();
    assert_eq!(store.delete("bob", "pilot"), Err(SessionError::Missing));
    store.delete("alice", "pilot").unwrap();
    for table in ["turn_native", "turn_runs"] {
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE turn_id = ?1"),
                [&turn.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
    assert_eq!(
        store.get_turn("alice", "pilot", &turn.id),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store
            .get_turn("bob", "foreign", &foreign.id)
            .unwrap()
            .native,
        Some(execution(2))
    );
    drop(connection);
    drop(store);
    let reopened = SessionStore::open(path).unwrap();
    let recreated = create_turn(&reopened, "alice", "pilot");
    assert!(recreated.native.is_none());
    assert!(recreated.runs.is_empty());
    reopened
        .bind_native("alice", "pilot", &recreated.id, execution(1))
        .unwrap();
}

#[test]
fn relation_cascade_failure_rolls_back_the_entire_business_delete() {
    let (_directory, path, store, turn) = fixture();
    store
        .bind_native("alice", "pilot", &turn.id, execution(1))
        .unwrap();
    let terminal = store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
        .unwrap();
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    let delivery = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    let connection = Connection::open(path).unwrap();
    connection.execute_batch("CREATE TRIGGER fail BEFORE DELETE ON turn_native BEGIN SELECT RAISE(ABORT, 'cascade failed'); END").unwrap();
    assert_storage(store.delete("alice", "pilot"));
    assert_eq!(
        store.get_turn("alice", "pilot", &turn.id).unwrap(),
        terminal
    );
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    assert_eq!(
        store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        delivery
    );
    connection.execute_batch("DROP TRIGGER fail").unwrap();
    store.delete("alice", "pilot").unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM turn_native WHERE turn_id = ?1",
            params![turn.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn exhausted_association_sequence_rolls_back_the_new_session_attachment() {
    let (_directory, path, store, turn) = fixture();
    store
        .associate_run("alice", "pilot", &turn.id, "rust-existing")
        .unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "UPDATE turn_runs SET seq = ?1 WHERE turn_id = ?2",
            params![i64::MAX, turn.id],
        )
        .unwrap();
    let turn = store.get_turn("alice", "pilot", &turn.id).unwrap();
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    assert_storage(store.associate_run("alice", "pilot", &turn.id, "rust-exhausted"));
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
}

#[test]
fn exhausted_activity_sequence_rolls_back_native_and_run_links_but_keeps_retries_safe() {
    let (_directory, path, store, turn) = fixture();
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "INSERT INTO session_events VALUES ('pilot', ?1, ?2, 'injected', '{}')",
            params![i64::MAX, turn.created_at.to_rfc3339()],
        )
        .unwrap();
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    assert_storage(store.bind_native("alice", "pilot", &turn.id, execution(1)));
    assert_storage(store.associate_run("alice", "pilot", &turn.id, "rust-exhausted"));
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    connection
        .execute("DELETE FROM session_events WHERE seq = ?1", [i64::MAX])
        .unwrap();
    store
        .bind_native("alice", "pilot", &turn.id, execution(1))
        .unwrap();
    let associated = store
        .associate_run("alice", "pilot", &turn.id, "rust-existing")
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_events VALUES ('pilot', ?1, ?2, 'injected', '{}')",
            params![i64::MAX, turn.created_at.to_rfc3339()],
        )
        .unwrap();
    let snapshot = store.get("alice", "pilot").unwrap();
    assert_eq!(
        store
            .bind_native("alice", "pilot", &turn.id, execution(1))
            .unwrap(),
        associated
    );
    assert_eq!(
        store
            .associate_run("alice", "pilot", &turn.id, "rust-existing")
            .unwrap(),
        associated
    );
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
}
