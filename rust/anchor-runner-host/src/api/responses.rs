use super::*;
use anchor_platform_session::{CreateSession, SessionStore, Turn, TurnStatus};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde_json::Value;
use std::{
    convert::Infallible,
    time::{Duration, Instant},
};

const RESPONSE_MODEL: &str = "anchor-copilot";

#[derive(Clone)]
struct ResponseReference {
    owner: String,
    session: String,
    turn: String,
}

fn input_text(input: &Value) -> Result<String, HttpResponse> {
    let text = match input {
        Value::String(text) => text.clone(),
        Value::Array(messages) if !messages.is_empty() => {
            let mut output = Vec::with_capacity(messages.len());
            for message in messages {
                let Some(message) = message.as_object() else {
                    return Err(error(
                        StatusCode::BAD_REQUEST,
                        "input must be text or a list of user text messages",
                    ));
                };
                if message.get("role").and_then(Value::as_str) != Some("user")
                    || message
                        .keys()
                        .any(|field| !matches!(field.as_str(), "role" | "content"))
                {
                    return Err(error(
                        StatusCode::BAD_REQUEST,
                        "input must be text or a list of user text messages",
                    ));
                }
                let content = match message.get("content") {
                    Some(Value::String(text)) => text.clone(),
                    Some(Value::Array(parts)) => {
                        let mut texts = Vec::with_capacity(parts.len());
                        for part in parts {
                            if part.get("type").and_then(Value::as_str) != Some("input_text")
                                || part.as_object().is_none_or(|fields| {
                                    fields
                                        .keys()
                                        .any(|field| !matches!(field.as_str(), "type" | "text"))
                                })
                            {
                                return Err(error(
                                    StatusCode::BAD_REQUEST,
                                    "input must be text or a list of user text messages",
                                ));
                            }
                            let Some(text) = part.get("text").and_then(Value::as_str) else {
                                return Err(error(
                                    StatusCode::BAD_REQUEST,
                                    "input must be text or a list of user text messages",
                                ));
                            };
                            texts.push(text);
                        }
                        texts.join("\n")
                    }
                    _ => {
                        return Err(error(
                            StatusCode::BAD_REQUEST,
                            "input must be text or a list of user text messages",
                        ));
                    }
                };
                output.push(content);
            }
            output.join("\n\n")
        }
        _ => {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "input must be text or a list of user text messages",
            ));
        }
    };
    if text.trim().is_empty() || text.len() > 64 * 1024 {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "input must be nonblank and at most 64 KiB",
        ));
    }
    Ok(text)
}

fn reference_for(
    sessions: &SessionStore,
    owner: &str,
    response_id: &str,
) -> Result<ResponseReference, HttpResponse> {
    let turn_id = response_id
        .strip_prefix("resp_")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "no such response"))?;
    let sessions_list = sessions.list(owner).map_err(session_error)?;
    for session in sessions_list
        .into_iter()
        .filter(|session| session.id.starts_with("responses-"))
    {
        let turns = sessions
            .list_turns(owner, &session.id)
            .map_err(session_error)?;
        if turns.iter().any(|turn| turn.id == turn_id) {
            return Ok(ResponseReference {
                owner: owner.into(),
                session: session.id,
                turn: turn_id.into(),
            });
        }
    }
    Err(error(StatusCode::NOT_FOUND, "no such response"))
}

fn response_object(id: &str, status: &str, text: &str, failure: Option<&str>) -> Value {
    let mut response = json!({
        "id":id,
        "object":"response",
        "created_at":chrono::Utc::now().timestamp(),
        "status":status,
        "model":RESPONSE_MODEL,
        "output":[],
        "output_text":text,
    });
    if !text.is_empty() {
        response["output"] = json!([{
            "id":format!("msg_{}", id.strip_prefix("resp_").unwrap_or(id)),
            "type":"message",
            "role":"assistant",
            "status":status,
            "content":[{"type":"output_text","text":text,"annotations":[]}]
        }]);
    }
    if let Some(failure) = failure {
        response["error"] = json!({"message":failure,"type":"server_error"});
    }
    response
}

fn response_text(
    sessions: &SessionStore,
    reference: &ResponseReference,
) -> Result<String, HttpResponse> {
    let events = sessions
        .turn_events(&reference.owner, &reference.session, &reference.turn, 0)
        .map_err(session_error)?;
    Ok(events
        .iter()
        .filter(|event| event.data["type"] == "text-delta")
        .filter_map(|event| event.data["delta"].as_str())
        .collect())
}

