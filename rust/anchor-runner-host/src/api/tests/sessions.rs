use super::*;
use crate::application::{
    ConversationSource,
    session_calls::{SessionCall, SessionContext},
};
use anchor_platform_session::{CreateSession, SessionStore};

async fn authorized_call(
    app: Router,
    method: &str,
    uri: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn native_session_lifecycle_events_restart_and_explicit_execution_boundary() {
    let (root, state) = fixture();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let (status, created) = call(
        app.clone(),
        "POST",
        "/sessions",
        Some(r#"{"id":"native-session"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["session"]["conversation_id"], "native-session");
    assert_eq!(created["session"]["status"], "active");
    assert!(!state.data_root.join("io-harness").exists());
    assert!(!root.path().join("sessions").exists());
    let (status, _) = call(
        app.clone(),
        "POST",
        "/sessions",
        Some(r#"{"id":"native-session"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, renamed) = call(
        app.clone(),
        "PUT",
        "/sessions/native-session",
        Some(r#"{"title":"  原生会话  "}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["session"]["title"], "原生会话");
    let (status, interrupted) = call(
        app.clone(),
        "POST",
        "/sessions/native-session/status",
        Some(r#"{"status":"interrupted","reason":"test checkpoint"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(interrupted["session"]["waiting_reason"], "test checkpoint");
    let (status, events) = call(
        app.clone(),
        "GET",
        "/sessions/native-session/events?after=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(events["events"].as_array().unwrap().len(), 2);
    assert_eq!(events["events"][0]["seq"], 2);
    assert_eq!(events["events"][1]["seq"], 3);
    drop(app);
    let app = router_with_web_root(state, root.path().join("web"));
    let (status, reloaded) = call(app.clone(), "GET", "/sessions/native-session", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reloaded["session"], interrupted["session"]);
    for (method, expected) in [("GET", StatusCode::OK), ("POST", StatusCode::BAD_REQUEST)] {
        let (status, _) = call(
            app.clone(),
            method,
            "/sessions/native-session/turns",
            Some(r#"{"message":"do not invoke a model"}"#),
        )
        .await;
        assert_eq!(status, expected);
    }
    let (status, _) = call(app.clone(), "DELETE", "/sessions/native-session", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(app, "GET", "/sessions/native-session", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn native_pilot_validates_inputs_and_reads_durable_turns_without_executing_a_model() {
    let (root, state) = fixture();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/sessions",
            Some(r#"{"id":"turn-api"}"#)
        )
        .await
        .0,
        StatusCode::CREATED
    );
    for body in [
        json!({"message":"missing request id"}),
        json!({"request_id":"request","message":"","resume":false}),
        json!({"request_id":"request","message":"input","resume":true}),
        json!({"request_id":"request","resume":false}),
        json!({"request_id":"request","message":"input","owner":"other"}),
    ] {
        assert_eq!(
            call(
                app.clone(),
                "POST",
                "/sessions/turn-api/turns",
                Some(&body.to_string())
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let sessions = store(&state).unwrap();
    let (turn, _) = sessions
        .create_turn("local", "turn-api", "saved-request", Some("stored prompt"))
        .unwrap();
    sessions
        .append_turn_event(
            "local",
            "turn-api",
            &turn.id,
            json!({"type":"text-delta","delta":"stored reply"}),
        )
        .unwrap();
    let terminal = sessions
        .finish_turn(
            "local",
            "turn-api",
            &turn.id,
            anchor_platform_session::TurnStatus::Completed,
            None,
        )
        .unwrap();
    let (_, repeated) = call(
        app.clone(),
        "POST",
        "/sessions/turn-api/turns",
        Some(r#"{"request_id":"saved-request","message":"stored prompt"}"#),
    )
    .await;
    assert_eq!(repeated["turn"], json!(terminal));
    let (status, _) = call(
        app.clone(),
        "POST",
        "/sessions/turn-api/turns",
        Some(r#"{"request_id":"saved-request","message":"changed prompt"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, messages) = call(app.clone(), "GET", "/sessions/turn-api/messages", None).await;
    #[cfg(feature = "legacy-regression")]
    assert_eq!(messages["messages"], json!([]));
    #[cfg(not(feature = "legacy-regression"))]
    assert_eq!(
        messages["messages"],
        json!([
            {"role":"user","text":"stored prompt"},
            {"role":"assistant","text":"stored reply"}
        ])
    );
    let (_, turns) = call(app.clone(), "GET", "/sessions/turn-api/turns", None).await;
    assert_eq!(turns["turns"], json!([terminal]));
    let request = Request::builder()
        .uri(format!(
            "/sessions/turn-api/turns/{}/events?after=1",
            turn.id
        ))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        text.contains("stored reply") && text.contains("event: turn") && !text.contains("id: 1\n"),
        "{text}"
    );
    let request = Request::builder()
        .uri(format!("/sessions/turn-api/turns/{}/events", turn.id))
        .header("Last-Event-ID", "not-an-integer")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            app.clone(),
            "GET",
            "/sessions/turn-api/turns/missing/events",
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert!(!state.data_root.join("platform/pilot").exists());
}

#[tokio::test]
async fn native_session_create_supports_the_existing_web_composers_empty_post() {
    let (root, state) = fixture();
    let app = router_with_web_root(state, root.path().join("web"));
    let request = Request::builder()
        .method("POST")
        .uri("/sessions")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(created["session"]["title"], "");
    for body in ["broken json", "[]", "null", r#"{"owner":"untrusted"}"#] {
        assert_eq!(
            call(app.clone(), "POST", "/sessions", Some(body)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn native_private_sessions_are_isolated_without_changing_shared_pilot_visibility() {
    let (root, mut state) = fixture();
    let first = "a".repeat(32);
    let second = "b".repeat(32);
    state.api_keys = vec![first.clone(), second.clone()];
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let (status, _) = authorized_call(
        app.clone(),
        "POST",
        "/sessions",
        &first,
        json!({"id":"responses-private"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    for (method, uri, body) in [
        ("GET", "/sessions/responses-private", json!({})),
        ("GET", "/sessions/responses-private/events", json!({})),
        ("GET", "/sessions/responses-private/messages", json!({})),
        ("GET", "/sessions/responses-private/turns", json!({})),
        (
            "GET",
            "/sessions/responses-private/turns/missing",
            json!({}),
        ),
        (
            "GET",
            "/sessions/responses-private/turns/missing/events",
            json!({}),
        ),
        ("POST", "/sessions/responses-private/stop", json!({})),
        (
            "PUT",
            "/sessions/responses-private",
            json!({"title":"stolen"}),
        ),
        (
            "POST",
            "/sessions/responses-private/status",
            json!({"status":"archived"}),
        ),
        (
            "POST",
            "/sessions/responses-private/turns",
            json!({"message":"stolen"}),
        ),
        ("DELETE", "/sessions/responses-private", json!({})),
    ] {
        let (status, _) = authorized_call(app.clone(), method, uri, &second, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
    let (status, listing) =
        authorized_call(app.clone(), "GET", "/sessions", &second, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["sessions"], json!([]));
    let (status, _) = authorized_call(
        app.clone(),
        "POST",
        "/sessions",
        &first,
        json!({"id":"operator-shared"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, listing) =
        authorized_call(app.clone(), "GET", "/sessions", &second, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["sessions"][0]["id"], "operator-shared");
    let (status, _) = authorized_call(
        app.clone(),
        "GET",
        "/sessions/operator-shared",
        "unknown",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for filename in [
        "sessions.sqlite",
        "sessions.sqlite-wal",
        "sessions.sqlite-shm",
    ] {
        let path = state.data_root.join("platform").join(filename);
        if path.exists() {
            let database = std::fs::read(path).unwrap();
            for key in [&first, &second] {
                assert!(
                    !database
                        .windows(key.len())
                        .any(|window| window == key.as_bytes())
                );
            }
        }
    }
}

#[tokio::test]
async fn native_session_storage_does_not_create_files_through_symlinked_parent() {
    use std::os::unix::fs::symlink;
    let (root, state) = fixture();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::create_dir(&state.data_root).unwrap();
    symlink(&outside, state.data_root.join("platform")).unwrap();
    let app = router_with_web_root(state, root.path().join("web"));
    let (status, body) = call(app, "POST", "/sessions", Some(r#"{"id":"blocked"}"#)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!body.to_string().contains(outside.to_str().unwrap()));
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}

#[tokio::test]
async fn native_sessions_cannot_reclaim_or_delete_identity_retained_by_runtime_facts() {
    for source in ["conversation", "call", "pilot"] {
        let (root, state) = fixture();
        let app = router_with_web_root(state.clone(), root.path().join("web"));
        let (status, _) = call(
            app.clone(),
            "POST",
            "/sessions",
            Some(r#"{"id":"retained-runtime"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let bundle = FileGraphBundleLoader::new(&state.bundle_root)
            .load()
            .unwrap();
        let record =
            GraphRunRecord::create_with_id(bundle.snapshot, json!({}), "retained-run").unwrap();
        let mut facts = RunMetadata::new(
            "retained-run".into(),
            "fixture".into(),
            record.graph_digest.clone(),
            &state.bundle_root,
        )
        .unwrap();
        if source == "call" {
            facts.session_call = Some(SessionCall {
                context: SessionContext {
                    session: "retained-runtime".into(),
                    reply_node: "work".into(),
                    conversation_id: "retained-runtime".into(),
                    channel: json!({}),
                },
                status: "pending".into(),
                error: String::new(),
            });
        } else if source == "conversation" {
            facts.conversation = Some(ConversationSource {
                session: "retained-runtime".into(),
                reply_node: "work".into(),
                previous_run: None,
            });
        } else {
            facts.pilot = Some(metadata::PilotRunSource {
                owner: "local".into(),
                session: "retained-runtime".into(),
                turn: "saved-turn".into(),
            });
        }
        metadata::save(&state.data_root, &facts).unwrap();
        FileRunStore::new(state.data_root.join("runs"))
            .save(&record)
            .unwrap();
        let (status, _) = call(app.clone(), "DELETE", "/sessions/retained-runtime", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        SessionStore::open(state.data_root.join("platform/sessions.sqlite"))
            .unwrap()
            .delete("local", "retained-runtime")
            .unwrap();
        let (status, _) = call(
            app,
            "POST",
            "/sessions",
            Some(r#"{"id":"retained-runtime"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            state
                .application
                .metadata("retained-run")
                .unwrap()
                .is_some()
        );
    }
}

#[tokio::test]
async fn startup_reconciles_accepted_pilot_run_without_replaying_or_late_delivery() {
    let (root, state) = fixture();
    let sessions = store(&state).unwrap();
    sessions
        .create(
            "local",
            CreateSession {
                id: Some("pilot-reconcile".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let (turn, _) = sessions
        .create_turn(
            "local",
            "pilot-reconcile",
            "admitted-before-crash",
            Some("Start a Graph once."),
        )
        .unwrap();
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let record =
        GraphRunRecord::create_with_id(bundle.snapshot, json!({}), "pilot-admitted").unwrap();
    let mut facts = RunMetadata::new(
        "pilot-admitted".into(),
        "fixture".into(),
        record.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    facts.pilot = Some(metadata::PilotRunSource {
        owner: "local".into(),
        session: "pilot-reconcile".into(),
        turn: turn.id.clone(),
    });
    metadata::save(&state.data_root, &facts).unwrap();
    FileRunStore::new(state.data_root.join("runs"))
        .save(&record)
        .unwrap();
    assert!(
        sessions
            .get_turn("local", "pilot-reconcile", &turn.id)
            .unwrap()
            .runs
            .is_empty()
    );
    recover_pilot_turns(&state).await.unwrap();
    let reconciled = sessions
        .get_turn("local", "pilot-reconcile", &turn.id)
        .unwrap();
    assert_eq!(
        reconciled.status,
        anchor_platform_session::TurnStatus::Interrupted
    );
    assert_eq!(reconciled.runs, vec!["pilot-admitted"]);
    assert_eq!(
        sessions.get("local", "pilot-reconcile").unwrap().run_ids,
        reconciled.runs
    );
    let events = sessions
        .turn_events("local", "pilot-reconcile", &turn.id, 0)
        .unwrap();
    recover_pilot_turns(&state).await.unwrap();
    assert_eq!(
        sessions
            .get_turn("local", "pilot-reconcile", &turn.id)
            .unwrap(),
        reconciled
    );
    assert_eq!(
        sessions
            .turn_events("local", "pilot-reconcile", &turn.id, 0)
            .unwrap(),
        events
    );
    assert_eq!(
        FileRunStore::new(state.data_root.join("runs"))
            .load("pilot-admitted")
            .unwrap()
            .unwrap(),
        record
    );
    assert!(state.application.active_runs(None).await.is_empty());
    assert!(!state.data_root.join("platform/pilot").exists());
    assert!(!root.path().join("work").exists());
}

#[tokio::test]
async fn native_sessions_reject_body_authority_invalid_updates_and_retained_run_deletion() {
    let (root, state) = fixture();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    for body in [
        r#"{"id":"native","owner":"someone-else"}"#,
        r#"{"graph":"fixture"}"#,
        r#"{"id":"../outside"}"#,
    ] {
        let (status, _) = call(app.clone(), "POST", "/sessions", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    sessions
        .create(
            "local",
            CreateSession {
                id: Some("retained".into()),
                ..Default::default()
            },
        )
        .unwrap();
    sessions
        .attach_run("local", "retained", "run-identity")
        .unwrap();
    let (status, _) = call(app.clone(), "DELETE", "/sessions/retained", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    for (method, uri, body) in [
        ("PUT", "/sessions/retained", r#"{"title":" "}"#),
        (
            "POST",
            "/sessions/retained/status",
            r#"{"status":"unknown"}"#,
        ),
        (
            "POST",
            "/sessions/retained/status",
            r#"{"status":"active","owner":"forged"}"#,
        ),
    ] {
        let (status, _) = call(app.clone(), method, uri, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _) = call(
        app.clone(),
        "POST",
        "/sessions/retained/status",
        Some(r#"{"status":"archived"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        app,
        "POST",
        "/sessions/retained/status",
        Some(r#"{"status":"active"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn enabling_authentication_does_not_publish_previous_loopback_private_sessions() {
    let (root, mut state) = fixture();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    for identifier in ["responses-loopback-private", "operator-shared-before-auth"] {
        let body = json!({"id":identifier}).to_string();
        let (status, _) = call(app.clone(), "POST", "/sessions", Some(&body)).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let first = "a".repeat(32);
    let second = "b".repeat(32);
    state.api_keys = vec![first.clone(), second.clone()];
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    for key in [&first, &second] {
        let (status, listing) =
            authorized_call(app.clone(), "GET", "/sessions", key, json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listing["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(listing["sessions"][0]["id"], "operator-shared-before-auth");
        let (status, _) = authorized_call(
            app.clone(),
            "GET",
            "/sessions/responses-loopback-private",
            key,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = authorized_call(
            app.clone(),
            "POST",
            "/sessions",
            key,
            json!({"id":"responses-loopback-private"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
    let retained = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    assert_eq!(
        retained
            .get("local", "responses-loopback-private")
            .unwrap()
            .id,
        "responses-loopback-private"
    );
}
