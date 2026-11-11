#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use anchor_feishu_gateway::protocol::{Frame, Header};
use anchor_platform_session::{SessionEvent, SessionStore, TurnStatus};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path as RoutePath, Query, State},
    http::HeaderMap,
    routing::{get, patch, post},
};
use futures_util::{SinkExt, StreamExt};
use goose::{Gate, Host, HttpHost, Provider, Step, command, wait_until};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const GRAPH: &str = "feishu-fixture";
const APP_ID: &str = "cli_fixture_app";
const APP_SECRET: &str = "fixture-app-secret-not-production";
const USER: &str = "ou_fixture_alice";
const CHAT: &str = "oc_fixture_private";
const FIRST_MESSAGE: &str = "om_fixture_first";
const SECOND_MESSAGE: &str = "om_fixture_second";
const FIRST_INPUT: &str = "feishu-first-private-input";
const SECOND_INPUT: &str = "feishu-second-private-input";
const FIRST_REPLY: &str = "# Feishu Result\n\n**feishu-first-private-reply**\n\n| Item | Result |\n| --- | --- |\n| Workspace | preserved |\n\n```text\nfixture output\n```";
const SECOND_REPLY: &str = "feishu-second-private-reply";
const FILE_MESSAGE: &str = "om_fixture_file";
const FILE_KEY: &str = "file_fixture_inbound";
const FILE_BYTES: &[u8] = b"feishu-inbound-file-bytes";
const FILE_REPLY: &str =
    "# File Processed\n\nThe generated file and its introduction have confirmed receipts.";
const OUTBOUND_FILE: &[u8] = b"feishu-generated-file-bytes";

#[derive(Default)]
struct PlatformFacts {
    connected: bool,
    bootstrap_requests: usize,
    token_requests: usize,
    acknowledgements: Vec<Value>,
    replies: Vec<Value>,
    notifications: Vec<Value>,
    progress: Vec<Value>,
    recalls: Vec<String>,
    uploads: Vec<Value>,
    downloads: Vec<Value>,
    files: Vec<Value>,
}

struct PlatformState {
    websocket_url: String,
    facts: Mutex<PlatformFacts>,
}

async fn endpoint(State(state): State<Arc<PlatformState>>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body, json!({"AppID":APP_ID,"AppSecret":APP_SECRET}));
    state.facts.lock().unwrap().bootstrap_requests += 1;
    Json(json!({"code":0,"data":{
        "URL":state.websocket_url,"ClientConfig":{"PingInterval":30}
    }}))
}

async fn token(State(state): State<Arc<PlatformState>>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body, json!({"app_id":APP_ID,"app_secret":APP_SECRET}));
    state.facts.lock().unwrap().token_requests += 1;
    Json(json!({"code":0,"tenant_access_token":"fixture-tenant-token","expire":7200}))
}

async fn reply(
    State(state): State<Arc<PlatformState>>,
    RoutePath(message): RoutePath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    let content = markdown(&body);
    let expected = match message.as_str() {
        FIRST_MESSAGE => FIRST_REPLY,
        SECOND_MESSAGE => SECOND_REPLY,
        FILE_MESSAGE => FILE_REPLY,
        unexpected => panic!("reply addressed another platform message: {unexpected}"),
    };
    let mut facts = state.facts.lock().unwrap();
    let final_reply = content["body"]["elements"][0]["content"] == expected;
    let sent = if final_reply {
        format!("om_fixture_sent_{}", facts.replies.len() + 1)
    } else {
        format!("om_fixture_status_{}", facts.progress.len() + 1)
    };
    let fact = json!({"reply_to":message,"body":body,"message_id":sent,"operation":"create"});
    if final_reply {
        facts.replies.push(fact);
    } else {
        facts.progress.push(fact);
    }
    Json(json!({"code":0,"data":{"message_id":sent}}))
}

fn markdown(body: &Value) -> Value {
    assert_eq!(body["msg_type"], "interactive");
    let card: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
    assert_eq!(card["schema"], "2.0");
    assert_eq!(card["config"]["update_multi"], true);
    card
}

async fn update_progress(
    State(state): State<Arc<PlatformState>>,
    RoutePath(message): RoutePath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    assert!(message.starts_with("om_fixture_status_"));
    let content: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
    assert_eq!(content["schema"], "2.0");
    assert_eq!(content["config"]["update_multi"], true);
    let mut facts = state.facts.lock().unwrap();
    let original = facts
        .progress
        .iter()
        .find(|event| event["operation"] == "create" && event["message_id"] == message)
        .expect("PATCH must use an actually created card");
    let source = original["reply_to"].clone();
    let text = content["body"]["elements"][0]["content"].as_str().unwrap();
    if matches!(text, FIRST_REPLY | SECOND_REPLY | FILE_REPLY) {
        facts
            .replies
            .push(json!({"reply_to":source,"message_id":message,
                                  "operation":"patch","body":body}));
    } else {
        facts
            .progress
            .push(json!({"message_id":message,"operation":"patch","body":body}));
    }
    Json(json!({"code":0,"data":{}}))
}

