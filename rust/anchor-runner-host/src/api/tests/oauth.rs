use super::*;
use anchor_library::{
    OAuthAuthorizationMetadata, OAuthBinding, OAuthClient, OAuthRefreshRequest,
    OAuthRefreshTransport, OAuthTokenResponse, OAuthTransportError,
};
use axum::{
    Json, Router,
    body::Body,
    extract::Form,
    http::{Request, header},
    response::Response,
    routing::{get, post},
};
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    ffi::OsString,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

struct Environment(Vec<(&'static str, Option<OsString>)>);

impl Environment {
    fn cleared(names: &[&'static str]) -> Self {
        let previous = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        for name in names {
            unsafe { std::env::remove_var(name) };
        }
        Self(previous)
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

#[test]
fn oauth_binding_owner_is_stable_private_and_keeps_local_development_scoped() {
    let owner = crate::api::oauth::binding_owner("api:caller-identity");
    assert_eq!(
        owner,
        crate::api::oauth::binding_owner("api:caller-identity")
    );
    assert_ne!(
        owner,
        crate::api::oauth::binding_owner("api:another-caller")
    );
    assert!(!owner.contains("caller-identity"));
    assert_eq!(crate::api::oauth::binding_owner("local"), "local");
}

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

fn write_oauth_plugin_with_resource(state: &ApiState, resource: &str) {
    let plugin = state.catalog_root.join("plugins/oauth-demo");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        serde_json::to_vec(&json!({
            "name": "OAuth Demo",
            "mcpServers": {
                "remote": {
                    "url": resource,
                    "oauth_resource": resource,
                    "auth": "oauth"
                }
            }
        }))
        .unwrap(),
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
    json_response(authorized_response(app, method, uri, key, body).await).await
}

async fn authorized_response(
    app: Router,
    method: &str,
    uri: &str,
    key: &str,
    body: Option<Value>,
) -> Response {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .header(header::CONTENT_TYPE, "application/json");
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    app.oneshot(request.body(body).unwrap()).await.unwrap()
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
    let _variables = Environment::cleared(&["ANCHOR_RUNNER_LIBRARY_ROOT"]);
    let (_root, mut state) = fixture();
    write_oauth_plugin(&state);
    let first = "a".repeat(32);
    let second = "b".repeat(32);
    seed_oauth_authorization(&state, &first);
    state.loopback = false;
    state.api_keys = vec![first.clone(), second.clone()];
    let app = router(state);

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
async fn oauth_browser_authorization_persists_owner_bound_tokens_and_completes_callback() {
    let _environment = PROCESS_ENV.lock().await;
    let _variables =
        Environment::cleared(&["ANCHOR_RUNNER_LIBRARY_ROOT", "ANCHOR_OAUTH_REDIRECT_URI"]);
    unsafe {
        std::env::set_var(
            "ANCHOR_OAUTH_REDIRECT_URI",
            "http://127.0.0.1:8077/oauth/callback",
        );
    }
    let (_root, mut state) = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let resource = format!("{base}/mcp");
    let token_form = Arc::new(Mutex::new(None::<HashMap<String, String>>));
    let token_form_capture = token_form.clone();
    let provider = Router::new()
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get({
                let base = base.clone();
                move || async move {
                    Json(json!({
                        "resource": format!("{base}/mcp"),
                        "authorization_servers":[base]
                    }))
                }
            }),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get({
                let base = base.clone();
                move || async move {
                    Json(json!({
                        "issuer":base,
                        "authorization_endpoint": format!("{base}/authorize"),
                        "token_endpoint": format!("{base}/token"),
                        "registration_endpoint": format!("{base}/register")
                    }))
                }
            }),
        )
        .route(
            "/register",
            post(|| async { Json(json!({"client_id":"registered-client"})) }),
        )
        .route(
            "/token",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let token_form_capture = token_form_capture.clone();
                async move {
                    *token_form_capture.lock().unwrap() = Some(form);
                    Json(json!({
                        "access_token":"mcp-access-secret",
                        "refresh_token":"mcp-refresh-secret",
                        "token_type":"Bearer",
                        "expires_in":3600
                    }))
                }
            }),
        );
    let provider_task = tokio::spawn(async move {
        axum::serve(listener, provider).await.unwrap();
    });
    write_oauth_plugin_with_resource(&state, &resource);
    let first = "a".repeat(32);
    let second = "b".repeat(32);
    state.loopback = false;
    state.api_keys = vec![first.clone(), second.clone()];
    let app = router(state);

    let start_response = authorized_response(
        app.clone(),
        "POST",
        "/plugins/oauth-demo/oauth/remote",
        &first,
        None,
    )
    .await;
    assert_eq!(start_response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(start_response.headers()["referrer-policy"], "no-referrer");
    let (status, start) = json_response(start_response).await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let authorization_url = url::Url::parse(start["authorization_url"].as_str().unwrap()).unwrap();
    assert_eq!(
        authorization_url
            .query_pairs()
            .find(|(key, _)| key == "code_challenge_method")
            .unwrap()
            .1,
        "S256"
    );
    assert_eq!(
        authorization_url
            .query_pairs()
            .find(|(key, _)| key == "resource")
            .unwrap()
            .1,
        resource
    );
    let state_value = authorization_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();

    let duplicate_callback = Request::builder()
        .uri(format!(
            "/oauth/callback?state={state_value}&state={state_value}&code=fixture-code"
        ))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone()
            .oneshot(duplicate_callback)
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    let callback_request = Request::builder()
        .uri(format!(
            "/oauth/callback?state={state_value}&code=fixture-code"
        ))
        .body(Body::empty())
        .unwrap();
    let callback = app.clone().oneshot(callback_request).await.unwrap();
    assert_eq!(callback.status(), StatusCode::OK);
    assert_eq!(callback.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(callback.headers()["referrer-policy"], "no-referrer");
    let callback_body = axum::body::to_bytes(callback.into_body(), 4096)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&callback_body).contains("authorization is complete"));

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
    assert!(!first_status.to_string().contains("mcp-access-secret"));
    let (status, other_status) = authorized_call(
        app.clone(),
        "GET",
        "/plugins/oauth-demo/oauth/remote",
        &second,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{other_status}");
    assert_eq!(other_status, json!({"authorized":false}));

    let posted = token_form.lock().unwrap().clone().unwrap();
    assert_eq!(posted["grant_type"], "authorization_code");
    assert_eq!(posted["code"], "fixture-code");
    assert_eq!(posted["client_id"], "registered-client");
    assert_eq!(
        posted["redirect_uri"],
        "http://127.0.0.1:8077/oauth/callback"
    );
    assert_eq!(posted["resource"], resource);
    assert_eq!(posted["code_verifier"].len(), 43);
    let expected_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(posted["code_verifier"].as_bytes()));
    assert_eq!(
        authorization_url
            .query_pairs()
            .find(|(key, _)| key == "code_challenge")
            .unwrap()
            .1,
        expected_challenge
    );
    let replay = Request::builder()
        .uri(format!(
            "/oauth/callback?state={state_value}&code=fixture-code"
        ))
        .body(Body::empty())
        .unwrap();
    let replay = app.clone().oneshot(replay).await.unwrap();
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert_eq!(replay.headers()[header::CACHE_CONTROL], "no-store");
    provider_task.abort();
}

#[tokio::test]
async fn oauth_routes_reject_token_payloads_and_non_oauth_servers() {
    let _environment = PROCESS_ENV.lock().await;
    let _variables = Environment::cleared(&["ANCHOR_RUNNER_LIBRARY_ROOT"]);
    let (_root, mut state) = fixture();
    write_oauth_plugin(&state);
    let key = "a".repeat(32);
    state.loopback = false;
    state.api_keys = vec![key.clone()];
    let app = router(state);

    let unauthenticated = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/plugins/oauth-demo/oauth/remote")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unauthenticated.headers()[header::CACHE_CONTROL], "no-store");

    let rejected_payload = authorized_response(
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
    assert_eq!(
        rejected_payload.headers()[header::CACHE_CONTROL],
        "no-store"
    );
    let (status, value) = json_response(rejected_payload).await;
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

#[tokio::test]
async fn oauth_provider_discovery_rejects_redirect_responses() {
    let _environment = PROCESS_ENV.lock().await;
    let _variables =
        Environment::cleared(&["ANCHOR_RUNNER_LIBRARY_ROOT", "ANCHOR_OAUTH_REDIRECT_URI"]);
    unsafe {
        std::env::set_var(
            "ANCHOR_OAUTH_REDIRECT_URI",
            "http://127.0.0.1:8077/oauth/callback",
        );
    }
    let (_root, mut state) = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let resource = format!("{base}/mcp");
    let provider = Router::new()
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get({
                let base = base.clone();
                let resource = resource.clone();
                move || async move {
                    (
                        StatusCode::FOUND,
                        [(header::LOCATION, format!("{base}/redirect-target"))],
                        Json(json!({
                            "resource": resource,
                            "authorization_servers": [base]
                        })),
                    )
                }
            }),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get({
                let base = base.clone();
                move || async move {
                    Json(json!({
                        "issuer":base,
                        "authorization_endpoint": format!("{base}/authorize"),
                        "token_endpoint": format!("{base}/token"),
                        "registration_endpoint": format!("{base}/register")
                    }))
                }
            }),
        )
        .route(
            "/register",
            post(|| async { Json(json!({"client_id":"registered-client"})) }),
        );
    let provider_task = tokio::spawn(async move {
        axum::serve(listener, provider).await.unwrap();
    });
    write_oauth_plugin_with_resource(&state, &resource);
    let key = "a".repeat(32);
    state.loopback = false;
    state.api_keys = vec![key.clone()];
    let app = router(state);

    let response =
        authorized_response(app, "POST", "/plugins/oauth-demo/oauth/remote", &key, None).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    provider_task.abort();
}