async fn response_snapshot(
    sessions: SessionStore,
    reference: ResponseReference,
    id: String,
) -> Result<(Value, Turn), HttpResponse> {
    blocking(move || {
        let turn = sessions
            .get_turn(&reference.owner, &reference.session, &reference.turn)
            .map_err(session_error)?;
        let text = response_text(&sessions, &reference)?;
        let status = match turn.status {
            TurnStatus::Running => "in_progress",
            TurnStatus::Completed => "completed",
            TurnStatus::Failed | TurnStatus::Stopped | TurnStatus::Interrupted => "failed",
        };
        Ok((
            response_object(&id, status, &text, turn.error.as_deref()),
            turn,
        ))
    })
    .await
}

fn sse_value(kind: &str, payload: Value) -> Result<Event, Infallible> {
    Ok(Event::default().event(kind).data(payload.to_string()))
}

pub(super) async fn create_response(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Result<HttpResponse, HttpResponse> {
    let Json(body) =
        body.map_err(|_| error(StatusCode::BAD_REQUEST, "invalid Responses request"))?;
    let Some(fields) = body.as_object() else {
        return Err(error(StatusCode::BAD_REQUEST, "request must be an object"));
    };
    let unsupported = fields
        .keys()
        .filter(|key| {
            !matches!(
                key.as_str(),
                "input" | "model" | "stream" | "previous_response_id"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if !unsupported.is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            format!("unsupported fields: {}", unsupported.join(", ")),
        ));
    }
    if body
        .get("model")
        .is_some_and(|model| model.as_str() != Some(RESPONSE_MODEL))
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "model must be anchor-copilot",
        ));
    }
    let stream = match body.get("stream") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(error(StatusCode::BAD_REQUEST, "stream must be a boolean")),
    };
    let prompt = input_text(body.get("input").unwrap_or(&Value::Null))?;
    let owner = private_owner(&state, &headers);
    let previous = match body.get("previous_response_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "previous_response_id must be a string",
            ));
        }
    };
    let sessions = blocking({
        let state = state.clone();
        move || store(&state)
    })
    .await?;
    let previous_reference = match previous {
        Some(previous) => Some(reference_for(&sessions, &owner, &previous)?),
        None => None,
    };
    let session = if let Some(reference) = &previous_reference {
        blocking({
            let sessions = sessions.clone();
            let owner = owner.clone();
            let id = reference.session.clone();
            move || sessions.get(&owner, &id).map_err(session_error)
        })
        .await?
    } else {
        let sessions = sessions.clone();
        let owner = owner.clone();
        let id = format!("responses-{}", crate::application::metadata::now_nanos());
        blocking(move || {
            sessions
                .create(
                    &owner,
                    CreateSession {
                        id: Some(id),
                        title: "Responses".into(),
                        ..Default::default()
                    },
                )
                .map_err(session_error)
        })
        .await?
    };
    if !session.graph.is_empty() || !session.channel.is_empty() {
        return Err(error(StatusCode::NOT_FOUND, "no such response"));
    }

    #[cfg(test)]
    let fixture_reply = state.response_fixture.clone();
    #[cfg(not(test))]
    let fixture_reply: Option<String> = None;
    let turn = if let Some(reply_prefix) = fixture_reply {
        let sessions_for_turn = sessions.clone();
        let turn = blocking({
            let owner = owner.clone();
            let session_id = session.id.clone();
            let request_id = format!("fixture-{}", crate::application::metadata::now_nanos());
            let prompt = prompt.clone();
            move || {
                sessions_for_turn
                    .create_turn(&owner, &session_id, &request_id, Some(&prompt))
                    .map(|(turn, _)| turn)
                    .map_err(session_error)
            }
        })
        .await?;
        let fixture_reply = format!("{reply_prefix}: {prompt}");
        blocking({
            let sessions = sessions.clone();
            let owner = owner.clone();
            let session_id = session.id.clone();
            let turn_id = turn.id.clone();
            move || {
                sessions
                    .append_turn_event(
                        &owner,
                        &session_id,
                        &turn_id,
                        json!({"type":"text-delta","delta":fixture_reply}),
                    )
                    .map_err(session_error)?;
                sessions
                    .finish_turn(&owner, &session_id, &turn_id, TurnStatus::Completed, None)
                    .map_err(session_error)?;
                Ok(())
            }
        })
        .await?;
        turn
    } else {
        let tools = crate::pilot_tools::PilotTools {
            application: state.application.clone(),
            graph_name: state.graph_name.clone(),
            catalog_root: state.catalog_root.clone(),
            library_root: library_root(&state.catalog_root),
            data_root: state.data_root.clone(),
            workspace_root: state.workspace_root.clone(),
            sessions: sessions.clone(),
            owner: owner.clone(),
            session: session.id.clone(),
            turn: String::new(),
        };
        state
            .pilots
            .admit(crate::pilot_host::PilotAdmission {
                sessions: sessions.clone(),
                owner: owner.clone(),
                session: session.clone(),
                request_id: format!("responses-{}", crate::application::metadata::now_nanos()),
                prompt: Some(prompt),
                root: crate::pilot_host::scope(&state.data_root, &owner, &session),
                tools,
            })
            .await
            .map_err(|failure| match failure {
                crate::pilot_host::AdmissionError::Session(failure) => session_error(failure),
                crate::pilot_host::AdmissionError::Provider => error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Pilot provider is not configured or unavailable",
                ),
            })?
    };
    let response_id = format!("resp_{}", turn.id);
    let reference = ResponseReference {
        owner,
        session: session.id,
        turn: turn.id.clone(),
    };
    let (response, turn) =
        response_snapshot(sessions.clone(), reference.clone(), response_id.clone()).await?;
    if !stream {
        if turn.status == TurnStatus::Running {
            let deadline = Instant::now() + Duration::from_secs(24 * 60 * 60);
            loop {
                if Instant::now() >= deadline {
                    let (response, _) =
                        response_snapshot(sessions.clone(), reference.clone(), response_id.clone())
                            .await?;
                    return Ok((StatusCode::ACCEPTED, Json(response)).into_response());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                let (response, turn) =
                    response_snapshot(sessions.clone(), reference.clone(), response_id.clone())
                        .await?;
                if turn.status != TurnStatus::Running {
                    return Ok((StatusCode::OK, Json(response)).into_response());
                }
            }
        }
        return Ok((StatusCode::OK, Json(response)).into_response());
    }

    let store = sessions.clone();
    let stream_reference = reference.clone();
    let stream_id = response_id.clone();
    let stream = futures_util::stream::unfold(
        Some((store, stream_reference, stream_id, 0_u64, false)),
        |state| async move {
            let (sessions, reference, id, mut cursor, mut initialized) = state?;
            if !initialized {
                initialized = true;
                let created = response_object(&id, "in_progress", "", None);
                let first = sse_value(
                    "response.created",
                    json!({"type":"response.created","response":created}),
                );
                return Some((first, Some((sessions, reference, id, cursor, initialized))));
            }
            let progress = response_object(&id, "in_progress", "", None);
            if cursor == 0 {
                cursor = u64::MAX;
                return Some((
                    sse_value(
                        "response.in_progress",
                        json!({"type":"response.in_progress","response":progress}),
                    ),
                    Some((sessions, reference, id, cursor, initialized)),
                ));
            }
            let lookup = sessions.clone();
            let owner = reference.owner.clone();
            let session = reference.session.clone();
            let turn_id = reference.turn.clone();
            let loaded = tokio::task::spawn_blocking(move || {
                let turn = lookup.get_turn(&owner, &session, &turn_id)?;
                let events = lookup.turn_events(&owner, &session, &turn_id, 0)?;
                Ok::<_, anchor_platform_session::SessionError>((turn, events))
            })
            .await;
            let (turn, events) = match loaded {
                Ok(Ok(value)) => value,
                _ => {
                    return Some((
                        sse_value(
                            "response.failed",
                            json!({"type":"response.failed","response":response_object(&id,"failed","",Some("Pilot delivery history unavailable"))}),
                        ),
                        None,
                    ));
                }
            };
            let deltas = events
                .into_iter()
                .filter(|event| {
                    (cursor == u64::MAX || event.seq > cursor) && event.data["type"] == "text-delta"
                })
                .collect::<Vec<_>>();
            if let Some(event) = deltas.first() {
                let next_cursor = event.seq;
                let delta = json!({"type":"response.output_text.delta","item_id":format!("msg_{}", id.strip_prefix("resp_").unwrap_or(&id)),"output_index":0,"content_index":0,"delta":event.data["delta"].as_str().unwrap_or_default()});
                return Some((
                    sse_value("response.output_text.delta", delta),
                    Some((sessions, reference, id, next_cursor, initialized)),
                ));
            }
            if turn.status != TurnStatus::Running {
                let lookup = sessions.clone();
                let final_reference = reference.clone();
                let final_id = id.clone();
                let final_value = response_snapshot(lookup, final_reference, final_id).await;
                let (response, turn) = match final_value {
                    Ok(value) => value,
                    Err(_) => {
                        return Some((
                            sse_value(
                                "response.failed",
                                json!({"type":"response.failed","response":response_object(&id,"failed","",Some("Pilot delivery history unavailable"))}),
                            ),
                            None,
                        ));
                    }
                };
                let kind = if turn.status == TurnStatus::Completed {
                    "response.completed"
                } else {
                    "response.failed"
                };
                return Some((
                    sse_value(kind, json!({"type":kind,"response":response})),
                    None,
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            Some((
                Ok::<_, Infallible>(Event::default().comment("response is in progress")),
                Some((sessions, reference, id, cursor, initialized)),
            ))
        },
    );
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
        .into_response();
    response
        .headers_mut()
        .insert("x-accel-buffering", "no".parse().unwrap());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());
    Ok(response)
}
