#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
mod goose {
    include!("support/goose_fixture.rs");

    pub fn serve_gated(host: &Host, provider: &Provider) -> HttpHost {
        let mut server = host.serve(provider);
        server.client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        server
    }
}

use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use anchor_platform_session::{ChannelDeliveryStatus, SessionStore, TurnStatus};
use anchor_runtime::graph::{GraphRunRecord, RunResult};
use base64::{Engine, engine::general_purpose::STANDARD};
use goose::{Gate, Host, HttpHost, Provider, Step, command, wait_until};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Cursor,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

const CASE_SOURCE: &str = "tests/goose_persistent_assistant.rs";
const EVENTS: &str = "/channels/wecom/events";
const ALICE: &str = "persistent-fixture-alice";
const BOB: &str = "persistent-fixture-bob";

fn graph() -> Value {
    json!({
        "objective":"Deterministic persistent assistant regression with no external channel delivery",
        "entry":"wait_input",
        "agents":{"assistant":{
            "model":"models.worker",
            "instructions":"Inspect the current input and workspace using authorized Anchor tools. Keep files in place between turns. Submit final_result with route reply only after inspecting actual tool feedback."
        }},
        "ops":{
            "wait_input":{"host":{"operation":"session.wait_input"}},
            "reply":{"host":{"operation":"session.reply"}}
        },
        "nodes":[
            {"id":"wait_input","op":"wait_input"},
            {"id":"assistant","agent":"assistant"},
            {"id":"reply","op":"reply"}
        ],
        "edges":[
            {"from":"wait_input","to":"assistant"},
            {"from":"assistant","to":"reply"},
            {"from":"reply","to":"wait_input"}
        ]
    })
}

fn host() -> Host {
    host_for(&graph())
}

fn host_for(definition: &Value) -> Host {
    Host::new(definition)
        .default_runtime()
        .with_extra_environment([
            ("ANCHOR_WECOM_GRAPH", "fixture".to_owned()),
            ("ANCHOR_WECOM_REPLY_NODE", "assistant".to_owned()),
            ("ANCHOR_WECOM_USERS", format!("{ALICE},{BOB}")),
        ])
}

/// The opt-out path: an operator who disables automatic recovery keeps the
/// explicit stop/resume workflow this Host had before.
fn host_without_auto_resume() -> Host {
    host_for(&graph()).with_extra_environment([("ANCHOR_ASSISTANT_AUTO_RESUME", "0".to_owned())])
}

fn event(user: &str, identity: &str, text: &str) -> Value {
    json!({"event":{
        "source":"wecom",
        "event_id":identity,
        "sender_id":user,
        "conversation_id":"persistent-fixture-conversation",
        "text":text,
        "reply_target":"persistent-fixture-conversation",
        "message_type":"text",
        "metadata":{"chat_type":"single","request_id":identity},
        "attachments":[]
    }})
}

fn finish(summary: &'static str) -> Step {
    Step::tool("final_result", json!({"summary":summary,"route":"reply"}))
}

