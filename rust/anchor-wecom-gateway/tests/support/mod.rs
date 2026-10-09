#![allow(dead_code)]

use std::{
    collections::{BTreeSet, VecDeque},
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use anchor_wecom_gateway::{GatewayConfig, Timing, WebhookConfig};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, UnixStream},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

pub const TOKEN: &str = "fixture-control-token-000000000000000000000";

enum Command {
    Frame(Value),
    Binary(Value),
    Close,
}

pub struct Platform {
    pub url: String,
    pub acknowledge_ping: Arc<AtomicBool>,
    commands: mpsc::Sender<Command>,
    requests: mpsc::Receiver<Value>,
    task: JoinHandle<()>,
}

impl Platform {
    pub async fn new(auth_code: Option<i64>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (request_tx, requests) = mpsc::channel(128);
        let (commands, mut command_rx) = mpsc::channel(128);
        let acknowledge_ping = Arc::new(AtomicBool::new(true));
        let ping = acknowledge_ping.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let Ok(mut socket) = accept_async(stream).await else {
                    continue;
                };
                loop {
                    tokio::select! {
                        command = command_rx.recv() => match command {
                            Some(Command::Frame(value)) => {
                                if socket.send(Message::Text(value.to_string().into())).await.is_err() { break; }
                            },
                            Some(Command::Binary(value)) => {
                                if socket.send(Message::Binary(serde_json::to_vec(&value).unwrap().into())).await.is_err() { break; }
                            },
                            Some(Command::Close) => { let _ = socket.close(None).await; break; },
                            None => return,
                        },
                        wire = socket.next() => {
                            let value = match wire {
                                Some(Ok(Message::Text(text))) => serde_json::from_str::<Value>(&text).unwrap(),
                                Some(Ok(Message::Ping(_))) => { let _ = socket.flush().await; continue; },
                                Some(Ok(Message::Pong(_))) => continue,
                                _ => break,
                            };
                            let code = match value["cmd"].as_str() {
                                Some("aibot_subscribe") => auth_code,
                                Some("ping") if ping.load(Ordering::SeqCst) => Some(0),
                                _ => None,
                            };
                            if let Some(code) = code {
                                let ack = json!({"headers":value["headers"],"errcode":code});
                                if socket.send(Message::Text(ack.to_string().into())).await.is_err() { break; }
                            }
                            if request_tx.send(value).await.is_err() { return; }
                        },
                    }
                }
            }
        });
        Self {
            url,
            acknowledge_ping,
            commands,
            requests,
            task,
        }
    }

    pub async fn send(&self, value: Value) {
        self.commands.send(Command::Frame(value)).await.unwrap();
    }

    pub async fn send_binary(&self, value: Value) {
        self.commands.send(Command::Binary(value)).await.unwrap();
    }

    pub async fn acknowledge(&self, value: &Value, code: i64) {
        self.send(json!({"headers":value["headers"],"errcode":code}))
            .await;
    }

    /// Acknowledge a command that returns a body, as the media protocol does.
    pub async fn acknowledge_body(&self, value: &Value, code: i64, body: Value) {
        self.send(json!({"headers":value["headers"],"errcode":code,"body":body}))
            .await;
    }

    pub async fn close(&self) {
        self.commands.send(Command::Close).await.unwrap();
    }

    pub async fn next(&mut self, command: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let value = self.requests.recv().await.unwrap();
                if value["cmd"] == command {
                    return value;
                }
            }
        })
        .await
        .unwrap()
    }

    pub async fn next_final_response(&mut self) -> Value {
        loop {
            let value = self.next("aibot_respond_msg").await;
            if value["body"]["stream"]["finish"] == false {
                self.acknowledge(&value, 0).await;
                continue;
            }
            return value;
        }
    }

    pub async fn no_command(&mut self, command: &str) {
        let result = tokio::time::timeout(Duration::from_millis(100), async {
            while let Some(value) = self.requests.recv().await {
                assert_ne!(
                    value["cmd"], command,
                    "unexpected duplicate platform dispatch"
                );
            }
        })
        .await;
        assert!(result.is_err());
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
pub struct HttpReply {
    pub status: StatusCode,
    pub body: Value,
    pub delay: Duration,
}

impl HttpReply {
    pub fn ok(body: Value) -> Self {
        Self {
            status: StatusCode::OK,
            body,
            delay: Duration::ZERO,
        }
    }
}

#[derive(Clone)]
struct HttpState {
    events: Arc<Mutex<Vec<Value>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    respond: Arc<dyn Fn(&Value) -> HttpReply + Send + Sync>,
    settle: Arc<dyn Fn(&Value) -> HttpReply + Send + Sync>,
}

pub struct WebhookServer {
    pub url: String,
    pub events: Arc<Mutex<Vec<Value>>>,
    pub requests: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
}

impl WebhookServer {
    pub async fn new(respond: impl Fn(&Value) -> HttpReply + Send + Sync + 'static) -> Self {
        Self::new_with_settlement(respond, |_| HttpReply::ok(json!({}))).await
    }

    pub async fn new_with_settlement(
        respond: impl Fn(&Value) -> HttpReply + Send + Sync + 'static,
        settle: impl Fn(&Value) -> HttpReply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/event", listener.local_addr().unwrap());
        let events = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = HttpState {
            events: events.clone(),
            requests: requests.clone(),
            respond: Arc::new(respond),
            settle: Arc::new(settle),
        };
        let router = Router::new()
            .route("/event", post(webhook))
            .with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url,
            events,
            requests,
            task,
        }
    }

    pub async fn wait_events(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if self.events.lock().unwrap().len() >= expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    pub async fn wait_requests(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if self.requests.lock().unwrap().len() >= expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

impl Drop for WebhookServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn webhook(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer fixture-webhook-key"
    );
    state.requests.lock().unwrap().push(body.clone());
    if body.get("settlement").is_some() {
        let reply = (state.settle)(&body);
        tokio::time::sleep(reply.delay).await;
        return (reply.status, Json(reply.body)).into_response();
    }
    let event = body["event"].clone();
    state.events.lock().unwrap().push(event.clone());
    let reply = (state.respond)(&event);
    tokio::time::sleep(reply.delay).await;
    (reply.status, Json(reply.body)).into_response()
}

/// One frame of a fixture progress stream.
pub struct ProgressFrame {
    pub data: Value,
    pub delay: Duration,
}

impl ProgressFrame {
    pub fn update(content: &str, delay: Duration) -> Self {
        Self {
            data: json!({"seq":1,"kind":"status","content":content,"settled":false}),
            delay,
        }
    }

    /// A status frame in the current Host format: category token plus step.
    pub fn status(content: &str, category: &str, step: u32, delay: Duration) -> Self {
        Self {
            data: json!({
                "seq":1,"kind":"status","content":content,
                "category":category,"step":step,"settled":false
            }),
            delay,
        }
    }

    pub fn settled(status: &str) -> Self {
        Self {
            data: json!({"seq":9,"settled":true,"status":status}),
            delay: Duration::ZERO,
        }
    }
}

type ProgressScript = Arc<dyn Fn(&str) -> Vec<ProgressFrame> + Send + Sync>;

#[derive(Clone)]
struct ProgressState {
    script: ProgressScript,
    requested: Arc<Mutex<Vec<String>>>,
    closed: Arc<Mutex<Vec<String>>>,
    /// Refuse this many requests as "not admitted yet" before answering.
    refusals: Arc<Mutex<usize>>,
}

/// A fixture progress stream. Records which events subscribed and, through the
/// stream's drop, which subscriptions the transport stopped early.
pub struct ProgressServer {
    pub url: String,
    pub requested: Arc<Mutex<Vec<String>>>,
    pub closed: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

struct ProgressBody {
    event_id: String,
    frames: VecDeque<ProgressFrame>,
    pending: Option<ProgressFrame>,
    closed: Arc<Mutex<Vec<String>>>,
    finished: bool,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl futures::Stream for ProgressBody {
    type Item = Result<SseEvent, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(sleep) = self.sleep.as_mut() {
            if sleep.as_mut().poll(context).is_pending() {
                return Poll::Pending;
            }
            self.sleep = None;
        }
        if self.pending.is_none() {
            let Some(frame) = self.frames.pop_front() else {
                self.finished = true;
                return Poll::Ready(None);
            };
            self.pending = Some(frame);
        }
        let frame = self.pending.as_mut().expect("frame was just ensured");
        if !frame.delay.is_zero() {
            // The delay is consumed now; the frame still waits for it.
            let mut sleep = Box::pin(tokio::time::sleep(frame.delay));
            if sleep.as_mut().poll(context).is_pending() {
                frame.delay = Duration::ZERO;
                self.sleep = Some(sleep);
                return Poll::Pending;
            }
        }
        let frame = self.pending.take().expect("frame was just ensured");
        let settled = frame.data["settled"].as_bool().unwrap_or(false);
        Poll::Ready(Some(Ok(SseEvent::default()
            .event(if settled { "settled" } else { "update" })
            .data(frame.data.to_string()))))
    }
}

impl Drop for ProgressBody {
    fn drop(&mut self) {
        if !self.finished {
            self.closed.lock().unwrap().push(self.event_id.clone());
        }
    }
}

impl ProgressServer {
    pub async fn new(script: impl Fn(&str) -> Vec<ProgressFrame> + Send + Sync + 'static) -> Self {
        Self::new_with_refusals(0, script).await
    }

    /// A server that answers 404 the first `refusals` times, modelling a
    /// subscribe attempt that races Host admission.
    pub async fn new_with_refusals(
        refusals: usize,
        script: impl Fn(&str) -> Vec<ProgressFrame> + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requested = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(Vec::new()));
        let state = ProgressState {
            script: Arc::new(script),
            requested: requested.clone(),
            closed: closed.clone(),
            refusals: Arc::new(Mutex::new(refusals)),
        };
        let router = Router::new()
            .route("/channels/wecom/progress/{event_id}", get(progress_stream))
            .with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url,
            requested,
            closed,
            task,
        }
    }

    pub async fn wait_requested(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if self.requested.lock().unwrap().len() >= expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    pub async fn wait_closed(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if self.closed.lock().unwrap().len() >= expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

impl Drop for ProgressServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn progress_stream(
    AxumPath(event_id): AxumPath<String>,
    State(state): State<ProgressState>,
    headers: HeaderMap,
) -> Response {
    // The Host authenticates this route; the fixture must too, otherwise an
    // unauthenticated subscription would look healthy in tests.
    if headers.get("authorization").is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid API key"})),
        )
            .into_response();
    }
    state.requested.lock().unwrap().push(event_id.clone());
    {
        let mut refusals = state.refusals.lock().unwrap();
        if *refusals > 0 {
            *refusals -= 1;
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"unknown event"})),
            )
                .into_response();
        }
    }
    let frames = (state.script)(&event_id);
    Sse::new(ProgressBody {
        event_id,
        frames: frames.into(),
        pending: None,
        closed: state.closed.clone(),
        finished: false,
        sleep: None,
    })
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
    .into_response()
}

