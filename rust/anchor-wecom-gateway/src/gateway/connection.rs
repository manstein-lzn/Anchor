use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    net::TcpStream,
    time::{Instant, MissedTickBehavior},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

use super::status;
use super::{
    ConnectionStatus, Driver, MAX_IN_FLIGHT, MAX_MEDIA_IN_FLIGHT, MediaJob, MediaPending,
    MediaStage, MediaStep, PendingDelivery, SendRequest, stopping,
};
use crate::{
    GatewayError, MAX_MEDIA_FRAME_BYTES, MAX_REQUEST_BYTES,
    ledger::ReadyReply,
    protocol::{digest, frame},
    stream_id,
};

pub(super) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

impl Driver {
    pub(super) async fn run(mut self) -> Result<(), GatewayError> {
        let mut delay = self.config.timing.reconnect_base;
        loop {
            self.status.send_replace(ConnectionStatus::Connecting);
            let connect = tokio::time::timeout(
                self.config.timing.connect_timeout,
                connect_async_with_config(
                    self.config.ws_url.as_str(),
                    Some(
                        WebSocketConfig::default()
                            .max_message_size(Some(MAX_MEDIA_FRAME_BYTES))
                            .max_frame_size(Some(MAX_MEDIA_FRAME_BYTES)),
                    ),
                    true,
                ),
            );
            let mut socket = tokio::select! {
                _ = stopping(&mut self.shutdown) => break,
                result = connect => match result {
                    Ok(Ok((socket, _))) => Some(socket),
                    _ => None,
                },
            };
            if let Some(socket) = &mut socket {
                self.status.send_replace(ConnectionStatus::Authenticating);
                let authentication = tokio::select! {
                    _ = stopping(&mut self.shutdown) => break,
                    result = authenticate(socket, &self.config) => result,
                };
                if authentication.is_ok() {
                    eprintln!("anchor-wecom-gateway: platform connection authenticated");
                    self.status.send_replace(ConnectionStatus::Authenticated);
                    delay = self.config.timing.reconnect_base;
                    self.recover_media()?;
                    self.connected(socket).await?;
                } else {
                    self.report(GatewayError::Disconnected);
                }
            }
            drop(socket);
            self.status.send_replace(ConnectionStatus::Reconnecting);
            self.abandon_pending()?;
            if *self.shutdown.borrow() {
                break;
            }
            let reconnect = tokio::time::sleep(delay);
            tokio::pin!(reconnect);
            loop {
                tokio::select! {
                    _ = stopping(&mut self.shutdown) => return Ok(()),
                    _ = &mut reconnect => break,
                    Some(request) = self.incoming.recv() => self.reject_disconnected(request),
                    Some(line) = self.progress.recv() => self.record_progress(line),
                    Some((event, result)) = self.callbacks.next(), if !self.callbacks.is_empty() => {
                        self.callback_finished(event, result)?;
                    },
                }
            }
            delay = delay
                .saturating_mul(2)
                .min(self.config.timing.reconnect_max);
        }
        self.abandon_pending()
    }

