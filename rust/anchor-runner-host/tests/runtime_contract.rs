//! Small deterministic Graphs executed by the production Host, Rig, io-harness and Bubblewrap.
//! Run with `cargo test -p anchor-runner-host --test runtime_contract -- --nocapture`.
#[path = "support/feedback_contract.rs"]
mod feedback_contract;
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[path = "support/graph_deletion_contract.rs"]
mod graph_deletion_contract;
#[path = "support/lifecycle_contract.rs"]
mod lifecycle_contract;
#[path = "support/pilot_mutations_contract.rs"]
mod pilot_mutations_contract;
#[path = "support/platform_contract.rs"]
mod platform_contract;
#[path = "support/plugin_contract.rs"]
mod plugin_contract;

use fixture::{Host, Provider, Reply, command, complete, evidence, read_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn worker() -> Value {
    json!({"model":"models.worker","instructions":"deterministic Runtime fixture","wall_time_limit_seconds":30})
}

#[test]
fn native_platform_session_http_lifecycle_survives_host_restart_without_model_calls() {
    let provider = Provider::new(std::iter::empty::<(&str, Vec<Reply>)>());
    let host = Host::new(&json!({
        "entry":"idle","ops":{"idle":{"run":"true"}},
        "nodes":[{"id":"idle","op":"idle"}],"edges":[]
    }));
    let server = host.serve(&provider);
    let (status, created) = server.request("POST", "/sessions", Some(&json!({"id":"native-http"})));
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["session"]["status"], "active");
    let (status, renamed) = server.request(
        "PUT",
        "/sessions/native-http",
        Some(&json!({"title":"native restart"})),
    );
    assert_eq!(status, 200, "{renamed}");
    drop(server);
    let server = host.serve(&provider);
    let (status, reloaded) = server.request("GET", "/sessions/native-http", None);
    assert_eq!(status, 200, "{reloaded}");
    assert_eq!(reloaded, renamed);
    let (status, events) = server.request("GET", "/sessions/native-http/events?after=1", None);
    assert_eq!(status, 200);
    assert_eq!(events["events"].as_array().unwrap().len(), 1);
    let (status, boundary) = server.request(
        "POST",
        "/sessions/native-http/turns",
        Some(&json!({"message":"must not call model"})),
    );
    assert_eq!(status, 400, "{boundary}");
    assert!(!host.root.path().join("state/io-harness").exists());
    assert!(provider.requests().is_empty());
    provider.assert_consumed();
    fixture::evidence_rejection(
        "native-session-lifecycle",
        &host,
        &provider,
        boundary,
        json!({"platform_session_storage":"Rust SQLite", "restart":true,"events":events,
               "model_calls":0,"invalid_turn_request_rejected":true}),
    );
}

fn wait_turn(server: &fixture::HttpHost, session: &str, turn: &str, expected: &str) -> Value {
    let mut terminal = Value::Null;
    fixture::wait_until(expected, || {
        let (status, result) =
            server.request("GET", &format!("/sessions/{session}/turns/{turn}"), None);
        assert_eq!(status, 200, "{result}");
        terminal = result["turn"].clone();
        if terminal["status"] != "running" {
            assert_eq!(terminal["status"], expected, "{terminal}");
            true
        } else {
            false
        }
    });
    terminal
}

