use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use anchor_channel_transport::{
    WebhookClient, allowed, digest,
    ledger::{Ledger, ReadyReply, SettlementStatus, current_time_millis},
    parse_receipt,
};
use serde_json::{Value, json};
use tokio::{
    sync::{broadcast, watch},
    task::JoinSet,
    time::Instant,
};

use crate::{
    ChannelEvent, GatewayConfig, GatewayError, MAX_MEDIA_REQUEST_BYTES, MAX_REQUEST_BYTES,
    MAX_TEXT_BYTES,
    api::{Api, ApiError},
    gateway::WorkerSignal,
    progress::{Activities, End, ReplyCard},
};

const MAX_IN_FLIGHT: usize = 16;

pub(crate) async fn run(
    config: Arc<GatewayConfig>,
    api: Arc<Api>,
    ledger: Arc<Ledger>,
    webhook: Arc<WebhookClient>,
    wake: Arc<WorkerSignal>,
    mut shutdown: watch::Receiver<bool>,
    errors: broadcast::Sender<GatewayError>,
) -> Result<(), GatewayError> {
    let mut requests = JoinSet::<(String, Result<End, GatewayError>)>::new();
    let mut activities = Activities::new(config.clone(), ledger.clone());
    let mut active = BTreeSet::new();
    let mut retry = BTreeMap::<String, (u32, Instant)>::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let (request_stop, request_shutdown) = watch::channel(false);
    let result: Result<(), GatewayError> = async {
    loop {
        tokio::select! {
            biased;
            _ = crate::gateway::stopping(&mut shutdown) => break,
            result = requests.join_next(), if !requests.is_empty() => {
                let (id, result) = result.ok_or(GatewayError::Stopped)?.map_err(|_| GatewayError::Stopped)?;
                active.remove(&id);
                activities.finish(&id, result.as_ref().copied().unwrap_or(End::Failed));
                if let Err(error) = result {
                    if matches!(error, GatewayError::Invalid(_) | GatewayError::Rejected | GatewayError::AssistantStopped | GatewayError::UnsupportedMedia) {
                        let rejected = id.clone();
                        ledger.blocking(move |ledger| ledger.reject(&rejected)).await?;
                        retry.remove(&id);
                    } else if matches!(error, GatewayError::Unconfirmed | GatewayError::PreviousUnconfirmed) {
                        retry.remove(&id);
                    } else {
                        let attempts = retry.get(&id).map_or(1, |(attempts, _)| attempts + 1);
                        let seconds = 1u64 << attempts.min(5);
                        retry.insert(id, (attempts, Instant::now() + Duration::from_secs(seconds)));
                    }
                    let _ = errors.send(error);
                } else {
                    retry.remove(&id);
                }
            },
            _ = wake.wake.notified() => {},
            _ = tick.tick() => {},
        }
        let window = config.recovery_window.as_millis() as i64;
        let (ready, pending) = ledger.blocking(move |ledger| {
            let now = current_time_millis()?;
            ledger.retire_stale_inbounds(now, window)?;
            Ok((ledger.ready_replies(MAX_IN_FLIGHT * 2)?, ledger.pending_events(now, window, MAX_IN_FLIGHT * 2)?))
        }).await?;
        if *shutdown.borrow() { break; }
        let work = ready
            .into_iter()
            .map(Work::Reply)
            .chain(pending.into_iter().map(Work::Event));
        for work in work {
            if active.len() >= MAX_IN_FLIGHT || wake.ack_pending.load(Ordering::Acquire) {
                break;
            }
            let id = work.id().to_owned();
            if active.contains(&id)
                || retry
                    .get(&id)
                    .is_some_and(|(_, when)| *when > Instant::now())
            {
                continue;
            }
            active.insert(id.clone());
            if let Work::Event(event) = &work {
                activities.start(event, webhook.api_key());
            }
            let card = activities.reply_card(&id);
            let config = config.clone();
            let ledger = ledger.clone();
            let api = api.clone();
            let webhook = webhook.clone();
            let mut shutdown = request_shutdown.clone();
            requests.spawn(async move {
                let result = match work {
                    Work::Reply(reply) => send_reply(&config, &api, &ledger, reply, card, &mut shutdown).await,
                    Work::Event(event) => {
                        process_event(&config, &api, &ledger, &webhook, event, card, &mut shutdown).await
                    }
                };
                (id, result)
            });
        }
    }
    Ok(())
    }.await;
    // Cancel network waits, but join requests rather than aborting a future
    // that may still be awaiting a durable blocking write.
    request_stop.send_replace(true);
    while requests.join_next().await.is_some() {}
    activities.shutdown().await;
    result
}

enum Work {
    Event(ChannelEvent),
    Reply(ReadyReply),
}

impl Work {
    fn id(&self) -> &str {
        match self {
            Self::Event(event) => &event.event_id,
            Self::Reply(reply) => &reply.event_id,
        }
    }
}

