use super::*;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use tower::ServiceExt;

fn write_plugin(state: &ApiState) {
    let plugin = state.catalog_root.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills/demo")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Demo","description":"a plugin","skills":"skills/","mcpServers":{"remote":{"url":"https://mcp.example","auth":"oauth"}}}"#,
    )
    .unwrap();
    std::fs::write(
        plugin.join("skills/demo/SKILL.md"),
        "---\ntitle: Demo\n---\n# Instructions\n\nUse the demo resource.",
    )
    .unwrap();
    std::fs::write(plugin.join("notes.txt"), "private note").unwrap();
}

async fn raw(
    app: Router,
    method: &str,
    uri: &str,
    authorization: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(value) = authorization {
        request = request.header(header::AUTHORIZATION, value);
    }
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, body)
}

#[tokio::test]
async fn plugin_catalog_detail_and_file_projection_are_read_only() {
    let (_root, state) = fixture();
    write_plugin(&state);
    let app = router(state.clone());

    let (status, catalog) = call(app.clone(), "GET", "/plugins", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["plugins"][0]["id"], "demo");
    assert_eq!(
        catalog["plugins"][0]["mcpServers"]["remote"]["auth"],
        "oauth"
    );
    assert!(catalog["plugins"][0].get("instructions").is_none());

    let (status, detail) = call(app.clone(), "GET", "/plugins/demo", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        detail["instructions"],
        "# Instructions\n\nUse the demo resource."
    );
    assert_eq!(detail["skills"][0], "skills/demo/SKILL.md");

    let (status, body) = raw(app.clone(), "GET", "/plugins/demo/files/notes.txt", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"private note");

    let (status, body) = raw(app, "GET", "/plugins/demo/files/../plugin.json", None).await;
    assert!(
        matches!(status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND),
        "{status}: {:?}",
        body
    );
}

#[tokio::test]
async fn malformed_plugin_is_unavailable_without_exposing_manifest_values() {
    let (_root, state) = fixture();
    let plugin = state.catalog_root.join("plugins/broken");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":12,"secret":"do-not-return"}"#,
    )
    .unwrap();
    let (status, value) = call(router(state), "GET", "/plugins", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["plugins"][0]["available"], false);
    assert_eq!(
        value["plugins"][0]["error"],
        "Plugin is unavailable or malformed"
    );
    assert!(!value.to_string().contains("do-not-return"));
}

#[tokio::test]
async fn plugin_routes_require_bearer_auth_when_configured() {
    let (_root, mut state) = fixture();
    write_plugin(&state);
    let key = "k".repeat(32);
    state.loopback = false;
    state.api_keys = vec![key.clone()];
    let app = router(state);
    let (status, _) = raw(app.clone(), "GET", "/plugins", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = raw(app, "GET", "/plugins", Some(&format!("Bearer {key}"))).await;
    assert_eq!(status, StatusCode::OK);
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_internal_symlink_is_rejected() {
    use std::os::unix::fs::symlink;
    let (_root, state) = fixture();
    write_plugin(&state);
    let plugin = state.catalog_root.join("plugins/demo");
    symlink("/etc/passwd", plugin.join("escape.txt")).unwrap();
    let (status, value) = call(router(state.clone()), "GET", "/plugins/demo", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"], "Plugin is unavailable or malformed");

    symlink("/etc", state.catalog_root.join("plugins/escape")).unwrap();
    let (status, value) = call(router(state), "GET", "/plugins", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        value["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == "escape")
            .unwrap()["available"],
        false
    );
}
