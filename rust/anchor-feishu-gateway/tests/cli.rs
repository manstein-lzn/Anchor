mod support;

use std::{
    io::Read,
    process::{Child, Command, Stdio},
    time::Duration,
};

use rustix::process::{Pid, Signal, kill_process};
use serde_json::Value;
use support::{Fixture, TOKEN, control, poll};

const BINARY: &str = env!("CARGO_BIN_EXE_anchor-feishu-gateway");

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn cli_environment_connects_and_sigterm_releases_the_private_control_lease() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    let config = fixture.config(root.path());
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("FEISHU_CHANNEL_STATE", &config.state_dir)
        .env("FEISHU_APP_ID", &config.app_id)
        .env("FEISHU_APP_SECRET", &config.app_secret)
        .env("ANCHOR_CHANNEL_CONTROL_TOKEN", TOKEN)
        .env("ANCHOR_CHANNEL_CONTROL_SOCKET", config.socket_path())
        .env(
            "ANCHOR_CHANNEL_CONTROL_DESCRIPTOR",
            config.descriptor_path(),
        )
        .env("ANCHOR_FEISHU_USERS", "ou_alice")
        .env("ANCHOR_FEISHU_SEND_USERS", "ou_alice")
        .env("ANCHOR_FEISHU_API_BASE_URL", &fixture.base)
        .env(
            "ANCHOR_CHANNEL_WEBHOOK_URL",
            &config.webhook.as_ref().unwrap().url,
        )
        .env("ANCHOR_API_KEY", "fixture-host-key")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut process = Process(command.spawn().unwrap());
    fixture.next("ping").await;
    let descriptor: Value =
        serde_json::from_slice(&std::fs::read(config.descriptor_path()).unwrap()).unwrap();
    assert_eq!(descriptor["token"], TOKEN);
    assert_eq!(
        control(&config.socket_path(), TOKEN, "status").await["status"],
        "authenticated"
    );
    fixture
        .event("om_process", "hello from a real child process")
        .await;
    assert_eq!(fixture.ack().await["code"], 200);
    fixture.wait_settlements(1).await;
    kill_process(Pid::from_raw(process.0.id() as i32).unwrap(), Signal::TERM).unwrap();
    let mut exit = None;
    poll(|| {
        exit = process.0.try_wait().unwrap();
        exit.is_some()
    })
    .await;
    assert!(exit.unwrap().success());
    assert!(!config.socket_path().exists());
    assert!(!config.descriptor_path().exists());
    let mut stderr = String::new();
    process
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    for value in [&config.app_secret, &config.app_id, &config.control_token] {
        assert!(!stderr.contains(value));
    }
    assert!(!stderr.contains("hello from a real child process"));
    // Exit really released the directory lease, rather than only deleting its
    // visible control files.
    let mut second = Process(command.spawn().unwrap());
    poll(|| {
        fixture
            .shared
            .connections
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 2
    })
    .await;
    assert_eq!(
        control(&config.socket_path(), TOKEN, "status").await["status"],
        "authenticated"
    );
    kill_process(Pid::from_raw(second.0.id() as i32).unwrap(), Signal::TERM).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(exit) = second.0.try_wait().unwrap() {
                assert!(exit.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn cli_fails_without_configuration_and_does_not_accept_credential_arguments() {
    let output = Command::new(BINARY).env_clear().output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("required gateway environment is missing")
    );
    let output = Command::new(BINARY)
        .env_clear()
        .arg("--secret=fixture-do-not-echo")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("configuration only through its environment"));
    assert!(!stderr.contains("fixture-do-not-echo"));
}
