//! HTTP management of one Graph's bot. It uses the same owner-scoped
//! application operations as the Pilot; credential values go only to the
//! credential store.
use super::*;
use crate::application::channel_management::{ChannelBinding, ChannelMembers};

pub(super) fn router() -> Router<ApiState> {
    Router::new()
        .route("/channels", get(list))
        .route("/graphs/{graph}/channel", get(status).put(configure))
        .route("/graphs/{graph}/channel/enabled", post(set_enabled))
        .route("/graphs/{graph}/channel/credentials", get(credentials))
        .route(
            "/graphs/{graph}/channel/credentials/{field}",
            axum::routing::put(set_credential).delete(clear_credential),
        )
        .layer(middleware::from_fn(private_responses))
}

pub(super) fn is_private_path(path: &str) -> bool {
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    parts.first() == Some(&"graphs")
        && parts.get(2) == Some(&"channel")
        && parts.get(3) == Some(&"credentials")
}

async fn private_responses(request: axum::extract::Request, next: Next) -> HttpResponse {
    let private = is_private_path(request.uri().path());
    let response = next.run(request).await;
    if private {
        credential_requests::secure_private_response(response)
    } else {
        response
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EnabledRequest {
    enabled: bool,
    expected_binding: ChannelBinding,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SetCredential {
    authorization_digest: String,
    value: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ClearCredential {
    authorization_digest: String,
}

fn channel_error(failure: ApplicationError) -> HttpResponse {
    match failure {
        ApplicationError::Missing => error(
            StatusCode::NOT_FOUND,
            "bot binding, Graph, or Plugin is unavailable",
        ),
        failure => application_error(failure),
    }
}

async fn list(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .channel_list(&private_owner(&state, &headers))
        .await
        .map(Json)
        .map_err(channel_error)
}

async fn status(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(graph): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .channel_status(&private_owner(&state, &headers), &graph)
        .await
        .map(Json)
        .map_err(channel_error)
}

async fn configure(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(graph): AxumPath<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, HttpResponse> {
    let members = serde_json::from_slice::<ChannelMembers>(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid bot member request"))?;
    state
        .application
        .configure_channel(&private_owner(&state, &headers), &graph, members)
        .await
        .map(Json)
        .map_err(channel_error)
}

async fn set_enabled(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(graph): AxumPath<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, HttpResponse> {
    let request = serde_json::from_slice::<EnabledRequest>(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid bot enabled request"))?;
    state
        .application
        .set_channel_enabled_checked(
            &private_owner(&state, &headers),
            &graph,
            request.enabled,
            &request.expected_binding,
        )
        .await
        .map(Json)
        .map_err(channel_error)
}

#[allow(clippy::result_large_err)]
async fn credentials(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(graph): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    let plan = state
        .application
        .channel_bot_plan(&graph)
        .map_err(channel_error)?;
    blocking(move || {
        crate::credential_store::channel_credentials(
            &state.data_root,
            state.application.library_root(),
            &private_owner(&state, &headers),
            &plan.plugin,
            &crate::credential_store::channel_target(plan.platform.as_str(), &graph),
        )
        .map(Json)
        .map_err(|reason| error(StatusCode::CONFLICT, reason))
    })
    .await
}

async fn set_credential(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((graph, field)): AxumPath<(String, String)>,
    Json(body): Json<SetCredential>,
) -> Result<Json<Value>, HttpResponse> {
    mutate(
        state,
        headers,
        graph,
        field,
        body.authorization_digest,
        Some(body.value),
    )
    .await
}

async fn clear_credential(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((graph, field)): AxumPath<(String, String)>,
    Json(body): Json<ClearCredential>,
) -> Result<Json<Value>, HttpResponse> {
    mutate(
        state,
        headers,
        graph,
        field,
        body.authorization_digest,
        None,
    )
    .await
}

#[allow(clippy::result_large_err)]
async fn mutate(
    state: ApiState,
    headers: HeaderMap,
    graph: String,
    field: String,
    digest: String,
    value: Option<String>,
) -> Result<Json<Value>, HttpResponse> {
    let _catalog = state.application.graph_catalog_mutation_guard().await;
    let _reference = state
        .application
        .plugin_reference_lease()
        .await
        .map_err(application_error)?;
    let owner = private_owner(&state, &headers);
    let plan = state
        .application
        .check_channel_credentials_mutable(&owner, &graph)
        .map_err(channel_error)?;
    blocking(move || {
        crate::credential_store::configure(
            &state.data_root,
            state.application.library_root(),
            &owner,
            &plan.plugin,
            &crate::credential_store::channel_target(plan.platform.as_str(), &graph),
            &field,
            &digest,
            value.as_deref(),
            &store(&state)?,
        )
        .map(Json)
        .map_err(|reason| error(StatusCode::CONFLICT, reason))
    })
    .await
}

#[cfg(test)]
mod tests;
