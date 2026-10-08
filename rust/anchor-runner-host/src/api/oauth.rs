use super::*;
use anchor_graph_host::FilePluginCatalog;
use anchor_library::{
    OAuthAuthorizationMetadata, OAuthAuthorizationResponse, OAuthAuthorizationTransaction,
    OAuthBinding, OAuthError, OAuthTokenResponse,
};
use axum::{
    Json,
    body::Bytes,
    extract::{Path as AxumPath, RawQuery, State},
    http::HeaderMap,
    response::Html,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};
use url::Url;

const OAUTH_TRANSACTION_TTL_SECONDS: u64 = 600;
const MAX_OAUTH_RESPONSE_BYTES: usize = 128 * 1024;
const OAUTH_CALLBACK_PATH: &str = "/oauth/callback";

#[derive(Debug, Clone)]
struct OAuthRouteBinding {
    binding: OAuthBinding,
    library_root: PathBuf,
    config: Value,
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    resource: String,
    authorization_servers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClientRegistrationResponse {
    client_id: String,
}

#[derive(Debug, Deserialize)]
struct TokenEndpointResponse {
    access_token: String,
    refresh_token: Option<String>,
    token_type: Option<String>,
    scope: Option<String>,
    expires_in: Option<u64>,
}

pub(super) async fn oauth_status(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((plugin, server)): AxumPath<(String, String)>,
) -> Result<HttpResponse, HttpResponse> {
    let owner = binding_owner(&private_owner(&state, &headers));
    secure_result(
        blocking(move || {
            let route = resolve_binding(&state, &plugin, &server, &owner)?;
            let store = anchor_library::FileOAuthTokenStore::new(route.library_root);
            let authorization = store.load(&route.binding).map_err(oauth_error)?;
            Ok(Json(project_authorization(&authorization)).into_response())
        })
        .await,
    )
}

pub(super) async fn authorize_oauth(
    State(state): State<ApiState>,
    headers: HeaderMap,
    path: AxumPath<(String, String)>,
    body: Bytes,
) -> Result<HttpResponse, HttpResponse> {
    secure_result(authorize_oauth_inner(state, headers, path, body).await)
}

async fn authorize_oauth_inner(
    state: ApiState,
    headers: HeaderMap,
    AxumPath((plugin, server)): AxumPath<(String, String)>,
    body: Bytes,
) -> Result<HttpResponse, HttpResponse> {
    if !body.is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "OAuth authorization does not accept token payloads",
        ));
    }
    let owner = binding_owner(&private_owner(&state, &headers));
    let route_state = state.clone();
    let route = blocking(move || resolve_binding(&route_state, &plugin, &server, &owner)).await?;
    let redirect_uri = std::env::var("ANCHOR_OAUTH_REDIRECT_URI").map_err(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ANCHOR_OAUTH_REDIRECT_URI must configure the registered OAuth callback URL",
        )
    })?;
    validate_callback_uri(&redirect_uri).map_err(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ANCHOR_OAUTH_REDIRECT_URI must target /oauth/callback without a query",
        )
    })?;
    let endpoints = discover_oauth_provider(&route.config).await?;
    let resource = route
        .config
        .get("oauth_resource")
        .and_then(Value::as_str)
        .or_else(|| route.config.get("url").and_then(Value::as_str))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "OAuth MCP server requires a URL"))?
        .to_owned();
    let client_id = match route
        .config
        .get("oauth_client_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        Some(client_id) => client_id.to_owned(),
        None => register_public_client(&endpoints, &redirect_uri).await?,
    };
    let scope = route
        .config
        .get("oauth_scope")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut metadata = OAuthAuthorizationMetadata::new(&endpoints.token_endpoint)
        .map_err(oauth_error)?
        .with_client_id(client_id.clone())
        .map_err(oauth_error)?;
    if let Some(scope) = scope.clone() {
        metadata = metadata.with_scope(scope).map_err(oauth_error)?;
    }
    metadata = metadata
        .with_resource(resource.clone())
        .map_err(oauth_error)?;
    let now = unix_time().map_err(oauth_error)?;
    let expires_at = now
        .checked_add(OAUTH_TRANSACTION_TTL_SECONDS)
        .ok_or_else(|| oauth_error(OAuthError::Storage))?;
    let transaction = OAuthAuthorizationTransaction::new(
        route.binding,
        &endpoints.authorization_endpoint,
        redirect_uri,
        client_id,
        scope,
        now,
        expires_at,
    )
    .and_then(|transaction| transaction.with_resource(resource))
    .map_err(oauth_error)?;
    let authorization_url = transaction.authorization_url().to_string();
    let store = anchor_library::FileOAuthTokenStore::new(route.library_root);
    blocking(move || {
        store
            .save_pending(transaction, metadata)
            .map_err(oauth_error)
    })
    .await?;
    let response = Json(json!({
        "authorization_url": authorization_url,
        "expires_at": expires_at,
    }))
    .into_response();
    Ok(response)
}