#[test]
fn native_pilot_tools_history_idempotency_restart_and_sse_cursor_replay() {
    let provider = Provider::new([(
        "fixture-default",
        vec![
            Reply::Tool("graph_read", json!({"graph":"fixture"})),
            Reply::Text("The saved graph has one idle node. #anchor/graph/fixture".into()),
            Reply::Tool("graph_read", json!({"graph":"fixture"})),
            Reply::Text("I remember the first request and its Graph result.".into()),
        ],
    )]);
    let host = Host::new(
        &json!({"entry":"idle","ops":{"idle":{"run":"true"}},"nodes":[{"id":"idle","op":"idle"}],"edges":[]}),
    );
    let server = host.serve(&provider);
    assert_eq!(
        server
            .request("POST", "/sessions", Some(&json!({"id":"pilot-native"})))
            .0,
        201
    );
    assert_eq!(
        server
            .request("GET", "/sessions/pilot-native/messages", None)
            .1["messages"],
        json!([])
    );
    let request =
        json!({"request_id":"first-request","message":"Read the fixture graph, do not run it."});
    let (status, accepted) = server.request("POST", "/sessions/pilot-native/turns", Some(&request));
    assert_eq!(status, 202, "{accepted}");
    let turn = accepted["turn"]["id"].as_str().unwrap();
    let terminal = wait_turn(&server, "pilot-native", turn, "completed");
    let (status, repeated) = server.request("POST", "/sessions/pilot-native/turns", Some(&request));
    assert_eq!(status, 202, "{repeated}");
    assert_eq!(repeated["turn"], terminal);
    assert_eq!(
        server
            .request(
                "POST",
                "/sessions/pilot-native/turns",
                Some(&json!({"request_id":"first-request","message":"different input"}))
            )
            .0,
        409
    );
    let path = format!("/sessions/pilot-native/turns/{turn}/events");
    let (status, initial) = server.events(&path, None);
    assert_eq!(status, 200);
    assert!(
        initial.contains("tool-input-available") && initial.contains("graph_read"),
        "{initial}"
    );
    assert!(
        initial.contains("tool-output-available") && initial.contains("text-delta"),
        "{initial}"
    );
    assert!(
        initial.contains("event: turn") && initial.contains("completed"),
        "{initial}"
    );
    let cursor = initial
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .map(|id| id.parse::<u64>().unwrap())
        .max()
        .unwrap();
    let (_, tail) = server.events(&path, Some(cursor));
    assert!(
        !tail.contains("id: ") && tail.contains("event: turn"),
        "{tail}"
    );
    let (status, accepted) = server.request(
        "POST",
        "/sessions/pilot-native/turns",
        Some(&json!({"request_id":"second-request","message":"Recall the first request."})),
    );
    assert_eq!(status, 202, "{accepted}");
    wait_turn(
        &server,
        "pilot-native",
        accepted["turn"]["id"].as_str().unwrap(),
        "completed",
    );
    let history = server
        .request("GET", "/sessions/pilot-native/messages", None)
        .1;
    assert!(
        history.to_string().contains("Read the fixture graph")
            && history.to_string().contains("remember"),
        "{history}"
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    for request in &requests {
        let names = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(names.contains(&"graph_read"));
        assert_eq!(names.len(), 15);
        assert!(
            !names.contains(&"final_result")
                && !names.contains(&"run")
                && !names.contains(&"anchor_run")
                && !names.contains(&"graph_delete"),
            "{names:?}"
        );
        for forbidden in [
            "exec",
            "shell",
            "read_file",
            "write_file",
            "grep",
            "git_commit",
            "ask_question",
        ] {
            assert!(!names.contains(&forbidden), "{names:?}");
        }
    }
    assert!(requests[1].to_string().contains("idle"));
    assert!(
        requests[2]
            .to_string()
            .contains("The saved graph has one idle node")
    );
    let second_call_ids = requests[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["tool_calls"].as_array().into_iter().flatten())
        .filter(|call| call["function"]["name"] == "graph_read")
        .map(|call| call["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(second_call_ids, vec!["call-3"]);
    drop(server);
    let server = host.serve(&provider);
    assert_eq!(
        server
            .request("GET", "/sessions/pilot-native/messages", None)
            .1,
        history
    );
    assert_eq!(server.events(&path, None).1, initial);
    assert_eq!(provider.requests().len(), 4);
    assert!(!host.root.path().join("state/runs").exists());
    provider.assert_consumed();
    fixture::evidence_rejection(
        "native-pilot-session",
        &host,
        &provider,
        terminal,
        json!({"history":history,"sse_cursor":cursor,"idempotency":true,"restart":true,"hidden_graph":false,"real_model_calls":0}),
    );
}

#[test]
fn native_pilot_stop_blocks_replacement_and_restart_keeps_unfinished_history() {
    let gate = fixture::Gate::new();
    let crash_gate = fixture::Gate::new();
    let provider = Provider::new([(
        "fixture-default",
        vec![
            Reply::Gated(
                gate.clone(),
                Box::new(Reply::Tool("graph_read", json!({"graph":"fixture"}))),
            ),
            Reply::Gated(
                crash_gate.clone(),
                Box::new(Reply::Text("discarded in the killed Host".into())),
            ),
            Reply::Text("I inspect the interrupted conversation, not replay it.".into()),
        ],
    )]);
    let host = Host::new(
        &json!({"entry":"idle","ops":{"idle":{"run":"true"}},"nodes":[{"id":"idle","op":"idle"}],"edges":[]}),
    );
    let server = host.serve(&provider);
    assert_eq!(
        server
            .request("POST", "/sessions", Some(&json!({"id":"pilot-stop"})))
            .0,
        201
    );
    let (status, first) = server.request(
        "POST",
        "/sessions/pilot-stop/turns",
        Some(&json!({"request_id":"stop-request","message":"First stopped request"})),
    );
    assert_eq!(status, 202, "{first}");
    gate.wait_entered();
    assert_eq!(
        server.request("POST", "/sessions/pilot-stop/stop", None).1["stopping"],
        true
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/sessions/pilot-stop/turns",
                Some(&json!({"request_id":"overlap","message":"must not overlap"}))
            )
            .0,
        409
    );
    gate.open();
    wait_turn(
        &server,
        "pilot-stop",
        first["turn"]["id"].as_str().unwrap(),
        "stopped",
    );
    let (_, second) = server.request(
        "POST",
        "/sessions/pilot-stop/turns",
        Some(&json!({"request_id":"crash-request","message":"Second interrupted request"})),
    );
    crash_gate.wait_entered();
    assert_eq!(
        server.request("DELETE", "/sessions/pilot-stop", None).0,
        409
    );
    drop(server);
    crash_gate.open();
    let server = host.serve(&provider);
    let interrupted = wait_turn(
        &server,
        "pilot-stop",
        second["turn"]["id"].as_str().unwrap(),
        "interrupted",
    );
    let request = json!({"request_id":"crash-request","message":"Second interrupted request"});
    assert_eq!(
        server
            .request("POST", "/sessions/pilot-stop/turns", Some(&request))
            .1["turn"],
        interrupted
    );
    assert_eq!(provider.requests().len(), 2);
    let (_, resumed) = server.request(
        "POST",
        "/sessions/pilot-stop/turns",
        Some(&json!({"request_id":"continue-request","resume":true})),
    );
    wait_turn(
        &server,
        "pilot-stop",
        resumed["turn"]["id"].as_str().unwrap(),
        "completed",
    );
    assert_eq!(provider.requests().len(), 3);
    let history = server
        .request("GET", "/sessions/pilot-stop/messages", None)
        .1;
    assert!(
        history.to_string().contains("First stopped request")
            && history.to_string().contains("Second interrupted request"),
        "{history}"
    );
    assert!(
        provider.requests()[2]
            .to_string()
            .contains("Second interrupted request")
    );
    provider.assert_consumed();
    fixture::evidence_rejection(
        "native-pilot-stop-restart",
        &host,
        &provider,
        interrupted,
        json!({"history":history,"stop":true,"same_session_exclusion":true,"restart_interruption":true,"no_automatic_replay":true,"real_model_calls":0}),
    );
}

fn assert_manifest(host: &Host, saved: &Value, node: &str, name: &str, expected: &[u8]) {
    assert_eq!(host.file(saved, node, name), expected);
    let manifest = read_json(host.artifact(saved, node).join("manifest.json"));
    assert_eq!(
        manifest["files"][name]["sha256"],
        format!("{:x}", Sha256::digest(expected))
    );
    assert_eq!(manifest["files"][name]["bytes"], expected.len());
}

#[test]
fn agent_tool_alias_readonly_artifact_and_completed_reopen() {
    let provider = Provider::new([(
        "fixture-worker",
        vec![
            command(
                "set -eu; test ! -e source.txt; cat /in/producer/source.txt > report.txt; printf once >> effects.txt",
            ),
            complete(Some("verify")),
        ],
    )]);
    let host = Host::new(&json!({
        "objective":"Op to Agent to Op", "entry":"producer", "agents":{"worker":worker()},
        "ops":{
            "produce":{"run":"sh -c 'printf native-evidence > source.txt'"},
            "verify":{"run":"sh -c 'set -eu; if printf corrupt > /in/worker/report.txt; then exit 4; fi; cat /in/worker/report.txt > verified.txt; git --git-dir=/in/worker/.git rev-parse HEAD > reviewed-git.txt'"}
        },
        "nodes":[{"id":"producer","op":"produce"},{"id":"worker","agent":"worker"},{"id":"verify","op":"verify"}],
        "edges":[{"from":"producer","to":"worker"},{"from":"worker","to":"verify"}]
    }));
    assert_eq!(host.run(&provider)["status"], "completed");
    let saved = host.record();
    assert_manifest(&host, &saved, "verify", "verified.txt", b"native-evidence");
    assert_manifest(&host, &saved, "worker", "effects.txt", b"once");
    let git = String::from_utf8(host.file(&saved, "verify", "reviewed-git.txt")).unwrap();
    let projection = read_json(
        host.artifact(&saved, "worker")
            .join("git-view/projection.json"),
    );
    assert_eq!(projection["head"], git.trim());
    assert_eq!(
        projection["artifact"],
        saved["results"]["worker"][0]["commit"]
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request["model"], "fixture-worker");
        let tools = request["tools"].as_array().unwrap();
        assert!(
            tools
                .iter()
                .any(|tool| tool["function"]["name"] == "anchor_run")
        );
        assert!(
            tools
                .iter()
                .any(|tool| tool["function"]["name"] == "final_result")
        );
    }
    assert!(requests[1]["messages"].to_string().contains("exit_code"));
    let history = host.history("fixture", "worker", 1);
    assert!(history.iter().any(|message| message["role"] == "assistant"
        && message["commands"].to_string().contains("anchor_run")));
    assert!(history.iter().any(|message| {
        message["role"] == "tool"
            && message["text"]
                .as_str()
                .is_some_and(|text| text.contains("exit_code"))
    }));
    assert_eq!(host.run(&provider)["status"], "completed");
    assert_eq!(host.record(), saved);
    assert_eq!(provider.requests(), requests);
    provider.assert_consumed();
    evidence(
        "serial-artifact-reopen",
        &host,
        &provider,
        json!({"readonly_input":true,"git_projection":true,"artifact_hashes":true,"completed_reopen_requests":0,"tool_effects":1}),
    );
}

#[test]
fn invalid_completion_route_is_corrected_before_any_branch_commits() {
    let provider = Provider::new([(
        "fixture-worker",
        vec![complete(Some("unknown")), complete(Some("good"))],
    )]);
    let host = Host::new(&json!({
        "entry":"worker","agents":{"worker":worker()},
        "ops":{"good":{"run":"true"},"bad":{"run":"true"}},
        "nodes":[{"id":"worker","agent":"worker"},{"id":"good","op":"good"},{"id":"bad","op":"bad"}],
        "edges":[{"from":"worker","to":"good"},{"from":"worker","to":"bad"}]
    }));
    assert_eq!(host.run(&provider)["status"], "completed");
    let saved = host.record();
    assert!(saved["results"].get("good").is_some());
    assert!(saved["results"].get("bad").is_none());
    assert_eq!(saved["results"]["worker"].as_array().unwrap().len(), 1);
    assert_eq!(saved["results"]["worker"][0]["key"]["invocation"], 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]["messages"].to_string().contains("output shape"));
    provider.assert_consumed();
    evidence(
        "completion-correction",
        &host,
        &provider,
        json!({"corrected_route":"good","invalid_branch_commits":0,"invocations":1}),
    );
}

