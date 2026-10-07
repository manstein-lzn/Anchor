use super::*;
use crate::application::{WECOM_REPLY_KEY_PREFIX, WECOM_REPLY_KIND};
use anchor_platform_session::{
    ChannelDeliveryRequest, ChannelDeliveryStatus, ChannelIdentity, ChannelInboundRequest,
    TurnStatus,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration;

const MAX_EVENT_TEXT_BYTES: usize = 100_000;
const RUN_WAIT: Duration = Duration::from_secs(120);
const RUN_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventEnvelope {
    event: WecomEvent,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettlementEnvelope {
    event: WecomEvent,
    settlement: GatewaySettlement,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewaySettlement {
    key: String,
    content_sha256: String,
    status: GatewaySettlementStatus,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GatewaySettlementStatus {
    Confirmed,
    Unknown,
    Suppressed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WecomEvent {
    source: String,
    event_id: String,
    sender_id: String,
    conversation_id: String,
    text: String,
    reply_target: String,
    message_type: String,
    metadata: HashMap<String, Value>,
    attachments: Vec<Value>,
}

impl WecomEvent {
    fn validate(&self, config: &WecomSettings) -> Result<(), HttpResponse> {
        if config.graph.is_empty() || config.reply_node.is_empty() {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "WeCom Graph and reply node are not configured",
            ));
        }
        self.validate_payload()?;
        if !config.allows(&self.sender_id) {
            return Err(error(StatusCode::FORBIDDEN, "WeCom user is not allowed"));
        }
        Ok(())
    }

    fn validate_settlement_event(&self) -> Result<(), HttpResponse> {
        self.validate_payload()
    }

    fn validate_payload(&self) -> Result<(), HttpResponse> {
        if self.source != "wecom"
            || !matches!(self.message_type.as_str(), "text" | "mixed")
            || self.event_id.trim().is_empty()
            || self.event_id.len() > 500
            || self.sender_id.trim().is_empty()
            || self.sender_id.len() > 200
            || self.conversation_id.trim().is_empty()
            || self.conversation_id.len() > 200
            || self.reply_target != self.conversation_id
            || self.text.trim().is_empty()
            || self.text.len() > MAX_EVENT_TEXT_BYTES
            || !self.attachments.is_empty()
            || self.metadata.len() != 2
            || self
                .metadata
                .keys()
                .any(|key| !matches!(key.as_str(), "chat_type" | "request_id"))
            || self.metadata.get("chat_type") != Some(&json!("single"))
            || self
                .metadata
                .get("request_id")
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty() || value.len() > 500)
        {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "invalid or unsupported WeCom text event",
            ));
        }
        Ok(())
    }

    fn inbound_id(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"anchor-wecom-inbound-v1\0");
        digest.update(self.source.as_bytes());
        digest.update([0]);
        digest.update(self.event_id.as_bytes());
        format!("wecom-{:x}", digest.finalize())
    }
}

