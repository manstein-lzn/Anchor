use super::*;
use anchor_wecom_gateway::{DeliveryFact, DeliveryStatus, Gateway as NativeGateway, GatewayConfig};
use futures_util::{SinkExt, StreamExt};
use std::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

struct Transport {
    _root: tempfile::TempDir,
    descriptor: PathBuf,
    frames: Arc<Mutex<Vec<Value>>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<thread::JoinHandle<Vec<DeliveryFact>>>,
}

impl Transport {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("gateway");
        let descriptor = state.join("control.json");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let shared = frames.clone();
        let (ready, readiness) = std::sync::mpsc::sync_channel(1);
        let (stop, stopped) = oneshot::channel();
        let task = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    let platform = tokio::spawn(async move {
                        let (stream, _) = listener.accept().await.unwrap();
                        let mut socket = accept_async(stream).await.unwrap();
                        while let Some(message) = socket.next().await {
                            let text = match message {
                                Ok(Message::Text(text)) => text,
                                Ok(Message::Close(_)) | Err(_) => break,
                                _ => continue,
                            };
                            let value: Value = serde_json::from_str(&text).unwrap();
                            let request_id = value["headers"]["req_id"].as_str().unwrap();
                            match value["cmd"].as_str().unwrap() {
                                "aibot_subscribe" => {
                                    assert_eq!(value["body"]["bot_id"], "fixture-bot");
                                    assert_eq!(value["body"]["secret"], "fixture-secret");
                                }
                                "aibot_send_msg" => {
                                    assert_eq!(value["body"]["chatid"], USER);
                                    assert_eq!(value["body"]["msgtype"], "markdown");
                                    assert_eq!(value["body"]["markdown"]["content"], CONTENT);
                                    shared.lock().unwrap().push(value.clone());
                                }
                                "ping" => {}
                                unexpected => {
                                    panic!("unexpected fixture platform command: {unexpected}")
                                }
                            }
                            let ack = json!({"headers":{"req_id":request_id},"errcode":0});
                            if socket
                                .send(Message::Text(ack.to_string().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    });
                    let mut config = GatewayConfig::new(
                        state,
                        "fixture-bot".into(),
                        "fixture-secret".into(),
                        "fixture-native-gateway-private-token-000000".into(),
                    );
                    config.ws_url = format!("ws://{address}");
                    config.send_users.insert(USER.into());
                    let gateway = NativeGateway::start(config).await.unwrap();
                    gateway
                        .wait_authenticated(Duration::from_secs(5))
                        .await
                        .unwrap();
                    ready.send(()).unwrap();
                    let _ = stopped.await;
                    let facts = gateway.delivery_facts().unwrap();
                    gateway.shutdown().await.unwrap();
                    platform.abort();
                    if let Err(error) = platform.await {
                        assert!(
                            error.is_cancelled(),
                            "local platform fixture failed: {error}"
                        );
                    }
                    facts
                })
        });
        readiness.recv_timeout(Duration::from_secs(10)).unwrap();
        Self {
            _root: root,
            descriptor,
            frames,
            stop: Some(stop),
            task: Some(task),
        }
    }

    fn bind_host(&self, host: Host) -> Host {
        host.with_extra_environment([
            (
                "ANCHOR_CHANNEL_CONTROL_DESCRIPTOR",
                self.descriptor.as_os_str().to_owned(),
            ),
            ("ANCHOR_WECOM_SEND_USERS", USER.into()),
        ])
    }

    fn finish(mut self) -> Vec<DeliveryFact> {
        let _ = self.stop.take().unwrap().send(());
        self.task.take().unwrap().join().unwrap()
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

#[test]
#[ignore = "requires pinned Goose and Bubblewrap; WeCom transport is strictly local"]
fn native_goose_sends_through_actual_rust_gateway_and_correlates_platform_acks() {
    let provider = Provider::new(
        "goose-channel-native-gateway",
        vec![
            send(USER),
            send(USER).after("accepted"),
            command("printf native-gateway-two-confirmed > evidence.txt; cat evidence.txt")
                .after("accepted"),
            complete("verify").after("native-gateway-two-confirmed"),
            Step::text("two native gateway deliveries confirmed by the local platform"),
        ],
    );
    let gateway = Transport::new();
    let host = gateway.bind_host(Host::new(&graph()).default_runtime());
    let plugin = install_resource_plugin(&host);
    let server = host.serve(&provider);
    let run = server.trigger();
    server.wait_status(&run, "completed");
    let frames = gateway.frames.lock().unwrap().clone();
    let deliveries = gateway.finish();
    assert_eq!(frames.len(), 2);
    assert_eq!(deliveries.len(), 2);
    assert_ne!(deliveries[0].request_id, deliveries[1].request_id);
    assert_eq!(deliveries[0].content_digest, deliveries[1].content_digest);
    let history = host.native_conversation(&run, "worker", 1);
    let sends = native_sends(&history);
    assert_eq!(sends.len(), 2);
    let fact = host.native_fact(&run, "worker", 1);
    let key: InvocationKey = serde_json::from_value(fact["key"].clone()).unwrap();
    let session = fact["session_id"].as_str().unwrap();
    for ((native, delivery), frame) in sends.iter().zip(&deliveries).zip(&frames) {
        assert_eq!(delivery.kind, "send");
        assert_eq!(delivery.status, DeliveryStatus::Confirmed);
        assert_eq!(delivery.wire_request_id, frame["headers"]["req_id"]);
        assert_eq!(
            delivery.request_id,
            expected_request_id(&key, session, native["id"].as_str().unwrap())
        );
        let response = history
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().unwrap())
            .find(|block| block["type"] == "toolResponse" && block["id"] == native["id"])
            .unwrap();
        assert_eq!(response["toolResult"]["status"], "success");
        assert!(response.to_string().contains(&delivery.request_id));
    }
    assert_artifact(&host, &run, b"native-gateway-two-confirmed");
    assert_no_legacy_attempts(&host);
    host.evidence(&provider, &run, json!({
        "case_source":CASE_SOURCE,"plugin":plugin,"platform_frames":frames,"deliveries":deliveries,
        "adapter_test_source_sha256":goose::digest(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/goose_native_gateway.rs")),
        "actual_native_gateway":true,"ack_matches_wire_and_native_tool_identity":true,
        "boundary":"actual Host/Goose/native Rust gateway with local model and WeCom WebSocket fixtures; no production sends, inbound Session supervisor or media acceptance"
    }));
}
