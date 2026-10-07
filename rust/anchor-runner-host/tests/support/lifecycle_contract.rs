use crate::fixture::{Gate, Host, Provider, Reply, command, complete, evidence_run, wait_until};
use serde_json::{Value, json};
use std::fs;

fn two_agents() -> Value {
    json!({
        "entry":"first","agents":{
            "first":{"model":"models.worker","instructions":"write once","wall_time_limit_seconds":30},
            "second":{"model":"models.left","instructions":"copy committed input","wall_time_limit_seconds":30}
        },
        "nodes":[{"id":"first","agent":"first"},{"id":"second","agent":"second"}],
        "edges":[{"from":"first","to":"second"}]
    })
}

#[test]
fn pause_restart_resume_keeps_frozen_graph_and_committed_node() {
    let checkpoint = Gate::new();
    let provider = Provider::new([
        (
            "fixture-worker",
            vec![
                command("printf once >> effects.txt"),
                Reply::Gated(checkpoint.clone(), Box::new(complete(Some("second")))),
            ],
        ),
        (
            "fixture-left",
            vec![
                command("cat /in/first/effects.txt > copied.txt"),
                complete(None),
            ],
        ),
    ]);
    let host = Host::new(&two_agents());
    let server = host.serve(&provider);
    let run = server.trigger();
    checkpoint.wait_entered();
    assert_eq!(
        server
            .request("POST", &format!("/runs/{run}/pause"), None)
            .0,
        202
    );
    let pending = server.request("GET", &format!("/runs/{run}"), None).1;
    assert_eq!(pending["control_requested"], "pause");
    assert_eq!(pending["active"], true);
    checkpoint.open();
    server.wait_status(&run, "paused");
    let paused = host.record_for(&run);
    assert_eq!(host.file(&paused, "first", "effects.txt"), b"once");
    assert!(paused["results"].get("second").is_none());
    let (status, _) = server.request(
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":{
            "entry":"broken","ops":{"broken":{"run":"sh -c 'exit 9'"}},
            "nodes":[{"id":"broken","op":"broken"}],"edges":[]
        }})),
    );
    assert_eq!(status, 200);
    drop(server);
    let restarted = host.serve(&provider);
    assert_eq!(host.record_for(&run), paused);
    assert_eq!(
        restarted
            .request("POST", &format!("/runs/{run}/resume"), None)
            .0,
        202
    );
    restarted.wait_status(&run, "completed");
    let completed = host.record_for(&run);
    assert_eq!(completed["results"]["first"], paused["results"]["first"]);
    assert_eq!(host.file(&completed, "second", "copied.txt"), b"once");
    assert_eq!(provider.requests().len(), 4);
    assert_eq!(
        provider
            .requests()
            .iter()
            .filter(|request| request["model"] == "fixture-worker")
            .count(),
        2
    );
    provider.assert_consumed();
    evidence_run(
        "pause-restart-frozen-resume",
        &host,
        &provider,
        &run,
        json!({"process_restart":true,"same_run":true,"frozen_definition":true,"completed_node_replays":0}),
    );
}

#[test]
fn killed_parallel_host_resumes_unfinished_agent_without_replaying_completed_branch() {
    let checkpoint = Gate::new();
    let provider = Provider::new([
        (
            "fixture-left",
            vec![
                command("printf L > result.txt; printf once >> effects.txt"),
                complete(Some("join")),
            ],
        ),
        (
            "fixture-right",
            vec![
                command("printf R > result.txt; printf once >> effects.txt"),
                Reply::Gated(checkpoint.clone(), Box::new(complete(Some("join")))),
                complete(Some("join")),
            ],
        ),
    ]);
    let host = Host::new(&json!({
        "entry":"fork","agents":{
            "left":{"model":"models.left","instructions":"left","wall_time_limit_seconds":30},
            "right":{"model":"models.right","instructions":"right","wall_time_limit_seconds":30}
        },
        "ops":{"fork":{"fanout":{"join":"join"}},"join":{"join":{}},"verify":{"run":"sh -c 'cat /in/left/result.txt /in/right/result.txt > merged.txt'"}},
        "nodes":[{"id":"fork","op":"fork"},{"id":"left","agent":"left"},{"id":"right","agent":"right"},{"id":"join","op":"join"},{"id":"verify","op":"verify"}],
        "edges":[{"from":"fork","to":"left"},{"from":"fork","to":"right"},{"from":"left","to":"join"},{"from":"right","to":"join"},{"from":"join","to":"verify"}]
    }));
    let server = host.serve(&provider);
    let run = server.trigger();
    checkpoint.wait_entered();
    wait_until("completed left branch", || {
        host.record_for(&run)["results"].get("left").is_some()
    });
    let before = host.record_for(&run);
    assert!(before["results"].get("right").is_none());
    drop(server);
    checkpoint.open();
    let restarted = host.serve(&provider);
    assert_eq!(
        restarted
            .request("POST", &format!("/runs/{run}/resume"), None)
            .0,
        202
    );
    restarted.wait_status(&run, "completed");
    let after = host.record_for(&run);
    assert_eq!(after["results"]["left"], before["results"]["left"]);
    assert_eq!(host.file(&after, "left", "effects.txt"), b"once");
    assert_eq!(host.file(&after, "right", "effects.txt"), b"once");
    assert_eq!(host.file(&after, "verify", "merged.txt"), b"LR");
    assert_eq!(after["results"]["right"][0]["key"]["invocation"], 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["model"] == "fixture-left")
            .count(),
        2
    );
    let right = requests
        .iter()
        .filter(|request| request["model"] == "fixture-right")
        .collect::<Vec<_>>();
    assert!(right[2]["messages"].to_string().contains("exit_code"));
    provider.assert_consumed();
    evidence_run(
        "parallel-midflight-restart",
        &host,
        &provider,
        &run,
        json!({"process_killed":true,"same_run":true,"completed_branch_replays":0,"tool_effects_per_branch":1,"provider_requests":5}),
    );
}

