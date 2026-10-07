use super::*;
use anchor_platform_session::{CreateSession, TurnStatus};

async fn question_call(
    app: Router,
    method: &str,
    uri: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn schema() -> Value {
    json!({"type":"object","properties":{"choice":{"type":"string","enum":["yes","no"]}},"required":["choice"],"additionalProperties":false})
}

#[tokio::test]
async fn question_api_preserves_the_turn_and_validates_answer_retries() {
    let (root, state) = fixture();
    let sessions = store(&state).unwrap();
    sessions
        .create(
            "local",
            CreateSession {
                id: Some("question-session".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let (turn, _) = sessions
        .create_turn("local", "question-session", "one", Some("ask"))
        .unwrap();
    let question = sessions
        .create_question("local", "question-session", &turn.id, "Choose", schema())
        .unwrap();
    let app = router_with_web_root(state, root.path().join("web"));
    let uri = format!("/sessions/question-session/turns/{}/questions", turn.id);
    let (status, saved) = question_call(app.clone(), "GET", &uri, "", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["questions"][0]["status"], "pending");
    assert_eq!(
        sessions
            .get_turn("local", "question-session", &turn.id)
            .unwrap()
            .status,
        TurnStatus::Running
    );
    assert_eq!(
        sessions.get("local", "question-session").unwrap().status,
        anchor_platform_session::SessionStatus::WaitingUser
    );
    let answer_uri = format!("{uri}/{}/answer", question.id);
    for invalid in [
        json!({"action":"accept","content":{}}),
        json!({"action":"accept","content":{"choice":"other"}}),
        json!({"action":"accept","content":{"choice":"yes","unknown":true}}),
        json!({"action":"decline","content":{"choice":"yes"}}),
        json!({"action":"accept","content":{"choice":"yes"},"unexpected":true}),
    ] {
        let (status, saved) = question_call(app.clone(), "POST", &answer_uri, "", invalid).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{saved}");
    }
    let answer = json!({"action":"accept","content":{"choice":"yes"}});
    let (status, saved) = question_call(app.clone(), "POST", &answer_uri, "", answer.clone()).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["question"]["status"], "answered");
    let events = sessions
        .turn_events("local", "question-session", &turn.id, 0)
        .unwrap();
    assert!(events.iter().any(|event| event.data["type"] == "question"));
    assert!(
        events
            .iter()
            .any(|event| event.data["type"] == "question-answered")
    );
    let (status, retry) = question_call(app.clone(), "POST", &answer_uri, "", answer.clone()).await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry, saved);
    assert_eq!(
        sessions
            .turn_events("local", "question-session", &turn.id, 0)
            .unwrap(),
        events
    );
    sessions
        .finish_turn(
            "local",
            "question-session",
            &turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    assert_eq!(
        question_call(app.clone(), "POST", &answer_uri, "", answer)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        question_call(app, "POST", &answer_uri, "", json!({"action":"decline"}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        sessions
            .list_turns("local", "question-session")
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn question_api_is_owner_scoped_and_rejects_late_answers_after_recovery() {
    let (root, mut state) = fixture();
    state.loopback = false;
    let first = "a".repeat(32);
    let second = "b".repeat(32);
    state.api_keys = vec![first.clone(), second.clone()];
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    assert_eq!(
        question_call(
            app.clone(),
            "POST",
            "/sessions",
            &first,
            json!({"id":"responses-private-question"})
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {first}").parse().unwrap(),
    );
    let (sessions, owner, _) =
        owned_session(&state, &headers, "responses-private-question").unwrap();
    let (turn, _) = sessions
        .create_turn(&owner, "responses-private-question", "one", Some("ask"))
        .unwrap();
    let question = sessions
        .create_question(
            &owner,
            "responses-private-question",
            &turn.id,
            "private message",
            schema(),
        )
        .unwrap();
    let uri = format!(
        "/sessions/responses-private-question/turns/{}/questions",
        turn.id
    );
    let answer_uri = format!("{uri}/{}/answer", question.id);
    assert_eq!(
        question_call(app.clone(), "GET", &uri, &second, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        question_call(
            app.clone(),
            "POST",
            &answer_uri,
            &second,
            json!({"action":"decline"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    recover_pilot_turns(&state).await.unwrap();
    let (status, saved) = question_call(app.clone(), "GET", &uri, &first, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["questions"][0]["status"], "interrupted");
    assert_eq!(
        question_call(
            app,
            "POST",
            &answer_uri,
            &first,
            json!({"action":"decline"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
