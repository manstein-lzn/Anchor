use super::*;
use anchor_graph_host::FilePluginCatalog;
use anchor_library::{OAuthBinding, OAuthError};
use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::HeaderMap,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct OAuthRouteBinding {
    binding: OAuthBinding,
    library_root: std::path::PathBuf,
}

pub(super) async fn oauth_status(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((plugin, server)): AxumPath<(String, String)>,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let route = resolve_binding(&state, &plugin, &server, &owner)?;
        let store = anchor_library::FileOAuthTokenStore::new(route.library_root);
        let authorization = store.load(&route.binding).map_err(oauth_error)?;
        Ok(Json(project_authorization(&authorization)))
    })
    .await
}

pub(super) async fn authorize_oauth(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((plugin, server)): AxumPath<(String, String)>,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let _route = resolve_binding(&state, &plugin, &server, &owner)?;
        if !body.is_empty() {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "OAuth authorization does not accept token payloads",
            ));
        }
        Err(error(
            StatusCode::NOT_IMPLEMENTED,
            "interactive OAuth authorization is not connected",
        ))
    })
    .await
}

pub(super) async fn revoke_oauth(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((plugin, server)): AxumPath<(String, String)>,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let route = resolve_binding(&state, &plugin, &server, &owner)?;
        let store = anchor_library::FileOAuthTokenStore::new(route.library_root);
        store.revoke(&route.binding).map_err(oauth_error)?;
        Ok(Json(json!({"authorized": false, "revoked": true})))
    })
    .await
}

fn resolve_binding(
    state: &ApiState,
    plugin: &str,
    server: &str,
    owner: &str,
) -> Result<OAuthRouteBinding, HttpResponse> {
    let library_root = super::plugins::library_root(&state.catalog_root);
    let definition = FilePluginCatalog::new(&library_root)
        .definition(plugin)
        .map_err(|_| error(StatusCode::NOT_FOUND, "no such Plugin"))?;
    let declaration = definition
        .mcp_servers
        .iter()
        .find(|candidate| candidate.name == server)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "no such MCP server"))?;
    let config = &declaration.config;
    if config.get("command").is_some()
        || (config.get("auth").and_then(Value::as_str) != Some("oauth")
            && config.get("oauth_resource").is_none())
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "this MCP server does not declare OAuth",
        ));
    }
    let provider_material = config
        .get("oauth_resource")
        .and_then(Value::as_str)
        .or_else(|| config.get("url").and_then(Value::as_str))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "OAuth MCP server requires a URL"))?;
    let provider = format!("{:x}", Sha256::digest(provider_material.as_bytes()));
    let oauth_owner = if owner == "local" {
        owner.to_owned()
    } else {
        format!("{:x}", Sha256::digest(owner.as_bytes()))
    };
    let binding = OAuthBinding::new(provider, plugin, server, oauth_owner)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid OAuth binding"))?;
    Ok(OAuthRouteBinding {
        binding,
        library_root,
    })
}

fn project_authorization(authorization: &Option<anchor_library::OAuthAuthorization>) -> Value {
    let now = unix_time().unwrap_or(0);
    match authorization {
        Some(authorization) => json!({
            "authorized": true,
            "expired": authorization.is_expired_at(now),
            "issued_at": authorization.issued_at(),
            "expires_at": authorization.expires_at(),
            "token_type": authorization.token_type(),
            "scope": authorization.metadata().scope(),
        }),
        None => json!({"authorized": false}),
    }
}

fn unix_time() -> Result<u64, OAuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| OAuthError::Storage)
}

fn oauth_error(failure: OAuthError) -> HttpResponse {
    match failure {
        OAuthError::NotAuthorized => error(
            StatusCode::NOT_FOUND,
            "OAuth authorization is not available",
        ),
        OAuthError::InvalidMetadata | OAuthError::InvalidResponse => {
            error(StatusCode::BAD_REQUEST, "invalid OAuth authorization")
        }
        OAuthError::MissingRefreshToken | OAuthError::RefreshFailed | OAuthError::Revoked => {
            error(StatusCode::CONFLICT, "OAuth authorization is not usable")
        }
        OAuthError::Storage => error(StatusCode::INTERNAL_SERVER_ERROR, "OAuth storage failed"),
    }
}