#[test]
fn stop_request_settles_before_restart_without_starting_downstream() {
    let checkpoint = Gate::new();
    let provider = Provider::new([(
        "fixture-worker",
        vec![
            command("printf once >> effects.txt"),
            Reply::Gated(checkpoint.clone(), Box::new(complete(Some("second")))),
        ],
    )]);
    let host = Host::new(&two_agents());
    let server = host.serve(&provider);
    let run = server.trigger();
    checkpoint.wait_entered();
    assert_eq!(
        server.request("POST", &format!("/runs/{run}/stop"), None).0,
        202
    );
    checkpoint.open();
    server.wait_status(&run, "stopped");
    let stopped = host.record_for(&run);
    assert!(stopped["results"].get("second").is_none());
    assert_eq!(
        host.workspace_files(&run, "effects.txt"),
        vec![b"once".to_vec()]
    );
    drop(server);
    let restarted = host.serve(&provider);
    restarted.wait_status(&run, "stopped");
    assert_eq!(host.record_for(&run), stopped);
    assert_eq!(provider.requests().len(), 2);
    provider.assert_consumed();
    evidence_run(
        "stop-settlement-restart",
        &host,
        &provider,
        &run,
        json!({"stop_settled":true,"downstream_started":false,"tool_effects":1,"restart_replays":0}),
    );
}

#[test]
fn graph_call_wait_and_detach_keep_child_identity_and_tool_effect_once() {
    for mode in ["wait", "detach"] {
        let provider = Provider::new([(
            "fixture-left",
            vec![
                command("printf child > report.txt; printf once >> effects.txt"),
                complete(None),
            ],
        )]);
        let mut call = json!({"graph":"child","mode":mode,"input":{"source":"parent"}});
        if mode == "wait" {
            call["result"] = json!({"node":"worker","files":["report.txt"]});
        }
        let host = Host::new(&json!({
            "entry":"invoke","ops":{"invoke":{"call":call}},
            "nodes":[{"id":"invoke","op":"invoke"}],"edges":[]
        }));
        let child_bundle = host.root.path().join("child");
        fs::create_dir(&child_bundle).unwrap();
        fs::write(child_bundle.join("graph.json"), json!({
            "entry":"worker","agents":{"worker":{"model":"models.left","instructions":"write child report","wall_time_limit_seconds":30}},
            "nodes":[{"id":"worker","agent":"worker"}],"edges":[]
        }).to_string()).unwrap();
        fs::write(
            child_bundle.join("manifest.json"),
            r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
        )
        .unwrap();
        let server = host.serve(&provider);
        let parent = server.trigger();
        let detail = server.wait_status(&parent, "completed");
        let calls = detail["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["mode"], mode);
        let child = calls[0]["run"].as_str().unwrap();
        server.wait_status(child, "completed");
        let saved_child = host.record_for(child);
        assert_eq!(saved_child["input"], json!({"source":"parent"}));
        assert_eq!(host.file(&saved_child, "worker", "report.txt"), b"child");
        assert_eq!(host.file(&saved_child, "worker", "effects.txt"), b"once");
        if mode == "wait" {
            assert_eq!(
                host.file(&host.record_for(&parent), "invoke", "result/report.txt"),
                b"child"
            );
        }
        drop(server);
        let restarted = host.serve(&provider);
        let after = restarted.wait_status(&parent, "completed");
        assert_eq!(after["calls"][0]["run"], child);
        assert_eq!(host.record_for(child), saved_child);
        assert_eq!(provider.requests().len(), 2);
        provider.assert_consumed();
        evidence_run(
            &format!("graph-call-{mode}-restart"),
            &host,
            &provider,
            &parent,
            json!({"mode":mode,"child":saved_child,"child_tool_effects":1,"restart_replays":0}),
        );
    }
}
