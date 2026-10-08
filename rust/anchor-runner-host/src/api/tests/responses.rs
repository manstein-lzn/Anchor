use super::*;

async fn keyed_call(
    app: Router,
    method: &str,
    uri: &str,
    key: &str,
    body: &str,
) -> (StatusCode, HeaderMap, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(Body::from(body.to_owned()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn responses_json_continuation_and_bearer_owner_isolation() {
    let (root, mut state) = fixture();
    state.api_keys = vec!["a".repeat(32), "b".repeat(32)];
    let app = router_with_web_root(state, root.path().join("web"));
    let first = "a".repeat(32);
    let second = "b".repeat(32);
    let (status, headers, body) = keyed_call(
        app.clone(),
        "POST",
        "/v1/responses",
        &first,
        r#"{"input":"hello"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    let first_response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(first_response["object"], "response");
    assert_eq!(first_response["status"], "completed");
    assert_eq!(first_response["model"], "anchor-copilot");
    assert_eq!(
        first_response["output_text"],
        "deterministic response fixture: hello"
    );
    let response_id = first_response["id"].as_str().unwrap();

    let (status, _, _) = keyed_call(
        app.clone(),
        "POST",
        "/v1/responses",
        &second,
        &json!({"input":"cross-key","previous_response_id":response_id}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = keyed_call(
        app.clone(),
        "POST",
        "/v1/responses",
        &first,
        &json!({"input":"follow up","previous_response_id":response_id}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = keyed_call(
        app.clone(),
        "POST",
        "/v1/responses",
        &first,
        r#"{"input":"x","previous_response_id":"resp_missing"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = keyed_call(
        app.clone(),
        "POST",
        "/v1/responses",
        &first,
        r#"{"input":"x","tools":[]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn responses_sse_has_protocol_events_and_text_delta() {
    let (root, mut state) = fixture();
    state.api_keys = vec!["a".repeat(32)];
    let app = router_with_web_root(state, root.path().join("web"));
    let (status, headers, body) = keyed_call(
        app,
        "POST",
        "/v1/responses",
        &"a".repeat(32),
        r#"{"input":"sse","stream":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    assert_eq!(headers.get("x-accel-buffering").unwrap(), "no");
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-cache");
    assert!(body.contains("event: response.created"), "{body}");
    assert!(body.contains("event: response.in_progress"), "{body}");
    assert!(body.contains("event: response.output_text.delta"), "{body}");
    assert!(body.contains("event: response.completed"), "{body}");
}

#[tokio::test]
async fn responses_rejects_invalid_fields_and_input_shapes() {
    let (root, mut state) = fixture();
    state.api_keys = vec!["a".repeat(32)];
    let app = router_with_web_root(state, root.path().join("web"));
    for body in [
        "{",
        r#"null"#,
        r#"{"input":null}"#,
        r#"{"input":[]}"#,
        r#"{"input":"   "}"#,
        r#"{"input":[{"role":"user"}]}"#,
        r#"{"input":[{"role":"user","content":[]}]}"#,
        r#"{"input":[{"role":"assistant","content":"no"}]}"#,
        r#"{"input":[{"role":"user","content":"ok","name":"extra"}]}"#,
        r#"{"input":[{"role":"user","content":[{"type":"input_text"}]}]}"#,
        r#"{"input":[{"role":"user","content":[{"type":"input_image","url":"x"}]}]}"#,
        r#"{"input":[{"role":"user","content":[{"type":"input_text","text":"x","image_url":"x"}]}]}"#,
        r#"{"input":"x","model":"other"}"#,
        r#"{"input":"x","stream":"true"}"#,
        r#"{"input":"x","temperature":0.2}"#,
    ] {
        let (status, _, response) =
            keyed_call(app.clone(), "POST", "/v1/responses", &"a".repeat(32), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
    }
    let oversized = json!({"input":"x".repeat(64 * 1024 + 1)}).to_string();
    let (status, _, response) = keyed_call(
        app.clone(),
        "POST",
        "/v1/responses",
        &"a".repeat(32),
        &oversized,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    let missing_auth = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"input":"unauthorized"}"#))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(missing_auth).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let (status, _, _) = keyed_call(
        app.clone(),
        "POST",
        "/v1/responses",
        &"c".repeat(32),
        r#"{"input":"unauthorized"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!root.path().join("state/platform/pilot").exists());
}
