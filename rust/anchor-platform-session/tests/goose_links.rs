use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use anchor_platform_session::{
    CreateSession, GooseExecution, NativeExecution, SessionError, SessionStore, Turn, TurnStatus,
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

fn execution() -> GooseExecution {
    GooseExecution {
        scope: "0123456789abcdef".repeat(4),
        session: "goose/opaque session:非整数".into(),
    }
}

fn native() -> NativeExecution {
    NativeExecution {
        scope: execution().scope,
        session: 11,
        run: 22,
    }
}

fn assert_conflict<Output>(result: Result<Output, SessionError>) {
    assert!(matches!(result, Err(SessionError::Conflict(_))));
}

fn assert_invalid<Output>(result: Result<Output, SessionError>) {
    assert!(matches!(result, Err(SessionError::Invalid(_))));
}

fn assert_storage<Output>(result: Result<Output, SessionError>) {
    assert!(matches!(result, Err(SessionError::Storage(_))));
}

#[test]
fn opaque_goose_json_round_trips_and_old_turns_default_to_no_goose() {
    let (_directory, _, _, turn) = fixture();
    let mut old = serde_json::to_value(&turn).unwrap();
    old.as_object_mut().unwrap().remove("goose");
    assert_eq!(serde_json::from_value::<Turn>(old).unwrap(), turn);
    let goose = execution();
    assert_eq!(
        serde_json::to_value(&goose).unwrap(),
        json!({"scope": goose.scope, "session": goose.session})
    );
    assert_eq!(
        serde_json::from_value::<GooseExecution>(serde_json::to_value(&goose).unwrap()).unwrap(),
        goose
    );
    assert_eq!(
        serde_json::to_value(native()).unwrap(),
        json!({"scope": execution().scope, "session": 11, "run": 22})
    );
}

#[test]
fn goose_binding_is_durable_idempotent_and_projected_on_all_read_paths() {
    let (_directory, path, store, initial) = fixture();
    let delivery = store.turn_events("alice", "pilot", &initial.id, 0).unwrap();
    let bound = store
        .bind_goose("alice", "pilot", &initial.id, execution())
        .unwrap();
    assert_eq!(bound.goose, Some(execution()));
    assert!(bound.native.is_none());
    assert!(bound.runs.is_empty());
    assert_eq!(bound.updated_at, initial.updated_at);
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    let event = activity.last().unwrap();
    assert_eq!(event.kind, "turn.goose_bound");
    assert_eq!(event.data["turn"], initial.id);
    assert_eq!(event.data["goose"], json!(execution()));
    assert_eq!(event.at, snapshot.updated_at);
    assert_eq!(
        store
            .bind_goose("alice", "pilot", &initial.id, execution())
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
    assert_eq!(completed.goose, bound.goose);
    let terminal_delivery = reopened
        .turn_events("alice", "pilot", &initial.id, 0)
        .unwrap();
    assert_eq!(
        terminal_delivery.last().unwrap().data["turn"]["goose"],
        json!(execution())
    );
    let snapshot = reopened.get("alice", "pilot").unwrap();
    let activity = reopened.events("alice", "pilot", 0).unwrap();
    assert_eq!(
        reopened
            .bind_goose("alice", "pilot", &initial.id, execution())
            .unwrap(),
        completed
    );
    assert_eq!(reopened.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(reopened.events("alice", "pilot", 0).unwrap(), activity);
}

#[test]
fn a_goose_session_is_reused_by_later_turns_in_the_same_platform_session() {
    let (_directory, path, store, first) = fixture();
    store
        .bind_goose("alice", "pilot", &first.id, execution())
        .unwrap();
    let first = store
        .finish_turn("alice", "pilot", &first.id, TurnStatus::Completed, None)
        .unwrap();
    let second = store
        .create_turn("alice", "pilot", "second", Some("continue"))
        .unwrap()
        .0;
    let second = store
        .bind_goose("alice", "pilot", &second.id, execution())
        .unwrap();
    assert_eq!(first.goose, second.goose);
    assert_ne!(first.id, second.id);
    assert_eq!(store.interrupt_running().unwrap(), 1);
    let interrupted = store.get_turn("alice", "pilot", &second.id).unwrap();
    assert_eq!(interrupted.status, TurnStatus::Interrupted);
    assert_eq!(interrupted.goose, second.goose);
    drop(store);
    let reopened = SessionStore::open(path).unwrap();
    let third = reopened
        .create_turn("alice", "pilot", "third", None)
        .unwrap()
        .0;
    let third = reopened
        .bind_goose("alice", "pilot", &third.id, execution())
        .unwrap();
    assert_eq!(third.goose, first.goose);
    assert_eq!(
        reopened.get_turn("alice", "pilot", &second.id).unwrap(),
        interrupted
    );
    assert_eq!(reopened.list_turns("alice", "pilot").unwrap().len(), 3);
}

#[test]
fn goose_scope_cannot_be_claimed_by_another_platform_session_or_owner() {
    let (_directory, _, store, first) = fixture();
    store
        .bind_goose("alice", "pilot", &first.id, execution())
        .unwrap();
    let other = create_turn(&store, "alice", "other");
    let foreign = create_turn(&store, "bob", "foreign");
    for (owner, session, turn) in [("alice", "other", &other), ("bob", "foreign", &foreign)] {
        for goose_session in [execution().session, "different-native-session".into()] {
            let before = store.get(owner, session).unwrap();
            let activity = store.events(owner, session, 0).unwrap();
            assert_conflict(store.bind_goose(
                owner,
                session,
                &turn.id,
                GooseExecution {
                    session: goose_session,
                    ..execution()
                },
            ));
            assert_eq!(store.get_turn(owner, session, &turn.id).unwrap(), *turn);
            assert_eq!(store.get(owner, session).unwrap(), before);
            assert_eq!(store.events(owner, session, 0).unwrap(), activity);
        }
    }
    for (owner, session, turn, scope) in [
        ("alice", "other", &other, "a".repeat(64)),
        ("bob", "foreign", &foreign, "b".repeat(64)),
    ] {
        let distinct = GooseExecution {
            scope,
            ..execution()
        };
        let bound = store
            .bind_goose(owner, session, &turn.id, distinct.clone())
            .unwrap();
        assert_eq!(bound.goose, Some(distinct));
    }
}

#[test]
fn goose_binding_is_owner_scoped_and_cannot_access_a_turn_in_another_session() {
    let (_directory, _, store, turn) = fixture();
    let other = create_turn(&store, "alice", "other");
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    for (owner, session, target) in [
        ("bob", "pilot", turn.id.as_str()),
        ("alice", "other", turn.id.as_str()),
        ("alice", "pilot", other.id.as_str()),
        ("alice", "missing", turn.id.as_str()),
        ("alice", "pilot", "missing"),
    ] {
        assert_eq!(
            store.bind_goose(owner, session, target, execution()),
            Err(SessionError::Missing)
        );
    }
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
}

#[test]
fn native_and_goose_bindings_are_mutually_exclusive_in_both_orders() {
    for goose_first in [false, true] {
        let (_directory, path, store, turn) = fixture();
        let bound = if goose_first {
            store
                .bind_goose("alice", "pilot", &turn.id, execution())
                .unwrap()
        } else {
            store
                .bind_native("alice", "pilot", &turn.id, native())
                .unwrap()
        };
        let snapshot = store.get("alice", "pilot").unwrap();
        let activity = store.events("alice", "pilot", 0).unwrap();
        if goose_first {
            assert_conflict(store.bind_native("alice", "pilot", &turn.id, native()));
        } else {
            assert_conflict(store.bind_goose("alice", "pilot", &turn.id, execution()));
        }
        assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), bound);
        assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
        assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
        drop(store);
        let reopened = SessionStore::open(path).unwrap();
        if goose_first {
            assert_conflict(reopened.bind_native("alice", "pilot", &turn.id, native()));
        } else {
            assert_conflict(reopened.bind_goose("alice", "pilot", &turn.id, execution()));
        }
        assert_eq!(
            reopened.get_turn("alice", "pilot", &turn.id).unwrap(),
            bound
        );
    }
}

#[test]
fn first_binding_requires_running_and_terminal_retries_are_idempotent() {
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
                    .bind_goose("alice", "pilot", &turn.id, execution())
                    .unwrap();
            }
            let terminal = store
                .finish_turn("alice", "pilot", &turn.id, status, None)
                .unwrap();
            let snapshot = store.get("alice", "pilot").unwrap();
            let activity = store.events("alice", "pilot", 0).unwrap();
            if bind_first {
                assert_eq!(
                    store
                        .bind_goose("alice", "pilot", &turn.id, execution())
                        .unwrap(),
                    terminal
                );
            } else {
                assert_conflict(store.bind_goose("alice", "pilot", &turn.id, execution()));
            }
            assert_eq!(
                store.get_turn("alice", "pilot", &turn.id).unwrap(),
                terminal
            );
            assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
            assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
        }
    }
}

