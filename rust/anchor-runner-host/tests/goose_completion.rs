//! The completion boundary must work in Goose itself, including direct transport.
#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Host, HttpHost, Provider, Step, command};
use serde_json::{Value, json};

const REMINDER: &str = "Use the work already done and call anchor__final_result alone now";

fn graph() -> Value {
    json!({"objective":"checked completion boundary", "entry":"worker",
        "agents":{"worker":{"model":"fixture-goose","instructions":"Inspect the tools then submit a checked result.","wall_time_limit_seconds":40}},
        "ops":{"done":{"run":"true"}},
        "nodes":[{"id":"worker","agent":"worker"},{"id":"done","op":"done"}],
        "edges":[{"from":"worker","to":"done"}]})
}

fn finish(summary: &str) -> Step {
    Step::tool(
        "final_result",
        json!({"files":[],"summary":summary,"route":"done"}),
    )
}

fn configure_responses(server: &HttpHost, provider: &Provider) {
    let (status, body) = server.request(
        "PUT",
        "/settings/model/providers/default",
        Some(&json!({
            "endpoint":format!("{}/v1",provider.url),"wire_api":"responses",
            "flash":provider.model,"pro":provider.model,
            "api_key":{"action":"set","value":"completion-fixture-only"}
        })),
    );
    assert_eq!(status, 200, "{body}");
}

fn checked(host: &Host, provider: &Provider, run: &str, expected: &str, count: usize) -> Value {
    provider.assert_consumed();
    assert_eq!(provider.requests().len(), count);
    let saved = host.record(run);
    assert_eq!(saved["projection"]["result"]["outcome"], "done", "{saved}");
    assert_eq!(saved["results"]["worker"].as_array().unwrap().len(), 1);
    assert_eq!(saved["results"]["done"].as_array().unwrap().len(), 1);
    let (fact, evidence, _) = host.native_record(run, "worker", 1);
    assert!(fact["completion"].to_string().contains(expected), "{fact}");
    assert_eq!(evidence["prompt_result"]["stopReason"], "end_turn");
    let history = host.native_conversation(run, "worker", 1);
    let blocks: Vec<_> = history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .collect();
    let accepted: Vec<_> = blocks
        .iter()
        .filter(|block| {
            block["type"] == "toolResponse"
                && block["toolResult"]["status"] == "success"
                && block["toolResult"]["value"]["isError"] != true
                && block["toolResult"]["value"]["content"]
                    .to_string()
                    .contains(expected)
        })
        .collect();
    assert_eq!(
        accepted.len(),
        1,
        "native success must be saved exactly once: {history}"
    );
    assert!(blocks.iter().any(|block| block["type"] == "toolRequest"
        && block["id"] == accepted[0]["id"]
        && block["toolCall"]["value"]["name"] == "anchor__final_result"));
    host.evidence(
        provider,
        run,
        json!({"case_source":"tests/goose_completion.rs",
        "expected":expected,"model_requests":count,"final_native_pair_saved":true,
        "no_model_request_after_accepted_completion":true,"downstream_committed_once":true}),
    );
    evidence
}

