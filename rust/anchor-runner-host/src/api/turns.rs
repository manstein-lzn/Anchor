use super::*;
use crate::{
    pilot_host::{PilotAdmission, scope},
    pilot_tools::PilotTools,
};
#[cfg(feature = "legacy-regression")]
use anchor_io_harness_runtime::pilot;
use anchor_platform_session::{Session, SessionStore, TurnStatus};
use axum::response::sse::{Event, KeepAlive, Sse};
use std::{collections::VecDeque, convert::Infallible, time::Duration};

fn pilot_session(session: &Session) -> Result<(), HttpResponse> {
    if !session.graph.is_empty() || !session.channel.is_empty() {
        return Err(error(
            StatusCode::NOT_IMPLEMENTED,
            "native Graph/channel Session Turn admission is not available in this slice",
        ));
    }
    Ok(())
}

pub(super) async fn recover_pilot_turns(state: &ApiState) -> Result<(), String> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        let sessions = store(&state).map_err(|_| "Pilot storage startup failed".to_owned())?;
        sessions
            .interrupt_running()
            .map_err(|_| "Pilot startup recovery failed".to_owned())?;
        sessions
            .recover_channel_deliveries()
            .map_err(|_| "Channel delivery startup recovery failed".to_owned())?;
        for (run, record) in state
            .application
            .records()
            .map_err(|_| "Pilot Run facts unavailable".to_owned())?
        {
            if matches!(
                record.status,
                anchor_runtime_rig::graph::RunStatus::Completed
                    | anchor_runtime_rig::graph::RunStatus::Failed
                    | anchor_runtime_rig::graph::RunStatus::Stopped
                    | anchor_runtime_rig::graph::RunStatus::Aborted
            ) {
                state
                    .application
                    .settle_channel_run(&run, record.status)
                    .map_err(|_| "Channel Turn startup settlement failed".to_owned())?;
            }
            let metadata = state
                .application
                .metadata(&run)
                .map_err(|_| "Pilot Run identity unavailable".to_owned())?;
            if let Some(source) = metadata
                .as_ref()
                .and_then(|metadata| metadata.pilot.clone())
            {
                match sessions.associate_run(&source.owner, &source.session, &source.turn, &run) {
                    Ok(_) | Err(anchor_platform_session::SessionError::Missing) => {}
                    Err(_) => return Err("Pilot Run association recovery failed".into()),
                }
            }
            if let Some(source) = metadata.and_then(|metadata| metadata.channel) {
                match sessions.associate_channel_run(
                    &source.owner,
                    &source.session,
                    &source.inbound,
                    &run,
                ) {
                    Ok(_) | Err(anchor_platform_session::SessionError::Missing) => {}
                    Err(_) => return Err("Channel Run association recovery failed".into()),
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| "Pilot startup recovery failed".to_owned())?
}

pub(super) async fn list_pilot_turns(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let (sessions, owner, session) = owned_session(&state, &headers, &identifier)?;
        pilot_session(&session)?;
        let turns = sessions
            .list_turns(&owner, &identifier)
            .map_err(session_error)?;
        Ok(Json(json!({"turns":turns})))
    })
    .await
}

pub(super) async fn get_pilot_turn(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((identifier, turn)): AxumPath<(String, String)>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let (sessions, owner, session) = owned_session(&state, &headers, &identifier)?;
        pilot_session(&session)?;
        let turn = sessions
            .get_turn(&owner, &identifier, &turn)
            .map_err(session_error)?;
        Ok(Json(json!({"turn":turn})))
    })
    .await
}

pub(super) async fn list_pilot_questions(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((identifier, turn)): AxumPath<(String, String)>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let (sessions, owner, session) = owned_session(&state, &headers, &identifier)?;
        pilot_session(&session)?;
        let questions = sessions
            .list_questions(&owner, &identifier, &turn)
            .map_err(session_error)?;
        Ok(Json(json!({"questions":questions})))
    })
    .await
}

pub(super) async fn answer_pilot_question(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((identifier, turn, question)): AxumPath<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let (sessions, owner, session) = owned_session(&state, &headers, &identifier)?;
        pilot_session(&session)?;
        let answer = serde_json::from_value(body)
            .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid question answer"))?;
        let question = sessions
            .answer_question(&owner, &identifier, &turn, &question, answer)
            .map_err(session_error)?;
        Ok(Json(json!({"question":question})))
    })
    .await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateTurnBody {
    request_id: String,
    message: Option<String>,
    #[serde(default)]
    resume: bool,
}

