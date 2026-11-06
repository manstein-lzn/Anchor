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

pub(super) async fn get_channel_assistant(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(session): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    let recovery_blocked = state
        .application
        .ensure_channel_session_eligible(&owner, &session)
        .err()
        .map(|error| format!("{error:?}"));
    let lookup_state = state.clone();
    let (assistant, run_id, plugin_update) = blocking(move || {
        let assistant = store(&lookup_state)?
            .get_channel_assistant(&owner, &session)
            .map_err(channel_error)?;
        let run_id = assistant.as_ref().map(|binding| binding.run_id.clone());
        let plugin_update = run_id.as_deref().map(|run| {
            lookup_state
                .application
                .assistant_plugin_update(run)
                .unwrap_or_else(
                    |failure| json!({"state":"unavailable","message":failure.to_string()}),
                )
        });
        Ok((json!(assistant), run_id, plugin_update))
    })
    .await?;
    // Needs recovery is derived, never stored: a live instance is bound to a Run
    // this process is running. A Run record that is missing (a handover whose
    // admission never ran) or terminal is not active either, so both read as
    // "needs recovery" — which is exactly the window a crash leaves behind.
    let needs_recovery = match run_id {
        Some(run) => {
            recovery_blocked.is_none() && !state.application.active_runs(None).await.contains(&run)
        }
        None => false,
    };
    Ok(Json(
        json!({"assistant": assistant, "needs_recovery": needs_recovery, "recovery_blocked": recovery_blocked, "plugin_update":plugin_update}),
    ))
}

pub(super) async fn retire_channel_assistant(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(session): AxumPath<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, HttpResponse> {
    let binding: RunBinding = decode(body, &["run_id"])?;
    let owner = private_owner(&state, &headers);
    state
        .application
        .retire_assistant(&owner, &session, &binding.run_id)
        .await
        .map_err(application_error)?;
    Ok(Json(json!({"retired": binding.run_id, "session":session})))
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
    state
        .application
        .delete_task_session(&owner, &session)
        .await
        .map_err(|failure| {
            channel_error(match failure {
                ApplicationError::Missing => SessionError::Missing,
                ApplicationError::Conflict(reason) => SessionError::Conflict(reason),
                ApplicationError::Invalid(reason) => SessionError::Invalid(reason),
                ApplicationError::Storage(reason) => SessionError::Storage(reason),
            })
        })?;
    Ok(Json(json!({"deleted": session})))
}

pub(super) async fn get_channel_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(session): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let sessions = store(&state)?;
        // Validate the channel boundary as well as owner. The generic Session
        // endpoint has Pilot's legacy owner selection and is not this adapter.
        sessions
            .get_channel_assistant(&owner, &session)
            .map_err(channel_error)?;
        let session = sessions.get(&owner, &session).map_err(channel_error)?;
        Ok(Json(json!({"session": session})))
    })
    .await
}

#[cfg(test)]
mod deletion_tests {
    use super::*;
    use crate::application::scheduling::{
        ScheduleManagementPort, TaskScheduleRequest, TaskScheduleUpdate,
    };
    use crate::task_delegation::TaskOrigin;
    use anchor_platform_session::{ChannelIdentity, TurnStatus};
    use anchor_runtime::graph::{GraphTaskPolicy, InvocationKey};
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn session_delete_endpoint_preserves_paused_task_plans_until_cancelled() {
        let (_root, state) = crate::api::tests::fixture();
        let sessions = store(&state).unwrap();
        let admitted = sessions
            .admit_channel_inbound(
                "local",
                ChannelInboundRequest {
                    inbound_id: "initial".into(),
                    identity: ChannelIdentity {
                        source: "wecom".into(),
                        account: Some("bot".into()),
                        conversation_kind: Default::default(),
                        conversation_id: "alice".into(),
                        sender_id: "alice".into(),
                    },
                    graph: "fixture".into(),
                    reply_node: "work".into(),
                    text: Some("later".into()),
                    attachments: Default::default(),
                    run_id: None,
                    replace_running: false,
                },
            )
            .unwrap();
        let session = admitted.session.id;
        sessions
            .finish_turn(
                "local",
                &session,
                &admitted.turn.id,
                TurnStatus::Completed,
                None,
            )
            .unwrap();
        let origin = TaskOrigin {
            owner: "local".into(),
            session: Some(session.clone()),
            graph: "fixture".into(),
            source: crate::task_delegation::TaskOriginSource::AgentDelegation {
                key: InvocationKey {
                    run_id: "retired-source".into(),
                    graph_digest: "a".repeat(64),
                    node_id: "work".into(),
                    invocation: 1,
                },
            },
            authorization_digest: "a".repeat(64),
            policy: GraphTaskPolicy::default(),
            bindings: Default::default(),
            targets: Default::default(),
        };
        let adapter =
            crate::api::schedules::ScheduleManagementAdapter::new(state.schedules.clone());
        let request = TaskScheduleRequest {
            graph: "fixture".into(),
            rules: vec![json!({"type":"daily","time":"09:00"})],
            input: json!({}),
            remaining: Some(2),
            request_id: "retained-plan".into(),
        };
        let created = adapter.create_tasks(&origin, &request).unwrap();
        let id = created["schedules"][0]["id"].as_str().unwrap();
        adapter
            .update_task(
                &origin,
                id,
                &TaskScheduleUpdate {
                    graph: request.graph.clone(),
                    rule: request.rules[0].clone(),
                    input: request.input.clone(),
                    enabled: false,
                    remaining: request.remaining,
                },
            )
            .unwrap();
        adapter
            .check_task_origin_deletion(&|candidate| Ok(candidate.owner == "another-owner"))
            .unwrap();
        adapter
            .check_task_origin_deletion(&|candidate| {
                Ok(candidate.session.as_deref() == Some("another-session"))
            })
            .unwrap();
        assert!(matches!(
            adapter.check_task_origin_deletion(&|_| Err(ApplicationError::Storage(
                "unreadable identity".into()
            ))),
            Err(ApplicationError::Storage(_))
        ));
        let app = crate::api::router(state.clone());
        let route = format!("/channel-sessions/{session}");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(&route)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(sessions.get("local", &session).is_ok());
        adapter.delete_task(&origin, id).unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(&route)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(sessions.get("local", &session), Err(SessionError::Missing));
    }
}
