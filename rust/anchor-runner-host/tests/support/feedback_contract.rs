use crate::fixture::{Host, Provider, Reply, command, complete, evidence, read_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

const REWORK_SUMMARY: &str = "feedback-contract: revise the draft with one concrete example";

fn worker() -> Value {
    json!({
        "model":"models.worker",
        "instructions":"deterministic feedback fixture",
        "network":false,
        "wall_time_limit_seconds":30
    })
}

fn terminal_graph() -> Value {
    json!({
        "entry":"worker","agents":{"worker":worker()},
        "nodes":[{"id":"worker","agent":"worker"}],"edges":[]
    })
}

fn artifact_path(host: &Host, result: &Value) -> PathBuf {
    host.root
        .path()
        .join("state/artifacts")
        .join(result["commit"]["id"].as_str().unwrap())
}

fn assert_artifact(host: &Host, result: &Value, files: &[(&str, &[u8])]) -> Value {
    let path = artifact_path(host, result);
    let manifest = read_json(path.join("manifest.json"));
    assert_eq!(manifest["key"], result["key"]);
    assert_eq!(manifest["completion"], result["completion"]);
    for (name, expected) in files {
        assert_eq!(fs::read(path.join("files").join(name)).unwrap(), *expected);
        assert_eq!(
            manifest["files"][*name]["sha256"],
            format!("{:x}", Sha256::digest(expected))
        );
        assert_eq!(manifest["files"][*name]["bytes"], expected.len());
    }
    manifest
}

fn user_message_contains(request: &Value, text: &str) -> bool {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "user" && message["content"].to_string().contains(text))
}