pub(super) async fn create_pilot_turn(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let lookup = state.clone();
    let (sessions, owner, session) =
        blocking(move || owned_session(&lookup, &headers, &identifier)).await?;
    pilot_session(&session)?;
    let body: CreateTurnBody = serde_json::from_value(body).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "provide request_id and either message or resume:true",
        )
    })?;
    if body.message.is_some() == body.resume {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "provide exactly one of message or resume:true",
        ));
    }
    if body.request_id.trim().is_empty()
        || body.request_id.len() > 128
        || body
            .message
            .as_ref()
            .is_some_and(|message| message.trim().is_empty() || message.len() > 64 * 1024)
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "request_id or message is empty or too long",
        ));
    }
    let root = scope(&state.data_root, &owner, &session);
    let tools = PilotTools {
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
    let turn = state
        .pilots
        .admit(PilotAdmission {
            sessions,
            owner,
            session,
            request_id: body.request_id,
            prompt: body.message,
            root,
            tools,
        })
        .await
        .map_err(|failure| match failure {
            crate::pilot_host::AdmissionError::Session(failure) => session_error(failure),
            crate::pilot_host::AdmissionError::Provider => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Pilot provider is not configured or unavailable",
            ),
        })?;
    Ok((StatusCode::ACCEPTED, Json(json!({"turn":turn}))))
}

pub(super) async fn pilot_messages(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let (sessions, owner, session) = owned_session(&state, &headers, &identifier)?;
        pilot_session(&session)?;
        #[cfg(feature = "legacy-regression")]
        let messages = {
            let root = scope(&state.data_root, &owner, &session);
            if root.join("goose.json").exists() {
                crate::goose_acp::pilot::messages(&sessions, &owner, &identifier)
            } else {
                pilot::pilot_messages(&root)
            }
        };
        #[cfg(not(feature = "legacy-regression"))]
        let messages = crate::goose_acp::pilot::messages(&sessions, &owner, &identifier);
        let messages = messages.map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Pilot history is unavailable",
            )
        })?;
        let messages = messages
            .into_iter()
            .filter(|message| {
                matches!(message["role"].as_str(), Some("user" | "assistant"))
                    && message["text"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty())
            })
            .map(|message| json!({"role":message["role"],"text":message["text"]}))
            .collect::<Vec<_>>();
        Ok(Json(json!({"messages":messages})))
    })
    .await
}

pub(super) async fn stop_pilot(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let lookup = state.clone();
    let (_, owner, session) =
        blocking(move || owned_session(&lookup, &headers, &identifier)).await?;
    pilot_session(&session)?;
    let stopping = state
        .pilots
        .stop(&scope(&state.data_root, &owner, &session))
        .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"session":session,"stopping":stopping})),
    ))
}

#[derive(serde::Deserialize, Default)]
pub(super) struct TurnEventsQuery {
    after: Option<u64>,
}

struct Cursor {
    sessions: SessionStore,
    owner: String,
    session: String,
    turn: String,
    after: u64,
    pending: VecDeque<Event>,
    ended: bool,
}

pub(super) async fn pilot_turn_events(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((identifier, turn)): AxumPath<(String, String)>,
    Query(query): Query<TurnEventsQuery>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, HttpResponse> {
    let last = match headers.get("last-event-id") {
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "invalid Last-Event-ID"))?,
        None => 0,
    };
    let after = query.after.unwrap_or(0).max(last);
    let (sessions, owner, session) =
        blocking(move || owned_session(&state, &headers, &identifier)).await?;
    pilot_session(&session)?;
    let lookup = sessions.clone();
    let lookup_owner = owner.clone();
    let lookup_session = session.id.clone();
    let lookup_turn = turn.clone();
    blocking(move || {
        lookup
            .get_turn(&lookup_owner, &lookup_session, &lookup_turn)
            .map_err(session_error)
    })
    .await?;
    let cursor = Cursor {
        sessions,
        owner,
        session: session.id,
        turn,
        after,
        pending: VecDeque::new(),
        ended: false,
    };
    let stream = futures_util::stream::unfold(cursor, |mut cursor| async move {
        loop {
            if let Some(event) = cursor.pending.pop_front() {
                return Some((Ok(event), cursor));
            }
            if cursor.ended {
                return None;
            }
            let sessions = cursor.sessions.clone();
            let owner = cursor.owner.clone();
            let session = cursor.session.clone();
            let turn = cursor.turn.clone();
            let after = cursor.after;
            let loaded = tokio::task::spawn_blocking(move || {
                let turn_record = sessions.get_turn(&owner, &session, &turn)?;
                let events = sessions.turn_events(&owner, &session, &turn, after)?;
                Ok::<_, anchor_platform_session::SessionError>((turn_record, events))
            })
            .await;
            match loaded {
                Ok(Ok((turn, events))) => {
                    for event in events {
                        cursor.after = event.seq;
                        cursor.pending.push_back(
                            Event::default()
                                .id(event.seq.to_string())
                                .data(event.data.to_string()),
                        );
                    }
                    if turn.status != TurnStatus::Running {
                        cursor.pending.push_back(
                            Event::default()
                                .event("turn")
                                .data(serde_json::to_string(&turn).expect("Turn is JSON")),
                        );
                        cursor.ended = true;
                    }
                }
                _ => {
                    cursor.pending.push_back(Event::default().event("error").data(json!({"type":"error","errorText":"Pilot delivery history unavailable"}).to_string()));
                    cursor.ended = true;
                }
            }
            if cursor.pending.is_empty() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}
