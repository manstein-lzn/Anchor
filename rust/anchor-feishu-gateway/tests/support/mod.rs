#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    convert::Infallible,
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use anchor_feishu_gateway::{
    GatewayConfig, Timing, WebhookConfig,
    protocol::{Frame, Header},
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Multipart, Path as AxumPath, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{get, patch, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, UnixStream},
    sync::{Mutex as AsyncMutex, Notify, mpsc},
    task::JoinHandle,
};

pub const TOKEN: &str = "fixture-control-token-at-least-32-bytes";

enum Command {
    Frame(Frame),
    Close,
}

pub struct Fixture {
    pub base: String,
    pub shared: Arc<FixtureState>,
    commands: mpsc::Sender<Command>,
    received: mpsc::Receiver<Frame>,
    task: JoinHandle<()>,
}

pub struct FixtureState {
    pub events: Mutex<Vec<Value>>,
    pub settlements: Mutex<Vec<Value>>,
    pub replies: Mutex<Vec<Value>>,
    pub sends: Mutex<Vec<Value>>,
    pub activity: Mutex<Vec<Value>>,
    pub patches: Mutex<Vec<Value>>,
    pub recalls: Mutex<Vec<String>>,
    pub final_mode: AtomicUsize,
    pub final_blocked: AtomicBool,
    pub final_entered: Notify,
    pub final_release: Notify,
    pub uploads: Mutex<Vec<Value>>,
    pub downloads: Mutex<Vec<Value>>,
    pub resource_body: Mutex<Vec<u8>>,
    pub resource_mode: AtomicUsize,
    pub resource_blocked: AtomicBool,
    pub resource_entered: Notify,
    pub resource_release: Notify,
    pub upload_mode: AtomicUsize,
    pub upload_entered: Notify,
    pub upload_release: Notify,
    pub activity_mode: AtomicUsize,
    pub activity_entered: Notify,
    pub activity_release: Notify,
    pub callback_response: Mutex<Option<Value>>,
    pub callback_status: AtomicUsize,
    pub progress_unavailable: AtomicBool,
    pub progress_opened: AtomicUsize,
    pub progress_closed: AtomicUsize,
    pub progress:
        Mutex<std::collections::BTreeMap<String, mpsc::UnboundedSender<Result<Event, Infallible>>>>,
    pub bootstraps: AtomicUsize,
    pub connections: AtomicUsize,
    pub tokens: AtomicUsize,
    pub reply_mode: AtomicUsize,
    pub reply_blocked: AtomicBool,
    pub reply_entered: Notify,
    pub reply_release: Notify,
    pub send_mode: AtomicUsize,
    pub token_failure: AtomicBool,
    pub token_blocked: AtomicBool,
    pub token_entered: Notify,
    pub token_release: Notify,
    pub send_entered: Notify,
    pub send_release: Notify,
    pub blocked: AtomicBool,
    pub entered: Notify,
    pub release: Notify,
    pub endpoint_config: Mutex<Value>,
    pub pong_config: Mutex<Value>,
    commands: AsyncMutex<mpsc::Receiver<Command>>,
    received: mpsc::Sender<Frame>,
    base: String,
}

