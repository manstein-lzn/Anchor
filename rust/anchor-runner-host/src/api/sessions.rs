use super::*;
use anchor_platform_session::{CreateSession, Session, SessionError, SessionStatus, SessionStore};
use sha2::{Digest, Sha256};
use std::path::Component;

pub(super) fn store(state: &ApiState) -> Result<SessionStore, HttpResponse> {
    use rustix::fs::{Mode, OFlags, fsync, mkdirat, open, openat};
    let parent = std::path::absolute(state.data_root.join("platform"))
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "Session storage failed"))?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = open("/", flags, Mode::empty())
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "Session storage failed"))?;
    for component in parent.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                match mkdirat(&directory, name, Mode::RWXU) {
                    Ok(()) => fsync(&directory).map_err(|_| {
                        error(StatusCode::INTERNAL_SERVER_ERROR, "Session storage failed")
                    })?,
                    Err(failure) if failure == rustix::io::Errno::EXIST => {}
                    Err(_) => {
                        return Err(error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Session storage failed",
                        ));
                    }
                }
                directory = openat(&directory, name, flags, Mode::empty()).map_err(|_| {
                    error(StatusCode::INTERNAL_SERVER_ERROR, "Session storage failed")
                })?;
            }
            _ => {
                return Err(error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Session storage failed",
                ));
            }
        }
    }
    SessionStore::open(state.data_root.join("platform/sessions.sqlite"))
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "Session storage failed"))
}

pub(super) async fn blocking<Value: Send + 'static>(
    action: impl FnOnce() -> Result<Value, HttpResponse> + Send + 'static,
) -> Result<Value, HttpResponse> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "Session storage failed"))?
}

pub(super) fn session_error(failure: SessionError) -> HttpResponse {
    match failure {
        SessionError::Missing => error(StatusCode::NOT_FOUND, "no such session"),
        SessionError::Invalid(message) => error(StatusCode::BAD_REQUEST, message),
        SessionError::Conflict(message) => error(StatusCode::CONFLICT, message),
        SessionError::Storage(_) => {
            error(StatusCode::INTERNAL_SERVER_ERROR, "Session storage failed")
        }
    }
}

pub(super) fn private_owner(state: &ApiState, headers: &HeaderMap) -> String {
    if state.api_keys.is_empty() && state.loopback {
        return "local".to_owned();
    }
    let key = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    format!("api:{:x}", Sha256::digest(key.as_bytes()))
}

fn owner_for_id(state: &ApiState, headers: &HeaderMap, identifier: &str) -> String {
    if identifier.starts_with("responses-") {
        private_owner(state, headers)
    } else {
        "local".to_owned()
    }
}

pub(super) fn owned_session(
    state: &ApiState,
    headers: &HeaderMap,
    identifier: &str,
) -> Result<(SessionStore, String, Session), HttpResponse> {
    let sessions = store(state)?;
    let owner = owner_for_id(state, headers, identifier);
    let session = sessions.get(&owner, identifier).map_err(session_error)?;
    Ok((sessions, owner, session))
}

fn has_retained_session_run(state: &ApiState, identifier: &str) -> Result<bool, HttpResponse> {
    for (run, _) in state.application.records().map_err(application_error)? {
        let Some(metadata) = state
            .application
            .metadata(&run)
            .map_err(application_error)?
        else {
            continue;
        };
        if metadata
            .conversation
            .as_ref()
            .is_some_and(|source| source.session == identifier)
            || metadata
                .session_call
                .as_ref()
                .is_some_and(|call| call.context.session == identifier)
            || metadata
                .pilot
                .as_ref()
                .is_some_and(|source| source.session == identifier)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) async fn list_sessions(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let sessions = store(&state)?;
        let mut visible = sessions.list("local").map_err(session_error)?;
        let owner = private_owner(&state, &headers);
        if owner != "local" {
            visible.retain(|session| !session.id.starts_with("responses-"));
            visible.extend(sessions.list(&owner).map_err(session_error)?);
        }
        visible.retain(|session| session.graph.is_empty() && session.channel.is_empty());
        visible.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then(left.id.cmp(&right.id))
        });
        Ok(Json(json!({"sessions":visible})))
    })
    .await
}