async fn recall_progress(
    State(state): State<Arc<PlatformState>>,
    RoutePath(message): RoutePath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    assert!(body.is_empty());
    let mut facts = state.facts.lock().unwrap();
    assert!(
        facts
            .progress
            .iter()
            .any(|event| { event["operation"] == "create" && event["message_id"] == message })
    );
    assert!(
        !facts
            .replies
            .iter()
            .any(|event| event["message_id"] == message)
    );
    facts.recalls.push(message);
    Json(json!({"code":0,"data":{}}))
}

async fn notify(
    State(state): State<Arc<PlatformState>>,
    Query(query): Query<std::collections::BTreeMap<String, String>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    assert_eq!(query["receive_id_type"], "open_id");
    assert_eq!(body["receive_id"], USER);
    let mut facts = state.facts.lock().unwrap();
    let sent = if body["msg_type"] == "file" {
        let content: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
        assert_eq!(content, json!({"file_key":"file_fixture_uploaded"}));
        facts.files.push(body);
        format!("om_fixture_file_sent_{}", facts.files.len())
    } else {
        markdown(&body);
        facts.notifications.push(body);
        format!("om_fixture_notification_{}", facts.notifications.len())
    };
    Json(json!({"code":0,"data":{"message_id":sent}}))
}

async fn upload(
    State(state): State<Arc<PlatformState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    assert!(
        headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("multipart/form-data; boundary=")
    );
    let wire = String::from_utf8(body.to_vec()).unwrap();
    for required in [
        "name=\"file_type\"",
        "stream",
        "name=\"file_name\"",
        "name=\"file\"",
    ] {
        assert!(
            wire.contains(required),
            "missing multipart field: {required}"
        );
    }
    state
        .facts
        .lock()
        .unwrap()
        .uploads
        .push(json!({"wire":wire}));
    Json(json!({"code":0,"data":{"file_key":"file_fixture_uploaded"}}))
}

async fn download(
    State(state): State<Arc<PlatformState>>,
    RoutePath((message, key)): RoutePath<(String, String)>,
    Query(query): Query<std::collections::BTreeMap<String, String>>,
    headers: HeaderMap,
) -> (HeaderMap, &'static [u8]) {
    assert_eq!(headers["authorization"], "Bearer fixture-tenant-token");
    assert_eq!(message, FILE_MESSAGE);
    assert_eq!(key, FILE_KEY);
    assert_eq!(query["type"], "file");
    state
        .facts
        .lock()
        .unwrap()
        .downloads
        .push(json!({"message_id":message,"file_key":key,"type":query["type"]}));
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "text/plain".parse().unwrap());
    (headers, FILE_BYTES)
}

struct Platform {
    api_base_url: String,
    state: Arc<PlatformState>,
    outbound: mpsc::UnboundedSender<Frame>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<thread::JoinHandle<()>>,
}

