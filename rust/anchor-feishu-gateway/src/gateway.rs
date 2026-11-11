use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anchor_channel_transport::{PrivateState, WebhookClient, allowed, digest, settlement_loop};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    net::{TcpStream, UnixListener},
    sync::{Notify, broadcast, oneshot, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};

use crate::{
    DeliveryFact, GatewayConfig, GatewayError,
    api::{Api, ClientConfig},
    protocol::{Frame, MAX_FRAME_BYTES, Reassembler, normalize_message},
};
use anchor_channel_transport::ledger::Ledger;

pub use anchor_channel_transport::{ConnectionStatus, stopping};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

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
        let mut state = PrivateState::acquire(&config.state_dir)?;
        let profile = digest(
            &serde_json::to_vec(&json!(["feishu", config.app_id, config.api_base_url]))
                .map_err(|_| GatewayError::Ledger)?,
        );
        let ledger = Arc::new(Ledger::open(&state.database_path(), &profile)?);
        let webhook =
            Arc::new(WebhookClient::new(config.webhook.take().ok_or(
                GatewayError::Invalid("Host channel webhook is required"),
            )?)?);
        let api = Arc::new(Api::new(&config)?);
        let listener =
            UnixListener::bind(state.socket_path()).map_err(|_| GatewayError::PrivateState)?;
        if let Err(error) = state.publish(&config.control_token) {
            state.cleanup();
            return Err(error);
        }
        let (status_tx, status) = watch::channel(ConnectionStatus::Connecting);
        let (stop, shutdown) = watch::channel(false);
        let (errors, _) = broadcast::channel(64);
        let config = Arc::new(config);
        let wake = Arc::new(WorkerSignal {
            wake: Notify::new(),
            ack_pending: AtomicBool::new(false),
        });
        let mut driver = Driver {
            config: config.clone(),
            api: api.clone(),
            ledger: ledger.clone(),
            wake: wake.clone(),
            status: status_tx.clone(),
            shutdown: shutdown.clone(),
            errors: errors.clone(),
            admissions: JoinSet::new(),
        };
        let mut worker = tokio::spawn(crate::worker::run(
            config.clone(),
            api.clone(),
            ledger.clone(),
            webhook.clone(),
            wake,
            shutdown.clone(),
            errors.clone(),
        ));
        let mut settlements = tokio::spawn(settlement_loop(
            ledger.clone(),
            webhook,
            shutdown.clone(),
            errors.clone(),
        ));
        let mut task_shutdown = shutdown.clone();
        let mut control = tokio::spawn(crate::control::run(
            listener,
            config.clone(),
            api,
            ledger.clone(),
            status.clone(),
            shutdown,
        ));
        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = stopping(&mut task_shutdown) => Ok(()),
                result = driver.run() => result,
                result = &mut worker => result.unwrap_or(Err(GatewayError::Stopped)),
                result = &mut control => result.unwrap_or(Err(GatewayError::Stopped)),
                _ = &mut settlements => Err(GatewayError::Stopped),
            };
            stopped.send_replace(true);
            // Timed-out admissions may still be committing. Keep the exclusive
            // directory lease until every blocking writer has actually exited.
            driver.admissions.shutdown().await;
            if !worker.is_finished() {
                let _ = worker.await;
            }
            if !control.is_finished() {
                let _ = control.await;
            }
            if !settlements.is_finished() {
                let _ = settlements.await;
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

struct Policy {
    heartbeat: Duration,
    reconnect_base: Duration,
    reconnect_max: Duration,
    reconnect_count: Option<u32>,
    reconnect_nonce: Duration,
}

impl Policy {
    fn new(config: &GatewayConfig) -> Self {
        Self {
            heartbeat: config.timing.heartbeat_interval,
            reconnect_base: config.timing.reconnect_base,
            reconnect_max: config.timing.reconnect_max,
            reconnect_count: None,
            reconnect_nonce: Duration::ZERO,
        }
    }

    fn apply(&mut self, value: ClientConfig) -> Result<(), GatewayError> {
        let invalid = || GatewayError::Invalid("Feishu connection configuration exceeds bounds");
        if let Some(value) = value.ping_interval {
            if !(1..=3600).contains(&value) {
                return Err(invalid());
            }
            self.heartbeat = Duration::from_secs(value);
        }
        if let Some(value) = value.reconnect_interval {
            if !(1..=3600).contains(&value) {
                return Err(invalid());
            }
            self.reconnect_base = Duration::from_secs(value);
            self.reconnect_max = self.reconnect_base;
        }
        if let Some(value) = value.reconnect_nonce {
            if value > 300 {
                return Err(invalid());
            }
            self.reconnect_nonce = Duration::from_secs(value);
        }
        if let Some(value) = value.reconnect_count {
            if !(-1..=1000).contains(&value) {
                return Err(invalid());
            }
            self.reconnect_count = u32::try_from(value).ok();
        }
        Ok(())
    }

    fn reconnect_delay(&self, attempt: u32) -> Duration {
        let base = self
            .reconnect_base
            .saturating_mul(1 << attempt.saturating_sub(1).min(16))
            .min(self.reconnect_max);
        if attempt == 1 && !self.reconnect_nonce.is_zero() {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos();
            base + Duration::from_millis(
                u64::from(nanos) % (self.reconnect_nonce.as_millis() as u64 + 1),
            )
        } else {
            base
        }
    }
}

struct Driver {
    config: Arc<GatewayConfig>,
    api: Arc<Api>,
    ledger: Arc<Ledger>,
    wake: Arc<WorkerSignal>,
    status: watch::Sender<ConnectionStatus>,
    shutdown: watch::Receiver<bool>,
    errors: broadcast::Sender<GatewayError>,
    admissions: JoinSet<()>,
}

pub(crate) struct WorkerSignal {
    pub wake: Notify,
    /// A scan can see a committed event before its ACK has reached the socket.
    /// Defer new workers across that short interval, including media downloads.
    pub ack_pending: AtomicBool,
}

impl Driver {
    async fn run(&mut self) -> Result<(), GatewayError> {
        let mut policy = Policy::new(&self.config);
        let mut attempts = 0u32;
        loop {
            self.status.send_replace(ConnectionStatus::Authenticating);
            let connection = tokio::select! {
                biased;
                _ = stopping(&mut self.shutdown) => return Ok(()),
                result = tokio::time::timeout(self.config.timing.connect_timeout, async {
                    let endpoint = self.api.endpoint().await?;
                    let limits = WebSocketConfig::default()
                        .max_message_size(Some(MAX_FRAME_BYTES)).max_frame_size(Some(MAX_FRAME_BYTES));
                    let (socket, _) = tokio_tungstenite::connect_async_with_config(&endpoint.url, Some(limits), false)
                        .await.map_err(|_| GatewayError::Disconnected)?;
                    Ok::<_, GatewayError>((socket, endpoint))
                }) => result.unwrap_or(Err(GatewayError::Disconnected)),
            };
            match connection {
                Ok((mut socket, endpoint)) => {
                    policy.apply(endpoint.config)?;
                    self.status.send_replace(ConnectionStatus::Authenticated);
                    attempts = 0;
                    let session = self
                        .session(&mut socket, endpoint.service_id, &mut policy)
                        .await;
                    if *self.shutdown.borrow() {
                        return Ok(());
                    }
                    if let Err(error) = session {
                        let _ = self.errors.send(error);
                    }
                }
                Err(error) => {
                    let _ = self.errors.send(error);
                }
            }
            if policy
                .reconnect_count
                .is_some_and(|count| attempts >= count)
            {
                return Err(GatewayError::Disconnected);
            }
            attempts = attempts.saturating_add(1);
            self.status.send_replace(ConnectionStatus::Reconnecting);
            tokio::select! {
                biased;
                _ = stopping(&mut self.shutdown) => return Ok(()),
                _ = tokio::time::sleep(policy.reconnect_delay(attempts)) => {},
            }
        }
    }

    async fn session(
        &mut self,
        socket: &mut Socket,
        service: i32,
        policy: &mut Policy,
    ) -> Result<(), GatewayError> {
        let mut partial = Reassembler::default();
        let mut ping = tokio::time::interval(policy.heartbeat);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_receive = tokio::time::Instant::now();
        loop {
            tokio::select! {
                biased;
                _ = stopping(&mut self.shutdown) => return Ok(()),
                wire = socket.next() => {
                    last_receive = tokio::time::Instant::now();
                    let bytes = match wire {
                        Some(Ok(Message::Binary(bytes))) => bytes,
                        Some(Ok(Message::Ping(_))) => {
                            tokio::time::timeout(Duration::from_secs(2), socket.flush()).await
                                .map_err(|_| GatewayError::Disconnected)?.map_err(|_| GatewayError::Disconnected)?;
                            continue;
                        }
                        Some(Ok(Message::Pong(_))) => continue,
                        Some(Ok(Message::Text(_))) => return Err(GatewayError::Invalid("Feishu requires binary frames")),
                        _ => return Err(GatewayError::Disconnected),
                    };
                    let frame = Frame::decode_binary(&bytes)?;
                    match (frame.method, frame.header("type")) {
                        (0, Some("pong")) if !frame.payload.is_empty() => {
                            let config = serde_json::from_slice(&frame.payload)
                                .map_err(|_| GatewayError::Invalid("invalid Feishu pong configuration"))?;
                            policy.apply(config)?;
                            ping = tokio::time::interval_at(tokio::time::Instant::now() + policy.heartbeat, policy.heartbeat);
                            ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        }
                        (1, Some("event")) => {
                            if let Some(payload) = partial.accept(&frame)? {
                                let started = tokio::time::Instant::now();
                                self.wake.ack_pending.store(true, Ordering::Release);
                                let outcome = self.admit(&payload, started + Duration::from_millis(800)).await;
                                let code = if outcome.is_ok() { 200 } else { 500 };
                                let ack = self.write_until(socket, frame.acknowledge(code, started.elapsed().as_millis()),
                                    started + Duration::from_secs(2)).await;
                                self.wake.ack_pending.store(false, Ordering::Release);
                                ack?;
                                match outcome {
                                    Ok(true) => self.wake.wake.notify_one(),
                                    Ok(false) => {},
                                    Err(error) => { let _ = self.errors.send(error); },
                                }
                            }
                        }
                        _ => {},
                    }
                }
                _ = ping.tick() => {
                    if last_receive.elapsed() > policy.heartbeat.saturating_mul(2) + Duration::from_secs(5) {
                        return Err(GatewayError::Disconnected);
                    }
                    self.write(socket, Frame::ping(service)).await?;
                }
            }
        }
    }

    async fn admit(
        &mut self,
        bytes: &[u8],
        deadline: tokio::time::Instant,
    ) -> Result<bool, GatewayError> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|_| GatewayError::Invalid("invalid Feishu event JSON"))?;
        let Some(event) = normalize_message(&value, &self.config.app_id)? else {
            return Ok(false);
        };
        if !allowed(&self.config.inbound_users, &event.sender_id)
            || !allowed(&self.config.send_users, &event.sender_id)
        {
            return Ok(false);
        }
        while self.admissions.try_join_next().is_some() {}
        if self.admissions.len() >= 2 {
            return Err(GatewayError::Invalid(
                "gateway admission is temporarily busy",
            ));
        }
        let ledger = self.ledger.clone();
        let (send, response) = oneshot::channel();
        self.admissions.spawn_blocking(move || {
            let _ = send.send(ledger.admit(&event));
        });
        // A late successful commit is rescued by the worker's durable scan.
        // Feishu retries get the same message-id deduplication decision.
        tokio::time::timeout_at(deadline, response)
            .await
            .map_err(|_| GatewayError::Invalid("gateway durable admission timed out"))?
            .map_err(|_| GatewayError::Stopped)?
    }

    async fn write(&self, socket: &mut Socket, frame: Frame) -> Result<(), GatewayError> {
        self.write_until(
            socket,
            frame,
            tokio::time::Instant::now() + Duration::from_secs(2),
        )
        .await
    }

    async fn write_until(
        &self,
        socket: &mut Socket,
        frame: Frame,
        deadline: tokio::time::Instant,
    ) -> Result<(), GatewayError> {
        tokio::time::timeout_at(
            deadline,
            socket.send(Message::Binary(frame.encode_binary().into())),
        )
        .await
        .map_err(|_| GatewayError::Disconnected)?
        .map_err(|_| GatewayError::Disconnected)
    }
}
