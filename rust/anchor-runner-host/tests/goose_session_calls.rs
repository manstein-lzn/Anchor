#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use goose::{Gate, Host, HttpHost, Provider, Step, command, complete, wait_until};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

const CASE_SOURCE: &str = "tests/goose_session_calls.rs";
const SESSION: &str = "fixture-alice-session";
const SESSION_TOKEN: &str = "fixture-session-boundary-not-a-secret";

fn nonce(label: &str) -> String {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    format!(
        "g2g-{label}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}

fn context() -> Value {
    json!({
        "session":SESSION,"reply_node":"worker","conversation_id":"fixture-alice-conversation",
        "channel":{"source":"wecom","sender_id":"fixture-alice-user"}
    })
}

struct BoundaryState {
    resolutions: Vec<Value>,
    acknowledgements: Vec<Value>,
    path: PathBuf,
}

impl BoundaryState {
    fn save(&self) {
        fs::write(
            &self.path,
            serde_json::to_vec_pretty(&json!({
                "boundary":"local Session resolve and ACK fixture, not a channel supervisor",
                "resolutions":self.resolutions,"acknowledgements":self.acknowledgements,
                "external_sends":0
            }))
            .unwrap(),
        )
        .unwrap();
    }
}

async fn resolve(
    State(shared): State<Arc<Mutex<BoundaryState>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let authorized = headers
        .get("authorization")
        .is_some_and(|value| value == format!("Bearer {SESSION_TOKEN}").as_str())
        && body.as_object().is_some_and(|fields| fields.len() == 3)
        && body["session"] == SESSION
        && body["graph"] == "child"
        && body["parent_run"]
            .as_str()
            .is_some_and(|run| !run.is_empty());
    let mut state = shared.lock().unwrap();
    state
        .resolutions
        .push(json!({"request":body,"authorized":authorized}));
    state.save();
    if authorized {
        (StatusCode::OK, Json(context()))
    } else {
        (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"fixture Session authority rejected identity"})),
        )
    }
}

async fn acknowledge(
    State(shared): State<Arc<Mutex<BoundaryState>>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let run = body["run"].as_str().unwrap_or_default();
    let request_id = delivery_id(run);
    if run.is_empty()
        || body["request_id"] != request_id
        || body["userid"] != context()["channel"]["sender_id"]
        || body["content"].as_str().is_none_or(|text| text.is_empty())
    {
        return (StatusCode::BAD_REQUEST, Json(json!({"accepted":false})));
    }
    let mut state = shared.lock().unwrap();
    let duplicate = state.acknowledgements.iter().any(|saved| saved == &body);
    if !duplicate {
        state.acknowledgements.push(body);
        state.save();
    }
    (
        StatusCode::OK,
        Json(json!({"accepted":true,"duplicate":duplicate,"request_id":request_id})),
    )
}

struct SessionBoundary {
    url: String,
    state: Arc<Mutex<BoundaryState>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<thread::JoinHandle<()>>,
}

impl SessionBoundary {
    fn new(provider: &Provider) -> Self {
        let state = Arc::new(Mutex::new(BoundaryState {
            resolutions: Vec::new(),
            acknowledgements: Vec::new(),
            path: provider.root.join("session-boundary.json"),
        }));
        state.lock().unwrap().save();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (stop, stopped) = oneshot::channel();
        let shared = state.clone();
        let task = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let app = Router::new()
                        .route("/v1/runtime/session-calls/resolve", post(resolve))
                        .route("/fixture/ack", post(acknowledge))
                        .with_state(shared);
                    let server = tokio::spawn(async move {
                        axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                            .await
                            .unwrap();
                    });
                    tokio::select! { result = server => result.unwrap(), _ = stopped => {} }
                });
        });
        Self {
            url: format!("http://{address}"),
            state,
            stop: Some(stop),
            task: Some(task),
        }
    }

    fn ack(&self, detail: &Value) -> Value {
        assert_eq!(detail["state"]["status"], "completed");
        assert_eq!(detail["active"], false);
        assert_eq!(detail["session_call"]["status"], "pending");
        let run = detail["run"].as_str().unwrap();
        let body = json!({
            "run":run,"request_id":delivery_id(run),
            "userid":detail["session_call"]["context"]["channel"]["sender_id"],
            "content":detail["state"]["nodes"]["worker"]["submission"]
        });
        let receipt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .unwrap()
                    .post(format!("{}/fixture/ack", self.url))
                    .json(&body)
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json::<Value>()
                    .await
                    .unwrap()
            });
        assert_eq!(receipt["accepted"], true);
        assert_eq!(receipt["duplicate"], false);
        assert_eq!(receipt["request_id"], delivery_id(run));
        receipt
    }

    fn assert_resolved_once(&self, parent: &str) {
        assert_eq!(
            self.state.lock().unwrap().resolutions,
            vec![
                json!({"request":{"parent_run":parent,"graph":"child","session":SESSION},"authorized":true})
            ]
        );
    }

    fn acknowledgements(&self) -> Vec<Value> {
        self.state.lock().unwrap().acknowledgements.clone()
    }
}

