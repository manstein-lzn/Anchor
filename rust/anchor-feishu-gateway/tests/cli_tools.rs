mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{OnceLock, atomic::Ordering},
    time::Duration,
};

use anchor_feishu_gateway::{DeliveryStatus, Gateway, GatewayConfig};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use support::{Fixture, TOKEN, control_request, poll};

fn request(id: &str, argv: &[&str]) -> Value {
    json!({"operation":"cli", "token":TOKEN, "request_id":id, "invocation":{"argv":argv}})
}

fn configured(fixture: &Fixture, root: &Path, binary: PathBuf) -> GatewayConfig {
    let mut config = fixture.config(root);
    config.cli_enabled = true;
    config.cli_binary = Some(binary);
    config.cli_network_helper = Some(PathBuf::from(
        std::env::var_os("ANCHOR_FEISHU_CLI_NETWORK_HELPER")
            .expect("set ANCHOR_FEISHU_CLI_NETWORK_HELPER to the absolute network helper path"),
    ));
    config
}

fn official() -> PathBuf {
    PathBuf::from(
        std::env::var_os("ANCHOR_FEISHU_CLI_BINARY")
            .expect("set ANCHOR_FEISHU_CLI_BINARY to the pinned static lark-cli 1.0.100"),
    )
}

fn workspaces(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root.join("state"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("cli-")
        })
        .collect()
}

#[tokio::test]
async fn authorization_and_invocation_errors_do_not_claim_or_fetch_credentials() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let mut config = fixture.config(root.path());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    let args = request("disabled", &["--help"]);
    assert!(
        control_request(&socket, args).await["error"]
            .as_str()
            .unwrap()
            .contains("cli_enabled")
    );
    assert!(gateway.delivery_facts().unwrap().is_empty());
    gateway.shutdown().await.unwrap();

    config = fixture.config(root.path());
    config.cli_enabled = true;
    let gateway = Gateway::start(config).await.unwrap();
    for argv in [
        vec!["auth", "token"],
        vec!["authen", "access_token", "create"],
        vec!["profile", "add"],
        vec!["config", "show"],
        vec!["event", "consume"],
        vec!["update"],
        vec!["--help", "auth", "--help=false"],
        vec!["--version", "profile", "--version=false"],
        vec!["--", "auth", "token"],
        vec!["im", "--as=user"],
        vec!["im", "--profile", "other"],
        vec!["im", "--debug"],
        vec![
            "api",
            "--format",
            "/open-apis/im/v1/chats",
            "GET",
            "/open-apis/auth/v3/token",
        ],
        vec!["api", "GET", "https://example.org/open-apis/im/v1/chats"],
        vec!["api", "POST", "/open-apis/auth"],
        vec!["api", "POST", "/open-apis/authen/v2/oauth/token"],
        vec!["api", "GET", "/open-apis/im/../auth/v3/token"],
        vec!["api", "GET", "/open-apis/%61uth/v3/token"],
        vec![
            "api",
            "GET",
            "/open-apis/im/v1/chats",
            "--headers",
            "Authorization: Bearer other",
        ],
        vec![
            "api",
            "GET",
            "/open-apis/im/v1/chats",
            "-HAuthorization: Bearer other",
        ],
    ] {
        let result = control_request(&socket, request("invalid", &argv)).await;
        assert!(result["error"].is_string(), "{argv:?}: {result}");
        assert!(
            !result["error"].as_str().unwrap().contains("not installed"),
            "{argv:?}: {result}"
        );
    }
    for invocation in [
        json!({"argv":["--help"], "files":[{"name":"../escape","data_base64":""}]}),
        json!({"argv":["api","--help"], "outputs":["/proc/self/environ"]}),
        json!({"argv":["--help"], "outputs":["same", "same"]}),
        json!({"argv":["--help"], "env":{"LARKSUITE_CLI_USER_ACCESS_TOKEN":"other"}}),
    ] {
        let mut args = request("invalid", &[]);
        args["invocation"] = invocation;
        assert!(control_request(&socket, args).await["error"].is_string());
    }
    let mut args = request("invalid", &["--help"]);
    args["token"] = json!("incorrect");
    assert!(
        control_request(&socket, args).await["error"]
            .as_str()
            .unwrap()
            .contains("unauthorized")
    );
    assert!(gateway.delivery_facts().unwrap().is_empty());
    assert_eq!(fixture.shared.tokens.load(Ordering::SeqCst), 0);
    gateway.shutdown().await.unwrap();

    config = fixture.config(root.path());
    config.cli_enabled = true;
    config.cli_binary = Some(std::env::current_exe().unwrap());
    let gateway = Gateway::start(config).await.unwrap();
    assert!(
        control_request(&socket, request("no-helper", &["--help"])).await["error"]
            .as_str()
            .unwrap()
            .contains("network helper")
    );
    assert!(gateway.delivery_facts().unwrap().is_empty());
    assert_eq!(fixture.shared.tokens.load(Ordering::SeqCst), 0);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn token_failure_or_shutdown_before_token_does_not_claim() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let mut config = fixture.config(root.path());
    config.cli_enabled = true;
    // These executables are never reached: this test stops at token acquisition.
    config.cli_binary = Some(std::env::current_exe().unwrap());
    config.cli_network_helper = config.cli_binary.clone();
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    fixture.shared.token_failure.store(true, Ordering::SeqCst);
    assert!(control_request(&socket, request("failed", &["--help"])).await["error"].is_string());
    fixture.shared.token_failure.store(false, Ordering::SeqCst);
    fixture.shared.token_blocked.store(true, Ordering::SeqCst);
    let pending =
        tokio::spawn(
            async move { control_request(&socket, request("cancelled", &["--help"])).await },
        );
    tokio::time::timeout(
        Duration::from_secs(3),
        fixture.shared.token_entered.notified(),
    )
    .await
    .unwrap();
    assert!(gateway.delivery_facts().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
    assert!(pending.await.unwrap()["error"].is_string());
    assert!(workspaces(root.path()).is_empty());
}