#[test]
fn changed_scope_or_session_conflicts_and_invalid_ids_leave_no_fact() {
    let (_directory, _, store, turn) = fixture();
    for invalid in [
        GooseExecution {
            scope: "a".repeat(63),
            ..execution()
        },
        GooseExecution {
            scope: "g".repeat(64),
            ..execution()
        },
        GooseExecution {
            scope: "é".repeat(32),
            ..execution()
        },
        GooseExecution {
            session: String::new(),
            ..execution()
        },
        GooseExecution {
            session: "a".repeat(1025),
            ..execution()
        },
        GooseExecution {
            session: "é".repeat(513),
            ..execution()
        },
    ] {
        assert_invalid(store.bind_goose("alice", "pilot", &turn.id, invalid));
    }
    for codepoint in (0..=31).chain(127..=159) {
        assert_invalid(store.bind_goose(
            "alice",
            "pilot",
            &turn.id,
            GooseExecution {
                session: format!("prefix{}suffix", char::from_u32(codepoint).unwrap()),
                ..execution()
            },
        ));
    }
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    let bound = store
        .bind_goose("alice", "pilot", &turn.id, execution())
        .unwrap();
    for conflicting in [
        GooseExecution {
            scope: "f".repeat(64),
            ..execution()
        },
        GooseExecution {
            session: "other".into(),
            ..execution()
        },
    ] {
        assert_conflict(store.bind_goose("alice", "pilot", &turn.id, conflicting));
    }
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), bound);
    let boundary = create_turn(&store, "alice", "boundary");
    let maximum = GooseExecution {
        scope: "A".repeat(64),
        session: "é".repeat(512),
    };
    assert_eq!(
        store
            .bind_goose("alice", "boundary", &boundary.id, maximum.clone())
            .unwrap()
            .goose,
        Some(maximum)
    );
}