impl Drop for SessionBoundary {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

fn delivery_id(run: &str) -> String {
    format!("{:x}", Sha256::digest(format!("graph-call-reply:{run}")))
}

fn parent_graph(mode: &str, marker: &str) -> Value {
    let mut call = json!({
        "graph":"child","mode":mode,"session":SESSION,
        "input":{"session":"forged-session","channel":{"sender_id":"forged-user"}},
        "input_map":{"message":"/message","mapped_value":"/mapped_value"},
        "files":[{"node":"publish","path":"request.txt","as":"request.txt"}]
    });
    let after = if mode == "wait" {
        call["result"] = json!({"node":"worker","files":["evidence.txt","effects.txt"]});
        "sh -c 'set -eu; cat /in/invoke/result/evidence.txt > parent-result.txt; cat /in/invoke/result/effects.txt > parent-effects.txt'"
    } else {
        "sh -c 'printf detached > parent-result.txt'"
    };
    json!({
        "objective":"native Goose call.session public API fixture","entry":"publish","agents":{},
        "ops":{
            "publish":{"run":format!("sh -c 'printf %s {marker} > request.txt'")},
            "invoke":{"call":call},"after":{"run":after}
        },
        "nodes":[{"id":"publish","op":"publish"},{"id":"invoke","op":"invoke"},{"id":"after","op":"after"}],
        "edges":[{"from":"publish","to":"invoke"},{"from":"invoke","to":"after"}]
    })
}

fn child_graph() -> Value {
    json!({
        "objective":"same Goose conversation with isolated Run workspaces","entry":"worker",
        "agents":{"worker":{"model":"models.worker","instructions":
            "Use authorized Anchor tools, inspect actual history and workspace, never replay a side effect, then final_result with route verify."}},
        "ops":{"verify":{"run":"sh -c 'set -eu; cat /in/worker/evidence.txt > verified.txt; cat /in/worker/effects.txt > verified-effects.txt'"}},
        "nodes":[{"id":"worker","agent":"worker"},{"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    })
}

fn host(mode: &str, marker: &str, boundary: &SessionBoundary) -> Host {
    Host::new(&parent_graph(mode, marker))
        .default_runtime()
        .with_extra_environment([
            ("ANCHOR_SESSION_HOST_URL", boundary.url.as_str()),
            ("ANCHOR_SESSION_HOST_TOKEN", SESSION_TOKEN),
        ])
}

fn create_child(server: &HttpHost) {
    let (status, created) = server.request(
        "POST",
        "/graphs",
        Some(&json!({"name":"child","definition":child_graph()})),
    );
    assert_eq!(status, 201, "{created}");
}

fn foreground(server: &HttpHost, session: &str, message: &str, previous: Option<&str>) -> String {
    let identity = format!("{:x}", Sha256::digest(nonce("foreground")));
    let run = format!(
        "channel-{}-{}-{}-{}-{}",
        &identity[..8],
        &identity[8..12],
        &identity[12..16],
        &identity[16..20],
        &identity[20..32]
    );
    let (status, accepted) = server.request(
        "POST",
        "/conversation-runs",
        Some(
            &json!({"graph":"child","run":run,"session":session,"reply_node":"worker",
            "input":{"message":message},"previous_run":previous}),
        ),
    );
    assert_eq!(status, 202, "{accepted}");
    assert_eq!(accepted["run"], run);
    run
}

fn foreground_steps(marker: &str, previous: Option<&str>) -> Vec<Step> {
    let previous_check = previous.map_or_else(
        || "test ! -e /previous/evidence.txt;".to_owned(),
        |value| format!("test \"$(cat /previous/evidence.txt)\" = '{value}'; if printf corrupt > /previous/evidence.txt; then exit 91; fi;"),
    );
    vec![
        command(&format!(
            "set -eu; test ! -e evidence.txt; test ! -e effects.txt; {previous_check} printf %s '{marker}' > evidence.txt; printf foreground > effects.txt; cat evidence.txt"
        )),
        complete("verify").after("exit_code"),
        Step::text("foreground finished"),
    ]
}

fn child_steps(marker: &str, previous: &str) -> Vec<Step> {
    vec![
        command(&format!(
            "set -eu; test ! -e evidence.txt; test ! -e effects.txt; test \"$(cat /previous/evidence.txt)\" = '{previous}'; if printf corrupt > /previous/evidence.txt; then exit 92; fi; test \"$(cat /in/call/request.txt)\" = '{marker}'; if printf corrupt > /in/call/request.txt; then exit 93; fi; printf once >> effects.txt; cat /in/call/request.txt > evidence.txt; cat evidence.txt"
        )).expect_request_contains(previous),
        complete("verify").after("exit_code"),
        Step::text("background finished"),
    ]
}

fn admit_child(server: &HttpHost, mode: &str, message: &str, mapped: &str) -> (String, String) {
    let (status, accepted) = server.request("POST", "/trigger", Some(&json!({
        "graph":"fixture","input":{"message":message,"mapped_value":mapped,"private":"not mapped"}
    })));
    assert_eq!(status, 202, "{accepted}");
    let parent = accepted["run"].as_str().unwrap().to_owned();
    let detail = server.wait_status(
        &parent,
        if mode == "wait" {
            "waiting_call"
        } else {
            "completed"
        },
    );
    let calls = detail["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1, "{detail}");
    assert_eq!(calls[0]["node"], "invoke");
    assert_eq!(calls[0]["mode"], mode);
    assert_eq!(calls[0]["invocation"], 1);
    let child = calls[0]["run"].as_str().unwrap().to_owned();
    assert_eq!(calls[0]["root_run"], parent);
    let ready = server.wait_status(&child, "ready");
    assert_eq!(ready["state"]["trigger"]["source"], "graph_call");
    assert_eq!(ready["state"]["trigger"]["run"], parent);
    assert_eq!(ready["state"]["trigger"]["mode"], mode);
    assert_eq!(ready["session_call"]["context"], context());
    assert_eq!(ready["session_call"]["status"], "pending");
    assert_eq!(ready["state"]["input"]["session"], SESSION);
    assert_eq!(ready["state"]["input"]["channel"], context()["channel"]);
    assert_eq!(ready["state"]["input"]["message"], message);
    assert_eq!(ready["state"]["input"]["mapped_value"], mapped);
    assert!(ready["state"]["input"].get("private").is_none());
    (parent, child)
}

fn execution(server: &HttpHost, run: &str, session: &str, previous: Option<&str>) -> (u16, Value) {
    server.request(
        "POST",
        &format!("/runs/{run}/session-execution"),
        Some(&json!({
            "session":session,"previous_run":previous
        })),
    )
}

fn execute(server: &HttpHost, run: &str, previous: Option<&str>) {
    let (status, accepted) = execution(server, run, SESSION, previous);
    assert_eq!(status, 200, "{accepted}");
}

fn settle(server: &HttpHost, run: &str) {
    let (status, settled) = server.request(
        "POST",
        &format!("/runs/{run}/session-settlement"),
        Some(&json!({"status":"delivered"})),
    );
    assert_eq!(status, 200, "{settled}");
}

fn metadata(host: &Host, run: &str) -> Value {
    fixture::read_json(
        host.base
            .root
            .path()
            .join("state/run-metadata")
            .join(format!("{run}.json")),
    )
}

fn record_bytes(host: &Host, run: &str, directory: &str) -> Vec<u8> {
    fs::read(
        host.base
            .root
            .path()
            .join("state")
            .join(directory)
            .join(format!("{run}.json")),
    )
    .unwrap()
}

fn assert_artifact(host: &Host, run: &str, node: &str, name: &str, expected: &[u8]) -> Value {
    let record = host.record(run);
    assert_eq!(record["status"], "completed", "{record}");
    assert_eq!(record["invocations"][node], 1);
    assert_eq!(host.base.file(&record, node, name), expected);
    assert_eq!(
        host.base
            .workspace_files(run, Path::new(name).file_name().unwrap().to_str().unwrap())
            .iter()
            .filter(|bytes| bytes.as_slice() == expected)
            .count(),
        1
    );
    let manifest = fixture::read_json(host.base.artifact(&record, node).join("manifest.json"));
    assert_eq!(manifest["files"][name]["bytes"], expected.len());
    assert_eq!(
        manifest["files"][name]["sha256"],
        format!("{:x}", Sha256::digest(expected))
    );
    manifest
}

fn assert_lineage(host: &Host, first: &str, second: &str) -> (Value, Value) {
    let first_fact = host.native_fact(first, "worker", 1);
    let second_fact = host.native_fact(second, "worker", 1);
    let binding = metadata(host, second);
    let scope = format!(
        "gc1-{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!([
                binding["bundle_source"],
                binding["conversation"]["session"],
                "worker"
            ]))
            .unwrap()
        )
    );
    assert_eq!(second_fact["conversation_scope"], scope);
    assert_eq!(
        first_fact["conversation_scope"],
        second_fact["conversation_scope"]
    );
    assert_eq!(first_fact["session_id"], second_fact["session_id"]);
    assert_eq!(first_fact["binary_sha256"], goose::GOOSE_SHA256);
    assert_eq!(first_fact["model_binding"], second_fact["model_binding"]);
    assert_ne!(first_fact["key"], second_fact["key"]);
    (first_fact, second_fact)
}

fn assert_native_pairs(history: &Value, name: &str, expected_calls: usize) {
    let blocks = history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .collect::<Vec<_>>();
    let requests = blocks
        .iter()
        .filter(|block| {
            block["type"] == "toolRequest" && block["toolCall"]["value"]["name"] == name
        })
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), expected_calls, "{history}");
    for request in requests {
        assert_eq!(request["toolCall"]["status"], "success");
        assert!(
            blocks
                .iter()
                .any(|response| response["type"] == "toolResponse"
                    && response["id"] == request["id"]
                    && response["toolResult"]["status"] == "success"
                    && response["toolResult"]["value"]["isError"] != true),
            "unpaired native tool request: {request}"
        );
    }
}

