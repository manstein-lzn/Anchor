#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
mod goose {
    include!("support/goose_fixture.rs");

    // Crash the owning process group while an HTTP request is still borrowed;
    // the ordinary mutable kill helper reaps it after the request exits.
    pub fn crash(server: &HttpHost) {
        assert!(
            Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", server.child.id())])
                .status()
                .unwrap()
                .success()
        );
    }

    pub fn interrupted_event(
        server: &HttpHost,
        event: &Value,
    ) -> Result<(u16, Value), reqwest::Error> {
        server.try_request_with_timeout(
            "POST",
            super::EVENTS,
            Some(event),
            Some(Duration::from_secs(30)),
        )
    }
}

use goose::{Gate, Host, HttpHost, Provider, Step, command, wait_until};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, thread, time::Duration};

const ALICE: &str = "plugin-turn-alice";
const BOB: &str = "plugin-turn-bob";
const EVENTS: &str = "/channels/plugin-assistant/events";
const GRAPH: &str = "plugin-assistant";

fn graph(mounted: bool) -> Value {
    json!({
        "objective":"Small persistent assistant Plugin Turn regression",
        "entry":"wait_input",
        "agents":{"assistant":{"model":"models.worker","instructions":"Inspect the current input and authorized tools. Keep the existing workspace and finish with route reply."}},
        "ops":{"wait_input":{"host":{"operation":"session.wait_input"}},"reply":{"host":{"operation":"session.reply"}}},
        "nodes":[{"id":"wait_input","op":"wait_input"},{"id":"assistant","agent":"assistant","plugins":if mounted {json!(["wecom","demo"])} else {json!(["wecom"])}},{"id":"reply","op":"reply"}],
        "edges":[{"from":"wait_input","to":"assistant"},{"from":"assistant","to":"reply"},{"from":"reply","to":"wait_input"}]
    })
}

fn host(mounted: bool) -> Host {
    let definition = graph(mounted);
    let host = Host::new(&definition)
        .default_runtime()
        .with_bot(GRAPH, &[ALICE, BOB])
        .with_extra_environment([
            ("ANCHOR_NODE_TOOL_DISCLOSURE", "0".to_owned()),
            ("ANCHOR_ASSISTANT_AUTO_RESUME", "0".to_owned()),
        ]);
    let root = host.base.root.path();
    let channel = root.join("plugins/wecom");
    fs::create_dir_all(channel.join("bin")).unwrap();
    fs::write(
        channel.join("plugin.json"),
        json!({"name":"Local channel fixture"}).to_string(),
    )
    .unwrap();
    fs::write(channel.join("channel.json"), json!({"platform":"wecom","transport":"websocket","entrypoint":"bin/gateway","required_environment":[]}).to_string()).unwrap();
    let gateway = channel.join("bin/gateway");
    fs::write(&gateway, "#!/bin/sh\nexec /bin/sleep 3600\n").unwrap();
    fs::set_permissions(gateway, fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.join("checkout/demo");
    fs::create_dir_all(&source).unwrap();
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/http_fixture");
    assert!(
        binary.is_file(),
        "build anchor-mcp-host --example http_fixture first"
    );
    fs::copy(binary, source.join("mcp-fixture")).unwrap();
    assert!(
        std::process::Command::new("strip")
            .arg(source.join("mcp-fixture"))
            .status()
            .unwrap()
            .success()
    );
    fs::write(source.join("plugin.json"), json!({"name":"Turn echo capability","description":"Frozen Turn echo tool","mcpServers":{"service":{"command":"mcp-fixture","args":["--stdio","echo","--prefix","turn:"]}}}).to_string()).unwrap();
    anchor_library::Library::new(root)
        .install_directory("demo", &source, false)
        .unwrap();
    fs::write(root.join("bundle/manifest.json"), json!({"format":1,"graph":"graph.json","plugins":if mounted {json!([{"id":"wecom"},{"id":"demo"}])} else {json!([{"id":"wecom"}])}}).to_string()).unwrap();
    let assistant = root.join(GRAPH);
    fs::create_dir_all(&assistant).unwrap();
    fs::write(assistant.join("graph.json"), definition.to_string()).unwrap();
    fs::copy(
        root.join("bundle/manifest.json"),
        assistant.join("manifest.json"),
    )
    .unwrap();
    host
}

fn event(user: &str, id: &str) -> Value {
    json!({"event":{"source":"wecom","event_id":id,"sender_id":user,"conversation_id":"local-plugin-turns","text":id,"reply_target":"local-plugin-turns","message_type":"text","metadata":{"chat_type":"single","request_id":id},"attachments":[]}})
}

fn submit(server: &HttpHost, user: &str, id: &str, expected: &str) -> Value {
    let (status, response) = server.request_with_timeout(
        "POST",
        EVENTS,
        Some(&event(user, id)),
        Duration::from_secs(30),
    );
    if status != 200 {
        let (_, runs) = server.request("GET", "/runs", None);
        for row in runs["runs"].as_array().into_iter().flatten() {
            if let Some(id) = row["run"].as_str() {
                let (_, detail) = server.request("GET", &format!("/runs/{id}"), None);
                eprintln!("failed fixture Run {id}: {}", detail["state"]);
            }
        }
    }
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["text"], expected, "{response}");
    response
}

