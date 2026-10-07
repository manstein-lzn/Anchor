#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Gate, Host, HttpHost, Provider, Step, command, complete};
use serde_json::{Value, json};

const CASE_SOURCE: &str = "tests/goose_trace.rs";
const DRAFT: &str = "Uncommitted assistant text, not the final user reply.";

fn graph() -> Value {
    json!({
        "objective":"Inspect native live trace without treating drafts as completion",
        "entry":"worker",
        "agents":{"worker":{"model":"models.worker","instructions":"Inspect workspace facts before submitting final_result with route verify. Ordinary text is not completion."}},
        "ops":{"verify":{"run":"sh -c 'set -eu; test \"$(cat /in/worker/effect.txt)\" = once; cat /in/worker/effect.txt > verified.txt'"}},
        "nodes":[{"id":"worker","agent":"worker"},{"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    })
}

fn trace(detail: &Value) -> &Vec<Value> {
    detail["traces"]["[\"worker\",1]"]
        .as_array()
        .expect("native trace was not exposed")
}

fn assert_live(server: &HttpHost, run: &str) -> Value {
    let (status, detail) = server.request("GET", &format!("/runs/{run}"), None);
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["state"]["status"], "running");
    assert_eq!(detail["active"], true);
    let messages = trace(&detail);
    assert!(messages.iter().any(|message| message["text"] == DRAFT));
    let request = messages
        .iter()
        .find(|message| {
            message["commands"].as_array().is_some_and(|commands| {
                commands
                    .iter()
                    .any(|command| command.as_str().unwrap().contains("effect.txt"))
            })
        })
        .unwrap();
    let result = messages
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    assert_eq!(request["tool_call_id"], result["tool_call_id"]);
    assert_eq!(request["status"], "completed");
    assert!(result["text"].as_str().unwrap().contains("once"));
    assert!(detail["state"]["executed"].as_array().unwrap().is_empty());
    detail
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn live_trace_precedes_completion_and_final_artifacts_remain_authoritative() {
    let gate = Gate::new();
    let provider = Provider::new(
        "goose-live-trace",
        vec![
            command("set -eu; test ! -e effect.txt; printf once > effect.txt; cat effect.txt")
                .with_preface(DRAFT),
            complete("verify").gated(&gate),
            Step::text("Node summary was submitted."),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let server = host.serve(&provider);
    let run = server.trigger();
    gate.wait_entered();
    let during = assert_live(&server, &run);
    assert!(host.native_fact(&run, "worker", 1)["completion"].is_null());
    let native = host.native_conversation(&run, "worker", 1);
    assert!(native.to_string().contains("effect.txt"));
    gate.open();
    server.wait_status(&run, "completed");
    let (status, after) = server.request("GET", &format!("/runs/{run}"), None);
    assert_eq!(status, 200);
    assert!(trace(&after).iter().any(|message| message["text"] == DRAFT));
    let record = host.record(&run);
    assert_eq!(
        record["results"]["worker"][0]["completion"]["submission"],
        "fixture complete"
    );
    assert_eq!(host.base.file(&record, "worker", "effect.txt"), b"once");
    assert_eq!(host.base.file(&record, "verify", "verified.txt"), b"once");
    host.evidence(&provider, &run, json!({"case_source":CASE_SOURCE,"live_detail":during,"settled_detail":after,"draft_is_not_completion":true,"artifact_matches":true}));
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn stopped_live_trace_survives_restart_without_replaying_the_effect() {
    let gate = Gate::new();
    let provider = Provider::new(
        "goose-live-trace-stop-restart",
        vec![
            command("set -eu; test ! -e effect.txt; printf once > effect.txt; cat effect.txt")
                .with_preface(DRAFT),
            complete("verify").gated(&gate),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let mut server = host.serve(&provider);
    let run = server.trigger();
    gate.wait_entered();
    let during = assert_live(&server, &run);
    let session = host.native_fact(&run, "worker", 1)["session_id"].clone();
    let (status, response) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{response}");
    server.wait_status(&run, "stopped");
    gate.open();
    let (status, stopped) = server.request("GET", &format!("/runs/{run}"), None);
    assert_eq!(status, 200);
    assert!(
        trace(&stopped)
            .iter()
            .any(|message| message["text"] == DRAFT)
    );
    assert!(host.native_fact(&run, "worker", 1)["completion"].is_null());
    server.kill();
    drop(server);
    provider.append(vec![
        command("set -eu; test \"$(cat effect.txt)\" = once; cat effect.txt").after("once"),
        complete("verify").after("once"),
        Step::text("Node summary was submitted after inspection."),
    ]);
    let restarted = host.serve(&provider);
    let (status, detail) = restarted.request("GET", &format!("/runs/{run}"), None);
    assert_eq!(status, 200);
    assert!(
        trace(&detail)
            .iter()
            .any(|message| message["text"] == DRAFT)
    );
    let (status, resumed) = restarted.request("POST", &format!("/runs/{run}/resume"), None);
    assert_eq!(status, 202, "{resumed}");
    restarted.wait_status(&run, "completed");
    assert_eq!(host.native_fact(&run, "worker", 1)["session_id"], session);
    let record = host.record(&run);
    assert_eq!(host.base.file(&record, "worker", "effect.txt"), b"once");
    assert_eq!(host.base.file(&record, "verify", "verified.txt"), b"once");
    let history = host.native_conversation(&run, "worker", 1);
    let writes = history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter(|content| {
            content["type"] == "toolRequest"
                && content["toolCall"]
                    .to_string()
                    .contains("printf once > effect.txt")
        })
        .count();
    assert_eq!(writes, 1);
    assert_eq!(provider.requests().len(), 5);
    host.evidence(&provider, &run, json!({"case_source":CASE_SOURCE,"live_detail":during,"stopped_detail":stopped,"restart_detail":detail,"same_native_session":true,"effect_once":true}));
}
