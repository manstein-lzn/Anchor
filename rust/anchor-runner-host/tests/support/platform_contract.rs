use crate::fixture::{
    Host, Provider, Reply, command, complete, evidence_run, read_json, wait_until,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;

fn graph() -> Value {
    json!({
        "entry":"worker","agents":{"worker":{"model":"models.worker","instructions":"perform the fixture actions and finish","wall_time_limit_seconds":30}},
        "nodes":[{"id":"worker","agent":"worker"}],"edges":[]
    })
}

fn conversation(serial: u64, session: &str, previous: Option<&str>) -> Value {
    json!({
        "graph":"fixture","run":format!("channel-00000000-0000-4000-8000-{serial:012x}"),
        "session":session,"reply_node":"worker","input":{"message":format!("turn {serial}")},
        "previous_run":previous
    })
}

#[test]
fn conversation_history_attachment_image_bytes_and_submission_identity_are_preserved() {
    let provider = Provider::new([(
        "fixture-worker",
        vec![
            command(
                "set -eu; test ! -e effects.txt; if printf corrupt > /in/channel/data.txt; then exit 9; fi; cat /in/channel/data.txt > copied.txt; printf once >> effects.txt; cat copied.txt",
            ),
            Reply::Tool("final_result", json!({"summary":"alice-first-completed"})),
            command(
                "set -eu; test ! -e copied.txt; test ! -e effects.txt; printf once >> effects.txt",
            ),
            complete(None),
            command(
                "set -eu; test ! -e copied.txt; test ! -e effects.txt; printf once >> effects.txt",
            ),
            complete(None),
        ],
    )]);
    let host = Host::new(&graph());
    let server = host.serve(&provider);
    let mut image = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut image, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[1, 2, 3, 255]).unwrap();
        writer.finish().unwrap();
    }
    let mut first = conversation(1, "alice", None);
    first["input"]["message"] = json!("alice-first-request");
    first["attachments"] = json!([
        {"name":"data.txt","data_base64":STANDARD.encode(b"alice-private"),"media_type":"text/plain"},
        {"name":"pixel.png","data_base64":STANDARD.encode(&image),"media_type":"image/png"}
    ]);
    let first_run = first["run"].as_str().unwrap();
    assert_eq!(
        server.request("POST", "/conversation-runs", Some(&first)).0,
        202
    );
    let first_detail = server.wait_status(first_run, "completed");
    let first_saved = host.record_for(first_run);
    assert_eq!(
        host.file(&first_saved, "worker", "copied.txt"),
        b"alice-private"
    );
    assert_eq!(
        first_detail["attachments"][1]["sha256"],
        format!("{:x}", Sha256::digest(&image))
    );
    assert!(first_detail["attachments"][1].get("data_base64").is_none());
    assert_eq!(
        server.request("POST", "/conversation-runs", Some(&first)).0,
        202
    );
    let mut conflicting = first.clone();
    conflicting["input"]["message"] = json!("different content");
    assert_eq!(
        server
            .request("POST", "/conversation-runs", Some(&conflicting))
            .0,
        409
    );
    assert_eq!(provider.requests().len(), 2);
    let second = conversation(2, "alice", Some(first_run));
    let second_run = second["run"].as_str().unwrap();
    assert_eq!(
        server
            .request("POST", "/conversation-runs", Some(&second))
            .0,
        202
    );
    server.wait_status(second_run, "completed");
    let other = conversation(3, "bob", None);
    let other_run = other["run"].as_str().unwrap();
    assert_eq!(
        server.request("POST", "/conversation-runs", Some(&other)).0,
        202
    );
    server.wait_status(other_run, "completed");
    let requests = provider.requests();
    assert_eq!(requests.len(), 6);
    let images = requests[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|content| content["type"] == "image_url")
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 1);
    assert_eq!(
        images[0]["image_url"]["url"],
        format!("data:image/png;base64,{}", STANDARD.encode(&image))
    );
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("alice-first-request")
    );
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("alice-first-completed")
    );
    assert!(
        !requests[4]["messages"]
            .to_string()
            .contains("alice-first-request")
    );
    assert!(
        !requests[4]["messages"]
            .to_string()
            .contains("alice-first-completed")
    );
    assert_eq!(host.record_for(first_run), first_saved);
    for run in [first_run, second_run, other_run] {
        assert_eq!(
            host.file(&host.record_for(run), "worker", "effects.txt"),
            b"once"
        );
        assert!(!host.history(run, "worker", 1).is_empty());
    }
    provider.assert_consumed();
    evidence_run(
        "conversation-history-media-idempotency",
        &host,
        &provider,
        second_run,
        json!({
            "runs":[first_saved,host.record_for(second_run),host.record_for(other_run)],
            "duplicate_submission_requests":0,"conflicting_submission_status":409,
            "same_session_history":true,"other_session_isolated":true,
            "image_bytes_preserved":true,"attachment_readonly":true,"provider_requests":6
        }),
    );
}

