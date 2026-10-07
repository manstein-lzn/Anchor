#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Gate, Host, HttpHost, Provider, Step};
use serde_json::{Value, json};
use std::{fs, path::Path};

const SESSION: &str = "goose-elicitation";
const QUESTION: &str = "Choose the fixture topic and number of examples.";

fn graph() -> Value {
    json!({"entry":"seed","ops":{"seed":{"run":"sh -c 'printf fixture > seed.txt'"}},
        "nodes":[{"id":"seed","op":"seed"}],"edges":[]})
}

fn schema() -> Value {
    json!({"type":"object","properties":{
        "topic":{"type":"string","enum":["anchor-answer","other"]},
        "examples":{"type":"integer"},
        "include_code":{"type":"boolean"}},
        "required":["topic","examples","include_code"],"additionalProperties":false})
}

fn accepted_answer() -> Value {
    json!({"action":"accept","content":{
        "topic":"anchor-answer","examples":2,"include_code":false}})
}

fn create(server: &HttpHost) {
    let (status, saved) = server.request("POST", "/sessions", Some(&json!({"id":SESSION})));
    assert_eq!(status, 201, "{saved}");
}

fn turn_path(turn: &Value) -> String {
    format!("/sessions/{SESSION}/turns/{}", turn["id"].as_str().unwrap())
}

fn submit(server: &HttpHost, request: &str, message: Option<&str>) -> Value {
    let body = match message {
        Some(message) => json!({"request_id":request,"message":message}),
        None => json!({"request_id":request,"resume":true}),
    };
    let (status, saved) =
        server.request("POST", &format!("/sessions/{SESSION}/turns"), Some(&body));
    assert_eq!(status, 202, "{saved}");
    saved["turn"].clone()
}

fn get_turn(server: &HttpHost, turn: &Value) -> Value {
    let (status, saved) = server.request("GET", &turn_path(turn), None);
    assert_eq!(status, 200, "{saved}");
    saved["turn"].clone()
}

fn wait_turn(server: &HttpHost, turn: &Value, expected: &str) -> Value {
    let mut saved = Value::Null;
    goose::wait_until("Goose elicitation terminal Turn", || {
        saved = get_turn(server, turn);
        assert!(
            saved["status"] == "running" || saved["status"] == expected,
            "unexpected Turn outcome: {saved}"
        );
        saved["status"] == expected
    });
    saved
}

fn questions(server: &HttpHost, turn: &Value) -> Value {
    let (status, saved) = server.request("GET", &format!("{}/questions", turn_path(turn)), None);
    assert_eq!(status, 200, "{saved}");
    assert!(saved["questions"].is_array(), "{saved}");
    saved["questions"].clone()
}

fn pending_question(server: &HttpHost, turn: &Value) -> Value {
    let mut pending = Value::Null;
    goose::wait_until("persisted native Goose question", || {
        let saved = questions(server, turn);
        let current = get_turn(server, turn);
        assert_eq!(
            current["status"], "running",
            "{current}; questions: {saved}"
        );
        if let Some(question) = saved.as_array().unwrap().first() {
            assert_eq!(saved.as_array().unwrap().len(), 1, "{saved}");
            assert_eq!(question["status"], "pending", "{question}");
            pending = question.clone();
            true
        } else {
            false
        }
    });
    assert_eq!(pending["session"], SESSION, "{pending}");
    assert_eq!(pending["turn"], turn["id"], "{pending}");
    assert!(
        pending["id"]
            .as_str()
            .is_some_and(|identifier| !identifier.is_empty())
    );
    assert!(pending["answer"].is_null(), "{pending}");
    let (status, session) = server.request("GET", &format!("/sessions/{SESSION}"), None);
    assert_eq!(status, 200, "{session}");
    assert_eq!(session["session"]["status"], "waiting_user", "{session}");
    pending
}

fn answer(server: &HttpHost, turn: &Value, question: &Value, body: &Value) -> (u16, Value) {
    server.request(
        "POST",
        &format!(
            "{}/questions/{}/answer",
            turn_path(turn),
            question["id"].as_str().unwrap()
        ),
        Some(body),
    )
}

fn turn_count(server: &HttpHost, expected: usize) {
    let (status, saved) = server.request("GET", &format!("/sessions/{SESSION}/turns"), None);
    assert_eq!(status, 200, "{saved}");
    assert_eq!(
        saved["turns"].as_array().unwrap().len(),
        expected,
        "{saved}"
    );
}

