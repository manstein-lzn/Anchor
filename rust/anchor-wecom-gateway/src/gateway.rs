mod connection;
mod control;

use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
    time::Duration,
};

use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::{
    net::UnixListener,
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
};

use crate::{
    ChannelEvent, DeliveryFact, GatewayConfig, GatewayError,
    ledger::{Ledger, SettlementStatus, current_time_millis},
    private_state::PrivateState,
    protocol::digest,
    webhook::{Webhook, WebhookReply},
};

const MAX_IN_FLIGHT: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    Connecting,
    Authenticating,
    Authenticated,
    Reconnecting,
    Stopped,
}

pub struct Gateway;

pub struct RunningGateway {
    status: watch::Receiver<ConnectionStatus>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<Result<(), GatewayError>>>,
    ledger: Arc<Ledger>,
    errors: broadcast::Sender<GatewayError>,
}

impl Gateway {
    pub async fn start(mut config: GatewayConfig) -> Result<RunningGateway, GatewayError> {
        config.validate()?;
        let mut state = PrivateState::acquire(&config)?;
        let profile = digest(
            &serde_json::to_vec(&json!([config.bot_id, config.ws_url]))
                .map_err(|_| GatewayError::Ledger)?,
        );
        let ledger = Arc::new(Ledger::open(&state.database_path(), &profile)?);
        let webhook = config
            .webhook
            .take()
            .map(Webhook::new)
            .transpose()?
            .map(Arc::new);
        let listener =
            UnixListener::bind(state.socket_path()).map_err(|_| GatewayError::PrivateState)?;
        if let Err(error) = state.publish(&config) {
            state.cleanup();
            return Err(error);
        }
        let (status_tx, status) = watch::channel(ConnectionStatus::Connecting);
        let (stop, shutdown) = watch::channel(false);
        let (errors, _) = broadcast::channel(64);
        let (send_tx, incoming) = mpsc::channel(MAX_IN_FLIGHT);
        let settlement_worker = webhook.clone().map(|webhook| {
            let worker_ledger = ledger.clone();
            let worker_shutdown = shutdown.clone();
            let worker_errors = errors.clone();
            tokio::spawn(async move {
                settlement_loop(worker_ledger, webhook, worker_shutdown, worker_errors).await;
            })
        });
        let control = control::Server {
            listener,
            token: config.control_token.clone(),
            users: config.send_users.clone(),
            send: send_tx,
            shutdown: shutdown.clone(),
            status: status.clone(),
        };
        let driver = Driver {
            config,
            ledger: ledger.clone(),
            webhook,
            incoming,
            shutdown,
            status: status_tx.clone(),
            errors: errors.clone(),
            callbacks: FuturesUnordered::new(),
            pending: BTreeMap::new(),
        };
        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            let mut control_task = tokio::spawn(control.run());
            let result = tokio::select! {
                result = driver.run() => result,
                result = &mut control_task => result.unwrap_or(Err(GatewayError::Stopped)),
            };
            stopped.send_replace(true);
            if !control_task.is_finished() {
                let _ = control_task.await;
            }
            if let Some(worker) = settlement_worker {
                let _ = worker.await;
            }
            state.cleanup();
            status_tx.send_replace(ConnectionStatus::Stopped);
            result
        });
        Ok(RunningGateway {
            status,
            stop,
            task: Some(task),
            ledger,
            errors,
        })
    }
}

impl RunningGateway {
    pub fn status(&self) -> ConnectionStatus {
        *self.status.borrow()
    }

    pub fn subscribe_errors(&self) -> broadcast::Receiver<GatewayError> {
        self.errors.subscribe()
    }

    pub fn delivery_facts(&self) -> Result<Vec<DeliveryFact>, GatewayError> {
        self.ledger.facts()
    }

    pub async fn wait_authenticated(&self, duration: Duration) -> Result<(), GatewayError> {
        let mut status = self.status.clone();
        tokio::time::timeout(duration, async {
            loop {
                match *status.borrow_and_update() {
                    ConnectionStatus::Authenticated => return Ok(()),
                    ConnectionStatus::Stopped => return Err(GatewayError::Stopped),
                    _ => {}
                }
                status.changed().await.map_err(|_| GatewayError::Stopped)?;
            }
        })
        .await
        .map_err(|_| GatewayError::Disconnected)?
    }

    pub async fn shutdown(mut self) -> Result<(), GatewayError> {
        self.stop.send_replace(true);
        self.task
            .take()
            .ok_or(GatewayError::Stopped)?
            .await
            .map_err(|_| GatewayError::Stopped)?
    }
}

impl Drop for RunningGateway {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

struct SendRequest {
    request_id: String,
    userid: String,
    content: String,
    response: oneshot::Sender<Result<Value, GatewayError>>,
}

impl SendRequest {
    fn digest(&self) -> String {
        digest(&serde_json::to_vec(&json!([self.userid, self.content])).expect("serializable send"))
    }

    fn complete(self, duplicate: bool) {
        let mut result = json!({"accepted": true, "request_id": self.request_id});
        if duplicate {
            result["duplicate"] = json!(true);
        }
        let _ = self.response.send(Ok(result));
    }