// A static Rust process exercises the real sandbox, independently of lark-cli's
// own path/URL checks. No shell, loader, host library or test credential file is
// mounted. The opt-in compiler must have the matching Linux musl target.
fn fixture_binary() -> PathBuf {
    static BUILD: OnceLock<tempfile::TempDir> = OnceLock::new();
    let root = BUILD.get_or_init(|| {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fixture.rs");
        fs::write(&source, r#"
use std::{env, fs, io::{Read, Write}, net::{SocketAddr, TcpStream}, time::Duration};
fn main() {
    let args: Vec<_> = env::args().collect();
    match args[1].as_str() {
        "probe" => {
            assert_eq!(env::current_dir().unwrap().to_str().unwrap(), "/workspace");
            assert_eq!(env::var("LARKSUITE_CLI_APP_ID").unwrap(), "cli_fixture_app");
            assert_eq!(env::var("LARKSUITE_CLI_TENANT_ACCESS_TOKEN").unwrap(), "fixture-tenant-token");
            for (key, _) in env::vars() {
                assert!(["HOME", "PWD", "TMPDIR", "SSL_CERT_FILE", "LARKSUITE_CLI_APP_ID",
                    "LARKSUITE_CLI_TENANT_ACCESS_TOKEN", "LARKSUITE_CLI_BRAND",
                    "LARKSUITE_CLI_DEFAULT_AS", "LARKSUITE_CLI_STRICT_MODE",
                    "LARKSUITE_CLI_NO_UPDATE_NOTIFIER", "LARKSUITE_CLI_NO_SKILLS_NOTIFIER"].contains(&key.as_str()), "{key}");
            }
            for path in [args[2].as_str(), "/proc/self/environ", "/bin/sh", "/etc/passwd", "/etc/pki"] {
                assert!(fs::metadata(path).is_err(), "{path}");
            }
            for ip in ["127.0.0.1", "10.0.2.2"] {
                let addr: SocketAddr = format!("{ip}:{}", args[3]).parse().unwrap();
                assert!(TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err());
            }
            println!("isolated");
        }
        "io" => {
            let mut bytes = fs::read("input.bin").unwrap();
            std::io::stdin().read_to_end(&mut bytes).unwrap();
            fs::write("result.bin", bytes).unwrap();
            println!("partial fixture-tenant-token");
            eprintln!("diagnostic fixture-tenant-token");
            std::process::exit(7);
        }
        "ignore" => { println!("invalid arguments"); std::process::exit(2); }
        "stdout" | "stderr" | "wait" => {
            fs::write("ready", b"ready").unwrap();
            if args[1] == "stdout" { std::io::stdout().write_all(&vec![b'x'; 48 * 1024 + 1]).unwrap(); }
            if args[1] == "stderr" { std::io::stderr().write_all(&vec![b'x'; 8193]).unwrap(); }
            std::thread::sleep(Duration::from_secs(3600));
        }
        "output" => match args[2].as_str() {
            "link" => std::os::unix::fs::symlink("/missing-target", "result.bin").unwrap(),
            "directory" => fs::create_dir("result.bin").unwrap(),
            "large" => fs::File::create("result.bin").unwrap().set_len(20 * 1024 * 1024 + 1).unwrap(),
            "token" => fs::write("result.bin", env::var("LARKSUITE_CLI_TENANT_ACCESS_TOKEN").unwrap()).unwrap(),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}
"#).unwrap();
        let compiler = std::env::var_os("ANCHOR_FEISHU_FIXTURE_RUSTC").unwrap_or_else(|| "rustc".into());
        let result = Command::new(compiler).arg("--edition=2024").arg("--target")
            .arg(format!("{}-unknown-linux-musl", std::env::consts::ARCH))
            .args(["-O", "-o"]).arg(root.path().join("fixture")).arg(source).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        root
    });
    root.path().join("fixture")
}

#[tokio::test]
#[ignore = "requires bwrap, slirp4netns, network helper and a musl Rust compiler"]
async fn sandbox_has_only_bot_token_staged_files_and_no_host_loopback() {
    let root = tempfile::tempdir().unwrap();
    let sentinel = root.path().join("host-only");
    fs::write(&sentinel, b"not visible to CLI").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let fixture = Fixture::new().await;
    let config = configured(&fixture, root.path(), fixture_binary());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    let result = control_request(
        &socket,
        request("probe", &["probe", sentinel.to_str().unwrap(), &port]),
    )
    .await;
    assert_eq!(result["exit_code"], 0, "{result}");
    assert_eq!(result["stdout"], "isolated\n");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    assert!(workspaces(root.path()).is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires bwrap, slirp4netns, network helper and a musl Rust compiler"]
async fn process_results_preserve_partial_failure_files_and_replay_identity() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let config = configured(&fixture, root.path(), fixture_binary());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    let mut args = request("io", &["io"]);
    args["invocation"]["stdin"] = json!("inline");
    args["invocation"]["files"] =
        json!([{"name":"input.bin", "data_base64":STANDARD.encode(b"\0\xffstaged")}]);
    args["invocation"]["outputs"] = json!(["result.bin", "absent.bin"]);
    let result = control_request(&socket, args.clone()).await;
    assert_eq!(result["accepted"], true, "{result}");
    assert_eq!(result["identity"], "bot");
    assert_eq!(result["app_id"], "cli_fixture_app");
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["stdout"], "partial [REDACTED]\n");
    assert!(
        result["stderr"]
            .as_str()
            .unwrap()
            .contains("diagnostic [REDACTED]\n"),
        "{result}"
    );
    assert!(!result.to_string().contains("fixture-tenant-token"));
    assert_eq!(
        result["outputs"],
        json!([{"name":"result.bin", "data_base64":STANDARD.encode(b"\0\xffstagedinline")} ])
    );
    let mut ignored_input = request("ignore", &["ignore"]);
    ignored_input["invocation"]["stdin"] = json!("x".repeat(128 * 1024));
    let result = control_request(&socket, ignored_input).await;
    assert_eq!(result["exit_code"], 2, "{result}");
    assert_eq!(result["stdout"], "invalid arguments\n");
    assert!(
        gateway
            .delivery_facts()
            .unwrap()
            .iter()
            .all(|fact| fact.status == DeliveryStatus::Confirmed)
    );
    gateway.shutdown().await.unwrap();

    fixture.shared.token_failure.store(true, Ordering::SeqCst);
    let mut config = fixture.config(root.path());
    config.cli_enabled = true; // Recovery needs neither an installed CLI nor a fresh token.
    let gateway = Gateway::start(config).await.unwrap();
    let replay = control_request(&socket, args.clone()).await;
    assert_eq!(replay["replayed"], true, "{replay}");
    assert_eq!(replay["outcome"], "previous_process_completed");
    assert!(replay.get("exit_code").is_none());
    for (field, value) in [
        ("argv", json!(["ignore"])),
        ("stdin", json!("changed")),
        ("files", json!([])),
        ("outputs", json!([])),
    ] {
        let mut changed = args.clone();
        changed["invocation"][field] = value;
        assert!(
            control_request(&socket, changed).await["error"]
                .as_str()
                .unwrap()
                .contains("different content")
        );
    }
    assert_eq!(fixture.shared.tokens.load(Ordering::SeqCst), 1);
    assert!(workspaces(root.path()).is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires bwrap, slirp4netns, network helper and a musl Rust compiler"]
async fn output_limits_and_unsafe_exports_never_confirm_or_replay() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let config = configured(&fixture, root.path(), fixture_binary());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    for (id, argv) in [
        ("stdout", vec!["stdout"]),
        ("stderr", vec!["stderr"]),
        ("link", vec!["output", "link"]),
        ("directory", vec!["output", "directory"]),
        ("large", vec!["output", "large"]),
        ("token", vec!["output", "token"]),
    ] {
        let mut args = request(id, &argv);
        args["invocation"]["outputs"] = json!(["result.bin"]);
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            control_request(&socket, args.clone()),
        )
        .await
        .unwrap();
        assert!(result["error"].is_string(), "{id}: {result}");
        assert!(!result.to_string().contains("fixture-tenant-token"));
        assert!(
            control_request(&socket, args).await["error"]
                .as_str()
                .unwrap()
                .contains("unconfirmed")
        );
        assert!(workspaces(root.path()).is_empty());
    }
    assert!(
        gateway
            .delivery_facts()
            .unwrap()
            .iter()
            .all(|fact| fact.status == DeliveryStatus::Unknown)
    );
    gateway.shutdown().await.unwrap();
}

// Observe host PIDs only from the test process, never by mounting /proc in CLI.
fn process_tree(workspace: &Path) -> Vec<u32> {
    let mut pids: Vec<u32> = fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let pid = entry.file_name().to_str()?.parse().ok()?;
            let cmdline = fs::read(entry.path().join("cmdline")).ok()?;
            cmdline
                .split(|b| *b == 0)
                .any(|arg| arg == workspace.as_os_str().as_encoded_bytes())
                .then_some(pid)
        })
        .collect();
    let mut index = 0;
    while index < pids.len() {
        let pid = pids[index];
        let children =
            fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
        for child in children
            .split_whitespace()
            .filter_map(|value| value.parse().ok())
        {
            if !pids.contains(&child) {
                pids.push(child);
            }
        }
        index += 1;
    }
    pids
}

async fn interrupted_process(timeout: bool) {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let config = configured(&fixture, root.path(), fixture_binary());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    let args = request("interrupted", &["wait"]);
    let pending = tokio::spawn({
        let socket = socket.clone();
        let args = args.clone();
        async move { control_request(&socket, args).await }
    });
    poll(|| {
        workspaces(root.path())
            .iter()
            .any(|path| path.join("ready").exists())
    })
    .await;
    let pids = process_tree(&workspaces(root.path())[0]);
    assert!(
        pids.len() >= 3,
        "expected network supervisor, bwrap and CLI: {pids:?}"
    );
    // A concurrent request cannot execute while the first claim is unknown.
    assert!(
        control_request(&socket, args.clone()).await["error"]
            .as_str()
            .unwrap()
            .contains("unconfirmed")
    );
    if timeout {
        let result = tokio::time::timeout(Duration::from_secs(130), pending)
            .await
            .unwrap()
            .unwrap();
        assert!(result["error"].as_str().unwrap().contains("unconfirmed"));
        gateway.shutdown().await.unwrap();
    } else {
        tokio::time::timeout(Duration::from_secs(5), gateway.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert!(pending.await.unwrap()["error"].is_string());
    }
    poll(|| {
        pids.iter()
            .all(|pid| !Path::new(&format!("/proc/{pid}")).exists())
    })
    .await;
    assert!(workspaces(root.path()).is_empty());
    let gateway = Gateway::start(configured(&fixture, root.path(), fixture_binary()))
        .await
        .unwrap();
    assert!(
        control_request(&socket, args).await["error"]
            .as_str()
            .unwrap()
            .contains("unconfirmed")
    );
    assert_eq!(
        gateway.delivery_facts().unwrap()[0].status,
        DeliveryStatus::Unknown
    );
    assert_eq!(fixture.shared.tokens.load(Ordering::SeqCst), 1);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires bwrap, slirp4netns, network helper and a musl Rust compiler"]
async fn shutdown_reaps_the_isolated_process_tree_and_preserves_unknown_claim() {
    interrupted_process(false).await;
}

#[tokio::test]
#[ignore = "takes 120s; requires bwrap, slirp4netns, network helper and a musl Rust compiler"]
async fn timeout_reaps_the_isolated_process_tree_and_preserves_unknown_claim() {
    interrupted_process(true).await;
}

#[tokio::test]
#[ignore = "requires pinned lark-cli 1.0.100 and isolated network helper; no external API calls"]
async fn official_cli_discovery_and_dry_run_support_stdin_and_staged_json() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let config = configured(&fixture, root.path(), official());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    for (id, argv, expected) in [
        ("version", vec!["--version"], "1.0.100"),
        ("help", vec!["api", "--help"], "--data"),
        ("schema", vec!["schema", "im.chats.get"], "chat_id"),
    ] {
        let mut args = request(id, &argv);
        args["invocation"]["stdin"] = json!("unused".repeat(16000));
        let result = control_request(&socket, args).await;
        assert_eq!(result["exit_code"], 0, "{result}");
        assert!(
            result["stdout"].as_str().unwrap().contains(expected),
            "{result}"
        );
    }
    for (id, data) in [("stdin", "-"), ("file", "@body.json")] {
        let mut args = request(
            id,
            &[
                "api",
                "POST",
                "/open-apis/im/v1/messages",
                "--data",
                data,
                "--dry-run",
            ],
        );
        let body = r#"{"receive_id":"ou_fixture","msg_type":"text","content":"{\"text\":\"fixture dry run\"}"}"#;
        args["invocation"]["stdin"] = json!(if data == "-" { body } else { "" });
        args["invocation"]["files"] =
            json!([{"name":"body.json", "data_base64":STANDARD.encode(body)}]);
        args["invocation"]["outputs"] = json!(["body.json"]);
        let result = control_request(&socket, args).await;
        assert_eq!(result["exit_code"], 0, "{result}");
        let output: Value = serde_json::from_str(result["stdout"].as_str().unwrap()).unwrap();
        assert_eq!(output["identity"], "bot", "{output}");
        assert!(output.to_string().contains("fixture dry run"));
        assert_eq!(
            STANDARD
                .decode(result["outputs"][0]["data_base64"].as_str().unwrap())
                .unwrap(),
            body.as_bytes()
        );
    }
    assert!(fixture.shared.sends.lock().unwrap().is_empty());
    assert!(workspaces(root.path()).is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "external API calls with an invalid fixture token; requires pinned CLI and network helper"]
async fn official_cli_media_urls_do_not_reach_host_loopback() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let config = configured(&fixture, root.path(), official());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    for (index, host) in [
        "127.0.0.1",
        "localhost",
        "10.0.2.2",
        "169.254.169.254",
        "[::1]",
    ]
    .iter()
    .enumerate()
    {
        let url = format!("http://{host}:{port}/fixture.png");
        let result = control_request(
            &socket,
            request(
                &format!("media-{index}"),
                &[
                    "im",
                    "+messages-send",
                    "--chat-id",
                    "oc_fixture",
                    "--image",
                    &url,
                ],
            ),
        )
        .await;
        assert_eq!(result["accepted"], true, "{result}");
        assert_eq!(result["exit_code"], 3, "{result}");
        // Pinned CLI turns blocked/failed URL downloads into a text-link send.
        // Its auth failure does not prove URL validation; the independent
        // static probe above tests the namespace boundary without SDK policy.
        let stderr = result["stderr"].as_str().unwrap();
        assert!(stderr.contains("downloading --image:"), "{result}");
        assert!(!stderr.contains("uploading image"), "{result}");
        assert!(stderr.contains("99991668"), "{result}");
        assert!(!result.to_string().contains("fixture-tenant-token"));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    assert!(fixture.shared.uploads.lock().unwrap().is_empty());
    assert!(fixture.shared.sends.lock().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "external read-only TLS probe with an invalid fixture token; requires pinned CLI and network helper"]
async fn official_cli_reaches_feishu_tls_without_host_credentials() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let config = configured(&fixture, root.path(), official());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    let result = control_request(
        &socket,
        request("tls", &["api", "GET", "/open-apis/im/v1/chats"]),
    )
    .await;
    assert_eq!(result["accepted"], true, "{result}");
    assert_ne!(
        result["exit_code"], 0,
        "fixture token must not authenticate: {result}"
    );
    let output = format!("{}{}", result["stdout"], result["stderr"]);
    assert!(!output.contains("unknown authority"), "{result}");
    assert!(
        output.contains("999916") || output.contains("999914"),
        "expected a Feishu auth error, not a network failure: {result}"
    );
    assert!(!output.contains("fixture-tenant-token"));
    gateway.shutdown().await.unwrap();
}
