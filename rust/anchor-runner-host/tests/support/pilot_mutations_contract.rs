use super::{fixture, wait_turn};
use fixture::{Gate, Host, Provider, Reply, command, complete};
use serde_json::{Value, json};

fn turn(server: &fixture::HttpHost, request: &str, prompt: &str) -> Value {
    let (status, accepted) = server.request(
        "POST",
        "/sessions/pilot-write/turns",
        Some(&json!({"request_id":request,"message":prompt})),
    );
    assert_eq!(status, 202, "{accepted}");
    wait_turn(
        server,
        "pilot-write",
        accepted["turn"]["id"].as_str().unwrap(),
        "completed",
    )
}

fn session(server: &fixture::HttpHost) {
    let (status, result) = server.request("POST", "/sessions", Some(&json!({"id":"pilot-write"})));
    assert_eq!(status, 201, "{result}");
}

fn wait_run(host: &Host, run: &str, expected: &str) -> Value {
    let mut record = Value::Null;
    fixture::wait_until(expected, || {
        record = host.record_for(run);
        record["status"] == expected
    });
    record
}

#[test]
fn native_pilot_creates_updates_starts_small_graph_and_associates_facts() {
    let original = json!({"objective":"saved-only","entry":"idle","ops":{"idle":{"run":"true"}},
        "nodes":[{"id":"idle","op":"idle"}],"edges":[]});
    let updated = json!({"objective":"small deterministic Pilot Graph","entry":"seed",
        "agents":{"finish":{"model":"models.worker","instructions":"fixture finish","wall_time_limit_seconds":30}},
        "ops":{"seed":{"run":"sh -c 'printf fixture-seed > seed.txt'"}},
        "nodes":[{"id":"seed","op":"seed"},{"id":"finish","agent":"finish"}],
        "edges":[{"from":"seed","to":"finish"}]});
    let provider = Provider::new([
        (
            "fixture-default",
            vec![
                Reply::Tool(
                    "graph_create",
                    json!({"name":"pilot-created","definition":original}),
                ),
                Reply::Tool(
                    "graph_update",
                    json!({"graph":"pilot-created","definition":updated}),
                ),
                Reply::Tool(
                    "graph_run",
                    json!({"graph":"pilot-created","input":{"marker":"small-fixture"}}),
                ),
                Reply::Text(
                    "Run accepted. Inspect its artifacts, not an assumed completion.".into(),
                ),
            ],
        ),
        (
            "fixture-worker",
            vec![
                command("cat /in/seed/seed.txt > receipt.txt; printf once > effects.txt"),
                complete(None),
            ],
        ),
    ]);
    let host = Host::new(&original);
    let server = host.serve(&provider);
    session(&server);
    let terminal = turn(
        &server,
        "create-update-run",
        "Create and update pilot-created, then run the small fixture once.",
    );
    let run = terminal["runs"][0].as_str().unwrap();
    assert_eq!(terminal["runs"].as_array().unwrap().len(), 1);
    let native = &terminal["native"];
    assert_eq!(native["scope"].as_str().unwrap().len(), 64);
    assert!(native["session"].as_i64().unwrap() > 0 && native["run"].as_i64().unwrap() > 0);
    let record = wait_run(&host, run, "completed");
    assert_eq!(host.file(&record, "finish", "receipt.txt"), b"fixture-seed");
    assert_eq!(host.file(&record, "finish", "effects.txt"), b"once");
    assert_eq!(
        host.workspace_files(run, "effects.txt"),
        vec![b"once".to_vec()]
    );
    assert!(!host.history(run, "finish", 1).is_empty());
    let metadata = fixture::read_json(
        host.root
            .path()
            .join("state/run-metadata")
            .join(format!("{run}.json")),
    );
    assert_eq!(metadata["pilot"]["session"], "pilot-write");
    assert_eq!(metadata["pilot"]["turn"], terminal["id"]);
    assert_eq!(metadata["pilot"]["owner"], "local");
    let (_, linked) = server.request("GET", "/sessions/pilot-write", None);
    assert_eq!(linked["session"]["run_ids"], terminal["runs"]);
    let (_, saved) = server.request("GET", "/graphs/pilot-created", None);
    assert_eq!(saved["definition"], updated);
    let requests = provider.requests().len();
    let (status, duplicate) = server.request("POST", "/sessions/pilot-write/turns", Some(&json!({
        "request_id":"create-update-run","message":"Create and update pilot-created, then run the small fixture once."
    })));
    assert_eq!(status, 202, "{duplicate}");
    assert_eq!(duplicate["turn"], terminal);
    assert_eq!(provider.requests().len(), requests);
    drop(server);
    let server = host.serve(&provider);
    assert_eq!(
        server
            .request(
                "GET",
                &format!(
                    "/sessions/pilot-write/turns/{}",
                    terminal["id"].as_str().unwrap()
                ),
                None
            )
            .1["turn"],
        terminal
    );
    assert_eq!(provider.requests().len(), requests);
    assert_eq!(host.record_for(run), record);
    provider.assert_consumed();
    fixture::evidence_run(
        "native-pilot-mutations",
        &host,
        &provider,
        run,
        json!({"terminal":terminal,"source":metadata["pilot"],"saved_graph":saved,
            "duplicate_without_execution":true,"restart_without_replay":true,"workspace_effects":"once",
            "real_model_calls":0,"graph_nodes":2}),
    );
}

