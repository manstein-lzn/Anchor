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
        bind_bot(host, &self.descriptor)
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
fn scheduled_task_uses_selected_bot_with_legacy_owner_and_platform_ack() {
    let provider = Provider::new(
        "goose-schedule-selected-bot",
        vec![
            Step::tool(
                SEND_TOOL,
                json!({"bot_graph":"fixture","userid":USER,"content":CONTENT}),
            ),
            command("printf scheduled-gateway-confirmed > evidence.txt; cat evidence.txt")
                .after("accepted"),
            complete("done").after("scheduled-gateway-confirmed"),
        ],
    );
    let gateway = Transport::new();
    let host = gateway.bind_host(Host::new(&graph()).default_runtime());
    // Model the installation that previously used API key login. Its persisted
    // principal differs from the credential binding scope recorded on Runs.
    let owner = format!("api:{}", "a".repeat(64));
    let platform = host.base.state_root().join("platform");
    fs::create_dir_all(&platform).unwrap();
    fs::set_permissions(&platform, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        platform.join("installation-owner.json"),
        json!({"format":1,"owner":owner}).to_string(),
    )
    .unwrap();
    let task = host.base.root.path().join("notification-task");
    fs::create_dir_all(&task).unwrap();
    fs::write(task.join("graph.json"), json!({
        "objective":"Send the authorized scheduled notification through the explicitly selected bot",
        "entry":"worker",
        "agents":{"worker":{"model":goose::MODEL,"instructions":"Use input.bot_graph and input.userid to send the authorized notification. Inspect its actual receipt, write evidence.txt, then finish with route done.","network":false,"wall_time_limit_seconds":40}},
        "ops":{"done":{"run":"true"}},
        "nodes":[{"id":"worker","agent":"worker","plugins":["wecom"]},{"id":"done","op":"done"}],
        "edges":[{"from":"worker","to":"done"}]
    }).to_string()).unwrap();
    fs::write(
        task.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[{"id":"wecom"}]}).to_string(),
    )
    .unwrap();
    let server = host.serve(&provider);
    let at = (chrono::Utc::now() + chrono::Duration::seconds(4))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    let (status, created) = server.request(
        "POST",
        "/schedules",
        Some(&json!({
            "graph":"notification-task","rule":{"type":"once","at":at},
            "input":{"bot_graph":"fixture","userid":USER}
        })),
    );
    assert_eq!(status, 201, "{created}");
    let schedule = &created["schedule"]["id"];
    let mut run = None;
    goose::wait_until("scheduled notification Run", || {
        let (status, listed) = server.request("GET", "/runs", None);
        assert_eq!(status, 200, "{listed}");
        run = listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["trigger"]["schedule"] == *schedule)
            .and_then(|run| run["run"].as_str())
            .map(str::to_owned);
        run.is_some()
    });
    let run = run.unwrap();
    server.wait_status(&run, "completed");
    provider.assert_consumed();
    let metadata: Value = serde_json::from_slice(
        &fs::read(
            host.base
                .state_root()
                .join("run-metadata")
                .join(format!("{run}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        metadata["oauth_owner"],
        format!("{:x}", Sha256::digest(owner.as_bytes()))
    );
    assert!(metadata["assistant"].is_null());
    let schema = tool_definition(&provider.requests()[0], SEND_TOOL)
        .unwrap()
        .clone();
    assert!(
        schema["function"]["parameters"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("bot_graph"))
    );
    assert!(tool_definition(&provider.requests()[0], "wecom_attach_image").is_err());
    let frames = gateway.frames.lock().unwrap().clone();
    let deliveries = gateway.finish();
    assert_eq!(frames.len(), 1);
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].status, DeliveryStatus::Confirmed);
    let native = native_sends(&host.native_conversation(&run, "worker", 1));
    assert_eq!(native.len(), 1);
    let fact = host.native_fact(&run, "worker", 1);
    let key: InvocationKey = serde_json::from_value(fact["key"].clone()).unwrap();
    assert_eq!(
        deliveries[0].request_id,
        expected_request_id(
            &key,
            fact["session_id"].as_str().unwrap(),
            native[0]["id"].as_str().unwrap()
        )
    );
    assert_artifact(&host, &run, b"scheduled-gateway-confirmed");
    let (_, schedules) = server.request("GET", "/schedules", None);
    assert_eq!(schedules["schedules"][0]["enabled"], false);
    host.evidence(
        &provider,
        &run,
        json!({
            "case_source":"tests/support/goose_native_gateway.rs",
            "actual_schedule_ticker":true,"legacy_owner_credential_scope":true,
            "explicit_bot_graph":"fixture","task_without_session_or_network":true,
            "actual_rust_gateway":true,"platform_acks":frames,"deliveries":deliveries,
            "real_channel_deliveries":0
        }),
    );
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
            complete("reply").after("native-gateway-two-confirmed"),
        ],
    );
    let gateway = Transport::new();
    let host = gateway.bind_host(Host::new(&graph()).default_runtime());
    let plugin = install_resource_plugin(&host, &gateway.descriptor);
    let server = host.serve(&provider);
    let run = ask(&server, "goose-channel-native-gateway");
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
