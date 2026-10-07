#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Gate, Host, HttpHost, Provider, Step};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const SESSION: &str = "goose-pilot-compaction";
const CONTINUATION: &str = "Your context was compacted.";
const FIRST_REPLY: &str = "Inspected the saved Graph; no Run was requested or started.";
const SLASH_REPLY: &str = "The compact directive was received as ordinary Pilot user text.";
const FOLLOWUP_REPLY: &str = "Inspected the saved Graph again in the same conversation.";
const RESTART_REPLY: &str = "Loaded the same conversation and checked the changed Graph on disk.";

fn nonce() -> String {
    let mut bytes = [0; 16];
    fs::File::open("/dev/urandom")
        .unwrap()
        .read_exact(&mut bytes)
        .unwrap();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn graph(token: &str) -> Value {
    json!({"entry":"seed","ops":{"seed":{
        "run":format!("sh -c 'printf {token} > seed.txt'")}},
        "nodes":[{"id":"seed","op":"seed"}],"edges":[]})
}

fn summary(token: &str) -> String {
    format!(
        "Pilot compacted fixture: graph_read observed the saved Graph fixture with a seed Op writing {token} to seed.txt. No Run was requested or started. Re-read saved Graphs to check actual state before continuing."
    )
}

fn get(server: &HttpHost, path: &str) -> Value {
    let (status, saved) = server.request("GET", path, None);
    assert_eq!(status, 200, "{path}: {saved}");
    saved
}

fn create(server: &HttpHost) {
    let (status, saved) = server.request("POST", "/sessions", Some(&json!({"id":SESSION})));
    assert_eq!(status, 201, "{saved}");
}

fn submit(server: &HttpHost, request: &str, message: &str) -> Value {
    let (status, saved) = server.request(
        "POST",
        &format!("/sessions/{SESSION}/turns"),
        Some(&json!({"request_id":request,"message":message})),
    );
    assert_eq!(status, 202, "{saved}");
    saved["turn"].clone()
}

fn turn_path(turn: &Value) -> String {
    format!("/sessions/{SESSION}/turns/{}", turn["id"].as_str().unwrap())
}

fn wait(server: &HttpHost, turn: &Value, expected: &str) -> Value {
    let mut saved = Value::Null;
    goose::wait_until("Goose Pilot compaction Turn", || {
        saved = get(server, &turn_path(turn))["turn"].clone();
        assert!(
            saved["status"] == "running" || saved["status"] == expected,
            "unexpected Turn status: {saved}"
        );
        saved["status"] == expected
    });
    assert!(saved["native"].is_null(), "{saved}");
    assert!(
        saved["goose"]["session"]
            .as_str()
            .is_some_and(|session| !session.is_empty()),
        "{saved}"
    );
    assert!(saved["runs"].as_array().unwrap().is_empty(), "{saved}");
    saved
}

fn pilot_root(host: &Host, turn: &Value) -> PathBuf {
    host.base
        .root
        .path()
        .join("state/platform/pilot")
        .join(turn["goose"]["scope"].as_str().unwrap())
}

fn native_history(host: &Host, turn: &Value) -> Value {
    host.session_conversation(
        &pilot_root(host, turn).join("process"),
        turn["goose"]["session"].as_str().unwrap(),
    )
}

fn fact(host: &Host, turn: &Value) -> Value {
    fixture::read_json(pilot_root(host, turn).join("goose.json"))
}

fn assert_usage(history: &Value, reply: &str, input: u32, output: u32) {
    let row = history
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["content"] == json!([{"type":"text","text":reply}]))
        .expect("missing native reply usage");
    assert_eq!(row["metadata"]["usage"]["inputTokens"], input);
    assert_eq!(row["metadata"]["usage"]["outputTokens"], output);
    assert_eq!(row["metadata"]["usage"]["totalTokens"], input + output);
}

