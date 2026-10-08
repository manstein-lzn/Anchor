use super::*;

#[tokio::test]
async fn graph_webhook_validates_body_and_starts_existing_graph() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    unsafe {
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh,cat,sleep,printf");
    }
    write_graph_bundle(
        &state.bundle_root,
        &json!({
            "objective":"webhook busy fixture",
            "entry":"work",
            "agents":{},
            "ops":{"work":{"run":"printf ready > gate.ready; sleep 1"}},
            "nodes":[{"id":"work","op":"work","plugins":[]}],
            "edges":[]
        }),
    )
    .unwrap();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    for body in [
        r#"[]"#,
        r#"{"input":[]}"#,
        r#"{"input":null}"#,
        r#"{"objective":"replace"}"#,
        r#"{"graph":"other"}"#,
    ] {
        let (status, _) = call(
            app.clone(),
            "POST",
            "/v1/webhooks/graphs/fixture",
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    let (status, missing) = call(
        app.clone(),
        "POST",
        "/v1/webhooks/graphs/not-found",
        Some(r#"{"input":{}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/v1/webhooks/graphs/fixture",
        Some(r#"{"input":{"task":"fixture"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    assert_eq!(accepted["graph"], "fixture");
    let run = accepted["run"].as_str().unwrap().to_owned();
    let (status, busy) = call(
        app.clone(),
        "POST",
        "/v1/webhooks/graphs/fixture",
        Some(r#"{"input":{}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{busy}");
    assert_eq!(busy["running"], run);
    wait_idle(&state).await;
}

#[tokio::test]
async fn graph_webhook_missing_input_defaults_to_empty_object() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let (status, accepted) = call(app, "POST", "/v1/webhooks/graphs/fixture", Some("{}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let run = accepted["run"].as_str().unwrap();
    wait_idle(&state).await;
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(run)
        .unwrap()
        .unwrap();
    assert_eq!(record.input, json!({}));
}

#[tokio::test]
async fn graph_webhook_requires_valid_bearer_before_admission() {
    let (root, mut state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let key = "a".repeat(32);
    state.loopback = false;
    state.api_keys = vec![key.clone()];
    let app = router_with_web_root(state.clone(), root.path().join("web"));

    for authorization in [None, Some(format!("Bearer {}", "b".repeat(32)))] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/v1/webhooks/graphs/fixture")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(authorization) = authorization {
            request = request.header(header::AUTHORIZATION, authorization);
        }
        let response = app
            .clone()
            .oneshot(
                request
                    .body(Body::from(r#"{"input":{"task":"fixture"}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(state.application.records().unwrap().is_empty());

    let request = Request::builder()
        .method("POST")
        .uri("/v1/webhooks/graphs/fixture")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(Body::from(r#"{"input":{"task":"fixture"}}"#))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    wait_idle(&state).await;
}
