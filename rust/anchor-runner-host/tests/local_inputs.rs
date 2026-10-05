//! Production framing and Bubblewrap coverage for operator-owned local inputs.
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn bundle(path: &Path, definition: &Value) {
    fs::create_dir_all(path).unwrap();
    fs::write(path.join("graph.json"), definition.to_string()).unwrap();
    fs::write(
        path.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
}

fn grants(root: &Path, graph: &str, definition: &Value) {
    let directory = root.join("operator-workspaces").join(graph);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("local-inputs.json"), definition.to_string()).unwrap();
}

fn invoke(root: &Path, run: &str, local: bool, graph: Option<&str>) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anchor-runner-host"));
    command
        .env("ANCHOR_RUNNER_STATE_ROOT", root.join("state"))
        .env("ANCHOR_RUNNER_WORKSPACE_ROOT", root.join("work"))
        .env("ANCHOR_RUNNER_BUNDLE_ROOT", root.join("bundle"))
        .env("ANCHOR_RUNNER_CATALOG_ROOT", root.join("catalog"))
        .env("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh")
        .env_remove("ANCHOR_RUNNER_LOCAL_INPUTS_ROOT")
        .env_remove("ANCHOR_RUNNER_GRAPH_NAME")
        .env_remove("ANCHOR_RUNNER_LIBRARY_ROOT")
        .env_remove("ANCHOR_MODEL_API_KEY")
        .env_remove("ANCHOR_MODEL_URL")
        .env_remove("ANCHOR_MODEL_NAME")
        .env_remove("ANCHOR_RUST_MCP_SERVERS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if local {
        command.env(
            "ANCHOR_RUNNER_LOCAL_INPUTS_ROOT",
            root.join("operator-workspaces"),
        );
    }
    if let Some(graph) = graph {
        command.env("ANCHOR_RUNNER_GRAPH_NAME", graph);
    }
    let mut child = command.spawn().unwrap();
    let data = serde_json::to_vec(
        &json!({"op":"start_bundle","version":1,"request_id":run,"run_id":run,"input":{}}),
    )
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&(data.len() as u32).to_be_bytes()).unwrap();
    stdin.write_all(&data).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout[4..]).unwrap()
}

fn record(root: &Path, run: &str) -> Value {
    serde_json::from_slice(&fs::read(root.join(format!("state/runs/{run}.json"))).unwrap()).unwrap()
}

fn collect_graph() -> Value {
    json!({
        "entry":"collect", "ops":{
            "collect":{"run":"set -eu; cat /local-inputs/history/source.txt > evidence.txt; ! touch /local-inputs/history/forbidden; printf granted"},
            "other":{"run":"test ! -e /local-inputs/history/source.txt && cat /in/collect/evidence.txt"}
        },
        "nodes":[{"id":"collect","op":"collect"},{"id":"other","op":"other"}],
        "edges":[{"from":"collect","to":"other"}]
    })
}

#[test]
fn operator_inputs_are_readonly_visible_only_to_the_granted_op_and_frozen_on_resume() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("private-history");
    let other = root.path().join("other-history");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&other).unwrap();
    fs::write(source.join("source.txt"), "approved-local-evidence").unwrap();
    fs::write(other.join("source.txt"), "unapproved-new-evidence").unwrap();
    bundle(&root.path().join("bundle"), &collect_graph());
    grants(
        root.path(),
        "weekly",
        &json!({"collect":{"history":source}}),
    );
    let completed = invoke(root.path(), "local-granted", true, Some("weekly"));
    assert_eq!(completed["status"], "completed", "{completed}");
    assert!(!source.join("forbidden").exists());
    let saved = record(root.path(), "local-granted");
    assert_eq!(
        saved["results"]["other"][0]["completion"]["submission"],
        "approved-local-evidence"
    );
    assert_eq!(
        invoke(root.path(), "local-granted", true, Some("weekly"))["status"],
        "completed"
    );

    grants(root.path(), "weekly", &json!({"collect":{"history":other}}));
    let changed = invoke(root.path(), "local-granted", true, Some("weekly"));
    assert_eq!(changed["kind"], "rejected", "{changed}");
    assert!(
        changed["reason"].as_str().unwrap().contains("grants"),
        "{changed}"
    );
    assert_eq!(
        record(root.path(), "local-granted")["results"],
        saved["results"]
    );
    assert_eq!(
        invoke(root.path(), "local-granted", false, Some("weekly"))["kind"],
        "rejected"
    );
}