pub fn config(root: &Path, platform: &Platform, webhook: Option<&WebhookServer>) -> GatewayConfig {
    config_with_progress(root, platform, webhook, None)
}

pub fn config_with_progress(
    root: &Path,
    platform: &Platform,
    webhook: Option<&WebhookServer>,
    progress: Option<&ProgressServer>,
) -> GatewayConfig {
    let mut config = GatewayConfig::new(
        root.join("state"),
        "fixture-bot".into(),
        "fixture-secret".into(),
        TOKEN.into(),
    );
    config.ws_url = platform.url.clone();
    config.inbound_users = BTreeSet::from(["alice".into(), "bob".into()]);
    config.send_users = config.inbound_users.clone();
    config.timing = Timing {
        connect_timeout: Duration::from_millis(500),
        ack_timeout: Duration::from_millis(150),
        heartbeat_interval: Duration::from_secs(10),
        reconnect_base: Duration::from_millis(20),
        reconnect_max: Duration::from_millis(100),
        // Tests that exercise the progress bubble lower this explicitly; the
        // default keeps every other test free of an extra frame.
        ack_delay: Duration::from_secs(60),
        // Animation is off unless a test asks for it, so an idle frame never
        // lands in another test's expectations.
        animation_interval: Duration::from_secs(60),
    };
    // Fixtures keep the (default-on) progress display enabled so the bubble
    // semantics stay covered; `ANCHOR_WECOM_PROGRESS=0` disables it in
    // production.
    config.progress = true;
    config.webhook = webhook.map(|server| WebhookConfig {
        url: server.url.clone(),
        api_key: "fixture-webhook-key".into(),
        timeout: Some(Duration::from_secs(2)),
    });
    config.progress_url = progress.map(|server| format!("{}/channels/wecom/progress", server.url));
    config
}