    fn fail(self, error: GatewayError) {
        let _ = self.response.send(Err(error));
    }
}

struct PendingDelivery {
    wire_id: String,
    event_id: Option<String>,
    request: Option<SendRequest>,
    deadline: tokio::time::Instant,
}

type Callback = BoxFuture<'static, (ChannelEvent, Result<WebhookReply, GatewayError>)>;

struct Driver {
    config: GatewayConfig,
    ledger: Arc<Ledger>,
    webhook: Option<Arc<Webhook>>,
    incoming: mpsc::Receiver<SendRequest>,
    shutdown: watch::Receiver<bool>,
    status: watch::Sender<ConnectionStatus>,
    errors: broadcast::Sender<GatewayError>,
    callbacks: FuturesUnordered<Callback>,
    pending: BTreeMap<String, PendingDelivery>,
}

impl Driver {
    fn report(&self, error: GatewayError) {
        let _ = self.errors.send(error);
    }

    fn callback(&mut self, value: &Value) -> Result<(), GatewayError> {
        let Some(event) = crate::normalize_message(value)? else {
            return Ok(());
        };
        if !crate::config::allowed(&self.config.inbound_users, &event.sender_id) {
            return Err(GatewayError::Invalid("callback sender is not allowed"));
        }
        let Some(webhook) = self.webhook.clone() else {
            return Err(GatewayError::Invalid("inbound webhook is not configured"));
        };
        if self.callbacks.len() >= MAX_IN_FLIGHT {
            return Err(GatewayError::Invalid(
                "inbound webhook concurrency limit reached",
            ));
        }
        if self.ledger.admit(&event)? {
            self.callbacks.push(
                async move {
                    let result = webhook.handle(&event).await;
                    (event, result)
                }
                .boxed(),
            );
        }
        Ok(())
    }

    fn callback_finished(
        &self,
        event: ChannelEvent,
        result: Result<WebhookReply, GatewayError>,
    ) -> Result<(), GatewayError> {
        match result {
            Ok(reply) if reply.superseded => self
                .ledger
                .suppress(&event.event_id, reply.receipt.as_ref()),
            Ok(reply) => match reply.text {
                Some(text) if self.ledger.is_latest(&event)? => {
                    self.ledger
                        .ready(&event.event_id, &text, reply.receipt.as_ref())
                }
                Some(_) => self
                    .ledger
                    .suppress(&event.event_id, reply.receipt.as_ref()),
                None => self.ledger.finish_event(&event.event_id, true),
            },
            Err(error) => {
                self.ledger.finish_event(&event.event_id, false)?;
                self.report(error);
                Ok(())
            }
        }
    }

    fn reject_disconnected(&self, request: SendRequest) {
        if request.response.is_closed() {
            return;
        }
        match self
            .ledger
            .previous("send", &request.request_id, &request.digest())
        {
            Ok(true) => request.complete(true),
            Ok(false) => request.fail(GatewayError::Disconnected),
            Err(error) => request.fail(error),
        }
    }

    fn abandon_pending(&mut self) -> Result<(), GatewayError> {
        for (_, pending) in std::mem::take(&mut self.pending) {
            self.finish_pending(pending, false)?;
        }
        Ok(())
    }

    fn finish_pending(
        &self,
        pending: PendingDelivery,
        confirmed: bool,
    ) -> Result<(), GatewayError> {
        if let Some(event_id) = pending.event_id {
            self.ledger.finish_reply(
                &event_id,
                &pending.wire_id,
                if confirmed {
                    SettlementStatus::Confirmed
                } else {
                    SettlementStatus::Unknown
                },
            )?;
        } else if confirmed {
            self.ledger.confirm(&pending.wire_id)?;
        }
        if let Some(request) = pending.request {
            if confirmed {
                request.complete(false);
            } else {
                request.fail(GatewayError::Unconfirmed);
            }
        }
        if !confirmed {
            self.report(GatewayError::Unconfirmed);
        }
        Ok(())
    }
}

async fn settlement_loop(
    ledger: Arc<Ledger>,
    webhook: Arc<Webhook>,
    mut shutdown: watch::Receiver<bool>,
    errors: broadcast::Sender<GatewayError>,
) {
    let mut requests =
        FuturesUnordered::<BoxFuture<'static, (String, Result<(), GatewayError>)>>::new();
    let mut active = HashSet::new();
    let mut retry = tokio::time::interval(Duration::from_millis(100));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let capacity = MAX_IN_FLIGHT.saturating_sub(active.len());
        if capacity > 0 {
            match current_time_millis()
                .and_then(|now| ledger.pending_settlements(now, MAX_IN_FLIGHT + active.len()))
            {
                Ok(settlements) => {
                    for settlement in settlements {
                        if active.contains(&settlement.receipt.key) {
                            continue;
                        }
                        let key = settlement.receipt.key.clone();
                        let request_webhook = webhook.clone();
                        active.insert(key.clone());
                        requests.push(
                            async move { (key, request_webhook.settle(&settlement).await) }.boxed(),
                        );
                        if active.len() >= MAX_IN_FLIGHT {
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = errors.send(error);
                }
            }
        }
        tokio::select! {
            _ = stopping(&mut shutdown) => break,
            result = requests.next(), if !requests.is_empty() => {
                if let Some((key, result)) = result {
                    active.remove(&key);
                    let succeeded = result.is_ok();
                    if let Err(error) = ledger.finish_settlement(&key, succeeded) {
                        let _ = errors.send(error);
                    }
                    if let Err(error) = result {
                        let _ = errors.send(error);
                    }
                }
            }
            _ = retry.tick() => {}
        }
    }
}

async fn stopping(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stopped| *stopped).await;
}
