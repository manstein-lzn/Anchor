#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;

use fixture::{Host, HttpHost};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;

const REPORT: &str = r#"{"intro":["Original first item","Original second item"]}"#;
const PDF: &str = "%PDF-local-contract-fixture";
const PDF_PATH: &str = "published/AI每日晨报_20261105.pdf";

fn write_file(path: &str, value: &str) -> String {
    format!("printf '%s' '{}' > {path}", value.replace('\'', "'\\''"))
}

fn production() -> Value {
    serde_json::from_str(include_str!(
        "../../../examples/graphs/ai-morning-briefing.json"
    ))
    .unwrap()
}

// Only research and publication preparation are deterministic substitutes. The
// production input, completion, digest and final selection gates run unchanged.
fn host_for(fault: Option<&str>) -> Host {
    let production = production();
    let publication = json!({
        "issue_id":"ai-daily-20261105", "issue_date":"2026-11-05", "revision":1,
        "report_sha256":format!("{:x}", Sha256::digest(REPORT)),
        "pdf_sha256":format!("{:x}", Sha256::digest(PDF)),
        "visual_review":{"approved":true,"pages_reviewed":[1],"notes":"Original archive notes"}
    });
    let manifest = json!({
        "publication":publication,
        "pdf_path":PDF_PATH,
        "report_path":"published/report.json"
    });
    let mut reconstructed = manifest.clone();
    reconstructed["publication"]["visual_review"]["notes"] = "Reconstructed notes".into();
    let feedback = if fault == Some("export_error") {
        json!({"decision":"complete","status":"failed","issues":[],"summary":"published; export incomplete"})
    } else {
        json!({"decision":"complete","issues":[]})
    };
    let mut render = vec![
        "set -eu; mkdir -p published evidence".to_owned(),
        write_file(
            "feedback.json",
            &if fault == Some("feedback") {
                format!("{feedback}\n{feedback}")
            } else {
                feedback.to_string()
            },
        ),
        write_file("publication.json", &reconstructed.to_string()),
        write_file("preview.png", "internal preview"),
        write_file("evidence/source.json", "internal evidence"),
        write_file(PDF_PATH, if fault == Some("pdf") { "changed" } else { PDF }),
        write_file(
            "published/report.json",
            if fault == Some("report") {
                "{}"
            } else {
                REPORT
            },
        ),
    ];
    if fault != Some("missing") {
        render.push(write_file(
            "published/publication.json",
            &if fault == Some("manifest") {
                format!("{manifest}\n{manifest}")
            } else {
                manifest.to_string()
            },
        ));
    }
    let graph = json!({
        "entry":"validate",
        "input":{"send_not_before":null},
        "ops":{
            "validate":production["ops"]["validate"],
            "write":{
                "run":write_file("report.json", if fault == Some("stale") { "{}" } else { REPORT }),
                "writes":["report.json"]
            },
            "render_publish":{
                "run":render.join("\n"),
                "writes":["feedback.json","publication.json","preview.png","evidence","published"]
            },
            "complete":production["ops"]["complete"],
            "wait_delivery":production["ops"]["wait_delivery"]
        },
        "nodes":[
            {"id":"validate","op":"validate"},
            {"id":"write","op":"write"},
            {"id":"render_publish","op":"render_publish"},
            {"id":"complete","op":"complete"},
            {"id":"wait_delivery","op":"wait_delivery"}
        ],
        "edges":[
            {"from":"validate","to":"write"},
            {"from":"write","to":"render_publish"},
            {"from":"render_publish","to":"complete"},
            {"from":"complete","to":"wait_delivery"}
        ]
    });
    Host::new(&graph).with_allowed_commands("sh,jq,sha256sum,date,sleep")
}

fn terminal(server: &HttpHost, run: &str, outcome: &str) -> Value {
    let mut detail = Value::Null;
    fixture::wait_until("briefing fixture terminal outcome", || {
        let (status, current) = server.request("GET", &format!("/runs/{run}"), None);
        assert_eq!(status, 200, "{current}");
        detail = current;
        detail["projection"]["phase"] == "settled" && detail["active"] == false
    });
    assert_eq!(detail["projection"]["outcome"], outcome, "{detail}");
    assert!(detail["calls"].as_array().unwrap().is_empty(), "{detail}");
    detail
}

#[test]
fn production_graph_compiles_as_a_product_without_a_delivery_dependency() {
    let source = production();
    anchor_runtime::graph::GraphSnapshot::from_authoring(source.clone()).unwrap();
    assert_eq!(source["input"], json!({"send_not_before":"09:05"}));
    assert!(source["ops"].get("deliver").is_none());
    assert!(
        source["ops"]
            .as_object()
            .unwrap()
            .values()
            .all(|op| op.get("call").is_none())
    );
    assert_eq!(
        source["nodes"].as_array().unwrap().last().unwrap()["id"],
        "wait_delivery"
    );
    assert_eq!(
        source["ops"]["wait_delivery"]["writes"],
        json!(["published"])
    );
}

#[test]
fn final_product_contains_only_the_three_exact_published_files() {
    let host = host_for(None);
    let server = host.serve_without_model();
    let run = server.trigger();
    terminal(&server, &run, "done");
    let record = host.record_for(&run);
    let artifact: Value = serde_json::from_slice(
        &fs::read(
            host.artifact(&record, "wait_delivery")
                .join("manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        artifact["files"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            PDF_PATH,
            "published/publication.json",
            "published/report.json"
        ]
    );
    let receipt = host.file(&record, "wait_delivery", "published/publication.json");
    assert_eq!(
        receipt,
        host.file(&record, "render_publish", "published/publication.json")
    );
    let publication: Value = serde_json::from_slice(&receipt).unwrap();
    assert_eq!(
        publication["publication"]["visual_review"]["notes"],
        "Original archive notes"
    );
    assert_eq!(
        host.file(&record, "wait_delivery", PDF_PATH),
        PDF.as_bytes()
    );
    assert_eq!(
        host.file(&record, "wait_delivery", "published/report.json"),
        REPORT.as_bytes()
    );
}

#[test]
fn incomplete_or_failed_publication_export_cannot_reach_final_product() {
    for fault in ["missing", "export_error"] {
        let host = host_for(Some(fault));
        let server = host.serve_without_model();
        let run = server.trigger();
        let detail = terminal(&server, &run, "failed");
        assert!(host.record_for(&run)["results"]["wait_delivery"].is_null());
        if fault == "export_error" {
            assert!(
                detail.to_string().contains("published; export incomplete"),
                "{detail}"
            );
        }
    }
}

#[test]
fn final_product_blocks_changed_bytes_and_stale_approved_drafts() {
    for (fault, diagnostic) in [
        ("pdf", "PDF digest check failed"),
        ("report", "report digest check failed"),
        (
            "stale",
            "published report differs from current approved draft",
        ),
    ] {
        let host = host_for(Some(fault));
        let server = host.serve_without_model();
        let run = server.trigger();
        let detail = terminal(&server, &run, "failed");
        assert!(detail.to_string().contains(diagnostic), "{detail}");
        assert!(host.record_for(&run)["results"]["wait_delivery"].is_null());
    }
}

#[test]
fn multiple_json_documents_cannot_replace_a_single_receipt_or_feedback() {
    for fault in ["feedback", "manifest"] {
        let host = host_for(Some(fault));
        let server = host.serve_without_model();
        let run = server.trigger();
        let detail = terminal(&server, &run, "failed");
        assert!(detail.to_string().contains("expected one"), "{detail}");
        assert!(host.record_for(&run)["results"]["wait_delivery"].is_null());
    }
}
