use super::*;
use crate::application::{WECOM_REPLY_KEY_PREFIX, WECOM_REPLY_KIND};
use anchor_platform_session::{
    AttachmentManifest, AttachmentManifestEntry, ChannelDeliveryRequest, ChannelDeliveryStatus,
    ChannelIdentity, ChannelInboundRequest, TurnStatus,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration;

const MAX_EVENT_TEXT_BYTES: usize = 100_000;
/// Interrupted user messages folded into the next Run of the same session.
const MAX_INTERRUPTED_MESSAGES: usize = 8;
const MAX_INTERRUPTED_MESSAGE_CHARS: usize = 500;
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
    attachments: Vec<crate::channel_inputs::UploadedAttachment>,
}

impl WecomEvent {
    fn validate(&self, config: &WecomSettings) -> Result<(), HttpResponse> {
        if config.graph.is_empty() || config.reply_node.is_empty() {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "WeCom Graph and reply node are not configured",
            ));
        }
        self.validate_payload(true)?;
        if !config.allows(&self.sender_id) {
            return Err(error(StatusCode::FORBIDDEN, "WeCom user is not allowed"));
        }
        Ok(())
    }

    fn validate_settlement_event(&self) -> Result<(), HttpResponse> {
        // A settlement identifies the event and its receipt. It does not carry
        // attachment content, because the gateway cannot resend fetched media.
        self.validate_payload(false)
    }

    fn validate_payload(&self, require_content: bool) -> Result<(), HttpResponse> {
        if self.source != "wecom"
            || !matches!(
                self.message_type.as_str(),
                "text" | "mixed" | "image" | "file" | "voice"
            )
            || self.event_id.trim().is_empty()
            || self.event_id.len() > 500
            || self.sender_id.trim().is_empty()
            || self.sender_id.len() > 200
            || self.conversation_id.trim().is_empty()
            || self.conversation_id.len() > 200
            || self.reply_target != self.conversation_id
            || self.text.len() > MAX_EVENT_TEXT_BYTES
            || (require_content
                && (self.text.trim().is_empty() && self.attachments.is_empty()
                    || (self.message_type == "text" && !self.attachments.is_empty())
                    || (matches!(self.message_type.as_str(), "image" | "file" | "voice")
                        && self.attachments.is_empty())))
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
        inbound_id_for(&self.source, &self.event_id)
    }
}

