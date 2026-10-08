use super::*;
use crate::PROCESS_ENV_LOCK;
use anchor_library::{
    FileOAuthTokenStore, OAuthAuthorizationMetadata, OAuthBinding, OAuthClient,
    OAuthRefreshRequest, OAuthRefreshTransport, OAuthTokenResponse, OAuthTransportError,
};
use axum::{Json, Router, extract::Form, routing::post};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tempfile::tempdir;

struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);

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

struct NoRefresh;

impl OAuthRefreshTransport for NoRefresh {
    fn refresh(
        &self,
        _request: &OAuthRefreshRequest<'_>,
    ) -> Result<OAuthTokenResponse, OAuthTransportError> {
        Err(OAuthTransportError::RequestFailed)
    }
}

#[tokio::test]
async fn mcp_oauth_header_uses_the_frozen_run_owner_and_is_redacted_from_debug() {
    let _environment_guard = PROCESS_ENV_LOCK.lock().await;
    let _environment = Environment::cleared(&[
        "ANCHOR_RUNNER_LIBRARY_ROOT",
        "ANCHOR_RUNNER_CATALOG_ROOT",
        "ANCHOR_RUNNER_BUNDLE_ROOT",
    ]);
    let root = tempdir().unwrap();
    let plugin_directory = root.path().join("plugins/oauth-demo");
    std::fs::create_dir_all(&plugin_directory).unwrap();
    let resource = "http://127.0.0.1:43210/mcp";
    std::fs::write(
        plugin_directory.join("plugin.json"),
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
    let catalog = FilePluginCatalog::new(root.path());
    let bindings = catalog.resolve(&["oauth-demo".into()]).unwrap();
    let owner = "run-owner-hash";
    let binding = OAuthBinding::new(
        format!("{:x}", Sha256::digest(resource.as_bytes())),
        "oauth-demo",
        "remote",
        owner,
    )
    .unwrap();
    let metadata = OAuthAuthorizationMetadata::new("https://provider.example/token")
        .unwrap()
        .with_client_id("fixture-client")
        .unwrap();
    OAuthClient::new(FileOAuthTokenStore::new(root.path()), NoRefresh)
        .authorize(
            binding,
            metadata,
            OAuthTokenResponse::new("mcp-access-secret", None, None).unwrap(),
            100,
        )
        .unwrap();

    let mut config = McpToolConfig::from_catalog(root.path(), &bindings).unwrap();
    config
        .authorize_oauth(Some(owner.into()), true)
        .await
        .unwrap();
    let authorization = config.servers["oauth-demo-remote"].config["headers"]["Authorization"]
        .as_str()
        .unwrap();
    assert_eq!(authorization, "Bearer mcp-access-secret");
    assert!(!format!("{config:?}").contains("mcp-access-secret"));

    let mut other_owner = McpToolConfig::from_catalog(root.path(), &bindings).unwrap();
    assert!(
        other_owner
            .authorize_oauth(Some("different-run-owner".into()), true)
            .await
            .is_err()
    );

    let mut network_disabled = McpToolConfig::from_catalog(root.path(), &bindings).unwrap();
    assert_eq!(
        network_disabled
            .authorize_oauth(Some(owner.into()), false)
            .await
            .unwrap_err(),
        "HTTP MCP requires node network=true"
    );
}

#[tokio::test]
async fn expired_oauth_token_refreshes_through_pinned_loopback_transport() {
    let _environment_guard = PROCESS_ENV_LOCK.lock().await;
    let _environment = Environment::cleared(&[
        "ANCHOR_RUNNER_LIBRARY_ROOT",
        "ANCHOR_RUNNER_CATALOG_ROOT",
        "ANCHOR_RUNNER_BUNDLE_ROOT",
    ]);
    let root = tempdir().unwrap();
    let plugin_directory = root.path().join("plugins/oauth-demo");
    std::fs::create_dir_all(&plugin_directory).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let resource = format!("http://{address}/mcp");
    let received = Arc::new(Mutex::new(None::<HashMap<String, String>>));
    let received_request = received.clone();
    let provider = Router::new().route(
        "/token",
        post(move |Form(form): Form<HashMap<String, String>>| {
            let received_request = received_request.clone();
            async move {
                *received_request.lock().unwrap() = Some(form);
                Json(json!({
                    "access_token": "refreshed-access-secret",
                    "token_type": "Bearer",
                    "expires_in": 3600
                }))
            }
        }),
    );
    let provider_task = tokio::spawn(async move {
        axum::serve(listener, provider).await.unwrap();
    });
    std::fs::write(
        plugin_directory.join("plugin.json"),
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
    let catalog = FilePluginCatalog::new(root.path());
    let bindings = catalog.resolve(&["oauth-demo".into()]).unwrap();
    let owner = "run-owner-hash";
    let binding = OAuthBinding::new(
        format!("{:x}", Sha256::digest(resource.as_bytes())),
        "oauth-demo",
        "remote",
        owner,
    )
    .unwrap();
    let metadata = OAuthAuthorizationMetadata::new(format!("http://{address}/token"))
        .unwrap()
        .with_client_id("fixture-client")
        .unwrap()
        .with_resource(resource.clone())
        .unwrap();
    OAuthClient::new(FileOAuthTokenStore::new(root.path()), NoRefresh)
        .authorize(
            binding,
            metadata,
            OAuthTokenResponse::new(
                "expired-access-secret",
                Some("refresh-secret".into()),
                Some(1),
            )
            .unwrap(),
            0,
        )
        .unwrap();

    let mut config = McpToolConfig::from_catalog(root.path(), &bindings).unwrap();
    config
        .authorize_oauth(Some(owner.into()), true)
        .await
        .unwrap();

    assert_eq!(
        config.servers["oauth-demo-remote"].config["headers"]["Authorization"],
        "Bearer refreshed-access-secret"
    );
    let request = received.lock().unwrap().clone().unwrap();
    assert_eq!(request["grant_type"], "refresh_token");
    assert_eq!(request["refresh_token"], "refresh-secret");
    assert_eq!(request["client_id"], "fixture-client");
    assert_eq!(request["resource"], resource);
    provider_task.abort();
}

#[test]
fn oauth_token_store_follows_the_host_catalog_fallback() {
    let catalog = Path::new("/srv/anchor/catalog");
    assert_eq!(
        resolve_oauth_library_root(
            catalog,
            None,
            None,
            Some(PathBuf::from("/srv/anchor/catalog/bundles/main")),
        ),
        catalog
    );
    assert_eq!(
        resolve_oauth_library_root(
            catalog,
            Some(PathBuf::from("/srv/anchor/library")),
            Some(PathBuf::from("/srv/anchor/catalog")),
            None,
        ),
        PathBuf::from("/srv/anchor/library")
    );
}
