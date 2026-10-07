mod support;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};
use support::Fixture;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

const BINARY: &str = env!("CARGO_BIN_EXE_anchor-wecom-tools");

fn temporary() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

struct StdioClient {
    process: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl StdioClient {
    fn start(executable: &Path, cwd: &Path, fixture: &Fixture, credentials: bool) -> Self {
        let mut command = Command::new(executable);
        command
            .current_dir(cwd)
            .env_clear()
            .env("WECOM_API_BASE_URL", &fixture.url)
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("WECOM_BOT_SECRET", "unapproved-bot-secret")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if credentials {
            command
                .env("WECOM_CORP_ID", "fixture-corp")
                .env("WECOM_AGENT_ID", "42")
                .env("WECOM_SECRET", "fixture-secret");
        }
        let mut process = command.spawn().unwrap();
        let input = process.stdin.take().unwrap();
        let output = BufReader::new(process.stdout.take().unwrap());
        Self {
            process,
            input,
            output,
        }
    }

    async fn write(&mut self, request: Value) {
        self.input
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        self.input.flush().await.unwrap();
    }

    async fn request(&mut self, request: Value) -> Value {
        let id = request["id"].clone();
        self.write(request).await;
        let mut line = String::new();
        let count = tokio::time::timeout(Duration::from_secs(10), self.output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(count > 0, "MCP stdout closed");
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], id);
        response
    }

    async fn initialize(&mut self) {
        let response = self.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"loopback-fixture","version":"1.0"}
        }})).await;
        assert_eq!(response["result"]["serverInfo"]["name"], "wecom");
        assert_eq!(response["result"]["serverInfo"]["version"], "1.0.0");
        assert!(response["result"]["capabilities"]["tools"].is_object());
        self.write(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
    }

    async fn finish(mut self) {
        drop(self.input);
        let status = tokio::time::timeout(Duration::from_secs(10), self.process.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
        let mut remaining = String::new();
        self.output.read_to_string(&mut remaining).await.unwrap();
        assert!(remaining.is_empty(), "unexpected stdout: {remaining}");
        let mut stderr = String::new();
        self.process
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .await
            .unwrap();
        assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    }
}

#[tokio::test]
async fn native_stdio_initialization_list_all_calls_and_unknown_tool() {
    let fixture = Fixture::start().await;
    let temp = temporary();
    let mut client = StdioClient::start(Path::new(BINARY), temp.path(), &fixture, true);
    client.initialize().await;
    let listed = client
        .request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .await;
    assert_eq!(
        listed["result"]["tools"],
        serde_json::to_value(anchor_wecom_tools::tools()).unwrap()
    );
    for (id, name, arguments) in [
        (
            3,
            "wecom_send_text",
            json!({"content":"原生文本","touser":"user"}),
        ),
        (
            4,
            "wecom_send_markdown",
            json!({"content":"# Markdown","toparty":"1"}),
        ),
        (5, "wecom_get_user", json!({"userid":"user"})),
    ] {
        let response = client.request(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await;
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(response["result"]["content"][0]["type"], "text");
        let result: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(result["errcode"], 0);
    }
    let unknown = client.request(json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"unknown-private-name","arguments":{}}})).await;
    assert_eq!(unknown["error"]["code"], -32602);
    assert_eq!(unknown["error"]["message"], "unknown tool");
    let invalid = client.request(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"wecom_send_text","arguments":{"content":"ok"}}})).await;
    assert_eq!(invalid["result"]["isError"], true);
    client.finish().await;
    assert_eq!(fixture.state.token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.sends.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.state.users.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dotenv_and_channel_credentials_are_not_loaded_by_stdio() {
    let fixture = Fixture::start().await;
    let temp = temporary();
    fs::write(
        temp.path().join(".env"),
        "WECOM_CORP_ID=dotenv-corp\nWECOM_AGENT_ID=42\nWECOM_SECRET=dotenv-secret\n",
    )
    .unwrap();
    let mut client = StdioClient::start(Path::new(BINARY), temp.path(), &fixture, false);
    client.initialize().await;
    let response = client.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"wecom_get_user","arguments":{"userid":"user"}}})).await;
    assert_eq!(response["result"]["isError"], true);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("WECOM_CORP_ID is not configured")
    );
    client.finish().await;
    assert_eq!(fixture.state.token_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_endpoint_environment_fails_without_stdout_or_private_values() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    for value in [
        OsString::from("http://user:private-secret@127.0.0.1/?access_token=private-token"),
        OsString::from_vec(vec![b'h', b't', b't', b'p', 0xff]),
    ] {
        let output = Command::new(BINARY)
            .env_clear()
            .env("WECOM_API_BASE_URL", value)
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "anchor-wecom-tools: WECOM_API_BASE_URL is invalid\n"
        );
    }
}

