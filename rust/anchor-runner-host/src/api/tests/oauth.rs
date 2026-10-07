use super::*;
use anchor_library::{
    OAuthAuthorizationMetadata, OAuthBinding, OAuthClient, OAuthRefreshRequest,
    OAuthRefreshTransport, OAuthTokenResponse, OAuthTransportError,
};
use axum::{
    body::Body,
    http::{Request, header},
    response::Response,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

struct NoRefreshTransport;

impl OAuthRefreshTransport for NoRefreshTransport {
    fn refresh(
        &self,
        _request: &OAuthRefreshRequest<'_>,
    ) -> Result<OAuthTokenResponse, OAuthTransportError> {
        Err(OAuthTransportError::RequestFailed)
    }
}

fn write_oauth_plugin(state: &ApiState) {
    let plugin = state.catalog_root.join("plugins/oauth-demo");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"OAuth Demo","mcpServers":{"remote":{"url":"https://mcp.example.test/mcp","auth":"oauth","oauth_resource":"https://mcp.example.test"},"plain":{"url":"https://plain.example.test/mcp"}}}"#,
    )
    .unwrap();
}

fn seed_oauth_authorization(state: &ApiState, key: &str) {
    let owner = format!("api:{:x}", Sha256::digest(key.as_bytes()));
    let owner = format!("{:x}", Sha256::digest(owner.as_bytes()));
    let binding = OAuthBinding::new(
        format!("{:x}", Sha256::digest(b"https://mcp.example.test")),
        "oauth-demo",
        "remote",
        owner,
    )
    .unwrap();
    let metadata = OAuthAuthorizationMetadata::new("https://mcp.example.test/token")
        .unwrap()
        .with_client_id("anchor-fixture")
        .unwrap()
        .with_scope("read write")
        .unwrap();
    let response =
        OAuthTokenResponse::new("access-secret", Some("refresh-secret".into()), Some(3600))
            .unwrap()
            .with_token_type("Bearer")
            .unwrap()
            .with_scope("read write")
            .unwrap();
    OAuthClient::new(
        anchor_library::FileOAuthTokenStore::new(&state.catalog_root),
        NoRefreshTransport,
    )
    .authorize(binding, metadata, response, 100)
    .unwrap();
}

async fn authorized_call(
    app: Router,
    method: &str,
    uri: &str,
    key: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .header(header::CONTENT_TYPE, "application/json");
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    let response = app.oneshot(request.body(body).unwrap()).await.unwrap();
    json_response(response).await
}

async fn json_response(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn oauth_status_and_revoke_are_owner_scoped_and_never_return_secrets() {
    let _environment = PROCESS_ENV.lock().await;
    unsafe { std::env::remove_var("ANCHOR_RUNNER_LIBRARY_ROOT") };
    let (_root, mut state) = fixture();
    write_oauth_plugin(&state);
    let first = "a".repeat(32);
    let second = "b".repeat(32);
    seed_oauth_authorization(&state, &first);
    state.loopback = false;
    state.api_keys = vec![first.clone(), second.clone()];
    let app = router(state);

    let (status, rejected) = authorized_call(
        app.clone(),
        "POST",
        "/plugins/oauth-demo/oauth/remote",
        &first,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{rejected}");

    let (status, first_status) = authorized_call(
        app.clone(),
        "GET",
        "/plugins/oauth-demo/oauth/remote",
        &first,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first_status}");
    assert_eq!(first_status["authorized"], true);
    assert_eq!(first_status["scope"], "read write");

    let (status, second_status) = authorized_call(
        app.clone(),
        "GET",
        "/plugins/oauth-demo/oauth/remote",
        &second,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second_status}");
    assert_eq!(second_status, json!({"authorized":false}));

    let (status, revoked) = authorized_call(
        app.clone(),
        "DELETE",
        "/plugins/oauth-demo/oauth/remote",
        &second,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    let (status, still_authorized) = authorized_call(
        app.clone(),
        "GET",
        "/plugins/oauth-demo/oauth/remote",
        &first,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{still_authorized}");
    assert_eq!(still_authorized["authorized"], true);

    let (status, revoked) = authorized_call(
        app,
        "DELETE",
        "/plugins/oauth-demo/oauth/remote",
        &first,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
}

#[tokio::test]
async fn oauth_routes_reject_token_payloads_and_non_oauth_servers() {
    let _environment = PROCESS_ENV.lock().await;
    unsafe { std::env::remove_var("ANCHOR_RUNNER_LIBRARY_ROOT") };
    let (_root, mut state) = fixture();
    write_oauth_plugin(&state);
    let key = "a".repeat(32);
    state.loopback = false;
    state.api_keys = vec![key.clone()];
    let app = router(state);

    let (status, value) = authorized_call(
        app.clone(),
        "POST",
        "/plugins/oauth-demo/oauth/remote",
        &key,
        Some(json!({
            "token_endpoint":"https://mcp.example.test/token",
            "access_token":"access-secret",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(
        value["error"],
        "OAuth authorization does not accept token payloads"
    );
    assert!(!value.to_string().contains("access-secret"));

    let (status, value) = authorized_call(
        app.clone(),
        "POST",
        "/plugins/oauth-demo/oauth/plain",
        &key,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(value["error"], "this MCP server does not declare OAuth");

    let (status, value) =
        authorized_call(app, "GET", "/plugins/missing/oauth/remote", &key, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{value}");
}