#[test]
fn concurrent_native_and_goose_bindings_cannot_mix_execution_facts() {
    let (_directory, path, store, turn) = fixture();
    let independent = SessionStore::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [(false, store.clone()), (true, independent)]
        .into_iter()
        .map(|(goose, store)| {
            let barrier = Arc::clone(&barrier);
            let turn = turn.id.clone();
            thread::spawn(move || {
                barrier.wait();
                if goose {
                    store.bind_goose("alice", "pilot", &turn, execution())
                } else {
                    store.bind_native("alice", "pilot", &turn, native())
                }
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
    let bound = store.get_turn("alice", "pilot", &turn.id).unwrap();
    assert_ne!(bound.native.is_some(), bound.goose.is_some());
    let activity = store.events("alice", "pilot", 0).unwrap();
    assert_eq!(
        activity
            .iter()
            .filter(|event| matches!(
                event.kind.as_str(),
                "turn.native_bound" | "turn.goose_bound"
            ))
            .count(),
        1
    );
}

#[test]
fn independent_connections_cannot_race_to_claim_the_same_goose_scope() {
    let (_directory, path, store, first) = fixture();
    let other = create_turn(&store, "bob", "foreign");
    let independent = SessionStore::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [
        (store.clone(), "alice", "pilot", first),
        (independent, "bob", "foreign", other),
    ]
    .into_iter()
    .map(|(store, owner, session, turn)| {
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            store.bind_goose(
                owner,
                session,
                &turn.id,
                GooseExecution {
                    session: format!("opaque-{session}"),
                    ..execution()
                },
            )
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
}

#[test]
fn event_failure_rolls_back_goose_binding_and_identical_retry_needs_no_new_event() {
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
    assert_storage(store.bind_goose("alice", "pilot", &turn.id, execution()));
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    connection
        .execute("DELETE FROM session_events WHERE seq = ?1", [i64::MAX])
        .unwrap();
    let bound = store
        .bind_goose("alice", "pilot", &turn.id, execution())
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
            .bind_goose("alice", "pilot", &turn.id, execution())
            .unwrap(),
        bound
    );
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
}

#[test]
fn deleting_a_terminal_session_cascades_goose_facts_without_touching_another_owner() {
    let (_directory, path, store, turn) = fixture();
    let foreign = create_turn(&store, "bob", "foreign");
    store
        .bind_goose("alice", "pilot", &turn.id, execution())
        .unwrap();
    let foreign = store
        .bind_goose(
            "bob",
            "foreign",
            &foreign.id,
            GooseExecution {
                scope: "b".repeat(64),
                ..execution()
            },
        )
        .unwrap();
    store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
        .unwrap();
    store.delete("alice", "pilot").unwrap();
    let connection = Connection::open(path).unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM turn_goose WHERE turn_id = ?1",
            [&turn.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        store.get_turn("bob", "foreign", &foreign.id).unwrap(),
        foreign
    );
}

#[test]
fn projection_rejects_dual_runtime_facts_instead_of_guessing_a_runtime() {
    let (_directory, path, store, turn) = fixture();
    store
        .bind_native("alice", "pilot", &turn.id, native())
        .unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "INSERT INTO turn_goose(turn_id, scope, session) VALUES (?1, ?2, ?3)",
            params![turn.id, execution().scope, execution().session],
        )
        .unwrap();
    assert_storage(store.get_turn("alice", "pilot", &turn.id));
    assert_storage(store.list_turns("alice", "pilot"));
    assert_storage(store.create_turn("alice", "pilot", "request", Some("hello")));
    assert_storage(store.bind_goose("alice", "pilot", &turn.id, execution()));
    assert_storage(store.bind_native("alice", "pilot", &turn.id, native()));
}

#[test]
fn goose_table_rejects_invalid_scope_and_session_control_characters() {
    let (_directory, path, _store, turn) = fixture();
    let connection = Connection::open(path).unwrap();
    for codepoint in (0..=31).chain(127..=159) {
        let session = format!("prefix{}suffix", char::from_u32(codepoint).unwrap());
        assert!(
            connection
                .execute(
                    "INSERT INTO turn_goose VALUES (?1, ?2, ?3)",
                    params![turn.id, execution().scope, session]
                )
                .is_err()
        );
    }
    for session in [String::new(), "a".repeat(1025), "é".repeat(513)] {
        assert!(
            connection
                .execute(
                    "INSERT INTO turn_goose VALUES (?1, ?2, ?3)",
                    params![turn.id, execution().scope, session]
                )
                .is_err()
        );
    }
    assert!(
        connection
            .execute(
                "INSERT INTO turn_goose VALUES (?1, ?2, ?3)",
                params![turn.id, "g".repeat(64), execution().session]
            )
            .is_err()
    );
}
