mod connection;
mod control;
mod feedback;
mod progress;
mod status;

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
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
    media::{ImageItem, MediaFetcher, media_send_digest, reply_images},
    private_state::PrivateState,
    protocol::digest,
    webhook::{Webhook, WebhookReply},
};

const MAX_IN_FLIGHT: usize = 32;
/// Concurrent media uploads. Uploads interleave with text delivery on the same
/// connection, so they stay well below the text concurrency limit.
const MAX_MEDIA_IN_FLIGHT: usize = 4;
/// In-session retries for one retryable upload step before the image is
/// abandoned. A platform send is never retried in-session.
const MAX_MEDIA_ATTEMPTS: u32 = 3;

/// Opt-in transport diagnostics. Off by default so routine reconnect and
/// unconfirmed-delivery reports never flood the service log.
pub(super) fn diagnostics() -> bool {
    std::env::var("ANCHOR_WECOM_DEBUG").as_deref() == Ok("1")
}

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
        let (progress_tx, progress) = mpsc::unbounded_channel();
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
        let mut driver = Driver {
            config,
            ledger: ledger.clone(),
            webhook,
            fetcher: Arc::new(MediaFetcher::new()?),
            incoming,
            shutdown,
            status: status_tx.clone(),
            errors: errors.clone(),
            callbacks: FuturesUnordered::new(),
            feedback: VecDeque::new(),
            feedback_pending: BTreeMap::new(),
            acks: Vec::new(),
            open_bubbles: BTreeMap::new(),
            progress,
            progress_tx,
            subscriptions: BTreeMap::new(),
            pending: BTreeMap::new(),
            media: VecDeque::new(),
            media_pending: BTreeMap::new(),
            recovery: VecDeque::new(),
        };
        driver.recover_unknown()?;
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

struct Feedback {
    event: ChannelEvent,
    finish: bool,
}

/// One accepted message waiting out its acknowledgement delay. A newer message
/// for the same conversation replaces it, so a burst shows one bubble.
struct ArmedAck {
    event: ChannelEvent,
    deadline: tokio::time::Instant,
    /// Latest observed status, shown instead of the static placeholder when the
    /// delay elapses. A burst therefore opens exactly one bubble, and it opens
    /// already saying what is happening.
    line: Option<String>,
}

/// The one progress bubble currently open for a conversation.
pub(super) struct OpenBubble {
    pub(super) event_id: String,
    pub(super) request_id: String,
    pub(super) stream_id: String,
    /// Status line waiting for the throttle window, if any.
    pub(super) pending: Option<String>,
    /// The status line the bubble currently shows, without animation dots.
    pub(super) content: String,
    /// Animation frames already drawn for that line.
    pub(super) frames: usize,
    /// When the next status line may go out (the throttle window).
    pub(super) next_update: tokio::time::Instant,
    /// When the next animation frame of the shown line is due.
    pub(super) next_animation: tokio::time::Instant,
    pub(super) updates: usize,
}

/// An update we are waiting for the platform to acknowledge.
#[derive(Clone, Copy)]
pub(super) struct AwaitingAck {
    pub(super) deadline: tokio::time::Instant,
    /// Animation frames are best effort: a late or lost acknowledgement only
    /// drops the entry. A strict entry (a status line, a notice or the reply
    /// itself) keeps the transport's fail-closed behaviour.
    pub(super) best_effort: bool,
}

/// One status line the Host projection observed for an admitted event.
pub(super) struct ProgressLine {
    pub(super) event_id: String,
    pub(super) sender_id: String,
    pub(super) conversation_id: String,
    pub(super) content: String,
    /// Stable category token, absent when the Host predates it.
    pub(super) category: Option<String>,
    /// 1-based tool step, absent for guidance lines.
    pub(super) step: Option<u32>,
    pub(super) settled: bool,
}

/// A live progress subscription for one admitted event.
struct Subscription {
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Subscription {
    fn stop(self) {
        let _ = self.cancel.send(true);
        self.task.abort();
    }
}

impl Drop for Subscription {
    /// A subscription must not outlive its handle: the transport stops, the
    /// stream stops. `stop` does the same, so an explicit stop is never needed
    /// for correctness — only for saying why.
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
        self.task.abort();
    }
}

