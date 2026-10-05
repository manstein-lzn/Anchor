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

fn spawn(root: &Path, run_id: &str, graph: &Value, input: &Value) -> Child {
    spawn_with_library(root, run_id, graph, input, None)
}

fn spawn_with_library(
    root: &Path,
    run_id: &str,
    graph: &Value,
    input: &Value,
    library: Option<&Path>,
) -> Child {
    let bundle = root.join("bundle");
    fs::create_dir_all(&bundle).unwrap();
    fs::write(bundle.join("graph.json"), graph.to_string()).unwrap();
    fs::write(
        bundle.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_anchor-runner-host"));
    command.env_remove("ANCHOR_RUNNER_LIBRARY_ROOT");
    if let Some(library) = library {
        command.env("ANCHOR_RUNNER_LIBRARY_ROOT", library);
    }
    let mut child = command
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
        "input":input
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

#[test]
fn operator_library_tools_reach_op_without_graph_host_path_grants() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    let tool = library.join("tools/sample");
    fs::create_dir_all(&tool).unwrap();
    fs::write(tool.join("run.sh"), "#!/bin/sh\nprintf external-tool\n").unwrap();
    fs::set_permissions(tool.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(tool.join("tool.json"), r#"{"entrypoint":"run.sh"}"#).unwrap();
    let definition = json!({
        "entry":"first", "ops":{"tool":{"run":"/tools/sample/run"}},
        "nodes":[{"id":"first","op":"tool"}], "edges":[]
    });
    let denied = finish(spawn(root.path(), "no-grant", &definition, &json!({})));
    assert_eq!(denied["status"], "failed", "{denied}");
    assert!(
        denied["error"]
            .as_str()
            .unwrap()
            .contains("exit_code=Some(127)"),
        "{denied}"
    );
    let response = finish(spawn_with_library(
        root.path(),
        "granted",
        &definition,
        &json!({}),
        Some(&library),
    ));
    assert_eq!(response["status"], "completed", "{response}");
    assert_eq!(
        record(root.path(), "granted")["results"]["first"][0]["completion"]["submission"],
        "external-tool"
    );
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
            r#"sh -c 'test "$ANCHOR_ROUTES" = "good,bad" && printf "ANCHOR_ROUTE: good\ninput=%s\n" "$ANCHOR_INPUT"'"#,
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
        let response = finish(spawn(
            root.path(),
            run_id,
            &graph(gate_command),
            &json!({"release":"binary"}),
        ));
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
                "input={\"release\":\"binary\"}"
            );
            assert_eq!(saved["results"]["good"].as_array().unwrap().len(), 1);
            assert!(saved["results"].get("bad").is_none());
        }
    }
}

#[test]
fn command_input_contains_merged_run_input_without_artifact_metadata() {
    let root = tempfile::tempdir().unwrap();
    let definition = json!({
        "objective":"pass effective input to each command",
        "entry":"first",
        "input":{"window":{"start":"default","end":"default"},"tags":["default"]},
        "ops":{"echo":{"run":r#"sh -c 'printf "%s" "$ANCHOR_INPUT"'"#}},
        "nodes":[{"id":"first","op":"echo"},{"id":"second","op":"echo"}],
        "edges":[{"from":"first","to":"second"}]
    });
    let input = json!({"window":{"end":"实际日期"},"tags":["a b","quote\""],"input":null});
    let expected = json!({
        "window":{"start":"default","end":"实际日期"},
        "tags":["a b","quote\""],
        "input":null
    });
    let response = finish(spawn(root.path(), "input", &definition, &input));
    assert_eq!(response["status"], "completed", "{response}");
    let saved = record(root.path(), "input");
    assert_eq!(saved["input"], expected);
    for node in ["first", "second"] {
        let submission = saved["results"][node][0]["completion"]["submission"]
            .as_str()
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(submission).unwrap(), expected);
    }
}

#[test]
fn original_shell_assignment_case_heredoc_and_redirect_use_native_route_helper() {
    let root = tempfile::tempdir().unwrap();
    let command = r#"set -eu
verdict=approved
cat > report.txt <<'REPORT'
quoted "content" and literal $verdict
second line
REPORT
case "$verdict" in
  approved) anchor-route --to good --reason "checked shell result" ;;
  *) anchor-route --to bad --reason rejected ;;
esac"#;
    let mut definition = graph(command);
    definition["ops"]["branch"]["run"] = json!("cat /in/gate/report.txt");
    let response = finish(spawn(
        root.path(),
        "original-shell",
        &definition,
        &json!({}),
    ));
    assert_eq!(response["status"], "completed", "{response}");
    let saved = record(root.path(), "original-shell");
    assert_eq!(saved["snapshot"]["ops"]["gate"]["run"], command);
    assert_eq!(saved["results"]["gate"][0]["completion"]["route"], "good");
    assert_eq!(
        saved["results"]["gate"][0]["completion"]["submission"],
        "checked shell result"
    );
    assert_eq!(
        saved["results"]["good"][0]["completion"]["submission"],
        "quoted \"content\" and literal $verdict\nsecond line"
    );
    assert!(saved["results"].get("bad").is_none());

    for (run_id, command) in [
        (
            "helper-invalid",
            "anchor-route --to elsewhere --reason rejected",
        ),
        (
            "helper-nonzero",
            "anchor-route --to good --reason incomplete; exit 7",
        ),
        ("shell-malformed", "printf 'unfinished"),
    ] {
        let response = finish(spawn(root.path(), run_id, &graph(command), &json!({})));
        assert_eq!(response["status"], "failed", "{response}");
        let saved = record(root.path(), run_id);
        assert!(saved["results"].get("good").is_none());
        assert!(saved["results"].get("bad").is_none());
    }
}

#[test]
fn op_network_declaration_controls_real_loopback_access() {
    use std::{
        io::Read,
        net::TcpListener,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let shell = format!("bash -c 'printf host-network-authorized > /dev/tcp/127.0.0.1/{port}'");
    let mut definition = json!({
        "entry":"network", "ops":{"connect":{"run":shell,"network":false}},
        "nodes":[{"id":"network","op":"connect"}], "edges":[]
    });
    let denied = finish(spawn(
        root.path(),
        "network-disabled",
        &definition,
        &json!({}),
    ));
    assert_eq!(denied["status"], "failed", "{denied}");
    assert!(listener.accept().is_err());

    let received = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut text = String::new();
                    stream.read_to_string(&mut text).unwrap();
                    return text;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "authorized command never connected"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("loopback fixture failed: {error}"),
            }
        }
    });
    definition["ops"]["connect"]["network"] = json!(true);
    let allowed = finish(spawn(
        root.path(),
        "network-enabled",
        &definition,
        &json!({}),
    ));
    assert_eq!(allowed["status"], "completed", "{allowed}");
    assert_eq!(received.join().unwrap(), "host-network-authorized");
}