#[test]
fn scheduler_crud_tick_timeline_and_missed_downtime_use_real_http_host() {
    let provider = Provider::new([(
        "fixture-worker",
        vec![
            command("printf scheduled > report.txt; printf once >> effects.txt"),
            complete(None),
        ],
    )]);
    let host = Host::new(&graph());
    let server = host.serve(&provider);
    let future = (Utc::now() + Duration::seconds(2))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    let (status, created) = server.request("POST", "/schedules", Some(&json!({
        "graph":"fixture","rule":{"type":"once","at":future},"input":{"marker":"scheduled-fixture"}
    })));
    assert_eq!(status, 201, "{created}");
    let schedule = created["schedule"]["id"].as_str().unwrap();
    assert_eq!(
        server.request("GET", "/schedules", None).1["schedules"][0],
        created["schedule"]
    );
    let timeline = server.request("GET", "/timeline?days=7", None).1;
    assert_eq!(timeline["capabilities"]["scheduling"], true);
    assert!(
        timeline["scheduled"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["schedule"] == schedule && item["status"] == "planned")
    );
    let mut run = String::new();
    wait_until("scheduled Run admission", || {
        let runs = server.request("GET", "/runs", None).1;
        if let Some(item) = runs["runs"].as_array().unwrap().first() {
            run = item["run"].as_str().unwrap().to_owned();
            true
        } else {
            false
        }
    });
    let completed = server.wait_status(&run, "completed");
    assert_eq!(completed["state"]["trigger"]["source"], "schedule");
    assert_eq!(completed["state"]["trigger"]["schedule"], schedule);
    assert_eq!(
        host.record_for(&run)["input"],
        json!({"marker":"scheduled-fixture"})
    );
    assert_eq!(
        host.file(&host.record_for(&run), "worker", "effects.txt"),
        b"once"
    );
    assert_eq!(
        server
            .request("DELETE", &format!("/schedules/{schedule}"), None)
            .0,
        200
    );
    assert_eq!(
        server
            .request("DELETE", &format!("/schedules/{schedule}"), None)
            .0,
        404
    );
    let later = (Utc::now() + Duration::hours(1))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    let (status, missed) = server.request(
        "POST",
        "/schedules",
        Some(&json!({"graph":"fixture","rule":{"type":"once","at":later}})),
    );
    assert_eq!(status, 201);
    let missed_id = missed["schedule"]["id"].as_str().unwrap();
    drop(server);
    let schedules_path = host.root.path().join("schedules.json");
    let mut schedules = read_json(&schedules_path);
    let past = Utc::now() - Duration::minutes(1);
    schedules[0]["created_at"] = json!(
        (past - Duration::minutes(1))
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    );
    schedules[0]["rule"]["at"] = json!(past.format("%Y-%m-%dT%H:%M:%S").to_string());
    schedules[0]["next_at"] = schedules[0]["rule"]["at"].clone();
    fs::write(&schedules_path, schedules.to_string()).unwrap();
    let restarted = host.serve(&provider);
    let reloaded = restarted.request("GET", "/schedules", None).1;
    assert_eq!(reloaded["schedules"][0]["enabled"], false);
    assert_eq!(reloaded["schedules"][0]["id"], missed_id);
    assert_eq!(
        restarted.request("GET", "/runs", None).1["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let timeline = restarted.request("GET", "/timeline?days=7", None).1;
    assert!(
        timeline["scheduled"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["schedule"] == missed_id && item["status"] == "missed_downtime")
    );
    assert_eq!(provider.requests().len(), 2);
    provider.assert_consumed();
    evidence_run(
        "scheduler-tick-downtime-restart",
        &host,
        &provider,
        &run,
        json!({
            "timezone":"UTC","tick_triggered":true,"missed_schedule":reloaded,
            "timeline":timeline,"downtime_catchup_runs":0,"provider_requests":2
        }),
    );
}