pub(super) async fn create_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let body = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice::<Value>(&body)
            .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid Session creation request"))?
    };
    blocking(move || {
        let fields = body
            .as_object()
            .filter(|fields| {
                fields
                    .keys()
                    .all(|name| matches!(name.as_str(), "id" | "title"))
            })
            .ok_or_else(|| {
                error(
                    StatusCode::BAD_REQUEST,
                    "provide optional id and title only",
                )
            })?;
        let request: CreateSession = serde_json::from_value(Value::Object(fields.clone()))
            .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid Session creation request"))?;
        if let Some(identifier) = &request.id
            && has_retained_session_run(&state, identifier)?
        {
            return Err(error(
                StatusCode::CONFLICT,
                "Session identity is retained by runtime history",
            ));
        }
        let owner = request
            .id
            .as_deref()
            .map(|identifier| owner_for_id(&state, &headers, identifier))
            .unwrap_or_else(|| "local".to_owned());
        let session = store(&state)?
            .create(&owner, request)
            .map_err(session_error)?;
        Ok((StatusCode::CREATED, Json(json!({"session":session}))))
    })
    .await
}

pub(super) async fn get_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let (_, _, session) = owned_session(&state, &headers, &identifier)?;
        Ok(Json(json!({"session":session})))
    })
    .await
}

pub(super) async fn rename_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let fields = body
            .as_object()
            .filter(|fields| fields.len() == 1)
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "provide title only"))?;
        let title = fields
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "title must be a string"))?;
        let (sessions, owner, _) = owned_session(&state, &headers, &identifier)?;
        let session = sessions
            .rename(&owner, &identifier, title)
            .map_err(session_error)?;
        Ok(Json(json!({"session":session})))
    })
    .await
}

pub(super) async fn set_session_status(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let fields = body
            .as_object()
            .filter(|fields| {
                fields
                    .keys()
                    .all(|name| matches!(name.as_str(), "status" | "reason"))
            })
            .ok_or_else(|| {
                error(
                    StatusCode::BAD_REQUEST,
                    "provide status and optional reason only",
                )
            })?;
        let status: SessionStatus =
            serde_json::from_value(fields.get("status").cloned().unwrap_or(Value::Null))
                .map_err(|_| error(StatusCode::BAD_REQUEST, "unknown session status"))?;
        let reason = match fields.get("reason") {
            None => "",
            Some(Value::String(value)) => value.as_str(),
            Some(_) => return Err(error(StatusCode::BAD_REQUEST, "reason must be a string")),
        };
        let (sessions, owner, _) = owned_session(&state, &headers, &identifier)?;
        let session = sessions
            .set_status(&owner, &identifier, status, reason)
            .map_err(session_error)?;
        Ok(Json(json!({"session":session})))
    })
    .await
}

#[derive(serde::Deserialize, Default)]
pub(super) struct SessionEventsQuery {
    #[serde(default)]
    after: u64,
}

pub(super) async fn session_events(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
    Query(query): Query<SessionEventsQuery>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let (sessions, owner, _) = owned_session(&state, &headers, &identifier)?;
        let events = sessions
            .events(&owner, &identifier, query.after)
            .map_err(session_error)?;
        Ok(Json(json!({"events":events})))
    })
    .await
}

pub(super) async fn delete_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let (sessions, owner, _) = owned_session(&state, &headers, &identifier)?;
        if has_retained_session_run(&state, &identifier)? {
            return Err(error(
                StatusCode::CONFLICT,
                "Session has retained runtime history",
            ));
        }
        sessions
            .delete(&owner, &identifier)
            .map_err(session_error)?;
        Ok(Json(json!({"deleted":identifier})))
    })
    .await
}

pub(super) async fn session_execution_unavailable(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        owned_session(&state, &headers, &identifier)?;
        Err(error(
            StatusCode::NOT_IMPLEMENTED,
            "native Session lifecycle is available; Pilot/Turn execution is not implemented yet",
        ))
    })
    .await
}
