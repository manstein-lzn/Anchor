#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Host, Provider, Step};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const CASE_SOURCE: &str = "tests/goose_development_environment.rs";

fn tool(program: &[&str]) -> Step {
    Step::tool("anchor_run", json!({"project":"one","command":program}))
}

fn submit(server: &goose::HttpHost, session: &str, request: &str, message: &str) -> String {
    let (status, result) = server.request(
        "POST",
        &format!("/sessions/{session}/turns"),
        Some(&json!({"request_id":request,"message":message})),
    );
    assert_eq!(status, 202, "{result}");
    result["turn"]["id"].as_str().unwrap().into()
}

fn turn(server: &goose::HttpHost, session: &str, id: &str) -> Value {
    let (status, result) = server.request("GET", &format!("/sessions/{session}/turns/{id}"), None);
    assert_eq!(status, 200, "{result}");
    result["turn"].clone()
}

fn wait_turn(server: &goose::HttpHost, session: &str, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let result = turn(server, session, id);
        if result["status"] == "completed" {
            return result;
        }
        assert_eq!(result["status"], "running", "{result}");
        assert!(Instant::now() < deadline, "{result}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn executable(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("requires {name} on PATH"))
        .canonicalize()
        .unwrap()
}

#[test]
#[ignore = "requires pinned Goose, uv, Python ensurepip and Bubblewrap"]
fn private_development_editable_and_formal_delivery_remain_independent_after_session_deletion() {
    let python = Path::new("/usr/bin/python3.12")
        .canonicalize()
        .expect("requires Python 3.12");
    let uv = executable("uv");
    let pyproject = include_str!("fixtures/development_environment/pyproject.toml")
        .replace(
            "dependencies = [\"probepkg==1.0; python_version >= '3.8'\"]",
            "dependencies = []",
        )
        .replace("dev = [\"trialpkg==1.0\"]", "dev = []");
    let backend = include_str!("fixtures/development_environment/backend.py")
        .replace(
            "Requires-Dist: probepkg==1.0; python_version >= '3.8'\\n",
            "",
        )
        .replace("Requires-Dist: trialpkg==1.0; extra == 'dev'\\n", "");
    let candidate = json!({"name":"Independent environment probe","mcpServers":{"service":{"command":python,"args":["server.py"]}}});
    let provider=Provider::new("private-development-formal-delivery",vec![
        goose::command("mkdir -p one/src/editableprobe candidate"),
        Step::tool("anchor_edit",json!({"path":"one/pyproject.toml","content":pyproject})),
        Step::tool("anchor_edit",json!({"path":"one/backend.py","content":backend})),
        Step::tool("anchor_edit",json!({"path":"one/src/editableprobe/__init__.py","content":"VALUE='first'\ndef main(): print(VALUE)\n"})),
        Step::tool("development_prepare",json!({"project":"one"})),
        tool(&["pip","install","--no-index","--no-build-isolation","-e",".[dev]"]).after("pip_available"),
        tool(&["editable-probe"]).after("Successfully installed"),
        Step::tool("anchor_edit",json!({"path":"one/src/editableprobe/__init__.py","old_string":"VALUE='first'","new_string":"VALUE='changed'"})).after("first"),
        tool(&["sh","-c","python -m pip check; editable-probe; python3 -c 'import sys; assert sys.prefix == \"/development/env\"'"]),
        Step::tool("development_prepare",json!({"project":"one"})).after("changed"),
        tool(&["editable-probe"]),
        Step::tool("anchor_edit",json!({"path":"candidate/plugin.json","content":candidate.to_string()})).after("changed"),
        Step::tool("anchor_edit",json!({"path":"candidate/server.py","content":include_str!("fixtures/development_environment/server.py")})),
        Step::tool("plugin_prepare",json!({"directory":"candidate"})),
        Step::tool("plugin_test",json!({"directory":"candidate","server":"service","tool":"probe","arguments":{"value":"candidate-proof"}})).after("prepared"),
        Step::tool("plugin_install",json!({"directory":"candidate","id":"env-probe"})).after("candidate-proof"),
        Step::text("Private editable was retested and an independent candidate business tool was installed after native confirmation.").after("installed"),
        Step::tool("env-probe-service_probe",json!({"value":"formal-before-delete"})),
        goose::command("printf formal-before-delete > proof.txt").after("formal-before-delete"),
        Step::tool("final_result",json!({"files":["proof.txt"],"summary":"formal-before-delete"})),
        Step::tool("platform_status",json!({})),
        Step::text("A new Session sees the surviving installed Plugin."),
        Step::tool("env-probe-service_probe",json!({"value":"formal-after-delete"})),
        goose::command("printf formal-after-delete > proof.txt").after("formal-after-delete"),
        Step::tool("final_result",json!({"files":["proof.txt"],"summary":"formal-after-delete"})),
    ]);
    let host=Host::new(&json!({"entry":"seed","ops":{"seed":{"run":"true"}},"nodes":[{"id":"seed","op":"seed"}],"edges":[]})).default_runtime().with_allowed_commands("sh,cat,git,true,python3.12");
    let sources = host.base.root.path().join("wheels");
    fs::create_dir(&sources).unwrap();
    let host = host.with_extra_environment([
        ("ANCHOR_DEPENDENCIES_PYTHON", python.display().to_string()),
        ("ANCHOR_PILOT_PYTHON_ENABLED", "1".into()),
        ("ANCHOR_DEPENDENCIES_UV", uv.display().to_string()),
        ("ANCHOR_DEPENDENCIES_UV_SHA256", goose::digest(&uv)),
        ("ANCHOR_DEPENDENCIES_SOURCES", sources.display().to_string()),
        ("ANCHOR_NODE_TOOL_DISCLOSURE", "0".into()),
    ]);
    let server = host.serve(&provider);
    let session = "private-development";
    assert_eq!(
        server
            .request("POST", "/sessions", Some(&json!({"id":session})))
            .0,
        201
    );
    let development = submit(
        &server,
        session,
        "develop",
        "Create and retest an editable Python project, independently test the candidate, then install it through native confirmation.",
    );
    let deadline = Instant::now() + Duration::from_secs(90);
    let question = loop {
        let (status, value) = server.request(
            "GET",
            &format!("/sessions/{session}/turns/{development}/questions"),
            None,
        );
        assert_eq!(status, 200, "{value}");
        if let Some(question) = value["questions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["status"] == "pending")
        {
            break question.clone();
        }
        assert_eq!(turn(&server, session, &development)["status"], "running");
        assert!(
            Instant::now() < deadline,
            "native installation question was not reached"
        );
        std::thread::sleep(Duration::from_millis(30));
    };
    assert!(question["message"].as_str().unwrap().contains("env-probe"));
    assert_eq!(
        server
            .request(
                "POST",
                &format!(
                    "/sessions/{session}/turns/{development}/questions/{}/answer",
                    question["id"].as_str().unwrap()
                ),
                Some(&json!({"action":"accept","content":{"confirm":true}}))
            )
            .0,
        200
    );
    let completed = wait_turn(&server, session, &development);
    let formal = json!({"entry":"worker","agents":{"worker":{"model":"models.worker","instructions":"Call the independent installed probe, save proof.txt and finish."}},"nodes":[{"id":"worker","agent":"worker","plugins":["env-probe"]}],"edges":[]});
    assert_eq!(
        server
            .request(
                "POST",
                "/graphs",
                Some(&json!({"name":"formal","definition":formal}))
            )
            .0,
        201
    );
    let start = || {
        let (status, value) = server.request("POST", "/trigger", Some(&json!({"graph":"formal"})));
        assert_eq!(status, 202, "{value}");
        value["run"].as_str().unwrap().to_owned()
    };
    let before = start();
    server.wait_status(&before, "completed");
    assert_eq!(
        host.base.file(&host.record(&before), "worker", "proof.txt"),
        b"formal-before-delete"
    );
    let owned = host.base.state_root().join("platform/pilot");
    let environments = goose::file_inventory(&owned)
        .into_iter()
        .filter(|v| {
            v["path"]
                .as_str()
                .unwrap_or_default()
                .ends_with("development/projects")
                || v["path"]
                    .as_str()
                    .unwrap_or_default()
                    .ends_with("selection.json")
        })
        .collect::<Vec<_>>();
    assert!(!environments.is_empty());
    assert_eq!(
        server
            .request("DELETE", &format!("/sessions/{session}"), None)
            .0,
        200
    );
    assert!(goose::file_inventory(&owned).into_iter().all(|v| {
        !v["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("selection.json")
    }));
    assert_eq!(
        server
            .request("POST", "/sessions", Some(&json!({"id":"after-cleanup"})))
            .0,
        201
    );
    let fresh = submit(
        &server,
        "after-cleanup",
        "inspect",
        "Inspect the platform after the development Session was removed.",
    );
    wait_turn(&server, "after-cleanup", &fresh);
    let after = start();
    server.wait_status(&after, "completed");
    assert_eq!(
        host.base.file(&host.record(&after), "worker", "proof.txt"),
        b"formal-after-delete"
    );
    host.evidence(&provider,&after,json!({"case_source":CASE_SOURCE,"development_turn":completed,
        "native_install_confirmation":question,"private_editable_tested":true,"source_change_retested":true,
        "candidate_business_tested_independently":true,"formal_before_run":before,
        "formal_survived_session_delete":true,"private_selection_removed":true,
        "interpreter_sha256":goose::digest(&python),"interpreter":python,"uv_sha256":goose::digest(&uv)}));
    assert_eq!(
        server.request("DELETE", "/sessions/after-cleanup", None).0,
        200
    );
}