impl Platform {
    fn new() -> Self {
        let http = TcpListener::bind("127.0.0.1:0").unwrap();
        let websocket = TcpListener::bind("127.0.0.1:0").unwrap();
        http.set_nonblocking(true).unwrap();
        websocket.set_nonblocking(true).unwrap();
        let api_base_url = format!("http://{}", http.local_addr().unwrap());
        let state = Arc::new(PlatformState {
            websocket_url: format!(
                "ws://{}/ws?service_id=1&device_id=fixture",
                websocket.local_addr().unwrap()
            ),
            facts: Mutex::new(PlatformFacts::default()),
        });
        let shared = state.clone();
        let (outbound, mut frames) = mpsc::unbounded_channel::<Frame>();
        let (stop, stopped) = oneshot::channel();
        let task = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let app = Router::new()
                        .route("/callback/ws/endpoint", post(endpoint))
                        .route("/open-apis/auth/v3/tenant_access_token/internal", post(token))
                        .route("/open-apis/im/v1/messages/{message}/reply", post(reply))
                        .route("/open-apis/im/v1/messages/{message}", patch(update_progress).delete(recall_progress))
                        .route("/open-apis/im/v1/messages/{message}/resources/{key}", get(download))
                        .route("/open-apis/im/v1/files", post(upload))
                        .route("/open-apis/im/v1/messages", post(notify))
                        .with_state(shared.clone());
                    let mut http = tokio::spawn(async move {
                        axum::serve(tokio::net::TcpListener::from_std(http).unwrap(), app)
                            .await
                            .unwrap();
                    });
                    let mut websocket = tokio::spawn(async move {
                        let listener = tokio::net::TcpListener::from_std(websocket).unwrap();
                        let (stream, _) = listener.accept().await.unwrap();
                        let mut socket = accept_async(stream).await.unwrap();
                        shared.facts.lock().unwrap().connected = true;
                        loop {
                            tokio::select! {
                                frame = frames.recv() => {
                                    let Some(frame) = frame else { break };
                                    socket.send(Message::Binary(frame.encode_binary().into())).await.unwrap();
                                }
                                message = socket.next() => {
                                    let Some(Ok(message)) = message else { break };
                                    match message {
                                        Message::Binary(bytes) => {
                                            let frame = Frame::decode_binary(&bytes).unwrap();
                                            if frame.method == 0 && frame.header("type") == Some("ping") {
                                                let pong = Frame { service:frame.service,
                                                    headers:vec![Header {key:"type".into(),value:"pong".into()}],
                                                    payload:b"{}".to_vec(), ..Frame::default() };
                                                socket.send(Message::Binary(pong.encode_binary().into())).await.unwrap();
                                            } else {
                                                assert_eq!(frame.method, 1);
                                                assert_eq!(frame.service, 1);
                                                let payload: Value = serde_json::from_slice(&frame.payload).unwrap();
                                                assert!(frame.header("biz_rt").unwrap().parse::<u64>().is_ok());
                                                shared.facts.lock().unwrap().acknowledgements.push(json!({
                                                    "seq_id":frame.seq_id,"message_id":frame.header("message_id"),
                                                    "response":payload
                                                }));
                                            }
                                        }
                                        Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await.unwrap(),
                                        Message::Close(_) => break,
                                        unexpected => panic!("non-binary Feishu platform frame: {unexpected:?}"),
                                    }
                                }
                            }
                        }
                    });
                    tokio::select! {
                        _ = stopped => {},
                        result = &mut websocket => result.unwrap(),
                        result = &mut http => panic!("local platform HTTP server exited: {result:?}"),
                    }
                    websocket.abort();
                    http.abort();
                });
        });
        Self {
            api_base_url,
            state,
            outbound,
            stop: Some(stop),
            task: Some(task),
        }
    }

    fn send(&self, seq: u64, message: &str, text: &str) {
        self.send_content(seq, message, "text", json!({"text":text}));
    }

    fn send_content(&self, seq: u64, message: &str, kind: &str, content: Value) {
        let wire_id = format!("transport_{seq}");
        let frame = Frame {
            seq_id: seq,
            log_id: seq,
            service: 1,
            method: 1,
            headers: [("type", "event"), ("message_id", &wire_id), ("sum", "1"), ("seq", "0")]
                .map(|(key, value)| Header { key: key.into(), value: value.into() }).to_vec(),
            payload_type: "json".into(),
            payload: serde_json::to_vec(&json!({
                "schema":"2.0",
                "header":{"app_id":APP_ID,"event_type":"im.message.receive_v1","event_id":format!("evt_{message}")},
                "event":{
                    "sender":{"sender_type":"user","sender_id":{"open_id":USER}},
                    "message":{"message_id":message,"chat_id":CHAT,"chat_type":"p2p",
                        "message_type":kind,"content":content.to_string()}
                }
            })).unwrap(),
            ..Frame::default()
        };
        self.outbound.send(frame).unwrap();
    }

    fn wait_ack(&self, seq: u64) -> Value {
        let mut ack = Value::Null;
        wait_until("Feishu durable-admission binary ACK", || {
            ack = self
                .state
                .facts
                .lock()
                .unwrap()
                .acknowledgements
                .iter()
                .find(|ack| ack["seq_id"] == seq)
                .cloned()
                .unwrap_or(Value::Null);
            !ack.is_null()
        });
        assert_eq!(ack["message_id"], format!("transport_{seq}"));
        assert_eq!(
            ack["response"],
            json!({"code":200,"headers":null,"data":null})
        );
        ack
    }

    fn snapshot(&self) -> Value {
        let facts = self.state.facts.lock().unwrap();
        json!({"connected":facts.connected,"bootstrap_requests":facts.bootstrap_requests,
            "token_requests":facts.token_requests,"acknowledgements":facts.acknowledgements,
            "replies":facts.replies,"notifications":facts.notifications,"progress":facts.progress,"recalls":facts.recalls,
            "uploads":facts.uploads,"downloads":facts.downloads,"files":facts.files})
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

fn graph() -> Value {
    let mut definition: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/graphs/feishu-persistent-assistant.json"
    )))
    .unwrap();
    definition["agents"]["assistant"]["model"] = json!("models.worker");
    definition
}

fn install_plugin(host: &Host) -> (PathBuf, Value) {
    let binary = fixture::host_binary().with_file_name("anchor-feishu-gateway");
    let installed = fixture::install_system_plugin(
        host.base.root.path(),
        anchor_graph_host::SystemPlugin::Feishu,
    );
    assert_eq!(
        goose::digest(&installed.directory.join("bin/anchor-feishu-gateway")),
        goose::digest(&binary)
    );
    (binary, serde_json::to_value(installed).unwrap())
}

fn configure_bot(server: &HttpHost) -> Value {
    configure_bot_definition(server, &graph())
}