async fn process_event(
    config: &GatewayConfig,
    api: &Api,
    ledger: &Arc<Ledger>,
    webhook: &WebhookClient,
    event: ChannelEvent,
    card: Option<Arc<ReplyCard>>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<End, GatewayError> {
    if !allowed(&config.inbound_users, &event.sender_id)
        || !allowed(&config.send_users, &event.sender_id)
    {
        return Err(GatewayError::Rejected);
    }
    let enriched = while_running(shutdown, crate::media::enrich(api, &event)).await?;
    let request_limit = if enriched.attachments.is_empty() {
        MAX_REQUEST_BYTES
    } else {
        MAX_MEDIA_REQUEST_BYTES
    };
    let bytes = while_running(
        shutdown,
        webhook.handle(&enriched, request_limit, MAX_REQUEST_BYTES),
    )
    .await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| GatewayError::Webhook)?;
    let object = value.as_object().ok_or(GatewayError::Webhook)?;
    if object
        .get("error")
        .is_some_and(|value| !value.is_null() && value != &json!(false) && value != "")
    {
        return Err(GatewayError::Webhook);
    }
    // An inbound callback may only provide a private text reply.
    if object.keys().any(|key| {
        matches!(
            key.as_str(),
            "msg_item" | "attachments" | "image" | "file" | "voice"
        )
    }) {
        return Err(GatewayError::UnsupportedMedia);
    }
    let receipt = object.get("receipt").map(parse_receipt).transpose()?;
    if object.get("superseded") == Some(&json!(true)) {
        return ledger
            .blocking(move |ledger| {
                ledger.supersede(&event.event_id)?;
                ledger.suppress(&event.event_id, receipt.as_ref())
            })
            .await
            .map(|_| End::Superseded);
    }
    let text = match object.get("text").or_else(|| object.get("reply")) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.trim().is_empty() => None,
        Some(Value::String(text)) if text.len() <= MAX_TEXT_BYTES => Some(text),
        _ => {
            return Err(GatewayError::Invalid(
                "webhook text reply exceeds limits or has invalid type",
            ));
        }
    };
    let Some(text) = text else {
        if receipt.is_some() {
            return Err(GatewayError::Invalid(
                "webhook receipt requires a reply or superseded outcome",
            ));
        }
        return ledger
            .blocking(move |ledger| ledger.suppress(&event.event_id, None))
            .await
            .map(|_| End::Suppressed);
    };
    let ready_id = event.event_id.clone();
    let ready_text = text.clone();
    ledger
        .blocking(move |ledger| ledger.ready(&ready_id, &ready_text, None, receipt.as_ref()))
        .await?;
    send_reply(
        config,
        api,
        ledger,
        ReadyReply {
            event_id: event.event_id.clone(),
            callback_id: event.event_id,
            sender_id: event.sender_id,
            conversation_id: event.conversation_id,
            text: text.clone(),
            items: None,
        },
        card,
        shutdown,
    )
    .await
}

async fn send_reply(
    config: &GatewayConfig,
    api: &Api,
    ledger: &Arc<Ledger>,
    reply: ReadyReply,
    progress: Option<Arc<ReplyCard>>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<End, GatewayError> {
    if !allowed(&config.inbound_users, &reply.sender_id)
        || !allowed(&config.send_users, &reply.sender_id)
    {
        return Err(GatewayError::Rejected);
    }
    let card = crate::rich::markdown_card(&reply.text)?;
    let token = while_running(shutdown, api.token()).await?;
    let mut target = if let Some(progress) = &progress {
        // Join bounded progress writes before claiming a final effect. Only
        // the worker owns this ID afterwards, so stale status cannot erase it.
        while_running(shutdown, async { Ok(progress.take_for_reply().await) }).await?
    } else {
        None
    };
    let check_id = reply.event_id.clone();
    if !ledger
        .blocking(move |ledger| ledger.is_latest_event(&check_id))
        .await?
    {
        if let (Some(progress), Some(id)) = (&progress, target.take()) {
            progress.return_for_status(id).await;
        }
        return ledger
            .blocking(move |ledger| ledger.suppress(&reply.event_id, None))
            .await
            .map(|_| End::Superseded);
    }
    let content_digest = digest(
        &serde_json::to_vec(&json!([
            reply.event_id,
            reply.sender_id,
            reply.conversation_id,
            reply.text,
        ]))
        .map_err(|_| GatewayError::Ledger)?,
    );
    let wire_id = format!("feishu-reply-{}", digest(reply.event_id.as_bytes()));
    let claim_id = reply.event_id.clone();
    let claim_wire = wire_id.clone();
    if !ledger
        .blocking(move |ledger| ledger.claim("reply", &claim_id, &content_digest, &claim_wire))
        .await?
    {
        return Ok(End::Sent);
    }
    let confirmed = while_running(shutdown, async {
        if let Some(id) = target {
            match api.update_card(&id, &card, &token).await {
                Ok(()) => return Ok(()),
                Err(ApiError::Unknown) => return Err(GatewayError::Unconfirmed),
                Err(ApiError::Rejected) => {
                    // The platform explicitly refused this mutation. Only in
                    // this case may the same claim use a separate final reply.
                    if let Some(progress) = &progress {
                        progress.return_for_status(id).await;
                    }
                }
            }
        }
        api.reply(&reply.event_id, &card, &token).await.map(|_| ())
    })
    .await
    .is_ok();
    ledger
        .blocking(move |ledger| {
            ledger.finish_reply(
                &reply.event_id,
                &wire_id,
                if confirmed {
                    SettlementStatus::Confirmed
                } else {
                    SettlementStatus::Unknown
                },
            )
        })
        .await?;
    if confirmed {
        Ok(End::Sent)
    } else {
        Err(GatewayError::Unconfirmed)
    }
}

pub(crate) async fn while_running<T>(
    shutdown: &mut watch::Receiver<bool>,
    operation: impl Future<Output = Result<T, GatewayError>>,
) -> Result<T, GatewayError> {
    tokio::select! {
        biased;
        _ = crate::gateway::stopping(shutdown) => Err(GatewayError::Stopped),
        result = operation => result,
    }
}