impl Fixture {
    pub async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (commands, command_rx) = mpsc::channel(64);
        let (received_tx, received) = mpsc::channel(256);
        let shared = Arc::new(FixtureState {
            events: Mutex::new(Vec::new()),
            settlements: Mutex::new(Vec::new()),
            replies: Mutex::new(Vec::new()),
            sends: Mutex::new(Vec::new()),
            activity: Mutex::new(Vec::new()),
            patches: Mutex::new(Vec::new()),
            recalls: Mutex::new(Vec::new()),
            final_mode: AtomicUsize::new(0),
            final_blocked: AtomicBool::new(false),
            final_entered: Notify::new(),
            final_release: Notify::new(),
            uploads: Mutex::new(Vec::new()),
            downloads: Mutex::new(Vec::new()),
            resource_body: Mutex::new(b"fixture inbound file contents".to_vec()),
            resource_mode: AtomicUsize::new(0),
            resource_blocked: AtomicBool::new(false),
            resource_entered: Notify::new(),
            resource_release: Notify::new(),
            upload_mode: AtomicUsize::new(0),
            upload_entered: Notify::new(),
            upload_release: Notify::new(),
            activity_mode: AtomicUsize::new(0),
            activity_entered: Notify::new(),
            activity_release: Notify::new(),
            callback_response: Mutex::new(None),
            callback_status: AtomicUsize::new(200),
            progress_unavailable: AtomicBool::new(false),
            progress_opened: AtomicUsize::new(0),
            progress_closed: AtomicUsize::new(0),
            progress: Mutex::new(std::collections::BTreeMap::new()),
            bootstraps: AtomicUsize::new(0),
            connections: AtomicUsize::new(0),
            tokens: AtomicUsize::new(0),
            reply_mode: AtomicUsize::new(0),
            reply_blocked: AtomicBool::new(false),
            reply_entered: Notify::new(),
            reply_release: Notify::new(),
            send_mode: AtomicUsize::new(0),
            token_failure: AtomicBool::new(false),
            token_blocked: AtomicBool::new(false),
            token_entered: Notify::new(),
            token_release: Notify::new(),
            send_entered: Notify::new(),
            send_release: Notify::new(),
            blocked: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
            endpoint_config: Mutex::new(json!({})),
            pong_config: Mutex::new(json!({})),
            commands: AsyncMutex::new(command_rx),
            received: received_tx,
            base: base.clone(),
        });
        let app = Router::new()
            .route("/callback/ws/endpoint", post(endpoint))
            .route("/ws", get(websocket))
            .route(
                "/open-apis/auth/v3/tenant_access_token/internal",
                post(token),
            )
            .route("/open-apis/im/v1/messages/{message_id}/reply", post(reply))
            .route(
                "/open-apis/im/v1/messages/{message_id}",
                patch(update_card).delete(recall),
            )
            .route(
                "/open-apis/im/v1/messages/{message_id}/resources/{key}",
                get(resource),
            )
            .route("/open-apis/im/v1/files", post(upload))
            .route("/open-apis/im/v1/messages", post(send_message))
            .route("/anchor/events", post(callback))
            .route("/anchor/progress/{message_id}", get(progress))
            .layer(DefaultBodyLimit::max(
                anchor_feishu_gateway::MAX_MEDIA_REQUEST_BYTES,
            ))
            .with_state(shared.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base,
            shared,
            commands,
            received,
            task,
        }
    }

    pub fn config(&self, root: &Path) -> GatewayConfig {
        let mut config = GatewayConfig::new(
            root.join("state"),
            "cli_fixture_app".into(),
            "fixture-app-secret".into(),
            TOKEN.into(),
        );
        config.api_base_url = self.base.clone();
        config.inbound_users = BTreeSet::from(["ou_alice".into(), "ou_bob".into()]);
        config.send_users = config.inbound_users.clone();
        config.webhook = Some(WebhookConfig {
            url: format!("{}/anchor/events", self.base),
            api_key: "fixture-host-key".into(),
            timeout: None,
        });
        config.timing = Timing {
            connect_timeout: Duration::from_secs(2),
            http_timeout: Duration::from_secs(2),
            heartbeat_interval: Duration::from_millis(40),
            reconnect_base: Duration::from_millis(20),
            reconnect_max: Duration::from_millis(100),
        };
        config
    }

    pub async fn send(&self, frame: Frame) {
        self.commands.send(Command::Frame(frame)).await.unwrap();
    }
    pub async fn event(&self, message_id: &str, text: &str) {
        self.send(event_frame(message_id, text)).await;
    }
    pub async fn disconnect(&self) {
        self.commands.send(Command::Close).await.unwrap();
    }

    pub async fn next(&mut self, kind: &str) -> Frame {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let frame = self.received.recv().await.unwrap();
                if frame.header("type") == Some(kind) {
                    return frame;
                }
            }
        })
        .await
        .expect("no expected platform frame")
    }

    pub async fn ack(&mut self) -> Value {
        let frame = self.next("event").await;
        serde_json::from_slice(&frame.payload).unwrap()
    }

    pub async fn wait_replies(&self, count: usize) {
        poll(|| self.shared.replies.lock().unwrap().len() >= count).await;
    }
    pub async fn wait_settlements(&self, count: usize) {
        poll(|| self.shared.settlements.lock().unwrap().len() >= count).await;
    }

    pub async fn progress_line(&self, message_id: &str, line: Value) {
        poll(|| {
            self.shared
                .progress
                .lock()
                .unwrap()
                .contains_key(message_id)
        })
        .await;
        let _ = self.shared.progress.lock().unwrap()[message_id].send(Ok(Event::default()
            .event(if line["settled"] == true {
                "settled"
            } else {
                "update"
            })
            .data(line.to_string())));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn endpoint(State(state): State<Arc<FixtureState>>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(
        body,
        json!({"AppID":"cli_fixture_app","AppSecret":"fixture-app-secret"})
    );
    state.bootstraps.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"code":0,"data":{"URL":format!("{}/ws?service_id=1&device_id=fixture",state.base.replacen("http", "ws", 1)),
        "ClientConfig":state.endpoint_config.lock().unwrap().clone()}}),
    )
}

