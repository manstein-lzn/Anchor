use super::*;
use crate::application::channel_management::fake::FakeChannels;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

fn fixture() -> (tempfile::TempDir, ApiState, Arc<FakeChannels>) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("task")).unwrap();
    crate::application::channel_management::fake::install_channel_plugin(root.path(), "wecom");
    std::fs::write(root.path().join("task/graph.json"), r#"{"entry":"wait","agents":{"reply":{"model":"fixture","instructions":"fixture"}},"ops":{"wait":{"host":{"operation":"session.wait_input"}},"deliver":{"host":{"operation":"session.reply"}}},"nodes":[{"id":"wait","op":"wait"},{"id":"reply","agent":"reply","plugins":["wecom"]},{"id":"deliver","op":"deliver"}],"edges":[{"from":"wait","to":"reply"},{"from":"reply","to":"deliver"},{"from":"deliver","to":"wait"}]}"#).unwrap();
    std::fs::write(
        root.path().join("task/manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[{"id":"wecom"}]}"#,
    )
    .unwrap();
    let port = Arc::new(FakeChannels::new());
    let (model_configuration, runtime_management) =
        crate::api::tests::management_fixture(root.path());
    let state = ApiState {
        model_configuration,
        runtime_management,
        bundle_root: root.path().join("task"),
        catalog_root: root.path().into(),
        data_root: root.path().join("state"),
        workspace_root: root.path().join("workspaces"),
        graph_name: String::new(),
        application: RunApplication::new(root.path().join("state"), root.path().into())
            .with_channel_management(port.clone()),
        owner: "local".into(),
        schedules: super::super::schedules::ScheduleStore::open(root.path().join("schedules.json"))
            .unwrap(),
        pilots: Default::default(),
        channel_event_locks: Default::default(),
        response_fixture: None,
    };
    (root, state, port)
}
async fn call(
    state: &ApiState,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        request = request.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    let response = super::super::router(state.clone())
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

#[tokio::test]
async fn channel_management_http_is_owner_bound_and_checks_expected_binding() {
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (_root, state, port) = fixture();
    let key = "a".repeat(32);
    let path = "/graphs/task/channel";
    let members = json!({"users":["alice"],"send_users":["alice"]});
    let (status, result) = call(&state, "PUT", path, Some(&key), members).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let saved = port.get("task").unwrap();
    assert_eq!(saved.owner, "local");
    // The Plugin comes from the Graph, never from the request.
    assert_eq!(saved.binding.plugin, "wecom");
    let binding = serde_json::to_value(&saved.binding).unwrap();
    let mut stale = binding.clone();
    stale["users"] = json!(["bob"]);
    let (status, _) = call(
        &state,
        "POST",
        "/graphs/task/channel/enabled",
        Some(&key),
        json!({"enabled":true,"expected_binding":stale}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!port.get("task").unwrap().enabled);
    let (status, result) = call(
        &state,
        "POST",
        "/graphs/task/channel/enabled",
        Some(&key),
        json!({"enabled":true,"expected_binding":binding}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(port.get("task").unwrap().enabled);
    let (status, listed) = call(&state, "GET", "/channels", Some(&key), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["channels"][0]["graph"], "task");
}

#[tokio::test]
async fn channel_management_http_rejects_secrets_owner_forgery_and_unbound_enable() {
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (_root, state, port) = fixture();
    let key = "a".repeat(32);
    let members = json!({"users":[],"send_users":[]});
    for (field, value) in [
        ("owner", json!("forged")),
        ("WECOM_BOT_SECRET", json!("private")),
        ("account", json!("bot")),
        // Graph, node and Plugin are derived, never chosen by the caller.
        ("graph", json!("other")),
        ("reply_node", json!("reply")),
        ("plugin", json!("wecom")),
    ] {
        let mut request = members.clone();
        request[field] = value;
        assert_eq!(
            call(&state, "PUT", "/graphs/task/channel", Some(&key), request)
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "{field}"
        );
    }
    assert!(port.get("task").is_none());
    assert_eq!(
        call(
            &state,
            "POST",
            "/graphs/task/channel/enabled",
            Some(&key),
            json!({"enabled":true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            "/graphs/missing/channel",
            Some(&key),
            members
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // The single-bot routes are gone.
    assert_eq!(
        call(
            &state,
            "GET",
            "/channels/wecom/configuration",
            Some(&key),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn channel_binding_requires_a_persistent_assistant_graph() {
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, state, port) = fixture();
    let key = "a".repeat(32);
    let members = json!({"users":["alice"],"send_users":["alice"]});
    let single = r#"{"entry":"reply","agents":{"reply":{"model":"fixture","instructions":"fixture"}},"nodes":[{"id":"reply","agent":"reply","plugins":["wecom"]}],"edges":[]}"#;
    let persistent = std::fs::read(root.path().join("task/graph.json")).unwrap();
    std::fs::write(root.path().join("task/graph.json"), single).unwrap();
    let (status, refused) = call(
        &state,
        "PUT",
        "/graphs/task/channel",
        Some(&key),
        members.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("persistent assistant"),
        "{refused}"
    );
    assert!(port.get("task").is_none());

    // A binding saved while the Graph was an assistant cannot be enabled once
    // the Graph has lost its input loop.
    std::fs::write(root.path().join("task/graph.json"), persistent).unwrap();
    let (status, result) = call(&state, "PUT", "/graphs/task/channel", Some(&key), members).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let binding = serde_json::to_value(port.get("task").unwrap().binding).unwrap();
    std::fs::write(root.path().join("task/graph.json"), single).unwrap();
    let (status, refused) = call(
        &state,
        "POST",
        "/graphs/task/channel/enabled",
        Some(&key),
        json!({"enabled":true,"expected_binding":binding}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(!port.get("task").unwrap().enabled);
}

#[tokio::test]
async fn credentials_of_a_bot_saved_enabled_cannot_change_even_when_it_is_not_running() {
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (_root, state, port) = fixture();
    let key = "a".repeat(32);
    let (status, _) = call(
        &state,
        "PUT",
        "/graphs/task/channel",
        Some(&key),
        json!({"users":["alice"],"send_users":["alice"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    port.update("task", |bot| bot.saved_enabled = true);
    let write = json!({"authorization_digest":"0".repeat(64),"value":"another-bot"});
    let path = "/graphs/task/channel/credentials/WECOM_BOT_ID";
    let (status, refused) = call(&state, "PUT", path, Some(&key), write.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        refused.to_string().contains("stop this Graph's bot"),
        "{refused}"
    );
    // Once stopped, the request reaches the credential store's own checks.
    port.update("task", |bot| bot.saved_enabled = false);
    let (_, response) = call(&state, "PUT", path, Some(&key), write).await;
    assert!(
        !response.to_string().contains("stop this Graph's bot"),
        "{response}"
    );
}
