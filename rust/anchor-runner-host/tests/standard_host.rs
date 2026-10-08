#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;

use fixture::{Host, wait_until};
use serde_json::{Value, json};

fn graph() -> Value {
    json!({"entry":"seed","ops":{"seed":{"run":"sh -c 'printf standard-host > report.txt'"}},
        "nodes":[{"id":"seed","op":"seed"}],"edges":[]})
}

#[test]
fn standard_host_runs_op_without_goose_model_or_runtime_switch() {
    let host = Host::new(&graph());
    let server = host.serve_without_model();
    let (status, accepted) = server.request("POST", "/trigger", Some(&json!({"graph":"fixture"})));
    assert_eq!(status, 202, "{accepted}");
    let run = accepted["run"].as_str().unwrap();
    wait_until("standard Host Op completion", || {
        let (_, detail) = server.request("GET", &format!("/runs/{run}"), None);
        assert_ne!(detail["state"]["status"], "failed", "{detail}");
        detail["state"]["status"] == "completed"
    });
    let record = host.record_for(run);
    assert_eq!(host.file(&record, "seed", "report.txt"), b"standard-host");
    assert!(!host.root.path().join("state/io-harness").exists());
    assert!(!host.root.path().join("state/goose-acp").exists());
}

#[test]
fn old_state_root_still_allows_op_without_becoming_goose_recovery() {
    let host = Host::new(&graph());
    let legacy = host.root.path().join("state/io-harness/facts");
    std::fs::create_dir_all(&legacy).unwrap();
    let marker = legacy.join("unrelated.started");
    std::fs::write(&marker, b"preserve legacy facts").unwrap();
    let server = host.serve_without_model();
    let (status, accepted) = server.request("POST", "/trigger", Some(&json!({"graph":"fixture"})));
    assert_eq!(status, 202, "{accepted}");
    let run = accepted["run"].as_str().unwrap();
    wait_until("Op alongside preserved legacy facts", || {
        let (_, detail) = server.request("GET", &format!("/runs/{run}"), None);
        detail["state"]["status"] == "completed"
    });
    assert_eq!(std::fs::read(marker).unwrap(), b"preserve legacy facts");
    assert!(!host.root.path().join("state/goose-acp").exists());
}

#[test]
fn unconfigured_standard_pilot_refuses_admission_without_creating_a_turn() {
    let host = Host::new(&graph());
    let server = host.serve_without_model();
    assert_eq!(
        server
            .request("POST", "/sessions", Some(&json!({"id":"pilot"})))
            .0,
        201
    );
    let (status, rejected) = server.request(
        "POST",
        "/sessions/pilot/turns",
        Some(&json!({"request_id":"unconfigured","message":"read graph"})),
    );
    assert_eq!(status, 503, "{rejected}");
    let (status, turns) = server.request("GET", "/sessions/pilot/turns", None);
    assert_eq!(status, 200);
    assert_eq!(turns["turns"], json!([]));
    assert!(!host.root.path().join("state/io-harness").exists());
}