fn assert_compacted(before: &Value, after: &Value, summary: &str) {
    let rows = after.as_array().unwrap();
    for original in before.as_array().unwrap() {
        assert!(
            rows.iter().any(|row| {
                row["role"] == original["role"]
                    && row["content"] == original["content"]
                    && row["metadata"]["agentVisible"] == false
                    && row["metadata"]["userVisible"] == original["metadata"]["userVisible"]
            }),
            "original native message was lost or remained agent-visible: {original}; {after}"
        );
    }
    let summaries = rows
        .iter()
        .filter(|row| row["content"].to_string().contains(summary))
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), 1, "{after}");
    assert_eq!(summaries[0]["metadata"]["agentVisible"], true);
    assert_eq!(summaries[0]["metadata"]["userVisible"], false);
    assert!(
        rows.iter().any(|row| {
            row["content"].to_string().contains(CONTINUATION)
                && row["metadata"]["agentVisible"] == true
                && row["metadata"]["userVisible"] == false
        }),
        "native continuation is missing: {after}"
    );
}

fn assert_latest_read(provider: &Provider, token: &str) -> Value {
    let request = provider.requests().last().unwrap().clone();
    let feedback = goose::tool_feedback(&request);
    let result = feedback.last().expect("missing actual MCP tool result");
    assert!(result["content"].to_string().contains(token), "{result}");
    result.clone()
}

fn assert_native_prefix_preserved(before: &Value, after: &Value) {
    let rows = after.as_array().unwrap();
    assert_eq!(
        &rows[..before.as_array().unwrap().len()],
        before.as_array().unwrap()
    );
}