#[test]
fn reviewer_requests_one_rework_with_inherited_workspaces_and_immutable_artifacts() {
    let provider = Provider::new([(
        "fixture-worker",
        vec![
            command(
                "set -eu; test ! -e draft.txt; test ! -e notes.txt; test ! -e effects.txt; \
                 printf 'draft-v1\n' > draft.txt; printf 'retained evidence\n' > notes.txt; \
                 printf 'writer-1\n' >> effects.txt",
            ),
            complete(Some("reviewer")),
            command(
                "set -eu; test ! -e review.txt; test ! -e draft.txt; test ! -e effects.txt; \
                 test \"$(cat /in/writer/draft.txt)\" = draft-v1; \
                 printf 'add one concrete example\n' > review.txt; \
                 printf 'reviewer-1\n' >> effects.txt",
            ),
            Reply::Tool(
                "final_result",
                json!({"summary":REWORK_SUMMARY,"route":"writer"}),
            ),
            command(
                "set -eu; test \"$(cat draft.txt)\" = draft-v1; \
                 test \"$(cat notes.txt)\" = 'retained evidence'; \
                 test \"$(cat effects.txt)\" = writer-1; test ! -e review.txt; \
                 test \"$(cat /in/reviewer/review.txt)\" = 'add one concrete example'; \
                 printf 'draft-v2 with one concrete example\n' > draft.txt; \
                 printf 'writer-2\n' >> effects.txt",
            ),
            complete(Some("reviewer")),
            command(
                "set -eu; test \"$(cat review.txt)\" = 'add one concrete example'; \
                 test \"$(cat effects.txt)\" = reviewer-1; test ! -e draft.txt; \
                 test \"$(cat /in/writer/draft.txt)\" = 'draft-v2 with one concrete example'; \
                 printf 'approved\n' > review.txt; printf 'reviewer-2\n' >> effects.txt",
            ),
            complete(Some("done")),
        ],
    )]);
    let host = Host::new(&json!({
        "objective":"revise once, then approve","entry":"writer","max_rounds":2,
        "agents":{"worker":worker()},
        "ops":{"finish":{"run":"sh -c 'set -eu; cat /in/writer/draft.txt > final.txt'"}},
        "nodes":[
            {"id":"writer","agent":"worker"},
            {"id":"reviewer","agent":"worker"},
            {"id":"done","op":"finish"}
        ],
        "edges":[
            {"from":"writer","to":"reviewer"},
            {"from":"reviewer","to":"writer"},
            {"from":"reviewer","to":"done"}
        ]
    }));
    let response = host.run(&provider);
    assert_eq!(response["status"], "completed", "{response}");
    let saved = host.record();
    assert_eq!(
        saved["invocations"],
        json!({"writer":2,"reviewer":2,"done":1})
    );
    assert_eq!(saved["results"].as_object().unwrap().len(), 3);
    for (node, count) in [("writer", 2), ("reviewer", 2), ("done", 1)] {
        let results = saved["results"][node].as_array().unwrap();
        assert_eq!(results.len(), count);
        for (index, result) in results.iter().enumerate() {
            assert_eq!(result["key"]["node_id"], node);
            assert_eq!(result["key"]["run_id"], saved["run_id"]);
            assert_eq!(result["key"]["invocation"], index + 1);
            assert_eq!(result["commit"]["node_id"], node);
            assert_eq!(result["commit"]["invocation"], index + 1);
            if node != "done" {
                assert_eq!(result["completion"]["model_requests"], 2);
            }
        }
    }
    let first_writer = &saved["results"]["writer"][0];
    let second_writer = &saved["results"]["writer"][1];
    let first_review = &saved["results"]["reviewer"][0];
    let second_review = &saved["results"]["reviewer"][1];
    assert_eq!(first_writer["completion"]["route"], "reviewer");
    assert_eq!(second_writer["completion"]["route"], "reviewer");
    assert_eq!(first_review["completion"]["route"], "writer");
    assert_eq!(first_review["completion"]["submission"], REWORK_SUMMARY);
    assert_eq!(second_review["completion"]["route"], "done");
    assert_ne!(first_writer["commit"]["id"], second_writer["commit"]["id"]);
    assert_ne!(first_review["commit"]["id"], second_review["commit"]["id"]);
    let first_writer_manifest = assert_artifact(
        &host,
        first_writer,
        &[
            ("draft.txt", b"draft-v1\n"),
            ("notes.txt", b"retained evidence\n"),
            ("effects.txt", b"writer-1\n"),
        ],
    );
    let first_review_manifest = assert_artifact(
        &host,
        first_review,
        &[
            ("review.txt", b"add one concrete example\n"),
            ("effects.txt", b"reviewer-1\n"),
        ],
    );
    let second_writer_manifest = assert_artifact(
        &host,
        second_writer,
        &[
            ("draft.txt", b"draft-v2 with one concrete example\n"),
            ("notes.txt", b"retained evidence\n"),
            ("effects.txt", b"writer-1\nwriter-2\n"),
        ],
    );
    let second_review_manifest = assert_artifact(
        &host,
        second_review,
        &[
            ("review.txt", b"approved\n"),
            ("effects.txt", b"reviewer-1\nreviewer-2\n"),
        ],
    );
    assert_eq!(first_writer_manifest["context"]["input_commits"], json!([]));
    assert_eq!(
        first_review_manifest["context"]["input_commits"],
        json!([first_writer["commit"]])
    );
    assert_eq!(
        second_writer_manifest["context"]["input_commits"],
        json!([first_review["commit"]])
    );
    assert_eq!(
        second_review_manifest["context"]["input_commits"],
        json!([second_writer["commit"]])
    );
    assert_artifact(
        &host,
        &saved["results"]["done"][0],
        &[("final.txt", b"draft-v2 with one concrete example\n")],
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 8);
    for request in &requests {
        assert_eq!(request["model"], "fixture-worker");
    }
    assert!(!user_message_contains(&requests[0], REWORK_SUMMARY));
    assert!(
        user_message_contains(&requests[4], REWORK_SUMMARY),
        "{}",
        requests[4]
    );
    provider.assert_consumed();
    evidence(
        "feedback-rework-once",
        &host,
        &provider,
        json!({
            "invocations":{"writer":2,"reviewer":2,"done":1},
            "provider_requests":8,"tool_effects":{"writer":2,"reviewer":2},
            "workspace_inheritance":true,"immutable_artifact_versions":true,
            "exact_input_lineage":true,"feedback_in_provider_request":true
        }),
    );
}