fn no_runs(server: &HttpHost) {
    let (status, saved) = server.request("GET", "/runs", None);
    assert_eq!(status, 200, "{saved}");
    assert!(saved["runs"].as_array().unwrap().is_empty(), "{saved}");
}

fn graph_detail(server: &HttpHost) -> Value {
    let (status, saved) = server.request("GET", "/graphs/fixture", None);
    assert_eq!(status, 200, "{saved}");
    saved
}

fn precondition(server: &HttpHost) -> String {
    let (status, saved) = server.request("GET", "/graphs/fixture/delete-precondition", None);
    assert_eq!(status, 200, "{saved}");
    saved["precondition"].as_str().unwrap().to_owned()
}

fn assert_delete_question(question: &Value, expected: &str) {
    let message = question["message"].as_str().unwrap();
    assert!(
        message.contains("fixture") && message.contains(expected),
        "{question}"
    );
    assert_eq!(question["requested_schema"]["required"], json!(["confirm"]));
    assert_eq!(
        question["requested_schema"]["properties"]["confirm"]["type"],
        "boolean"
    );
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

fn history(server: &HttpHost, turn: &Value, question: &Value, expected: &[&str]) -> Value {
    let path = format!("{}/events", turn_path(turn));
    let stream = server.events(&path, 0);
    let records = sse_records(&stream);
    assert!(!records.is_empty(), "{stream}");
    assert!(
        records.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "{records:?}"
    );
    let question_events = records
        .iter()
        .filter(|(_, event)| {
            matches!(
                event["type"].as_str(),
                Some("question" | "question-answered" | "question-interrupted")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        question_events
            .iter()
            .map(|(_, event)| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected,
        "{stream}"
    );
    for (_, event) in &question_events {
        assert!(
            event.to_string().contains(question["id"].as_str().unwrap()),
            "{event}"
        );
    }
    let after = question_events[0].0;
    let replay = server.events(&path, after);
    let replayed = sse_records(&replay);
    assert_eq!(
        replayed,
        records
            .iter()
            .filter(|(sequence, _)| *sequence > after)
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(sse_records(&server.events(&path, 0)), records);
    json!({"events":records,"replay_after":after,"replayed_events":replayed})
}

fn payload_with(value: &Value, field: &str) -> Option<Value> {
    match value {
        Value::Object(fields) if fields.contains_key(field) => Some(value.clone()),
        Value::Object(fields) => fields.values().find_map(|value| payload_with(value, field)),
        Value::Array(values) => values
            .iter()
            .rev()
            .find_map(|value| payload_with(value, field)),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|value| payload_with(&value, field)),
        _ => None,
    }
}

fn last_tool_result(provider: &Provider, field: &str) -> Value {
    let requests = provider.requests();
    let feedback = goose::tool_feedback(requests.last().unwrap());
    payload_with(
        feedback.last().expect("missing actual tool feedback"),
        field,
    )
    .unwrap_or_else(|| panic!("missing {field} in actual last tool feedback: {feedback:?}"))
}

fn evidence(host: &Host, provider: &Provider, checks: Value) {
    provider.assert_consumed();
    assert!(
        provider.url.starts_with("http://127.0.0.1:"),
        "{}",
        provider.url
    );
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goose_elicitation.rs");
    let report = json!({
        "status":"passed","runtime":"goose",
        "goose_version":"1.53.0","goose_binary_sha256":goose::GOOSE_SHA256,
        "host_binary_sha256":goose::digest(&host.base.root.path().join("anchor-runner-host")),
        "source_sha256":goose::digest(&source),"real_model_requests":0,"production_data_used":false,
        "provider_requests":provider.requests(),"checks":checks,
        "platform_files":goose::file_inventory(&host.base.root.path().join("state/platform")),
        "bundle_files":goose::file_inventory(&host.base.root.path().join("bundle")),
        "workspace_files":goose::file_inventory(&host.base.root.path().join("work")),
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
fn goose_ask_user_validates_answer_and_preserves_one_turn_and_sse_history() {
    let gate = Gate::new();
    let provider = Provider::new(
        "goose-ask-user",
        vec![
            Step::tool(
                "ask_user",
                json!({"message":QUESTION,"requested_schema":schema()}),
            ),
            Step::text("The user's native answer was received in this same Turn.")
                .after("anchor-answer")
                .gated(&gate),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let server = host.serve(&provider);
    create(&server);
    let submitted = submit(
        &server,
        "ask",
        Some("Ask for the fixture topic and example count; wait for my answer."),
    );
    let pending = pending_question(&server, &submitted);
    assert_eq!(pending["message"], QUESTION);
    let mut native_schema = schema();
    native_schema
        .as_object_mut()
        .unwrap()
        .remove("additionalProperties");
    assert_eq!(pending["requested_schema"], native_schema);
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(questions(&server, &submitted), json!([pending]));
    let duplicate = submit(
        &server,
        "ask",
        Some("Ask for the fixture topic and example count; wait for my answer."),
    );
    assert_eq!(duplicate["id"], submitted["id"]);
    assert_eq!(duplicate["status"], "running");
    turn_count(&server, 1);
    let invalid = vec![
        json!({"action":"accept"}),
        json!({"action":"accept","content":{"topic":"anchor-answer"}}),
        json!({"action":"accept","content":{"topic":"anchor-answer","examples":"two","include_code":false}}),
        json!({"action":"accept","content":{"topic":"unknown","examples":2,"include_code":false}}),
        json!({"action":"accept","content":{"topic":"anchor-answer","examples":2.5,"include_code":false}}),
        json!({"action":"accept","content":{"topic":"anchor-answer","examples":2,"include_code":"false"}}),
        json!({"action":"accept","content":{"topic":"anchor-answer","examples":2,"include_code":false,"extra":true}}),
        json!({"action":"accept","content":null}),
        json!({"action":"accept","content":[]}),
        json!({"action":"decline","content":{"topic":"anchor-answer"}}),
        json!({"action":"cancel","content":{"topic":"anchor-answer"}}),
        json!({"action":"approve","content":{}}),
        json!({"action":"accept","content":accepted_answer()["content"],"extra":true}),
    ];
    let mut rejected = Vec::new();
    for body in invalid {
        let (status, saved) = answer(&server, &submitted, &pending, &body);
        assert!(
            matches!(status, 400 | 422),
            "{status}: {saved}; body: {body}"
        );
        assert_eq!(questions(&server, &submitted), json!([pending]));
        assert_eq!(get_turn(&server, &submitted)["status"], "running");
        assert_eq!(provider.requests().len(), 1);
        rejected.push(json!({"request":body,"status":status,"response":saved}));
    }
    let body = accepted_answer();
    let (status, saved) = answer(&server, &submitted, &pending, &body);
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["question"]["status"], "answered");
    assert_eq!(saved["question"]["answer"], body);
    gate.wait_entered();
    assert_eq!(last_tool_result(&provider, "action"), body);
    let (status, repeated) = answer(&server, &submitted, &pending, &body);
    assert_eq!(status, 200, "{repeated}");
    assert_eq!(repeated, saved);
    let (status, conflicting) = answer(&server, &submitted, &pending, &json!({"action":"decline"}));
    assert_eq!(status, 409, "{conflicting}");
    gate.open();
    let completed = wait_turn(&server, &submitted, "completed");
    assert_eq!(completed["id"], submitted["id"]);
    assert!(completed["native"].is_null());
    assert!(
        completed["goose"]["session"]
            .as_str()
            .is_some_and(|identifier| !identifier.is_empty())
    );
    let answered = questions(&server, &completed);
    assert_eq!(answered, json!([saved["question"]]));
    let delivery = history(
        &server,
        &completed,
        &pending,
        &["question", "question-answered"],
    );
    let (status, repeated) = answer(&server, &completed, &pending, &body);
    assert_eq!(
        status, 200,
        "terminal answer retry lost idempotency: {repeated}"
    );
    assert_eq!(repeated, saved);
    let (status, late) = answer(&server, &completed, &pending, &json!({"action":"decline"}));
    assert_eq!(
        status, 409,
        "terminal Turn accepted a different answer: {late}"
    );
    assert_eq!(questions(&server, &completed), answered);
    assert_eq!(provider.requests().len(), 2);
    turn_count(&server, 1);
    no_runs(&server);
    evidence(
        &host,
        &provider,
        json!({"pending":pending,"answered":answered,"completed":completed,
        "invalid_answers":rejected,"delivery":delivery,"late_answer_status":status}),
    );
}

fn graph_delete_decision(scenario: &str, body: Value, deleted: bool) {
    let provider = Provider::new(
        scenario,
        vec![
            Step::tool("graph_delete", json!({"graph":"fixture"})),
            Step::text("The native deletion decision has been respected.").after("deleted"),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let server = host.serve(&provider);
    create(&server);
    let before = graph_detail(&server);
    let expected = precondition(&server);
    let submitted = submit(
        &server,
        "delete",
        Some("Delete fixture, but wait for my native confirmation."),
    );
    let pending = pending_question(&server, &submitted);
    assert_delete_question(&pending, &expected);
    assert_eq!(graph_detail(&server), before);
    assert!(host.base.root.path().join("bundle/graph.json").is_file());
    let (status, invalid) = answer(
        &server,
        &submitted,
        &pending,
        &json!({"action":"accept","content":{}}),
    );
    assert!(matches!(status, 400 | 422), "{invalid}");
    assert_eq!(questions(&server, &submitted), json!([pending]));
    assert_eq!(provider.requests().len(), 1);
    let (status, saved) = answer(&server, &submitted, &pending, &body);
    assert_eq!(status, 200, "{saved}");
    let completed = wait_turn(&server, &submitted, "completed");
    let result = last_tool_result(&provider, "deleted");
    assert_eq!(result["graph"], "fixture", "{result}");
    assert_eq!(result["deleted"], deleted, "{result}");
    if deleted {
        let (status, missing) = server.request("GET", "/graphs/fixture", None);
        assert_eq!(status, 404, "{missing}");
        assert!(!host.base.root.path().join("bundle").exists());
        assert_eq!(result["precondition"], expected);
    } else {
        assert_eq!(graph_detail(&server), before);
        assert_eq!(precondition(&server), expected);
        assert!(host.base.root.path().join("bundle/graph.json").is_file());
    }
    let answered = questions(&server, &completed);
    assert_eq!(answered[0]["status"], "answered");
    assert_eq!(answered[0]["answer"]["action"], body["action"]);
    assert_eq!(answered[0]["answer"]["content"], body["content"]);
    let delivery = history(
        &server,
        &completed,
        &pending,
        &["question", "question-answered"],
    );
    assert_eq!(provider.requests().len(), 2);
    turn_count(&server, 1);
    no_runs(&server);
    evidence(
        &host,
        &provider,
        json!({"pending":pending,"answer":body,"answered":answered,
            "tool_result":result,"completed":completed,"delivery":delivery}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_graph_delete_requires_native_true_confirmation() {
    graph_delete_decision(
        "goose-delete-true",
        json!({"action":"accept","content":{"confirm":true}}),
        true,
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_graph_delete_native_false_does_not_delete() {
    graph_delete_decision(
        "goose-delete-false",
        json!({"action":"accept","content":{"confirm":false}}),
        false,
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_graph_delete_native_decline_does_not_delete() {
    graph_delete_decision("goose-delete-decline", json!({"action":"decline"}), false);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_graph_delete_native_cancel_does_not_delete() {
    graph_delete_decision("goose-delete-cancel", json!({"action":"cancel"}), false);
}

fn graph_delete_changed(scenario: &str, change_definition: bool) {
    let provider = Provider::new(
        scenario,
        vec![
            Step::tool("graph_delete", json!({"graph":"fixture"})),
            Step::text("The Graph changed; a fresh confirmation is necessary.")
                .after("Graph changed"),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let server = host.serve(&provider);
    create(&server);
    let expected = precondition(&server);
    let submitted = submit(
        &server,
        "delete-stale",
        Some("Delete fixture only after confirming the exact current contents."),
    );
    let pending = pending_question(&server, &submitted);
    assert_delete_question(&pending, &expected);
    let resource = host.base.root.path().join("bundle/manifest.json");
    if change_definition {
        let mut definition = graph_detail(&server)["definition"].clone();
        definition["objective"] = json!("Changed during the native confirmation wait.");
        let (status, saved) = server.request(
            "PUT",
            "/graphs/fixture",
            Some(&json!({"definition":definition})),
        );
        assert_eq!(status, 200, "{saved}");
    } else {
        let manifest = fs::read_to_string(&resource).unwrap();
        fs::write(&resource, format!("{manifest}\n")).unwrap();
    }
    let changed = graph_detail(&server);
    let current = precondition(&server);
    assert_ne!(expected, current);
    let (status, saved) = answer(
        &server,
        &submitted,
        &pending,
        &json!({"action":"accept","content":{"confirm":true}}),
    );
    assert_eq!(status, 200, "{saved}");
    let completed = wait_turn(&server, &submitted, "completed");
    assert_eq!(graph_detail(&server), changed);
    assert_eq!(precondition(&server), current);
    assert!(host.base.root.path().join("bundle/graph.json").is_file());
    if !change_definition {
        assert!(fs::read_to_string(&resource).unwrap().ends_with('\n'));
    }
    let requests = provider.requests();
    let feedback = goose::tool_feedback(requests.last().unwrap());
    assert!(
        feedback
            .last()
            .unwrap()
            .to_string()
            .contains("Graph deletion rejected; request a fresh confirmation"),
        "{feedback:?}"
    );
    let delivery = history(
        &server,
        &completed,
        &pending,
        &["question", "question-answered"],
    );
    turn_count(&server, 1);
    no_runs(&server);
    evidence(
        &host,
        &provider,
        json!({"pending":pending,"expected_precondition":expected,
            "current_precondition":current,"retained_graph":changed,"completed":completed,
            "questions":questions(&server, &completed),"tool_feedback":feedback,"delivery":delivery}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_graph_delete_rejects_confirmation_after_definition_changes() {
    graph_delete_changed("goose-delete-stale-definition", true);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_graph_delete_rejects_confirmation_after_resource_changes() {
    graph_delete_changed("goose-delete-stale-resource", false);
}

fn interrupted_confirmation_recovery(scenario: &str, kill_host: bool) {
    let provider = Provider::new(
        scenario,
        vec![
            Step::tool("graph_delete", json!({"graph":"fixture"})),
            Step::tool("graph_read", json!({"graph":"fixture"})),
            Step::text(
                "Checked the retained fixture Graph; the interrupted deletion was not replayed.",
            )
            .after("seed"),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let mut server = host.serve(&provider);
    create(&server);
    let before = graph_detail(&server);
    let expected = precondition(&server);
    let submitted = submit(
        &server,
        "interrupted-delete",
        Some("Request deletion of fixture and wait for confirmation."),
    );
    let pending = pending_question(&server, &submitted);
    assert_delete_question(&pending, &expected);
    if kill_host {
        server.kill();
        drop(server);
        server = host.serve(&provider);
    } else {
        let (status, saved) = server.request(
            "POST",
            &format!("/sessions/{SESSION}/stop"),
            Some(&json!({})),
        );
        assert_eq!(status, 202, "{saved}");
        assert_eq!(saved["stopping"], true, "{saved}");
    }
    let interrupted = wait_turn(
        &server,
        &submitted,
        if kill_host { "interrupted" } else { "stopped" },
    );
    let interrupted_questions = questions(&server, &interrupted);
    let mut expected_question = pending.clone();
    expected_question["status"] = json!("interrupted");
    assert_eq!(interrupted_questions, json!([expected_question]));
    assert_eq!(
        provider.requests().len(),
        1,
        "interruption unexpectedly called the Provider"
    );
    let (status, late) = answer(
        &server,
        &interrupted,
        &pending,
        &json!({"action":"accept","content":{"confirm":true}}),
    );
    assert_eq!(status, 409, "late confirmation accepted: {late}");
    assert_eq!(graph_detail(&server), before);
    assert_eq!(precondition(&server), expected);
    let delivery = history(
        &server,
        &interrupted,
        &pending,
        &["question", "question-interrupted"],
    );
    let resumed = submit(&server, "inspect-after-interruption", None);
    let resumed = wait_turn(&server, &resumed, "completed");
    assert_ne!(resumed["id"], interrupted["id"]);
    assert!(
        interrupted["goose"]["session"]
            .as_str()
            .is_some_and(|identifier| !identifier.is_empty())
    );
    assert_eq!(
        resumed["goose"], interrupted["goose"],
        "resume silently replaced the native Session"
    );
    assert_eq!(questions(&server, &interrupted), interrupted_questions);
    assert_eq!(
        questions(&server, &resumed),
        json!([]),
        "resume replayed the deletion question"
    );
    assert_eq!(graph_detail(&server), before);
    assert_eq!(precondition(&server), expected);
    assert!(host.base.root.path().join("bundle/graph.json").is_file());
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    let inspected = last_tool_result(&provider, "definition");
    assert_eq!(inspected["definition"], before["definition"]);
    assert_eq!(
        history(
            &server,
            &interrupted,
            &pending,
            &["question", "question-interrupted"]
        ),
        delivery
    );
    let (status, late) = answer(
        &server,
        &interrupted,
        &pending,
        &json!({"action":"decline"}),
    );
    assert_eq!(status, 409, "late answer accepted after resume: {late}");
    turn_count(&server, 2);
    no_runs(&server);
    evidence(
        &host,
        &provider,
        json!({"pending":pending,"interrupted":interrupted,
        "interrupted_questions":interrupted_questions,"late_answer_status":status,
        "resumed":resumed,"inspected_graph":inspected,"delivery":delivery,"host_restarted":kill_host}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_pending_question_stop_rejects_late_answers_and_resumes_with_graph_read() {
    interrupted_confirmation_recovery("goose-question-stop", false);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn goose_pending_question_host_restart_preserves_facts_without_replaying_deletion() {
    interrupted_confirmation_recovery("goose-question-host-restart", true);
}
