#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Gate, Host, Provider, Step};
use serde_json::{Value, json};
use std::fs;

fn graph() -> Value {
    json!({"entry":"seed","ops":{"seed":{"run":"sh -c 'printf fixture > seed.txt'"}},
        "nodes":[{"id":"seed","op":"seed"}],"edges":[]})
}

fn create(server: &goose::HttpHost) {
    let (status, saved) = server.request("POST", "/sessions", Some(&json!({"id":"goose-pilot"})));
    assert_eq!(status, 201, "{saved}");
}

fn submit(server: &goose::HttpHost, request: &str, message: &str) -> Value {
    let (status, saved) = server.request(
        "POST",
        "/sessions/goose-pilot/turns",
        Some(&json!({"request_id":request,"message":message})),
    );
    assert_eq!(status, 202, "{saved}");
    saved["turn"].clone()
}

fn wait(server: &goose::HttpHost, id: &str, expected: &str) -> Value {
    let mut turn = Value::Null;
    goose::wait_until("Goose Pilot terminal Turn", || {
        let (status, saved) =
            server.request("GET", &format!("/sessions/goose-pilot/turns/{id}"), None);
        assert_eq!(status, 200, "{saved}");
        turn = saved["turn"].clone();
        assert!(
            turn["status"] == "running" || turn["status"] == expected,
            "{turn}"
        );
        turn["status"] == expected
    });
    turn
}