async fn package(destination: &Path) -> std::process::Output {
    Command::new(BINARY)
        .env_clear()
        .env("WECOM_CORP_ID", "never-embed-corp-8b2d")
        .env("WECOM_AGENT_ID", "1234567")
        .env("WECOM_SECRET", "never-embed-secret-2f55")
        .arg("package-plugin")
        .arg(destination)
        .output()
        .await
        .unwrap()
}

fn files(root: &Path, directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        assert!(!entry.file_type().unwrap().is_symlink());
        if entry.file_type().unwrap().is_dir() {
            files(root, &entry.path(), found);
        } else {
            found.push(entry.path().strip_prefix(root).unwrap().to_owned());
        }
    }
}

#[tokio::test]
async fn independent_native_package_preserves_metadata_contains_no_credentials_and_runs() {
    let fixture = Fixture::start().await;
    let temp = temporary();
    let destination = temp.path().join("native");
    let output = package(&destination).await;
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let mut found = Vec::new();
    files(&destination, &destination, &mut found);
    found.sort();
    assert_eq!(
        found,
        vec![
            PathBuf::from("bin/anchor-wecom-tools"),
            PathBuf::from("plugin.json"),
            PathBuf::from("skills/wecom/SKILL.md")
        ]
    );
    let manifest: Value =
        serde_json::from_slice(&fs::read(destination.join("plugin.json")).unwrap()).unwrap();
    let mut expected: Value =
        serde_json::from_str(include_str!("../../../plugins/wecom/plugin.json")).unwrap();
    expected["mcpServers"]["wecom"]["command"] = "bin/anchor-wecom-tools".into();
    expected["mcpServers"]["wecom"]["args"] = json!([]);
    expected["mcpServers"]["wecom"]["cwd"] = ".".into();
    assert_eq!(manifest, expected);
    assert_eq!(
        fs::read_to_string(destination.join("skills/wecom/SKILL.md")).unwrap(),
        include_str!("../../../plugins/wecom/skills/wecom/SKILL.md")
    );
    let executable = destination.join("bin/anchor-wecom-tools");
    assert_eq!(
        Sha256::digest(fs::read(&executable).unwrap()),
        Sha256::digest(fs::read(BINARY).unwrap())
    );
    assert_ne!(
        fs::metadata(&executable).unwrap().permissions().mode() & 0o111,
        0
    );
    for path in found {
        let bytes = fs::read(destination.join(path)).unwrap();
        for secret in [
            b"never-embed-corp-8b2d".as_slice(),
            b"never-embed-secret-2f55".as_slice(),
        ] {
            assert!(!bytes.windows(secret.len()).any(|window| window == secret));
        }
    }
    let mut client = StdioClient::start(&executable, &destination, &fixture, true);
    client.initialize().await;
    let response = client.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"wecom_get_user","arguments":{"userid":"user"}}})).await;
    assert_eq!(response["result"]["isError"], false);
    client.finish().await;
}

#[tokio::test]
async fn packaging_refuses_existing_outputs_symlinks_missing_parents_and_keeps_them_intact() {
    let temp = temporary();
    let directory = temp.path().join("directory");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("sentinel"), "keep").unwrap();
    let file = temp.path().join("file");
    fs::write(&file, "keep-file").unwrap();
    let link = temp.path().join("link");
    symlink(&directory, &link).unwrap();
    let dangling = temp.path().join("dangling");
    symlink(temp.path().join("missing"), &dangling).unwrap();
    for destination in [
        &directory,
        &file,
        &link,
        &dangling,
        &link.join("native"),
        &temp.path().join("missing/native"),
    ] {
        let output = package(destination).await;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!directory.join("native").exists());
    }
    assert_eq!(
        fs::read_to_string(directory.join("sentinel")).unwrap(),
        "keep"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "keep-file");
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 4);
}

#[tokio::test]
async fn concurrent_package_publication_has_exactly_one_winner_and_no_staging_debris() {
    let temp = temporary();
    let destination = temp.path().join("native");
    let (first, second) = tokio::join!(package(&destination), package(&destination));
    assert_ne!(first.status.success(), second.status.success());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    assert_eq!(
        Sha256::digest(fs::read(destination.join("bin/anchor-wecom-tools")).unwrap()),
        Sha256::digest(fs::read(BINARY).unwrap())
    );
}
