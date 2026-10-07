use super::*;
use anchor_platform_session::{
    ChannelDeliveryRequest, ChannelDeliveryStatus, ChannelInboundRequest, SessionError,
};
use serde::de::DeserializeOwned;

fn decode<T: DeserializeOwned>(
    body: axum::body::Bytes,
    allowed: &[&str],
) -> Result<T, HttpResponse> {
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid channel Session request"))?;
    let fields = value.as_object().ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "channel Session request must be a JSON object",
        )
    })?;
    if fields.keys().any(|name| !allowed.contains(&name.as_str())) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "channel Session request contains unknown fields",
        ));
    }
    serde_json::from_value(value)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid channel Session request"))
}

fn channel_error(failure: SessionError) -> HttpResponse {
    match failure {
        SessionError::Conflict(message) if message == "operation requires a channel Turn" => {
            error(StatusCode::NOT_FOUND, "no such channel Turn")
        }
        SessionError::Conflict(message) if message == "operation requires a channel Session" => {
            error(StatusCode::NOT_FOUND, "no such channel Session")
        }
        failure => session_error(failure),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RunBinding {
    run_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryBegin {
    kind: String,
    content_sha256: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliverySettlement {
    kind: String,
    content_sha256: String,
    status: ChannelDeliveryStatus,
    #[serde(default)]
    error: Option<String>,
}

pub(super) async fn admit_channel_inbound(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let request: ChannelInboundRequest = decode(
        body,
        &[
            "inbound_id",
            "identity",
            "graph",
            "reply_node",
            "text",
            "attachments",
            "run_id",
            "replace_running",
        ],
    )?;
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let admission = store(&state)?
            .admit_channel_inbound(&owner, request)
            .map_err(channel_error)?;
        Ok((
            StatusCode::ACCEPTED,
            Json(json!({
                "session": admission.session,
                "turn": admission.turn,
                "relation": admission.relation,
            })),
        ))
    })
    .await
}

pub(super) async fn get_channel_relation(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((session, inbound)): AxumPath<(String, String)>,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let relation = store(&state)?
            .get_channel_relation(&owner, &session, &inbound)
            .map_err(channel_error)?;
        Ok(Json(json!({"relation": relation})))
    })
    .await
}

pub(super) async fn associate_channel_run(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((session, inbound)): AxumPath<(String, String)>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, HttpResponse> {
    let binding: RunBinding = decode(body, &["run_id"])?;
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let relation = store(&state)?
            .associate_channel_run(&owner, &session, &inbound, &binding.run_id)
            .map_err(channel_error)?;
        Ok(Json(json!({"relation": relation})))
    })
    .await
}

pub(super) async fn admit_channel_delivery(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((session, turn)): AxumPath<(String, String)>,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let request: ChannelDeliveryRequest = decode(body, &["key", "kind", "content_sha256"])?;
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let delivery = store(&state)?
            .admit_channel_delivery(&owner, &session, &turn, request)
            .map_err(channel_error)?;
        Ok((StatusCode::CREATED, Json(json!({"delivery": delivery}))))
    })
    .await
}

pub(super) async fn begin_channel_delivery(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((session, turn, key)): AxumPath<(String, String, String)>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, HttpResponse> {
    let request: DeliveryBegin = decode(body, &["kind", "content_sha256"])?;
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let delivery = store(&state)?
            .begin_channel_delivery(
                &owner,
                &session,
                &turn,
                ChannelDeliveryRequest {
                    key,
                    kind: request.kind,
                    content_sha256: request.content_sha256,
                },
            )
            .map_err(channel_error)?;
        Ok(Json(json!({"delivery": delivery})))
    })
    .await
}

pub(super) async fn settle_channel_delivery(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((session, key)): AxumPath<(String, String)>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, HttpResponse> {
    let request: DeliverySettlement = decode(body, &["kind", "content_sha256", "status", "error"])?;
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let delivery = store(&state)?
            .settle_channel_delivery(
                &owner,
                &session,
                ChannelDeliveryRequest {
                    key,
                    kind: request.kind,
                    content_sha256: request.content_sha256,
                },
                request.status,
                request.error.as_deref(),
            )
            .map_err(channel_error)?;
        Ok(Json(json!({"delivery": delivery})))
    })
    .await
}

pub(super) async fn list_channel_deliveries(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(session): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let deliveries = store(&state)?
            .list_unfinished_channel_deliveries(&owner, &session)
            .map_err(channel_error)?;
        Ok(Json(json!({"deliveries": deliveries})))
    })
    .await
}

pub(super) async fn delete_channel_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(session): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    blocking(move || {
        store(&state)?
            .delete_channel_session(&owner, &session)
            .map_err(channel_error)?;
        Ok(Json(json!({"deleted": session})))
    })
    .await
}
