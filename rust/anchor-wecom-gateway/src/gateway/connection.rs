use std::time::Duration;

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

use super::{ConnectionStatus, Driver, MAX_IN_FLIGHT, PendingDelivery, SendRequest, stopping};
use crate::{
    GatewayError, MAX_REQUEST_BYTES,
    ledger::ReadyReply,
    protocol::{digest, frame},
    stream_id,
};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

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
                            .max_message_size(Some(MAX_REQUEST_BYTES))
                            .max_frame_size(Some(MAX_REQUEST_BYTES)),
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
                    self.status.send_replace(ConnectionStatus::Authenticated);
                    delay = self.config.timing.reconnect_base;
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
            let next_deadline = self
                .pending
                .values()
                .map(|pending| pending.deadline)
                .chain(ping.iter().map(|(_, deadline)| *deadline))
                .min()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(180));
            tokio::select! {
                _ = stopping(&mut self.shutdown) => return Ok(()),
                incoming = socket.next() => {
                    let value = match incoming {
                        Some(Ok(Message::Text(text))) => serde_json::from_str::<Value>(&text).ok(),
                        Some(Ok(Message::Ping(_))) => {
                            if !flush(socket, self.config.timing.ack_timeout).await { return Ok(()); }
                            continue;
                        },
                        Some(Ok(Message::Pong(_))) => continue,
                        _ => return Ok(()),
                    };
                    let Some(value) = value.filter(Value::is_object) else {
                        self.report(GatewayError::Invalid("invalid platform JSON frame"));
                        continue;
                    };
                    if value.get("cmd").and_then(Value::as_str) == Some("aibot_msg_callback") {
                        if let Err(error) = self.callback(&value) { self.report(error); }
                    } else if let Some((request_id, code)) = ack(&value) {
                        if ping.as_ref().is_some_and(|(id, _)| id == request_id) {
                            ping = None;
                            if code != 0 { return Ok(()); }
                        } else if let Some(pending) = self.pending.remove(request_id) {
                            self.finish_pending(pending, code == 0)?;
                        }
                    }
                },
                Some(request) = self.incoming.recv() => {
                    if !self.dispatch_send(socket, request).await? { return Ok(()); }
                },
                Some((event, result)) = self.callbacks.next(), if !self.callbacks.is_empty() => {
                    self.callback_finished(event, result)?;
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
        let replies = self
            .ledger
            .ready_replies(MAX_IN_FLIGHT.saturating_sub(self.pending.len()))?;
        for reply in replies {
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
        self.pending.insert(
            reply.callback_id.clone(),
            PendingDelivery {
                wire_id: reply.callback_id,
                event_id: Some(reply.event_id),
                request: None,
                deadline: Instant::now() + self.config.timing.ack_timeout,
            },
        );
        Ok(write(socket, data, self.config.timing.ack_timeout).await)
    }
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
                Some(Ok(Message::Text(text))) => {
                    let value: Value =
                        serde_json::from_str(&text).map_err(|_| GatewayError::Disconnected)?;
                    if value.get("cmd").and_then(Value::as_str) == Some("aibot_msg_callback") {
                        continue;
                    }
                    if let Some((id, code)) = ack(&value)
                        && id == request_id
                    {
                        return if code == 0 {
                            Ok(())
                        } else {
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