fn call_case(mode: &str, scenario: &str) {
    let prior_marker = nonce("prior");
    let call_marker = nonce("call");
    let mapped = nonce("mapped");
    let provider = Provider::new(scenario, foreground_steps(&prior_marker, None));
    let boundary = SessionBoundary::new(&provider);
    let host = host(mode, &call_marker, &boundary);
    let mut server = host.serve(&provider);
    create_child(&server);
    let prior = foreground(&server, SESSION, &prior_marker, None);
    server.wait_status(&prior, "completed");
    assert_artifact(
        &host,
        &prior,
        "worker",
        "evidence.txt",
        prior_marker.as_bytes(),
    );
    let prior_record = record_bytes(&host, &prior, "runs");
    let (parent, child) = admit_child(&server, mode, &call_marker, &mapped);
    boundary.assert_resolved_once(&parent);
    assert_eq!(provider.requests().len(), 3);
    assert!(metadata(&host, &child)["conversation"].is_null());
    let queued = record_bytes(&host, &child, "runs");
    server.kill();
    drop(server);

    let mut server = host.serve(&provider);
    server.wait_status(&child, "ready");
    assert_eq!(record_bytes(&host, &child, "runs"), queued);
    assert_eq!(provider.requests().len(), 3);
    provider.append(child_steps(&call_marker, &prior_marker));
    execute(&server, &child, Some(&prior));
    let finished = server.wait_status(&child, "completed");
    assert_eq!(finished["session_call"]["status"], "pending");
    assert_eq!(finished["state"]["trigger"]["previous_run"], prior);
    let artifact = assert_artifact(
        &host,
        &child,
        "worker",
        "evidence.txt",
        call_marker.as_bytes(),
    );
    assert_artifact(&host, &child, "worker", "effects.txt", b"once");
    assert_artifact(
        &host,
        &child,
        "verify",
        "verified.txt",
        call_marker.as_bytes(),
    );
    let (prior_fact, child_fact) = assert_lineage(&host, &prior, &child);
    let history = host.native_conversation(&child, "worker", 1);
    assert!(history.to_string().contains(&prior_marker));
    assert!(history.to_string().contains(&call_marker));
    assert_native_pairs(&history, "anchor__anchor_run", 2);
    assert_native_pairs(&history, "anchor__final_result", 2);
    assert_eq!(
        metadata(&host, &child)["conversation"]["previous_run"],
        prior
    );
    let parent_before_ack = server.wait_status(
        &parent,
        if mode == "wait" {
            "waiting_call"
        } else {
            "completed"
        },
    );
    let parent_record_before_ack = record_bytes(&host, &parent, "runs");
    if mode == "wait" {
        assert!(host.record(&parent)["results"]["after"].is_null());
    }
    let completed_record = record_bytes(&host, &child, "runs");
    let receipt = boundary.ack(&finished);
    let acknowledgements = boundary.acknowledgements();
    assert_eq!(acknowledgements.len(), 1);
    server.kill();
    drop(server);

    let server = host.serve(&provider);
    assert_eq!(record_bytes(&host, &child, "runs"), completed_record);
    assert_eq!(provider.requests().len(), 6);
    let still_pending = server.wait_status(&child, "completed");
    assert_eq!(still_pending["session_call"]["status"], "pending");
    assert_eq!(still_pending["run"], child);
    settle(&server, &child);
    let parent_finished = server.wait_status(&parent, "completed");
    settle(&server, &child);
    let (status, late_failure) = server.request(
        "POST",
        &format!("/runs/{child}/session-settlement"),
        Some(&json!({"status":"failed","error":"late delivery failure"})),
    );
    assert_eq!(status, 409, "{late_failure}");
    let (status, settled_child) = server.request("GET", &format!("/runs/{child}"), None);
    assert_eq!(status, 200);
    assert_eq!(settled_child["session_call"]["status"], "delivered");
    assert_eq!(record_bytes(&host, &child, "runs"), completed_record);
    assert_eq!(host.native_fact(&child, "worker", 1), child_fact);
    assert_eq!(host.native_conversation(&child, "worker", 1), history);
    assert_eq!(record_bytes(&host, &prior, "runs"), prior_record);
    assert_eq!(boundary.acknowledgements(), acknowledgements);
    boundary.assert_resolved_once(&parent);
    assert_eq!(provider.requests().len(), 6);
    assert_eq!(parent_finished["calls"].as_array().unwrap().len(), 1);
    assert_eq!(parent_finished["calls"][0]["run"], child);
    assert_eq!(host.record(&parent)["invocations"]["invoke"], 1);
    if mode == "wait" {
        assert_artifact(
            &host,
            &parent,
            "invoke",
            "result/evidence.txt",
            call_marker.as_bytes(),
        );
        assert_artifact(
            &host,
            &parent,
            "after",
            "parent-result.txt",
            call_marker.as_bytes(),
        );
        assert_artifact(&host, &parent, "after", "parent-effects.txt", b"once");
    } else {
        assert_eq!(
            record_bytes(&host, &parent, "runs"),
            parent_record_before_ack
        );
        assert_artifact(&host, &parent, "after", "parent-result.txt", b"detached");
    }
    host.evidence(&provider, &child, json!({
        "case_source":CASE_SOURCE,"mode":mode,"parent":parent_finished,
        "prior_fact":prior_fact,"child_fact":child_fact,"worker_artifact":artifact,
        "local_ack":receipt,"acknowledgements":acknowledgements,
        "admission_restart_no_dispatch":true,"completed_before_ack_pending":parent_before_ack,
        "ack_restart_settlement_only":true,"idempotent_settlement":true,
        "late_failure_rejected":late_failure,"tool_effect_count":1,
        "boundary":"local public Session APIs and ACK fixture only; no channel supervisor or platform delivery"
    }));
}