pub(super) async fn receive_event(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid WeCom event envelope"))?;
    if payload.get("settlement").is_some() {
        return Ok((
            StatusCode::OK,
            settle_delivery_value(&state, &headers, payload).await?,
        ));
    }
    let envelope: EventEnvelope = serde_json::from_value(payload)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid WeCom event envelope"))?;
    envelope.event.validate(&state.wecom)?;
    let (_, bundle) = load_graph_definition(&state, &state.wecom.graph).map_err(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "configured WeCom Graph is unavailable",
        )
    })?;
    if !bundle
        .snapshot
        .nodes
        .iter()
        .any(|node| node.id == state.wecom.reply_node)
    {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "configured WeCom reply node is not in the Graph",
        ));
    }

    let event = envelope.event;
    let owner = private_owner(&state, &headers);
    let deadline = tokio::time::Instant::now() + RUN_WAIT;
    let lock_key = format!(
        "{}\0{}\0{}\0{}\0{}",
        owner,
        state.wecom.graph,
        state.wecom.account.as_deref().unwrap_or(""),
        event.conversation_id,
        event.sender_id
    );
    let channel_lock = {
        let mut locks = state.channel_event_locks.lock().await;
        locks.retain(|_, lock| std::sync::Arc::strong_count(lock) > 1);
        locks
            .entry(lock_key)
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let admitted = {
        let _guard = channel_lock.lock().await;
        let inbound_id = event.inbound_id();
        let request = ChannelInboundRequest {
            inbound_id: inbound_id.clone(),
            identity: ChannelIdentity {
                source: "wecom".into(),
                account: state.wecom.account.clone(),
                conversation_id: event.conversation_id.clone(),
                sender_id: event.sender_id.clone(),
            },
            graph: state.wecom.graph.clone(),
            reply_node: state.wecom.reply_node.clone(),
            text: Some(event.text.clone()),
            attachments: Default::default(),
            run_id: None,
            replace_running: true,
        };
        let state_for_admission = state.clone();
        let owner_for_admission = owner.clone();
        let session_event_id = inbound_id.clone();
        let (admission, inbound) = blocking(move || {
            let sessions = store(&state_for_admission)?;
            let admission = sessions
                .admit_channel_inbound(&owner_for_admission, request)
                .map_err(session_error)?;
            let inbound = sessions
                .get_channel_inbound(
                    &owner_for_admission,
                    &admission.session.id,
                    &session_event_id,
                )
                .map_err(session_error)?;
            Ok((admission, inbound))
        })
        .await?;

        if inbound.relation.superseded_by_turn_id.is_some() {
            return Ok((StatusCode::OK, Json(json!({"text":"","superseded":true}))));
        }
        let run = format!("channel-{}", admission.turn.id);
        if let Some(previous) = inbound.previous_run.as_deref() {
            stop_previous_run(&state, previous, deadline).await?;
        }
        let current =
            read_channel_inbound(&state, &owner, &admission.session.id, &inbound_id).await?;
        if current.relation.superseded_by_turn_id.is_some() {
            return Ok((StatusCode::OK, Json(json!({"text":"","superseded":true}))));
        }
        let channel = json!({
            "source":"wecom",
            "conversation_id":event.conversation_id,
            "sender_id":event.sender_id,
            "reply_target":event.reply_target,
        });
        let request = crate::application::ConversationAdmission {
            graph: state.wecom.graph.clone(),
            run: run.clone(),
            session: admission.session.id.clone(),
            reply_node: state.wecom.reply_node.clone(),
            input: json!({
                "message":event.text,
                "channel":channel,
                "session":admission.session.id,
            }),
            previous_run: current.previous_run,
            attachments: Vec::new(),
            channel_inbound: Some(inbound_id),
        };
        let (status, accepted) =
            super::runs::conversation_run(State(state.clone()), headers.clone(), Ok(Json(request)))
                .await?;
        if status != StatusCode::ACCEPTED || accepted.0["run"] != run {
            return Err(error(
                StatusCode::BAD_GATEWAY,
                "WeCom Graph admission returned an unexpected Run",
            ));
        }
        AdmittedRun {
            run_id: run,
            session: admission.session.id,
        }
    };

    let Some(reply) = wait_for_channel_reply(
        &state,
        &owner,
        &state.wecom.reply_node,
        &admitted.session,
        &event.inbound_id(),
        &admitted.run_id,
        deadline,
    )
    .await?
    else {
        return Ok((StatusCode::OK, Json(json!({"text":"","superseded":true}))));
    };

    let content_sha256 = format!("{:x}", Sha256::digest(reply.text.as_bytes()));
    let key = format!(
        "{WECOM_REPLY_KEY_PREFIX}{}:{}",
        reply.session,
        event.inbound_id()
    );
    let delivery_request = ChannelDeliveryRequest {
        key: key.clone(),
        kind: WECOM_REPLY_KIND.into(),
        content_sha256: content_sha256.clone(),
    };
    let session = reply.session.clone();
    let turn = reply.turn.clone();
    let delivery_state = state.clone();
    let owner_for_delivery = owner.clone();
    let delivery = blocking(move || {
        let sessions = store(&delivery_state)?;
        sessions
            .admit_completed_channel_delivery(
                &owner_for_delivery,
                &session,
                &turn,
                delivery_request.clone(),
            )
            .map_err(session_error)?;
        sessions
            .begin_channel_delivery(&owner_for_delivery, &session, &turn, delivery_request)
            .map_err(session_error)
    })
    .await;
    let delivery = match delivery {
        Ok(delivery) => delivery,
        Err(failure) if failure.status() == StatusCode::CONFLICT => {
            if channel_was_superseded(&state, &owner, &reply.session, &event.inbound_id()).await? {
                return Ok((StatusCode::OK, Json(json!({"text":"","superseded":true}))));
            }
            return Err(failure);
        }
        Err(failure) => return Err(failure),
    };
    if delivery.status == ChannelDeliveryStatus::Suppressed {
        return Ok((StatusCode::OK, Json(json!({"text":"","superseded":true}))));
    }
    let response = json!({
        "text":reply.text,
        "session":reply.session,
        "run":admitted.run_id,
        "receipt":{"key":key,"content_sha256":content_sha256},
    });
    Ok((StatusCode::OK, Json(response)))
}