fn save(server: &HttpHost, mounted: bool) -> Value {
    let (status, saved) = server.request(
        "PUT",
        &format!("/graphs/{GRAPH}"),
        Some(&json!({"definition":graph(mounted)})),
    );
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["plugin_activation"]["mode"], "next_turn", "{saved}");
    saved
}

fn finish(summary: &'static str) -> Step {
    Step::tool(
        "final_result",
        json!({"files":[],"summary":summary,"route":"reply"}),
    )
}

fn echo(value: &'static str) -> Step {
    Step::tool("demo-service_echo", json!({"value":value}))
}

fn unavailable() -> Step {
    Step::tool(
        "anchor_tools_call",
        json!({"name":"demo-service_echo","arguments":{"value":"must-not-run"}}),
    )
}

fn waiting(host: &Host, run: &str, replies: usize) -> Value {
    let mut record = Value::Null;
    wait_until("assistant returning to wait_input", || {
        record = host.record(run);
        assert!(
            matches!(
                record["projection"]["phase"].as_str(),
                Some("executing" | "waiting")
            ),
            "{record}"
        );
        record["frontier"][0]["node_id"] == record["snapshot"]["entry"]
            && record["results"]["reply"]
                .as_array()
                .is_some_and(|values| values.len() == replies)
    });
    record
}

fn workspace(host: &Host, run: &str) -> PathBuf {
    host.base
        .root
        .path()
        .join("work")
        .join(run)
        .join("nodes")
        .join(format!("{:x}", Sha256::digest(b"assistant")))
}

fn records(host: &Host, server: &HttpHost) -> Vec<Value> {
    let (status, listed) = server.request("GET", "/runs", None);
    assert_eq!(status, 200, "{listed}");
    listed["runs"]
        .as_array()
        .expect("Host Run list")
        .iter()
        .map(|row| host.record(row["run"].as_str().expect("Host Run identity")))
        .collect()
}

fn stop(server: &HttpHost, run: &str) {
    let (status, saved) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{saved}");
    server.wait_status(run, "stopped");
}

fn advertised(request: &Value) -> bool {
    goose::tool_definition(request, "demo-service_echo").is_ok()
}