fn submit(server: &HttpHost, body: &Value, expected: &str) -> Value {
    let (status, response) = server.request("POST", EVENTS, Some(body));
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["text"], expected, "{response}");
    let run = response["run"].as_str().unwrap();
    let identity = run.strip_prefix("assistant-").unwrap();
    assert_eq!(identity.len(), 36, "{run}");
    for (index, character) in identity.bytes().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            assert_eq!(character, b'-', "{run}");
        } else {
            assert!(character.is_ascii_hexdigit(), "{run}");
        }
    }
    assert!(
        response["session"]
            .as_str()
            .is_some_and(|session| !session.is_empty())
    );
    assert!(
        response["receipt"]["key"]
            .as_str()
            .is_some_and(|key| !key.is_empty())
    );
    assert_eq!(
        response["receipt"]["content_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    response
}

fn waiting(host: &Host, run: &str, replies: usize) -> Value {
    let mut saved = Value::Null;
    wait_until("persistent assistant waiting without a model call", || {
        saved = host.record(run);
        assert!(
            matches!(
                saved["status"].as_str(),
                Some("running" | "paused" | "ready")
            ),
            "{saved}"
        );
        saved["status"] == "running"
            && saved["results"]["reply"]
                .as_array()
                .is_some_and(|results| results.len() == replies)
            && saved["cursor"]["node_id"] == "wait_input"
            && saved["cursor"]["key"]["invocation"] == replies + 1
    });
    let record: GraphRunRecord = serde_json::from_value(saved.clone()).unwrap();
    assert!(
        record
            .snapshot
            .nodes
            .iter()
            .all(|node| node.max_rounds.is_none())
    );
    assert!(record.snapshot.module_rounds.is_empty());
    assert_eq!(record.results["wait_input"].len(), replies);
    assert_eq!(record.results["assistant"].len(), replies);
    saved
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

fn artifact(host: &Host, saved: &Value, invocation: usize) -> PathBuf {
    let commit = &saved["results"]["assistant"][invocation - 1]["commit"];
    let identity = commit["id"].as_str().unwrap();
    assert!(identity.starts_with("fs2-"), "{commit}");
    host.base.root.path().join("state/artifacts").join(identity)
}

fn snapshot_inventory(artifact: &Path) -> Value {
    json!({
        "manifest_sha256":goose::digest(&artifact.join("manifest.json")),
        "files":goose::file_inventory(&artifact.join("files"))
    })
}

fn evidence(host: &Host, provider: &Provider, run: &str, checks: Value) {
    provider.assert_consumed();
    let saved = host.record(run);
    let results = saved["results"]["assistant"].as_array().unwrap();
    let invocation = results.last().unwrap()["key"]["invocation"]
        .as_u64()
        .unwrap();
    let facts = results
        .iter()
        .map(|result| {
            host.native_fact(
                run,
                "assistant",
                result["key"]["invocation"].as_u64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let controlled = fixture::read_json(provider.root.join("provider.json"));
    assert_eq!(controlled["real_model_requests"], 0);
    let root = host.base.root.path();
    fs::write(provider.root.join("evidence.json"), serde_json::to_vec_pretty(&json!({
        "status":"passed","runtime":"real Goose native loop over ACP",
        "real_model_requests":controlled["real_model_requests"],"goose_version":"1.53.0",
        "goose_binary_sha256":goose::GOOSE_SHA256,"provider_url":provider.url,
        "host_binary_sha256":goose::digest(&root.join("anchor-runner-host")),
        "test_binary_sha256":goose::digest(&std::env::current_exe().unwrap()),
        "test_source_sha256":goose::digest(&Path::new(env!("CARGO_MANIFEST_DIR")).join(CASE_SOURCE)),
        "graph":fixture::read_json(root.join("bundle/graph.json")),
        "run":saved,"native_facts":facts,"provider_requests":provider.requests(),"checks":checks,
        "native_goose_conversation":host.native_conversation(run,"assistant",invocation),
        "workspace_files":goose::file_inventory(&root.join("work")),
        "state_files":goose::file_inventory(&root.join("state")),
        "production_data_used":false,"dotenv_loaded":false
    })).unwrap()).unwrap();
    println!(
        "evidence: {}",
        provider.root.join("evidence.json").display()
    );
}

fn native_round(host: &Host, provider: &Provider, run: &str, invocation: u64) -> Value {
    let (fact, native, _) = host.native_record(run, "assistant", invocation);
    assert_eq!(fact["binary_sha256"], goose::GOOSE_SHA256);
    assert_eq!(native["binary_sha256"], goose::GOOSE_SHA256);
    assert_eq!(native["initialize"]["agentInfo"]["version"], "1.53.0");
    assert_eq!(native["session"]["_meta"]["workingDir"], "/workspace");
    assert_eq!(native["runtime"], "goose");
    assert_eq!(native["model"], provider.model);
    assert_eq!(native["model_binding"], fact["model_binding"]);
    assert_eq!(
        native["observed_model_requests"],
        fact["completion"]["model_requests"]
    );
    assert!(
        native["observed_model_requests"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    let endpoint = reqwest::Url::parse(&provider.url).unwrap();
    assert_eq!(endpoint.scheme(), "http");
    assert_eq!(endpoint.host_str(), Some("127.0.0.1"));
    let controlled = fixture::read_json(provider.root.join("provider.json"));
    assert_eq!(controlled["real_model_requests"], 0);
    assert_eq!(controlled["model"], provider.model);
    assert_eq!(controlled["requests"], json!(provider.requests()));
    assert!(controlled["failures"].as_array().unwrap().is_empty());
    assert!(
        fact["session_id"]
            .as_str()
            .is_some_and(|session| !session.is_empty())
    );
    assert!(
        fact["conversation_scope"]
            .as_str()
            .is_some_and(|scope| scope.starts_with("gc1-"))
    );
    assert!(fact["completion"].is_object(), "{fact}");
    fact
}

fn idle(provider: &Provider, expected: usize) {
    assert_eq!(provider.requests().len(), expected);
    thread::sleep(Duration::from_millis(250));
    assert_eq!(
        provider.requests().len(),
        expected,
        "idle assistant invoked the model"
    );
}

fn entered<Output>(
    provider: &Provider,
    expected: usize,
    gate: &Gate,
    request: &thread::ScopedJoinHandle<'_, Output>,
) {
    wait_until("HTTP event reaching the gated local Provider", || {
        assert!(
            !request.is_finished(),
            "HTTP event exited before the Provider gate; inspect its response and Host log"
        );
        provider.requests().len() >= expected
    });
    gate.wait_entered();
}

fn stop(server: &HttpHost, provider: &Provider, run: &str) -> Value {
    let requests = provider.requests().len();
    let (status, response) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{response}");
    let stopped = server.wait_status(run, "stopped");
    assert_eq!(provider.requests().len(), requests);
    stopped
}

fn runs(host: &Host) -> Vec<Value> {
    fs::read_dir(host.base.root.path().join("state/runs"))
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|entry| fixture::read_json(entry.path()))
        .collect()
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn two_turns_reuse_run_workspace_and_native_session_without_mutating_fs2_snapshots() {
    let provider = Provider::new(
        "goose-persistent-assistant-two-turns",
        vec![
            command("set -eu; test ! -e state.txt; pwd > cwd.txt; printf first > state.txt; printf first > first.txt; printf turn-one-written")
                .expect_request_contains("persistent-first-input"),
            finish("persistent-first-reply").after("turn-one-written"),
            Step::text("first persistent turn finished"),
            command("set -eu; test \"$(cat state.txt)\" = first; test \"$(cat first.txt)\" = first; test \"$(pwd)\" = \"$(cat cwd.txt)\"; printf second > state.txt; printf second > second.txt; printf turn-two-updated")
                .expect_request_contains("persistent-second-input"),
            finish("persistent-second-reply").after("turn-two-updated"),
            Step::text("second persistent turn finished"),
        ],
    );
    let host = host();
    let server = host.serve(&provider);
    let first_event = event(
        ALICE,
        "persistent-two-turns-first",
        "persistent-first-input",
    );
    let first = submit(&server, &first_event, "persistent-first-reply");
    let run = first["run"].as_str().unwrap();
    let first_saved = waiting(&host, run, 1);
    let directory = workspace(&host, run);
    let directory_identity = fs::metadata(&directory).unwrap();
    let first_artifact = artifact(&host, &first_saved, 1);
    let first_inventory = snapshot_inventory(&first_artifact);
    assert_eq!(fs::read(directory.join("state.txt")).unwrap(), b"first");
    assert_eq!(
        fs::read(first_artifact.join("files/state.txt")).unwrap(),
        b"first"
    );
    let first_native = native_round(&host, &provider, run, 1);
    idle(&provider, 3);
    let replay = submit(&server, &first_event, "persistent-first-reply");
    assert_eq!(replay, first);
    idle(&provider, 3);

    let second_event = event(
        ALICE,
        "persistent-two-turns-second",
        "persistent-second-input",
    );
    let second = submit(&server, &second_event, "persistent-second-reply");
    assert_eq!(second["run"], first["run"]);
    assert_eq!(second["session"], first["session"]);
    assert_ne!(second["receipt"]["key"], first["receipt"]["key"]);
    let second_saved = waiting(&host, run, 2);
    assert_eq!(runs(&host).len(), 1);
    let updated_directory_identity = fs::metadata(&directory).unwrap();
    assert_eq!(updated_directory_identity.dev(), directory_identity.dev());
    assert_eq!(updated_directory_identity.ino(), directory_identity.ino());
    assert_eq!(fs::read(directory.join("state.txt")).unwrap(), b"second");
    assert_eq!(fs::read(directory.join("first.txt")).unwrap(), b"first");
    assert_eq!(fs::read(directory.join("second.txt")).unwrap(), b"second");
    let second_artifact = artifact(&host, &second_saved, 2);
    assert_ne!(first_artifact, second_artifact);
    assert_eq!(
        fs::read(second_artifact.join("files/state.txt")).unwrap(),
        b"second"
    );
    assert_eq!(snapshot_inventory(&first_artifact), first_inventory);
    let second_native = native_round(&host, &provider, run, 2);
    assert_eq!(second_native["session_id"], first_native["session_id"]);
    assert_eq!(
        second_native["conversation_scope"],
        first_native["conversation_scope"]
    );
    let requests = provider.requests();
    goose::tool_definition(&requests[0], "anchor_run").unwrap();
    goose::tool_definition(&requests[1], "final_result").unwrap();
    assert!(
        requests[3]["messages"]
            .to_string()
            .contains("persistent-first-input")
    );
    assert!(
        requests[3]["messages"]
            .to_string()
            .contains("turn-one-written")
    );
    assert_eq!(
        submit(&server, &second_event, "persistent-second-reply"),
        second
    );
    idle(&provider, 6);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({
            "case_source":CASE_SOURCE,"first_response":first,"second_response":second,
            "same_run":true,"stable_workspace":directory,"old_fs2_snapshot_unchanged":true,
            "same_native_session":first_native["session_id"],"idle_window_ms":250,
            "duplicate_event_model_requests":0,"stopped":stopped
        }),
    );
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn new_input_interrupts_gated_execution_suppresses_late_reply_and_retains_uncommitted_files() {
    let old_gate = Gate::new();
    let new_gate = Gate::new();
    let provider = Provider::new(
        "goose-persistent-assistant-interruption",
        vec![
            command("set -eu; test ! -e draft.txt; printf uncommitted-draft > draft.txt; printf old-draft-ready")
                .expect_request_contains("persistent-interrupted-input"),
            finish("must-not-deliver-late-old-reply").after("old-draft-ready").gated(&old_gate),
            command("set -eu; test \"$(cat draft.txt)\" = uncommitted-draft; test ! -e resumed.txt; printf inspected > resumed.txt; printf retained-draft-inspected")
                .expect_request_contains("persistent-replacement-input").gated(&new_gate),
            finish("replacement-reply-after-inspection").after("retained-draft-inspected"),
            Step::text("replacement persistent turn finished"),
        ],
    );
    let host = host();
    let server = host.serve(&provider);
    let first_event = event(
        ALICE,
        "persistent-interrupt-old",
        "persistent-interrupted-input",
    );
    let second_event = event(
        ALICE,
        "persistent-interrupt-new",
        "persistent-replacement-input",
    );
    let (old_response, replacement, interrupted) = thread::scope(|scope| {
        let old_request = scope.spawn(|| {
            let response = server.request("POST", EVENTS, Some(&first_event));
            assert_eq!(response.0, 200, "{}", response.1);
            response
        });
        entered(&provider, 2, &old_gate, &old_request);
        let saved = runs(&host);
        assert_eq!(saved.len(), 1);
        let run = saved[0]["run_id"].as_str().unwrap().to_owned();
        let old_native = host.native_fact(&run, "assistant", 1);
        assert!(old_native["completion"].is_null());
        assert_eq!(
            fs::read(workspace(&host, &run).join("draft.txt")).unwrap(),
            b"uncommitted-draft"
        );
        let replacement_request =
            scope.spawn(|| submit(&server, &second_event, "replacement-reply-after-inspection"));
        entered(&provider, 3, &new_gate, &replacement_request);
        let interrupted = host.record(&run);
        let result: RunResult =
            serde_json::from_value(interrupted["results"]["assistant"][0].clone()).unwrap();
        assert!(
            result
                .interruption
                .as_ref()
                .is_some_and(|reason| !reason.is_empty()),
            "{interrupted}"
        );
        assert_eq!(result.completion.output["interrupted"], true);
        assert_eq!(
            interrupted["results"]["reply"][0]["completion"]["output"]["suppressed"],
            true
        );
        assert_eq!(interrupted["cursor"]["node_id"], "assistant");
        assert_eq!(interrupted["cursor"]["key"]["invocation"], 2);
        let new_native = host.native_fact(&run, "assistant", 2);
        assert_eq!(new_native["session_id"], old_native["session_id"]);
        assert_eq!(
            new_native["conversation_scope"],
            old_native["conversation_scope"]
        );
        assert_eq!(
            fs::read(workspace(&host, &run).join("draft.txt")).unwrap(),
            b"uncommitted-draft"
        );
        assert!(!workspace(&host, &run).join("resumed.txt").exists());
        old_gate.open();
        idle(&provider, 3);
        assert!(host.native_fact(&run, "assistant", 1)["completion"].is_null());
        assert_eq!(
            host.record(&run)["results"]["assistant"][0],
            interrupted["results"]["assistant"][0]
        );
        new_gate.open();
        (
            old_request.join().unwrap(),
            replacement_request.join().unwrap(),
            interrupted,
        )
    });
    assert_eq!(old_response.0, 200, "{}", old_response.1);
    assert_eq!(old_response.1["text"], "");
    assert_eq!(old_response.1["superseded"], true);
    let run = replacement["run"].as_str().unwrap();
    assert_eq!(replacement["run"], interrupted["run_id"]);
    let saved = waiting(&host, run, 2);
    assert_eq!(runs(&host).len(), 1);
    assert_eq!(
        saved["results"]["assistant"][0],
        interrupted["results"]["assistant"][0]
    );
    let completed = native_round(&host, &provider, run, 2);
    assert_eq!(
        completed["session_id"],
        host.native_fact(run, "assistant", 1)["session_id"]
    );
    let committed = artifact(&host, &saved, 2);
    assert_eq!(
        fs::read(committed.join("files/draft.txt")).unwrap(),
        b"uncommitted-draft"
    );
    assert_eq!(
        fs::read(committed.join("files/resumed.txt")).unwrap(),
        b"inspected"
    );
    assert!(
        provider.requests()[2]["messages"]
            .to_string()
            .contains("old-draft-ready")
    );
    assert_eq!(
        submit(&server, &second_event, "replacement-reply-after-inspection"),
        replacement
    );
    idle(&provider, 5);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({
            "case_source":CASE_SOURCE,"superseded_response":old_response.1,
            "replacement_response":replacement,"interrupted_result":saved["results"]["assistant"][0],
            "old_execution_exited_before_new_request":true,"late_reply_suppressed":true,
            "uncommitted_workspace_retained":true,"same_run_and_native_session":true,"stopped":stopped
        }),
    );
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn concurrent_users_keep_separate_runs_workspaces_and_native_histories() {
    let alice_gate = Gate::new();
    let bob_gate = Gate::new();
    let provider = Provider::new(
        "goose-persistent-assistant-user-isolation",
        vec![
            command("set -eu; test ! -e owner.txt; printf alice > owner.txt; printf alice-first-workspace")
                .expect_request_contains("alice-private-first-input").gated(&alice_gate),
            command("set -eu; test ! -e owner.txt; printf bob > owner.txt; printf bob-first-workspace")
                .expect_request_contains("bob-private-first-input").gated(&bob_gate),
            finish("alice-private-first-reply").after("alice-first-workspace"),
            Step::text("alice persistent turn finished"),
            finish("bob-private-first-reply").after("bob-first-workspace"),
            Step::text("bob persistent turn finished"),
            command("set -eu; test \"$(cat owner.txt)\" = alice; printf continued > continued.txt; printf alice-workspace-continued")
                .expect_request_contains("alice-private-followup-input"),
            finish("alice-private-followup-reply").after("alice-workspace-continued"),
            Step::text("alice persistent followup finished"),
        ],
    );
    let host = host();
    let server = goose::serve_gated(&host, &provider);
    let alice_event = event(
        ALICE,
        "persistent-isolation-alice",
        "alice-private-first-input",
    );
    let bob_event = event(BOB, "persistent-isolation-bob", "bob-private-first-input");
    let (alice, bob) = thread::scope(|scope| {
        let alice_request =
            scope.spawn(|| submit(&server, &alice_event, "alice-private-first-reply"));
        entered(&provider, 1, &alice_gate, &alice_request);
        let bob_request = scope.spawn(|| submit(&server, &bob_event, "bob-private-first-reply"));
        entered(&provider, 2, &bob_gate, &bob_request);
        assert_eq!(runs(&host).len(), 2);
        assert_eq!(provider.requests().len(), 2);
        assert!(
            !provider.requests()[0]["messages"]
                .to_string()
                .contains("bob-private")
        );
        assert!(
            !provider.requests()[1]["messages"]
                .to_string()
                .contains("alice-private")
        );
        alice_gate.open();
        let alice = alice_request.join().unwrap();
        waiting(&host, alice["run"].as_str().unwrap(), 1);
        assert_eq!(provider.requests().len(), 4);
        bob_gate.open();
        (alice, bob_request.join().unwrap())
    });
    assert_ne!(alice["session"], bob["session"]);
    assert_ne!(alice["run"], bob["run"]);
    let alice_run = alice["run"].as_str().unwrap();
    let bob_run = bob["run"].as_str().unwrap();
    let alice_saved = waiting(&host, alice_run, 1);
    let bob_saved = waiting(&host, bob_run, 1);
    let alice_native = native_round(&host, &provider, alice_run, 1);
    let bob_native = native_round(&host, &provider, bob_run, 1);
    assert_ne!(
        alice_native["conversation_scope"],
        bob_native["conversation_scope"]
    );
    let (_, _, alice_process) = host.native_record(alice_run, "assistant", 1);
    let (_, _, bob_process) = host.native_record(bob_run, "assistant", 1);
    assert_ne!(alice_process, bob_process);
    assert_ne!(
        (
            &alice_native["conversation_scope"],
            &alice_native["session_id"]
        ),
        (&bob_native["conversation_scope"], &bob_native["session_id"])
    );
    let alice_workspace = workspace(&host, alice_run);
    let bob_workspace = workspace(&host, bob_run);
    assert_ne!(alice_workspace, bob_workspace);
    assert_eq!(
        fs::read(alice_workspace.join("owner.txt")).unwrap(),
        b"alice"
    );
    assert_eq!(fs::read(bob_workspace.join("owner.txt")).unwrap(), b"bob");
    let bob_artifact = artifact(&host, &bob_saved, 1);
    let bob_inventory = snapshot_inventory(&bob_artifact);
    let alice_history = host
        .native_conversation(alice_run, "assistant", 1)
        .to_string();
    let bob_history = host
        .native_conversation(bob_run, "assistant", 1)
        .to_string();
    assert!(alice_history.contains("alice-private-first-input"));
    assert!(!alice_history.contains("bob-private"));
    assert!(bob_history.contains("bob-private-first-input"));
    assert!(!bob_history.contains("alice-private"));

    let followup_event = event(
        ALICE,
        "persistent-isolation-alice-followup",
        "alice-private-followup-input",
    );
    let followup = submit(&server, &followup_event, "alice-private-followup-reply");
    assert_eq!(followup["run"], alice["run"]);
    assert_eq!(followup["session"], alice["session"]);
    waiting(&host, alice_run, 2);
    let followup_native = native_round(&host, &provider, alice_run, 2);
    assert_eq!(followup_native["session_id"], alice_native["session_id"]);
    assert_eq!(
        followup_native["conversation_scope"],
        alice_native["conversation_scope"]
    );
    assert!(
        provider.requests()[6]["messages"]
            .to_string()
            .contains("alice-private-first-input")
    );
    assert!(
        !provider.requests()[6]["messages"]
            .to_string()
            .contains("bob-private")
    );
    assert!(!bob_workspace.join("continued.txt").exists());
    assert_eq!(snapshot_inventory(&bob_artifact), bob_inventory);
    assert_eq!(waiting(&host, bob_run, 1)["results"], bob_saved["results"]);
    assert_eq!(submit(&server, &bob_event, "bob-private-first-reply"), bob);
    idle(&provider, 9);
    let alice_stopped = stop(&server, &provider, alice_run);
    let bob_stopped = stop(&server, &provider, bob_run);
    evidence(
        &host,
        &provider,
        alice_run,
        json!({
            "case_source":CASE_SOURCE,"alice_first":alice,"bob_first":bob,"alice_followup":followup,
            "initial_alice_result":alice_saved["results"]["assistant"][0],
        "concurrent_users":true,"isolated_native_sessions":[
            {"scope":alice_native["conversation_scope"],"session_id":alice_native["session_id"],"process_directory":alice_process},
            {"scope":bob_native["conversation_scope"],"session_id":bob_native["session_id"],"process_directory":bob_process}
        ],
            "isolated_workspaces":[alice_workspace,bob_workspace],"bob_fs2_unchanged":true,
            "stopped":[alice_stopped,bob_stopped]
        }),
    );
}

fn action(server: &HttpHost, run: &str, operation: &str) -> Value {
    let (status, response) = server.request("POST", &format!("/runs/{run}/{operation}"), None);
    assert_eq!(status, 202, "{response}");
    assert_eq!(response["run"], run);
    assert_eq!(response["asked"], operation);
    response
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn idle_wait_pause_resume_dispatches_no_model_until_new_input_and_stop_exits() {
    let provider = Provider::new("goose-persistent-assistant-idle-control", vec![
        command("set -eu; printf pause-seed > control.txt; printf pause-seed-written")
            .expect_request_contains("persistent-idle-control-first-input"),
        finish("persistent-idle-control-first-reply").after("pause-seed-written"),
        Step::text("idle control first turn finished"),
        command("set -eu; test \"$(cat control.txt)\" = pause-seed; printf resumed > control.txt; printf resumed-input-inspected")
            .expect_request_contains("persistent-idle-control-second-input"),
        finish("persistent-idle-control-second-reply").after("resumed-input-inspected"),
        Step::text("idle control second turn finished"),
    ]);
    let host = host();
    let server = host.serve(&provider);
    let first_event = event(
        ALICE,
        "persistent-idle-control-first",
        "persistent-idle-control-first-input",
    );
    let first = submit(&server, &first_event, "persistent-idle-control-first-reply");
    let run = first["run"].as_str().unwrap();
    let before = waiting(&host, run, 1);
    let native = native_round(&host, &provider, run, 1);
    idle(&provider, 3);
    action(&server, run, "pause");
    let paused = server.wait_status(run, "paused");
    let paused_record = host.record(run);
    assert_eq!(paused_record["cursor"], before["cursor"]);
    assert_eq!(paused_record["results"], before["results"]);
    idle(&provider, 3);
    action(&server, run, "resume");
    let resumed = waiting(&host, run, 1);
    assert_eq!(resumed["cursor"], before["cursor"]);
    assert_eq!(resumed["results"], before["results"]);
    assert_eq!(host.native_fact(run, "assistant", 1), native);
    idle(&provider, 3);
    let second_event = event(
        ALICE,
        "persistent-idle-control-second",
        "persistent-idle-control-second-input",
    );
    let second = submit(
        &server,
        &second_event,
        "persistent-idle-control-second-reply",
    );
    assert_eq!(second["run"], first["run"]);
    assert_eq!(second["session"], first["session"]);
    waiting(&host, run, 2);
    assert_eq!(
        native_round(&host, &provider, run, 2)["session_id"],
        native["session_id"]
    );
    assert_eq!(
        fs::read(workspace(&host, run).join("control.txt")).unwrap(),
        b"resumed"
    );
    idle(&provider, 6);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({
            "case_source":CASE_SOURCE,"responses":[first,second],"paused":paused,
            "cursor_preserved":true,"model_requests_during_idle_pause_resume":0,"stopped":stopped
        }),
    );
}

/// Automatic recovery is on by default; this covers the explicit path an
/// operator gets with `ANCHOR_ASSISTANT_AUTO_RESUME=0`.
#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn host_restart_requires_explicit_resume_when_automatic_recovery_is_disabled() {
    let provider = Provider::new(
        "goose-persistent-assistant-host-restart",
        vec![
            command("set -eu; printf restart-seed > restart.txt; printf restart-seed-written")
                .expect_request_contains("persistent-restart-first-input"),
            finish("persistent-restart-first-reply").after("restart-seed-written"),
            Step::text("restart first turn finished"),
        ],
    );
    let host = host_without_auto_resume();
    let mut server = host.serve(&provider);
    let first_event = event(
        ALICE,
        "persistent-restart-first",
        "persistent-restart-first-input",
    );
    let first = submit(&server, &first_event, "persistent-restart-first-reply");
    let run = first["run"].as_str().unwrap();
    let before = waiting(&host, run, 1);
    let native = native_round(&host, &provider, run, 1);
    let history = host.native_conversation(run, "assistant", 1);
    let directory = workspace(&host, run);
    let directory_identity = fs::metadata(&directory).unwrap().ino();
    let files = goose::file_inventory(&directory);
    let first_artifact = artifact(&host, &before, 1);
    let first_snapshot = snapshot_inventory(&first_artifact);
    idle(&provider, 3);
    server.kill();
    drop(server);
    fs::copy(
        provider.root.join("host-http.log"),
        provider.root.join("host-before-restart.log"),
    )
    .unwrap();
    let restarted = host.serve(&provider);
    let (status, inactive) = restarted.request("GET", &format!("/runs/{run}"), None);
    assert_eq!(status, 200, "{inactive}");
    assert_eq!(inactive["active"], false);
    assert_eq!(host.record(run)["cursor"], before["cursor"]);
    assert_eq!(host.record(run)["results"], before["results"]);
    assert_eq!(fs::metadata(&directory).unwrap().ino(), directory_identity);
    assert_eq!(goose::file_inventory(&directory), files);
    assert_eq!(host.native_conversation(run, "assistant", 1), history);
    idle(&provider, 3);
    assert_eq!(
        submit(&restarted, &first_event, "persistent-restart-first-reply"),
        first
    );
    idle(&provider, 3);
    action(&restarted, run, "resume");
    let resumed = waiting(&host, run, 1);
    assert_eq!(resumed["cursor"], before["cursor"]);
    assert_eq!(resumed["results"], before["results"]);
    idle(&provider, 3);
    provider.append(vec![
        command("set -eu; test \"$(cat restart.txt)\" = restart-seed; printf inspected > after-restart.txt; printf restart-workspace-inspected")
            .expect_request_contains("persistent-restart-second-input"),
        finish("persistent-restart-second-reply").after("restart-workspace-inspected"),
        Step::text("restart second turn finished"),
    ]);
    let second_event = event(
        ALICE,
        "persistent-restart-second",
        "persistent-restart-second-input",
    );
    let second = submit(&restarted, &second_event, "persistent-restart-second-reply");
    assert_eq!(second["run"], first["run"]);
    assert_eq!(second["session"], first["session"]);
    waiting(&host, run, 2);
    assert_eq!(runs(&host).len(), 1);
    let continued = native_round(&host, &provider, run, 2);
    assert_eq!(continued["session_id"], native["session_id"]);
    assert_eq!(
        continued["conversation_scope"],
        native["conversation_scope"]
    );
    assert!(
        provider.requests()[3]["messages"]
            .to_string()
            .contains("persistent-restart-first-input")
    );
    assert!(
        provider.requests()[3]["messages"]
            .to_string()
            .contains("restart-seed-written")
    );
    assert_eq!(snapshot_inventory(&first_artifact), first_snapshot);
    assert_eq!(
        fs::read(directory.join("restart.txt")).unwrap(),
        b"restart-seed"
    );
    assert_eq!(
        fs::read(directory.join("after-restart.txt")).unwrap(),
        b"inspected"
    );
    idle(&provider, 6);
    let stopped = stop(&restarted, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({
            "case_source":CASE_SOURCE,"responses":[first,second],"inactive_after_restart":inactive,
            "automatic_recovery_disabled":true,"explicit_resume_required":true,
            "workspace_and_history_retained":true,
            "same_run_and_native_session":true,"stopped":stopped
        }),
    );
}

/// A Host restart resumes the bound assistant instance on its own: the same
/// Goose conversation continues in a new Run that inherited the old workspace
/// scene, while the old Run and its committed Artifact stay byte-identical.
#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn host_restart_automatically_resumes_the_instance_with_its_workspace_and_session() {
    let provider = Provider::new(
        "goose-persistent-assistant-auto-resume",
        vec![
            command(
                "set -eu; printf auto-resume-seed > inherited.txt; printf auto-resume-seed-written",
            )
            .expect_request_contains("persistent-auto-resume-first-input"),
            finish("persistent-auto-resume-first-reply").after("auto-resume-seed-written"),
            Step::text("auto resume first turn finished"),
        ],
    );
    let host = host();
    let mut server = host.serve(&provider);
    let first_event = event(
        ALICE,
        "persistent-auto-resume-first",
        "persistent-auto-resume-first-input",
    );
    let first = submit(&server, &first_event, "persistent-auto-resume-first-reply");
    let run = first["run"].as_str().unwrap().to_owned();
    let session = first["session"].as_str().unwrap().to_owned();
    let before = waiting(&host, &run, 1);
    let native = native_round(&host, &provider, &run, 1);
    let directory = workspace(&host, &run);
    assert_eq!(
        fs::read(directory.join("inherited.txt")).unwrap(),
        b"auto-resume-seed"
    );
    let first_artifact = artifact(&host, &before, 1);
    let first_snapshot = snapshot_inventory(&first_artifact);
    idle(&provider, 3);

    server.kill();
    drop(server);
    fs::copy(
        provider.root.join("host-http.log"),
        provider.root.join("host-before-auto-resume.log"),
    )
    .unwrap();
    let restarted = host.serve(&provider);

    // The restart handed the instance over without any operator action, and the
    // stopped old Run is history rather than a live instance.
    let (status, binding) = restarted.request(
        "GET",
        &format!("/channel-sessions/{session}/assistant"),
        None,
    );
    assert_eq!(status, 200, "{binding}");
    let resumed = binding["assistant"]["run_id"]
        .as_str()
        .expect("assistant binding after restart")
        .to_owned();
    assert_ne!(resumed, run);
    assert!(
        resumed.starts_with("assistant-resume-"),
        "unexpected recovery Run: {resumed}"
    );
    assert_eq!(binding["needs_recovery"], json!(false), "{binding}");
    assert_eq!(host.record(&run)["status"], "stopped");
    wait_until("recovered assistant waiting for input", || {
        let saved = host.record(&resumed);
        saved["status"] == "running"
            && saved["cursor"]["node_id"] == "wait_input"
            && saved["cursor"]["key"]["invocation"] == 1
            && saved["results"].get("reply").is_none()
    });
    idle(&provider, 3);

    let sessions =
        SessionStore::open(host.base.root.path().join("state/platform/sessions.sqlite")).unwrap();
    let events = sessions.events("local", &session, 0).unwrap();
    let automatic = events
        .iter()
        .find(|event| event.kind == "channel.assistant_auto_resume")
        .expect("automatic recovery was not recorded");
    assert_eq!(automatic.data["from"], json!(run));
    assert_eq!(automatic.data["to"], json!(resumed));
    drop(sessions);

    provider.append(vec![
        command("set -eu; test \"$(cat inherited.txt)\" = auto-resume-seed; printf inherited > continued.txt; printf auto-resume-workspace-inherited")
            .expect_request_contains("persistent-auto-resume-second-input"),
        finish("persistent-auto-resume-second-reply").after("auto-resume-workspace-inherited"),
        Step::text("auto resume second turn finished"),
    ]);
    idle(&provider, 3);
    let second_event = event(
        ALICE,
        "persistent-auto-resume-second",
        "persistent-auto-resume-second-input",
    );
    let second = submit_any(
        &restarted,
        &second_event,
        "persistent-auto-resume-second-reply",
    );
    assert_eq!(second["run"], json!(resumed));
    assert_eq!(second["session"], json!(session));
    let resumed_saved = waiting(&host, &resumed, 1);
    // The recovery replaced the old Run instead of adding a second instance.
    assert_eq!(runs(&host).len(), 2);
    // Same Goose conversation, new Run: the recovered instance continues the
    // native session instead of starting one.
    let resumed_native = native_round(&host, &provider, &resumed, 1);
    assert_eq!(resumed_native["session_id"], native["session_id"]);
    assert_eq!(
        resumed_native["conversation_scope"],
        native["conversation_scope"]
    );
    // The new workspace inherited the old scene, including the uncommitted
    // file, and the old Run's Artifact is unchanged.
    let resumed_directory = workspace(&host, &resumed);
    assert_ne!(resumed_directory, directory);
    assert_eq!(
        fs::read(resumed_directory.join("inherited.txt")).unwrap(),
        b"auto-resume-seed"
    );
    assert_eq!(
        fs::read(resumed_directory.join("continued.txt")).unwrap(),
        b"inherited"
    );
    assert_eq!(
        fs::read(directory.join("inherited.txt")).unwrap(),
        b"auto-resume-seed"
    );
    assert_eq!(snapshot_inventory(&first_artifact), first_snapshot);
    assert_eq!(
        resumed_saved["results"]["assistant"][0]["completion"]["submission"],
        "persistent-auto-resume-second-reply"
    );
    assert!(
        provider.requests()[3]["messages"]
            .to_string()
            .contains("persistent-auto-resume-first-input")
    );
    idle(&provider, 6);
    let stopped = stop(&restarted, &provider, &resumed);
    evidence(
        &host,
        &provider,
        &resumed,
        json!({
            "case_source":CASE_SOURCE,"first_response":first,"second_response":second,
            "recovery_binding":binding,"automatic_event":automatic.data,
            "resumed_run":resumed,"previous_run":run,"previous_status":host.record(&run)["status"],
            "same_native_session":native["session_id"],
            "new_workspace":resumed_directory,"old_workspace":directory,
            "inherited_workspace":true,"old_fs2_snapshot_unchanged":true,"stopped":stopped
        }),
    );
}

fn submit_any(server: &HttpHost, body: &Value, expected: &str) -> Value {
    let (status, response) = server.request("POST", EVENTS, Some(body));
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["text"], expected, "{response}");
    response
}

fn inbound_identity(body: &Value) -> String {
    let mut digest = Sha256::new();
    digest.update(b"anchor-wecom-inbound-v1\0");
    digest.update(body["event"]["source"].as_str().unwrap().as_bytes());
    digest.update([0]);
    digest.update(body["event"]["event_id"].as_str().unwrap().as_bytes());
    format!("wecom-{:x}", digest.finalize())
}

fn confirm_reply(
    host: &Host,
    server: &HttpHost,
    body: &Value,
    response: &Value,
    invocation: usize,
    items: &[Value],
) -> Value {
    let run = response["run"].as_str().unwrap();
    let session = response["session"].as_str().unwrap();
    let inbound_id = inbound_identity(body);
    let sessions =
        SessionStore::open(host.base.root.path().join("state/platform/sessions.sqlite")).unwrap();
    let input = sessions
        .get_channel_inbound("local", session, &inbound_id)
        .unwrap();
    assert_eq!(input.turn.status, TurnStatus::Completed);
    assert_eq!(input.relation.run_id.as_deref(), Some(run));
    let saved = host.record(run);
    let output = &saved["results"]["reply"][invocation - 1]["completion"]["output"];
    assert_eq!(
        saved["results"]["wait_input"][invocation - 1]["completion"]["output"]["turn"],
        input.turn.id
    );
    assert_eq!(output["reply_for"], input.turn.id);
    assert_eq!(
        output["source_commit"],
        saved["results"]["assistant"][invocation - 1]["commit"]
    );
    assert_eq!(output["items"], json!(items));
    if items.is_empty() {
        assert!(response.get("msg_item").is_none(), "{response}");
    } else {
        assert_eq!(response["msg_item"], json!(items));
        let path = host
            .base
            .root
            .path()
            .join("state/channel-replies")
            .join(format!("turn-{}.json", input.turn.id));
        assert_eq!(fixture::read_json(path), json!(items));
    }
    let key = format!("channel-wecom-reply:{session}:{inbound_id}");
    let content_sha256 = format!(
        "{:x}",
        Sha256::digest(response["text"].as_str().unwrap().as_bytes())
    );
    assert_eq!(response["receipt"]["key"], key);
    assert_eq!(response["receipt"]["content_sha256"], content_sha256);
    let deliveries = sessions
        .list_unfinished_channel_deliveries("local", session)
        .unwrap();
    let delivery = deliveries
        .iter()
        .find(|delivery| delivery.key == key)
        .unwrap();
    assert_eq!(delivery.turn_id, input.turn.id);
    assert_eq!(delivery.status, ChannelDeliveryStatus::Sending);
    assert_eq!(delivery.content_sha256, content_sha256);
    drop(sessions);
    let settlement = json!({"event":body["event"],"settlement":{
        "key":key,"content_sha256":content_sha256,"status":"confirmed"
    }});
    let (status, settled) = server.request("POST", EVENTS, Some(&settlement));
    assert_eq!(status, 200, "{settled}");
    assert_eq!(settled["delivery"]["key"], key);
    assert_eq!(settled["delivery"]["turn_id"], input.turn.id);
    assert_eq!(settled["delivery"]["session_id"], session);
    assert_eq!(settled["delivery"]["content_sha256"], content_sha256);
    assert_eq!(settled["delivery"]["status"], "confirmed");
    settled["delivery"].clone()
}

fn install_image_plugin(host: &Host) {
    let bundle = host.base.root.path().join("bundle");
    let plugin = bundle.join("plugins/wecom");
    fs::create_dir_all(plugin.join("skills/channel")).unwrap();
    fs::write(plugin.join("plugin.json"), json!({
        "name":"WeCom reply-image fixture","description":"Resource-only authorized channel image tools; no real delivery.","skills":"./skills"
    }).to_string()).unwrap();
    fs::write(plugin.join("skills/channel/SKILL.md"), "---\nname: fixture-reply-image\ndescription: Prepare a turn-scoped reply image.\n---\nUse wecom_attach_image to prepare a validated workspace PNG. Do not send external messages.\n").unwrap();
    let binding = FilePluginCatalog::new(&bundle)
        .resolve(&["wecom".into()])
        .unwrap()
        .remove(0);
    assert!(binding.mcp_servers.is_empty());
    fs::write(bundle.join("manifest.json"), json!({"format":1,"graph":"graph.json","plugins":[{
        "id":binding.id,"digest":binding.digest,"resources":binding.resources,"mcp_servers":binding.mcp_servers
    }]}).to_string()).unwrap();
}

fn reply_picture(picture: &Value) -> Value {
    let encoded = picture["data_base64"].as_str().unwrap();
    let bytes = STANDARD.decode(encoded).unwrap();
    json!({"msgtype":"image","image":{"base64":encoded,"md5":format!("{:x}", md5::Md5::digest(bytes))}})
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn reply_images_are_bound_to_exact_turn_receipts_and_not_reused_by_text_only_turns() {
    let (red, _) = picture("outgoing.png", [255, 0, 0]);
    let (blue, _) = picture("outgoing.png", [0, 0, 255]);
    let red_item = reply_picture(&red);
    let blue_item = reply_picture(&blue);
    assert_ne!(red_item, blue_item);
    let red_command = format!(
        "set -eu; printf %s '{}' | base64 -d > outgoing.png; printf red-image-created",
        red["data_base64"].as_str().unwrap()
    );
    let blue_command = format!(
        "set -eu; test -f outgoing.png; printf %s '{}' | base64 -d > outgoing.png; printf blue-image-created",
        blue["data_base64"].as_str().unwrap()
    );
    let provider = Provider::new(
        "goose-persistent-assistant-reply-images",
        vec![
            command(&red_command).expect_request_contains("persistent-red-reply-input"),
            Step::tool(
                "wecom_attach_image",
                json!({"path":"/workspace/outgoing.png"}),
            )
            .after("red-image-created"),
            finish("persistent-red-reply").after("attached"),
            Step::text("red reply turn finished"),
            command("set -eu; test -f outgoing.png; printf red-not-reattached")
                .expect_request_contains("persistent-after-red-input"),
            finish("persistent-after-red-reply").after("red-not-reattached"),
            Step::text("text-only turn after red finished"),
            command(&blue_command).expect_request_contains("persistent-blue-reply-input"),
            Step::tool(
                "wecom_attach_image",
                json!({"path":"/workspace/outgoing.png"}),
            )
            .after("blue-image-created"),
            finish("persistent-blue-reply").after("attached"),
            Step::text("blue reply turn finished"),
            command("set -eu; test -f outgoing.png; printf blue-not-reattached")
                .expect_request_contains("persistent-after-blue-input"),
            finish("persistent-after-blue-reply").after("blue-not-reattached"),
            Step::text("text-only turn after blue finished"),
        ],
    );
    let mut definition = graph();
    definition["nodes"][1]["plugins"] = json!(["wecom"]);
    let host = host_for(&definition).with_allowed_commands("sh,cat,git,true,base64");
    install_image_plugin(&host);
    let server = host.serve(&provider);
    let specifications = [
        (
            "persistent-red-reply-event",
            "persistent-red-reply-input",
            "persistent-red-reply",
            vec![red_item.clone()],
        ),
        (
            "persistent-after-red-event",
            "persistent-after-red-input",
            "persistent-after-red-reply",
            Vec::new(),
        ),
        (
            "persistent-blue-reply-event",
            "persistent-blue-reply-input",
            "persistent-blue-reply",
            vec![blue_item.clone()],
        ),
        (
            "persistent-after-blue-event",
            "persistent-after-blue-input",
            "persistent-after-blue-reply",
            Vec::new(),
        ),
    ];
    let mut responses: Vec<Value> = Vec::new();
    let mut events = Vec::new();
    let mut settlements = Vec::new();
    let mut session_id = Value::Null;
    for (index, (identity, text, summary, items)) in specifications.iter().enumerate() {
        let body = event(ALICE, identity, text);
        let response = submit(&server, &body, summary);
        let run = response["run"].as_str().unwrap();
        waiting(&host, run, index + 1);
        let native = native_round(&host, &provider, run, (index + 1) as u64);
        if index == 0 {
            session_id = native["session_id"].clone();
        } else {
            assert_eq!(response["run"], responses[0]["run"]);
            assert_eq!(response["session"], responses[0]["session"]);
            assert_eq!(native["session_id"], session_id);
            let mismatched = json!({"event":body["event"],"settlement":{
                "key":responses[0]["receipt"]["key"],"content_sha256":responses[0]["receipt"]["content_sha256"],"status":"confirmed"
            }});
            let (status, rejected) = server.request("POST", EVENTS, Some(&mismatched));
            assert_eq!(status, 400, "{rejected}");
        }
        settlements.push(confirm_reply(
            &host,
            &server,
            &body,
            &response,
            index + 1,
            items,
        ));
        events.push(body);
        responses.push(response);
    }
    let run = responses[0]["run"].as_str().unwrap();
    let saved = waiting(&host, run, 4);
    assert_eq!(runs(&host).len(), 1);
    assert_eq!(
        fs::read(artifact(&host, &saved, 1).join("files/outgoing.png")).unwrap(),
        STANDARD
            .decode(red["data_base64"].as_str().unwrap())
            .unwrap()
    );
    assert_eq!(
        fs::read(artifact(&host, &saved, 3).join("files/outgoing.png")).unwrap(),
        STANDARD
            .decode(blue["data_base64"].as_str().unwrap())
            .unwrap()
    );
    goose::tool_definition(&provider.requests()[1], "wecom_attach_image").unwrap();
    for (body, response) in events.iter().zip(&responses) {
        assert_eq!(
            submit(&server, body, response["text"].as_str().unwrap()),
            *response
        );
    }
    idle(&provider, 14);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({
            "case_source":CASE_SOURCE,"responses":responses,"settlements":settlements,
            "distinct_images":[red_item,blue_item],"exact_turn_receipts":true,"text_only_turns_have_no_images":true,
            "actual_wecom_attach_image_tool":true,"stopped":stopped
        }),
    );
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn interrupted_file_is_readonly_in_pending_scope_until_replacement_reply_is_confirmed() {
    let old_gate = Gate::new();
    let new_gate = Gate::new();
    let provider = Provider::new("goose-persistent-assistant-pending-file", vec![
        command("set -eu; test \"$(cat /in/channel/handoff.txt)\" = pending-file-contents; printf old-started > draft.txt; printf old-file-checked")
            .expect_request_contains("persistent-old-file-input"),
        finish("must-not-deliver-old-file-reply").after("old-file-checked").gated(&old_gate),
    ]);
    let host = host();
    let server = host.serve(&provider);
    let mut old_event = event(
        ALICE,
        "persistent-old-file-event",
        "persistent-old-file-input",
    );
    old_event["event"]["message_type"] = json!("mixed");
    old_event["event"]["attachments"] = json!([{
        "name":"handoff.txt","media_type":"text/plain","data_base64":STANDARD.encode(b"pending-file-contents")
    }]);
    let replacement_event = event(
        ALICE,
        "persistent-pending-file-replacement",
        "persistent-file-replacement-input",
    );
    let (old_response, replacement, pending_output, pending_path) = thread::scope(|scope| {
        let old_request = scope.spawn(|| {
            let response = server.request("POST", EVENTS, Some(&old_event));
            assert_eq!(response.0, 200, "{}", response.1);
            response.1
        });
        entered(&provider, 2, &old_gate, &old_request);
        let saved = runs(&host);
        assert_eq!(saved.len(), 1);
        let run = saved[0]["run_id"].as_str().unwrap();
        let old_turn = saved[0]["results"]["wait_input"][0]["completion"]["output"]["turn"]
            .as_str()
            .unwrap();
        let pending_path = format!("/in/channel-pending/{old_turn}/handoff.txt");
        let recovery_command = format!(
            "set -eu; test ! -e /in/channel/handoff.txt; test \"$(cat '{pending_path}')\" = pending-file-contents; if printf forbidden > '{pending_path}'; then exit 94; fi; cat '{pending_path}' > recovered.txt; printf pending-file-recovered"
        );
        let after_confirmation = format!(
            "set -eu; test ! -e '{pending_path}'; test ! -e /in/channel/handoff.txt; test \"$(cat recovered.txt)\" = pending-file-contents; printf confirmed-pending-scope-removed"
        );
        provider.append(vec![
            command(&recovery_command)
                .expect_request_contains("persistent-file-replacement-input")
                .gated(&new_gate),
            finish("persistent-file-replacement-reply").after("pending-file-recovered"),
            Step::text("pending file replacement finished"),
            command(&after_confirmation)
                .expect_request_contains("persistent-after-file-confirmation-input"),
            finish("persistent-after-file-confirmation-reply")
                .after("confirmed-pending-scope-removed"),
            Step::text("confirmed pending file scope removed"),
        ]);
        let replacement_request = scope.spawn(|| {
            submit(
                &server,
                &replacement_event,
                "persistent-file-replacement-reply",
            )
        });
        entered(&provider, 3, &new_gate, &replacement_request);
        let interrupted = host.record(run);
        assert!(interrupted["results"]["assistant"][0]["interruption"].is_string());
        let output = interrupted["results"]["wait_input"][1]["completion"]["output"].clone();
        assert!(
            output["attachments"]["files"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let pending = output["interrupted_messages"].as_array().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["message"], "persistent-old-file-input");
        assert_eq!(pending[0]["turn"], old_turn);
        let files = pending[0]["attachments"]["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["path"], pending_path);
        assert_eq!(files[0]["name"], "handoff.txt");
        assert_eq!(
            files[0]["sha256"],
            format!("{:x}", Sha256::digest(b"pending-file-contents"))
        );
        assert_eq!(files[0]["size"], b"pending-file-contents".len());
        assert_eq!(files[0]["media_type"], "text/plain");
        old_gate.open();
        idle(&provider, 3);
        new_gate.open();
        (
            old_request.join().unwrap(),
            replacement_request.join().unwrap(),
            output,
            pending_path,
        )
    });
    assert_eq!(old_response["text"], "");
    assert_eq!(old_response["superseded"], true);
    let run = replacement["run"].as_str().unwrap();
    waiting(&host, run, 2);
    assert_eq!(
        fs::read(workspace(&host, run).join("recovered.txt")).unwrap(),
        b"pending-file-contents"
    );
    let native = native_round(&host, &provider, run, 2);
    let confirmed = confirm_reply(&host, &server, &replacement_event, &replacement, 2, &[]);
    let next_event = event(
        ALICE,
        "persistent-after-file-confirmation-event",
        "persistent-after-file-confirmation-input",
    );
    let next = submit(
        &server,
        &next_event,
        "persistent-after-file-confirmation-reply",
    );
    assert_eq!(next["run"], replacement["run"]);
    assert_eq!(next["session"], replacement["session"]);
    let saved = waiting(&host, run, 3);
    assert!(
        saved["results"]["wait_input"][2]["completion"]["output"]["interrupted_messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        native_round(&host, &provider, run, 3)["session_id"],
        native["session_id"]
    );
    let next_confirmed = confirm_reply(&host, &server, &next_event, &next, 3, &[]);
    idle(&provider, 8);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({
            "case_source":CASE_SOURCE,"old_response":old_response,"pending_input":pending_output,"pending_path":pending_path,
            "responses":[replacement,next],"settlements":[confirmed,next_confirmed],
            "readonly_pending_file_read_by_actual_tool":true,"scope_removed_after_confirmation":true,"stopped":stopped
        }),
    );
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn retired_instance_rejects_resume_and_new_instance_seeds_workspace_and_native_session_once() {
    let provider = Provider::new("goose-persistent-assistant-retire-handoff", vec![
        command("set -eu; printf committed-once > retirement-seed.txt; printf retirement-seed-written")
            .expect_request_contains("persistent-retirement-first-input"),
        finish("persistent-retirement-first-reply").after("retirement-seed-written"),
        Step::text("retirement first turn finished"),
        command("set -eu; test \"$(cat retirement-seed.txt)\" = committed-once; printf updated > retirement-seed.txt; printf handoff-inspected > new-instance.txt; printf retired-workspace-seed-inspected")
            .expect_request_contains("persistent-retirement-new-instance-input"),
        finish("persistent-retirement-new-instance-reply").after("retired-workspace-seed-inspected"),
        Step::text("new assistant instance seeded"),
        command("set -eu; test \"$(cat retirement-seed.txt)\" = updated; test \"$(cat new-instance.txt)\" = handoff-inspected; printf retained > followup.txt; printf seed-not-reapplied")
            .expect_request_contains("persistent-retirement-followup-input"),
        finish("persistent-retirement-followup-reply").after("seed-not-reapplied"),
        Step::text("new assistant instance continued"),
    ]);
    let host = host();
    let server = host.serve(&provider);
    let first_event = event(
        ALICE,
        "persistent-retirement-first-event",
        "persistent-retirement-first-input",
    );
    let first = submit(&server, &first_event, "persistent-retirement-first-reply");
    let old_run = first["run"].as_str().unwrap();
    let session = first["session"].as_str().unwrap();
    let before = waiting(&host, old_run, 1);
    let old_native = native_round(&host, &provider, old_run, 1);
    let old_artifact = artifact(&host, &before, 1);
    let old_snapshot = snapshot_inventory(&old_artifact);
    let assistant_path = format!("/channel-sessions/{session}/assistant");
    let retire_path = format!("{assistant_path}/retire");
    let body = json!({"run_id":old_run});
    let (status, binding) = server.request("GET", &assistant_path, None);
    assert_eq!(status, 200, "{binding}");
    assert_eq!(binding["assistant"]["run_id"], old_run);
    let (status, active_rejection) = server.request("POST", &retire_path, Some(&body));
    assert_eq!(status, 409, "{active_rejection}");
    let old_stopped = stop(&server, &provider, old_run);
    let (status, delivery_rejection) = server.request("POST", &retire_path, Some(&body));
    assert_eq!(status, 409, "{delivery_rejection}");
    let confirmed = confirm_reply(&host, &server, &first_event, &first, 1, &[]);
    let (status, retired) = server.request("POST", &retire_path, Some(&body));
    assert_eq!(status, 200, "{retired}");
    assert_eq!(retired["retired"], old_run);
    assert_eq!(retired["session"], session);
    let (status, absent) = server.request("GET", &assistant_path, None);
    assert_eq!(status, 200, "{absent}");
    assert!(absent["assistant"].is_null(), "{absent}");
    let (status, resume_rejection) =
        server.request("POST", &format!("/runs/{old_run}/resume"), None);
    assert_eq!(status, 409, "{resume_rejection}");
    assert_eq!(host.native_fact(old_run, "assistant", 1), old_native);
    assert_eq!(snapshot_inventory(&old_artifact), old_snapshot);
    idle(&provider, 3);

    let next_event = event(
        ALICE,
        "persistent-retirement-new-instance-event",
        "persistent-retirement-new-instance-input",
    );
    let next = submit(
        &server,
        &next_event,
        "persistent-retirement-new-instance-reply",
    );
    let run = next["run"].as_str().unwrap();
    assert_ne!(next["run"], first["run"]);
    assert_eq!(next["session"], first["session"]);
    waiting(&host, run, 1);
    let new_native = native_round(&host, &provider, run, 1);
    assert_eq!(new_native["session_id"], old_native["session_id"]);
    assert_eq!(
        new_native["conversation_scope"],
        old_native["conversation_scope"]
    );
    assert_ne!(workspace(&host, run), workspace(&host, old_run));
    let metadata = fixture::read_json(
        host.base
            .root
            .path()
            .join("state/run-metadata")
            .join(format!("{run}.json")),
    );
    assert_eq!(metadata["conversation"]["previous_run"], old_run);
    assert!(
        provider.requests()[3]["messages"]
            .to_string()
            .contains("persistent-retirement-first-input")
    );
    let next_confirmed = confirm_reply(&host, &server, &next_event, &next, 1, &[]);
    let followup_event = event(
        ALICE,
        "persistent-retirement-followup-event",
        "persistent-retirement-followup-input",
    );
    let followup = submit(
        &server,
        &followup_event,
        "persistent-retirement-followup-reply",
    );
    assert_eq!(followup["run"], next["run"]);
    assert_eq!(followup["session"], next["session"]);
    waiting(&host, run, 2);
    assert_eq!(
        native_round(&host, &provider, run, 2)["session_id"],
        old_native["session_id"]
    );
    let followup_confirmed = confirm_reply(&host, &server, &followup_event, &followup, 2, &[]);
    assert_eq!(runs(&host).len(), 2);
    assert_eq!(snapshot_inventory(&old_artifact), old_snapshot);
    assert_eq!(host.record(old_run)["results"], before["results"]);
    let (status, new_binding) = server.request("GET", &assistant_path, None);
    assert_eq!(status, 200, "{new_binding}");
    assert_eq!(new_binding["assistant"]["run_id"], run);
    idle(&provider, 9);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({
            "case_source":CASE_SOURCE,"responses":[first,next,followup],
            "settlements":[confirmed,next_confirmed,followup_confirmed],"retired":retired,
            "active_retire_rejected":active_rejection,"unconfirmed_delivery_retire_rejected":delivery_rejection,
            "old_resume_rejected":resume_rejection,"old_run":host.record(old_run),"old_stopped":old_stopped,
            "new_instance_same_session_and_native_history":true,"workspace_seed_applied_once":true,"stopped":stopped
        }),
    );
}

fn picture(name: &str, color: [u8; 3]) -> (Value, String) {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 1, Rgb(color)));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png).unwrap();
    let encoded = STANDARD.encode(bytes.into_inner());
    (
        json!({"name":name,"media_type":"image/png","data_base64":encoded}),
        format!("data:image/png;base64,{encoded}"),
    )
}

fn prompt_images(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "image_url")
        .map(|block| block["image_url"]["url"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn attachments_are_readonly_and_turn_scoped_while_native_history_keeps_distinct_images() {
    let provider = Provider::with_model(
        "goose-persistent-assistant-attachment-turns",
        vec![
            command("set -eu; test \"$(cat /in/channel/notes.txt)\" = first-turn-attachment; test -f /in/channel/first.png; test ! -e /in/channel/second.png; if printf forbidden > /in/channel/notes.txt; then exit 91; fi; if printf forbidden > /in/channel/first.png; then exit 92; fi; cat /in/channel/notes.txt > saved-note.txt; printf first-image-checked")
                .expect_request_contains("persistent-first-image-input"),
            finish("persistent-first-image-reply").after("first-image-checked"),
            Step::text("first image turn finished"),
            command("set -eu; test ! -e /in/channel/notes.txt; test ! -e /in/channel/first.png; test ! -e /in/channel/second.png; test \"$(cat saved-note.txt)\" = first-turn-attachment; printf text-turn-no-attachments")
                .expect_request_contains("persistent-text-only-input"),
            finish("persistent-text-only-reply").after("text-turn-no-attachments"),
            Step::text("attachment-free turn finished"),
            command("set -eu; test \"$(cat /in/channel/notes.txt)\" = second-turn-attachment; test ! -e /in/channel/first.png; test -f /in/channel/second.png; test \"$(cat saved-note.txt)\" = first-turn-attachment; if printf forbidden > /in/channel/notes.txt; then exit 93; fi; if printf forbidden > /in/channel/second.png; then exit 94; fi; cat /in/channel/notes.txt > second-note.txt; printf second-image-checked")
                .expect_request_contains("persistent-second-image-input"),
            finish("persistent-second-image-reply").after("second-image-checked"),
            Step::text("second image turn finished"),
        ],
        "gpt-4o",
    );
    let host = host();
    let server = host.serve(&provider);
    let (first_picture, first_url) = picture("first.png", [255, 0, 0]);
    let (second_picture, second_url) = picture("second.png", [0, 0, 255]);
    assert_ne!(first_url, second_url);
    let mut first_event = event(
        ALICE,
        "persistent-attachment-first",
        "persistent-first-image-input",
    );
    first_event["event"]["message_type"] = json!("mixed");
    first_event["event"]["attachments"] = json!([
        {"name":"notes.txt","media_type":"text/plain","data_base64":STANDARD.encode(b"first-turn-attachment")},
        first_picture
    ]);
    let first = submit(&server, &first_event, "persistent-first-image-reply");
    let run = first["run"].as_str().unwrap();
    let first_saved = waiting(&host, run, 1);
    let first_artifact = artifact(&host, &first_saved, 1);
    let first_inventory = snapshot_inventory(&first_artifact);
    let first_native = native_round(&host, &provider, run, 1);
    assert_eq!(
        prompt_images(&provider.requests()[0]),
        vec![first_url.clone()]
    );

    let text_event = event(
        ALICE,
        "persistent-attachment-text-only",
        "persistent-text-only-input",
    );
    let text_only = submit(&server, &text_event, "persistent-text-only-reply");
    assert_eq!(text_only["run"], first["run"]);
    assert_eq!(text_only["session"], first["session"]);
    let text_saved = waiting(&host, run, 2);
    assert!(
        text_saved["results"]["wait_input"][1]["completion"]["output"]["attachments"]["files"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        prompt_images(&provider.requests()[3]),
        vec![first_url.clone()]
    );
    assert_eq!(
        native_round(&host, &provider, run, 2)["session_id"],
        first_native["session_id"]
    );

    let mut second_event = event(
        ALICE,
        "persistent-attachment-second",
        "persistent-second-image-input",
    );
    second_event["event"]["message_type"] = json!("mixed");
    second_event["event"]["attachments"] = json!([
        {"name":"notes.txt","media_type":"text/plain","data_base64":STANDARD.encode(b"second-turn-attachment")},
        second_picture
    ]);
    let second = submit(&server, &second_event, "persistent-second-image-reply");
    assert_eq!(second["run"], first["run"]);
    assert_eq!(second["session"], first["session"]);
    let saved = waiting(&host, run, 3);
    assert_eq!(runs(&host).len(), 1);
    assert_eq!(
        native_round(&host, &provider, run, 3)["session_id"],
        first_native["session_id"]
    );
    let images = prompt_images(&provider.requests()[6]);
    assert_eq!(images, vec![first_url, second_url]);
    let committed = artifact(&host, &saved, 3);
    assert_eq!(
        fs::read(committed.join("files/saved-note.txt")).unwrap(),
        b"first-turn-attachment"
    );
    assert_eq!(
        fs::read(committed.join("files/second-note.txt")).unwrap(),
        b"second-turn-attachment"
    );
    assert_eq!(snapshot_inventory(&first_artifact), first_inventory);
    assert_eq!(
        submit(&server, &second_event, "persistent-second-image-reply"),
        second
    );
    idle(&provider, 9);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({
            "case_source":CASE_SOURCE,"responses":[first,text_only,second],
            "current_turn_attachments_only":true,"attachments_readonly":true,
            "same_run_and_native_session":true,"native_prompt_images":images,
            "old_fs2_snapshot_unchanged":true,"real_vision_model_requests":0,"stopped":stopped
        }),
    );
}