/// The single definition of a WeCom inbound identity: `sha256` over the domain
/// tag, the platform source and the platform message id. The admission path and
/// the read-only progress path must agree on it, so neither may reimplement it.
pub(super) fn inbound_id_for(source: &str, event_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"anchor-wecom-inbound-v1\0");
    digest.update(source.as_bytes());
    digest.update([0]);
    digest.update(event_id.as_bytes());
    format!("wecom-{:x}", digest.finalize())
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
    let prepared_attachments = crate::channel_inputs::prepare(&envelope.event.attachments)
        .map_err(|failure| error(StatusCode::BAD_REQUEST, failure))?;
    let attachment_entries = prepared_attachments.manifest();
    if envelope.event.message_type == "image"
        && !attachment_entries.iter().any(|attachment| {
            attachment
                .media_type
                .as_deref()
                .is_some_and(|mime| mime.starts_with("image/"))
        })
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "image events must include a supported image attachment",
        ));
    }
    let channel_attachments = AttachmentManifest {
        format: 1,
        files: attachment_entries
            .into_iter()
            .map(|attachment| AttachmentManifestEntry {
                path: format!("/in/channel/{}", attachment.name),
                name: attachment.name,
                sha256: attachment.sha256,
                size: attachment.size,
                media_type: attachment.media_type,
            })
            .collect(),
    };
    let (bundle_path, bundle) =
        load_graph_definition(&state, &state.wecom.graph).map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "configured WeCom Graph is unavailable",
            )
        })?;
    let assistant_plan =
        crate::assistant::AssistantPlan::from_snapshot(&bundle.snapshot, &state.wecom.reply_node);
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

    let mut event = envelope.event;
    let input_message = if event.text.trim().is_empty() {
        "请查看随附文件并据此回复。".to_owned()
    } else {
        event.text.clone()
    };
    let owner = private_owner(&state, &headers);
    let lock_key = format!(
        "{}\0{}\0{}\0{}\0{}",
        owner,
        state.wecom.graph,
        state.wecom.account.as_deref().unwrap_or(""),
        event.conversation_id,
        event.sender_id
    );
    // Persist the accepted message before serialising on the conversation. The
    // inbound row is the durable fact that makes an accepted message visible —
    // the progress projection, pending assistant inputs and supersede
    // bookkeeping all read it — while the channel lock still guards Run
    // admission and keeps one conversation strictly serial. Admission stays
    // idempotent per event and supersedes the previous running Turn inside the
    // same store transaction, so the winner never depends on lock order.
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
        text: Some(input_message.clone()),
        attachments: channel_attachments.clone(),
        run_id: None,
        replace_running: true,
    };
    let admission = {
        let state_for_admission = state.clone();
        let owner_for_admission = owner.clone();
        blocking(move || {
            store(&state_for_admission)?
                .admit_channel_inbound(&owner_for_admission, request)
                .map_err(session_error)
        })
        .await?
    };
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
        // Authoritative state under the lock: between admission and
        // serialisation another message may have superseded this Turn.
        let current =
            read_channel_inbound(&state, &owner, &admission.session.id, &inbound_id).await?;
        if current.relation.superseded_by_turn_id.is_some() {
            return Ok((StatusCode::OK, Json(json!({"text":"","superseded":true}))));
        }
        let saved_assistant = store(&state)?
            .get_channel_assistant(&owner, &admission.session.id)
            .map_err(session_error)?;
        let persistent_assistant =
            assistant_plan.as_ref().is_ok_and(Option::is_some) || saved_assistant.is_some();
        if persistent_assistant {
            let run = match saved_assistant {
                Some(binding) => binding.run_id,
                None => {
                    let plan = assistant_plan
                        .map_err(|message| error(StatusCode::BAD_REQUEST, message))?
                        .ok_or_else(|| {
                            error(StatusCode::CONFLICT, "assistant Graph has no input loop")
                        })?;
                    let previous_run = match current.previous_run.as_deref() {
                        Some(previous) if stop_previous_run(&state, previous).await? => {
                            Some(previous.to_owned())
                        }
                        _ => None,
                    };
                    let run = format!("assistant-{}", admission.turn.id);
                    let lease = state
                        .application
                        .acquire_graph_lease_waiting(&bundle_path)
                        .await
                        .map_err(application_error)?;
                    state
                        .application
                        .admit_assistant(
                            crate::application::AssistantAdmission {
                                graph: state.wecom.graph.clone(),
                                session: admission.session.id.clone(),
                                owner: owner.clone(),
                                oauth_owner: super::oauth::binding_owner(&owner),
                                run,
                                previous_run,
                            },
                            &bundle_path,
                            bundle,
                            lease,
                            plan,
                        )
                        .await
                        .map_err(application_error)?
                }
            };
            let metadata = crate::application::metadata::load(&state.data_root, &run)
                .map_err(application_error)?
                .ok_or_else(|| error(StatusCode::CONFLICT, "assistant metadata is missing"))?;
            if metadata.assistant.as_ref().is_none_or(|binding| {
                binding.owner != owner || binding.session != admission.session.id
            }) {
                return Err(error(
                    StatusCode::FORBIDDEN,
                    "assistant binding does not belong to this Session",
                ));
            }
            let mut selected = metadata.clone();
            selected.run_id = format!("turn-{}", admission.turn.id);
            selected.attachments = prepared_attachments.manifest();
            crate::channel_inputs::freeze(&state.data_root, &selected, &prepared_attachments)
                .map_err(|message| error(StatusCode::CONFLICT, message))?;
            crate::assistant::signal(&admission.session.id).notify_waiters();
            AdmittedRun {
                run_id: run,
                session: admission.session.id,
            }
        } else {
            assistant_plan.map_err(|message| error(StatusCode::BAD_REQUEST, message))?;
            let run = format!("channel-{}", admission.turn.id);
            let previous_run = match current.previous_run.as_deref() {
                Some(previous) if stop_previous_run(&state, previous).await? => {
                    Some(previous.to_owned())
                }
                _ => None,
            };
            let channel = json!({
                "source":"wecom",
                "conversation_id":event.conversation_id,
                "sender_id":event.sender_id,
                "reply_target":event.reply_target,
            });
            // A new message interrupts the previous turn. Whatever the model has
            // not been asked yet travels with this Run, so the interrupted work
            // continues with the new information instead of disappearing.
            let interrupted =
                read_interrupted_messages(&state, &owner, &admission.session.id, &inbound_id)
                    .await?;
            let mut input = json!({
                "message":input_message,
                "channel":channel,
                "session":admission.session.id,
            });
            if !interrupted.is_empty() {
                input["interrupted_messages"] = json!(interrupted);
            }
            let request = crate::application::ConversationAdmission {
                graph: state.wecom.graph.clone(),
                run: run.clone(),
                session: admission.session.id.clone(),
                reply_node: state.wecom.reply_node.clone(),
                input,
                previous_run,
                attachments: std::mem::take(&mut event.attachments),
                channel_inbound: Some(inbound_id),
            };
            let (status, accepted) = super::runs::conversation_run(
                State(state.clone()),
                headers.clone(),
                Ok(Json(request)),
            )
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
        }
    };

    let Some(reply) = wait_for_channel_reply(
        &state,
        &owner,
        &state.wecom.reply_node,
        &admitted.session,
        &event.inbound_id(),
        &admitted.run_id,
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
    let ChannelReply {
        session: reply_session,
        turn: _,
        text,
        items,
    } = reply;
    let mut response = json!({
        "text":text,
        "session":reply_session,
        "run":admitted.run_id,
        "receipt":{"key":key,"content_sha256":content_sha256},
    });
    if !items.is_empty() {
        response["msg_item"] = json!(items);
    }
    Ok((StatusCode::OK, Json(response)))
}

