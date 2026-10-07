#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Host, HttpHost, Provider, Step, command, complete, tool_feedback, wait_until};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const CASE_SOURCE: &str = "tests/goose_conversation.rs";

fn nonce(label: &str) -> String {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    format!(
        "g2e-{label}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}

fn turn(session: &str, message: &str, previous: Option<&str>) -> Value {
    let identity = format!("{:x}", Sha256::digest(nonce("turn").as_bytes()));
    json!({
        "graph":"fixture",
        "run":format!("channel-{}-{}-{}-{}-{}", &identity[..8], &identity[8..12],
            &identity[12..16], &identity[16..20], &identity[20..32]),
        "session":session,"reply_node":"worker","input":{"message":message},
        "previous_run":previous
    })
}

fn graph(two_agents: bool) -> Value {
    let mut definition = json!({
        "objective":"deterministic Goose Graph/channel conversation contract",
        "entry":"worker",
        "agents":{"worker":{"model":"models.worker","instructions":
            "Use only the authorized Anchor tools. Inspect actual tool results, write evidence.txt, then final_result with an allowed route. Never replay an uncertain side effect."}},
        "ops":{"verify":{"run":"sh -c 'set -eu; cat /in/worker/evidence.txt > verified.txt'"}},
        "nodes":[{"id":"worker","agent":"worker"},{"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    });
    if two_agents {
        definition["agents"]["peer"] = definition["agents"]["worker"].clone();
        definition["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"peer","agent":"peer"}));
        definition["edges"] = json!([
            {"from":"worker","to":"peer"},
            {"from":"worker","to":"verify"},
            {"from":"peer","to":"verify"}
        ]);
        definition["ops"]["verify"]["run"] = json!(
            "sh -c 'set -eu; if [ -f /in/peer/evidence.txt ]; then cat /in/peer/evidence.txt; else cat /in/worker/evidence.txt; fi > verified.txt'"
        );
    }
    definition
}

fn submit(server: &HttpHost, request: &Value) -> String {
    let (status, accepted) = server.request("POST", "/conversation-runs", Some(request));
    assert_eq!(status, 202, "{accepted}");
    assert_eq!(accepted["run"], request["run"]);
    request["run"].as_str().unwrap().to_owned()
}

fn node_steps(value: &str, route: &str, previous: Option<&str>) -> Vec<Step> {
    let previous_check = match previous {
        Some(expected) => format!(
            "test \"$(cat /previous/evidence.txt)\" = '{expected}'; if printf corrupt > /previous/evidence.txt; then exit 91; fi; test \"$(cat /previous/evidence.txt)\" = '{expected}'; printf previous-readonly;"
        ),
        None => "test ! -e /previous/evidence.txt;".into(),
    };
    vec![
        command(&format!(
            "set -eu; test ! -e evidence.txt; {previous_check} printf '%s' '{value}' > evidence.txt; cat evidence.txt"
        )),
        complete(route).after(""),
        Step::text("conversation node finished"),
    ]
}

fn assert_artifact(host: &Host, run: &str, node: &str, value: &[u8]) -> Value {
    let record = host.record(run);
    assert_eq!(record["status"], "completed", "{record}");
    assert_eq!(record["invocations"][node], 1);
    let name = if node == "verify" {
        "verified.txt"
    } else {
        "evidence.txt"
    };
    assert_eq!(host.base.file(&record, node, name), value);
    let manifest = fixture::read_json(host.base.artifact(&record, node).join("manifest.json"));
    assert_eq!(
        manifest["files"][name]["sha256"],
        format!("{:x}", Sha256::digest(value))
    );
    assert_eq!(manifest["files"][name]["bytes"], value.len());
    assert_eq!(
        host.base
            .workspace_files(run, name)
            .iter()
            .filter(|bytes| bytes.as_slice() == value)
            .count(),
        1
    );
    manifest
}

fn assert_scope(host: &Host, run: &str, node: &str) -> (Value, PathBuf) {
    let metadata = fixture::read_json(
        host.base
            .root
            .path()
            .join("state/run-metadata")
            .join(format!("{run}.json")),
    );
    let identity = serde_json::to_vec(&json!([
        metadata["bundle_source"],
        metadata["conversation"]["session"],
        node
    ]))
    .unwrap();
    let expected = format!("gc1-{:x}", Sha256::digest(identity));
    let fact = host.native_fact(run, node, 1);
    let process = host
        .base
        .root
        .path()
        .join("work/.goose-process/conversations")
        .join(&expected)
        .join("process");
    assert_eq!(fact["version"], 2);
    assert_eq!(fact["conversation_scope"], expected);
    assert_eq!(fact["key"]["run_id"], run);
    assert_eq!(fact["key"]["node_id"], node);
    assert_eq!(fact["key"]["invocation"], 1);
    assert_eq!(fact["binary_sha256"], goose::GOOSE_SHA256);
    assert!(
        fact["model_binding"]
            .as_str()
            .is_some_and(|binding| binding.len() == 64)
    );
    assert!(
        fact["session_id"]
            .as_str()
            .is_some_and(|session| !session.is_empty())
    );
    assert_eq!(
        process,
        host.base
            .root
            .path()
            .join("work/.goose-process/conversations")
            .join(&expected)
            .join("process")
    );
    assert!(process.join("data/sessions/sessions.db").is_file());
    (fact, process)
}

fn fact_path(host: &Host, run: &str, node: &str) -> PathBuf {
    let record = host.record(run);
    let key = anchor_runtime_rig::graph::InvocationKey {
        run_id: run.into(),
        graph_digest: record["graph_digest"].as_str().unwrap().into(),
        node_id: node.into(),
        invocation: 1,
    };
    host.base.root.path().join("state/goose-acp").join(format!(
        "{:x}.json",
        Sha256::digest(key.durable_key().as_bytes())
    ))
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_channel_two_turns_restart_reuse_history_but_not_workspace() {
    let first_nonce = nonce("restart-first");
    let second_nonce = nonce("restart-second");
    let provider = Provider::new(
        "goose-conversation-restart",
        node_steps(&first_nonce, "verify", None),
    );
    let host = Host::new(&graph(false)).default_runtime();
    let mut server = host.serve(&provider);
    let first = submit(&server, &turn("restart-user", &first_nonce, None));
    server.wait_status(&first, "completed");
    assert_artifact(&host, &first, "worker", first_nonce.as_bytes());
    assert_artifact(&host, &first, "verify", first_nonce.as_bytes());
    let (first_fact, first_process) = assert_scope(&host, &first, "worker");
    let first_history = host.native_conversation(&first, "worker", 1);
    assert!(first_history.to_string().contains(&first_nonce));
    let frozen_record = fs::read(
        host.base
            .root
            .path()
            .join("state/runs")
            .join(format!("{first}.json")),
    )
    .unwrap();
    server.kill();
    drop(server);

    provider.append(node_steps(&second_nonce, "verify", Some(&first_nonce)));
    let server = host.serve(&provider);
    let second_request = turn("restart-user", &second_nonce, Some(&first));
    let second = submit(&server, &second_request);
    server.wait_status(&second, "completed");
    let (second_fact, second_process) = assert_scope(&host, &second, "worker");
    assert_eq!(first_fact["session_id"], second_fact["session_id"]);
    assert_eq!(
        first_fact["conversation_scope"],
        second_fact["conversation_scope"]
    );
    assert_eq!(first_process, second_process);
    assert_ne!(first_fact["key"], second_fact["key"]);
    let worker_artifact = assert_artifact(&host, &second, "worker", second_nonce.as_bytes());
    assert_artifact(&host, &second, "verify", second_nonce.as_bytes());
    assert_artifact(&host, &first, "worker", first_nonce.as_bytes());
    assert_ne!(
        host.base.artifact(&host.record(&first), "worker"),
        host.base.artifact(&host.record(&second), "worker")
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 6);
    assert!(requests[3]["messages"].to_string().contains(&first_nonce));
    assert!(
        requests[3]["messages"]
            .to_string()
            .contains("conversation node finished")
    );
    assert!(requests[3]["messages"].to_string().contains(&second_nonce));
    assert!(
        tool_feedback(&requests[4])
            .last()
            .unwrap()
            .to_string()
            .contains("previous-readonly")
    );
    let history = host.native_conversation(&second, "worker", 1);
    assert!(history.to_string().contains(&first_nonce));
    assert!(history.to_string().contains(&second_nonce));
    assert!(history.as_array().unwrap().len() > first_history.as_array().unwrap().len());
    for (method, path) in [
        ("POST", format!("/runs/{first}/resume")),
        ("DELETE", format!("/runs/{first}")),
        ("DELETE", format!("/runs/{second}")),
    ] {
        let (status, rejected) = server.request(method, &path, None);
        assert_eq!(status, 409, "{rejected}");
    }
    assert_eq!(
        fs::read(
            host.base
                .root
                .path()
                .join("state/runs")
                .join(format!("{first}.json"))
        )
        .unwrap(),
        frozen_record
    );
    assert_eq!(submit(&server, &second_request), second);
    assert_eq!(provider.requests().len(), 6);
    host.evidence(
        &provider,
        &second,
        json!({
            "case_source":CASE_SOURCE,"restart":true,"same_native_session":true,
            "first_fact":first_fact,"second_fact":second_fact,"native_process":second_process,
            "prior_native_history":first_history,"native_history":history,
            "previous_readonly":true,"separate_workspaces_and_artifacts":true,
            "worker_artifact":worker_artifact,"old_resume_and_single_run_delete_rejected":true,
            "idempotent_submission_model_requests":0
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_channel_users_and_nodes_ignore_forged_input_authority() {
    let alice_nonce = nonce("alice");
    let bob_nonce = nonce("bob");
    let alice_worker = nonce("alice-worker-private");
    let alice_peer = nonce("alice-peer-private");
    let bob_worker = nonce("bob-worker-private");
    let bob_peer = nonce("bob-peer-private");
    let provider = Provider::new("goose-conversation-isolation", vec![]);
    provider.append(node_steps(&alice_worker, "peer", None));
    provider.append(node_steps(&alice_peer, "verify", None));
    let host = Host::new(&graph(true)).default_runtime();
    let server = host.serve(&provider);
    let alice = submit(&server, &turn("alice", &alice_nonce, None));
    server.wait_status(&alice, "completed");
    let (alice_worker_fact, alice_worker_process) = assert_scope(&host, &alice, "worker");
    let (alice_peer_fact, alice_peer_process) = assert_scope(&host, &alice, "peer");
    assert_ne!(
        alice_worker_fact["conversation_scope"],
        alice_peer_fact["conversation_scope"]
    );
    assert_ne!(alice_worker_process, alice_peer_process);

    let crossing = turn("bob", &bob_nonce, Some(&alice));
    let (status, rejected) = server.request("POST", "/conversation-runs", Some(&crossing));
    assert_eq!(status, 409, "{rejected}");
    assert_eq!(provider.requests().len(), 6);
    let protected = host.base.root.path().join("operator-private.txt");
    fs::write(&protected, "operator-data").unwrap();
    provider.append(vec![
        Step::tool("anchor_run", json!({"command":["touch",protected]})),
        command(&format!(
            "set -eu; test ! -e evidence.txt; test ! -e /previous/evidence.txt; if printf stolen > '{}'; then exit 92; fi; printf '%s' '{bob_worker}' > evidence.txt; cat evidence.txt",
            protected.display()
        )).after("not authorized"),
        complete("peer").after(""),
        Step::text("isolated worker finished"),
    ]);
    provider.append(node_steps(&bob_peer, "verify", None));
    let mut bob_request = turn("bob", &bob_nonce, None);
    bob_request["input"]["session"] = json!("alice");
    bob_request["input"]["node"] = json!("peer");
    bob_request["input"]["bundle_source"] = json!(host.base.root.path().join("bundle"));
    bob_request["input"]["conversation_scope"] = alice_worker_fact["conversation_scope"].clone();
    bob_request["input"]["previous_run"] = json!(alice);
    bob_request["input"]["network"] = json!(true);
    bob_request["input"]["allowed_commands"] = json!(["touch"]);
    let bob = submit(&server, &bob_request);
    let bob_detail = server.wait_status(&bob, "completed");
    assert_eq!(bob_detail["state"]["trigger"]["session"], "bob");
    assert_eq!(fs::read(&protected).unwrap(), b"operator-data");
    let (bob_worker_fact, bob_worker_process) = assert_scope(&host, &bob, "worker");
    let (bob_peer_fact, bob_peer_process) = assert_scope(&host, &bob, "peer");
    let scopes = [
        &alice_worker_fact,
        &alice_peer_fact,
        &bob_worker_fact,
        &bob_peer_fact,
    ]
    .into_iter()
    .map(|fact| fact["conversation_scope"].as_str().unwrap())
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(scopes.len(), 4);
    assert_ne!(alice_worker_process, bob_worker_process);
    assert_ne!(alice_peer_process, bob_peer_process);
    let requests = provider.requests();
    assert_eq!(requests.len(), 13);
    assert!(
        !requests[3]["messages"]
            .to_string()
            .contains("conversation node finished")
    );
    for request in &requests[6..] {
        for private in [&alice_nonce, &alice_worker, &alice_peer] {
            assert!(
                !request["messages"].to_string().contains(private),
                "other user's native history leaked: {request}"
            );
        }
    }
    let denial = tool_feedback(&requests[7]).last().unwrap().to_string();
    assert!(
        denial.contains("not_executed") && denial.contains("not authorized"),
        "{denial}"
    );
    assert_artifact(&host, &alice, "worker", alice_worker.as_bytes());
    assert_artifact(&host, &alice, "peer", alice_peer.as_bytes());
    assert_artifact(&host, &alice, "verify", alice_peer.as_bytes());
    assert_artifact(&host, &bob, "worker", bob_worker.as_bytes());
    assert_artifact(&host, &bob, "peer", bob_peer.as_bytes());
    assert_artifact(&host, &bob, "verify", bob_peer.as_bytes());
    let peer_history = host.native_conversation(&bob, "peer", 1);
    assert!(
        !peer_history
            .to_string()
            .contains("isolated worker finished")
    );
    host.evidence(
        &provider,
        &bob,
        json!({
            "case_source":CASE_SOURCE,"distinct_user_and_node_scopes":scopes,
            "alice_worker_fact":alice_worker_fact,"alice_peer_fact":alice_peer_fact,
            "bob_worker_fact":bob_worker_fact,"bob_peer_fact":bob_peer_fact,
            "bob_peer_native_history":peer_history,"cross_user_predecessor_rejection":rejected,
            "input_scope_and_permission_spoofing_rejected":true,"command_denial":denial,
            "host_file_unchanged":true
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_channel_unknown_effect_new_turn_checks_state_and_rejects_old_receipt() {
    let first_nonce = nonce("unknown-first");
    let second_nonce = nonce("unknown-second");
    let provider = Provider::new("goose-conversation-unknown-effect", vec![]);
    let payload = json!({"effect_key":first_nonce});
    provider.append(vec![
        command("printf checkpoint > checkpoint.txt; cat checkpoint.txt"),
        command(&format!(
            "curl --silent --show-error --fail --max-time 5 --noproxy '*' --request POST --header 'Content-Type: application/json' --data '{}' '{}/effects'; printf ready > effect-ready; sleep 60",
            payload, provider.url
        )).after("checkpoint"),
    ]);
    let mut definition = graph(false);
    definition["agents"]["worker"]["network"] = json!(true);
    let host = Host::new(&definition)
        .default_runtime()
        .with_allowed_commands("sh,cat,curl,sleep,true");
    let mut server = host.serve(&provider);
    let first = submit(&server, &turn("unknown-user", &first_nonce, None));
    wait_until("unknown external effect and workspace checkpoint", || {
        provider.effects().len() == 1
            && !host.base.workspace_files(&first, "effect-ready").is_empty()
    });
    let (before_fact, process) = assert_scope(&host, &first, "worker");
    assert_eq!(before_fact["tool_observation"]["tool"], "anchor_run");
    assert!(
        before_fact["tool_observation"]["arguments"]
            .to_string()
            .contains(&first_nonce)
    );
    assert!(before_fact["tool_observation"]["result"].is_null());
    assert!(before_fact["completion"].is_null());
    let before_history = host.native_conversation(&first, "worker", 1);
    assert!(!before_history.to_string().contains("effect_count"));
    server.kill();
    drop(server);
    assert_eq!(provider.effects(), vec![payload.clone()]);
    assert_eq!(provider.requests().len(), 2);
    provider.append(vec![
        complete("verify"),
        command(&format!(
            "set -eu; test ! -e checkpoint.txt; test ! -e effect-ready; test -f /previous/effect-ready; test \"$(cat /previous/checkpoint.txt)\" = checkpoint; if printf corrupt > /previous/checkpoint.txt; then exit 93; fi; curl --silent --show-error --fail --max-time 5 --noproxy '*' '{}/effects' > evidence.txt; cat evidence.txt",
            provider.url
        )).after("after observing business results"),
        complete("verify").after("effect_count"),
        Step::text("checked external state without repeating the interrupted POST"),
    ]);
    let server = host.serve(&provider);
    let (status, stopped) = server.request("POST", &format!("/runs/{first}/stop"), None);
    assert_eq!(status, 202, "{stopped}");
    server.wait_status(&first, "stopped");
    let second = submit(&server, &turn("unknown-user", &second_nonce, Some(&first)));
    server.wait_status(&second, "completed");
    let (after_fact, after_process) = assert_scope(&host, &second, "worker");
    assert_eq!(after_fact["session_id"], before_fact["session_id"]);
    assert_eq!(process, after_process);
    assert_ne!(after_fact["key"], before_fact["key"]);
    assert_eq!(provider.effects(), vec![payload]);
    let requests = provider.requests();
    assert_eq!(requests.len(), 6);
    let continued = requests[2]["messages"].to_string();
    assert!(continued.contains(&first_nonce) && continued.contains(&second_nonce));
    assert!(continued.contains("do not blindly repeat"));
    assert!(continued.contains("checkpoint"));
    let rejected_completion = tool_feedback(&requests[3]).last().unwrap().to_string();
    assert!(
        rejected_completion.contains("after observing business results"),
        "{rejected_completion}"
    );
    let old_record_path = host
        .base
        .root
        .path()
        .join("state/runs")
        .join(format!("{first}.json"));
    let frozen_record = fs::read(&old_record_path).unwrap();
    let (status, rejected_resume) = server.request("POST", &format!("/runs/{first}/resume"), None);
    assert_eq!(status, 409, "{rejected_resume}");
    assert_eq!(fs::read(&old_record_path).unwrap(), frozen_record);
    assert_eq!(provider.requests().len(), 6);
    let saved = host.record(&second);
    let bytes = host.base.file(&saved, "worker", "evidence.txt");
    let report: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(report["effect_count"], 1);
    assert_artifact(&host, &second, "worker", &bytes);
    assert_artifact(&host, &second, "verify", &bytes);
    host.evidence(&provider, &second, json!({
        "case_source":CASE_SOURCE,"unknown_before_kill":before_fact,
        "native_history_before_kill":before_history,"continued_fact":after_fact,
        "external_effect_count":1,"old_receipt_completion_rejection":rejected_completion,
        "old_run_resume_rejection":rejected_resume,"new_run_workspace_clean":true,
        "previous_uncommitted_workspace_readonly":true,"external_state_verified_before_completion":report
    }));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_channel_skipped_agent_keeps_nearest_selected_native_session_and_previous() {
    let first_nonce = nonce("skip-first");
    let second_nonce = nonce("skip-second");
    let third_nonce = nonce("skip-third");
    let first_worker = nonce("first-worker");
    let first_peer = nonce("first-peer");
    let second_worker = nonce("second-worker");
    let third_worker = nonce("third-worker");
    let third_peer = nonce("third-peer");
    let provider = Provider::new("goose-conversation-skipped-agent", vec![]);
    provider.append(node_steps(&first_worker, "peer", None));
    provider.append(node_steps(&first_peer, "verify", None));
    provider.append(node_steps(&second_worker, "verify", Some(&first_worker)));
    provider.append(node_steps(&third_worker, "peer", Some(&second_worker)));
    provider.append(node_steps(&third_peer, "verify", Some(&first_peer)));
    let host = Host::new(&graph(true)).default_runtime();
    let server = host.serve(&provider);
    let first = submit(&server, &turn("skip-user", &first_nonce, None));
    server.wait_status(&first, "completed");
    let (first_peer_fact, first_peer_process) = assert_scope(&host, &first, "peer");
    let first_peer_history = host.native_conversation(&first, "peer", 1);
    let second = submit(&server, &turn("skip-user", &second_nonce, Some(&first)));
    server.wait_status(&second, "completed");
    let second_record = host.record(&second);
    assert!(second_record["invocations"]["peer"].is_null());
    assert!(second_record["results"]["peer"].is_null());
    assert!(!fact_path(&host, &second, "peer").exists());
    let after_skip_history = host.session_conversation(
        &first_peer_process,
        first_peer_fact["session_id"].as_str().unwrap(),
    );
    assert_eq!(after_skip_history, first_peer_history);
    let (second_worker_fact, second_worker_process) = assert_scope(&host, &second, "worker");
    let third = submit(&server, &turn("skip-user", &third_nonce, Some(&second)));
    server.wait_status(&third, "completed");
    let (third_peer_fact, third_peer_process) = assert_scope(&host, &third, "peer");
    let (third_worker_fact, third_worker_process) = assert_scope(&host, &third, "worker");
    assert_eq!(first_peer_fact["session_id"], third_peer_fact["session_id"]);
    assert_eq!(first_peer_process, third_peer_process);
    assert_eq!(
        second_worker_fact["session_id"],
        third_worker_fact["session_id"]
    );
    assert_eq!(second_worker_process, third_worker_process);
    let requests = provider.requests();
    assert_eq!(requests.len(), 15);
    let peer_request = requests[12]["messages"].to_string();
    assert!(peer_request.contains(&first_nonce) && peer_request.contains(&first_peer));
    assert!(peer_request.contains(&third_nonce));
    assert!(!peer_request.contains(&second_nonce));
    assert!(requests[9]["messages"].to_string().contains(&second_nonce));
    assert!(
        tool_feedback(&requests[13])
            .last()
            .unwrap()
            .to_string()
            .contains("previous-readonly")
    );
    assert_artifact(&host, &first, "peer", first_peer.as_bytes());
    assert_artifact(&host, &second, "worker", second_worker.as_bytes());
    assert_artifact(&host, &second, "verify", second_worker.as_bytes());
    assert_artifact(&host, &third, "worker", third_worker.as_bytes());
    assert_artifact(&host, &third, "peer", third_peer.as_bytes());
    assert_artifact(&host, &third, "verify", third_peer.as_bytes());
    let third_peer_history = host.native_conversation(&third, "peer", 1);
    assert!(third_peer_history.to_string().contains(&first_peer));
    assert!(third_peer_history.to_string().contains(&third_peer));
    assert!(!third_peer_history.to_string().contains(&second_nonce));
    host.evidence(&provider, &third, json!({
        "case_source":CASE_SOURCE,"skipped_run":second_record,
        "skipped_node_has_no_invocation_or_native_fact":true,"history_unchanged_while_skipped":true,
        "first_peer_fact":first_peer_fact,"third_peer_fact":third_peer_fact,
        "second_worker_fact":second_worker_fact,"third_worker_fact":third_worker_fact,
        "peer_native_history":third_peer_history,"previous_skips_inactive_predecessor":true,
        "previous_readonly":true
    }));
}

fn assert_no_silent_rebind(server: &HttpHost, request: &Value, provider: &Provider) -> Value {
    let before = provider.requests().len();
    let (status, response) = server.request("POST", "/conversation-runs", Some(request));
    let rejected = if status == 202 {
        server.wait_status(request["run"].as_str().unwrap(), "failed")
    } else {
        assert!(matches!(status, 400 | 409), "{status}: {response}");
        response
    };
    assert_eq!(
        provider.requests().len(),
        before,
        "binding failure must precede any model call"
    );
    rejected
}

fn binding_failure(failure: &str) {
    let first_nonce = nonce(failure);
    let scenario = format!("goose-conversation-{failure}");
    let provider = Provider::new(&scenario, node_steps(&first_nonce, "verify", None));
    let host = Host::new(&graph(false)).default_runtime();
    let mut server = host.serve(&provider);
    let first = submit(&server, &turn("binding-user", &first_nonce, None));
    server.wait_status(&first, "completed");
    let (first_fact, process) = assert_scope(&host, &first, "worker");
    let first_history = host.native_conversation(&first, "worker", 1);
    let path = fact_path(&host, &first, "worker");
    let original = fs::read(&path).unwrap();
    let second_request = turn("binding-user", &nonce("rebind-denied"), Some(&first));
    server.kill();
    drop(server);
    let changed_provider = (failure == "changed-model").then(|| {
        Provider::with_model(
            &format!("{scenario}-new-provider"),
            vec![],
            "fixture-goose-changed",
        )
    });
    let active_provider = changed_provider.as_ref().unwrap_or(&provider);
    match failure {
        "missing-fact" => fs::remove_file(&path).unwrap(),
        "missing-scope" | "missing-model-binding" => {
            let mut damaged = first_fact.clone();
            damaged
                .as_object_mut()
                .unwrap()
                .remove(if failure == "missing-scope" {
                    "conversation_scope"
                } else {
                    "model_binding"
                });
            fs::write(&path, serde_json::to_vec_pretty(&damaged).unwrap()).unwrap();
        }
        "changed-model" => {}
        _ => unreachable!(),
    }
    let server = host.serve(active_provider);
    let rejected = assert_no_silent_rebind(&server, &second_request, active_provider);
    let after_history =
        host.session_conversation(&process, first_fact["session_id"].as_str().unwrap());
    assert_eq!(
        after_history, first_history,
        "failure must not modify native history"
    );
    let second_fact_path = fact_path_for_request(&host, &second_request, &first_fact);
    if second_fact_path.exists() {
        let rejected_fact = fixture::read_json(&second_fact_path);
        assert!(rejected_fact["completion"].is_null());
        assert!(
            rejected_fact["session_id"].is_null()
                || rejected_fact["session_id"] == first_fact["session_id"]
        );
    }
    provider.assert_consumed();
    active_provider.assert_consumed();
    assert_eq!(provider.requests().len(), 3);
    fs::write(&path, &original).unwrap();
    assert_artifact(&host, &first, "worker", first_nonce.as_bytes());
    assert_artifact(&host, &first, "verify", first_nonce.as_bytes());
    host.evidence(
        active_provider,
        &first,
        json!({
            "case_source":CASE_SOURCE,"failure":failure,"rejected_request":second_request,
            "rejection":rejected,"no_model_calls_after_binding_failure":true,
            "baseline_provider_requests":provider.requests(),"baseline_fact":first_fact,
            "native_history_unchanged":after_history,
            "model_and_endpoint_changed":failure == "changed-model",
            "fixture_fact_restored_only_for_evidence":failure != "changed-model"
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn missing_predecessor_fact_fails_closed() {
    binding_failure("missing-fact");
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn missing_predecessor_scope_fails_closed() {
    binding_failure("missing-scope");
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn missing_predecessor_model_binding_fails_closed() {
    binding_failure("missing-model-binding");
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn changed_model_and_endpoint_fail_closed() {
    binding_failure("changed-model");
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn same_run_loop_reuses_native_session_for_each_invocation() {
    let provider = Provider::new(
        "goose-conversation-loop",
        vec![
            command("printf first-loop > evidence.txt; cat evidence.txt"),
            complete("worker").after("first-loop"),
            Step::text("first loop finished"),
            command(
                "test \"$(cat evidence.txt)\" = first-loop; printf second-loop > evidence.txt; cat evidence.txt",
            ),
            complete("verify").after("second-loop"),
            Step::text("second loop finished"),
        ],
    );
    let mut definition = graph(false);
    definition["edges"] = json!([{"from":"worker","to":"worker"},{"from":"worker","to":"verify"}]);
    definition["nodes"][0]["max_rounds"] = json!(3);
    let host = Host::new(&definition).default_runtime();
    let server = host.serve(&provider);
    let run = submit(&server, &turn("loop-user", "loop", None));
    server.wait_status(&run, "completed");
    let first = host.native_fact(&run, "worker", 1);
    let second = host.native_fact(&run, "worker", 2);
    assert_eq!(first["session_id"], second["session_id"]);
    assert_eq!(first["conversation_scope"], second["conversation_scope"]);
    assert_eq!(host.record(&run)["invocations"]["worker"], 2);
    assert_eq!(
        host.base.file(&host.record(&run), "verify", "verified.txt"),
        b"second-loop"
    );
    assert!(
        provider.requests()[3]["messages"]
            .to_string()
            .contains("first loop finished")
    );
    host.evidence(&provider, &run, json!({"case_source":CASE_SOURCE,"first_fact":first,"second_fact":second,"same_run_native_continuation":true}));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn frozen_channel_image_reaches_native_prompt_without_host_path_grants() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let bytes = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap();
    let provider = Provider::with_model(
        "goose-conversation-image-input",
        vec![
            command(
                "test -f /in/channel/pixel.png; if printf invalid > /in/channel/pixel.png; then exit 91; fi; printf frozen-image > evidence.txt; cat evidence.txt",
            ),
            complete("verify").after("frozen-image"),
            Step::text("image input fixture finished"),
        ],
        "gpt-4o",
    );
    let host = Host::new(&graph(false)).default_runtime();
    let server = host.serve(&provider);
    let mut request = turn("image-user", "inspect frozen input", None);
    request["attachments"] = json!([{"name":"pixel.png","data_base64":STANDARD.encode(&bytes),"media_type":"image/png"}]);
    let run = submit(&server, &request);
    server.wait_status(&run, "completed");
    let images = provider.requests()[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "image_url")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 1);
    assert_eq!(
        images[0]["image_url"]["url"],
        format!("data:image/png;base64,{}", STANDARD.encode(&bytes))
    );
    assert_artifact(&host, &run, "worker", b"frozen-image");
    assert_artifact(&host, &run, "verify", b"frozen-image");
    host.evidence(&provider, &run, json!({"case_source":CASE_SOURCE,"native_prompt_image":images,"frozen_input_readonly":true,"image_sha256":format!("{:x}",Sha256::digest(&bytes)),"real_vision_model_requests":0}));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn graph_delete_cleans_all_conversation_scopes_and_keeps_another_graph() {
    let provider = Provider::new("goose-conversation-delete", vec![]);
    let host = Host::new(&graph(false)).default_runtime();
    let server = host.serve(&provider);
    let mut scopes = Vec::new();
    let mut runs = Vec::new();
    for user in ["delete-alice", "delete-bob"] {
        provider.append(node_steps(user, "verify", None));
        let run = submit(&server, &turn(user, user, None));
        server.wait_status(&run, "completed");
        scopes.push(
            assert_scope(&host, &run, "worker")
                .1
                .parent()
                .unwrap()
                .to_path_buf(),
        );
        runs.push(run);
    }
    let (status, created) = server.request(
        "POST",
        "/graphs",
        Some(&json!({"name":"other","definition":graph(false)})),
    );
    assert_eq!(status, 201, "{created}");
    provider.append(node_steps("other-graph-kept", "verify", None));
    let mut other_request = turn("delete-other", "keep this graph", None);
    other_request["graph"] = json!("other");
    let other = submit(&server, &other_request);
    server.wait_status(&other, "completed");
    let (other_fact, other_scope) = assert_scope(&host, &other, "worker");
    let (status, deleted) = server.request("DELETE", "/graphs/fixture", None);
    assert_eq!(status, 204, "{deleted}");
    for scope in &scopes {
        assert!(!scope.exists());
    }
    for run in &runs {
        assert_eq!(server.request("GET", &format!("/runs/{run}"), None).0, 404);
    }
    assert!(other_scope.is_dir());
    assert_artifact(&host, &other, "worker", b"other-graph-kept");
    assert_artifact(&host, &other, "verify", b"other-graph-kept");
    host.evidence(&provider, &other, json!({"case_source":CASE_SOURCE,"deleted_runs":runs,"deleted_scopes":scopes,"retained_other_graph_fact":other_fact,"graph_cleanup_verified":true}));
}

fn fact_path_for_request(host: &Host, request: &Value, previous_fact: &Value) -> PathBuf {
    let key = anchor_runtime_rig::graph::InvocationKey {
        run_id: request["run"].as_str().unwrap().into(),
        graph_digest: previous_fact["key"]["graph_digest"]
            .as_str()
            .unwrap()
            .into(),
        node_id: "worker".into(),
        invocation: 1,
    };
    host.base.root.path().join("state/goose-acp").join(format!(
        "{:x}.json",
        Sha256::digest(key.durable_key().as_bytes())
    ))
}