#[test]
fn independent_host_roots_do_not_inherit_another_runs_workspace_or_provider_context() {
    let graph = terminal_graph();
    let first = Host::new(&graph);
    let second = Host::new(&graph);
    assert_ne!(first.root.path(), second.root.path());
    let first_provider = Provider::new([(
        "fixture-worker",
        vec![
            command(
                "set -eu; test ! -e marker.txt; test ! -e effects.txt; \
                 test ! -e /in/worker/marker.txt; \
                 printf 'first-host-only\n' > marker.txt; printf 'once\n' >> effects.txt",
            ),
            complete(None),
        ],
    )]);
    let second_provider = Provider::new([(
        "fixture-worker",
        vec![
            command(
                "set -eu; test ! -e marker.txt; test ! -e effects.txt; \
                 test ! -e /in/worker/marker.txt; \
                 printf 'second-host-only\n' > marker.txt; printf 'once\n' >> effects.txt",
            ),
            complete(None),
        ],
    )]);
    let first_response = first.run(&first_provider);
    assert_eq!(first_response["status"], "completed", "{first_response}");
    let first_saved = first.record();
    let first_requests = first_provider.requests();
    let first_manifest = assert_artifact(
        &first,
        &first_saved["results"]["worker"][0],
        &[
            ("marker.txt", b"first-host-only\n"),
            ("effects.txt", b"once\n"),
        ],
    );
    let second_response = second.run(&second_provider);
    assert_eq!(second_response["status"], "completed", "{second_response}");
    let second_saved = second.record();
    for saved in [&first_saved, &second_saved] {
        assert_eq!(saved["invocations"], json!({"worker":1}));
        assert_eq!(saved["results"]["worker"].as_array().unwrap().len(), 1);
        assert_eq!(saved["results"]["worker"][0]["key"]["invocation"], 1);
        assert_eq!(
            saved["results"]["worker"][0]["completion"]["model_requests"],
            2
        );
    }
    assert_eq!(
        first_saved["results"]["worker"][0]["key"],
        second_saved["results"]["worker"][0]["key"]
    );
    assert_ne!(
        first.artifact(&first_saved, "worker"),
        second.artifact(&second_saved, "worker")
    );
    assert_eq!(first.record(), first_saved);
    assert_eq!(
        assert_artifact(
            &first,
            &first_saved["results"]["worker"][0],
            &[
                ("marker.txt", b"first-host-only\n"),
                ("effects.txt", b"once\n")
            ],
        ),
        first_manifest
    );
    let second_manifest = assert_artifact(
        &second,
        &second_saved["results"]["worker"][0],
        &[
            ("marker.txt", b"second-host-only\n"),
            ("effects.txt", b"once\n"),
        ],
    );
    assert_eq!(first_manifest["context"]["input_commits"], json!([]));
    assert_eq!(second_manifest["context"]["input_commits"], json!([]));
    let second_requests = second_provider.requests();
    assert_eq!(first_requests.len(), 2);
    assert_eq!(second_requests.len(), 2);
    assert_eq!(first_provider.requests(), first_requests);
    for request in &first_requests {
        assert_eq!(request["model"], "fixture-worker");
        assert!(!request["messages"].to_string().contains("second-host-only"));
    }
    for request in &second_requests {
        assert_eq!(request["model"], "fixture-worker");
        assert!(!request["messages"].to_string().contains("first-host-only"));
    }
    first_provider.assert_consumed();
    second_provider.assert_consumed();
    evidence(
        "feedback-cross-run-isolation",
        &second,
        &second_provider,
        json!({
            "distinct_host_roots":true,"same_invocation_key":true,
            "first_run":first_saved,"first_provider_requests":first_requests,
            "provider_requests_per_host":2,"tool_effects_per_host":1,
            "first_artifact_unchanged":true,"cross_run_workspace_inheritance":false,
            "cross_run_provider_context":false
        }),
    );
}

#[test]
fn missing_completion_summary_is_corrected_without_repeating_tool_effects() {
    let provider = Provider::new([(
        "fixture-worker",
        vec![
            command("set -eu; test ! -e effects.txt; printf 'once\n' >> effects.txt"),
            Reply::Tool("final_result", json!({"route":null})),
            complete(None),
        ],
    )]);
    let host = Host::new(&terminal_graph());
    let response = host.run(&provider);
    assert_eq!(response["status"], "completed", "{response}");
    let saved = host.record();
    assert_eq!(saved["invocations"], json!({"worker":1}));
    assert_eq!(saved["results"]["worker"].as_array().unwrap().len(), 1);
    let result = &saved["results"]["worker"][0];
    assert_eq!(result["key"]["invocation"], 1);
    assert_eq!(result["completion"]["submission"], "fixture completed");
    assert_eq!(result["completion"]["route"], Value::Null);
    assert_eq!(result["completion"]["model_requests"], 3);
    assert_artifact(&host, result, &[("effects.txt", b"once\n")]);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request["model"], "fixture-worker");
    }
    let completion_tool = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "final_result")
        .unwrap();
    assert!(
        completion_tool["function"]["parameters"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "summary")
    );
    assert!(
        user_message_contains(&requests[2], "output shape"),
        "{}",
        requests[2]
    );
    assert!(
        user_message_contains(&requests[2], "summary"),
        "{}",
        requests[2]
    );
    provider.assert_consumed();
    evidence(
        "feedback-missing-summary-correction",
        &host,
        &provider,
        json!({"missing_summary_corrections":1,"invocations":1,"provider_requests":3,"tool_effects":1}),
    );
}
