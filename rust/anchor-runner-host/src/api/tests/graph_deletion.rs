use super::*;

async fn conditional_delete(app: Router, name: &str, conditions: &[&str]) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("DELETE")
        .uri(format!("/graphs/{name}"));
    for condition in conditions {
        request = request.header(header::IF_MATCH, *condition);
    }
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn conditional_graph_delete_http_keeps_unchanged_target_and_legacy_behavior() {
    let (_root, state) = fixture();
    let app = router(state.clone());
    let (status, snapshot) = call(
        app.clone(),
        "GET",
        "/graphs/fixture/delete-precondition",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{snapshot}");
    let expected = format!("\"{}\"", snapshot["precondition"].as_str().unwrap());
    let (status, result) = conditional_delete(app.clone(), "fixture", &[&expected]).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{result}");
    assert!(!state.bundle_root.exists());

    let (status, result) = call(
        app.clone(),
        "POST",
        "/graphs",
        Some(&json!({"name":"legacy"}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{result}");
    let (status, result) = call(app, "DELETE", "/graphs/legacy", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{result}");
}

#[tokio::test]
async fn conditional_graph_delete_http_refuses_changed_recreated_and_wrong_targets() {
    let (_root, state) = fixture();
    let app = router(state.clone());
    let (_, snapshot) = call(
        app.clone(),
        "GET",
        "/graphs/fixture/delete-precondition",
        None,
    )
    .await;
    let expected = format!("\"{}\"", snapshot["precondition"].as_str().unwrap());
    let (status, graph) = call(app.clone(), "GET", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::OK);
    let mut definition = graph["definition"].clone();
    definition["objective"] = json!("changed after confirmation");
    let (status, result) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (status, result) = conditional_delete(app.clone(), "fixture", &[&expected]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    assert!(state.bundle_root.is_dir());

    let (status, result) = call(
        app.clone(),
        "POST",
        "/graphs",
        Some(&json!({"name":"other"}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{result}");
    let (status, result) = conditional_delete(app.clone(), "other", &[&expected]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");

    let (_, snapshot) = call(
        app.clone(),
        "GET",
        "/graphs/other/delete-precondition",
        None,
    )
    .await;
    let expected = format!("\"{}\"", snapshot["precondition"].as_str().unwrap());
    let (status, _) = call(app.clone(), "DELETE", "/graphs/other", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        app.clone(),
        "POST",
        "/graphs",
        Some(&json!({"name":"other"}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, result) = conditional_delete(app, "other", &[&expected]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    assert!(state.catalog_root.join("other").is_dir());
}

#[tokio::test]
async fn conditional_graph_delete_http_rejects_malformed_headers_without_deleting() {
    let (_root, state) = fixture();
    let app = router(state.clone());
    for conditions in [
        vec!["*"],
        vec!["unquoted"],
        vec!["W/\"weak\""],
        vec!["\"one\", \"two\""],
        vec!["\"invalid\""],
        vec!["\"one\"", "\"two\""],
    ] {
        let (status, result) = conditional_delete(app.clone(), "fixture", &conditions).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{conditions:?} {result}");
        assert!(state.bundle_root.is_dir());
    }
    let (status, _) = call(app, "GET", "/graphs/missing/delete-precondition", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn conditional_graph_delete_http_preserves_host_authentication() {
    let (_root, mut state) = fixture();
    let key = "operator-key-".repeat(4);
    state.api_keys = vec![key.clone()];
    let app = router(state.clone());
    let (status, _) = call(
        app.clone(),
        "GET",
        "/graphs/fixture/delete-precondition",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/graphs/fixture/delete-precondition")
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let snapshot: Value = serde_json::from_slice(&body).unwrap();
    let expected = format!("\"{}\"", snapshot["precondition"].as_str().unwrap());
    let (status, _) = conditional_delete(app, "fixture", &[&expected]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(state.bundle_root.is_dir());
}
