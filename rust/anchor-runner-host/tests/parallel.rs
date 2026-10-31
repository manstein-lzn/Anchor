//! Tests the real host boundary, not just parallel fake execution ports.
#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;

use anchor_kernel::{FileStore, Resource, Store};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn graph(left: &str, right: &str) -> Value {
    json!({"objective":"parallel files","entry":"seed","ops":{
        "seed":{"run":"sh -c 'printf seed > seed.txt'","writes":["seed.txt"]},
        "fork":{"fanout":{"join":"join"}}, "join":{"join":{}},
        "left":{"run":left,"writes":["result.txt"]},"right":{"run":right,"writes":["result.txt"]},
        "combine":{"run":"sh -c 'cat /in/left/result.txt /in/right/result.txt > combined.txt; cat /in/join/join.json > join-copy.json'","writes":["combined.txt","join-copy.json"]}},
        "nodes":[{"id":"seed","op":"seed"},{"id":"fork","op":"fork"},{"id":"left","op":"left"},{"id":"right","op":"right"},{"id":"join","op":"join"},{"id":"combine","op":"combine"}],
        "edges":[{"from":"seed","to":"fork"},{"from":"fork","to":"left"},{"from":"fork","to":"right"},{"from":"left","to":"join"},{"from":"right","to":"join"},{"from":"join","to":"combine"}]})
}
fn spawn(root: &Path, graph: &Value) -> Child {
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
        .env("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh,cat,date,sleep,false")
        .env_remove("ANCHOR_MODEL_API_KEY")
        .env_remove("ANCHOR_MODEL_URL")
        .env_remove("ANCHOR_MODEL_NAME")
        .env_remove("ANCHOR_RUST_MCP_SERVERS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let bytes = serde_json::to_vec(&json!({"op":"start_bundle","version":1,"request_id":"parallel","run_id":"parallel","input":{}})).unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    stdin.write_all(&bytes).unwrap();
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
    serde_json::from_slice(&output.stdout[4..]).unwrap()
}
fn resource(root: &Path) -> Option<Resource> {
    FileStore::new(root.join("state/kernel-runs"))
        .ok()?
        .load(&anchor_runtime::graph::validator::run_resource_id(
            "parallel",
        ))
        .ok()
        .flatten()
}
fn record(root: &Path) -> Value {
    resource(root).expect("durable Kernel Run").state
}

fn file(root: &Path, saved: &Value, node: &str, path: &str) -> Vec<u8> {
    let commit = saved["results"][node][0]["commit"]["id"].as_str().unwrap();
    fixture::artifact_file(&root.join("state/artifacts").join(commit), path)
}
// Keep a generous overlap window: Bubblewrap startup and fsync scheduling can
// be delayed under the full workspace test load. Kernel concurrency itself is
// asserted directly by the provider-free peak-active-calls test.
const BRANCH: &str = "sh -c 'date +%s%N > start.txt; sleep 2; test ! -e /in/right/result.txt; test ! -e /in/left/result.txt; cat /in/seed/seed.txt > result.txt; if printf bad > /in/seed/seed.txt; then exit 4; fi; date +%s%N > end.txt'";

#[test]
fn paired_parallel_nodes_overlap_and_join_delivers_exact_branch_files() {
    let root = tempfile::tempdir().unwrap();
    let mut graph = graph(BRANCH, BRANCH);
    for node in ["left", "right"] {
        graph["ops"][node]["writes"] = json!(["result.txt", "start.txt", "end.txt"]);
    }
    let response = finish(spawn(root.path(), &graph));
    assert_eq!(response["projection"]["outcome"], "done", "{response}");
    let saved = record(root.path());
    let timestamp = |node, name| {
        String::from_utf8(file(root.path(), &saved, node, name))
            .unwrap()
            .trim()
            .parse::<u128>()
            .unwrap()
    };
    assert!(timestamp("left", "start.txt") < timestamp("right", "end.txt"));
    assert!(timestamp("right", "start.txt") < timestamp("left", "end.txt"));
    assert_eq!(
        file(root.path(), &saved, "combine", "combined.txt"),
        b"seedseed"
    );
    let manifest: Value =
        serde_json::from_slice(&file(root.path(), &saved, "combine", "join-copy.json")).unwrap();
    assert_eq!(manifest["branches"].as_array().unwrap().len(), 2);
    for branch in manifest["branches"].as_array().unwrap() {
        for node in branch["nodes"].as_array().unwrap() {
            assert_eq!(
                node["commit"],
                saved["results"][node["node"].as_str().unwrap()][0]["commit"]
            );
        }
    }
    assert_eq!(
        finish(spawn(root.path(), &graph))["projection"]["outcome"],
        "done"
    );
    // Re-entering a completed Run is a read: the stored record must stay
    // byte-identical, including its write stamp.
    assert_eq!(saved, record(root.path()));
}

#[test]
fn failed_branch_never_produces_join_or_downstream_files() {
    let root = tempfile::tempdir().unwrap();
    let graph = graph(BRANCH, "false");
    let response = finish(spawn(root.path(), &graph));
    assert_eq!(response["projection"]["outcome"], "failed", "{response}");
    let saved = record(root.path());
    assert!(saved["results"].get("join").is_none());
    assert!(saved["results"].get("combine").is_none());
    assert_eq!(
        finish(spawn(root.path(), &graph))["projection"]["outcome"],
        "failed"
    );
}

#[test]
fn killing_a_wave_preserves_completed_branch_and_fences_unknown_branch() {
    let root = tempfile::tempdir().unwrap();
    let graph = graph(
        "sh -c 'cat /in/seed/seed.txt > result.txt'",
        "sh -c 'printf once > started.txt; sleep 20; cat /in/seed/seed.txt > result.txt'",
    );
    let mut child = spawn(root.path(), &graph);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut before = None;
    while Instant::now() < deadline {
        if let Some(resource) = resource(root.path()) {
            let saved = resource.state;
            if saved["results"].get("left").is_some()
                && fs::read_dir(root.path().join("state/facts"))
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.path().extension().is_some_and(|e| e == "started"))
                    .count()
                    == 3
            {
                before = Some(saved);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let before = before.expect("one branch must settle while the other was started");
    let response = finish(spawn(root.path(), &graph));
    assert_eq!(response["projection"]["phase"], "waiting", "{response}");
    assert_eq!(response["projection"]["waiting_for"], "human", "{response}");
    assert!(
        response["projection"]["reason"]
            .as_str()
            .unwrap()
            .contains("durably started"),
        "{response}"
    );
    let after = record(root.path());
    assert_eq!(after["results"]["left"], before["results"]["left"]);
    assert!(after["results"].get("join").is_none());
    assert_eq!(
        fs::read_dir(root.path().join("state/facts"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "started"))
            .count(),
        3
    );
}
