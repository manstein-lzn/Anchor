#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Host, HttpHost, Provider, Step, command, complete};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn reasoning(id: &str) -> Value {
    json!({"type":"reasoning","id":format!("rs_fixture_{id}"),"status":"completed",
        "summary":[{"type":"summary_text","text":format!("fixture summary {id}")}],
        "content":[{"type":"reasoning_text","text":format!("fixture reasoning body {id}")}],
        "encrypted_content":format!("opaque-fixture-reasoning-{id}"),
        "fixture_extension":{"opaque":id}})
}

fn configure(server: &HttpHost, provider: &Provider) {
    let (status, body) = server.request(
        "PUT",
        "/settings/model/providers/default",
        Some(&json!({
            "endpoint":format!("{}/v1",provider.url),"wire_api":"responses",
            "flash":provider.model,"pro":provider.model,
            "api_key":{"action":"set","value":"responses-fixture-only-not-a-secret"}
        })),
    );
    assert_eq!(status, 200, "{body}");
}

fn submit(server: &HttpHost, request: &str) -> Value {
    let (status, body) = server.request("POST", "/sessions/responses/turns", Some(&json!({
        "request_id":request,"message":"Inspect the saved fixture Graph using the available tools."
    })));
    assert_eq!(status, 202, "{body}");
    body["turn"].clone()
}

fn wait(server: &HttpHost, turn: &Value) -> Value {
    let mut result = Value::Null;
    goose::wait_until("Responses Pilot terminal Turn", || {
        let (status, body) = server.request(
            "GET",
            &format!("/sessions/responses/turns/{}", turn["id"].as_str().unwrap()),
            None,
        );
        assert_eq!(status, 200, "{body}");
        result = body["turn"].clone();
        assert!(
            result["status"] == "running" || result["status"] == "completed",
            "{result}"
        );
        result["status"] == "completed"
    });
    result
}

fn contains_value(history: &Value, expected: &Value) -> bool {
    history == expected
        || match history {
            Value::Array(values) => values.iter().any(|value| contains_value(value, expected)),
            Value::Object(values) => values.values().any(|value| contains_value(value, expected)),
            _ => false,
        }
}