    async fn connected(&mut self, socket: &mut Socket) -> Result<(), GatewayError> {
        let mut heartbeat = tokio::time::interval_at(
            Instant::now() + self.config.timing.heartbeat_interval,
            self.config.timing.heartbeat_interval,
        );
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut ping: Option<(String, Instant)> = None;
        loop {
            if !self.dispatch_ready(socket).await? {
                return Ok(());
            }
            if !self.dispatch_media(socket).await? {
                return Ok(());
            }
            let next_deadline = self
                .pending
                .values()
                .map(|pending| pending.deadline)
                .chain(self.feedback_pending.values().map(|entry| entry.deadline))
                .chain(self.acks.iter().map(|ack| ack.deadline))
                // An idle bubble is not idle work: it still owes the next
                // animation frame, so the loop must wake for it.
                .chain(self.open_bubbles.values().flat_map(|bubble| {
                    let status = bubble.pending.is_some().then_some(bubble.next_update);
                    let animation =
                        status::animating(bubble.frames).then_some(bubble.next_animation);
                    status.into_iter().chain(animation)
                }))
                .chain(self.media_pending.values().map(|pending| pending.deadline))
                .chain(ping.iter().map(|(_, deadline)| *deadline))
                .min()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(180));
            tokio::select! {
                _ = stopping(&mut self.shutdown) => return Ok(()),
                incoming = socket.next() => {
                    let (value, binary) = match incoming {
                        Some(Ok(message @ (Message::Text(_) | Message::Binary(_)))) => {
                            let binary = matches!(&message, Message::Binary(_));
                            (serde_json::from_slice::<Value>(&message.into_data()).ok(), binary)
                        },
                        Some(Ok(Message::Ping(_))) => {
                            if !flush(socket, self.config.timing.ack_timeout).await { return Ok(()); }
                            continue;
                        },
                        Some(Ok(Message::Pong(_))) => continue,
                        Some(Ok(Message::Close(frame))) => {
                            eprintln!(
                                "anchor-wecom-gateway: platform closed the connection: {frame:?}"
                            );
                            return Ok(());
                        },
                        Some(Err(error)) => {
                            eprintln!(
                                "anchor-wecom-gateway: platform connection error: {error}"
                            );
                            return Ok(());
                        },
                        Some(Ok(other)) => {
                            eprintln!(
                                "anchor-wecom-gateway: unexpected platform frame: {other:?}"
                            );
                            return Ok(());
                        },
                        None => {
                            eprintln!("anchor-wecom-gateway: platform connection ended; reconnecting");
                            return Ok(());
                        },
                    };
                    let Some(value) = value.filter(Value::is_object) else {
                        self.report(GatewayError::Invalid("invalid platform JSON frame"));
                        continue;
                    };
                    if value.get("cmd").and_then(Value::as_str) == Some("aibot_msg_callback") {
                        eprintln!("anchor-wecom-gateway: {} message callback received", if binary { "binary" } else { "text" });
                        if let Err(error) = self.callback(&value) { self.report(error); }
                    } else if let Some((request_id, code)) = ack(&value) {
                        if ping.as_ref().is_some_and(|(id, _)| id == request_id) {
                            ping = None;
                            if code != 0 { return Ok(()); }
                        } else if let Some(entry) = self.feedback_pending.remove(request_id) {
                            if code != 0 && !entry.best_effort {
                                self.report(GatewayError::Unconfirmed);
                            }
                        } else if let Some((_, pending)) = self.media_pending.remove_entry(request_id) {
                            self.finish_media(pending, &value, code)?;
                        } else if let Some(pending) = self.pending.remove(request_id) {
                            self.finish_pending(pending, code == 0)?;
                        }
                    } else if value.get("cmd").and_then(Value::as_str) == Some("aibot_event_callback") {
                        eprintln!(
                            "anchor-wecom-gateway: non-message platform event received: {}",
                            frame_snippet(&value)
                        );
                    } else {
                        eprintln!(
                            "anchor-wecom-gateway: unrecognized platform frame: {}",
                            frame_snippet(&value)
                        );
                        self.report(GatewayError::Invalid("unrecognized platform frame"));
                    }
                },
                Some(request) = self.incoming.recv() => {
                    if !self.dispatch_send(socket, request).await? { return Ok(()); }
                },
                Some(line) = self.progress.recv() => {
                    self.record_progress(line);
                },
                Some((event, result)) = self.callbacks.next(), if !self.callbacks.is_empty() => {
                    if let Err(error) = self.callback_finished(event, result) {
                        eprintln!(
                            "anchor-wecom-gateway: callback settlement failed: {error}"
                        );
                        return Err(error);
                    }
                },
                _ = heartbeat.tick(), if ping.is_none() => {
                    let request_id = identifier("ping");
                    let value = json!({"cmd":"ping", "headers":{"req_id":request_id}});
                    if !write(socket, value.to_string(), self.config.timing.ack_timeout).await { return Ok(()); }
                    ping = Some((request_id, Instant::now() + self.config.timing.ack_timeout));
                },
                _ = tokio::time::sleep_until(next_deadline) => {
                    let now = Instant::now();
                    if ping.as_ref().is_some_and(|(_, deadline)| *deadline <= now) { return Ok(()); }
                    if self
                        .feedback_pending
                        .values()
                        .any(|entry| entry.deadline <= now && !entry.best_effort)
                    {
                        self.report(GatewayError::Unconfirmed);
                        return Ok(());
                    }
                    // An animation frame nobody acknowledged is decoration: drop
                    // it and keep the connection instead of failing closed.
                    self.feedback_pending
                        .retain(|_, entry| entry.deadline > now);
                    let expired_media: Vec<_> = self.media_pending.iter()
                        .filter(|(_, pending)| pending.deadline <= now)
                        .map(|(id, _)| id.clone()).collect();
                    for id in expired_media {
                        self.expire_media(&id)?;
                    }
                    let expired: Vec<_> = self.pending.iter().filter(|(_, pending)| pending.deadline <= now)
                        .map(|(id, _)| id.clone()).collect();
                    for id in expired {
                        if let Some(pending) = self.pending.remove(&id) { self.finish_pending(pending, false)?; }
                    }
                },
            }
        }
    }

    async fn dispatch_send(
        &mut self,
        socket: &mut Socket,
        request: SendRequest,
    ) -> Result<bool, GatewayError> {
        if request.response.is_closed() {
            return Ok(true);
        }
        let digest = request.digest();
        match self.ledger.previous("send", &request.request_id, &digest) {
            Ok(true) => {
                request.complete(true);
                return Ok(true);
            }
            Err(error) => {
                request.fail(error);
                return Ok(true);
            }
            Ok(false) => {}
        }
        if self.pending.len() >= MAX_IN_FLIGHT {
            request.fail(GatewayError::Invalid(
                "platform send concurrency limit reached",
            ));
            return Ok(true);
        }
        let wire_id = identifier("send");
        let value = frame(
            "aibot_send_msg",
            &wire_id,
            json!({"chatid": request.userid, "chat_type": 1, "msgtype":"markdown",
                "markdown":{"content":request.content}}),
        );
        let data = match encode(value) {
            Ok(data) => data,
            Err(error) => {
                request.fail(error);
                return Ok(true);
            }
        };
        match self
            .ledger
            .claim("send", &request.request_id, &digest, &wire_id)
        {
            Ok(true) => {}
            Ok(false) => {
                request.complete(true);
                return Ok(true);
            }
            Err(error) => {
                request.fail(error);
                return Ok(true);
            }
        }
        self.pending.insert(
            wire_id.clone(),
            PendingDelivery {
                wire_id: wire_id.clone(),
                event_id: None,
                request: Some(request),
                deadline: Instant::now() + self.config.timing.ack_timeout,
            },
        );
        Ok(write(socket, data, self.config.timing.ack_timeout).await)
    }

    async fn dispatch_ready(&mut self, socket: &mut Socket) -> Result<bool, GatewayError> {
        if !self.dispatch_progress(socket).await? {
            return Ok(false);
        }
        let replies = self
            .ledger
            .ready_replies(MAX_IN_FLIGHT.saturating_sub(self.pending.len()))?;
        for reply in replies {
            // Any outstanding update on this request id, including a decorative
            // animation frame, must be acknowledged first: acknowledgements are
            // keyed by request id, so sending the reply early could let a
            // frame's acknowledgement be mistaken for the reply's own.
            if self.feedback_pending.contains_key(&reply.callback_id) {
                continue;
            }
            if !crate::config::allowed(&self.config.inbound_users, &reply.sender_id) {
                self.ledger.finish_event(&reply.event_id, false)?;
                self.report(GatewayError::Invalid(
                    "saved reply sender is no longer allowed",
                ));
                continue;
            }
            if !self.ledger.is_latest_identity(
                &reply.event_id,
                &reply.sender_id,
                &reply.conversation_id,
            )? {
                self.ledger.suppress(&reply.event_id, None)?;
                continue;
            }
            // Queue reply images before the text so the text claim orders them;
            // media waits for the text acknowledgement before uploading.
            self.enqueue_reply_media(&reply);
            if !self.dispatch_reply(socket, reply).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn dispatch_reply(
        &mut self,
        socket: &mut Socket,
        reply: ReadyReply,
    ) -> Result<bool, GatewayError> {
        let value = frame(
            "aibot_respond_msg",
            &reply.callback_id,
            json!({"msgtype":"stream",
            "stream":{"id":stream_id(&reply.event_id), "content":reply.text, "finish":true}}),
        );
        let data = match encode(value) {
            Ok(data) => data,
            Err(error) => {
                self.ledger.finish_event(&reply.event_id, false)?;
                self.report(error);
                return Ok(true);
            }
        };
        let digest = digest(
            &serde_json::to_vec(&json!([
                reply.callback_id,
                reply.sender_id,
                reply.conversation_id,
                reply.text,
            ]))
            .map_err(|_| GatewayError::Ledger)?,
        );
        match self
            .ledger
            .claim("reply", &reply.event_id, &digest, &reply.callback_id)
        {
            Ok(true) => {}
            Ok(false) => {
                self.ledger.finish_event(&reply.event_id, true)?;
                return Ok(true);
            }
            Err(error) => {
                self.ledger.finish_event(&reply.event_id, false)?;
                self.report(error);
                return Ok(true);
            }
        }
        let pending_wire = reply.callback_id.clone();
        self.pending.insert(
            pending_wire.clone(),
            PendingDelivery {
                wire_id: reply.callback_id,
                event_id: Some(reply.event_id),
                request: None,
                deadline: Instant::now() + self.config.timing.ack_timeout,
            },
        );
        let sent = write(socket, data, self.config.timing.ack_timeout).await;
        eprintln!("anchor-wecom-gateway: stream update {pending_wire} finish=true reply");
        Ok(sent)
    }

    /// Advance reply-image uploads. One pass sends at most one frame per job;
    /// each frame waits for its correlated acknowledgement before the next step.
    async fn dispatch_media(&mut self, socket: &mut Socket) -> Result<bool, GatewayError> {
        let queued = self.media.len();
        let mut deferred = Vec::with_capacity(queued);
        for _ in 0..queued {
            let Some(mut job) = self.media.pop_front() else {
                break;
            };
            // Reply images and the text share one platform `req_id`, so their
            // acknowledgements are indistinguishable: keep a single media step
            // in flight per event, and wait until the text stops being pending.
            let busy = self
                .media_pending
                .values()
                .any(|pending| pending.event_id == job.event_id)
                || self.pending.contains_key(&job.callback_id);
            if busy || self.media_pending.len() >= MAX_MEDIA_IN_FLIGHT {
                deferred.push(job);
                continue;
            }
            if !self.ledger.is_latest_identity(
                &job.event_id,
                &job.sender_id,
                &job.conversation_id,
            )? {
                continue;
            }
            // A send that already happened, or whose outcome is unknown, must
            // never be repeated: skip the upload instead of spending platform
            // round trips only to refuse the send at the end.
            match self.ledger.previous(
                "image",
                &format!("{}:{}", job.event_id, job.index),
                &job.send_digest,
            ) {
                Ok(false) => {}
                Ok(true) => continue,
                Err(GatewayError::PreviousUnconfirmed) => {
                    self.report(GatewayError::PreviousUnconfirmed);
                    continue;
                }
                Err(error) => return Err(error),
            }
            match self.media_step(socket, &mut job).await? {
                StepOutcome::Sent => deferred.push(job),
                StepOutcome::Dropped => {}
                StepOutcome::Disconnected => {
                    deferred.push(job);
                    self.media.extend(deferred);
                    return Ok(false);
                }
            }
        }
        self.media.extend(deferred);
        Ok(true)
    }

    async fn media_step(
        &mut self,
        socket: &mut Socket,
        job: &mut MediaJob,
    ) -> Result<StepOutcome, GatewayError> {
        let ledger_wire = format!("{}:{}", job.callback_id, job.index);
        let (command, wire_id, body, step) = match job.stage {
            MediaStage::Init => {
                let wire_id = identifier("media-init");
                (
                    "aibot_upload_media_init",
                    wire_id,
                    json!({
                        "type": "image",
                        "filename": job.item.name,
                        "total_size": job.item.bytes.len(),
                        "total_chunks": job.item.chunk_count(),
                        "md5": job.item.md5,
                    }),
                    MediaStep::Init,
                )
            }
            MediaStage::Uploading {
                ref upload_id,
                next,
            } => {
                if next < job.item.chunk_count() {
                    let wire_id = identifier("media-chunk");
                    (
                        "aibot_upload_media_chunk",
                        wire_id,
                        json!({
                            "upload_id": upload_id,
                            "chunk_index": next,
                            "base64_data": STANDARD.encode(job.item.chunk(next)),
                        }),
                        MediaStep::Chunk,
                    )
                } else {
                    let wire_id = identifier("media-finish");
                    (
                        "aibot_upload_media_finish",
                        wire_id,
                        json!({"upload_id": upload_id}),
                        MediaStep::Finish,
                    )
                }
            }
            MediaStage::Sending { ref media_id } => {
                let request_id = format!("{}:{}", job.event_id, job.index);
                match self
                    .ledger
                    .claim("image", &request_id, &job.send_digest, &ledger_wire)
                {
                    Ok(true) => {}
                    Ok(false) => return Ok(StepOutcome::Dropped),
                    Err(GatewayError::PreviousUnconfirmed) => {
                        self.report(GatewayError::PreviousUnconfirmed);
                        return Ok(StepOutcome::Dropped);
                    }
                    Err(error) => return Err(error),
                }
                (
                    "aibot_respond_msg",
                    job.callback_id.clone(),
                    json!({"msgtype": "image", "image": {"media_id": media_id}}),
                    MediaStep::Send,
                )
            }
        };
        let data = encode_media(frame(command, &wire_id, body))?;
        let sent = write(socket, data, self.config.timing.ack_timeout).await;
        if super::diagnostics() {
            eprintln!(
                "anchor-wecom-gateway: media step {} for {}:{} written={sent}",
                command, job.event_id, job.index
            );
        }
        if !sent {
            return Ok(StepOutcome::Disconnected);
        }
        self.media_pending.insert(
            wire_id,
            MediaPending {
                event_id: job.event_id.clone(),
                index: job.index,
                step,
                ledger_wire,
                deadline: Instant::now() + self.config.timing.ack_timeout,
            },
        );
        Ok(StepOutcome::Sent)
    }
}

enum StepOutcome {
    Sent,
    Dropped,
    Disconnected,
}

async fn authenticate(
    socket: &mut Socket,
    config: &crate::GatewayConfig,
) -> Result<(), GatewayError> {
    let request_id = identifier("auth");
    let subscribe = frame(
        "aibot_subscribe",
        &request_id,
        json!({"bot_id":config.bot_id, "secret":config.secret}),
    );
    tokio::time::timeout(config.timing.ack_timeout, async {
        socket
            .send(Message::Text(subscribe.to_string().into()))
            .await
            .map_err(|_| GatewayError::Disconnected)?;
        loop {
            match socket.next().await {
                Some(Ok(message @ (Message::Text(_) | Message::Binary(_)))) => {
                    let value: Value = serde_json::from_slice(&message.into_data())
                        .map_err(|_| GatewayError::Disconnected)?;
                    if value.get("cmd").and_then(Value::as_str) == Some("aibot_msg_callback") {
                        continue;
                    }
                    if let Some((id, code)) = ack(&value)
                        && id == request_id
                    {
                        return if code == 0 {
                            Ok(())
                        } else {
                            eprintln!(
                                "anchor-wecom-gateway: platform rejected the subscription with errcode {code}"
                            );
                            Err(GatewayError::Disconnected)
                        };
                    }
                }
                Some(Ok(Message::Ping(_))) => socket
                    .flush()
                    .await
                    .map_err(|_| GatewayError::Disconnected)?,
                Some(Ok(Message::Pong(_))) => {}
                _ => return Err(GatewayError::Disconnected),
            }
        }
    })
    .await
    .map_err(|_| GatewayError::Disconnected)?
}

fn ack(value: &Value) -> Option<(&str, i64)> {
    Some((
        value.pointer("/headers/req_id")?.as_str()?,
        value.get("errcode")?.as_i64()?,
    ))
}

const MAX_LOG_FRAME_CHARS: usize = 2048;

/// Bounded, credential-free rendering of one platform frame for diagnostics.
fn frame_snippet(value: &Value) -> String {
    let text = value.to_string();
    let mut snippet: String = text.chars().take(MAX_LOG_FRAME_CHARS).collect();
    if text.chars().count() > MAX_LOG_FRAME_CHARS {
        snippet.push('…');
    }
    snippet
}

fn identifier(kind: &str) -> String {
    format!("anchor-{kind}-{}", uuid::Uuid::new_v4().simple())
}

fn encode(value: Value) -> Result<String, GatewayError> {
    let data = value.to_string();
    if data.len() > MAX_REQUEST_BYTES {
        return Err(GatewayError::Invalid(
            "encoded platform message exceeds transport limits",
        ));
    }
    Ok(data)
}

/// Media frames carry one bounded upload chunk, so they may exceed the
/// text-control frame limit without relaxing it for control messages.
fn encode_media(value: Value) -> Result<String, GatewayError> {
    let data = value.to_string();
    if data.len() > MAX_MEDIA_FRAME_BYTES {
        return Err(GatewayError::Invalid(
            "encoded media frame exceeds transport limits",
        ));
    }
    Ok(data)
}

async fn write(socket: &mut Socket, data: String, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, socket.send(Message::Text(data.into()))).await,
        Ok(Ok(()))
    )
}

async fn flush(socket: &mut Socket, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, socket.flush()).await,
        Ok(Ok(()))
    )
}
