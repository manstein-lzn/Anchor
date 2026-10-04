//! End-to-end tests for deterministic Op routing through the production Host.
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Child, Command, Stdio},
};

fn graph(gate_command: &str) -> Value {
    json!({
        "objective":"deterministic Op routing",
        "entry":"gate",
        "ops":{
            "gate":{"run":gate_command},
            "branch":{"run":"true"}
        },
        "nodes":[
            {"id":"gate","op":"gate"},
            {"id":"good","op":"branch"},
            {"id":"bad","op":"branch"}
        ],
        "edges":[
            {"from":"gate","to":"good"},
            {"from":"gate","to":"bad"}
        ]
    })
}

fn spawn(root: &Path, run_id: &str, graph: &Value) -> Child {
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
        .env("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh,true")
        .env_remove("ANCHOR_MODEL_API_KEY")
        .env_remove("ANCHOR_MODEL_URL")
        .env_remove("ANCHOR_MODEL_NAME")
        .env_remove("ANCHOR_RUST_MCP_SERVERS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let bytes = serde_json::to_vec(&json!({
        "op":"start_bundle",
        "version":1,
        "request_id":run_id,
        "run_id":run_id,
        "input":{}
    }))
    .unwrap();
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

fn record(root: &Path, run_id: &str) -> Value {
    serde_json::from_slice(&fs::read(root.join(format!("state/runs/{run_id}.json"))).unwrap())
        .unwrap()
}

#[test]
fn successful_op_marker_routes_one_branch_and_invalid_or_failed_choices_stop() {
    let root = tempfile::tempdir().unwrap();
    let scenarios = [
        (
            "routed",
            r#"sh -c 'test "$ANCHOR_ROUTES" = "good,bad" && printf "ANCHOR_ROUTE: good\nchosen deterministically\n"'"#,
            "completed",
            None,
        ),
        (
            "missing-route",
            r#"sh -c 'printf "check passed but no choice\n"'"#,
            "failed",
            Some("multiple exits require route"),
        ),
        (
            "invalid-route",
            r#"sh -c 'printf "ANCHOR_ROUTE: elsewhere\n"'"#,
            "failed",
            Some("route `elsewhere` is not an exit"),
        ),
        (
            "failed-with-marker",
            r#"sh -c 'printf "ANCHOR_ROUTE: good\n"; exit 1'"#,
            "failed",
            Some("exit_code=Some(1)"),
        ),
    ];

    for (run_id, gate_command, expected_status, expected_error) in scenarios {
        let response = finish(spawn(root.path(), run_id, &graph(gate_command)));
        assert_eq!(response["status"], expected_status, "{response}");
        let saved = record(root.path(), run_id);
        if let Some(expected_error) = expected_error {
            assert!(
                saved["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(expected_error),
                "{saved}"
            );
            assert!(saved["results"].get("good").is_none());
            assert!(saved["results"].get("bad").is_none());
        } else {
            assert_eq!(saved["results"]["gate"][0]["completion"]["route"], "good");
            assert_eq!(
                saved["results"]["gate"][0]["completion"]["submission"],
                "chosen deterministically"
            );
            assert_eq!(saved["results"]["good"].as_array().unwrap().len(), 1);
            assert!(saved["results"].get("bad").is_none());
        }
    }
}
