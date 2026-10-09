//! Real subprocess + Bubblewrap acceptance, with no model/provider dependency.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn graph(commands: &[(&str, &str)]) -> Value {
    let mut ops = serde_json::Map::new();
    for (name, command) in commands {
        ops.insert((*name).into(), json!({"run":command}));
    }
    json!({"objective":"native file pipeline","entry":commands[0].0,"ops":ops,
        "nodes":commands.iter().map(|(name,_)|json!({"id":name,"op":name})).collect::<Vec<_>>(),
        "edges":commands.windows(2).map(|pair|json!({"from":pair[0].0,"to":pair[1].0})).collect::<Vec<_>>()})
}
fn spawn(root: &Path, graph: &Value, run: &str) -> Child {
    let bundle = root.join("bundle");
    fs::create_dir_all(&bundle).unwrap();
    fs::write(bundle.join("graph.json"), graph.to_string()).unwrap();
    fs::write(
        bundle.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_anchor-runner-host"))
        .env("ANCHOR_RUNNER_STATE_ROOT", root.join("state"))
        .env("ANCHOR_RUNNER_WORKSPACE_ROOT", root.join("work"))
        .env("ANCHOR_RUNNER_BUNDLE_ROOT", bundle)
        .env("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh,cat,true,false,sleep")
        .env_remove("ANCHOR_MODEL_API_KEY")
        .env_remove("ANCHOR_MODEL_URL")
        .env_remove("ANCHOR_MODEL_NAME")
        .env_remove("ANCHOR_RUST_MCP_SERVERS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = serde_json::to_vec(
        &json!({"op":"start_bundle","version":1,"request_id":"test","run_id":run,"input":{}}),
    )
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&(request.len() as u32).to_be_bytes())
        .unwrap();
    stdin.write_all(&request).unwrap();
    drop(stdin);
    child
}
fn finish(child: Child) -> Value {
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() >= 4, "missing host response");
    let len = u32::from_be_bytes(output.stdout[..4].try_into().unwrap()) as usize;
    serde_json::from_slice(&output.stdout[4..4 + len]).unwrap()
}
fn record(root: &Path, run: &str) -> Value {
    serde_json::from_slice(&fs::read(root.join("state/runs").join(format!("{run}.json"))).unwrap())
        .unwrap()
}
fn workspace(root: &Path, key: &Value) -> std::path::PathBuf {
    let identity = format!(
        "{}:{}:{}:{}",
        key["run_id"].as_str().unwrap(),
        key["graph_digest"].as_str().unwrap(),
        key["node_id"].as_str().unwrap(),
        key["invocation"].as_u64().unwrap()
    );
    root.join("work")
        .join(key["run_id"].as_str().unwrap())
        .join(format!("{:x}", Sha256::digest(identity.as_bytes())))
}
fn file(root: &Path, record: &Value, node: &str, name: &str) -> Vec<u8> {
    let id = record["results"][node][0]["commit"]["id"].as_str().unwrap();
    fs::read(
        root.join("state/artifacts")
            .join(id)
            .join("files")
            .join(name),
    )
    .unwrap()
}

#[test]
fn serial_bundle_transfers_exact_readonly_snapshots_and_reloads_without_reexecution() {
    let root = tempfile::tempdir().unwrap();
    let graph = graph(&[
        ("producer", "sh -c 'printf native-evidence > source.txt'"),
        (
            "transform",
            "sh -c 'if printf corrupt > /in/producer/source.txt; then exit 4; fi; cat /in/producer/source.txt > report.txt'",
        ),
        ("verify", "sh -c 'cat /in/transform/report.txt > final.txt'"),
    ]);
    let response = finish(spawn(root.path(), &graph, "serial"));
    assert_eq!(response["status"], "completed", "{response}");
    let saved = record(root.path(), "serial");
    assert_eq!(saved["results"].as_object().unwrap().len(), 3);
    assert_eq!(
        file(root.path(), &saved, "verify", "final.txt"),
        b"native-evidence"
    );
    assert_eq!(
        file(root.path(), &saved, "producer", "source.txt"),
        b"native-evidence"
    );
    let producer_work = workspace(root.path(), &saved["results"]["producer"][0]["key"]);
    let verify_work = workspace(root.path(), &saved["results"]["verify"][0]["key"]);
    assert_ne!(producer_work, verify_work);
    assert!(!verify_work.join("source.txt").exists());
    fs::write(producer_work.join("source.txt"), "changed after commit").unwrap();
    let response = finish(spawn(root.path(), &graph, "serial"));
    assert_eq!(response["status"], "completed", "{response}");
    // Re-entering a completed Run is a read: the stored record must stay
    // byte-identical, including its write stamp.
    assert_eq!(saved, record(root.path(), "serial"));
    assert_eq!(
        file(root.path(), &saved, "producer", "source.txt"),
        b"native-evidence"
    );
}

#[test]
fn known_failure_stops_the_pipeline_and_does_not_create_downstream_workspace() {
    let root = tempfile::tempdir().unwrap();
    let graph = graph(&[("fail", "false"), ("never", "true")]);
    let response = finish(spawn(root.path(), &graph, "failure"));
    assert_eq!(response["status"], "failed", "{response}");
    let saved = record(root.path(), "failure");
    assert!(saved["results"].as_object().unwrap().is_empty());
    assert!(saved["invocations"].get("never").is_none());
    assert_eq!(
        finish(spawn(root.path(), &graph, "failure"))["status"],
        "failed"
    );
}

#[test]
fn killed_host_keeps_unknown_node_effects_and_never_replays_them() {
    let root = tempfile::tempdir().unwrap();
    let graph = graph(&[
        ("slow", "sh -c 'printf once >> attempts.txt; sleep 20'"),
        ("never", "true"),
    ]);
    let mut child = spawn(root.path(), &graph, "interrupted");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut attempts = None;
    while Instant::now() < deadline {
        let path = root.path().join("state/runs/interrupted.json");
        if let Ok(bytes) = fs::read(path)
            && let Ok(saved) = serde_json::from_slice::<Value>(&bytes)
            && saved["cursor"]["key"].is_object()
        {
            let candidate = workspace(root.path(), &saved["cursor"]["key"]).join("attempts.txt");
            if fs::read(&candidate).is_ok_and(|content| content == b"once") {
                attempts = Some(candidate);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let attempts = attempts.expect("command must have run before killing host");
    let response = finish(spawn(root.path(), &graph, "interrupted"));
    assert_eq!(response["status"], "failed", "{response}");
    assert!(response["error"].as_str().unwrap().contains("uncertain"));
    assert_eq!(fs::read(attempts).unwrap(), b"once");
    assert!(
        record(root.path(), "interrupted")["invocations"]
            .get("never")
            .is_none()
    );
}