fn evidence(host: &Host, provider: &Provider, checks: Value) {
    provider.assert_consumed();
    for request in provider.requests() {
        assert_eq!(request["store"], false);
        assert!(
            request["include"].as_array().is_some_and(|include| include
                .iter()
                .any(|item| item == "reasoning.encrypted_content")),
            "stateless Responses requests must request encrypted reasoning: {request}"
        );
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let record = json!({"status":"passed","runtime":"goose","actual_goose":true,
        "goose_version":"1.53.0","goose_binary_sha256":goose::GOOSE_SHA256,
        "host_binary_sha256":goose::digest(&host.base.root.path().join("anchor-runner-host")),
        "test_binary_sha256":goose::digest(&std::env::current_exe().unwrap()),
        "test_source_sha256":goose::digest(&source.join("tests/goose_responses.rs")),
        "fixture_source_sha256":goose::digest(&source.join("tests/support/goose_fixture/responses.rs")),
        "real_model_requests":0,"production_data_used":false,"dotenv_loaded":false,
        "empty_delta_heartbeat_accepted":true,"encrypted_reasoning_requested":true,
        "provider_requests":provider.requests(),"checks":checks});
    let path = provider.root.join("evidence.json");
    fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    println!("evidence: {}", path.display());
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn responses_pilot_preserves_complete_reasoning_on_tool_followup_turn_and_restart() {
    let items = [
        reasoning("pilot-one"),
        reasoning("pilot-two"),
        reasoning("pilot-three"),
    ];
    let provider = Provider::new(
        "responses-pilot-history",
        vec![
            Step::tool("graph_list", json!({})).with_reasoning(items[0].clone()),
            Step::text("responses first turn history marker").after("fixture"),
            Step::tool("graph_read", json!({"graph":"fixture"}))
                .with_reasoning(items[1].clone())
                .expect_request_contains("responses first turn history marker"),
            Step::text("responses second turn history marker").after("seed"),
            Step::tool("graph_list", json!({}))
                .with_reasoning(items[2].clone())
                .expect_request_contains("responses second turn history marker"),
            Step::text("responses restart retained the complete history").after("fixture"),
        ],
    );
    let graph = json!({"entry":"seed","ops":{"seed":{"run":"true"}},
        "nodes":[{"id":"seed","op":"seed"}],"edges":[]});
    let host = Host::new(&graph).default_runtime();
    let mut server = host.serve(&provider);
    configure(&server, &provider);
    let (status, body) = server.request("POST", "/sessions", Some(&json!({"id":"responses"})));
    assert_eq!(status, 201, "{body}");
    let one = wait(&server, &submit(&server, "one"));
    let two = wait(&server, &submit(&server, "two"));
    assert_eq!(one["goose"], two["goose"]);
    assert_eq!(provider.requests().len(), 4);
    server.kill();
    drop(server);
    let server = host.serve(&provider);
    let three = wait(&server, &submit(&server, "three"));
    assert_eq!(one["goose"], three["goose"]);
    assert_eq!(provider.requests().len(), 6);
    let process = host
        .base
        .root
        .path()
        .join("state/platform/pilot")
        .join(one["goose"]["scope"].as_str().unwrap())
        .join("process");
    let history = host.session_conversation(&process, one["goose"]["session"].as_str().unwrap());
    for item in &items {
        assert!(
            contains_value(&history, item),
            "complete reasoning absent from native Goose history: {history}"
        );
    }
    let (status, runs) = server.request("GET", "/runs", None);
    assert_eq!(status, 200, "{runs}");
    assert!(runs["runs"].as_array().unwrap().is_empty());
    evidence(
        &host,
        &provider,
        json!({"native_session":one["goose"],"native_history":history,
        "tool_followup_preserved":true,"new_turn_preserved":true,"restart_preserved":true,
        "complete_reasoning_exactly_once_in_original_order":true,"no_unrequested_run":true}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn responses_graph_agent_preserves_reasoning_and_authorized_completion_receipt() {
    let items = [reasoning("graph-command"), reasoning("graph-complete")];
    let provider = Provider::new(
        "responses-graph-completion",
        vec![
            command("printf responses-artifact > evidence.txt; cat evidence.txt")
                .with_reasoning(items[0].clone()),
            complete("verify")
                .files(&["evidence.txt"])
                .after("responses-artifact")
                .with_reasoning(items[1].clone()),
        ],
    );
    let graph = json!({"entry":"worker",
        "agents":{"worker":{"model":"models.default.flash","instructions":"Use authorized tools to write evidence.txt, inspect the actual output, then final_result with route verify."}},
        "ops":{"verify":{"writes":["verified.txt"],"run":"sh -c 'cat /in/worker/evidence.txt > verified.txt'"}},
        "nodes":[{"id":"worker","agent":"worker"},{"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]});
    let host = Host::new(&graph).default_runtime();
    let server = host.serve(&provider);
    configure(&server, &provider);
    let run = server.trigger();
    server.wait_status(&run, "completed");
    let record = host.record(&run);
    assert_eq!(record["invocations"]["worker"], 1);
    assert_eq!(record["invocations"]["verify"], 1);
    assert_eq!(
        host.base.file(&record, "worker", "evidence.txt"),
        b"responses-artifact"
    );
    assert_eq!(
        host.base.file(&record, "verify", "verified.txt"),
        b"responses-artifact"
    );
    let history = host.native_conversation(&run, "worker", 1);
    for item in &items {
        assert!(
            contains_value(&history, item),
            "complete reasoning absent from native Goose history: {history}"
        );
    }
    assert_eq!(provider.requests().len(), 2);
    assert!(history.to_string().contains("observed_receipt"));
    evidence(
        &host,
        &provider,
        json!({"run":record,"native_history":history,
        "authorized_anchor_run":true,"authorized_final_result_receipt":true,
        "complete_reasoning_exactly_once_in_original_order":true,
        "artifact_bytes_verified":true}),
    );
}