fn no_followup(rejected: bool) {
    let mut script = Vec::new();
    if rejected {
        script.push(command("printf inspected"));
        script.push(Step::tool(
            "final_result",
            json!({"files":[],"summary":"bad receipt", "route":"done", "observed_receipt":"guessed"}),
        ));
    }
    // Any additional model request is an unscripted fixture error. In particular,
    // Goose cannot ask the model to repeat/rewrite final_result or run more tools.
    script.push(finish("accepted canonical answer"));
    let provider = Provider::new(&format!("completion-no-followup-{rejected}"), script);
    let host = Host::new(&graph()).native();
    let server = host.serve(&provider);
    let run = server.trigger();
    server.wait_status(&run, "completed");
    checked(
        &host,
        &provider,
        &run,
        "accepted canonical answer",
        if rejected { 3 } else { 1 },
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn draft_is_followed_by_completion_in_the_same_invocation() {
    let draft = "## Inspected\nThe workspace check is ready.";
    let provider = Provider::new(
        "completion-after-draft",
        vec![
            command("printf inspected >> inspection.txt; cat inspection.txt"),
            Step::text(draft).after("inspected"),
            finish("submitted existing work")
                .expect_request_contains(REMINDER)
                .after("inspected"),
        ],
    );
    let host = Host::new(&graph()).native();
    let server = host.serve(&provider);
    let run = server.trigger();
    server.wait_status(&run, "completed");
    let evidence = checked(&host, &provider, &run, "submitted existing work", 3);
    let calls = evidence["tool_calls"].as_array().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "the business operation must not be replayed"
    );
    assert_eq!(calls[0]["tool"], "anchor_run");
    assert_eq!(calls[0]["result"]["ok"], true);
    let receipt = &calls[0]["result"]["output"]["anchor_receipt"];
    assert_eq!(receipt.as_str().unwrap().len(), 64);
    assert_eq!(calls[1]["tool"], "final_result");
    assert_eq!(calls[1]["arguments"]["observed_receipt"], *receipt);
    assert_eq!(
        host.base.workspace_files(&run, "inspection.txt"),
        vec![b"inspected".to_vec()]
    );
    let history = host.native_conversation(&run, "worker", 1);
    assert!(
        history.as_array().unwrap().iter().any(|message| {
            message["role"] == "assistant"
                && message["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|block| block["type"] == "text" && block["text"] == draft)
        }),
        "the draft belongs to the same saved session: {history}"
    );
    assert_eq!(host.record(&run)["invocations"]["worker"], 1);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn repeated_drafts_fail_without_blocking_the_next_occurrence() {
    let last_draft = "Another draft without submission.";
    let provider = Provider::new(
        "completion-drafts-exhausted",
        vec![
            Step::text("The first draft is ready."),
            Step::text("The second draft is ready.").expect_request_contains(REMINDER),
            Step::text(last_draft).expect_request_contains(REMINDER),
            finish("next occurrence completed"),
        ],
    );
    let host = Host::new(&graph()).native();
    let now = chrono::Utc::now();
    let at = |seconds| {
        (now - chrono::Duration::seconds(seconds))
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    };
    let first_at = at(60);
    let second_at = at(30);
    // A finite recurring plan is already due twice at startup, without sleeps
    // to cross a real schedule boundary or manipulating an active plan.
    std::fs::write(
        host.base.root.path().join("schedules.json"),
        json!([{"id":"drafts","graph":"fixture","rule":{"type":"interval","seconds":30},
            "next_at":first_at,"created_at":at(90),"enabled":true,"remaining":2}])
        .to_string(),
    )
    .unwrap();
    let server = host.serve(&provider);
    let occurrence = |at: &str| {
        let mut id = None;
        fixture::wait_until(&format!("Run for scheduled occurrence {at}"), || {
            let (status, listed) = server.request("GET", "/runs", None);
            assert_eq!(status, 200, "{listed}");
            let matches: Vec<_> = listed["runs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|run| {
                    run["trigger"]["schedule"] == "drafts" && run["trigger"]["scheduled_at"] == at
                })
                .collect();
            assert!(
                matches.len() <= 1,
                "duplicate admission for {at}: {matches:?}"
            );
            id = matches
                .first()
                .map(|row| row["run"].as_str().unwrap().to_owned());
            id.is_some()
        });
        id.unwrap()
    };
    let failed_run = occurrence(&first_at);
    server.wait_status(&failed_run, "failed");
    let failed = host.record(&failed_run);
    assert!(failed["results"].get("worker").is_none(), "{failed}");
    assert!(failed["results"].get("done").is_none(), "{failed}");
    let (fact, evidence, _) = host.native_record(&failed_run, "worker", 1);
    assert!(fact["completion"].is_null(), "{fact}");
    assert!(
        fact["reason"]
            .as_str()
            .unwrap()
            .contains("final_result not submitted"),
        "{fact}"
    );
    assert_eq!(evidence["prompt_result"]["stopReason"], "end_turn");
    assert_eq!(
        evidence["prompt_result"]["_meta"]["anchorExternalCompletion"],
        "not_submitted"
    );
    assert!(
        host.native_conversation(&failed_run, "worker", 1)
            .to_string()
            .contains(last_draft)
    );
    let next_run = occurrence(&second_at);
    assert_ne!(failed_run, next_run);
    server.wait_status(&next_run, "completed");
    assert_eq!(
        host.record(&next_run)["results"]["done"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    provider.assert_consumed();
    assert_eq!(provider.requests().len(), 4);
    host.evidence(
        &provider,
        &failed_run,
        json!({
            "case_source":"tests/goose_completion.rs", "failed_without_downstream":true,
            "two_reminders":true,"reason":fact["reason"],"next_occurrence":host.record(&next_run)
        }),
    );
}

fn multi_tool_receipt(responses: bool, back: usize) {
    let provider = Provider::new(
        &format!("completion-batch-{responses}-{back}"),
        vec![
            Step::batch(vec![
                (
                    "anchor_run",
                    json!({"command":["sh","-c","printf batch-inspected"]}),
                ),
                ("anchor_plugins", json!({"limit":10})),
            ]),
            finish("batch acknowledged").observing_result(back),
        ],
    );
    let host = Host::new(&graph()).native();
    let server = host.serve(&provider);
    if responses {
        configure_responses(&server, &provider);
    }
    let run = server.trigger();
    server.wait_status(&run, "completed");
    let evidence = checked(&host, &provider, &run, "batch acknowledged", 2);
    let calls = evidence["tool_calls"].as_array().unwrap();
    assert_eq!(
        calls.len(),
        3,
        "both actual tools must precede completion: {calls:?}"
    );
    assert!(calls[..2].iter().all(|call| call["result"]["ok"] == true));
    assert_eq!(calls[2]["tool"], "final_result");
    assert_eq!(calls[2]["result"]["ok"], true);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn a_receipt_from_an_older_batch_is_rejected_then_correctable() {
    let provider = Provider::new(
        "completion-stale-batch",
        vec![
            command("printf earlier-batch"),
            command("printf latest-batch"),
            finish("stale must not commit").observing_result(1),
            finish("latest batch acknowledged"),
        ],
    );
    let host = Host::new(&graph()).native();
    let server = host.serve(&provider);
    let run = server.trigger();
    server.wait_status(&run, "completed");
    let evidence = checked(&host, &provider, &run, "latest batch acknowledged", 4);
    assert_eq!(evidence["tool_calls"][2]["result"]["ok"], false);
}

fn mixed_completion(duplicate: bool) {
    let extra = if duplicate {
        (
            "final_result",
            json!({"files":[],"summary":"conflicting candidate","route":"done"}),
        )
    } else {
        (
            "anchor_run",
            json!({"command":["sh","-c","printf mixed-batch-inspected"]}),
        )
    };
    let provider = Provider::new(
        &format!("completion-mixed-{duplicate}"),
        vec![
            command("printf previous-inspected"),
            Step::batch(vec![
                (
                    "final_result",
                    json!({"files":[],"summary":"mixed candidate","route":"done"}),
                ),
                extra,
            ]),
            finish("standalone accepted answer"),
        ],
    );
    let host = Host::new(&graph()).native();
    let server = host.serve(&provider);
    let run = server.trigger();
    server.wait_status(&run, "completed");
    let evidence = checked(&host, &provider, &run, "standalone accepted answer", 3);
    assert!(
        host.native_conversation(&run, "worker", 1)
            .to_string()
            .contains(
                "External completion tool must be called alone after observing business results"
            )
    );
    let calls = evidence["tool_calls"].as_array().unwrap();
    let finals: Vec<_> = calls
        .iter()
        .filter(|call| call["tool"] == "final_result")
        .collect();
    assert_eq!(
        finals
            .iter()
            .filter(|call| call["result"]["ok"] == true)
            .count(),
        1
    );
    assert_eq!(
        finals.last().unwrap()["arguments"]["summary"],
        "standalone accepted answer"
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn completion_waits_for_declared_outputs_then_is_correctable() {
    // blind-03: the reviewer wrote review/review.json, finished, and its
    // downstream read nothing. The refusal comes while the model can still act.
    let mut graph = graph();
    graph["agents"]["worker"]["writes"] = json!(["review.json"]);
    graph["ops"]["done"]["reads"] = json!(["review.json"]);
    let provider = Provider::new(
        "completion-declared-outputs",
        vec![
            command("mkdir -p review && printf '{}' > review/review.json"),
            finish("nested output must not commit").files(&["review.json"]),
            command("printf '{}' > review.json"),
            finish("declared output written").files(&["review.json"]),
        ],
    );
    let host = Host::new(&graph).native();
    let server = host.serve(&provider);
    let run = server.trigger();
    server.wait_status(&run, "completed");
    let evidence = checked(&host, &provider, &run, "declared output written", 4);
    let calls = evidence["tool_calls"].as_array().unwrap();
    assert_eq!(calls[1]["tool"], "final_result");
    assert_eq!(calls[1]["result"]["ok"], false);
    assert!(
        calls[1]["result"]
            .to_string()
            .contains("selected delivery path does not exist: review.json"),
        "{}",
        calls[1]
    );
}

// Each scenario has its own evidence record in the standard regression runner.

#[test]
#[ignore = "requires pinned Goose binary"]
fn accepted_completion_finishes_without_another_model_request() {
    no_followup(false);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn rejected_then_corrected_completion_finishes_without_another_model_request() {
    no_followup(true);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn chat_batch_can_confirm_first_result() {
    multi_tool_receipt(false, 1);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn chat_batch_can_confirm_last_result() {
    multi_tool_receipt(false, 0);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn responses_batch_can_confirm_first_result() {
    multi_tool_receipt(true, 1);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn responses_batch_can_confirm_last_result() {
    multi_tool_receipt(true, 0);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn mixed_business_and_completion_is_rejected() {
    mixed_completion(false);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn duplicate_completion_batch_is_rejected() {
    mixed_completion(true);
}
