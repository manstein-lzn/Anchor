//! The real gateway and ledger, with only WeCom's transport replaced locally.
use super::*;
use anchor_wecom_gateway::{Gateway, GatewayConfig, RunningGateway};
use futures_util::{SinkExt, StreamExt};
use std::sync::Mutex;
use tokio_tungstenite::{accept_async, tungstenite::Message};

pub(super) struct TaskGateway {
    pub(super) gateway: RunningGateway,
    pub(super) descriptor: PathBuf,
    frames: Arc<Mutex<Vec<Value>>>,
    withhold_introductions: Arc<AtomicBool>,
    platform: tokio::task::JoinHandle<()>,
}

impl TaskGateway {
    pub(super) async fn start(root: &Path) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let shared = frames.clone();
        let withhold_introductions = Arc::new(AtomicBool::new(false));
        let withhold = withhold_introductions.clone();
        let platform = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = accept_async(stream).await.unwrap();
                while let Some(Ok(message)) = socket.next().await {
                    let Message::Text(text) = message else {
                        continue;
                    };
                    let value: Value = serde_json::from_str(&text).unwrap();
                    let body = match value["cmd"].as_str().unwrap() {
                        "aibot_subscribe" | "ping" => json!({}),
                        "aibot_upload_media_init" => {
                            shared.lock().unwrap().push(value.clone());
                            json!({"upload_id":"fixture-upload"})
                        }
                        "aibot_upload_media_chunk" => {
                            shared.lock().unwrap().push(value.clone());
                            json!({})
                        }
                        "aibot_upload_media_finish" => {
                            shared.lock().unwrap().push(value.clone());
                            json!({"media_id":"fixture-file"})
                        }
                        "aibot_send_msg" => {
                            assert_eq!(value["body"]["chatid"], "alice");
                            shared.lock().unwrap().push(value.clone());
                            if value["body"]["msgtype"] == "markdown"
                                && withhold.load(Ordering::Relaxed)
                            {
                                continue;
                            }
                            json!({})
                        }
                        unexpected => panic!("unexpected local platform command: {unexpected}"),
                    };
                    let ack = json!({"headers":value["headers"],"errcode":0,"body":body});
                    if socket
                        .send(Message::Text(ack.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
        let mut config = GatewayConfig::new(
            root.join("gateway"),
            "fixture-bot".into(),
            "fixture-secret".into(),
            "fixture-gateway-private-control-token-000000".into(),
        );
        config.ws_url = format!("ws://{address}");
        config.send_users.insert("alice".into());
        config.send_users.insert("bob".into());
        config.timing.ack_timeout = Duration::from_millis(300);
        let descriptor = config.descriptor_path();
        let gateway = Gateway::start(config).await.unwrap();
        gateway
            .wait_authenticated(Duration::from_secs(2))
            .await
            .unwrap();
        Self {
            gateway,
            descriptor,
            frames,
            withhold_introductions,
            platform,
        }
    }

    pub(super) fn frames(&self) -> Vec<Value> {
        self.frames.lock().unwrap().clone()
    }

    pub(super) fn withhold_introductions(&self, withhold: bool) {
        self.withhold_introductions
            .store(withhold, Ordering::Relaxed);
    }

    pub(super) async fn shutdown(self) {
        self.gateway.shutdown().await.unwrap();
        self.platform.abort();
        let _ = self.platform.await;
    }
}