#[test]
#[ignore = "requires explicit pinned Goose v1.53.0 binary in ANCHOR_GOOSE_BINARY"]
fn native_goose_session_call_wait_admission_lineage_and_ack_restart_settlement() {
    call_case("wait", "goose-session-call-wait-ack-restart");
}

#[test]
#[ignore = "requires explicit pinned Goose v1.53.0 binary in ANCHOR_GOOSE_BINARY"]
fn native_goose_session_call_detach_admission_lineage_and_ack_restart_settlement() {
    call_case("detach", "goose-session-call-detach-ack-restart");
}

#[test]
#[ignore = "requires explicit pinned Goose v1.53.0 binary in ANCHOR_GOOSE_BINARY"]
fn native_goose_session_call_wrong_session_and_previous_reject_without_model_calls() {
    let alice_marker = nonce("alice");
    let bob_marker = nonce("bob");
    let call_marker = nonce("identity-call");
    let provider = Provider::new(
        "goose-session-call-identity-rejection",
        foreground_steps(&alice_marker, None),
    );
    let boundary = SessionBoundary::new(&provider);
    let host = host("wait", &call_marker, &boundary);
    let server = host.serve(&provider);
    create_child(&server);
    let alice = foreground(&server, SESSION, &alice_marker, None);
    server.wait_status(&alice, "completed");
    provider.append(foreground_steps(&bob_marker, None));
    let bob = foreground(&server, "fixture-bob-session", &bob_marker, None);
    server.wait_status(&bob, "completed");
    assert_ne!(
        host.native_fact(&alice, "worker", 1)["conversation_scope"],
        host.native_fact(&bob, "worker", 1)["conversation_scope"]
    );
    let (parent, child) = admit_child(&server, "wait", &call_marker, "identity-map");
    let frozen_record = record_bytes(&host, &child, "runs");
    let frozen_metadata = record_bytes(&host, &child, "run-metadata");
    let alice_history = host.native_conversation(&alice, "worker", 1);
    let bob_history = host.native_conversation(&bob, "worker", 1);
    let mut rejections = Vec::new();
    for (session, previous) in [
        ("fixture-bob-session", Some(alice.as_str())),
        (SESSION, None),
        (SESSION, Some(bob.as_str())),
        (SESSION, Some("missing-run")),
    ] {
        let (status, rejected) = execution(&server, &child, session, previous);
        assert_eq!(status, 409, "{rejected}");
        assert_eq!(record_bytes(&host, &child, "runs"), frozen_record);
        assert_eq!(record_bytes(&host, &child, "run-metadata"), frozen_metadata);
        assert_eq!(provider.requests().len(), 6);
        assert_eq!(host.native_conversation(&alice, "worker", 1), alice_history);
        assert_eq!(host.native_conversation(&bob, "worker", 1), bob_history);
        rejections.push(
            json!({"session":session,"previous":previous,"response":rejected,"model_calls":0}),
        );
    }
    for operation in ["resume", "pause", "recovery"] {
        let (status, rejected) = server.request(
            "POST",
            &format!("/runs/{child}/{operation}"),
            Some(&json!({"node_id":"worker","invocation":1,"attempt_id":1,"decision":"retry"})),
        );
        assert_eq!(status, 409, "{rejected}");
        assert_eq!(record_bytes(&host, &child, "runs"), frozen_record);
        assert_eq!(record_bytes(&host, &child, "run-metadata"), frozen_metadata);
        assert_eq!(provider.requests().len(), 6);
        rejections.push(json!({"operation":operation,"response":rejected,"model_calls":0}));
    }
    provider.append(child_steps(&call_marker, &alice_marker));
    execute(&server, &child, Some(&alice));
    let detail = server.wait_status(&child, "completed");
    let completed_record = record_bytes(&host, &child, "runs");
    let bound_metadata = record_bytes(&host, &child, "run-metadata");
    let history = host.native_conversation(&child, "worker", 1);
    let (status, rejected) = execution(&server, &child, SESSION, Some(&bob));
    assert_eq!(status, 409, "{rejected}");
    assert_eq!(record_bytes(&host, &child, "runs"), completed_record);
    assert_eq!(record_bytes(&host, &child, "run-metadata"), bound_metadata);
    assert_eq!(host.native_conversation(&child, "worker", 1), history);
    assert_eq!(provider.requests().len(), 9);
    assert!(!history.to_string().contains(&bob_marker));
    assert_native_pairs(&history, "anchor__anchor_run", 2);
    assert_artifact(
        &host,
        &child,
        "worker",
        "evidence.txt",
        call_marker.as_bytes(),
    );
    assert_artifact(&host, &child, "worker", "effects.txt", b"once");
    assert_lineage(&host, &alice, &child);
    boundary.assert_resolved_once(&parent);
    let receipt = boundary.ack(&detail);
    settle(&server, &child);
    server.wait_status(&parent, "completed");
    host.evidence(&provider, &child, json!({
        "case_source":CASE_SOURCE,"rejections":rejections,"bound_previous_rejection":rejected,
        "rejected_requests_model_calls":0,"child_metadata_from_real_op_call":metadata(&host, &child),
        "bob_native_history_unchanged":host.native_conversation(&bob, "worker", 1) == bob_history,
        "local_ack":receipt,"tool_effect_count":1,
        "boundary":"local public Session APIs only; no channel supervisor or platform delivery"
    }));
}