/// One reply image moving through the platform media protocol. Upload is
/// retryable and invisible to the user; the send is claimed durably first so an
/// interrupted send is never repeated.
struct MediaJob {
    event_id: String,
    callback_id: String,
    sender_id: String,
    conversation_id: String,
    index: usize,
    item: ImageItem,
    send_digest: String,
    attempts: u32,
    stage: MediaStage,
}

enum MediaStage {
    Init,
    Uploading { upload_id: String, next: usize },
    Sending { media_id: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MediaStep {
    Init,
    Chunk,
    Finish,
    Send,
}

struct MediaPending {
    event_id: String,
    index: usize,
    step: MediaStep,
    /// Durable delivery identity for this platform send. The image message
    /// reuses the callback's platform `req_id`, which the text reply already
    /// owns, so the ledger keys the send by callback and image index.
    ledger_wire: String,
    deadline: tokio::time::Instant,
}

type Callback = BoxFuture<'static, (ChannelEvent, Result<WebhookReply, GatewayError>)>;

struct Driver {
    config: GatewayConfig,
    ledger: Arc<Ledger>,
    webhook: Option<Arc<Webhook>>,
    fetcher: Arc<MediaFetcher>,
    incoming: mpsc::Receiver<SendRequest>,
    shutdown: watch::Receiver<bool>,
    status: watch::Sender<ConnectionStatus>,
    errors: broadcast::Sender<GatewayError>,
    callbacks: FuturesUnordered<Callback>,
    feedback: VecDeque<Feedback>,
    feedback_pending: BTreeMap<String, AwaitingAck>,
    acks: Vec<ArmedAck>,
    open_bubbles: BTreeMap<(String, String), OpenBubble>,
    progress: mpsc::UnboundedReceiver<ProgressLine>,
    progress_tx: mpsc::UnboundedSender<ProgressLine>,
    subscriptions: BTreeMap<String, Subscription>,
    pending: BTreeMap<String, PendingDelivery>,
    media: VecDeque<MediaJob>,
    media_pending: BTreeMap<String, MediaPending>,
    recovery: VecDeque<ChannelEvent>,
}

impl Driver {
    fn report(&self, error: GatewayError) {
        if diagnostics() {
            eprintln!("anchor-wecom-gateway: transport error: {error}");
        }
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
        if self.callbacks.len() + self.feedback.len() + self.feedback_pending.len() >= MAX_IN_FLIGHT
        {
            return Err(GatewayError::Invalid(
                "inbound webhook concurrency limit reached",
            ));
        }
        if self.ledger.admit(&event)? {
            if diagnostics() {
                eprintln!("anchor-wecom-gateway: callback admitted {}", event.event_id);
            }
            if self.config.progress {
                self.arm_ack(event.clone());
                self.subscribe_progress(&event);
            }
            self.enqueue_callback(event, webhook);
        } else if diagnostics() {
            eprintln!(
                "anchor-wecom-gateway: callback already admitted {}",
                event.event_id
            );
        }
        Ok(())
    }

    /// Fetch platform media before the Host sees the event: the temporary
    /// download URL stays in this process, and the Host receives only the
    /// decrypted bytes its attachment contract already defines.
    /// Arm (or re-arm) the one acknowledgement for this conversation.
    fn arm_ack(&mut self, event: ChannelEvent) {
        // Acks are kept per message: arrival order does not say which turn the
        // Host is really running (a message whose media had to be downloaded
        // first reaches it later and can take over). Replacing an earlier ack
        // silenced the turn that was actually working. The gates in
        // `show_due_acknowledgements` — still processing, and the live turn
        // after the Host's cancellations — pick the single bubble instead.
        if self
            .acks
            .iter()
            .any(|ack| ack.event.event_id == event.event_id)
        {
            return;
        }
        self.acks.push(ArmedAck {
            event,
            deadline: tokio::time::Instant::now() + self.config.timing.ack_delay,
            line: None,
        });
    }

    /// Subscribe to the read-only progress projection for an admitted event.
    ///
    /// Every admitted event keeps its own stream. Callback arrival order does
    /// not say which turn the Host is really running — a message whose media had
    /// to be downloaded first reaches the Host later and can take over, which
    /// `arm_ack` already accounts for — so a newer callback must never stop an
    /// older event's subscription. Doing that silenced the turn that was
    /// actually working while the superseded turn kept the only live stream.
    ///
    /// A subscription therefore ends only where the truth is known: the Host
    /// reported the turn settled, the event's own callback settled (including
    /// the Host's explicit `superseded` answer, which stops it in
    /// `callback_finished`), the bubble was closed as superseded, the progress
    /// connection ended by itself, or the transport stopped. Progress for an
    /// event without an open bubble is dropped by `record_progress`, so an
    /// extra live stream can never open or feed the wrong bubble.
    fn subscribe_progress(&mut self, event: &ChannelEvent) {
        // A stream that already ended leaves nothing to stop. Keeping its entry
        // would only make the map grow with every finished round.
        self.subscriptions
            .retain(|_, subscription| !subscription.task.is_finished());
        let Some(url) = self.config.progress_url.clone() else {
            return;
        };
        let (cancel, cancelled) = watch::channel(false);
        // `Gateway::start` takes the webhook config out of `self.config`, so the
        // key must come from the live webhook: reading it from the config sent
        // the subscription unauthenticated and the Host answered 401.
        let api_key = self.webhook.as_ref().map(|hook| hook.api_key().to_owned());
        let task = progress::subscribe(
            url,
            api_key,
            event.event_id.clone(),
            event.sender_id.clone(),
            event.conversation_id.clone(),
            self.progress_tx.clone(),
            cancelled,
        );
        self.subscriptions
            .insert(event.event_id.clone(), Subscription { cancel, task });
    }

    /// Whether an update that must be acknowledged is still in flight. Animation
    /// frames do not count: they are decoration, and waiting for one would stall
    /// the line behind a frame that nobody misses.
    pub(super) fn strict_pending(&self, request_id: &str) -> bool {
        self.feedback_pending
            .get(request_id)
            .is_some_and(|entry| !entry.best_effort)
    }

    fn cancel_progress(&mut self, event_id: &str) {
        if let Some(subscription) = self.subscriptions.remove(event_id) {
            subscription.stop();
        }
    }

    /// Record one observed status line. A line for the open bubble waits for the
    /// throttle window; a line for a message that has not shown its bubble yet
    /// becomes the content that bubble opens with; anything else is ignored.
    pub(super) fn record_progress(&mut self, line: ProgressLine) {
        if line.settled {
            self.cancel_progress(&line.event_id);
            return;
        }
        let key = (line.sender_id.clone(), line.conversation_id.clone());
        let content = status::render(line.category.as_deref(), line.step, &line.content);
        if let Some(bubble) = self.open_bubbles.get_mut(&key)
            && bubble.event_id == line.event_id
        {
            bubble.pending = Some(content);
            return;
        }
        if let Some(ack) = self
            .acks
            .iter_mut()
            .find(|ack| ack.event.event_id == line.event_id)
        {
            ack.line = Some(content);
        }
    }

    fn enqueue_callback(&mut self, event: ChannelEvent, webhook: Arc<Webhook>) {
        let fetcher = self.fetcher.clone();
        self.callbacks.push(
            async move {
                let result = match fetcher.fetch_all(&event.attachments).await {
                    Ok(attachments) => {
                        let mut enriched = event.clone();
                        enriched.attachments = attachments;
                        webhook.handle(&enriched).await
                    }
                    Err(error) => Err(error),
                };
                (event, result)
            }
            .boxed(),
        );
    }

    fn recover_unknown(&mut self) -> Result<(), GatewayError> {
        if self.webhook.is_none() {
            return Ok(());
        }
        let now = current_time_millis()?;
        let window = self.recovery_window_millis();
        let retired = self.ledger.retire_stale_inbounds(now, window)?;
        if retired > 0 {
            eprintln!("anchor-wecom-gateway: retired {retired} stale undelivered inbound events");
        }
        self.recovery
            .extend(self.ledger.retryable_events(now, window)?);
        self.pump_recovery();
        self.recover_media()
    }

    fn recovery_window_millis(&self) -> i64 {
        self.config
            .recovery_window
            .as_millis()
            .min(i64::MAX as u128) as i64
    }

    /// Re-queue reply images whose text reply already settled. An uncertain text
    /// reply (unknown delivery) is never completed with a late image, and a
    /// per-image claim still prevents resending an uncertain platform send.
    fn recover_media(&mut self) -> Result<(), GatewayError> {
        for reply in self.ledger.pending_media(MAX_IN_FLIGHT)? {
            let Some(items) = reply.items.as_deref() else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(items) else {
                continue;
            };
            let Ok(images) = reply_images(Some(&value)) else {
                continue;
            };
            for (index, item) in images.into_iter().enumerate() {
                self.enqueue_media(&reply, index, item);
            }
        }
        Ok(())
    }

    fn enqueue_media(&mut self, reply: &crate::ledger::ReadyReply, index: usize, item: ImageItem) {
        if self
            .media
            .iter()
            .any(|job| job.event_id == reply.event_id && job.index == index)
            || self
                .media_pending
                .values()
                .any(|pending| pending.event_id == reply.event_id && pending.index == index)
        {
            return;
        }
        let Ok(send_digest) = media_send_digest(&reply.sender_id, &reply.conversation_id, &item)
        else {
            return;
        };
        self.media.push_back(MediaJob {
            event_id: reply.event_id.clone(),
            callback_id: reply.callback_id.clone(),
            sender_id: reply.sender_id.clone(),
            conversation_id: reply.conversation_id.clone(),
            index,
            item,
            send_digest,
            attempts: 0,
            stage: MediaStage::Init,
        });
    }

    fn enqueue_reply_media(&mut self, reply: &crate::ledger::ReadyReply) {
        let Some(items) = reply.items.as_deref() else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(items) else {
            return;
        };
        let Ok(images) = reply_images(Some(&value)) else {
            return;
        };
        for (index, item) in images.into_iter().enumerate() {
            self.enqueue_media(reply, index, item);
        }
    }

    /// Apply one acknowledged media step. Upload steps advance the job; the
    /// final image send confirms its durable claim and ends the job.
    fn finish_media(
        &mut self,
        pending: MediaPending,
        value: &serde_json::Value,
        code: i64,
    ) -> Result<(), GatewayError> {
        let position = self
            .media
            .iter()
            .position(|job| job.event_id == pending.event_id && job.index == pending.index);
        if code != 0 {
            self.report(GatewayError::Unconfirmed);
            if let Some(position) = position {
                self.retry_or_drop_media(position);
            }
            return Ok(());
        }
        let Some(position) = position else {
            return Ok(());
        };
        match pending.step {
            MediaStep::Init => match media_identifier(value, "upload_id") {
                Some(upload_id) => {
                    self.media[position].stage = MediaStage::Uploading { upload_id, next: 0 };
                }
                None => self.retry_or_drop_media(position),
            },
            MediaStep::Chunk => {
                if let MediaStage::Uploading { upload_id, next } = &self.media[position].stage {
                    self.media[position].stage = MediaStage::Uploading {
                        upload_id: upload_id.clone(),
                        next: next + 1,
                    };
                }
            }
            MediaStep::Finish => match media_identifier(value, "media_id") {
                Some(media_id) => {
                    self.media[position].stage = MediaStage::Sending { media_id };
                }
                None => self.retry_or_drop_media(position),
            },
            MediaStep::Send => {
                self.ledger.confirm(&pending.ledger_wire)?;
                self.media.remove(position);
            }
        }
        Ok(())
    }

    fn retry_or_drop_media(&mut self, position: usize) {
        let job = &mut self.media[position];
        job.attempts += 1;
        if job.attempts < MAX_MEDIA_ATTEMPTS {
            job.stage = MediaStage::Init;
        } else {
            self.media.remove(position);
        }
    }

    /// Drop a media step that never completed, without resending the send.
    fn expire_media(&mut self, wire_id: &str) -> Result<(), GatewayError> {
        let Some(pending) = self.media_pending.remove(wire_id) else {
            return Ok(());
        };
        self.report(GatewayError::Unconfirmed);
        if pending.step == MediaStep::Send {
            self.media
                .retain(|job| job.event_id != pending.event_id || job.index != pending.index);
            return Ok(());
        }
        if let Some(position) = self
            .media
            .iter()
            .position(|job| job.event_id == pending.event_id && job.index == pending.index)
        {
            self.retry_or_drop_media(position);
        }
        Ok(())
    }

    fn pump_recovery(&mut self) {
        let Some(webhook) = self.webhook.clone() else {
            return;
        };
        while self.callbacks.len() < MAX_IN_FLIGHT {
            let Some(event) = self.recovery.pop_front() else {
                break;
            };
            self.enqueue_callback(event, webhook.clone());
        }
    }

    fn callback_finished(
        &mut self,
        event: ChannelEvent,
        result: Result<WebhookReply, GatewayError>,
    ) -> Result<(), GatewayError> {
        // The reply or failure notice finishes the same stream the progress
        // bubble uses, so neither is left open.
        self.acks.retain(|ack| ack.event.event_id != event.event_id);
        self.open_bubbles
            .retain(|_, bubble| bubble.event_id != event.event_id);
        self.cancel_progress(&event.event_id);
        if diagnostics() {
            let summary = match &result {
                Ok(reply) => format!(
                    "text={} items={} superseded={}",
                    reply.text.is_some(),
                    reply.items.len(),
                    reply.superseded
                ),
                Err(error) => format!("error={error}"),
            };
            eprintln!(
                "anchor-wecom-gateway: callback {} settled: {summary}",
                event.event_id
            );
        }
        let outcome = match result {
            Ok(reply) if reply.superseded => {
                // The Host cancelled this turn for a newer message. That
                // decision, not our arrival order, says which turn is live.
                self.ledger.supersede(&event.event_id)?;
                self.ledger
                    .suppress(&event.event_id, reply.receipt.as_ref())
            }
            Ok(reply) => match reply.text {
                Some(text) if self.ledger.is_latest(&event)? => {
                    let items = crate::media::encode_images(&reply.items)?;
                    self.ledger.ready(
                        &event.event_id,
                        &text,
                        items.as_deref(),
                        reply.receipt.as_ref(),
                    )
                }
                Some(_) => self
                    .ledger
                    .suppress(&event.event_id, reply.receipt.as_ref()),
                None => self.ledger.finish_event(&event.event_id, true),
            },
            Err(error) => {
                if error == GatewayError::Rejected {
                    // The Host will never accept this event. Retire it so no
                    // restart replays it; only close the user's progress bubble
                    // when the event is recent enough that one may exist.
                    self.ledger.reject(&event.event_id)?;
                } else {
                    self.ledger.finish_event(&event.event_id, false)?;
                }
                self.report(error);
                let now = current_time_millis()?;
                if self.ledger.is_latest(&event)?
                    && self
                        .ledger
                        .is_recent(&event.event_id, now, self.recovery_window_millis())?
                {
                    self.feedback.push_back(Feedback {
                        event,
                        finish: true,
                    });
                }
                Ok(())
            }
        };
        self.pump_recovery();
        outcome
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
        self.feedback_pending.clear();
        self.media_pending.clear();
        // An interrupted upload is retryable and invisible to the user; an
        // interrupted image send may already have reached the platform, so its
        // durable claim keeps it unknown and it is never repeated.
        for job in std::mem::take(&mut self.media) {
            match job.stage {
                MediaStage::Sending { .. } => {}
                _ => self.media.push_back(MediaJob {
                    stage: MediaStage::Init,
                    ..job
                }),
            }
        }
        for (_, pending) in std::mem::take(&mut self.pending) {
            self.finish_pending(pending, false)?;
        }
        Ok(())
    }

    fn finish_pending(
        &mut self,
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
            if !confirmed {
                // The text reply's outcome is unknown, so this turn has no
                // confirmed user-visible answer. A late image the user cannot
                // match to a reply is not dispatched; its queued job is dropped.
                self.media.retain(|job| job.event_id != event_id);
            }
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

/// A bounded identifier from a media acknowledgement body.
fn media_identifier(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .pointer(&format!("/body/{key}"))
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= 512 && !text.chars().any(char::is_control))
        .map(str::to_owned)
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
                    match &result {
                        Ok(()) => {
                            if let Err(error) = ledger.finish_settlement(&key, true) {
                                let _ = errors.send(error);
                            }
                        }
                        Err(GatewayError::Rejected) => {
                            eprintln!(
                                "anchor-wecom-gateway: settlement refused by the Host; not retrying"
                            );
                            if let Err(error) = ledger.abandon_settlement(&key) {
                                let _ = errors.send(error);
                            }
                        }
                        Err(_) => {
                            if let Err(error) = ledger.finish_settlement(&key, false) {
                                let _ = errors.send(error);
                            }
                            if let Err(error) = result {
                                let _ = errors.send(error);
                            }
                        }
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