struct ChannelReply {
    session: String,
    turn: String,
    text: String,
    items: Vec<Value>,
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
        if let Some(record) = record.as_ref() {
            let metadata = crate::application::metadata::load(&state.data_root, run_id)
                .map_err(application_error)?;
            if let Some(source) = metadata
                .as_ref()
                .and_then(|metadata| metadata.assistant.as_ref())
            {
                if source.owner != owner || source.session != session {
                    return Err(error(
                        StatusCode::FORBIDDEN,
                        "assistant reply belongs to another Session",
                    ));
                }
                if let Some(results) = record.results.get(&source.reply_node) {
                    for result in results.iter().rev() {
                        let Some(binding) =
                            crate::assistant::binding(&state.data_root, &result.key)
                                .map_err(|message| error(StatusCode::CONFLICT, message))?
                        else {
                            continue;
                        };
                        if binding.turn != inbound.relation.turn_id || binding.inbound != inbound_id
                        {
                            continue;
                        }
                        if result.completion.output.get("suppressed") == Some(&json!(true)) {
                            return Ok(None);
                        }
                        if result
                            .completion
                            .output
                            .get("reply_for")
                            .and_then(Value::as_str)
                            != Some(binding.turn.as_str())
                            || result.completion.submission.trim().is_empty()
                        {
                            return Err(error(
                                StatusCode::CONFLICT,
                                "assistant reply identity changed",
                            ));
                        }
                        let commit: anchor_runtime::graph::CommitRef = serde_json::from_value(
                            result.completion.output["source_commit"].clone(),
                        )
                        .map_err(|_| {
                            error(StatusCode::CONFLICT, "assistant reply source is missing")
                        })?;
                        let work_key = anchor_runtime::graph::InvocationKey {
                            node_id: commit.node_id,
                            invocation: commit.invocation,
                            ..result.key.clone()
                        };
                        let items = crate::channel_tools::read_reply_images_for_invocation(
                            &state.data_root,
                            &work_key,
                        )
                        .map_err(|message| error(StatusCode::UNPROCESSABLE_ENTITY, message))?;
                        if json!(items) != result.completion.output["items"] {
                            return Err(error(
                                StatusCode::CONFLICT,
                                "assistant reply image set changed",
                            ));
                        }
                        if inbound.turn.status == TurnStatus::Running {
                            store(state)?
                                .finish_turn(
                                    owner,
                                    session,
                                    &binding.turn,
                                    TurnStatus::Completed,
                                    None,
                                )
                                .map_err(session_error)?;
                        }
                        return Ok(Some(ChannelReply {
                            session: session.to_owned(),
                            turn: binding.turn,
                            text: result.completion.submission.clone(),
                            items,
                        }));
                    }
                }
                if matches!(
                    record.status,
                    RunStatus::Failed | RunStatus::Aborted | RunStatus::Stopped
                ) || (matches!(record.status, RunStatus::Ready | RunStatus::Running)
                    && !state.application.run_is_active(run_id).await)
                {
                    return Err(error(
                        StatusCode::CONFLICT,
                        "assistant is stopped; explicit resume or a new instance is required",
                    ));
                }
                tokio::time::sleep(RUN_POLL).await;
                continue;
            }
        }
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
                        tokio::time::sleep(RUN_POLL).await;
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
                    // The configured Gateway delivers text plus validated reply
                    // images. Anything else in the file is refused here rather
                    // than silently dropped by the transport.
                    let items =
                        crate::channel_tools::read_reply_images(&state.data_root, run_id)
                            .map_err(|failure| error(StatusCode::UNPROCESSABLE_ENTITY, failure))?;
                    return Ok(Some(ChannelReply {
                        session: session.to_owned(),
                        turn: inbound.relation.turn_id,
                        text: result.completion.submission.clone(),
                        items,
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
        tokio::time::sleep(RUN_POLL).await;
    }
}

/// User messages this session has no confirmed reply for, newest last.
async fn read_interrupted_messages(
    state: &ApiState,
    owner: &str,
    session: &str,
    inbound_id: &str,
) -> Result<Vec<String>, HttpResponse> {
    let state = state.clone();
    let owner = owner.to_owned();
    let session = session.to_owned();
    let inbound_id = inbound_id.to_owned();
    blocking(move || {
        let pending = store(&state)?
            .pending_channel_messages(&owner, &session, &inbound_id, MAX_INTERRUPTED_MESSAGES)
            .map_err(session_error)?;
        Ok(pending
            .iter()
            .filter_map(interrupted_text)
            .collect::<Vec<_>>())
    })
    .await
}

fn interrupted_text(message: &anchor_platform_session::ChannelPendingMessage) -> Option<String> {
    let text = message.text.trim();
    let rendered = match (text.is_empty(), message.attachments) {
        (false, 0) => text.to_owned(),
        (false, count) => format!("{text}（同一条消息还附带了 {count} 个文件）"),
        (true, 0) => return None,
        (true, count) => format!("（用户发来 {count} 个文件，本轮工作区没有挂载这些文件）"),
    };
    Some(
        rendered
            .chars()
            .take(MAX_INTERRUPTED_MESSAGE_CHARS)
            .collect(),
    )
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

pub(super) async fn stop_previous_run(
    state: &ApiState,
    run_id: &str,
) -> Result<bool, HttpResponse> {
    let active = state.application.active_runs(None).await;
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(run_id)
        .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))?;
    let Some(record) = record else {
        if active.contains(&run_id.to_owned()) {
            return Err(error(
                StatusCode::CONFLICT,
                "previous channel Run is active but its durable record is missing",
            ));
        }
        return Ok(false);
    };
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
            .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))?;
        let Some(record) = record else {
            if active.contains(&run_id.to_owned()) {
                return Err(error(
                    StatusCode::CONFLICT,
                    "previous channel Run is active but its durable record is missing",
                ));
            }
            return Ok(false);
        };
        if !active.contains(&run_id.to_owned())
            && matches!(
                record.status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted | RunStatus::Stopped
            )
        {
            return Ok(true);
        }
        tokio::time::sleep(RUN_POLL).await;
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
    // An attachment-only message is admitted with a synthesized instruction, so
    // the platform's own empty text can never match it. The receipt key names
    // this exact inbound and the identity fields below pin the sender and
    // conversation, which is what "the same message" means here.
    if inbound.request.identity.source != envelope.event.source
        || inbound.request.identity.conversation_id != envelope.event.conversation_id
        || inbound.request.identity.sender_id != envelope.event.sender_id
        || inbound.request.text.as_deref().is_none()
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