fn assert_tool_requests(provider: &Provider, expected: &[&str]) {
    let ledger = fixture::read_json(provider.root.join("provider.json"));
    let actual = ledger["exchanges"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|exchange| exchange["selected_actual_tool_schema"]["function"]["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "{ledger}");
}

fn native_tools(history: &Value) -> Vec<String> {
    history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row["content"].as_array().unwrap())
        .filter(|content| content["type"] == "toolRequest")
        .map(|content| {
            content["toolCall"]["value"]["name"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn no_runs(server: &HttpHost) {
    let saved = get(server, "/runs");
    assert!(saved["runs"].as_array().unwrap().is_empty(), "{saved}");
}

fn sse_records(stream: &str) -> Vec<(u64, Value)> {
    stream
        .split("\n\n")
        .filter_map(|frame| {
            let sequence = frame.lines().find_map(|line| line.strip_prefix("id: "))?;
            let data = frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .expect("persisted SSE event has no data");
            Some((
                sequence.parse().unwrap(),
                serde_json::from_str(data).unwrap(),
            ))
        })
        .collect()
}

fn delivery(server: &HttpHost, turn: &Value, reply: &str) -> Value {
    let path = format!("{}/events", turn_path(turn));
    let stream = server.events(&path, 0);
    let records = sse_records(&stream);
    assert!(!records.is_empty(), "{stream}");
    assert!(stream.contains(reply), "{stream}");
    assert!(records.windows(2).all(|pair| pair[0].0 < pair[1].0));
    let after = records[records.len() / 2].0;
    let replayed = sse_records(&server.events(&path, after));
    assert_eq!(
        replayed,
        records
            .iter()
            .filter(|(sequence, _)| *sequence > after)
            .cloned()
            .collect::<Vec<_>>()
    );
    json!({"events":records,"replay_after":after,"replayed_events":replayed})
}

fn evidence(host: &Host, provider: &Provider, turn: &Value, checks: Value) {
    provider.assert_consumed();
    let root = host.base.root.path();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goose_pilot_compaction.rs");
    let report = json!({
        "status":"passed","runtime":"goose","goose_version":"1.53.0",
        "goose_binary_sha256":goose::GOOSE_SHA256,
        "host_binary_sha256":goose::digest(&root.join("anchor-runner-host")),
        "source_sha256":goose::digest(&source),
        "fixture_source_sha256":goose::digest(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/goose_fixture.rs")),
        "test_binary_sha256":goose::digest(&std::env::current_exe().unwrap()),
        "real_model_requests":0,"production_data_used":false,"dotenv_loaded":false,
        "provider_requests":provider.requests(),"checks":checks,
        "native_history":native_history(host,turn),"pilot_fact":fact(host,turn),
        "platform_files":goose::file_inventory(&root.join("state/platform")),
        "bundle_files":goose::file_inventory(&root.join("bundle")),
        "workspace_files":goose::file_inventory(&root.join("work")),
    });
    fs::write(
        provider.root.join("evidence.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!(
        "evidence: {}",
        provider.root.join("evidence.json").display()
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_pilot_auto_compaction_followup_restart_preserves_native_and_display_history() {
    let original_token = nonce();
    let changed_token = nonce();
    let summary = summary(&original_token);
    let provider = Provider::new(
        "goose-pilot-auto-compaction-followup-restart",
        vec![
            Step::tool("graph_read", json!({"graph":"fixture"})),
            Step::text(FIRST_REPLY)
                .after("seed")
                .expect_request_contains(&original_token)
                .with_usage(120_000, 7),
            Step::summary(&summary).expect_request_contains(&original_token),
            Step::tool("graph_read", json!({"graph":"fixture"})).expect_request_contains(&summary),
            Step::text(FOLLOWUP_REPLY)
                .after("seed")
                .expect_request_contains(&original_token),
            Step::tool("graph_read", json!({"graph":"fixture"})).expect_request_contains(&summary),
            Step::text(RESTART_REPLY)
                .after("seed")
                .expect_request_contains(&changed_token),
        ],
    );
    let host = Host::new(&graph(&original_token)).default_runtime();
    let mut server = host.serve(&provider);
    create(&server);
    let first_prompt =
        "Read the saved fixture Graph using graph_read. Inspect only; do not start it.";
    let first = submit(&server, "read", first_prompt);
    let first = wait(&server, &first, "completed");
    assert_eq!(provider.requests().len(), 2);
    assert!(
        provider
            .requests()
            .iter()
            .all(|request| !goose::is_summary_request(request))
    );
    let original_read = assert_latest_read(&provider, &original_token);
    let before = native_history(&host, &first);
    assert_usage(&before, FIRST_REPLY, 120_000, 7);
    assert!(before.to_string().contains("toolRequest"));
    assert!(before.to_string().contains("toolResponse"));
    let first_delivery = delivery(&server, &first, FIRST_REPLY);

    let followup_prompt =
        "Inspect the saved fixture Graph again in this same conversation; do not run it.";
    let second = submit(&server, "followup", followup_prompt);
    let second = wait(&server, &second, "completed");
    assert_eq!(first["goose"], second["goose"]);
    assert_eq!(provider.requests().len(), 5);
    let requests = provider.requests();
    assert!(goose::is_summary_request(&requests[2]), "{}", requests[2]);
    assert!(requests[3]["messages"].to_string().contains(CONTINUATION));
    let compacted = native_history(&host, &second);
    assert_compacted(&before, &compacted, &summary);
    let second_delivery = delivery(&server, &second, FOLLOWUP_REPLY);
    let messages = get(&server, &format!("/sessions/{SESSION}/messages"));
    assert_eq!(
        messages["messages"],
        json!([
            {"role":"user","text":first_prompt},{"role":"assistant","text":FIRST_REPLY},
            {"role":"user","text":followup_prompt},{"role":"assistant","text":FOLLOWUP_REPLY}
        ])
    );
    assert!(!messages.to_string().contains(&summary));
    let (status, updated) = server.request(
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":graph(&changed_token)})),
    );
    assert_eq!(status, 200, "{updated}");
    let changed_graph = get(&server, "/graphs/fixture");
    assert!(changed_graph.to_string().contains(&changed_token));
    let saved_fact = fact(&host, &second);
    server.kill();
    drop(server);

    let server = host.serve(&provider);
    assert_eq!(fact(&host, &second), saved_fact);
    assert_eq!(native_history(&host, &second), compacted);
    assert_eq!(
        get(&server, &format!("/sessions/{SESSION}/messages")),
        messages
    );
    assert_eq!(delivery(&server, &first, FIRST_REPLY), first_delivery);
    assert_eq!(delivery(&server, &second, FOLLOWUP_REPLY), second_delivery);
    assert_eq!(provider.requests().len(), 5);
    let restart_prompt = "Continue after the Host restart. First graph_read the current fixture Graph; do not recreate it or run it.";
    let third = submit(&server, "restart", restart_prompt);
    let third = wait(&server, &third, "completed");
    assert_eq!(first["goose"], third["goose"]);
    assert_eq!(provider.requests().len(), 7);
    let restarted_read = assert_latest_read(&provider, &changed_token);
    assert!(
        !restarted_read["content"]
            .to_string()
            .contains(&original_token)
    );
    assert_compacted(&before, &native_history(&host, &third), &summary);
    assert_eq!(
        native_tools(&native_history(&host, &third)),
        vec!["anchor__graph_read"; 3]
    );
    assert_eq!(get(&server, "/graphs/fixture"), changed_graph);
    let requests = provider.requests();
    assert!(requests[5]["messages"].to_string().contains(CONTINUATION));
    assert!(!goose::is_summary_request(&requests[5]));
    assert!(!requests[5]["messages"].to_string().contains(FIRST_REPLY));
    let calls = requests.len();
    assert_eq!(submit(&server, "restart", restart_prompt), third);
    assert_eq!(provider.requests().len(), calls);
    let final_messages = get(&server, &format!("/sessions/{SESSION}/messages"));
    let mut expected_messages = messages["messages"].as_array().unwrap().clone();
    expected_messages.extend([
        json!({"role":"user","text":restart_prompt}),
        json!({"role":"assistant","text":RESTART_REPLY}),
    ]);
    assert_eq!(final_messages["messages"], json!(expected_messages));
    assert_eq!(
        get(&server, &format!("/sessions/{SESSION}/turns"))["turns"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    no_runs(&server);
    evidence(
        &host,
        &provider,
        &third,
        json!({
            "original_read":original_read,"before_compaction":before,"compacted_history":compacted,
            "summary_request_index":2,"threshold_usage":{"input":120000,"output":7},
            "first":first,"followup":second,"restart":third,"changed_graph":changed_graph,
            "restarted_read":restarted_read,"messages_before_restart":messages,"messages":final_messages,
            "first_delivery":first_delivery,"followup_delivery":second_delivery,
            "restart_delivery":delivery(&server,&third,RESTART_REPLY),
            "duplicate_idempotent":true,"no_unrequested_run":true
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_pilot_cancel_native_summary_preserves_facts_and_continues_same_session() {
    let original_token = nonce();
    let changed_token = nonce();
    let summary = summary(&original_token);
    let cancelled_summary = format!("{summary} Cancelled response marker: {}", nonce());
    let gate = Gate::new();
    let provider = Provider::new(
        "goose-pilot-cancel-native-summary",
        vec![
            Step::tool("graph_read", json!({"graph":"fixture"})),
            Step::text(FIRST_REPLY)
                .after("seed")
                .expect_request_contains(&original_token),
            Step::text(SLASH_REPLY)
                .expect_request_contains("User message: /compact")
                .with_usage(120_000, 7),
            Step::summary(&cancelled_summary)
                .expect_request_contains(&original_token)
                .gated(&gate),
            Step::summary(&summary).expect_request_contains(&original_token),
            Step::tool("graph_read", json!({"graph":"fixture"})).expect_request_contains(&summary),
            Step::text(RESTART_REPLY)
                .after("seed")
                .expect_request_contains(&changed_token),
        ],
    );
    let host = Host::new(&graph(&original_token)).default_runtime();
    let server = host.serve(&provider);
    create(&server);
    let first = submit(
        &server,
        "read",
        "Inspect fixture using graph_read; do not run it.",
    );
    let first = wait(&server, &first, "completed");
    assert_latest_read(&provider, &original_token);
    let slash = submit(&server, "slash-observation", "/compact");
    let slash = wait(&server, &slash, "completed");
    assert_eq!(first["goose"], slash["goose"]);
    assert_eq!(provider.requests().len(), 3);
    assert!(
        provider
            .requests()
            .iter()
            .all(|request| !goose::is_summary_request(request))
    );
    let before = native_history(&host, &slash);
    assert_usage(&before, SLASH_REPLY, 120_000, 7);
    assert!(!before.to_string().contains(CONTINUATION));
    let saved_fact = fact(&host, &slash);
    let saved_graph = get(&server, "/graphs/fixture");
    let saved_messages = get(&server, &format!("/sessions/{SESSION}/messages"));
    let interrupted_prompt =
        "Inspect the actual saved fixture Graph after context compaction; do not run it.";
    let interrupted = submit(&server, "cancel-summary", interrupted_prompt);
    gate.wait_entered();
    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    assert!(goose::is_summary_request(&requests[3]));
    assert_eq!(
        get(&server, &turn_path(&interrupted))["turn"]["status"],
        "running"
    );
    let during = native_history(&host, &slash);
    assert_native_prefix_preserved(&before, &during);
    assert!(!during.to_string().contains(&cancelled_summary));
    assert_eq!(fact(&host, &slash), saved_fact);
    let (status, stopped) = server.request(
        "POST",
        &format!("/sessions/{SESSION}/stop"),
        Some(&json!({})),
    );
    assert_eq!(status, 202, "{stopped}");
    assert_eq!(stopped["stopping"], true, "{stopped}");
    let interrupted = wait(&server, &interrupted, "stopped");
    gate.open();
    assert_eq!(first["goose"], interrupted["goose"]);
    assert_eq!(provider.requests().len(), 4);
    let after_cancel = native_history(&host, &interrupted);
    assert_native_prefix_preserved(&before, &after_cancel);
    assert!(!after_cancel.to_string().contains(&cancelled_summary));
    assert!(!after_cancel.to_string().contains(CONTINUATION));
    assert_eq!(fact(&host, &interrupted), saved_fact);
    assert_eq!(get(&server, "/graphs/fixture"), saved_graph);
    let stopped_messages = get(&server, &format!("/sessions/{SESSION}/messages"));
    let mut expected_messages = saved_messages["messages"].as_array().unwrap().clone();
    expected_messages.push(json!({"role":"user","text":interrupted_prompt}));
    assert_eq!(stopped_messages["messages"], json!(expected_messages));
    let stopped_stream = server.events(&format!("{}/events", turn_path(&interrupted)), 0);
    assert!(stopped_stream.contains("stopped"));
    assert!(!stopped_stream.contains(&cancelled_summary));
    let (status, changed) = server.request(
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":graph(&changed_token)})),
    );
    assert_eq!(status, 200, "{changed}");
    let continued_prompt = "Continue this Session after cancellation. First graph_read the current fixture Graph; do not recreate it or start it.";
    let continued = submit(&server, "continue", continued_prompt);
    let continued = wait(&server, &continued, "completed");
    assert_eq!(continued["goose"], first["goose"]);
    assert_eq!(provider.requests().len(), 7);
    let requests = provider.requests();
    assert!(goose::is_summary_request(&requests[4]));
    assert!(requests[5]["messages"].to_string().contains(CONTINUATION));
    assert!(
        !requests[5]["messages"]
            .to_string()
            .contains(&cancelled_summary)
    );
    let continued_read = assert_latest_read(&provider, &changed_token);
    let continued_history = native_history(&host, &continued);
    assert_compacted(&before, &continued_history, &summary);
    assert_eq!(
        native_tools(&continued_history),
        vec!["anchor__graph_read"; 2]
    );
    assert!(!continued_history.to_string().contains(&cancelled_summary));
    assert_eq!(
        fact(&host, &continued)["tool_observation"]["tool"],
        "graph_read"
    );
    assert_eq!(
        get(&server, &format!("/sessions/{SESSION}/turns"))["turns"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_tool_requests(&provider, &["anchor__graph_read", "anchor__graph_read"]);
    no_runs(&server);
    evidence(
        &host,
        &provider,
        &continued,
        json!({
            "before_compaction":before,"during_summary":during,"after_cancel":after_cancel,
            "first":first,"slash_turn":slash,"stopped":interrupted,"continued":continued,
            "cancelled_summary":cancelled_summary,"successful_summary":summary,
            "cancelled_summary_request_index":3,"successful_summary_request_index":4,
            "fact_preserved":saved_fact,"saved_graph":saved_graph,
            "saved_messages":saved_messages,"stopped_messages":stopped_messages,
            "stopped_sse":stopped_stream,"continued_read":continued_read,
            "delivery":delivery(&server,&continued,RESTART_REPLY),
            "manual_compact_command_exposed":false,"slash_is_wrapped_user_text":true,
            "no_unrequested_run":true
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_pilot_compacted_graph_create_restart_reads_saved_effect_without_replay() {
    const CREATED_GRAPH: &str = "compaction-created";
    const UNDELIVERED_REPLY: &str =
        "The requested Graph was saved; this reply was never delivered.";
    const CONTINUED_REPLY: &str =
        "Checked the existing saved Graph without replaying graph_create.";
    let original_token = nonce();
    let created_token = nonce();
    let summary = summary(&original_token);
    let gate = Gate::new();
    let provider = Provider::new(
        "goose-pilot-compacted-create-restart",
        vec![
            Step::tool("graph_read", json!({"graph":"fixture"})),
            Step::text(FIRST_REPLY)
                .after("seed")
                .expect_request_contains(&original_token)
                .with_usage(120_000, 7),
            Step::summary(&summary).expect_request_contains(&original_token),
            Step::tool(
                "graph_create",
                json!({"name":CREATED_GRAPH,"definition":graph(&created_token)}),
            )
            .expect_request_contains(&summary),
            Step::text(UNDELIVERED_REPLY)
                .after(CREATED_GRAPH)
                .expect_request_contains(&created_token)
                .gated(&gate),
            Step::tool("graph_read", json!({"graph":CREATED_GRAPH}))
                .expect_request_contains(&summary),
            Step::text(CONTINUED_REPLY)
                .after("seed")
                .expect_request_contains(&created_token),
        ],
    );
    let host = Host::new(&graph(&original_token)).default_runtime();
    let mut server = host.serve(&provider);
    create(&server);
    let first = submit(
        &server,
        "read",
        "Inspect saved fixture with graph_read; do not start it.",
    );
    let first = wait(&server, &first, "completed");
    let before = native_history(&host, &first);
    assert_usage(&before, FIRST_REPLY, 120_000, 7);
    assert_latest_read(&provider, &original_token);
    let create_prompt = "Save a new Graph named compaction-created using a seed OpNode, then reply. Do not start it.";
    let created = submit(&server, "create", create_prompt);
    gate.wait_entered();
    assert_eq!(provider.requests().len(), 5);
    let requests = provider.requests();
    assert!(goose::is_summary_request(&requests[2]));
    assert!(requests[3]["messages"].to_string().contains(CONTINUATION));
    let (status, saved_turn) = server.request("GET", &turn_path(&created), None);
    assert_eq!(status, 200, "{saved_turn}");
    let created = saved_turn["turn"].clone();
    assert_eq!(created["status"], "running");
    assert_eq!(created["goose"], first["goose"]);
    let saved = get(&server, &format!("/graphs/{CREATED_GRAPH}"));
    assert!(saved.to_string().contains(&created_token), "{saved}");
    let created_root = host.base.root.path().join(CREATED_GRAPH);
    assert_eq!(
        fixture::read_json(created_root.join("graph.json")),
        saved["definition"]
    );
    let created_files = goose::file_inventory(&created_root);
    assert!(
        created_files
            .iter()
            .any(|file| file["path"] == "graph.json")
    );
    assert!(
        created_files
            .iter()
            .any(|file| file["path"] == "manifest.json")
    );
    let saved_fact = fact(&host, &created);
    assert_eq!(saved_fact["tool_observation"]["tool"], "graph_create");
    assert_eq!(
        saved_fact["tool_observation"]["arguments"]["name"],
        CREATED_GRAPH
    );
    assert_eq!(saved_fact["tool_observation"]["result"]["ok"], true);
    let after_create = native_history(&host, &created);
    assert_compacted(&before, &after_create, &summary);
    assert_eq!(
        native_tools(&after_create),
        vec!["anchor__graph_read", "anchor__graph_create"]
    );
    assert!(!after_create.to_string().contains(UNDELIVERED_REPLY));
    let saved_messages = get(&server, &format!("/sessions/{SESSION}/messages"));
    assert_eq!(saved_messages["messages"].as_array().unwrap().len(), 3);
    assert_eq!(
        saved_messages["messages"][2],
        json!({"role":"user","text":create_prompt})
    );
    no_runs(&server);
    server.kill();
    drop(server);
    gate.open();

    let server = host.serve(&provider);
    let interrupted = wait(&server, &created, "interrupted");
    assert_eq!(interrupted["goose"], first["goose"]);
    assert_eq!(provider.requests().len(), 5);
    assert_eq!(fact(&host, &interrupted), saved_fact);
    assert_eq!(native_history(&host, &interrupted), after_create);
    assert_eq!(get(&server, &format!("/graphs/{CREATED_GRAPH}")), saved);
    assert_eq!(goose::file_inventory(&created_root), created_files);
    assert_eq!(
        get(&server, &format!("/sessions/{SESSION}/messages")),
        saved_messages
    );
    let interrupted_stream = server.events(&format!("{}/events", turn_path(&interrupted)), 0);
    assert!(interrupted_stream.contains("interrupted"));
    assert!(interrupted_stream.contains("graph_create"));
    assert!(!interrupted_stream.contains(UNDELIVERED_REPLY));
    let continued_prompt = "Continue this same Session after the Host interruption. First graph_read compaction-created to check whether the prior save happened. Do not replay graph_create or start any Run.";
    let continued = submit(&server, "inspect-saved", continued_prompt);
    let continued = wait(&server, &continued, "completed");
    assert_eq!(continued["goose"], first["goose"]);
    assert_eq!(provider.requests().len(), 7);
    let requests = provider.requests();
    assert!(!goose::is_summary_request(&requests[5]));
    assert!(requests[5]["messages"].to_string().contains(CONTINUATION));
    assert!(
        requests[5]["messages"]
            .to_string()
            .contains("Latest durable Anchor tool observation:")
    );
    assert!(requests[5]["messages"].to_string().contains("graph_create"));
    let inspected = assert_latest_read(&provider, &created_token);
    assert_eq!(get(&server, &format!("/graphs/{CREATED_GRAPH}")), saved);
    assert_eq!(goose::file_inventory(&created_root), created_files);
    let final_history = native_history(&host, &continued);
    assert_compacted(&before, &final_history, &summary);
    assert_eq!(
        native_tools(&final_history),
        vec![
            "anchor__graph_read",
            "anchor__graph_create",
            "anchor__graph_read"
        ]
    );
    assert_tool_requests(
        &provider,
        &[
            "anchor__graph_read",
            "anchor__graph_create",
            "anchor__graph_read",
        ],
    );
    assert_eq!(
        fact(&host, &continued)["tool_observation"]["tool"],
        "graph_read"
    );
    let messages = get(&server, &format!("/sessions/{SESSION}/messages"));
    let mut expected_messages = saved_messages["messages"].as_array().unwrap().clone();
    expected_messages.extend([
        json!({"role":"user","text":continued_prompt}),
        json!({"role":"assistant","text":CONTINUED_REPLY}),
    ]);
    assert_eq!(messages["messages"], json!(expected_messages));
    assert_eq!(
        get(&server, &format!("/sessions/{SESSION}/turns"))["turns"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    no_runs(&server);
    evidence(
        &host,
        &provider,
        &continued,
        json!({
            "before_compaction":before,"compacted_history_after_create":after_create,
            "first":first,"created_running_turn":created,"interrupted":interrupted,"continued":continued,
            "saved_graph":saved,"saved_graph_files":created_files,
            "saved_fact_before_kill":saved_fact,"inspected_after_restart":inspected,
            "messages_before_kill":saved_messages,"messages":messages,
            "interrupted_sse":interrupted_stream,"delivery":delivery(&server,&continued,CONTINUED_REPLY),
            "create_count":1,"no_create_replay":true,"no_unrequested_run":true
        }),
    );
}