async fn websocket(State(state): State<Arc<FixtureState>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| connection(socket, state))
}

async fn connection(mut socket: WebSocket, state: Arc<FixtureState>) {
    state.connections.fetch_add(1, Ordering::SeqCst);
    let mut commands = state.commands.lock().await;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Frame(frame)) => {
                    if socket.send(Message::Binary(frame.encode_binary().into())).await.is_err() { break; }
                }
                Some(Command::Close) => { let _ = socket.close().await; break; }
                None => break,
            },
            wire = socket.next() => {
                let frame = match wire {
                    Some(Ok(Message::Binary(bytes))) => Frame::decode_binary(&bytes).unwrap(),
                    Some(Ok(Message::Ping(_))) => { let _ = socket.flush().await; continue; }
                    Some(Ok(Message::Pong(_))) => continue,
                    _ => break,
                };
                if frame.header("type") == Some("ping") {
                    let payload = state.pong_config.lock().unwrap().clone();
                    let pong = Frame {service:1,headers:vec![Header{key:"type".into(),value:"pong".into()}],
                        payload:serde_json::to_vec(&payload).unwrap(),..Frame::default()};
                    if socket.send(Message::Binary(pong.encode_binary().into())).await.is_err() { break; }
                }
                if state.received.send(frame).await.is_err() { break; }
            }
        }
    }
}

async fn token(State(state): State<Arc<FixtureState>>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(
        body,
        json!({"app_id":"cli_fixture_app","app_secret":"fixture-app-secret"})
    );
    state.tokens.fetch_add(1, Ordering::SeqCst);
    if state.token_failure.load(Ordering::SeqCst) {
        return Json(json!({"code":999}));
    }
    if state.token_blocked.load(Ordering::SeqCst) {
        state.token_entered.notify_one();
        state.token_release.notified().await;
    }
    Json(json!({"code":0,"tenant_access_token":"fixture-tenant-token","expire":7200}))
}

async fn send_message(
    State(state): State<Arc<FixtureState>>,
    Query(query): Query<std::collections::BTreeMap<String, String>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    assert_eq!(query["receive_id_type"], "open_id");
    assert!(matches!(
        body["msg_type"].as_str(),
        Some("interactive" | "file")
    ));
    if body["msg_type"] == "interactive" {
        card_text(&body);
    }
    state.sends.lock().unwrap().push(body);
    let mode = state.send_mode.load(Ordering::SeqCst);
    if mode == 4 {
        state.send_entered.notify_one();
        state.send_release.notified().await;
    }
    match mode {
        1 => Json(json!({"code":0,"data":{}})).into_response(),
        2 => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"code":0,"data":{"message_id":"om_untrusted"}})),
        )
            .into_response(),
        3 => Json(json!({"code":999,"data":{"message_id":"om_untrusted"}})).into_response(),
        _ => Json(json!({"code":0,"data":{"message_id":"om_notification"}})).into_response(),
    }
}