struct ChannelReply {
    session: String,
    turn: String,
    text: String,
}

struct AdmittedRun {
    run_id: String,
    session: String,
}

async fn wait_for_channel_reply(
    state: &ApiState,
    owner: &str,
    reply_node: &str,
    session: &str,
    inbound_id: &str,
    run_id: &str,
    deadline: tokio::time::Instant,
) -> Result<Option<ChannelReply>, HttpResponse> {
    loop {
        let inbound = read_channel_inbound(state, owner, session, inbound_id).await?;
        if inbound.relation.superseded_by_turn_id.is_some()
            || inbound.turn.status == TurnStatus::Interrupted
        {
            return Ok(None);
        }
        let record = FileRunStore::new(state.data_root.join("runs"))
            .load(run_id)
            .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))?;
        if let Some(record) = record
            && !state
                .application
                .active_runs(None)
                .await
                .contains(&run_id.to_owned())
        {
            match record.status {
                RunStatus::Completed => {
                    if inbound.turn.status != TurnStatus::Completed {
                        let elapsed =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        if elapsed.is_zero() {
                            return Err(error(
                                StatusCode::GATEWAY_TIMEOUT,
                                "WeCom Graph is still running",
                            ));
                        }
                        tokio::time::sleep(RUN_POLL.min(elapsed)).await;
                        continue;
                    }
                    let Some(result) = record
                        .results
                        .get(reply_node)
                        .and_then(|results| results.last())
                    else {
                        return Err(error(
                            StatusCode::BAD_GATEWAY,
                            "WeCom reply node produced no result",
                        ));
                    };
                    if result.completion.submission.trim().is_empty() {
                        return Err(error(
                            StatusCode::BAD_GATEWAY,
                            "WeCom reply node produced an empty reply",
                        ));
                    }
                    let path = state
                        .data_root
                        .join("channel-replies")
                        .join(format!("{run_id}.json"));
                    if path.try_exists().map_err(|failure| {
                        error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string())
                    })? {
                        return Err(error(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "rich WeCom replies are not supported by the configured Gateway",
                        ));
                    }
                    return Ok(Some(ChannelReply {
                        session: session.to_owned(),
                        turn: inbound.relation.turn_id,
                        text: result.completion.submission.clone(),
                    }));
                }
                RunStatus::Failed | RunStatus::Aborted | RunStatus::Stopped => {
                    return Err(error(
                        StatusCode::BAD_GATEWAY,
                        "WeCom Graph Run did not complete successfully",
                    ));
                }
                RunStatus::Ready
                | RunStatus::Running
                | RunStatus::Paused
                | RunStatus::BudgetStopped
                | RunStatus::WaitingCall
                | RunStatus::WaitingRecovery => {}
            }
        }
        let elapsed = deadline.saturating_duration_since(tokio::time::Instant::now());
        if elapsed.is_zero() {
            return Err(error(
                StatusCode::GATEWAY_TIMEOUT,
                "WeCom Graph is still running",
            ));
        }
        tokio::time::sleep(RUN_POLL.min(elapsed)).await;
    }
}

async fn read_channel_inbound(
    state: &ApiState,
    owner: &str,
    session: &str,
    inbound_id: &str,
) -> Result<anchor_platform_session::ChannelInboundAdmission, HttpResponse> {
    let state = state.clone();
    let owner = owner.to_owned();
    let session = session.to_owned();
    let inbound_id = inbound_id.to_owned();
    blocking(move || {
        store(&state)?
            .get_channel_inbound(&owner, &session, &inbound_id)
            .map_err(session_error)
    })
    .await
}

async fn channel_was_superseded(
    state: &ApiState,
    owner: &str,
    session: &str,
    inbound_id: &str,
) -> Result<bool, HttpResponse> {
    Ok(read_channel_inbound(state, owner, session, inbound_id)
        .await?
        .relation
        .superseded_by_turn_id
        .is_some())
}

