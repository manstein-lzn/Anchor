use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use anchor_platform_session::{
    CreateSession, Question, QuestionAction, QuestionAnswer, QuestionStatus, SessionError,
    SessionStatus, SessionStore, Turn, TurnStatus, validate_question_schema,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf, SessionStore, Turn) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.sqlite");
    let store = SessionStore::open(&path).unwrap();
    store
        .create(
            "alice",
            CreateSession {
                id: Some("pilot".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let turn = store
        .create_turn("alice", "pilot", "request", Some("hello"))
        .unwrap()
        .0;
    (directory, path, store, turn)
}

fn schema() -> Value {
    json!({"type":"object", "properties":{
        "choice":{"type":"string", "enum":["alpha","beta"], "enumNames":["A","B"]},
        "count":{"type":"integer", "minimum":1, "maximum":5},
        "ratio":{"type":"number", "exclusiveMinimum":0, "exclusiveMaximum":1},
        "label":{"type":"string", "minLength":2, "maxLength":8, "pattern":"^[a-z]+$"},
        "confirm":{"type":"boolean"}
    }, "required":["choice"]})
}

fn pending(store: &SessionStore, turn: &Turn) -> Question {
    store
        .create_question("alice", "pilot", &turn.id, "Which choice?", schema())
        .unwrap()
}

fn accept(content: Value) -> QuestionAnswer {
    QuestionAnswer {
        action: QuestionAction::Accept,
        content: Some(content),
    }
}

fn assert_invalid<Output>(result: Result<Output, SessionError>) {
    assert!(matches!(result, Err(SessionError::Invalid(_))));
}

fn assert_conflict<Output>(result: Result<Output, SessionError>) {
    assert!(matches!(result, Err(SessionError::Conflict(_))));
}

#[test]
fn question_contract_serializes_exact_fields_and_rejects_unknown_answer_fields() {
    let (_directory, _, store, turn) = fixture();
    let question = pending(&store, &turn);
    let value = serde_json::to_value(&question).unwrap();
    assert_eq!(
        value,
        json!({
            "id":question.id, "session":"pilot", "turn":turn.id, "message":"Which choice?",
            "requested_schema":schema(), "status":"pending", "answer":null
        })
    );
    assert_eq!(serde_json::from_value::<Question>(value).unwrap(), question);
    assert_eq!(
        serde_json::from_value::<QuestionAnswer>(json!({"action":"decline"})).unwrap(),
        QuestionAnswer {
            action: QuestionAction::Decline,
            content: None
        }
    );
    assert!(
        serde_json::from_value::<QuestionAnswer>(
            json!({"action":"accept", "content":{}, "owner":"bob"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<QuestionAnswer>(json!({"action":"Accept", "content":{}})).is_err()
    );
    for (status, name) in [
        (QuestionStatus::Pending, "pending"),
        (QuestionStatus::Answered, "answered"),
        (QuestionStatus::Interrupted, "interrupted"),
    ] {
        assert_eq!(serde_json::to_value(status).unwrap(), name);
    }
}

#[test]
fn pending_question_is_durable_keeps_the_turn_running_and_allows_sse_to_continue() {
    let (_directory, path, store, turn) = fixture();
    let question = pending(&store, &turn);
    let snapshot = store.get("alice", "pilot").unwrap();
    assert_eq!(snapshot.status, SessionStatus::WaitingUser);
    assert_eq!(snapshot.waiting_reason, question.message);
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(store.list_turns("alice", "pilot").unwrap().len(), 1);
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    assert_eq!(
        events.last().unwrap().data,
        json!({"type":"question", "question":question})
    );
    let activity = store.events("alice", "pilot", 0).unwrap();
    assert_eq!(activity.last().unwrap().kind, "question");
    assert_eq!(activity.last().unwrap().data["question"], json!(question));
    assert_eq!(activity.last().unwrap().at, snapshot.updated_at);
    assert_conflict(store.set_status("alice", "pilot", SessionStatus::Archived, "archive"));
    assert_conflict(store.create_turn("alice", "pilot", "another", Some("new")));
    store
        .append_turn_event(
            "alice",
            "pilot",
            &turn.id,
            json!({"type":"text-delta", "delta":"waiting"}),
        )
        .unwrap();
    drop(store);
    let reopened = SessionStore::open(path).unwrap();
    assert_eq!(
        reopened
            .get_question("alice", "pilot", &turn.id, &question.id)
            .unwrap(),
        question
    );
    assert_eq!(reopened.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(
        reopened
            .get_turn("alice", "pilot", &turn.id)
            .unwrap()
            .status,
        TurnStatus::Running
    );
}

#[test]
fn all_question_operations_check_owner_before_payload_status_or_answer() {
    let (_directory, _, store, turn) = fixture();
    let question = pending(&store, &turn);
    assert_eq!(
        store.create_question(
            "bob",
            "pilot",
            &turn.id,
            "",
            json!({"$ref":"https://example.invalid"})
        ),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.get_question("bob", "pilot", &turn.id, &question.id),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.list_questions("bob", "pilot", &turn.id),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.answer_question(
            "bob",
            "pilot",
            &turn.id,
            &question.id,
            QuestionAnswer {
                action: QuestionAction::Cancel,
                content: Some(json!({}))
            }
        ),
        Err(SessionError::Missing)
    );
    store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Stopped, None)
        .unwrap();
    assert_eq!(
        store.answer_question("bob", "pilot", &turn.id, &question.id, accept(json!({}))),
        Err(SessionError::Missing)
    );
}

#[test]
fn question_lookup_is_scoped_to_both_session_and_turn() {
    let (_directory, _, store, turn) = fixture();
    let question = pending(&store, &turn);
    store
        .create(
            "alice",
            CreateSession {
                id: Some("other".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let other = store
        .create_turn("alice", "other", "other", None)
        .unwrap()
        .0;
    assert_eq!(
        store.get_question("alice", "other", &other.id, &question.id),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.answer_question(
            "alice",
            "other",
            &other.id,
            &question.id,
            accept(json!({"choice":"alpha"}))
        ),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.get_question("alice", "pilot", &other.id, &question.id),
        Err(SessionError::Missing)
    );
    assert_eq!(
        store.list_questions("alice", "pilot", "absent"),
        Err(SessionError::Missing)
    );
    assert_invalid(store.get_question("alice", "pilot", &turn.id, "../escape"));
    assert_invalid(store.answer_question("alice", "pilot", "", &question.id, accept(json!({}))));
}

#[test]
fn accept_is_cas_idempotent_without_new_turn_or_duplicate_events() {
    let (_directory, path, store, turn) = fixture();
    let question = pending(&store, &turn);
    let answer =
        accept(json!({"choice":"alpha", "count":3, "ratio":0.5, "label":"valid", "confirm":true}));
    let answered = store
        .answer_question("alice", "pilot", &turn.id, &question.id, answer.clone())
        .unwrap();
    assert_eq!(answered.status, QuestionStatus::Answered);
    assert_eq!(answered.answer, Some(answer.clone()));
    let snapshot = store.get("alice", "pilot").unwrap();
    assert_eq!(snapshot.status, SessionStatus::Active);
    assert!(snapshot.waiting_reason.is_empty());
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    assert_eq!(
        events.last().unwrap().data,
        json!({"type":"question-answered", "question":answered})
    );
    assert_eq!(activity.last().unwrap().kind, "question-answered");
    assert_eq!(
        store
            .answer_question("alice", "pilot", &turn.id, &question.id, answer.clone())
            .unwrap(),
        answered
    );
    assert_conflict(store.answer_question(
        "alice",
        "pilot",
        &turn.id,
        &question.id,
        accept(json!({"choice":"beta"})),
    ));
    assert_conflict(store.answer_question(
        "alice",
        "pilot",
        &turn.id,
        &question.id,
        QuestionAnswer {
            action: QuestionAction::Cancel,
            content: None,
        },
    ));
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(
        store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        events
    );
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    assert_eq!(
        store.list_turns("alice", "pilot").unwrap(),
        vec![turn.clone()]
    );
    drop(store);
    let reopened = SessionStore::open(path).unwrap();
    assert_eq!(
        reopened
            .answer_question("alice", "pilot", &turn.id, &question.id, answer)
            .unwrap(),
        answered
    );
}

#[test]
fn invalid_answers_leave_question_session_and_delivery_unchanged() {
    let (_directory, _, store, turn) = fixture();
    let question = pending(&store, &turn);
    let snapshot = store.get("alice", "pilot").unwrap();
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    for content in [
        json!(null),
        json!("alpha"),
        json!([]),
        json!({}),
        json!({"choice":"gamma"}),
        json!({"choice":3}),
        json!({"choice":"alpha", "extra":true}),
        json!({"choice":"alpha", "count":0}),
        json!({"choice":"alpha", "count":6}),
        json!({"choice":"alpha", "count":2.5}),
        json!({"choice":"alpha", "ratio":0}),
        json!({"choice":"alpha", "ratio":1}),
        json!({"choice":"alpha", "label":"a"}),
        json!({"choice":"alpha", "label":"toolongvalue"}),
        json!({"choice":"alpha", "label":"BAD"}),
        json!({"choice":"alpha", "confirm":"true"}),
    ] {
        assert_invalid(store.answer_question(
            "alice",
            "pilot",
            &turn.id,
            &question.id,
            accept(content),
        ));
    }
    assert_invalid(store.answer_question(
        "alice",
        "pilot",
        &turn.id,
        &question.id,
        QuestionAnswer {
            action: QuestionAction::Accept,
            content: None,
        },
    ));
    for action in [QuestionAction::Decline, QuestionAction::Cancel] {
        assert_invalid(store.answer_question(
            "alice",
            "pilot",
            &turn.id,
            &question.id,
            QuestionAnswer {
                action,
                content: Some(json!({})),
            },
        ));
    }
    assert_eq!(
        store
            .get_question("alice", "pilot", &turn.id, &question.id)
            .unwrap(),
        question
    );
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(
        store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        events
    );
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
}

#[test]
fn decline_and_cancel_resolve_only_the_question_not_the_running_turn() {
    for action in [QuestionAction::Decline, QuestionAction::Cancel] {
        let (_directory, _, store, turn) = fixture();
        let question = pending(&store, &turn);
        let answer = QuestionAnswer {
            action,
            content: None,
        };
        let answered = store
            .answer_question("alice", "pilot", &turn.id, &question.id, answer.clone())
            .unwrap();
        assert_eq!(answered.answer, Some(answer));
        assert_eq!(answered.status, QuestionStatus::Answered);
        assert_eq!(
            store.get("alice", "pilot").unwrap().status,
            SessionStatus::Active
        );
        assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    }
}

#[test]
fn only_one_pending_question_per_turn_but_sequential_questions_remain_in_order() {
    let (_directory, _, store, turn) = fixture();
    let first = pending(&store, &turn);
    let snapshot = store.get("alice", "pilot").unwrap();
    assert_conflict(store.create_question("alice", "pilot", &turn.id, "Another?", schema()));
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    let answer = accept(json!({"choice":"alpha"}));
    let answered = store
        .answer_question("alice", "pilot", &turn.id, &first.id, answer.clone())
        .unwrap();
    let second = pending(&store, &turn);
    assert_ne!(first.id, second.id);
    assert_eq!(
        store.list_questions("alice", "pilot", &turn.id).unwrap(),
        vec![answered.clone(), second.clone()]
    );
    let waiting = store.get("alice", "pilot").unwrap();
    assert_eq!(
        store
            .answer_question("alice", "pilot", &turn.id, &first.id, answer)
            .unwrap(),
        answered
    );
    assert_eq!(store.get("alice", "pilot").unwrap(), waiting);
    assert_eq!(waiting.status, SessionStatus::WaitingUser);
}

#[test]
fn every_terminal_turn_interrupts_pending_before_terminal_sse_and_rejects_answers() {
    for status in [
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Stopped,
        TurnStatus::Interrupted,
    ] {
        let (_directory, _, store, turn) = fixture();
        let question = pending(&store, &turn);
        store
            .finish_turn("alice", "pilot", &turn.id, status, Some("end"))
            .unwrap();
        let interrupted = store
            .get_question("alice", "pilot", &turn.id, &question.id)
            .unwrap();
        assert_eq!(interrupted.status, QuestionStatus::Interrupted);
        assert!(interrupted.answer.is_none());
        let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
        assert_eq!(
            events[events.len() - 2].data,
            json!({"type":"question-interrupted", "question":interrupted})
        );
        assert_eq!(
            events.last().unwrap().data["turn"]["status"],
            serde_json::to_value(status).unwrap()
        );
        let activity = store.events("alice", "pilot", 0).unwrap();
        assert_eq!(activity[activity.len() - 2].kind, "question-interrupted");
        assert_eq!(activity[activity.len() - 2].at, activity.last().unwrap().at);
        assert_conflict(store.answer_question(
            "alice",
            "pilot",
            &turn.id,
            &question.id,
            accept(json!({"choice":"alpha"})),
        ));
        assert_conflict(store.create_question("alice", "pilot", &turn.id, "new", schema()));
        assert_conflict(store.append_turn_event(
            "alice",
            "pilot",
            &turn.id,
            json!({"type":"text-delta"}),
        ));
        store
            .finish_turn("alice", "pilot", &turn.id, status, Some("end"))
            .unwrap();
        assert_eq!(
            store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
            events
        );
    }
}

#[test]
fn restart_interruption_preserves_answered_facts_and_allows_identical_history_retries() {
    let (_directory, path, store, turn) = fixture();
    let first = pending(&store, &turn);
    let answer = accept(json!({"choice":"alpha"}));
    let answered = store
        .answer_question("alice", "pilot", &turn.id, &first.id, answer.clone())
        .unwrap();
    let second = pending(&store, &turn);
    drop(store);
    let reopened = SessionStore::open(&path).unwrap();
    assert_eq!(
        reopened
            .get_question("alice", "pilot", &turn.id, &second.id)
            .unwrap()
            .status,
        QuestionStatus::Pending
    );
    assert_eq!(reopened.interrupt_running().unwrap(), 1);
    assert_eq!(
        reopened.get("alice", "pilot").unwrap().status,
        SessionStatus::Interrupted
    );
    assert_eq!(
        reopened
            .get_turn("alice", "pilot", &turn.id)
            .unwrap()
            .status,
        TurnStatus::Interrupted
    );
    assert_eq!(
        reopened
            .get_question("alice", "pilot", &turn.id, &first.id)
            .unwrap(),
        answered
    );
    assert_eq!(
        reopened
            .get_question("alice", "pilot", &turn.id, &second.id)
            .unwrap()
            .status,
        QuestionStatus::Interrupted
    );
    assert_eq!(
        reopened
            .answer_question("alice", "pilot", &turn.id, &first.id, answer)
            .unwrap(),
        answered
    );
    assert_conflict(reopened.answer_question(
        "alice",
        "pilot",
        &turn.id,
        &second.id,
        accept(json!({"choice":"alpha"})),
    ));
    let events = reopened.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    assert_eq!(reopened.interrupt_running().unwrap(), 0);
    assert_eq!(
        reopened.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        events
    );
    let new_turn = reopened
        .create_turn("alice", "pilot", "continue", None)
        .unwrap()
        .0;
    assert_eq!(
        reopened
            .answer_question(
                "alice",
                "pilot",
                &turn.id,
                &first.id,
                accept(json!({"choice":"alpha"}))
            )
            .unwrap(),
        answered
    );
    assert_eq!(
        reopened
            .get_turn("alice", "pilot", &new_turn.id)
            .unwrap()
            .status,
        TurnStatus::Running
    );
}

#[test]
fn identical_answer_retry_on_terminal_turn_is_read_only_even_with_a_new_pending_question() {
    for status in [
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Stopped,
        TurnStatus::Interrupted,
    ] {
        let (_directory, path, store, turn) = fixture();
        let question = pending(&store, &turn);
        let answer = accept(json!({"choice":"alpha"}));
        let answered = store
            .answer_question("alice", "pilot", &turn.id, &question.id, answer.clone())
            .unwrap();
        store
            .finish_turn("alice", "pilot", &turn.id, status, None)
            .unwrap();
        let snapshot = store.get("alice", "pilot").unwrap();
        let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
        let activity = store.events("alice", "pilot", 0).unwrap();
        assert_eq!(
            store
                .answer_question("alice", "pilot", &turn.id, &question.id, answer.clone())
                .unwrap(),
            answered
        );
        assert_conflict(store.answer_question(
            "alice",
            "pilot",
            &turn.id,
            &question.id,
            accept(json!({"choice":"beta"})),
        ));
        assert_eq!(
            store.answer_question("bob", "pilot", &turn.id, &question.id, answer.clone()),
            Err(SessionError::Missing)
        );
        assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
        assert_eq!(
            store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
            events
        );
        assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
        let next = store.create_turn("alice", "pilot", "next", None).unwrap().0;
        let next_question = pending(&store, &next);
        let waiting = store.get("alice", "pilot").unwrap();
        let activity = store.events("alice", "pilot", 0).unwrap();
        drop(store);
        let reopened = SessionStore::open(path).unwrap();
        assert_eq!(
            reopened
                .answer_question("alice", "pilot", &turn.id, &question.id, answer)
                .unwrap(),
            answered
        );
        assert_eq!(reopened.get("alice", "pilot").unwrap(), waiting);
        assert_eq!(reopened.events("alice", "pilot", 0).unwrap(), activity);
        assert_eq!(
            reopened
                .get_question("alice", "pilot", &next.id, &next_question.id)
                .unwrap()
                .status,
            QuestionStatus::Pending
        );
        assert_eq!(
            reopened.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
            events
        );
    }
}

#[test]
fn question_creation_rolls_back_snapshot_and_fact_when_turn_sse_write_fails() {
    let (_directory, path, store, turn) = fixture();
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_question_sse BEFORE INSERT ON turn_events
        WHEN json_extract(NEW.data, '$.type') = 'question'
        BEGIN SELECT RAISE(ABORT, 'injected question SSE failure'); END;",
        )
        .unwrap();
    let result = store.create_question("alice", "pilot", &turn.id, "Choice?", schema());
    assert!(matches!(result, Err(SessionError::Storage(_))));
    assert!(
        store
            .list_questions("alice", "pilot", &turn.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    assert_eq!(
        store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        events
    );
}

#[test]
fn answering_rolls_back_question_snapshot_and_sse_when_session_event_write_fails() {
    let (_directory, path, store, turn) = fixture();
    let question = pending(&store, &turn);
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_answer_activity BEFORE INSERT ON session_events
        WHEN NEW.kind = 'question-answered'
        BEGIN SELECT RAISE(ABORT, 'injected answer activity failure'); END;",
        )
        .unwrap();
    let result = store.answer_question(
        "alice",
        "pilot",
        &turn.id,
        &question.id,
        accept(json!({"choice":"alpha"})),
    );
    assert!(matches!(result, Err(SessionError::Storage(_))));
    assert_eq!(
        store
            .get_question("alice", "pilot", &turn.id, &question.id)
            .unwrap(),
        question
    );
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    assert_eq!(
        store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        events
    );
}

#[test]
fn turn_finish_rolls_back_all_facts_when_question_interruption_sse_write_fails() {
    let (_directory, path, store, turn) = fixture();
    let question = pending(&store, &turn);
    let snapshot = store.get("alice", "pilot").unwrap();
    let activity = store.events("alice", "pilot", 0).unwrap();
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_interrupt_sse BEFORE INSERT ON turn_events
        WHEN json_extract(NEW.data, '$.type') = 'question-interrupted'
        BEGIN SELECT RAISE(ABORT, 'injected interruption SSE failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        store.finish_turn("alice", "pilot", &turn.id, TurnStatus::Stopped, None),
        Err(SessionError::Storage(_))
    ));
    assert!(matches!(
        store.interrupt_running(),
        Err(SessionError::Storage(_))
    ));
    assert_eq!(
        store
            .get_question("alice", "pilot", &turn.id, &question.id)
            .unwrap(),
        question
    );
    assert_eq!(store.get_turn("alice", "pilot", &turn.id).unwrap(), turn);
    assert_eq!(store.get("alice", "pilot").unwrap(), snapshot);
    assert_eq!(store.events("alice", "pilot", 0).unwrap(), activity);
    assert_eq!(
        store.turn_events("alice", "pilot", &turn.id, 0).unwrap(),
        events
    );
}

#[test]
fn answered_question_survives_normal_completion_and_session_delete_cascades_questions() {
    let (_directory, path, store, turn) = fixture();
    let question = pending(&store, &turn);
    let answered = store
        .answer_question(
            "alice",
            "pilot",
            &turn.id,
            &question.id,
            accept(json!({"choice":"alpha"})),
        )
        .unwrap();
    store
        .finish_turn("alice", "pilot", &turn.id, TurnStatus::Completed, None)
        .unwrap();
    assert_eq!(
        store
            .get_question("alice", "pilot", &turn.id, &question.id)
            .unwrap(),
        answered
    );
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.data["type"] == "question-interrupted")
    );
    store.delete("alice", "pilot").unwrap();
    let connection = Connection::open(path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM questions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn schema_subset_rejects_refs_nested_objects_unknown_keywords_and_malformed_constraints() {
    for schema in [
        json!(false),
        json!({}),
        json!({"type":"array", "items":{"type":"string"}}),
        json!({"type":"object", "properties":{}, "$ref":"https://example.invalid/schema"}),
        json!({"type":"object", "properties":{"name":{"type":"string", "$ref":"file:///etc/passwd"}}}),
        json!({"type":"object", "properties":{"name":{"type":"string", "$ref":"#/definitions/name"}}}),
        json!({"type":"object", "properties":{"nested":{"type":"object", "properties":{}}}}),
        json!({"type":"object", "properties":{"nested":{"type":"array", "items":{"type":"string"}}}}),
        json!({"type":"object", "properties":{"name":{"type":["string","null"]}}}),
        json!({"type":"object", "properties":{"name":{"type":"string", "oneOf":[]}}}),
        json!({"type":"object", "properties":{"name":{"type":"string", "minLength":-1}}}),
        json!({"type":"object", "properties":{"name":{"type":"string", "pattern":"["}}}),
        json!({"type":"object", "properties":{"name":{"type":"string", "enum":[3]}}}),
        json!({"type":"object", "properties":{"name":{"type":"string", "enum":[]}}}),
        json!({"type":"object", "properties":{"name":{"type":"string", "default":{}}}}),
        json!({"type":"object", "properties":{"name":{"type":"string", "enumNames":["A"]}}}),
        json!({"type":"object", "properties":{"name":{"type":"string", "enum":["alpha"], "enumNames":[]}}}),
        json!({"type":"object", "properties":{"name":{"type":"string", "format":"invented"}}}),
        json!({"type":"object", "properties":{}, "required":["unknown"]}),
        json!({"type":"object", "properties":{}, "additionalProperties":true}),
        json!({"type":"object", "properties":{}, "required":[3]}),
        json!({"type":"object", "properties":{}, "title":3}),
        json!({"type":"object", "properties":{"count":{"type":"integer", "multipleOf":0}}}),
        json!({"type":"object", "properties":{"count":{"type":"integer", "minimum":"1"}}}),
    ] {
        assert_invalid(validate_question_schema(&schema));
    }
    validate_question_schema(&schema()).unwrap();
    validate_question_schema(
        &json!({"type":"object", "properties":{}, "additionalProperties":false}),
    )
    .unwrap();
}

#[test]
fn schema_string_formats_and_numeric_constraints_are_checked_by_the_validator() {
    let (_directory, _, store, turn) = fixture();
    let schema = json!({"type":"object", "properties":{
        "email":{"type":"string", "format":"email"}, "uri":{"type":"string", "format":"uri"},
        "date":{"type":"string", "format":"date"}, "time":{"type":"string", "format":"date-time"},
        "count":{"type":"integer", "multipleOf":2}
    }});
    let question = store
        .create_question("alice", "pilot", &turn.id, "Fields?", schema)
        .unwrap();
    for content in [
        json!({"email":"invalid"}),
        json!({"uri":"relative"}),
        json!({"date":"2026-99-99"}),
        json!({"time":"not-a-time"}),
        json!({"count":3}),
    ] {
        assert_invalid(store.answer_question(
            "alice",
            "pilot",
            &turn.id,
            &question.id,
            accept(content),
        ));
    }
    store
        .answer_question(
            "alice",
            "pilot",
            &turn.id,
            &question.id,
            accept(json!({
                "email":"user@example.com", "uri":"https://example.com", "date":"2026-10-06",
                "time":"2026-10-06T12:34:56Z", "count":4
            })),
        )
        .unwrap();
}

#[test]
fn messages_schemas_and_serialized_answers_are_bounded_in_utf8_bytes() {
    let (_directory, _, store, turn) = fixture();
    for message in [
        String::new(),
        " \n ".into(),
        "x".repeat(65537),
        "界".repeat(21846),
    ] {
        assert_invalid(store.create_question("alice", "pilot", &turn.id, &message, schema()));
    }
    let large_schema = json!({"type":"object", "properties":{}, "description":"x".repeat(65536)});
    assert_invalid(store.create_question("alice", "pilot", &turn.id, "Fields?", large_schema));
    let unbounded = json!({"type":"object", "properties":{"value":{"type":"string"}}});
    let question = store
        .create_question("alice", "pilot", &turn.id, &"x".repeat(65536), unbounded)
        .unwrap();
    for value in ["x".repeat(65536), "界".repeat(21846), "\n".repeat(32768)] {
        assert_invalid(store.answer_question(
            "alice",
            "pilot",
            &turn.id,
            &question.id,
            accept(json!({"value":value})),
        ));
    }
    store
        .answer_question(
            "alice",
            "pilot",
            &turn.id,
            &question.id,
            accept(json!({"value":"x".repeat(65000)})),
        )
        .unwrap();
}

#[test]
fn competing_answers_on_independent_connections_commit_exactly_one_answer_event() {
    let (_directory, path, store, turn) = fixture();
    let question = pending(&store, &turn);
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = ["alpha", "beta"]
        .into_iter()
        .map(|choice| {
            let barrier = barrier.clone();
            let path = path.clone();
            let turn = turn.id.clone();
            let question = question.id.clone();
            thread::spawn(move || {
                let connection = SessionStore::open(path).unwrap();
                barrier.wait();
                connection.answer_question(
                    "alice",
                    "pilot",
                    &turn,
                    &question,
                    accept(json!({"choice":choice})),
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
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.data["type"] == "question-answered")
            .count(),
        1
    );
    assert_eq!(
        store
            .list_questions("alice", "pilot", &turn.id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn identical_competing_answers_are_both_successful_without_duplicate_events() {
    let (_directory, path, store, turn) = fixture();
    let question = pending(&store, &turn);
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            let path = path.clone();
            let turn = turn.id.clone();
            let question = question.id.clone();
            thread::spawn(move || {
                let connection = SessionStore::open(path).unwrap();
                barrier.wait();
                connection
                    .answer_question(
                        "alice",
                        "pilot",
                        &turn,
                        &question,
                        accept(json!({"choice":"alpha"})),
                    )
                    .unwrap()
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results[0], results[1]);
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.data["type"] == "question-answered")
            .count(),
        1
    );
}

#[test]
fn competing_question_creation_preserves_single_pending_invariant() {
    let (_directory, path, store, turn) = fixture();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            let path = path.clone();
            let turn = turn.id.clone();
            thread::spawn(move || {
                let connection = SessionStore::open(path).unwrap();
                barrier.wait();
                connection.create_question("alice", "pilot", &turn, "Choice?", schema())
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
    assert_eq!(
        store
            .list_questions("alice", "pilot", &turn.id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn answer_racing_stop_commits_an_answer_or_interruption_never_revives_the_turn() {
    let (_directory, path, store, turn) = fixture();
    let question = pending(&store, &turn);
    let barrier = Arc::new(Barrier::new(2));
    let answer_worker = {
        let path = path.clone();
        let barrier = barrier.clone();
        let turn = turn.id.clone();
        let question = question.id.clone();
        thread::spawn(move || {
            let connection = SessionStore::open(path).unwrap();
            barrier.wait();
            connection.answer_question(
                "alice",
                "pilot",
                &turn,
                &question,
                accept(json!({"choice":"alpha"})),
            )
        })
    };
    let stop_worker = {
        let barrier = barrier.clone();
        let turn = turn.id.clone();
        thread::spawn(move || {
            let connection = SessionStore::open(path).unwrap();
            barrier.wait();
            connection
                .finish_turn("alice", "pilot", &turn, TurnStatus::Stopped, None)
                .unwrap()
        })
    };
    let answer = answer_worker.join().unwrap();
    let stopped = stop_worker.join().unwrap();
    assert_eq!(stopped.status, TurnStatus::Stopped);
    assert_eq!(
        store.get("alice", "pilot").unwrap().status,
        SessionStatus::Interrupted
    );
    assert_eq!(
        store.get_turn("alice", "pilot", &turn.id).unwrap().status,
        TurnStatus::Stopped
    );
    let final_question = store
        .get_question("alice", "pilot", &turn.id, &question.id)
        .unwrap();
    let expected_event = match answer {
        Ok(answered) => {
            assert_eq!(final_question, answered);
            assert_eq!(final_question.status, QuestionStatus::Answered);
            "question-answered"
        }
        Err(SessionError::Conflict(_)) => {
            assert_eq!(final_question.status, QuestionStatus::Interrupted);
            assert!(final_question.answer.is_none());
            "question-interrupted"
        }
        Err(error) => panic!("unexpected race result: {error}"),
    };
    let events = store.turn_events("alice", "pilot", &turn.id, 0).unwrap();
    assert_eq!(events[events.len() - 2].data["type"], expected_event);
    assert_eq!(events.last().unwrap().data["type"], "turn.stopped");
}
