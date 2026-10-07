mod support;

use std::{
    io::Read,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    time::Duration,
};

use anchor_wecom_gateway::{Gateway, GatewayConfig, GatewayError};
use rustix::process::{Pid, Signal, kill_process};
use serde_json::{Value, json};
use support::{Platform, TOKEN, config, control, send_request};
use tokio::{io::AsyncWriteExt, net::UnixStream};

const BINARY: &str = env!("CARGO_BIN_EXE_anchor-wecom-gateway");

struct Process(Child);

impl Process {
    fn start(settings: &GatewayConfig) -> Self {
        let child = Command::new(BINARY)
            .env_clear()
            .current_dir(settings.state_dir.parent().unwrap())
            .env("WECOM_CHANNEL_STATE", &settings.state_dir)
            .env("WECOM_BOT_ID", &settings.bot_id)
            .env("WECOM_BOT_SECRET", &settings.secret)
            .env("ANCHOR_CHANNEL_CONTROL_TOKEN", &settings.control_token)
            .env("ANCHOR_WECOM_WS_URL", &settings.ws_url)
            .env("ANCHOR_WECOM_USERS", "alice,bob")
            .env("ANCHOR_WECOM_SEND_USERS", "alice,bob")
            .env("ANCHOR_WECOM_ACK_MS", "150")
            .env("ANCHOR_WECOM_HEARTBEAT_MS", "25")
            .env("ANCHOR_WECOM_CONNECT_MS", "500")
            .env("ANCHOR_WECOM_RECONNECT_MS", "20")
            .env("ANCHOR_WECOM_RECONNECT_MAX_MS", "100")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self(child)
    }

    async fn stop(&mut self, signal: Signal) -> ExitStatus {
        kill_process(Pid::from_raw(self.0.id() as i32).unwrap(), signal).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }

    fn stderr(&mut self) -> String {
        let mut value = String::new();
        self.0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut value)
            .unwrap();
        value
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn descriptor(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn cli_kill_after_dispatch_keeps_unknown_and_sigterm_restart_cleans_private_control() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settings = config(root.path(), &platform, None);
    let mut first = Process::start(&settings);
    platform.next("aibot_subscribe").await;
    platform.next("ping").await;
    assert_eq!(descriptor(&settings.descriptor_path())["token"], TOKEN);
    assert!(matches!(
        Gateway::start(config(root.path(), &platform, None)).await,
        Err(GatewayError::AlreadyRunning)
    ));
    let request = send_request("killed-send", "alice", "uncertain across process restart");
    let mut payload = request.clone();
    payload["token"] = json!(TOKEN);
    let mut bytes = serde_json::to_vec(&payload).unwrap();
    bytes.push(b'\n');
    let mut client = UnixStream::connect(settings.socket_path()).await.unwrap();
    client.write_all(&bytes).await.unwrap();
    platform.next("aibot_send_msg").await;
    assert!(!first.stop(Signal::KILL).await.success());
    drop(client);
    let mut rotated = config(root.path(), &platform, None);
    rotated.control_token = "rotated-cli-token-000000000000000000000000".into();
    let mut second = Process::start(&rotated);
    platform.next("aibot_subscribe").await;
    platform.next("ping").await;
    assert_eq!(
        descriptor(&rotated.descriptor_path())["token"],
        rotated.control_token
    );
    let result = control(&rotated.socket_path(), &rotated.control_token, request).await;
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("previous delivery is unconfirmed")
    );
    platform.no_command("aibot_send_msg").await;
    let connection = rusqlite::Connection::open_with_flags(
        rotated.state_dir.join("delivery.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let status: String = connection
        .query_row(
            "SELECT status FROM deliveries WHERE request_id='killed-send'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "unknown");
    drop(connection);
    assert!(second.stop(Signal::TERM).await.success());
    assert!(!rotated.socket_path().exists());
    assert!(!rotated.descriptor_path().exists());
    for stderr in [first.stderr(), second.stderr()] {
        for secret in [&rotated.bot_id, &rotated.secret, &rotated.control_token] {
            assert!(!stderr.contains(secret));
        }
    }
}

#[tokio::test]
async fn cli_rejects_legacy_events_ledger_without_taking_over_or_leaking_secrets() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settings = config(root.path(), &platform, None);
    std::fs::create_dir(&settings.state_dir).unwrap();
    std::fs::set_permissions(&settings.state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let legacy = settings.state_dir.join("events.sqlite");
    std::fs::write(&legacy, "legacy-sentinel-preserved").unwrap();
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut process = Process::start(&settings);
    let status = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(status) = process.0.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!status.success());
    let stderr = process.stderr();
    assert!(stderr.contains("legacy Python ledger cannot become native gateway state"));
    assert!(!stderr.contains(&settings.secret));
    assert!(!stderr.contains(&settings.control_token));
    assert_eq!(
        std::fs::read_to_string(&legacy).unwrap(),
        "legacy-sentinel-preserved"
    );
    assert!(!settings.descriptor_path().exists());
    assert!(!settings.socket_path().exists());
    platform.no_command("aibot_subscribe").await;
}