#[test]
fn native_pilot_run_controls_use_safe_graph_boundaries_and_never_replay_effects() {
    let first = Gate::new();
    let second = Gate::new();
    let provider = Provider::new([
        (
            "fixture-default",
            vec![
                Reply::Tool("graph_run", json!({"graph":"fixture"})),
                Reply::Text("Run accepted.".into()),
            ],
        ),
        (
            "fixture-worker",
            vec![
                Reply::Gated(
                    first.clone(),
                    Box::new(command("printf once > effects.txt")),
                ),
                complete(None),
                Reply::Gated(second.clone(), Box::new(complete(None))),
            ],
        ),
    ]);
    let host = Host::new(
        &json!({"entry":"first","agents":{"worker":{"model":"models.worker",
        "instructions":"bounded controls fixture","wall_time_limit_seconds":30}},
        "nodes":[{"id":"first","agent":"worker"},{"id":"second","agent":"worker"}],
        "edges":[{"from":"first","to":"second"}]}),
    );
    let server = host.serve(&provider);
    session(&server);
    let started = turn(&server, "start", "Start fixture once.");
    let run = started["runs"][0].as_str().unwrap();
    first.wait_entered();
    provider.append_replies(
        "fixture-default",
        vec![
            Reply::Tool("run_pause", json!({"run":run})),
            Reply::Text("Pause requested, not yet completed.".into()),
        ],
    );
    turn(&server, "pause", "Pause the accepted Run.");
    first.open();
    let paused = wait_run(&host, run, "paused");
    assert_eq!(
        host.workspace_files(run, "effects.txt"),
        vec![b"once".to_vec()]
    );
    assert!(paused["results"]["second"].is_null());
    provider.append_replies(
        "fixture-default",
        vec![
            Reply::Tool("run_resume", json!({"run":run})),
            Reply::Text("Resume requested.".into()),
        ],
    );
    turn(&server, "resume", "Resume that existing Run.");
    second.wait_entered();
    provider.append_replies(
        "fixture-default",
        vec![
            Reply::Tool("run_stop", json!({"run":run})),
            Reply::Text("Stop requested; wait for its safe boundary.".into()),
        ],
    );
    turn(&server, "stop", "Stop that Run.");
    second.open();
    let stopped = wait_run(&host, run, "stopped");
    assert_eq!(
        host.workspace_files(run, "effects.txt"),
        vec![b"once".to_vec()]
    );
    provider.append_replies(
        "fixture-default",
        vec![
            Reply::Tool("run_status", json!({"run":run})),
            Reply::Tool("session_wait", json!({})),
            Reply::Text("The Run is stopped; no effect was replayed.".into()),
        ],
    );
    let inspected = turn(
        &server,
        "inspect",
        "Inspect Run and Session facts, do not restart.",
    );
    assert_ne!(inspected["native"]["run"], started["native"]["run"]);
    let (_, events) = server.events(
        &format!(
            "/sessions/pilot-write/turns/{}/events",
            inspected["id"].as_str().unwrap()
        ),
        None,
    );
    assert!(
        events.contains("stopped") && events.contains(run),
        "{events}"
    );
    provider.assert_consumed();
    fixture::evidence_run(
        "native-pilot-controls",
        &host,
        &provider,
        run,
        json!({
        "paused":paused["status"],"stopped":stopped["status"],"effects":"once",
        "native_runs_per_turn":true,"session_wait":true,"real_model_calls":0}),
    );
}