pub fn callback(event_id: &str, sender: &str, text: &str) -> Value {
    json!({"cmd":"aibot_msg_callback","headers":{"req_id":format!("callback-{event_id}")},
        "body":{"msgid":event_id,"msgtype":"text","from":{"userid":sender},"text":{"content":text}}})
}

pub fn send_request(request_id: &str, userid: &str, content: &str) -> Value {
    json!({"operation":"send","request_id":request_id,"userid":userid,"content":content})
}

pub async fn control(path: &Path, token: &str, mut request: Value) -> Value {
    request["token"] = json!(token);
    let mut bytes = serde_json::to_vec(&request).unwrap();
    bytes.push(b'\n');
    raw_control(path, &bytes).await
}

pub async fn raw_control(path: &Path, bytes: &[u8]) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = UnixStream::connect(path).await.unwrap();
        stream.write_all(bytes).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut response = Vec::new();
        BufReader::new(stream)
            .read_until(b'\n', &mut response)
            .await
            .unwrap();
        serde_json::from_slice(&response).unwrap()
    })
    .await
    .unwrap()
}

/// A local stand-in for the platform's short-lived media download URL.
///
/// It answers any path with the same ciphertext and headers, so a test can
/// assert exactly what the gateway does with the platform's bytes without any
/// network access.
pub struct MediaServer {
    pub url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl MediaServer {
    pub async fn new(body: Vec<u8>, disposition: Option<&str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/media/ciphertext", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shared = requests.clone();
        let disposition = disposition.map(str::to_owned);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let body = body.clone();
                let disposition = disposition.clone();
                let requests = shared.clone();
                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    let read = stream.read(&mut buffer).await.unwrap_or(0);
                    if let Some(line) = String::from_utf8_lossy(&buffer[..read]).lines().next() {
                        requests.lock().unwrap().push(line.to_owned());
                    }
                    let mut response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n",
                        body.len()
                    );
                    if let Some(disposition) = disposition {
                        response.push_str(&format!("Content-Disposition: {disposition}\r\n"));
                    }
                    response.push_str("\r\n");
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                    let _ = stream.flush().await;
                });
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// An inbound callback whose payload is a platform media descriptor.
pub fn media_callback(event_id: &str, sender: &str, kind: &str, url: &str, aeskey: &str) -> Value {
    json!({"cmd":"aibot_msg_callback","headers":{"req_id":format!("callback-{event_id}")},
        "body":{"msgid":event_id,"msgtype":kind,"from":{"userid":sender},
            kind:{"url":url,"aeskey":aeskey}}})
}