#[test]
#[ignore = "requires explicit pinned Goose v1.53.0 binary in ANCHOR_GOOSE_BINARY"]
fn native_goose_session_call_yield_foreground_restart_continue_same_run_without_replay() {
    let prior_marker = nonce("yield-prior");
    let call_marker = nonce("yield-call");
    let foreground_marker = nonce("yield-foreground");
    let provider = Provider::new(
        "goose-session-call-yield-foreground-restart",
        foreground_steps(&prior_marker, None),
    );
    let boundary = SessionBoundary::new(&provider);
    let host = host("wait", &call_marker, &boundary);
    let mut server = host.serve(&provider);
    create_child(&server);
    let prior = foreground(&server, SESSION, &prior_marker, None);
    server.wait_status(&prior, "completed");
    let (parent, child) = admit_child(&server, "wait", &call_marker, "yield-map");
    let gate = Gate::new();
    let mut steps = child_steps(&call_marker, &prior_marker);
    steps.truncate(1);
    steps.push(
        Step::text("cancelled background response")
            .after("exit_code")
            .gated(&gate),
    );
    provider.append(steps);
    execute(&server, &child, Some(&prior));
    gate.wait_entered();
    assert_eq!(
        host.base
            .workspace_files(&child, "effects.txt")
            .iter()
            .filter(|bytes| bytes.as_slice() == b"once")
            .count(),
        1
    );
    let before_fact = host.native_fact(&child, "worker", 1);
    assert!(before_fact["completion"].is_null());
    assert!(!before_fact["tool_observation"]["result"].is_null());
    let (status, yielded) = server.request("POST", &format!("/runs/{child}/session-yield"), None);
    assert_eq!(status, 200, "{yielded}");
    let stopped_detail = server.wait_status(&child, "stopped");
    gate.open();
    assert_eq!(stopped_detail["session_call"]["status"], "pending");
    let stopped = host.record(&child);
    assert_eq!(stopped["cursor"]["key"], before_fact["key"]);
    let frozen_record = record_bytes(&host, &child, "runs");
    let frozen_metadata = record_bytes(&host, &child, "run-metadata");
    provider.append(foreground_steps(&foreground_marker, Some(&call_marker)));
    let next = foreground(&server, SESSION, &foreground_marker, Some(&child));
    server.wait_status(&next, "completed");
    let (_, foreground_fact) = assert_lineage(&host, &child, &next);
    assert_artifact(
        &host,
        &next,
        "worker",
        "evidence.txt",
        foreground_marker.as_bytes(),
    );
    assert_eq!(record_bytes(&host, &child, "runs"), frozen_record);
    assert_eq!(record_bytes(&host, &child, "run-metadata"), frozen_metadata);
    let foreground_history = host.native_conversation(&next, "worker", 1);
    assert!(foreground_history.to_string().contains(&call_marker));
    assert_eq!(provider.requests().len(), 8);
    let prior_record = record_bytes(&host, &prior, "runs");
    let (status, old_run_resume) = server.request("POST", &format!("/runs/{prior}/resume"), None);
    assert_eq!(status, 409, "{old_run_resume}");
    assert_eq!(record_bytes(&host, &prior, "runs"), prior_record);
    assert_eq!(provider.requests().len(), 8);
    fs::write(
        provider.root.join("yield-before-restart.json"),
        serde_json::to_vec_pretty(&json!({
            "parent":parent,"child":child,"foreground":next,"stopped":stopped,
            "child_metadata":metadata(&host, &child),"before_fact":before_fact,
            "foreground_fact":foreground_fact,"foreground_history":foreground_history
        }))
        .unwrap(),
    )
    .unwrap();
    server.kill();
    drop(server);

    let server = host.serve(&provider);
    assert_eq!(provider.requests().len(), 8);
    assert_eq!(host.record(&child), stopped);
    let (status, changed_binding) = execution(&server, &child, SESSION, Some(&next));
    assert_eq!(status, 409, "{changed_binding}");
    assert_eq!(record_bytes(&host, &child, "run-metadata"), frozen_metadata);
    assert_eq!(provider.requests().len(), 8);
    provider.append(vec![
        complete("verify").expect_request_contains(&foreground_marker),
        command(&format!("set -eu; test \"$(cat effects.txt)\" = once; test \"$(cat evidence.txt)\" = '{call_marker}'; test \"$(cat /in/call/request.txt)\" = '{call_marker}'; printf inspected-once; cat evidence.txt"))
            .after("after observing business results"),
        complete("verify").after("inspected-once"),
        Step::text("background resumed after inspecting existing effect"),
    ]);
    let (status, continued) = execution(&server, &child, SESSION, Some(&prior));
    fs::write(
        provider.root.join("yield-resume-response.json"),
        serde_json::to_vec_pretty(&json!({
            "status":status,"response":continued,"child":child,"before_fact":before_fact,
            "foreground_fact":foreground_fact,"requests_before_continue":8
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(status, 200, "{continued}");
    let mut detail = Value::Null;
    wait_until("same background Run continuation", || {
        let (status, saved) = server.request("GET", &format!("/runs/{child}"), None);
        assert_eq!(status, 200, "{saved}");
        detail = saved;
        fs::write(
            provider.root.join("yield-resume-detail.json"),
            serde_json::to_vec_pretty(&detail).unwrap(),
        )
        .unwrap();
        assert_ne!(detail["state"]["status"], "failed", "{detail}");
        if detail["active"] == false && detail["state"]["status"] == "stopped" {
            panic!("same Run continuation stopped before model execution: {detail}");
        }
        detail["state"]["status"] == "completed" && detail["active"] == false
    });
    let resumed = host.record(&child);
    assert_eq!(resumed["run_id"], stopped["run_id"]);
    assert_eq!(resumed["snapshot"], stopped["snapshot"]);
    assert_eq!(resumed["input"], stopped["input"]);
    assert_eq!(resumed["invocations"]["worker"], 1);
    assert_eq!(
        metadata(&host, &child)["conversation"],
        serde_json::from_slice::<Value>(&frozen_metadata).unwrap()["conversation"]
    );
    let after_fact = host.native_fact(&child, "worker", 1);
    assert_eq!(after_fact["key"], before_fact["key"]);
    assert_eq!(after_fact["session_id"], before_fact["session_id"]);
    assert_eq!(
        after_fact["conversation_scope"],
        before_fact["conversation_scope"]
    );
    assert_eq!(after_fact["session_id"], foreground_fact["session_id"]);
    assert_artifact(
        &host,
        &child,
        "worker",
        "evidence.txt",
        call_marker.as_bytes(),
    );
    assert_artifact(&host, &child, "worker", "effects.txt", b"once");
    assert_artifact(
        &host,
        &child,
        "verify",
        "verified.txt",
        call_marker.as_bytes(),
    );
    assert_artifact(
        &host,
        &next,
        "worker",
        "evidence.txt",
        foreground_marker.as_bytes(),
    );
    let history = host.native_conversation(&child, "worker", 1);
    assert_native_pairs(&history, "anchor__anchor_run", 4);
    assert!(history.to_string().contains(&foreground_marker));
    assert_eq!(provider.requests().len(), 12);
    let receipt = boundary.ack(&detail);
    settle(&server, &child);
    server.wait_status(&parent, "completed");
    boundary.assert_resolved_once(&parent);
    host.evidence(&provider, &child, json!({
        "case_source":CASE_SOURCE,"before_fact":before_fact,"after_fact":after_fact,
        "foreground_fact":foreground_fact,"foreground_run":next,"yielded":yielded,
        "same_run_invocation_and_session":true,"foreground_history_seen_on_resume":true,
        "changed_binding_rejected":changed_binding,"ordinary_old_run_resume_rejected":old_run_resume,
        "tool_effect_count":1,
        "fresh_observation_required":true,"local_ack":receipt,
        "boundary":"local public Session APIs and ACK fixture only; no channel supervisor or platform delivery"
    }));
}