#[test]
fn retryable_http_failure_keeps_same_invocation_and_tool_effect_once() {
    let provider = Provider::new([(
        "fixture-worker",
        vec![
            command("printf once >> effects.txt"),
            Reply::Unavailable,
            complete(None),
        ],
    )]);
    let host = Host::new(&json!({
        "entry":"worker","agents":{"worker":worker()},
        "nodes":[{"id":"worker","agent":"worker"}],"edges":[]
    }));
    assert_eq!(host.run(&provider)["status"], "completed");
    let saved = host.record();
    assert_manifest(&host, &saved, "worker", "effects.txt", b"once");
    assert_eq!(saved["results"]["worker"][0]["key"]["invocation"], 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1]["messages"], requests[2]["messages"]);
    assert_eq!(host.run(&provider)["status"], "completed");
    assert_eq!(host.record(), saved);
    assert_eq!(provider.requests().len(), 3);
    provider.assert_consumed();
    evidence(
        "provider-retry",
        &host,
        &provider,
        json!({"http_503_failures":1,"provider_requests":3,"tool_effects":1,"invocations":1,"completed_reopen_requests":0}),
    );
}

#[test]
fn parallel_agent_branches_join_exact_artifacts_and_reopen_without_replay() {
    let provider = Provider::new([
        (
            "fixture-left",
            vec![
                command(
                    "cat /in/seed/seed.txt > result.txt; printf L >> result.txt; printf once >> effects.txt",
                ),
                complete(Some("join")),
            ],
        ),
        (
            "fixture-right",
            vec![
                command(
                    "cat /in/seed/seed.txt > result.txt; printf R >> result.txt; printf once >> effects.txt",
                ),
                complete(Some("join")),
            ],
        ),
    ]);
    let host = Host::new(&json!({
        "entry":"seed", "agents":{
            "left":{"model":"models.left","instructions":"left fixture","wall_time_limit_seconds":30},
            "right":{"model":"models.right","instructions":"right fixture","wall_time_limit_seconds":30}
        },
        "ops":{
            "seed":{"run":"sh -c 'printf seed > seed.txt'"},
            "fork":{"fanout":{"join":"join"}}, "join":{"join":{}},
            "verify":{"run":"sh -c 'cat /in/left/result.txt /in/right/result.txt > combined.txt; cat /in/join/join.json > join-copy.json'"}
        },
        "nodes":[{"id":"seed","op":"seed"},{"id":"fork","op":"fork"},{"id":"left","agent":"left"},{"id":"right","agent":"right"},{"id":"join","op":"join"},{"id":"verify","op":"verify"}],
        "edges":[{"from":"seed","to":"fork"},{"from":"fork","to":"left"},{"from":"fork","to":"right"},{"from":"left","to":"join"},{"from":"right","to":"join"},{"from":"join","to":"verify"}]
    }));
    assert_eq!(host.run(&provider)["status"], "completed");
    let saved = host.record();
    assert_manifest(&host, &saved, "verify", "combined.txt", b"seedLseedR");
    for node in ["left", "right"] {
        assert_manifest(&host, &saved, node, "effects.txt", b"once");
    }
    let joined: Value =
        serde_json::from_slice(&host.file(&saved, "verify", "join-copy.json")).unwrap();
    assert_eq!(joined["branches"].as_array().unwrap().len(), 2);
    for branch in joined["branches"].as_array().unwrap() {
        for node in branch["nodes"].as_array().unwrap() {
            assert_eq!(
                node["commit"],
                saved["results"][node["node"].as_str().unwrap()][0]["commit"]
            );
        }
    }
    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(host.run(&provider)["status"], "completed");
    assert_eq!(host.record(), saved);
    assert_eq!(provider.requests(), requests);
    provider.assert_consumed();
    evidence(
        "parallel-join-reopen",
        &host,
        &provider,
        json!({"branches":2,"provider_requests":4,"tool_effects_per_branch":1,"joined_commit_lineage":true,"completed_reopen_requests":0}),
    );
}

#[test]
fn terminal_provider_error_does_not_retry_or_commit_an_artifact() {
    let provider = Provider::new([("fixture-worker", vec![Reply::BadRequest])]);
    let host = Host::new(&json!({
        "entry":"worker","agents":{"worker":worker()},
        "nodes":[{"id":"worker","agent":"worker"}],"edges":[]
    }));
    let response = host.run(&provider);
    assert_eq!(response["status"], "failed", "{response}");
    assert!(host.record()["results"].get("worker").is_none());
    assert_eq!(provider.requests().len(), 1);
    provider.assert_consumed();
    evidence(
        "provider-terminal-error",
        &host,
        &provider,
        json!({"provider_requests":1,"retries":0,"artifacts_committed":0}),
    );
}