async fn reply(
    AxumPath(message_id): AxumPath<String>,
    State(state): State<Arc<FixtureState>>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer fixture-tenant-token"
    );
    assert_eq!(body["msg_type"], "interactive");
    let markdown = card_text(&body);
    body["message_id"] = json!(message_id);
    if markdown.starts_with("**工作进度**\n") {
        state.activity.lock().unwrap().push(body);
        let mode = state.activity_mode.load(Ordering::SeqCst);
        if mode == 3 {
            state.activity_entered.notify_one();
            state.activity_release.notified().await;
        }
        return if mode == 1 {
            Json(json!({"code":999})).into_response()
        } else {
            Json(json!({"code":0,"data":{"message_id":format!("om_activity_{message_id}")}}))
                .into_response()
        };
    }
    state.replies.lock().unwrap().push(body);
    if state.reply_blocked.load(Ordering::SeqCst) {
        state.reply_entered.notify_one();
        state.reply_release.notified().await;
    }
    match state.reply_mode.load(Ordering::SeqCst) {
        1 => Json(json!({"code":0,"data":{}})).into_response(),
        2 => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"code":0,"data":{"message_id":"om_untrusted"}})),
        )
            .into_response(),
        3 => Json(json!({"code":999,"data":{"message_id":"om_untrusted"}})).into_response(),
        _ => Json(json!({"code":0,"data":{"message_id":format!("om_sent_{message_id}")}}))
            .into_response(),
    }
}

async fn callback(
    State(state): State<Arc<FixtureState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer fixture-host-key"
    );
    if body.get("settlement").is_some() {
        state.settlements.lock().unwrap().push(body);
        return Json(json!({})).into_response();
    }
    state.events.lock().unwrap().push(body.clone());
    if state.blocked.load(Ordering::SeqCst) {
        state.entered.notify_one();
        state.release.notified().await;
    }
    if state.callback_status.load(Ordering::SeqCst) != 200 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if let Some(response) = state.callback_response.lock().unwrap().clone() {
        return Json(response).into_response();
    }
    Json(
        json!({"text":format!("reply to {}", body["event"]["text"].as_str().unwrap()),
        "receipt":{"key":format!("channel-fixture-{}",body["event"]["event_id"].as_str().unwrap()),"content_sha256":"a".repeat(64)}}),
    ).into_response()
}

pub fn card_text(body: &Value) -> String {
    assert!(serde_json::to_vec(body).unwrap().len() <= 30_000);
    let card: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
    assert_eq!(card["schema"], "2.0");
    assert_eq!(card["config"]["update_multi"], true);
    assert_eq!(card["body"]["elements"][0]["tag"], "markdown");
    card["body"]["elements"][0]["content"]
        .as_str()
        .unwrap()
        .into()
}

async fn update_card(
    AxumPath(message_id): AxumPath<String>,
    State(state): State<Arc<FixtureState>>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    assert_eq!(body.as_object().unwrap().len(), 1);
    let is_final = !card_text(&body).starts_with("**工作进度**\n");
    body["message_id"] = json!(message_id);
    state.patches.lock().unwrap().push(body);
    if is_final {
        state.final_entered.notify_one();
        let mode = state.final_mode.load(Ordering::SeqCst);
        if mode == 4 || state.final_blocked.load(Ordering::SeqCst) {
            state.final_release.notified().await;
        }
        return match mode {
            1 => (StatusCode::BAD_REQUEST, Json(json!({"code":230011}))).into_response(),
            2 => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"code":0}))).into_response(),
            3 => Json(json!({"code":230099})).into_response(),
            5 => Json(json!({"data":{}})).into_response(),
            6 => (StatusCode::BAD_REQUEST, "untrusted-not-json").into_response(),
            7 => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"code":230099})),
            )
                .into_response(),
            _ => Json(json!({"code":0,"data":{}})).into_response(),
        };
    }
    if state.activity_mode.load(Ordering::SeqCst) == 4 {
        state.activity_entered.notify_one();
        state.activity_release.notified().await;
    }
    if state.activity_mode.load(Ordering::SeqCst) == 2 {
        return Json(json!({"code":999})).into_response();
    }
    Json(json!({"code":0,"data":{}})).into_response()
}

async fn recall(
    AxumPath(message_id): AxumPath<String>,
    State(state): State<Arc<FixtureState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    assert!(body.is_empty());
    assert!(state.activity.lock().unwrap().iter().any(|created| {
        message_id == format!("om_activity_{}", created["message_id"].as_str().unwrap())
    }));
    state.recalls.lock().unwrap().push(message_id);
    Json(json!({"code":0,"data":{}})).into_response()
}