fn configure_bot_definition(server: &HttpHost, definition: &Value) -> Value {
    let (status, created) = server.request(
        "POST",
        "/graphs",
        Some(&json!({"name":GRAPH,"definition":definition})),
    );
    assert_eq!(status, 201, "{created}");
    let base = format!("/graphs/{GRAPH}/channel");
    let (status, current) = server.request("GET", &base, None);
    assert_eq!(status, 200, "{current}");
    assert_eq!(current["platform"], "feishu");
    assert_eq!(current["configured"], false);
    let (status, saved) = server.request(
        "PUT",
        &base,
        Some(&json!({"users":[USER],"send_users":[USER],"private_only":true})),
    );
    assert_eq!(status, 200, "{saved}");
    let (status, credentials) = server.request("GET", &format!("{base}/credentials"), None);
    assert_eq!(status, 200, "{credentials}");
    assert_eq!(
        credentials["targets"][0]["target"],
        format!("channel_feishu@{GRAPH}")
    );
    for (field, value) in [("FEISHU_APP_ID", APP_ID), ("FEISHU_APP_SECRET", APP_SECRET)] {
        let (status, stored) = server.request("PUT", &format!("{base}/credentials/{field}"),
            Some(&json!({"authorization_digest":credentials["targets"][0]["authorization_digest"],"value":value})));
        assert_eq!(status, 200, "{stored}");
        assert!(!stored.to_string().contains(APP_SECRET));
    }
    let (status, enabled) = server.request(
        "POST",
        &format!("{base}/enabled"),
        Some(&json!({"enabled":true,"expected_binding":saved["binding"]})),
    );
    assert_eq!(status, 200, "{enabled}");
    wait_until("real supervised Feishu Gateway online", || {
        let (status, current) = server.request("GET", &base, None);
        assert_eq!(status, 200, "{current}");
        current["listener"] == "authenticated"
    });
    saved["binding"].clone()
}

fn confirmed(store: &SessionStore, session: &str) -> Vec<SessionEvent> {
    store
        .events("local", session, 0)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "channel.delivery.confirmed")
        .collect()
}

fn waiting(host: &Host, store: &SessionStore, session: &str, run: &str, count: usize) -> Value {
    let mut saved = Value::Null;
    wait_until(
        "Feishu platform reply confirmed by Host and assistant waiting",
        || {
            saved = host.record(run);
            assert_ne!(saved["projection"]["outcome"], "failed", "{saved}");
            saved["results"]["reply"]
                .as_array()
                .is_some_and(|results| results.len() == count)
                && saved["frontier"][0]["node_id"] == "wait_input"
                && confirmed(store, session).len() == count
        },
    );
    saved
}