pub(super) async fn oauth_callback(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> HttpResponse {
    let values = match parse_callback_query(raw_query.as_deref()) {
        Ok(values) => values,
        Err((status, _)) => return callback_error_response(status),
    };
    let Some(state_value) = values.get("state") else {
        return callback_error_response(StatusCode::BAD_REQUEST);
    };
    let callback_result = if values.contains_key("error") {
        let store = anchor_library::FileOAuthTokenStore::new(library_root(&state.catalog_root));
        let callback_state = state_value.to_owned();
        match tokio::task::spawn_blocking(move || {
            store.consume_pending(
                &callback_state,
                OAuthAuthorizationResponse::Error {
                    state: &callback_state,
                },
                unix_time().map_err(|_| OAuthError::Storage)?,
            )
        })
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_)) | Err(_) => Err(StatusCode::BAD_REQUEST),
        }
    } else {
        let Some(code_value) = values.get("code") else {
            return callback_error_response(StatusCode::BAD_REQUEST);
        };
        let root = library_root(&state.catalog_root);
        let store = anchor_library::FileOAuthTokenStore::new(root.clone());
        let callback_state = state_value.to_owned();
        let callback_code = code_value.to_owned();
        let pending = match tokio::task::spawn_blocking(move || {
            store.consume_pending(
                &callback_state,
                OAuthAuthorizationResponse::Code {
                    state: &callback_state,
                    code: &callback_code,
                },
                unix_time().map_err(|_| OAuthError::Storage)?,
            )
        })
        .await
        {
            Ok(Ok(pending)) => pending,
            Ok(Err(_)) | Err(_) => return callback_error_response(StatusCode::BAD_REQUEST),
        };
        exchange_authorization_code(pending.0, pending.1, root)
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)
    };
    match callback_result {
        Ok(()) => secure_callback_response(callback_success_html().into_response()),
        Err(status) => callback_error_response(status),
    }
}

pub(super) async fn revoke_oauth(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((plugin, server)): AxumPath<(String, String)>,
) -> Result<HttpResponse, HttpResponse> {
    let owner = binding_owner(&private_owner(&state, &headers));
    secure_result(
        blocking(move || {
            let route = resolve_binding(&state, &plugin, &server, &owner)?;
            let store = anchor_library::FileOAuthTokenStore::new(route.library_root);
            store.revoke(&route.binding).map_err(oauth_error)?;
            Ok(Json(json!({"authorized": false, "revoked": true})).into_response())
        })
        .await,
    )
}

fn resolve_binding(
    state: &ApiState,
    plugin: &str,
    server: &str,
    owner: &str,
) -> Result<OAuthRouteBinding, HttpResponse> {
    let library_root = library_root(&state.catalog_root);
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
    let binding = OAuthBinding::new(provider, plugin, server, owner)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid OAuth binding"))?;
    Ok(OAuthRouteBinding {
        binding,
        library_root,
        config: config.clone(),
    })
}

pub(crate) fn binding_owner(owner: &str) -> String {
    if owner == "local" {
        owner.to_owned()
    } else {
        format!("{:x}", Sha256::digest(owner.as_bytes()))
    }
}

async fn discover_oauth_provider(
    config: &Value,
) -> Result<AuthorizationServerMetadata, HttpResponse> {
    let resource = config
        .get("oauth_resource")
        .and_then(Value::as_str)
        .or_else(|| config.get("url").and_then(Value::as_str))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "OAuth MCP server requires a URL"))?;
    let resource_metadata_url = protected_resource_metadata_url(resource)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid OAuth resource URL"))?;
    let resource_metadata: ProtectedResourceMetadata =
        get_json(&resource_metadata_url).await.map_err(|_| {
            error(
                StatusCode::BAD_GATEWAY,
                "OAuth resource metadata is unavailable",
            )
        })?;
    let issuer = resource_metadata
        .authorization_servers
        .first()
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "OAuth authorization server is missing",
            )
        })?;
    let metadata_url = authorization_server_metadata_url(issuer).map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "OAuth authorization server URL is invalid",
        )
    })?;
    let metadata: AuthorizationServerMetadata = get_json(&metadata_url).await.map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "OAuth server metadata is unavailable",
        )
    })?;
    let expected_resource = Url::parse(resource)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid OAuth resource URL"))?;
    let discovered_resource = Url::parse(&resource_metadata.resource).map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "OAuth resource metadata is invalid",
        )
    })?;
    let expected_issuer = Url::parse(issuer).map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "OAuth authorization server URL is invalid",
        )
    })?;
    let discovered_issuer = Url::parse(&metadata.issuer)
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "OAuth server metadata is invalid"))?;
    if expected_resource != discovered_resource || expected_issuer != discovered_issuer {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "OAuth provider metadata identity does not match its discovery URL",
        ));
    }
    validate_oauth_endpoint(&metadata.authorization_endpoint)
        .and_then(|_| validate_oauth_endpoint(&metadata.token_endpoint))
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "OAuth server metadata is invalid"))?;
    if let Some(registration_endpoint) = &metadata.registration_endpoint {
        validate_oauth_endpoint(registration_endpoint)
            .map_err(|_| error(StatusCode::BAD_GATEWAY, "OAuth registration URL is invalid"))?;
    }
    Ok(metadata)
}

