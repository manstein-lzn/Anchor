#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anchor_wecom_gateway::{GatewayConfig, Timing, WebhookConfig};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, UnixStream},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

pub const TOKEN: &str = "fixture-control-token-000000000000000000000";

enum Command {
    Frame(Value),
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

    pub async fn acknowledge(&self, value: &Value, code: i64) {
        self.send(json!({"headers":value["headers"],"errcode":code}))
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

pub fn config(root: &Path, platform: &Platform, webhook: Option<&WebhookServer>) -> GatewayConfig {
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
    };
    config.webhook = webhook.map(|server| WebhookConfig {
        url: server.url.clone(),
        api_key: "fixture-webhook-key".into(),
        timeout: Duration::from_secs(2),
    });
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