fn evidence(host: &Host, provider: &Provider, runs: &[&str], checks: Value) {
    provider.assert_consumed();
    fs::write(provider.root.join("evidence.json"), serde_json::to_vec_pretty(&json!({
        "status":"passed","case_source":"tests/goose_plugin_turns.rs",
        "runtime":"real Goose native loop over ACP",
        "runtime_description":"actual pinned Goose ACP, Host, KernelScheduler, Sandbox and stdio Plugin MCP",
        "goose_binary_sha256":goose::GOOSE_SHA256,
        "host_binary_sha256":goose::digest(&host.base.root.path().join("anchor-runner-host")),
        "test_source_sha256":goose::digest(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goose_plugin_turns.rs")),
        "real_model_requests":0,"production_data_used":false,"dotenv_loaded":false,
        "provider_requests":provider.requests(),"runs":runs.iter().map(|run| host.record(run)).collect::<Vec<_>>(),"checks":checks
    })).unwrap()).unwrap();
    println!(
        "evidence: {}",
        provider.root.join("evidence.json").display()
    );
}

#[test]
#[ignore = "requires pinned Goose binary, built http_fixture and local Bubblewrap"]
fn add_call_remove_preserves_run_native_history_workspace_and_frozen_run_snapshot() {
    let provider = Provider::new("goose-plugin-turn-add-remove", vec![
        command("printf retained > state.txt; printf initial-workspace").expect_request_contains("plugin-initial-input"),
        finish("initial-reply").after("initial-workspace"),
        echo("added"),
        command("set -eu; test \"$(cat state.txt)\" = retained; printf plugin-used > added.txt; printf added-workspace").after("turn:added"),
        finish("added-reply").after("added-workspace"),
        unavailable(),
        command("set -eu; test \"$(cat added.txt)\" = plugin-used; printf removed-workspace").after("not available in this invocation"),
        finish("removed-reply").after("removed-workspace"),
    ]);
    let host = host(false);
    let server = host.serve(&provider);
    let first = submit(&server, ALICE, "plugin-initial-input", "initial-reply");
    let run = first["run"].as_str().unwrap();
    let initial = waiting(&host, run, 1);
    let native = host.native_fact(run, "assistant", 1);
    let added = save(&server, true);
    let second = submit(&server, ALICE, "plugin-added-input", "added-reply");
    waiting(&host, run, 2);
    let removed = save(&server, false);
    let third = submit(&server, ALICE, "plugin-removed-input", "removed-reply");
    let final_record = waiting(&host, run, 3);
    assert_eq!(second["run"], first["run"]);
    assert_eq!(third["run"], first["run"]);
    assert_eq!(third["session"], first["session"]);
    assert_eq!(records(&host, &server).len(), 1);
    assert_eq!(final_record["snapshot"], initial["snapshot"]);
    assert_eq!(final_record["plugin_bindings"], initial["plugin_bindings"]);
    for round in [2, 3] {
        let current = host.native_fact(run, "assistant", round);
        assert_eq!(current["session_id"], native["session_id"]);
        assert_eq!(current["conversation_scope"], native["conversation_scope"]);
    }
    let requests = provider.requests();
    assert!(!advertised(&requests[0]));
    assert!(advertised(&requests[2]));
    assert!(!advertised(&requests[5]));
    assert!(
        requests[5]["messages"].to_string().contains("turn:added"),
        "native history must survive revocation"
    );
    let mut structural = graph(false);
    structural["agents"]["assistant"]["instructions"] = json!("changed workflow instructions");
    let (status, rejection) = server.request(
        "PUT",
        &format!("/graphs/{GRAPH}"),
        Some(&json!({"definition":structural})),
    );
    assert_eq!(status, 409, "{rejection}");
    assert_eq!(
        fixture::read_json(host.base.root.path().join(GRAPH).join("graph.json")),
        graph(false)
    );
    stop(&server, run);
    evidence(
        &host,
        &provider,
        &[run],
        json!({"same_native_session_and_workspace":true,"snapshot_unchanged":true,"added":added,"removed":removed,"structural_change_rejection":rejection}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary, built http_fixture and local Bubblewrap"]
fn operator_reapproves_changed_plugin_for_reselected_graph_digest() {
    reapprove_changed_plugin(false);
}

#[test]
#[ignore = "requires pinned Goose binary, built http_fixture and local Bubblewrap"]
fn operator_reapproves_changed_plugin_for_initially_admitted_graph_digest() {
    reapprove_changed_plugin(true);
}

fn reapprove_changed_plugin(initially_mounted: bool) {
    let provider = Provider::new(
        if initially_mounted {
            "goose-plugin-turn-reapprove-admitted"
        } else {
            "goose-plugin-turn-reapprove-reselected"
        },
        vec![
            finish("before-approval"),
            echo("reauthorized"),
            finish("after-approval").after("turn:reauthorized"),
        ],
    );
    let host = host(initially_mounted);
    let server = host.serve(&provider);
    let first = submit(&server, ALICE, "before-approval-input", "before-approval");
    let run = first["run"].as_str().unwrap();
    let initial = waiting(&host, run, 1);
    let native = host.native_fact(run, "assistant", 1);
    let policy_root = host.base.state_root().join("assistant-plugin-turns");
    let frozen_policies = fs::read_dir(&policy_root)
        .unwrap()
        .map(Result::unwrap)
        .flat_map(|directory| fs::read_dir(directory.path()).unwrap().map(Result::unwrap))
        .map(|entry| (entry.path(), fs::read(entry.path()).unwrap()))
        .collect::<Vec<_>>();
    assert!(!frozen_policies.is_empty());
    save(&server, true);
    save(&server, false);
    let source = host.base.root.path().join("checkout/demo");
    let mut plugin = fixture::read_json(source.join("plugin.json"));
    plugin["mutable"] = json!(["cache/"]);
    fs::create_dir_all(source.join("cache")).unwrap();
    fs::write(source.join("plugin.json"), plugin.to_string()).unwrap();
    anchor_library::Library::new(host.base.root.path())
        .install_directory("demo", &source, true)
        .unwrap();
    save(&server, true);
    for (path, bytes) in frozen_policies {
        assert_eq!(
            fs::read(path).unwrap(),
            bytes,
            "reapproval rewrote a frozen Turn"
        );
    }
    let second = submit(&server, ALICE, "after-approval-input", "after-approval");
    let after = waiting(&host, run, 2);
    assert_eq!(second["run"], first["run"]);
    assert_eq!(after["snapshot"], initial["snapshot"]);
    assert_eq!(after["plugin_bindings"], initial["plugin_bindings"]);
    assert_eq!(
        host.native_fact(run, "assistant", 2)["session_id"],
        native["session_id"]
    );
    stop(&server, run);
    evidence(
        &host,
        &provider,
        &[run],
        json!({"initially_mounted":initially_mounted,"operator_reapproval":true,"same_run_and_history":true}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary, built http_fixture and local Bubblewrap"]
fn save_during_actual_tool_keeps_current_turn_and_other_users_adopt_independently() {
    let provider = Provider::new("goose-plugin-turn-concurrent", vec![
        echo("bob-initial"),
        command("printf bob-secret > bob.txt; printf bob-ready").after("turn:bob-initial"),
        finish("bob-initial-reply").after("bob-ready"),
        command("printf alice-secret > alice.txt; printf blocked > blocked; while test ! -f release; do sleep 0.02; done; printf alice-released"),
        unavailable(),
        command("set -eu; test \"$(cat bob.txt)\" = bob-secret; test ! -e alice.txt; printf bob-next").after("not available in this invocation"),
        finish("bob-next-reply").after("bob-next"),
        echo("alice-still-authorized").after("alice-released"),
        finish("alice-current-reply").after("turn:alice-still-authorized"),
        unavailable(),
        command("set -eu; test \"$(cat alice.txt)\" = alice-secret; test ! -e bob.txt; printf alice-next").after("not available in this invocation"),
        finish("alice-next-reply").after("alice-next"),
    ]);
    let host = host(true);
    let server = host.serve(&provider);
    let bob = submit(&server, BOB, "bob-initial-input", "bob-initial-reply");
    let bob_run = bob["run"].as_str().unwrap();
    waiting(&host, bob_run, 1);
    let (alice, bob_next) = thread::scope(|scope| {
        let pending =
            scope.spawn(|| submit(&server, ALICE, "alice-current-input", "alice-current-reply"));
        let mut alice_run = String::new();
        wait_until("actual anchor_run tool waiting in Alice workspace", || {
            for record in records(&host, &server) {
                let candidate = record["run_id"].as_str().unwrap();
                if candidate != bob_run && workspace(&host, candidate).join("blocked").is_file() {
                    alice_run = candidate.to_owned();
                    return true;
                }
            }
            false
        });
        struct Release(PathBuf);
        impl Drop for Release {
            fn drop(&mut self) {
                let _ = fs::write(&self.0, "release");
            }
        }
        let release = Release(workspace(&host, &alice_run).join("release"));
        save(&server, false);
        let next = submit(&server, BOB, "bob-next-input", "bob-next-reply");
        waiting(&host, bob_run, 2);
        assert!(
            !pending.is_finished(),
            "Alice's tool must still own the current Turn"
        );
        drop(release);
        (pending.join().unwrap(), next)
    });
    let alice_run = alice["run"].as_str().unwrap();
    waiting(&host, alice_run, 1);
    let alice_next = submit(&server, ALICE, "alice-next-input", "alice-next-reply");
    waiting(&host, alice_run, 2);
    assert_eq!(bob_next["run"], bob["run"]);
    assert_eq!(alice_next["run"], alice["run"]);
    assert_ne!(bob["session"], alice["session"]);
    assert_ne!(
        host.native_fact(bob_run, "assistant", 1)["conversation_scope"],
        host.native_fact(alice_run, "assistant", 1)["conversation_scope"]
    );
    let requests = provider.requests();
    assert!(advertised(&requests[3]));
    assert!(!advertised(&requests[4]));
    assert!(advertised(&requests[7]));
    assert!(!advertised(&requests[9]));
    assert!(!requests[4]["messages"].to_string().contains("alice-secret"));
    assert!(!requests[7]["messages"].to_string().contains("bob-secret"));
    stop(&server, alice_run);
    stop(&server, bob_run);
    evidence(
        &host,
        &provider,
        &[alice_run, bob_run],
        json!({"actual_tool_barrier":true,"current_turn_kept_authority":true,"users_adopted_next_turn_independently":true,"isolated_workspaces_and_histories":true}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary, built http_fixture and local Bubblewrap"]
fn default_restart_handover_restores_same_turn_grants_then_next_turn_revokes() {
    let gate = Gate::new();
    let provider = Provider::new(
        "goose-plugin-turn-restart",
        vec![
            echo("before-restart"),
            finish("cancelled-reply")
                .after("turn:before-restart")
                .gated(&gate),
        ],
    );
    let host = host(true).with_extra_environment([("ANCHOR_ASSISTANT_AUTO_RESUME", "1")]);
    let mut server = host.serve(&provider);
    let (run, native) = thread::scope(|scope| {
        let pending = scope
            .spawn(|| goose::interrupted_event(&server, &event(ALICE, "restart-frozen-input")));
        gate.wait_entered();
        let saved = records(&host, &server);
        assert_eq!(saved.len(), 1);
        let run = saved[0]["run_id"].as_str().unwrap().to_owned();
        let native = host.native_fact(&run, "assistant", 1);
        save(&server, false);
        goose::crash(&server);
        gate.open();
        assert!(pending.join().unwrap().is_err());
        (run, native)
    });
    server.kill();
    drop(server);
    provider.append(vec![
        echo("restored-frozen-grant").expect_request_contains("restart-frozen-input"),
        finish("restored-reply").after("turn:restored-frozen-grant"),
        unavailable(),
        command("printf next-turn-revoked").after("not available in this invocation"),
        finish("revoked-reply").after("next-turn-revoked"),
    ]);
    let restarted = host.serve(&provider);
    let session = host.record(&run)["input"]["session"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, binding) = restarted.request(
        "GET",
        &format!("/channel-sessions/{session}/assistant"),
        None,
    );
    assert_eq!(status, 200, "{binding}");
    let resumed = binding["assistant"]["run_id"].as_str().unwrap().to_owned();
    assert_ne!(
        resumed, run,
        "default recovery hands the instance to a new Run"
    );
    waiting(&host, &resumed, 1);
    assert_eq!(
        host.record(&resumed)["results"]["wait_input"][0]["completion"]["output"]["turn"],
        host.record(&run)["results"]["wait_input"][0]["completion"]["output"]["turn"]
    );
    assert_eq!(
        host.native_fact(&resumed, "assistant", 1)["session_id"],
        native["session_id"]
    );
    let next = submit(&restarted, ALICE, "restart-next-input", "revoked-reply");
    assert_eq!(next["run"], resumed);
    waiting(&host, &resumed, 2);
    assert_eq!(
        host.native_fact(&resumed, "assistant", 2)["session_id"],
        native["session_id"]
    );
    let requests = provider.requests();
    assert!(advertised(&requests[2]));
    assert!(!advertised(&requests[4]));
    stop(&restarted, &resumed);
    evidence(
        &host,
        &provider,
        &[&run, &resumed],
        json!({"default_restart_handover":true,"same_claimed_turn":true,"frozen_old_grants_restored":true,"following_turn_revoked":true,"same_native_session":true}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary, built http_fixture and local Bubblewrap"]
fn idle_restart_after_retirement_and_instruction_change_uses_new_turn_current_plugins() {
    retirement_and_idle_restart("wait_input", "goose-plugin-turn-retired-idle-restart");
}

#[test]
#[ignore = "requires pinned Goose binary, built http_fixture and local Bubblewrap"]
fn renamed_wait_after_retirement_and_idle_restart_uses_current_workflow() {
    retirement_and_idle_restart("current_wait", "goose-plugin-turn-renamed-wait-restart");
}

fn retirement_and_idle_restart(wait: &str, evidence_name: &str) {
    let provider = Provider::new(evidence_name, vec![
        command("printf retired-instance > retired.txt; printf retired-workspace-written")
            .expect_request_contains("retired-original-input"),
        finish("retired-original-reply").after("retired-workspace-written"),
        echo("current-before-restart").expect_request_contains("retired-new-instance-input"),
        command("set -eu; test ! -e retired-live-only.txt; test \"$(cat retired.txt)\" = retired-instance; printf current-instance > current.txt; printf current-workspace-written")
            .after("turn:current-before-restart"),
        finish("retired-new-instance-reply").after("current-workspace-written"),
        echo("current-after-restart").expect_request_contains("retired-restart-new-input"),
        command("set -eu; test \"$(cat current.txt)\" = current-instance; printf current-workspace-retained")
            .after("turn:current-after-restart"),
        finish("retired-restart-new-reply").after("current-workspace-retained"),
    ]);
    let host = host(false).with_extra_environment([("ANCHOR_ASSISTANT_AUTO_RESUME", "1")]);
    let mut server = host.serve(&provider);
    let first = submit(
        &server,
        ALICE,
        "retired-original-input",
        "retired-original-reply",
    );
    let old_run = first["run"].as_str().unwrap();
    let session = first["session"].as_str().unwrap();
    let original = waiting(&host, old_run, 1);
    fs::write(
        workspace(&host, old_run).join("retired-live-only.txt"),
        "uncommitted prior scene",
    )
    .unwrap();
    confirm_response(&server, "retired-original-input", &first);
    stop(&server, old_run);
    let assistant_path = format!("/channel-sessions/{session}/assistant");
    let (status, retired) = server.request(
        "POST",
        &format!("{assistant_path}/retire"),
        Some(&json!({"run_id":old_run})),
    );
    assert_eq!(status, 200, "{retired}");
    assert_eq!(retired["retired"], old_run);

    // Retiring permits an intentional new workflow. The previous conversation
    // remains history, but its old instructions and Plugin grants cannot become a
    // permission predecessor for a fresh Turn after a later idle restart.
    let mut definition = graph(true);
    definition["entry"] = json!(wait);
    definition["nodes"][0]["id"] = json!(wait);
    definition["edges"][0]["from"] = json!(wait);
    definition["edges"][2]["to"] = json!(wait);
    definition["agents"]["assistant"]["instructions"] = json!(
        "Use the current workflow's authorized tools, retain its workspace, and finish with route reply."
    );
    let (status, saved) = server.request(
        "PUT",
        &format!("/graphs/{GRAPH}"),
        Some(&json!({"definition":definition})),
    );
    assert_eq!(status, 200, "{saved}");
    let current = submit(
        &server,
        ALICE,
        "retired-new-instance-input",
        "retired-new-instance-reply",
    );
    let current_run = current["run"].as_str().unwrap();
    assert_ne!(current_run, old_run);
    assert_eq!(current["session"], first["session"]);
    let current_record = waiting(&host, current_run, 1);
    assert_ne!(current_record["snapshot"], original["snapshot"]);
    confirm_response(&server, "retired-new-instance-input", &current);
    let native = host.native_fact(current_run, "assistant", 1);
    assert_eq!(provider.requests().len(), 5);
    server.kill();
    drop(server);
    fs::copy(
        provider.root.join("host-http.log"),
        provider.root.join("host-before-idle-restart.log"),
    )
    .unwrap();

    let restarted = host.serve(&provider);
    let (status, binding) = restarted.request("GET", &assistant_path, None);
    assert_eq!(status, 200, "{binding}");
    let resumed = binding["assistant"]["run_id"].as_str().unwrap().to_owned();
    assert_ne!(resumed, current_run);
    wait_until("idle recovery waiting in the current workflow", || {
        let record = host.record(&resumed);
        assert_eq!(record["projection"]["phase"], "executing", "{record}");
        record["frontier"][0]["node_id"] == wait && record["results"].get("reply").is_none()
    });
    assert_eq!(
        provider.requests().len(),
        5,
        "idle recovery must not call the model"
    );
    let next = submit(
        &restarted,
        ALICE,
        "retired-restart-new-input",
        "retired-restart-new-reply",
    );
    assert_eq!(next["run"], resumed);
    let completed = waiting(&host, &resumed, 1);
    let new_turn = completed["results"][wait][0]["completion"]["output"]["turn"]
        .as_str()
        .unwrap();
    let previous_turn = current_record["results"][wait][0]["completion"]["output"]["turn"]
        .as_str()
        .unwrap();
    assert_ne!(new_turn, previous_turn);
    let sessions = anchor_platform_session::SessionStore::open(
        host.base.state_root().join("platform/sessions.sqlite"),
    )
    .unwrap();
    assert_eq!(
        sessions.get_turn("local", session, new_turn).unwrap().runs,
        vec![resumed.clone()]
    );
    assert_eq!(
        host.native_fact(&resumed, "assistant", 1)["session_id"],
        native["session_id"]
    );
    let requests = provider.requests();
    assert!(!advertised(&requests[0]));
    assert!(advertised(&requests[2]));
    assert!(advertised(&requests[5]));
    confirm_response(&restarted, "retired-restart-new-input", &next);
    stop(&restarted, &resumed);
    evidence(
        &host,
        &provider,
        &[old_run, current_run, &resumed],
        json!({
            "retired":retired,"published":saved,"responses":[first,current,next],
            "explicit_retirement_then_instruction_and_plugin_change":true,
            "current_wait_node":wait,"retired_uncommitted_scene_not_inherited":true,
            "previous_committed_files_seeded":true,
            "idle_restart_no_model_call":true,"fresh_turn_belongs_only_to_current_run":true,
            "current_plugin_called_after_restart":true,"current_workspace_and_native_session_retained":true,
            "old_retirement_boundary_not_used_for_permission_inheritance":true
        }),
    );
}

fn confirm_response(server: &HttpHost, id: &str, response: &Value) {
    let (status, settled) = server.request(
        "POST",
        EVENTS,
        Some(&json!({
            "event":event(ALICE, id)["event"],
            "settlement":{
                "key":response["receipt"]["key"],
                "content_sha256":response["receipt"]["content_sha256"],
                "status":"confirmed"
            }
        })),
    );
    assert_eq!(status, 200, "{settled}");
    assert_eq!(settled["delivery"]["status"], "confirmed", "{settled}");
}