async fn register_public_client(
    metadata: &AuthorizationServerMetadata,
    redirect_uri: &str,
) -> Result<String, HttpResponse> {
    let registration_endpoint = metadata.registration_endpoint.as_deref().ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "OAuth provider requires oauth_client_id or supports no dynamic registration",
        )
    })?;
    validate_callback_uri(redirect_uri)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "OAuth callback URL is invalid"))?;
    let endpoint = Url::parse(registration_endpoint)
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "OAuth registration URL is invalid"))?;
    let client = crate::oauth_http::async_client(&endpoint)
        .await
        .map_err(|_| {
            error(
                StatusCode::BAD_GATEWAY,
                "OAuth registration URL is unavailable",
            )
        })?;
    let response = client
        .post(endpoint)
        .json(&json!({
            "client_name": "Anchor",
            "application_type": "web",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "OAuth client registration failed"))?;
    if !response.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "OAuth client registration failed",
        ));
    }
    let response = response
        .error_for_status()
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "OAuth client registration failed"))?;
    let registration: ClientRegistrationResponse = read_json(response)
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "OAuth client registration failed"))?;
    if registration.client_id.is_empty() || registration.client_id.len() > 256 {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "OAuth client registration response is invalid",
        ));
    }
    Ok(registration.client_id)
}

async fn exchange_authorization_code(
    code: anchor_library::OAuthAuthorizationCode,
    metadata: OAuthAuthorizationMetadata,
    library_root: PathBuf,
) -> Result<(), OAuthError> {
    let endpoint = metadata.token_endpoint().to_owned();
    let client_id = code.client_id().to_owned();
    let redirect_uri = code.redirect_uri().to_owned();
    let resource = metadata.resource().map(str::to_owned);
    let code_value = code.code().expose_secret().to_owned();
    let verifier = code.code_verifier().expose_secret().to_owned();
    let endpoint = Url::parse(&endpoint).map_err(|_| OAuthError::InvalidResponse)?;
    let client = crate::oauth_http::async_client(&endpoint)
        .await
        .map_err(|_| OAuthError::InvalidResponse)?;
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code_value.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier.as_str()),
    ];
    if let Some(resource) = resource.as_deref() {
        form.push(("resource", resource));
    }
    let response = client
        .post(endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|_| OAuthError::InvalidResponse)?;
    if !response.status().is_success() {
        return Err(OAuthError::InvalidResponse);
    }
    let response = response
        .error_for_status()
        .map_err(|_| OAuthError::InvalidResponse)?;
    let token: TokenEndpointResponse = read_json(response)
        .await
        .map_err(|_| OAuthError::InvalidResponse)?;
    if token
        .token_type
        .as_deref()
        .is_some_and(|token_type| !token_type.eq_ignore_ascii_case("bearer"))
    {
        return Err(OAuthError::InvalidResponse);
    }
    let mut response =
        OAuthTokenResponse::new(token.access_token, token.refresh_token, token.expires_in)?;
    if let Some(token_type) = token.token_type {
        response = response.with_token_type(token_type)?;
    }
    if let Some(scope) = token.scope {
        response = response.with_scope(scope)?;
    }
    let binding = code.binding().clone();
    let store = anchor_library::FileOAuthTokenStore::new(library_root);
    tokio::task::spawn_blocking(move || {
        anchor_library::OAuthClient::new(store, NoRefreshTransport).authorize(
            binding,
            metadata,
            response,
            unix_time()?,
        )
    })
    .await
    .map_err(|_| OAuthError::Storage)?
}

struct NoRefreshTransport;

impl anchor_library::OAuthRefreshTransport for NoRefreshTransport {
    fn refresh(
        &self,
        _request: &anchor_library::OAuthRefreshRequest<'_>,
    ) -> Result<OAuthTokenResponse, anchor_library::OAuthTransportError> {
        Err(anchor_library::OAuthTransportError::RequestFailed)
    }
}

