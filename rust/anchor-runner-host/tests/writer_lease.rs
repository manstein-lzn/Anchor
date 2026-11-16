//! A deployment has one writing host; status remains a read-only entry point.
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn host(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anchor-runner-host"));
    command
        .env("ANCHOR_RUNNER_STATE_ROOT", root.join("state"))
        .env("ANCHOR_RUNNER_WORKSPACE_ROOT", root.join("work"))
        .env("ANCHOR_RUNNER_BUNDLE_ROOT", root.join("bundle"))
        .env("ANCHOR_RUNNER_CATALOG_ROOT", root.join("catalog"))
        .env("ANCHOR_RUNNER_LIBRARY_ROOT", root.join("catalog"))
        .env("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh,true")
        .env_remove("ANCHOR_MODEL_API_KEY")
        .env_remove("ANCHOR_MODEL_URL")
        .env_remove("ANCHOR_MODEL_NAME")
        .env_remove("ANCHOR_RUST_MCP_SERVERS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn prepare(root: &Path, command: &str) {
    fs::create_dir_all(root.join("state")).unwrap();
    fs::create_dir_all(root.join("work")).unwrap();
    fs::create_dir_all(root.join("catalog")).unwrap();
    fs::create_dir_all(root.join("bundle")).unwrap();
    fs::write(
        root.join("bundle/graph.json"),
        json!({"objective":"lease","entry":"work",
        "ops":{"work":{"run":command}},"nodes":[{"id":"work","op":"work"}],"edges":[]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        root.join("bundle/manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
}

fn framed(root: &Path, operation: &str, run: &str) -> Child {
    let mut child = host(root).spawn().unwrap();
    let mut request = json!({"op":operation,"version":1,"request_id":"lease-test","run_id":run});
    if operation != "status" {
        request["input"] = json!({});
    }
    let request = serde_json::to_vec(&request).unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&(request.len() as u32).to_be_bytes())
        .unwrap();
    stdin.write_all(&request).unwrap();
    child
}

fn response(child: Child) -> Value {
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let length = u32::from_be_bytes(output.stdout[..4].try_into().unwrap()) as usize;
    serde_json::from_slice(&output.stdout[4..4 + length]).unwrap()
}

fn address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn http_host(root: &Path, address: SocketAddr) -> Child {
    host(root)
        .arg("serve")
        .env("ANCHOR_RUNNER_LISTEN", address.to_string())
        .env("ANCHOR_RETENTION_DAYS", "30")
        .env("ANCHOR_RETENTION_MAX_GB", "20")
        .spawn()
        .unwrap()
}

fn wait_http(child: &mut Child, address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        assert!(
            child.try_wait().unwrap().is_none(),
            "HTTP host exited before listening"
        );
        if let Ok(mut socket) = TcpStream::connect(address) {
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            socket
                .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut body = String::new();
            socket.read_to_string(&mut body).unwrap();
            if body.contains("200 OK") {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("HTTP host did not listen");
}

#[test]
fn stdio_writer_removes_stale_durable_writes_before_execution() {
    let root = tempfile::tempdir().unwrap();
    prepare(root.path(), "true");
    fs::create_dir(root.path().join("state/facts")).unwrap();
    let stale = root.path().join("state/facts/.anchor-write-Ab123x");
    fs::write(&stale, b"partial").unwrap();
    assert_eq!(
        response(framed(root.path(), "start_bundle", "cleanup"))["projection"]["outcome"],
        "done"
    );
    assert!(!stale.exists());
}

#[test]
fn stdio_status_does_not_start_cleanup_or_create_a_writer_lease() {
    let root = tempfile::tempdir().unwrap();
    prepare(root.path(), "true");
    fs::create_dir(root.path().join("state/facts")).unwrap();
    let stale = root.path().join("state/facts/.anchor-write-Ab123x");
    fs::write(&stale, b"partial").unwrap();
    assert_eq!(
        response(framed(root.path(), "status", "missing"))["kind"],
        "missing"
    );
    assert!(stale.exists());
    assert!(!root.path().join("state/deployment-locks").exists());
}

#[test]
fn http_startup_removes_stale_durable_writes_before_serving() {
    let root = tempfile::tempdir().unwrap();
    prepare(root.path(), "true");
    fs::create_dir(root.path().join("state/facts")).unwrap();
    let stale = root.path().join("state/facts/.anchor-write-Ab123x");
    fs::write(&stale, b"partial").unwrap();
    let listen = address();
    let mut child = http_host(root.path(), listen);
    wait_http(&mut child, listen);
    let retained = stale.exists();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!retained);
}

#[test]
fn http_writer_blocks_other_writers_but_exit_releases_lease() {
    let root = tempfile::tempdir().unwrap();
    prepare(root.path(), "true");
    let first_address = address();
    let mut http = http_host(root.path(), first_address);
    wait_http(&mut http, first_address);
    let reader = anchor_kernel::HistoryReader::open(root.path().join("state/kernel-runs")).unwrap();
    assert!(reader.load("validator/effect").unwrap().is_some());
    let rejected = response(framed(root.path(), "start_bundle", "blocked"));
    assert_eq!(rejected["kind"], "rejected");
    assert!(
        rejected["reason"]
            .as_str()
            .unwrap()
            .contains("writing host")
    );
    assert_eq!(
        response(framed(root.path(), "status", "blocked"))["kind"],
        "missing"
    );
    let second = http_host(root.path(), address())
        .wait_with_output()
        .unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("writing host"));
    http.kill().unwrap();
    http.wait().unwrap();
    let released = response(framed(root.path(), "start_bundle", "released"));
    assert_eq!(released["projection"]["outcome"], "done", "{released}");
}

#[test]
fn stdio_writer_blocks_http_until_process_exit() {
    let root = tempfile::tempdir().unwrap();
    prepare(root.path(), "sh -c 'sleep 20'");
    let mut stdio = framed(root.path(), "start_bundle", "holder");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root
        .path()
        .join("state/kernel-runs/run%2Fholder/resource.json")
        .exists()
    {
        assert!(Instant::now() < deadline, "stdio holder did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let rejected = http_host(root.path(), address())
        .wait_with_output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("writing host"));
    stdio.kill().unwrap();
    stdio.wait().unwrap();
    let listen = address();
    let mut http = http_host(root.path(), listen);
    wait_http(&mut http, listen);
    http.kill().unwrap();
    http.wait().unwrap();
}
