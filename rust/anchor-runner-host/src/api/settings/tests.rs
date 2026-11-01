use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;

async fn request(
    app: Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&bytes)}));
    (status, value)
}

#[tokio::test]
async fn named_provider_settings_keep_secrets_private() {
    let (_root, state) = crate::api::tests::fixture();
    let token = "a".repeat(32);
    let configuration = state.model_configuration.clone();
    let app = crate::api::router(state);
    let body = json!({"endpoint":"https://provider.example/v1","wire_api":"chat","flash":"fast","pro":"strong","api_key":{"action":"set","value":"slot-private-key"}});
    assert_eq!(
        configuration.status().unwrap()["providers"]["default"]["source"],
        "empty"
    );
    let (status, value) = request(
        app.clone(),
        "PUT",
        "/settings/model/providers/default",
        Some(&token),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["providers"]["default"]["configured"], true);
    assert_eq!(value["providers"]["default"]["source"], "host");
    assert!(!value.to_string().contains("slot-private-key"));
    let (status, value) = request(app.clone(), "GET", "/settings/model", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["providers"]["default"]["api_key_present"], true);
    let (status, _) = request(
        app.clone(),
        "PUT",
        "/settings/model/providers/other",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        app.clone(),
        "PUT",
        "/settings/model/providers/default",
        Some(&token),
        Some(json!({"unknown":"invalid"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = request(
        app.clone(),
        "POST",
        "/settings/model/providers/default/test",
        Some(&token),
        Some(json!({"tier":"automatic"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = request(
        app,
        "POST",
        "/settings/model/providers/default/test",
        Some(&token),
        Some(json!({"tier":"flash","fallback":true})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        configuration.status().unwrap()["providers"]["default"]["configured"],
        true
    );
}

#[tokio::test]
async fn named_provider_http_test_calls_only_requested_model_without_saving() {
    let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = observations.clone();
    let provider = Router::new().route("/v1/responses", post(move |headers: HeaderMap, Json(body): Json<Value>| {
        observed.lock().unwrap().push((headers[header::AUTHORIZATION].to_str().unwrap().to_owned(), body));
        async { Json(json!({"output":[{"type":"message","content":[{"type":"output_text","text":"OK"}]}]})) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let (_root, state) = crate::api::tests::fixture();
    let token = "a".repeat(32);
    let configuration = state.model_configuration.clone();
    let before = configuration.status().unwrap();
    let app = crate::api::router(state);
    let (status, value) = request(app, "POST", "/settings/model/providers/backup/test", Some(&token), Some(json!({"endpoint":endpoint,"wire_api":"responses","flash":"fast","pro":"strong","tier":"pro","api_key":{"action":"set","value":"test-private-key"}}))).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["model"], "strong");
    assert_eq!(value["saved"], false);
    assert!(!value.to_string().contains("test-private-key"));
    assert_eq!(configuration.status().unwrap(), before);
    let observations = observations.lock().unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].0, "Bearer test-private-key");
    assert_eq!(observations[0].1["model"], "strong");
    assert_eq!(observations[0].1["max_output_tokens"], 16);
    assert_eq!(observations[0].1["stream"], false);
    server.abort();
}

#[tokio::test]
async fn runtime_get_reports_the_configured_python_full_version() {
    use crate::runtime_configuration::{RuntimeConfiguration, RuntimeSnapshot};
    use std::os::unix::fs::PermissionsExt;
    let (root, mut state) = crate::api::tests::fixture();
    let python = root.path().join("python-3.9.0/bin/python3");
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(&python, "#!/bin/sh\nprintf '3.12.13\\n'\n").unwrap();
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
    let configuration = Arc::new(RuntimeConfiguration::from_snapshot_for_test(
        root.path(),
        &root.path().join("library"),
        "bwrap".into(),
        RuntimeSnapshot {
            python: Some(python),
            python_enabled: true,
            rust: None,
            pixi: None,
            provisioner: None,
        },
    ));
    state.runtime_management = Arc::new(
        crate::runtime_management::RuntimeManagement::new(configuration, root.path()).unwrap(),
    );
    let app = crate::api::router(state);
    let (status, value) = request(app, "GET", "/settings/runtime", None, None).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["runtime"]["python_version"], "3.12.13");
    assert!(value["runtime"]["python_version_error"].is_null());
    assert_eq!(value["runtime"]["python_enabled"], true);
    assert!(!root.path().join("managed/runtime-config.json").exists());
}
