//! Operator adapters for explicit schedule sharing and Session subscriptions.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct ScheduleSharing {
    description: String,
    sessions: Vec<String>,
    enabled: bool,
}

pub(in crate::api) async fn share_schedule(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
    Json(body): Json<ScheduleSharing>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .share_schedule(
            &private_owner(&state, &headers),
            &identifier,
            &body.description,
            &body.sessions,
            body.enabled,
        )
        .await
        .map(Json)
        .map_err(|failure| management_error(failure, "no such schedule or channel Session"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct ScheduleSubscription {
    session: String,
    instruction: String,
    not_before: Option<String>,
}

pub(in crate::api) async fn subscribe_schedule(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
    Json(body): Json<ScheduleSubscription>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .operator_subscribe_schedule(
            &private_owner(&state, &headers),
            &identifier,
            &body.session,
            &body.instruction,
            body.not_before.as_deref(),
        )
        .await
        .map(Json)
        .map_err(|failure| management_error(failure, "no such shared schedule or channel Session"))
}

pub(in crate::api) async fn list_schedule_subscriptions(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .operator_list_subscriptions(&private_owner(&state, &headers))
        .map(Json)
        .map_err(|failure| management_error(failure, "no such schedule subscription"))
}

pub(in crate::api) async fn unsubscribe_schedule(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .operator_unsubscribe_schedule(&private_owner(&state, &headers), &identifier)
        .await
        .map(Json)
        .map_err(|failure| management_error(failure, "no such schedule subscription"))
}