#[test]
fn unknown_nodes_and_missing_standalone_identity_are_rejected_before_command_execution() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("history");
    fs::create_dir(&source).unwrap();
    bundle(&root.path().join("bundle"), &collect_graph());
    grants(
        root.path(),
        "weekly",
        &json!({"unknown":{"history":source}}),
    );
    let unknown = invoke(root.path(), "unknown-node", true, Some("weekly"));
    assert_eq!(unknown["kind"], "rejected", "{unknown}");
    assert!(unknown["reason"].as_str().unwrap().contains("unknown node"));
    assert!(!root.path().join("state/runs/unknown-node.json").exists());
    let nameless = invoke(root.path(), "nameless", true, None);
    assert_eq!(nameless["kind"], "rejected", "{nameless}");
    assert!(
        nameless["reason"]
            .as_str()
            .unwrap()
            .contains("ANCHOR_RUNNER_GRAPH_NAME")
    );
}

#[test]
fn absent_operator_authority_is_frozen_and_cannot_be_added_to_an_existing_run() {
    let root = tempfile::tempdir().unwrap();
    bundle(
        &root.path().join("bundle"),
        &json!({
            "entry":"collect", "ops":{"check":{"run":"test ! -e /local-inputs/history/source.txt"}},
            "nodes":[{"id":"collect","op":"check"}], "edges":[]
        }),
    );
    let source = root.path().join("history");
    fs::create_dir(&source).unwrap();
    grants(
        root.path(),
        "weekly",
        &json!({"collect":{"history":source}}),
    );
    let original = invoke(root.path(), "empty-grants", false, None);
    assert_eq!(original["status"], "completed", "{original}");
    let added = invoke(root.path(), "empty-grants", true, Some("weekly"));
    assert_eq!(added["kind"], "rejected", "{added}");
    assert!(added["reason"].as_str().unwrap().contains("grants"));
}

#[test]
fn wait_child_uses_its_own_grants_and_never_inherits_parent_authority() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("history");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("source.txt"), "child-only-evidence").unwrap();
    bundle(
        &root.path().join("bundle"),
        &json!({
            "entry":"collect", "ops":{"invoke":{"call":{"graph":"child","mode":"wait","input":{}}}},
            "nodes":[{"id":"collect","op":"invoke"}], "edges":[]
        }),
    );
    let child = root.path().join("catalog/child");
    bundle(
        &child,
        &json!({
            "entry":"collect", "ops":{"check":{"run":"test ! -e /local-inputs/history/source.txt"}},
            "nodes":[{"id":"collect","op":"check"}], "edges":[]
        }),
    );
    grants(
        root.path(),
        "parent",
        &json!({"collect":{"history":source}}),
    );
    grants(root.path(), "child", &json!({}));
    let response = invoke(root.path(), "child-no-inheritance", true, Some("parent"));
    assert_eq!(response["status"], "completed", "{response}");

    bundle(
        &child,
        &json!({
            "entry":"collect", "ops":{"check":{"run":"cat /local-inputs/history/source.txt"}},
            "nodes":[{"id":"collect","op":"check"}], "edges":[]
        }),
    );
    let denied = invoke(root.path(), "child-without-grant", true, Some("parent"));
    assert_eq!(denied["status"], "failed", "{denied}");
    grants(root.path(), "child", &json!({"collect":{"history":source}}));
    let allowed = invoke(root.path(), "child-with-grant", true, Some("parent"));
    assert_eq!(allowed["status"], "completed", "{allowed}");
    let parent = record(root.path(), "child-with-grant");
    let child_id = parent["graph_calls"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()["child_run_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        record(root.path(), child_id)["results"]["collect"][0]["completion"]["submission"],
        "child-only-evidence"
    );
}