async fn stop_previous_run(
    state: &ApiState,
    run_id: &str,
    deadline: tokio::time::Instant,
) -> Result<(), HttpResponse> {
    let active = state.application.active_runs(None).await;
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(run_id)
        .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))?
        .ok_or_else(|| error(StatusCode::CONFLICT, "previous channel Run is missing"))?;
    if (active.contains(&run_id.to_owned())
        || !matches!(
            record.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted | RunStatus::Stopped
        ))
        && let Err(failure) = state.application.control(run_id, "stop").await
    {
        let current = FileRunStore::new(state.data_root.join("runs"))
            .load(run_id)
            .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))?;
        if current.as_ref().is_none_or(|record| {
            !matches!(
                record.status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted | RunStatus::Stopped
            )
        }) {
            return Err(application_error(failure));
        }
    }
    loop {
        let active = state.application.active_runs(None).await;
        let record = FileRunStore::new(state.data_root.join("runs"))
            .load(run_id)
            .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))?
            .ok_or_else(|| error(StatusCode::CONFLICT, "previous channel Run is missing"))?;
        if !active.contains(&run_id.to_owned())
            && matches!(
                record.status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted | RunStatus::Stopped
            )
        {
            return Ok(());
        }
        let elapsed = deadline.saturating_duration_since(tokio::time::Instant::now());
        if elapsed.is_zero() {
            return Err(error(
                StatusCode::GATEWAY_TIMEOUT,
                "previous channel Run has not stopped",
            ));
        }
        tokio::time::sleep(RUN_POLL.min(elapsed)).await;
    }
}

pub(super) async fn settle_delivery(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, HttpResponse> {
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid WeCom settlement envelope"))?;
    settle_delivery_value(&state, &headers, payload).await
}

async fn settle_delivery_value(
    state: &ApiState,
    headers: &HeaderMap,
    payload: Value,
) -> Result<Json<Value>, HttpResponse> {
    let envelope: SettlementEnvelope = serde_json::from_value(payload)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid WeCom settlement envelope"))?;
    envelope.event.validate_settlement_event()?;

    let inbound_id = envelope.event.inbound_id();
    let Some(key_suffix) = envelope.settlement.key.strip_prefix(WECOM_REPLY_KEY_PREFIX) else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid WeCom delivery receipt key",
        ));
    };
    let Some((session, key_inbound_id)) = key_suffix.split_once(':') else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid WeCom delivery receipt key",
        ));
    };
    let key = &envelope.settlement.key;
    if !(9..=256).contains(&key.len())
        || !key.is_ascii()
        || !key[8..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        || session.is_empty()
        || key_inbound_id != inbound_id
        || key.as_str() != format!("{WECOM_REPLY_KEY_PREFIX}{session}:{inbound_id}").as_str()
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "WeCom delivery receipt does not match its event",
        ));
    }

    let owner = private_owner(state, headers);
    let lookup_state = state.clone();
    let lookup_owner = owner.clone();
    let lookup_session = session.to_owned();
    let lookup_inbound = inbound_id.clone();
    let inbound = blocking(move || {
        store(&lookup_state)?
            .get_channel_inbound(&lookup_owner, &lookup_session, &lookup_inbound)
            .map_err(session_error)
    })
    .await?;
    if inbound.request.identity.source != envelope.event.source
        || inbound.request.identity.conversation_id != envelope.event.conversation_id
        || inbound.request.identity.sender_id != envelope.event.sender_id
        || inbound.request.text.as_deref() != Some(envelope.event.text.as_str())
    {
        return Err(error(
            StatusCode::NOT_FOUND,
            "WeCom settlement does not match an admitted message",
        ));
    }

    let status = match envelope.settlement.status {
        GatewaySettlementStatus::Confirmed => ChannelDeliveryStatus::Confirmed,
        GatewaySettlementStatus::Unknown => ChannelDeliveryStatus::Unknown,
        GatewaySettlementStatus::Suppressed => ChannelDeliveryStatus::Suppressed,
    };
    let request = ChannelDeliveryRequest {
        key: key.clone(),
        kind: WECOM_REPLY_KIND.into(),
        content_sha256: envelope.settlement.content_sha256,
    };
    let settle_state = state.clone();
    let settle_inbound = inbound_id;
    let settle_session = session.to_owned();
    let delivery = blocking(move || {
        store(&settle_state)?
            .settle_channel_delivery_from_gateway(
                &owner,
                &settle_session,
                &settle_inbound,
                request,
                status,
            )
            .map_err(session_error)
    })
    .await?;
    Ok(Json(json!({"delivery":delivery})))
}