fn evidence(host: &Host, provider: &Provider, checks: Value) {
    provider.assert_consumed();
    fs::write(provider.root.join("evidence.json"), serde_json::to_vec_pretty(&json!({
        "status":"passed","runtime":"goose","goose_version":"1.53.0",
        "goose_binary_sha256":goose::GOOSE_SHA256,"real_model_requests":0,
        "host_binary_sha256":goose::digest(&host.base.root.path().join("anchor-runner-host")),
        "source_sha256":goose::digest(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goose_pilot.rs")),
        "checks":checks,"provider_requests":provider.requests(),"production_data_used":false,
    })).unwrap()).unwrap();
    println!(
        "evidence: {}",
        provider.root.join("evidence.json").display()
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_pilot_tools_followup_restart_and_sse_replay_share_native_session() {
    let provider = Provider::new(
        "goose-pilot-followup",
        vec![
            Step::tool("graph_list", json!({})),
            Step::text("fixture graph exists; no graph was started").after("fixture"),
            Step::tool("graph_read", json!({"graph":"fixture"})),
            Step::text("same conversation inspected the saved graph").after("seed"),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let mut server = host.serve(&provider);
    create(&server);
    let accepted = submit(
        &server,
        "one",
        "List saved graphs only, do not start any graph.",
    );
    let first = wait(&server, accepted["id"].as_str().unwrap(), "completed");
    assert!(first["native"].is_null());
    assert!(
        first["goose"]["session"]
            .as_str()
            .is_some_and(|session| !session.is_empty())
    );
    let calls = provider.requests().len();
    let duplicate = submit(
        &server,
        "one",
        "List saved graphs only, do not start any graph.",
    );
    assert_eq!(duplicate, first);
    assert_eq!(provider.requests().len(), calls);
    let path = format!(
        "/sessions/goose-pilot/turns/{}/events",
        first["id"].as_str().unwrap()
    );
    let stream = server.events(&path, 0);
    let sequences = stream
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .map(|value| value.parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    assert!(!sequences.is_empty());
    assert!(stream.contains("fixture graph exists"));
    let after = sequences[sequences.len() / 2];
    let replay = server.events(&path, after);
    assert!(
        replay
            .lines()
            .filter_map(|line| line.strip_prefix("id: "))
            .all(|value| value.parse::<u64>().unwrap() > after)
    );
    server.kill();
    drop(server);
    let server = host.serve(&provider);
    let second = submit(
        &server,
        "two",
        "Read the fixture Graph in this same conversation; do not start it.",
    );
    let second = wait(&server, second["id"].as_str().unwrap(), "completed");
    assert_eq!(first["goose"], second["goose"]);
    assert!(second["runs"].as_array().unwrap().is_empty());
    let (status, messages) = server.request("GET", "/sessions/goose-pilot/messages", None);
    assert_eq!(status, 200, "{messages}");
    assert_eq!(messages["messages"].as_array().unwrap().len(), 4);
    assert!(messages.to_string().contains("same conversation"));
    let (_, runs) = server.request("GET", "/runs", None);
    assert!(runs["runs"].as_array().unwrap().is_empty(), "{runs}");
    for request in provider.requests() {
        let tools = request["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| {
            tool["function"]["name"]
                .as_str()
                .unwrap()
                .ends_with("__graph_list")
        }));
        assert!(!tools.iter().any(|tool| {
            tool["function"]["name"]
                .as_str()
                .unwrap()
                .ends_with("__final_result")
        }));
    }
    let scope = first["goose"]["scope"].as_str().unwrap();
    let process = host
        .base
        .root
        .path()
        .join("state/platform/pilot")
        .join(scope)
        .join("process");
    assert!(process.is_dir());
    let history = host.session_conversation(&process, first["goose"]["session"].as_str().unwrap());
    assert!(
        history.to_string().contains("toolRequest") && history.to_string().contains("toolResponse")
    );
    assert!(history.to_string().contains("same conversation"));
    let pilot_root = process.parent().unwrap();
    fs::rename(
        pilot_root.join("goose.json"),
        pilot_root.join("goose.json.saved"),
    )
    .unwrap();
    let calls = provider.requests().len();
    let (status,rejected) = server.request("POST","/sessions/goose-pilot/turns",
        Some(&json!({"request_id":"missing-fact","message":"Do not silently create a replacement conversation."})));
    assert_eq!(status, 503, "{rejected}");
    assert_eq!(provider.requests().len(), calls);
    let (_, turns) = server.request("GET", "/sessions/goose-pilot/turns", None);
    assert_eq!(turns["turns"].as_array().unwrap().len(), 2);
    fs::rename(
        pilot_root.join("goose.json.saved"),
        pilot_root.join("goose.json"),
    )
    .unwrap();
    evidence(
        &host,
        &provider,
        json!({"native_session":first["goose"],"followup":second,
            "messages":messages,"native_history":history,
            "workspace_files":goose::file_inventory(&process),
            "sse_sequences":sequences,"replay_after":after,
            "no_unrequested_run":true,"duplicate_idempotent":true,"restart_session_load":true,
            "missing_retained_fact_rejects_without_replacement":true}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_pilot_cancel_and_unknown_mutation_restart_inspects_facts_without_replay() {
    let gate = Gate::new();
    let provider = Provider::new(
        "goose-pilot-unknown-mutation",
        vec![
            Step::tool(
                "graph_create",
                json!({"name":"made-by-pilot","definition":graph()}),
            ),
            Step::text("completion not delivered")
                .after("made-by-pilot")
                .gated(&gate),
            Step::tool("graph_read", json!({"graph":"made-by-pilot"})),
            Step::text("checked existing graph rather than creating twice").after("seed"),
        ],
    );
    let host = Host::new(&graph()).native();
    let mut server = host.serve(&provider);
    create(&server);
    let first = submit(
        &server,
        "create",
        "Create made-by-pilot using a single seed OpNode that writes fixture to seed.txt; save only, do not start.",
    );
    gate.wait_entered();
    let (status, saved) = server.request("GET", "/graphs/made-by-pilot", None);
    assert_eq!(status, 200, "{saved}");
    server.kill();
    drop(server);
    gate.open();
    let server = host.serve(&provider);
    let first = wait(&server, first["id"].as_str().unwrap(), "interrupted");
    let next = submit(
        &server,
        "inspect",
        "Continue; first inspect whether made-by-pilot already exists, do not recreate it or run it.",
    );
    let next = wait(&server, next["id"].as_str().unwrap(), "completed");
    assert_eq!(first["goose"], next["goose"]);
    let (_, retained) = server.request("GET", "/graphs/made-by-pilot", None);
    assert_eq!(saved, retained);
    let gate = Gate::new();
    provider.append(vec![Step::text("cancelled response").gated(&gate)]);
    let stopped = submit(&server, "cancel", "A cancellable short reply.");
    gate.wait_entered();
    let (status, ack) = server.request("POST", "/sessions/goose-pilot/stop", Some(&json!({})));
    assert_eq!(status, 202, "{ack}");
    assert_eq!(ack["stopping"], true);
    let stopped = wait(&server, stopped["id"].as_str().unwrap(), "stopped");
    gate.open();
    evidence(
        &host,
        &provider,
        json!({"unknown_mutation_checked":true,"no_replay":true,
        "same_session":first["goose"],"interrupted":first,"continued":next,"cancelled":stopped}),
    );
}