async fn get_json<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, ()> {
    let parsed = Url::parse(url).map_err(|_| ())?;
    let client = crate::oauth_http::async_client(&parsed).await?;
    let response = client.get(parsed).send().await.map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    let response = response.error_for_status().map_err(|_| ())?;
    read_json(response).await
}

async fn read_json<T: for<'de> Deserialize<'de>>(mut response: reqwest::Response) -> Result<T, ()> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_OAUTH_RESPONSE_BYTES as u64)
    {
        return Err(());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if body.len().saturating_add(chunk.len()) > MAX_OAUTH_RESPONSE_BYTES {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| ())
}

fn protected_resource_metadata_url(resource: &str) -> Result<String, ()> {
    let url = Url::parse(resource).map_err(|_| ())?;
    crate::oauth_http::validate_oauth_url(&url)?;
    if url.query().is_some() {
        return Err(());
    }
    let path = url.path().trim_start_matches('/');
    let mut result = format!(
        "{}/.well-known/oauth-protected-resource",
        url.origin().ascii_serialization()
    );
    if !path.is_empty() {
        result.push('/');
        result.push_str(path);
    }
    Ok(result)
}

fn authorization_server_metadata_url(issuer: &str) -> Result<String, ()> {
    let url = Url::parse(issuer).map_err(|_| ())?;
    crate::oauth_http::validate_oauth_url(&url)?;
    if url.query().is_some() {
        return Err(());
    }
    let mut result = format!(
        "{}/.well-known/oauth-authorization-server",
        url.origin().ascii_serialization()
    );
    let path = url.path().trim_start_matches('/');
    if !path.is_empty() {
        result.push('/');
        result.push_str(path);
    }
    Ok(result)
}

fn validate_oauth_endpoint(value: &str) -> Result<(), ()> {
    let url = Url::parse(value).map_err(|_| ())?;
    crate::oauth_http::validate_oauth_url(&url)
}

fn validate_callback_uri(value: &str) -> Result<(), ()> {
    let url = Url::parse(value).map_err(|_| ())?;
    crate::oauth_http::validate_oauth_url(&url)?;
    if (url.path() != OAUTH_CALLBACK_PATH && !url.path().ends_with(OAUTH_CALLBACK_PATH))
        || url.query().is_some()
    {
        return Err(());
    }
    Ok(())
}

fn parse_callback_query(
    raw: Option<&str>,
) -> Result<HashMap<String, String>, (StatusCode, Html<&'static str>)> {
    let raw = raw
        .filter(|value| value.len() <= 8192)
        .ok_or((StatusCode::BAD_REQUEST, callback_error_html()))?;
    let mut values = HashMap::new();
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        if values
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err((StatusCode::BAD_REQUEST, callback_error_html()));
        }
    }
    if values.contains_key("code") == values.contains_key("error")
        || values.get("state").is_none_or(String::is_empty)
    {
        return Err((StatusCode::BAD_REQUEST, callback_error_html()));
    }
    Ok(values)
}

fn callback_error_html() -> Html<&'static str> {
    Html(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\"></head><body><p>OAuth authorization failed or expired. Return to Anchor and try again.</p></body></html>",
    )
}

fn callback_success_html() -> Html<&'static str> {
    Html(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\"></head><body><p>OAuth authorization is complete. Return to the Anchor tab.</p></body></html>",
    )
}

fn callback_error_response(status: StatusCode) -> HttpResponse {
    secure_callback_response((status, callback_error_html()).into_response())
}

fn secure_result(result: Result<HttpResponse, HttpResponse>) -> Result<HttpResponse, HttpResponse> {
    result
        .map(secure_oauth_response)
        .map_err(secure_oauth_response)
}

fn secure_callback_response(response: HttpResponse) -> HttpResponse {
    secure_oauth_response(response)
}

pub(super) fn secure_oauth_response(mut response: HttpResponse) -> HttpResponse {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("referrer-policy"),
        axum::http::HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("x-content-type-options"),
        axum::http::HeaderValue::from_static("nosniff"),
    );
    response
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_metadata_urls_preserve_provider_path_trailing_slashes() {
        assert_eq!(
            protected_resource_metadata_url("https://resource.example/mcp/").unwrap(),
            "https://resource.example/.well-known/oauth-protected-resource/mcp/"
        );
        assert_eq!(
            authorization_server_metadata_url("https://identity.example/tenant/").unwrap(),
            "https://identity.example/.well-known/oauth-authorization-server/tenant/"
        );
    }
}