#[test]
#[ignore = "requires pinned Goose, Bubblewrap, relay and sibling Feishu Gateway binary; all endpoints are loopback fixtures"]
fn private_text_roundtrip_reuses_run_and_native_history_without_duplicate_delivery() {
    let first_gate = Gate::new();
    let activity_gate = Gate::new();
    let second_gate = Gate::new();
    let provider = Provider::new("goose-feishu-private-roundtrip", vec![
        command("set -eu; test ! -e state.txt; printf first > state.txt; printf feishu-first-tool-success")
            .expect_request_contains(FIRST_INPUT).gated(&first_gate),
        Step::tool("final_result", json!({"summary":FIRST_REPLY,"route":"reply","files":[]}))
            .after("feishu-first-tool-success").gated(&activity_gate),
        command("set -eu; test \"$(cat state.txt)\" = first; printf second >> state.txt; printf feishu-second-tool-success")
            .expect_request_contains(SECOND_INPUT),
        Step::tool("final_result", json!({"summary":SECOND_REPLY,"route":"reply","files":[]}))
            .after("feishu-second-tool-success").gated(&second_gate),
    ]);
    let platform = Platform::new();
    let host = Host::new(&json!({"entry":"work","ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]}))
        .default_runtime().with_extra_environment([("ANCHOR_FEISHU_API_BASE_URL", platform.api_base_url.as_str())]);
    let (gateway, installed) = install_plugin(&host);
    let server = host.serve(&provider);
    let binding = configure_bot(&server);
    assert!(provider.requests().is_empty());
    assert_eq!(platform.snapshot()["connected"], true);

    platform.send(1, FIRST_MESSAGE, FIRST_INPUT);
    // Platform admission must not wait for the model or the final reply.
    first_gate.wait_entered();
    let first_ack = platform.wait_ack(1);
    let store =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    let sessions = store.list("local").unwrap();
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(session.graph, GRAPH);
    assert_eq!(session.channel["source"], "feishu");
    assert_eq!(session.channel["sender_id"], USER);
    assert_eq!(session.channel["conversation_id"], CHAT);
    assert_eq!(session.run_ids.len(), 1);
    let run = &session.run_ids[0];
    let first_turn = store.list_turns("local", &session.id).unwrap();
    assert_eq!(first_turn.len(), 1);
    assert_eq!(first_turn[0].status, TurnStatus::Running);
    assert_eq!(first_turn[0].prompt.as_deref(), Some(FIRST_INPUT));
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(platform.snapshot()["replies"], json!([]));
    wait_until(
        "initial Feishu activity card while model response is pending",
        || {
            !platform.snapshot()["progress"]
                .as_array()
                .unwrap()
                .is_empty()
        },
    );
    first_gate.open();
    activity_gate.wait_entered();
    wait_until(
        "real Goose tool lifecycle patched into Feishu before final reply",
        || {
            platform.snapshot()["progress"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| {
                    event["operation"] == "patch"
                        && event["body"]["content"]
                            .as_str()
                            .is_some_and(|content| content.contains("工作区命令已结束"))
                })
        },
    );
    assert_eq!(platform.snapshot()["replies"], json!([]));
    let activity = platform.snapshot()["progress"].to_string();
    for private in [
        "state.txt",
        "printf",
        "first-tool-success",
        "anchor_run",
        APP_SECRET,
        "fixture-tenant-token",
    ] {
        assert!(
            !activity.contains(private),
            "private tool detail in activity: {private}"
        );
    }
    activity_gate.open();

    let first_saved = waiting(&host, &store, &session.id, run, 1);
    let first_card = platform.snapshot()["progress"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["operation"] == "create" && event["reply_to"] == FIRST_MESSAGE)
        .unwrap()["message_id"]
        .clone();
    wait_until(
        "the activity card becomes the confirmed final answer without another message",
        || {
            platform.snapshot()["replies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reply| reply["message_id"] == first_card && reply["operation"] == "patch")
        },
    );
    let first_native = host.native_fact(run, "assistant", 1);
    assert_eq!(platform.snapshot()["replies"].as_array().unwrap().len(), 1);
    assert_eq!(provider.requests().len(), 2);
    // A new transport identity does not turn one platform message into another Turn.
    platform.send(2, FIRST_MESSAGE, FIRST_INPUT);
    let duplicate_ack = platform.wait_ack(2);

    platform.send(3, SECOND_MESSAGE, SECOND_INPUT);
    let second_ack = platform.wait_ack(3);
    second_gate.wait_entered();
    wait_until("second turn has its own temporary progress card", || {
        platform.snapshot()["progress"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["operation"] == "create" && event["reply_to"] == SECOND_MESSAGE)
    });
    second_gate.open();
    let second_saved = waiting(&host, &store, &session.id, run, 2);
    let second_native = host.native_fact(run, "assistant", 2);
    provider.assert_consumed();
    assert_eq!(provider.requests().len(), 4);
    assert_eq!(store.list("local").unwrap().len(), 1);
    assert_eq!(
        store.get("local", &session.id).unwrap().run_ids,
        std::slice::from_ref(run)
    );
    let turns = store.list_turns("local", &session.id).unwrap();
    assert_eq!(turns.len(), 2);
    assert!(
        turns
            .iter()
            .all(|turn| turn.status == TurnStatus::Completed)
    );
    assert_eq!(first_native["session_id"], second_native["session_id"]);
    assert_eq!(
        first_native["conversation_scope"],
        second_native["conversation_scope"]
    );
    assert_eq!(
        fs::read(host.base.workspace(run, "assistant").join("state.txt")).unwrap(),
        b"firstsecond"
    );
    let requests = provider.requests();
    assert!(requests[2]["messages"].to_string().contains(FIRST_INPUT));
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("feishu-first-tool-success")
    );
    for request in &requests {
        goose::tool_definition(request, "anchor_run").unwrap();
        goose::tool_definition(request, "final_result").unwrap();
        for unavailable in [
            "wecom_send_message",
            "wecom_send_file",
            "wecom_attach_image",
        ] {
            assert!(goose::tool_definition(request, unavailable).is_err());
        }
        assert!(!request.to_string().contains(APP_SECRET));
        assert!(!request.to_string().contains("fixture-tenant-token"));
    }
    let native_history = host.native_conversation(run, "assistant", 2);
    assert!(native_history.to_string().contains(FIRST_INPUT));
    assert!(native_history.to_string().contains(SECOND_INPUT));
    let confirmations = confirmed(&store, &session.id);
    for (event, text) in confirmations.iter().zip([FIRST_REPLY, SECOND_REPLY]) {
        assert_eq!(event.data["kind"], "feishu_reply");
        assert!(
            event.data["key"]
                .as_str()
                .unwrap()
                .starts_with("channel-feishu-reply:")
        );
        assert_eq!(
            event.data["content_sha256"],
            format!("{:x}", Sha256::digest(text.as_bytes()))
        );
    }
    let facts = platform.snapshot();
    assert_eq!(facts["bootstrap_requests"], 1);
    assert_eq!(facts["replies"].as_array().unwrap().len(), 2);
    assert_eq!(facts["replies"][0]["reply_to"], FIRST_MESSAGE);
    assert_eq!(facts["replies"][1]["reply_to"], SECOND_MESSAGE);
    for reply in facts["replies"].as_array().unwrap() {
        assert_eq!(reply["operation"], "patch");
        assert_eq!(
            facts["progress"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|event| {
                    event["operation"] == "create"
                        && event["message_id"] == reply["message_id"]
                        && event["reply_to"] == reply["reply_to"]
                })
                .count(),
            1
        );
    }
    assert_eq!(facts["recalls"], json!([]));
    assert!(
        store
            .list_unfinished_channel_deliveries("local", &session.id)
            .unwrap()
            .is_empty()
    );
    host.evidence_report(&provider, "tests/goose_feishu.rs", json!({
        "supervised_real_feishu_gateway":true,"loopback_binary_websocket_and_rest":true,
        "ack_before_model_response":first_ack,"duplicate_message_ack":duplicate_ack,"second_ack":second_ack,
        "same_run":run,"same_session":session.id,"same_native_session":second_native["session_id"],
        "dynamic_native_tool_activity_before_final_reply":true,"markdown_schema_2_reply":true,
        "confirmed_final_answer_replaces_its_progress_card":true,
        "duplicate_model_requests":0,"duplicate_platform_replies":0,"real_channel_deliveries":0
    }), json!({
        "gateway_binary_sha256":goose::digest(&gateway),"installed_plugin":installed,"binding":binding,
        "graph":graph(),"platform":facts,"first_run":first_saved,"run":second_saved,"turns":turns,
        "confirmed_deliveries":confirmations,"native_facts":[first_native,second_native],
        "native_goose_conversation":native_history
    }));
}

#[test]
#[ignore = "requires pinned Goose, Bubblewrap, relay and sibling Feishu Gateway; platform and provider are loopback fixtures"]
fn private_file_is_mounted_readonly_and_generated_file_is_sent_to_the_original_user() {
    const INTRODUCTION: &str =
        "## Generated File\n\nProcessed the private attachment into `generated.txt`.";
    let provider = Provider::new("goose-feishu-file-roundtrip", vec![
        command("set -eu; test \"$(cat /in/channel/fixture-input.txt)\" = feishu-inbound-file-bytes; if printf forbidden > /in/channel/fixture-input.txt; then exit 93; fi; printf feishu-generated-file-bytes > generated.txt; printf feishu-file-inspected-and-generated")
            .expect_request_contains("fixture-input.txt"),
        Step::tool("anchor_tools", json!({"name":"feishu_send_file"})).after("feishu-file-inspected-and-generated"),
        Step::tool("anchor_tool_detail", json!({"name":"feishu_send_file"})).after("feishu_send_file"),
        Step::tool("anchor_tools_call", json!({"name":"feishu_send_file","arguments":{
            "path":"/workspace/generated.txt","content":INTRODUCTION
        }})).after("feishu_send_file"),
        Step::tool("final_result", json!({"summary":FILE_REPLY,"route":"reply","files":["generated.txt"]})).after("file_status"),
    ]);
    let platform = Platform::new();
    let host = Host::new(&json!({"entry":"work","ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]}))
        .default_runtime().with_extra_environment([("ANCHOR_FEISHU_API_BASE_URL", platform.api_base_url.as_str())]);
    let (gateway, installed) = install_plugin(&host);
    let server = host.serve(&provider);
    configure_bot(&server);
    platform.send_content(
        1,
        FILE_MESSAGE,
        "file",
        json!({"file_key":FILE_KEY,"file_name":"fixture-input.txt"}),
    );
    let ack = platform.wait_ack(1);
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    wait_until("file Turn admitted into original private Session", || {
        sessions
            .list("local")
            .unwrap()
            .first()
            .is_some_and(|session| !session.run_ids.is_empty())
    });
    let session = sessions.list("local").unwrap().remove(0);
    let run = &session.run_ids[0];
    let completed = waiting(&host, &sessions, &session.id, run, 1);
    provider.assert_consumed();
    assert_eq!(sessions.list("local").unwrap().len(), 1);
    assert_eq!(session.channel["sender_id"], USER);
    assert_eq!(session.channel["conversation_id"], CHAT);
    assert_eq!(
        fs::read(host.base.workspace(run, "assistant").join("generated.txt")).unwrap(),
        OUTBOUND_FILE
    );
    let facts = platform.snapshot();
    assert_eq!(facts["downloads"].as_array().unwrap().len(), 1);
    assert_eq!(facts["uploads"].as_array().unwrap().len(), 1);
    let upload = facts["uploads"][0]["wire"].as_str().unwrap();
    assert!(upload.contains("generated.txt"));
    assert!(upload.contains(std::str::from_utf8(OUTBOUND_FILE).unwrap()));
    assert_eq!(facts["files"].as_array().unwrap().len(), 1);
    assert_eq!(facts["files"][0]["receive_id"], USER);
    assert_eq!(facts["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(
        markdown(&facts["notifications"][0])["body"]["elements"][0]["content"],
        INTRODUCTION
    );
    assert_eq!(facts["replies"].as_array().unwrap().len(), 1);
    // Durable message identity suppresses replay of both the resource fetch and
    // the model/file send when the platform changes only its transport frame.
    platform.send_content(
        2,
        FILE_MESSAGE,
        "file",
        json!({"file_key":FILE_KEY,"file_name":"fixture-input.txt"}),
    );
    platform.wait_ack(2);
    assert_eq!(
        platform.snapshot()["downloads"].as_array().unwrap().len(),
        1
    );
    assert_eq!(platform.snapshot()["files"].as_array().unwrap().len(), 1);
    for request in provider.requests() {
        assert!(!request.to_string().contains(APP_SECRET));
        assert!(!request.to_string().contains("fixture-tenant-token"));
    }
    host.evidence_report(&provider, "tests/goose_feishu.rs", json!({
        "private_received_file":true,"readonly_channel_mount":true,"same_private_recipient":true,
        "file_upload_and_send_confirmed":true,"markdown_introduction":true,"markdown_reply":true,
        "duplicate_media_does_not_download_execute_or_send":true,"durable_admission_ack":ack,
        "real_model_requests":0,"real_feishu_requests":0
    }), json!({"gateway_binary_sha256":goose::digest(&gateway),"installed_plugin":installed,
        "platform":facts,"run":completed,"session":session,"native":host.native_fact(run,"assistant",1)}));
}

#[test]
#[ignore = "requires pinned Goose, Bubblewrap, relay and sibling Feishu Gateway; platform and provider are loopback fixtures"]
fn native_schedule_completion_notifies_the_original_feishu_private_chat_with_a_confirmed_gateway_fact()
 {
    const NOTICE: &str =
        "The scheduled fixture forecast is ready. This is test data, not real weather.";
    const FILE_NOTICE: &str = "## Forecast File\n\nThe immutable fixture result is attached.";
    let task_gate = Gate::new();
    let provider = Provider::new("goose-feishu-scheduled-notice", vec![
        Step::tool("anchor_tools", json!({"name":"graph_tasks_schedule_create"})).expect_request_contains(FIRST_INPUT),
        Step::tool("anchor_tool_detail", json!({"name":"graph_tasks_schedule_create"})).after("graph_tasks_schedule_create"),
        Step::tool("anchor_tools_call", json!({"name":"graph_tasks_schedule_create","arguments":{
            "graph":"forecast-source","rule":{"type":"interval","seconds":2},"remaining":1,"input":{"city":"fixture-city"}
        }})).after("graph_tasks_schedule_create"),
        Step::tool("final_result", json!({"summary":FIRST_REPLY,"route":"reply","files":[]})).after("next_at"),
        command("set -eu; test \"$(cat /in/tasks/*/1-forecast.txt)\" = fixture-forecast; printf scheduled-forecast-inspected").expect_request_contains("task_result").gated(&task_gate),
    ]);
    let platform = Platform::new();
    let host = Host::new(&json!({"entry":"work","ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]}))
        .default_runtime().with_extra_environment([("ANCHOR_FEISHU_API_BASE_URL", platform.api_base_url.as_str())]);
    let (gateway, installed) = install_plugin(&host);
    fixture::install_system_plugin(
        host.base.root.path(),
        anchor_graph_host::SystemPlugin::GraphTasks,
    );
    let server = host.serve(&provider);
    let (status, source) = server.request("POST", "/graphs", Some(&json!({"name":"forecast-source","definition":{
        "objective":"Produce deterministic fixture data", "entry":"produce",
        "ops":{"produce":{"run":"sh -c 'printf fixture-forecast > forecast.txt'","writes":["forecast.txt"]}},
        "nodes":[{"id":"produce","op":"produce"}],"edges":[],
    }})));
    assert_eq!(status, 201, "{source}");
    let mut definition = graph();
    definition["nodes"][1]["plugins"] =
        json!(["feishu",{"id":"graph-tasks","graph_tasks":{"graphs":["forecast-source"]}}]);
    configure_bot_definition(&server, &definition);
    let (status, approved) = server.request(
        "PUT",
        &format!("/graphs/{GRAPH}"),
        Some(&json!({"definition":definition})),
    );
    assert_eq!(status, 200, "{approved}");
    platform.send(1, FIRST_MESSAGE, FIRST_INPUT);
    platform.wait_ack(1);
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    let mut session = Value::Null;
    wait_until("Feishu initial Session", || {
        let saved = sessions.list("local").unwrap();
        if let Some(saved) = saved.first() {
            session = serde_json::to_value(saved).unwrap();
        }
        !session["run_ids"].is_null() && !session["run_ids"].as_array().unwrap().is_empty()
    });
    let session_id = session["id"].as_str().unwrap();
    let run = session["run_ids"][0].as_str().unwrap();
    task_gate.wait_entered();
    let pending = sessions.pending_task_results("local", session_id).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].files.len(), 1);
    let file_path = format!("/in/tasks/{}/{}", pending[0].id, pending[0].files[0].name);
    provider.append(vec![
        Step::tool("anchor_tools", json!({"name":"feishu_send_message"})).after("scheduled-forecast-inspected"),
        Step::tool("anchor_tool_detail", json!({"name":"feishu_send_message"})).after("feishu_send_message"),
        Step::tool("anchor_tools_call", json!({"name":"feishu_send_message","arguments":{"content":NOTICE}})).after("feishu_send_message"),
        Step::tool("anchor_tools", json!({"name":"feishu_send_file"})).after("confirmed"),
        Step::tool("anchor_tool_detail", json!({"name":"feishu_send_file"})).after("feishu_send_file"),
        Step::tool("anchor_tools_call", json!({"name":"feishu_send_file","arguments":{
            "path":file_path,"content":FILE_NOTICE
        }})).after("feishu_send_file"),
        Step::tool("final_result", json!({"summary":"Notification and immutable file confirmed; task processed.","route":"reply","files":[]})).after("file_status"),
    ]);
    task_gate.open();
    let mut completed = Value::Null;
    wait_until(
        "real Scheduler -> producer -> task result -> Feishu REST notification",
        || {
            completed = host.record(run);
            assert_ne!(completed["projection"]["outcome"], "failed", "{completed}");
            completed["results"]["reply"]
                .as_array()
                .is_some_and(|replies| replies.len() == 2)
                && completed["frontier"][0]["node_id"] == "wait_input"
        },
    );
    provider.assert_consumed();
    let input = &completed["results"]["wait_input"][1]["completion"]["output"];
    assert_eq!(input["source"], "task_result");
    assert!(input.get("channel").is_none());
    assert_eq!(input["task_result"]["outcome"], "done");
    let producer = input["task_result"]["source_run"].as_str().unwrap();
    assert_eq!(
        host.record(producer)["input"],
        json!({"city":"fixture-city"})
    );
    let metadata = fixture::read_json(
        host.base
            .state_root()
            .join("run-metadata")
            .join(format!("{producer}.json")),
    );
    assert_eq!(metadata["task"]["session"], session_id);
    assert_eq!(metadata["task"]["source"]["kind"], "agent_delegation");
    assert!(metadata["schedule"].as_str().is_some());
    assert_eq!(
        completed["results"]["reply"][1]["completion"]["output"]["task_result_handled"],
        true
    );
    assert_eq!(
        completed["results"]["reply"][1]["completion"]["output"]["suppressed"],
        true
    );
    assert!(
        sessions
            .pending_task_results("local", session_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(sessions.list_turns("local", session_id).unwrap().len(), 2);
    assert_eq!(confirmed(&sessions, session_id).len(), 1);
    let facts = platform.snapshot();
    assert_eq!(facts["replies"].as_array().unwrap().len(), 1);
    assert_eq!(facts["notifications"].as_array().unwrap().len(), 2);
    assert_eq!(
        markdown(&facts["notifications"][0])["body"]["elements"][0]["content"],
        NOTICE
    );
    assert_eq!(
        markdown(&facts["notifications"][1])["body"]["elements"][0]["content"],
        FILE_NOTICE
    );
    assert_eq!(facts["files"].as_array().unwrap().len(), 1);
    assert_eq!(facts["uploads"].as_array().unwrap().len(), 1);
    let upload = facts["uploads"][0]["wire"].as_str().unwrap();
    assert!(upload.contains("forecast.txt"));
    assert!(!upload.contains("1-forecast.txt"));
    assert!(upload.contains("fixture-forecast"));
    let database = host
        .base
        .state_root()
        .join("channels/graphs")
        .join(&format!("{:x}", Sha256::digest(GRAPH.as_bytes()))[..16])
        .join("delivery.sqlite");
    let ledger =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let (id, status): (String, String) = ledger
        .query_row(
            "SELECT request_id,status FROM deliveries WHERE kind='send'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(id.starts_with("channel-task-"));
    assert_eq!(status, "confirmed");
    let file_status: String = ledger
        .query_row(
            "SELECT status FROM deliveries WHERE kind='send' AND request_id LIKE '%-file'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(file_status, "confirmed");
    let first_native = host.native_fact(run, "assistant", 1);
    let task_native = host.native_fact(run, "assistant", 2);
    assert_eq!(first_native["session_id"], task_native["session_id"]);
    for request in provider.requests() {
        assert!(!request.to_string().contains(APP_SECRET));
        assert!(!request.to_string().contains("fixture-tenant-token"));
    }
    let (status, stopped) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{stopped}");
    server.wait_status(run, "stopped");
    host.evidence_report(&provider, "tests/goose_feishu.rs", json!({
        "native_schedule_create":true,"real_scheduler_and_producer":true,"original_private_recipient":true,
        "confirmed_gateway_notification":id,"same_session_and_run":true,"same_native_session":true,
        "immutable_task_file_confirmed_with_original_filename":true,
        "no_automatic_duplicate_reply":true,"real_model_requests":0,"real_feishu_requests":0,
    }), json!({"gateway_binary_sha256":goose::digest(&gateway),"installed_plugin":installed,
        "platform":facts,"run":completed,"task_metadata":metadata,"native_facts":[first_native,task_native]}));
}