async fn upload(
    State(state): State<Arc<FixtureState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    let mut received = json!({});
    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap().to_owned();
        assert!(received.get(&name).is_none());
        received[&name] = if name == "file" {
            json!({"name":field.file_name().unwrap(),"data_base64":STANDARD.encode(field.bytes().await.unwrap())})
        } else {
            json!(field.text().await.unwrap())
        };
    }
    assert_eq!(received["file_type"], "stream");
    assert_eq!(received["file_name"], received["file"]["name"]);
    state.uploads.lock().unwrap().push(received);
    let mode = state.upload_mode.load(Ordering::SeqCst);
    if mode == 4 {
        state.upload_entered.notify_one();
        state.upload_release.notified().await;
    }
    match mode {
        1 => Json(json!({"code":999,"msg":"untrusted-upstream-upload-detail"})).into_response(),
        2 => Json(json!({"code":0,"data":{}})).into_response(),
        3 => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        _ => Json(json!({"code":0,"data":{"file_key":"file_uploaded_fixture"}})).into_response(),
    }
}

async fn resource(
    AxumPath((message_id, key)): AxumPath<(String, String)>,
    State(state): State<Arc<FixtureState>>,
    Query(query): Query<std::collections::BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    assert!(matches!(query["type"].as_str(), "file" | "image"));
    state
        .downloads
        .lock()
        .unwrap()
        .push(json!({"message_id":message_id,"key":key,"type":query["type"]}));
    if state.resource_blocked.load(Ordering::SeqCst) {
        state.resource_entered.notify_one();
        state.resource_release.notified().await;
    }
    match state.resource_mode.load(Ordering::SeqCst) {
        1 => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        2 => (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", "/untrusted-resource")],
        )
            .into_response(),
        _ => state.resource_body.lock().unwrap().clone().into_response(),
    }
}

struct ProgressStream {
    receiver: mpsc::UnboundedReceiver<Result<Event, Infallible>>,
    state: Arc<FixtureState>,
}

impl futures::Stream for ProgressStream {
    type Item = Result<Event, Infallible>;
    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().receiver.poll_recv(context)
    }
}

impl Drop for ProgressStream {
    fn drop(&mut self) {
        self.state.progress_closed.fetch_add(1, Ordering::SeqCst);
    }
}

async fn progress(
    AxumPath(message_id): AxumPath<String>,
    State(state): State<Arc<FixtureState>>,
    headers: HeaderMap,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer fixture-host-key");
    state.progress_opened.fetch_add(1, Ordering::SeqCst);
    if state.progress_unavailable.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let (sender, receiver) = mpsc::unbounded_channel();
    sender
        .send(Ok(Event::default().event("update").data(
            json!({"content":"Preparing","category":"preparing"}).to_string(),
        )))
        .unwrap();
    state.progress.lock().unwrap().insert(message_id, sender);
    Sse::new(ProgressStream { receiver, state })
        .keep_alive(axum::response::sse::KeepAlive::default().interval(Duration::from_millis(50)))
        .into_response()
}

pub fn event(message_id: &str, text: &str) -> Value {
    json!({"schema":"2.0","header":{"app_id":"cli_fixture_app","event_type":"im.message.receive_v1","event_id":"not-the-dedup-key"},
        "event":{"sender":{"sender_type":"user","sender_id":{"open_id":"ou_alice"}},
        "message":{"message_id":message_id,"chat_id":"oc_private","chat_type":"p2p","message_type":"text","content":json!({"text":text}).to_string()}}})
}

pub fn event_frame(message_id: &str, text: &str) -> Frame {
    Frame {
        seq_id: 42,
        log_id: 83,
        service: 1,
        method: 1,
        headers: vec![
            Header {
                key: "type".into(),
                value: "event".into(),
            },
            Header {
                key: "message_id".into(),
                value: format!("transport-{message_id}"),
            },
            Header {
                key: "sum".into(),
                value: "1".into(),
            },
            Header {
                key: "seq".into(),
                value: "0".into(),
            },
        ],
        payload: serde_json::to_vec(&event(message_id, text)).unwrap(),
        ..Frame::default()
    }
}

pub async fn control(path: &Path, token: &str, operation: &str) -> Value {
    control_request(path, json!({"operation":operation,"token":token})).await
}

pub async fn control_request(path: &Path, request: Value) -> Value {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    stream.shutdown().await.unwrap();
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut bytes)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

pub async fn poll(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition did not become true");
}
