use super::*;
use crate::application::run_access::fixtures;
use crate::run_metadata::{self as metadata, RunMetadata};

fn is_executing(run: &fixtures::Observation) -> bool {
    matches!(run.phase, anchor_kernel::Phase::Executing)
}
fn is_failed(run: &fixtures::Observation) -> bool {
    matches!(
        run.phase,
        anchor_kernel::Phase::Settled {
            result: anchor_kernel::Outcome::Failed { .. }
        }
    )
}
fn is_aborted(run: &fixtures::Observation) -> bool {
    matches!(
        run.phase,
        anchor_kernel::Phase::Settled {
            result: anchor_kernel::Outcome::Aborted { .. }
        }
    )
}

fn observed_error(run: &fixtures::Observation) -> Option<&str> {
    match run.view.phase() {
        crate::application::run_access::RunPhase::Settled { reason, .. } => reason,
        crate::application::run_access::RunPhase::Waiting { reason, .. } => reason,
        _ => None,
    }
}

fn kernel_record_path(state: &ApiState, id: &str) -> PathBuf {
    state
        .data_root
        .join("kernel-runs")
        .join(format!("run%2F{id}"))
        .join("resource.json")
}
pub(in crate::api) async fn finish_with_artifacts(state: &ApiState, id: &str) {
    use anchor_runtime::graph::{ArtifactPort, InvocationKey};
    for _ in 0..100 {
        let view = state.application.read_run(id).unwrap().unwrap();
        if view.is_settled() {
            return;
        }
        let dispatch = view.dispatches().first().unwrap();
        let key: InvocationKey = dispatch.key.clone();
        let completion = fixtures::completion(json!({}));
        let artifacts = HostArtifacts::new(
            state.data_root.join("artifacts"),
            state.workspace_root.clone(),
        );
        std::fs::create_dir_all(artifacts.workspace_path(&key).unwrap()).unwrap();
        let commit = artifacts.freeze(&key, &completion).await.unwrap();
        fixtures::complete(
            &state.application,
            id,
            &key.node_id,
            completion,
            Some(commit),
        );
    }
    panic!("fixture exceeded finite graph bound");
}

pub(in crate::api) async fn seed_scenario(state: &ApiState, id: &str) -> GraphRunRecord {
    let record = if id == "child" {
        GraphRunRecord::create_with_id(
            GraphSnapshot::from_authoring(json!({
                "objective":"test", "entry":"invoke", "agents":{},
                "ops":{"invoke":{"call":{"graph":"child","mode":"wait"}}},
                "nodes":[{"id":"invoke","op":"invoke"}],"edges":[]
            }))
            .unwrap(),
            json!({}),
            id,
        )
        .unwrap()
    } else {
        fixtures::work_record(id)
    };
    fixtures::create(&state.application, &record);
    match id {
        "paused" | "stopped" | "budget" => fixtures::control(&state.application, id, "stop", id),
        "done" => finish_with_artifacts(state, id).await,
        "failed" => fixtures::fail(&state.application, id, "worker", "recorded reason"),
        "aborted" => fixtures::control(&state.application, id, "abandon", "recorded reason"),
        "child" => fixtures::bind_child(&state.application, id, "invoke", "child-target"),
        "recovery" => {
            fixtures::recovery(
                &state.application,
                id,
                "worker",
                vec![anchor_runtime::graph::RecoveryAttempt {
                    attempt_id: 1,
                    step: 1,
                    tool: "fixture-effect".into(),
                    started_at: "2026-10-19T00:00:00Z".into(),
                }],
            );
        }
        "executing" => {}
        other => panic!("undefined fixture: {other}"),
    }
    fixtures::record(&state.application, id)
}

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tower::ServiceExt;

use crate::PROCESS_ENV_LOCK as PROCESS_ENV;

mod channel_sessions;
mod conversations;
mod graph_deletion;
mod graph_listeners;
mod graph_validation;
mod kernel_probe;
mod oauth;
mod persistent_assistants;
mod pilot_attachments;
mod pilot_credentials;
mod pilot_graph;
mod pilot_network_defaults;
mod pilot_oauth;
mod platform_contract;
mod plugin_library;
mod production_readiness;
mod questions;
mod responses;
mod run_abandon;
mod run_access_json;
mod run_plugins;
mod schedules;
mod session_calls;
mod session_plugins;
mod sessions;
mod webhooks;
mod wecom;

#[test]
fn installation_owner_is_local_unless_a_former_key_identity_was_recorded() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        crate::api::sessions::installation_owner(root.path()).unwrap(),
        "local"
    );
    let path = root.path().join("platform/installation-owner.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let legacy = format!("api:{}", "a".repeat(64));
    std::fs::write(&path, json!({"format":1,"owner":legacy}).to_string()).unwrap();
    assert_eq!(
        crate::api::sessions::installation_owner(root.path()).unwrap(),
        legacy
    );
    for invalid in [
        json!({"format":1,"owner":"api:short"}),
        json!({"format":1,"owner":"someone"}),
        json!({"format":2,"owner":"local"}),
        json!({"format":1,"owner":"local","extra":true}),
    ] {
        std::fs::write(&path, invalid.to_string()).unwrap();
        assert!(
            crate::api::sessions::installation_owner(root.path()).is_err(),
            "{invalid}"
        );
    }
}

fn set_host_env(root: &std::path::Path) {
    // The subprocess host currently obtains sandbox configuration from
    // process configuration. Serialize this test and set only isolated
    // temporary paths; production API requests cannot mutate them.
    unsafe {
        env::set_var("ANCHOR_RUNNER_WORKSPACE_ROOT", root.join("workspaces"));
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh,cat");

        env::remove_var("ANCHOR_MODEL_API_KEY");
        env::remove_var("ANCHOR_MODEL_URL");
        env::remove_var("ANCHOR_MODEL_NAME");
    }
}

pub(in crate::api) fn management_fixture(
    root: &std::path::Path,
) -> (
    Arc<crate::model_configuration::ModelConfiguration>,
    Arc<crate::runtime_management::RuntimeManagement>,
) {
    // Management state stays outside the execution root so static graph
    // validation tests can continue proving that they create no execution facts.
    let management_root = root.join("management");
    let model = Arc::new(
        crate::model_configuration::ModelConfiguration::from_env(&management_root).unwrap(),
    );
    let runtime = Arc::new(
        crate::runtime_configuration::RuntimeConfiguration::from_env(
            &management_root,
            &root.join("library"),
            PathBuf::from("bwrap"),
        )
        .unwrap(),
    );
    let operations = Arc::new(
        crate::runtime_management::RuntimeManagement::new(runtime, &management_root).unwrap(),
    );
    (model, operations)
}

pub(super) fn fixture() -> (tempfile::TempDir, ApiState) {
    let root = tempdir().unwrap();
    let bundle = root.path().join("bundle");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("graph.json"), r#"{"objective":"fixture","entry":"work","agents":{},"ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]}"#).unwrap();
    std::fs::write(
        bundle.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let schedules = ScheduleStore::open(root.path().join("schedules.json")).unwrap();
    let (model_configuration, runtime_management) = management_fixture(root.path());
    let state = ApiState {
        model_configuration,
        runtime_management,
        bundle_root: bundle.clone(),
        catalog_root: root.path().to_path_buf(),
        application: RunApplication::new(root.path().join("state"), root.path().to_path_buf())
            .with_configured_graph("fixture".into(), bundle)
            .with_schedule_management(Arc::new(
                crate::api::schedules::ScheduleManagementAdapter::new(schedules.clone()),
            )),
        data_root: root.path().join("state"),
        workspace_root: root.path().join("workspaces"),
        graph_name: "fixture".into(),
        owner: "local".into(),
        // Keep the schedule fixture outside data_root: static graph validation
        // must leave that root absent and otherwise unchanged.
        schedules,
        pilots: crate::pilot_host::PilotService::default(),
        channel_event_locks: Default::default(),
        response_fixture: Some("deterministic response fixture".into()),
    };
    (root, state)
}

/// Serve `fixture` as an enabled bot whose result node is `reply_node`. The
/// real adapter, its files and gateway processes are covered by its own tests.
fn with_bot(state: &mut ApiState, reply_node: &str, users: &[&str]) {
    with_bot_of(state, "fixture", reply_node, users, None);
}

fn with_bot_of(
    state: &mut ApiState,
    graph: &str,
    reply_node: &str,
    users: &[&str],
    descriptor: Option<PathBuf>,
) {
    use crate::application::channel_management::fake::FakeChannels;
    let mut bot = FakeChannels::bot(graph, "local", reply_node, users);
    bot.descriptor = descriptor;
    state.application = state
        .application
        .clone()
        .with_channel_management(Arc::new(FakeChannels::new().with(bot)));
}

async fn call(app: Router, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_owned())))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "invalid JSON response {:?}: {error}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, value)
}

async fn authorized_response(
    app: Router,
    method: &str,
    uri: &str,
    key: &str,
    body: Option<Value>,
) -> HttpResponse {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .header(header::CONTENT_TYPE, "application/json");
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    app.oneshot(request.body(body).unwrap()).await.unwrap()
}

async fn json_response(response: HttpResponse) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

async fn authorized_call(
    app: Router,
    method: &str,
    uri: &str,
    key: &str,
    body: impl Into<Option<Value>>,
) -> (StatusCode, Value) {
    json_response(authorized_response(app, method, uri, key, body.into()).await).await
}

/// Read a response body as text: SSE routes answer with an event stream, not
/// JSON, so the JSON helper cannot inspect them.
async fn call_text(app: Router, method: &str, uri: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}
#[tokio::test]
async fn health_and_graph_routes_return_loaded_bundle_projection() {
    let (_root, state) = fixture();
    let app = router(state);
    let (status, health) = call(app.clone(), "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "ok");
    let (status, graphs) = call(app.clone(), "GET", "/graphs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(graphs["graphs"][0]["graph"], "fixture");
    let (status, graph) = call(app.clone(), "GET", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(graph["definition"]["objective"], "fixture");
    let (status, _) = call(app, "GET", "/graphs/missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn graph_crud_uses_admitted_catalog_bundles() {
    let (_root, state) = fixture();
    let app = router(state);
    let (status, created) = call(
        app.clone(),
        "POST",
        "/graphs",
        Some(r#"{"name":"new-graph"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["graph"], "new-graph");
    let (status, listed) = call(app.clone(), "GET", "/graphs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listed["graphs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["graph"] == "new-graph")
    );
    let updated = r#"{"definition":{"objective":"changed","entry":"start","agents":{},"ops":{"start":{"run":"true"}},"nodes":[{"id":"start","op":"start","plugins":[]}],"edges":[]}}"#;
    let (status, value) = call(app.clone(), "PUT", "/graphs/new-graph", Some(updated)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["definition"]["objective"], "changed");
    let (status, fetched) = call(app.clone(), "GET", "/graphs/new-graph", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["graph"], "new-graph");
    assert_eq!(fetched["definition"]["objective"], "changed");
    let (status, _) = call(app.clone(), "DELETE", "/graphs/new-graph", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(app, "GET", "/graphs/new-graph", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[cfg(unix)]
#[tokio::test]
async fn graph_mutation_lease_is_released_before_forked_child_executes() {
    use std::{
        io::{Read, Write},
        os::unix::{net::UnixStream, process::CommandExt},
        process::Command,
        time::Duration,
    };

    let (_root, state) = fixture();
    let lease = state
        .application
        .graph_admission_lease(&state.bundle_root)
        .unwrap();
    let (mut parent, mut child) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    child
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    // Bubblewrap's pre_exec also forces fork instead of posix_spawn. Keep this
    // child before exec so its inherited lock descriptor cannot close yet.
    let process = std::thread::spawn(move || {
        let mut command = Command::new("/usr/bin/true");
        unsafe {
            command.pre_exec(move || {
                child.write_all(&[1])?;
                child.read_exact(&mut [0])?;
                Ok(())
            });
        }
        command.status()
    });
    parent.read_exact(&mut [0]).unwrap();

    let app = router(state);
    let definition = r#"{"definition":{"objective":"changed","entry":"work","agents":{},"ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]}}"#;
    let (held_status, held) = call(app.clone(), "PUT", "/graphs/fixture", Some(definition)).await;
    drop(lease);
    let (released_status, released) = call(app, "PUT", "/graphs/fixture", Some(definition)).await;
    parent.write_all(&[1]).unwrap();
    assert!(process.join().unwrap().unwrap().success());
    assert_eq!(held_status, StatusCode::CONFLICT, "{held}");
    assert_eq!(released_status, StatusCode::OK, "{released}");
}

#[tokio::test]
async fn graph_crud_round_trips_authoring_modules_and_layout() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(_root.path());
    let app = router(state);
    let authoring = r#"{
        "objective":"authoring graph",
        "entry":"stage",
        "agents":{},
        "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"stage","graph":"inner"}],
        "edges":[],
        "graphs":{"inner":{
            "entry":"inside",
            "exit":"inside",
            "nodes":[{"id":"inside","op":"work"}],
            "edges":[]
        }},
        "layout":{"positions":{"stage":{"x":17,"y":29}},
                   "edgeLabels":{"stage|done":"Finished"}}
    }"#;
    let (status, value) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&format!(r#"{{"definition":{authoring}}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["definition"]["layout"]["positions"]["stage"]["x"], 17);
    assert!(value["definition"]["graphs"]["inner"].is_object());

    let (status, fetched) = call(app.clone(), "GET", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fetched["definition"]["layout"],
        value["definition"]["layout"]
    );
    assert_eq!(
        fetched["definition"]["graphs"],
        value["definition"]["graphs"]
    );

    let (status, started) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture","input":{}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let run = started["run"].as_str().unwrap();
    let mut completed = false;
    for _ in 0..100 {
        let (status, detail) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
        if status == StatusCode::OK && detail["projection"]["phase"] != "executing" {
            assert_eq!(detail["projection"]["outcome"], "done", "{detail}");
            assert_eq!(detail["nodes"][0], "stage/inside");
            assert_eq!(detail["state"]["executed"][0], "stage/inside");
            completed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(completed, "authoring Graph Run did not complete: {run}");
}

#[tokio::test]
async fn graph_crud_binds_plugin_resources_and_removes_stale_bundle_plugins() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(_root.path());
    let plugin = state.catalog_root.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills/example")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Demo","skills":"skills/"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("skills/example/SKILL.md"), "demo skill").unwrap();
    let app = router(state.clone());
    let definition = serde_json::json!({
        "objective":"plugin authoring graph",
        "agents":{"worker":{"model":"fixture","instructions":"work"}},
        "ops":{},
        "nodes":[{"id":"work","agent":"worker","plugins":["demo"]}],
        "edges":[]
    });
    let (status, value) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .with_library_root(state.application.library_root())
        .load()
        .unwrap();
    // The bundle references the Plugin by id and carries no Plugin bytes.
    assert_eq!(bundle.plugins, vec!["demo".to_owned()]);
    assert!(
        !state.bundle_root.join("plugins").exists(),
        "a bundle must not carry Plugin bytes"
    );
    assert!(plugin.join("skills/example/SKILL.md").is_file());

    std::fs::write(plugin.join(".env"), "TOKEN=must-not-copy").unwrap();
    let (status, failure) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{failure}");
    assert!(
        FileGraphBundleLoader::new(&state.bundle_root)
            .with_library_root(state.application.library_root())
            .load()
            .is_ok()
    );
    std::fs::remove_file(plugin.join(".env")).unwrap();

    let mut no_plugin = definition;
    no_plugin["nodes"][0]
        .as_object_mut()
        .unwrap()
        .remove("plugins");
    let (status, value) = call(
        app,
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":no_plugin}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(!state.bundle_root.join("plugins").exists());
    assert!(
        FileGraphBundleLoader::new(&state.bundle_root)
            .load()
            .is_ok()
    );
}

#[tokio::test]
async fn rust_host_serves_the_workbench_and_api_without_a_key() {
    let (root, state) = fixture();
    let web_root = root.path().join("web");
    std::fs::create_dir_all(web_root.join("assets")).unwrap();
    std::fs::write(web_root.join("index.html"), "<html>Anchor</html>").unwrap();
    std::fs::write(web_root.join("assets/app.js"), "console.log('app')").unwrap();
    let app = router_with_web_root(state, web_root);
    for path in ["/", "/assets/app.js", "/health", "/graphs"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
}

#[tokio::test]
async fn trigger_persists_run_and_projects_list_detail_and_control() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(_root.path());
    let mut definition: Value =
        serde_json::from_slice(&std::fs::read(state.bundle_root.join("graph.json")).unwrap())
            .unwrap();
    definition["ops"]["work"]["run"] = json!("sh -c 'printf rust-artifact > report.txt'");
    definition["ops"]["work"]["writes"] = json!(["report.txt"]);
    std::fs::write(state.bundle_root.join("graph.json"), definition.to_string()).unwrap();
    let app = router(state.clone());
    let (status, started) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture","input":{}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let run = started["run"].as_str().unwrap();
    for _ in 0..100 {
        let (status, value) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
        if status == StatusCode::OK && value["projection"]["phase"] != "executing" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let (status, runs) = call(app.clone(), "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runs["runs"][0]["run"], run);
    let (status, detail) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["projection"]["outcome"], "done");
    assert_eq!(detail["state"]["nodes"]["work"]["files"][0], "report.txt");
    let record = fixtures::Reader::new(&state.application)
        .load(run)
        .unwrap()
        .unwrap();
    let key = &record.results["work"][0].key;
    let artifact = &record.results["work"][0].commit;
    let artifact_base = format!("/api/artifacts/{}/files", artifact.id);
    assert_eq!(
        detail["state"]["nodes"]["work"]["artifact"],
        json!(artifact)
    );
    assert_eq!(
        detail["state"]["nodes"]["work"]["artifact_availability"],
        "available"
    );
    {
        let root = state.data_root.join("goose-acp");
        std::fs::create_dir_all(&root).unwrap();
        let stem = format!("{:x}", Sha256::digest(key.durable_key().as_bytes()));
        std::fs::write(
            root.join(format!("{stem}.json")),
            json!({
                "version":2,"key":key,"binary_sha256":"fixture","session_id":"trace-fixture",
                "completion":null,"reason":null,"model_binding":null,"tool_observation":null
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(root.join(format!("{stem}.evidence.json")), json!({
            "version":1,"key":key,"runtime":"goose","notifications":[
                {"method":"session/update","params":{"sessionId":"trace-fixture","update":{"sessionUpdate":"agent_message_chunk","content":{"text":"Inspecting the task now."}}}},
                {"method":"session/update","params":{"sessionId":"trace-fixture","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"completed","content":[{"content":{"text":"tool returned report.txt"}}]}}}
            ]
        }).to_string()).unwrap();
    }
    let (status, traced) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        traced["traces"]["[\"work\",1]"][0]["text"],
        "Inspecting the task now."
    );
    assert_eq!(traced["traces"]["[\"work\",1]"][1]["role"], "tool");
    let workspace = HostArtifacts::new(
        state.data_root.join("artifacts"),
        state.workspace_root.clone(),
    )
    .workspace_path(key)
    .unwrap();
    // Changing the live workspace must never rewrite already committed files.
    std::fs::write(workspace.join("report.txt"), "changed-after-commit").unwrap();
    let (missing_status, _) = call(
        app.clone(),
        "GET",
        &format!("/runs/{run}/files/unknown"),
        None,
    )
    .await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    let (status, files) = call(app.clone(), "GET", &format!("/runs/{run}/files/work"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(files["files"][0]["path"], "report.txt");
    let (status, fixed_files) = call(app.clone(), "GET", &artifact_base, None).await;
    assert_eq!(status, StatusCode::OK, "{fixed_files}");
    assert_eq!(fixed_files["artifact"], json!(artifact));
    assert_eq!(fixed_files["artifact_availability"], "available");
    assert_eq!(fixed_files["files"], files["files"]);
    let (status, body) = call(
        app.clone(),
        "GET",
        &format!("/runs/{run}/files/work/report.txt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], "rust-artifact");
    for base in [format!("/runs/{run}/files/work"), artifact_base.clone()] {
        let download = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{base}/report.txt?download=1"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(download.status(), StatusCode::OK);
        assert_eq!(
            download.headers()[header::CONTENT_DISPOSITION],
            "attachment"
        );
        assert_eq!(
            &to_bytes(download.into_body(), 1024).await.unwrap()[..],
            b"rust-artifact"
        );
    }
    let (status, timeline) = call(app.clone(), "GET", "/timeline?days=30", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(timeline["runs"][0]["run"], run);
    assert_eq!(timeline["capabilities"]["scheduling"], true);
    for base in [format!("/runs/{run}/files/work"), artifact_base.clone()] {
        let traversal = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{base}/%2e%2e/manifest.json"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(traversal.status(), StatusCode::BAD_REQUEST);
    }
    wait_idle(&state).await;
    let (status, _) = call(app.clone(), "POST", &format!("/runs/{run}/pause"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let retention = format!("/api/artifacts/{}/retention", artifact.id);
    let (status, retained) = call(
        app.clone(),
        "POST",
        &retention,
        Some(r#"{"retained":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{retained}");
    assert_eq!(retained["retained"], true);
    assert_eq!(retained["artifact"], json!(artifact));
    let (status, _) = call(app.clone(), "DELETE", &format!("/runs/{run}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, delivered) = call(
        app.clone(),
        "GET",
        &format!("{artifact_base}/report.txt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{delivered}");
    assert_eq!(delivered["text"], "rust-artifact");
    let (status, released) = call(
        app.clone(),
        "POST",
        &retention,
        Some(r#"{"retained":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{released}");
    state
        .application
        .reclaim(
            &crate::retention::Policy {
                max_age: None,
                max_bytes: Some(0),
            },
            &state.workspace_root,
            chrono::Utc::now().timestamp(),
        )
        .await
        .unwrap();
    let (status, expired) = call(app.clone(), "GET", &artifact_base, None).await;
    assert_eq!(status, StatusCode::OK, "{expired}");
    assert_eq!(expired["artifact_availability"], "expired");
    assert_eq!(expired["files"], files["files"]);
    for suffix in ["report.txt", "report.txt?download=1"] {
        let (status, body) = call(
            app.clone(),
            "GET",
            &format!("{artifact_base}/{suffix}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::GONE, "{body}");
        assert_eq!(body["error"], "交付文件已过期");
    }
}

#[tokio::test]
async fn startup_recovery_dispatches_only_ready_detached_children() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child_bundle = root.path().join("child");
    std::fs::create_dir_all(&child_bundle).unwrap();
    let definition = json!({
        "objective":"child", "entry":"work", "agents":{},
        "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work","plugins":[]}], "edges":[]
    });
    std::fs::write(child_bundle.join("graph.json"), definition.to_string()).unwrap();
    std::fs::write(
        child_bundle.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let snapshot = anchor_runtime::graph::GraphSnapshot::admit(definition).unwrap();
    let store = fixtures::Reader::new(&state.application);
    let parent_digest = "parent-digest".to_owned();
    for (id, mode) in [("detached-ready", "detach"), ("waiting-ready", "wait")] {
        let mut record = GraphRunRecord::create_with_id(snapshot.clone(), json!({}), id).unwrap();
        record.plugin_bindings_initialized = true;
        fixtures::create(&state.application, &record);
        let metadata = RunMetadata::graph_call_child(
            id.to_owned(),
            "child".into(),
            record.graph_digest.clone(),
            &child_bundle,
            crate::run_metadata::GraphCallSource {
                parent_run: "missing-parent".into(),
                parent_graph: "parent".into(),
                parent_graph_digest: parent_digest.clone(),
                node: "call".into(),
                invocation: 1,
                mode: mode.into(),
                root_run: "missing-parent".into(),
            },
        )
        .unwrap();
        metadata::save(&state.data_root, &metadata).unwrap();
    }
    state
        .application
        .recover_detached_at_startup()
        .await
        .unwrap();
    for _ in 0..100 {
        if store
            .load("detached-ready")
            .unwrap()
            .unwrap()
            .has_succeeded()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        store
            .load("detached-ready")
            .unwrap()
            .unwrap()
            .has_succeeded()
    );
    assert!(is_executing(&store.load("waiting-ready").unwrap().unwrap()));
}

#[tokio::test]
async fn paused_wait_child_resumes_independently_then_releases_parent() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    let child_definition = json!({
        "objective":"child", "entry":"slow", "agents":{},
        "ops":{"slow":{"run":"sh -c 'sleep 2'"},"finish":{"run":"true"}},
        "nodes":[{"id":"slow","op":"slow","plugins":[]},{"id":"finish","op":"finish","plugins":[]}],
        "edges":[{"from":"slow","to":"finish"}]
    });
    std::fs::write(child.join("graph.json"), child_definition.to_string()).unwrap();
    std::fs::write(
        child.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let parent = json!({
        "objective":"parent", "entry":"call", "agents":{},
        "ops":{"call":{"call":{"graph":"child","mode":"wait"}}},
        "nodes":[{"id":"call","op":"call","plugins":[]}], "edges":[]
    });
    write_graph_bundle(&state.bundle_root, &parent).unwrap();
    let app = router(state.clone());
    let (status, started) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let parent_id = started["run"].as_str().unwrap().to_owned();
    let store = fixtures::Reader::new(&state.application);
    let mut child_id = None;
    for _ in 0..500 {
        let (_, detail) = call(app.clone(), "GET", &format!("/runs/{parent_id}"), None).await;
        child_id = detail["calls"]
            .as_array()
            .and_then(|calls| calls.first())
            .and_then(|call| call["run"].as_str())
            .map(str::to_owned);
        if child_id.as_ref().is_some_and(|id| {
            store
                .load(id)
                .unwrap()
                .is_some_and(|child| is_executing(&child))
        }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let child_id = child_id.unwrap_or_else(|| {
        panic!(
            "wait child was not admitted: {:?}",
            store.load(&parent_id).unwrap()
        )
    });
    let (status, pause_response) = call(
        app.clone(),
        "POST",
        &format!("/runs/{child_id}/pause"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{pause_response:?}");
    for _ in 0..200 {
        if store
            .load(&child_id)
            .unwrap()
            .unwrap()
            .waits_reason("paused")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        store
            .load(&child_id)
            .unwrap()
            .unwrap()
            .waits_reason("paused")
    );
    assert!(
        store
            .load(&parent_id)
            .unwrap()
            .unwrap()
            .waits_for_children()
    );
    let (status, _) = call(
        app.clone(),
        "POST",
        &format!("/runs/{child_id}/resume"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    for _ in 0..300 {
        if store.load(&parent_id).unwrap().unwrap().has_succeeded() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(store.load(&child_id).unwrap().unwrap().has_succeeded());
    assert!(store.load(&parent_id).unwrap().unwrap().has_succeeded());
}

#[tokio::test]
async fn stopping_wait_parent_stops_only_its_wait_child() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    let child_definition = json!({
        "objective":"child", "entry":"slow", "agents":{},
        "ops":{"slow":{"run":"sh -c 'sleep 5'"}},
        "nodes":[{"id":"slow","op":"slow","plugins":[]}], "edges":[]
    });
    std::fs::write(child.join("graph.json"), child_definition.to_string()).unwrap();
    std::fs::write(
        child.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let parent = json!({"objective":"parent","entry":"call","agents":{},
        "ops":{"call":{"call":{"graph":"child","mode":"wait"}}},
        "nodes":[{"id":"call","op":"call","plugins":[]}],"edges":[]});
    write_graph_bundle(&state.bundle_root, &parent).unwrap();
    let app = router(state.clone());
    let (status, started) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let parent_id = started["run"].as_str().unwrap().to_owned();
    let store = fixtures::Reader::new(&state.application);
    let mut child_id = None;
    for _ in 0..300 {
        let (_, detail) = call(app.clone(), "GET", &format!("/runs/{parent_id}"), None).await;
        child_id = detail["calls"]
            .as_array()
            .and_then(|calls| calls.first())
            .and_then(|call| call["run"].as_str())
            .map(str::to_owned);
        if child_id.as_ref().is_some_and(|id| {
            store
                .load(id)
                .unwrap()
                .is_some_and(|child| is_executing(&child))
        }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let child_id = child_id.expect("wait child admission should be API-visible while running");
    let (status, _) = call(
        app.clone(),
        "POST",
        &format!("/runs/{parent_id}/stop"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    for _ in 0..200 {
        if store
            .load(&parent_id)
            .unwrap()
            .unwrap()
            .waits_reason("stopped")
            && store
                .load(&child_id)
                .unwrap()
                .unwrap()
                .waits_reason("stopped")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        store
            .load(&parent_id)
            .unwrap()
            .unwrap()
            .waits_reason("stopped")
    );
    assert!(
        store
            .load(&child_id)
            .unwrap()
            .unwrap()
            .waits_reason("stopped")
    );
}

#[tokio::test]
async fn stopping_detach_parent_does_not_cancel_detached_child() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    let child_definition = json!({"objective":"child","entry":"slow","agents":{},
        "ops":{"slow":{"run":"sh -c 'sleep 1'"}},
        "nodes":[{"id":"slow","op":"slow","plugins":[]}],"edges":[]});
    std::fs::write(child.join("graph.json"), child_definition.to_string()).unwrap();
    std::fs::write(
        child.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let parent = json!({"objective":"parent","entry":"call","agents":{},
        "ops":{"call":{"call":{"graph":"child","mode":"detach"}},"after":{"run":"sh -c 'sleep 3'"}},
        "nodes":[{"id":"call","op":"call","plugins":[]},{"id":"after","op":"after","plugins":[]}],
        "edges":[{"from":"call","to":"after"}]});
    write_graph_bundle(&state.bundle_root, &parent).unwrap();
    let app = router(state.clone());
    let (status, started) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let parent_id = started["run"].as_str().unwrap().to_owned();
    let store = fixtures::Reader::new(&state.application);
    let mut child_id = None;
    for _ in 0..300 {
        let (_, detail) = call(app.clone(), "GET", &format!("/runs/{parent_id}"), None).await;
        child_id = detail["calls"]
            .as_array()
            .and_then(|calls| calls.first())
            .and_then(|call| call["run"].as_str())
            .map(str::to_owned);
        if child_id.as_ref().is_some_and(|id| {
            store
                .load(id)
                .unwrap()
                .is_some_and(|child| is_executing(&child))
        }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let child_id = child_id.expect("detached child should be active while parent continues");
    let (status, _) = call(
        app.clone(),
        "POST",
        &format!("/runs/{parent_id}/stop"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    for _ in 0..300 {
        if store.load(&child_id).unwrap().unwrap().has_succeeded() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        store
            .load(&parent_id)
            .unwrap()
            .unwrap()
            .waits_reason("stopped")
    );
    assert!(store.load(&child_id).unwrap().unwrap().has_succeeded());
}

#[tokio::test]
async fn distinct_parent_call_identities_run_same_target_concurrently() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    let child_definition = json!({"objective":"child","entry":"slow","agents":{},
        "ops":{"slow":{"run":"sh -c 'sleep 1'"}},
        "nodes":[{"id":"slow","op":"slow","plugins":[]}],"edges":[]});
    std::fs::write(child.join("graph.json"), child_definition.to_string()).unwrap();
    std::fs::write(
        child.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let parent_definition = json!({"objective":"parent","entry":"call","agents":{},
        "ops":{"call":{"call":{"graph":"child","mode":"detach"}}},
        "nodes":[{"id":"call","op":"call","plugins":[]}],"edges":[]});
    write_graph_bundle(&state.bundle_root, &parent_definition).unwrap();
    let parent2 = root.path().join("parent2");
    write_graph_bundle(&parent2, &parent_definition).unwrap();
    let app = router(state.clone());
    let (_, started1) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    let (_, started2) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"parent2"}"#),
    )
    .await;
    let parent1_id = started1["run"].as_str().unwrap().to_owned();
    let parent2_id = started2["run"].as_str().unwrap().to_owned();
    let mut child_ids = Vec::new();
    for _ in 0..200 {
        child_ids = state
            .application
            .child_metadata(&parent1_id)
            .unwrap()
            .into_iter()
            .chain(state.application.child_metadata(&parent2_id).unwrap())
            .map(|metadata| metadata.run_id)
            .collect();
        if child_ids.len() == 2
            && child_ids.iter().all(|id| {
                fixtures::Reader::new(&state.application)
                    .load(id)
                    .unwrap()
                    .is_some_and(|record| is_executing(&record))
            })
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(child_ids.len(), 2);
    assert_ne!(child_ids[0], child_ids[1]);
    let store = fixtures::Reader::new(&state.application);
    for _ in 0..200 {
        if child_ids.iter().all(|id| {
            store
                .load(id)
                .unwrap()
                .is_some_and(|record| record.has_succeeded())
        }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        child_ids
            .iter()
            .all(|id| store.load(id).unwrap().unwrap().has_succeeded())
    );
}

#[tokio::test]
async fn stopped_and_budget_stopped_runs_block_graph_retrigger_but_allow_update() {
    let _env_guard = PROCESS_ENV.lock().await;
    let _environment = crate::test_support::Environment::cleared(&[
        "ANCHOR_RUNNER_GRAPH_NAME",
        "ANCHOR_RUNNER_BUNDLE_ROOT",
    ]);
    let (_root, state) = fixture();
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    for (run_id, status) in [("stopped-child", "stopped"), ("budget-child", "budget")] {
        let mut record =
            GraphRunRecord::create_with_id(bundle.snapshot.clone(), json!({}), run_id).unwrap();
        record.plugin_bindings_initialized = true;
        fixtures::create(&state.application, &record);
        fixtures::control(&state.application, run_id, "stop", status);
        let metadata = RunMetadata::graph_call_child(
            run_id.into(),
            state.graph_name.clone(),
            record.graph_digest.clone(),
            &state.bundle_root,
            crate::run_metadata::GraphCallSource {
                parent_run: "parent-run".into(),
                parent_graph: "parent".into(),
                parent_graph_digest: "parent-digest".into(),
                node: "call".into(),
                invocation: 1,
                mode: "detach".into(),
                root_run: "parent-run".into(),
            },
        )
        .unwrap();
        metadata::save(&state.data_root, &metadata).unwrap();
    }
    assert!(
        state
            .application
            .graph_admission_lease(&state.bundle_root)
            .is_ok()
    );
    let app = router(state.clone());
    let (status, _) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, updated) = call(
        app,
        "PUT",
        "/graphs/fixture",
        Some(
            &json!({"definition":{
                "objective":"changed","entry":"work","agents":{},"ops":{"work":{"run":"true"}},
                "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]
            }})
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
}

#[tokio::test]
async fn admission_is_per_graph_and_same_graph_is_atomic() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(_root.path());
    let mut definition: Value =
        serde_json::from_slice(&std::fs::read(state.bundle_root.join("graph.json")).unwrap())
            .unwrap();
    definition["ops"]["work"]["run"] = json!("sh -c 'sleep 0.2'");
    std::fs::write(state.bundle_root.join("graph.json"), definition.to_string()).unwrap();
    let app = router(state.clone());
    let (left, right) = tokio::join!(
        call(
            app.clone(),
            "POST",
            "/trigger",
            Some(r#"{"graph":"fixture"}"#)
        ),
        call(
            app.clone(),
            "POST",
            "/trigger",
            Some(r#"{"graph":"fixture"}"#)
        ),
    );
    assert_eq!(
        [left.0, right.0]
            .into_iter()
            .filter(|status| *status == StatusCode::ACCEPTED)
            .count(),
        1
    );
    assert_eq!(
        [left.0, right.0]
            .into_iter()
            .filter(|status| *status == StatusCode::CONFLICT)
            .count(),
        1
    );
    let first = if left.0 == StatusCode::ACCEPTED {
        left.1
    } else {
        right.1
    };
    write_graph_bundle(&state.catalog_root.join("other"), &definition).unwrap();
    let (status, second) = call(app, "POST", "/trigger", Some(r#"{"graph":"other"}"#)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_ne!(first["run"], second["run"]);
    assert_eq!(state.application.active_runs(None).await.len(), 2);
    wait_idle(&state).await;
    let records = state.application.records().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].1.graph_digest, records[1].1.graph_digest);
    assert!(
        records
            .iter()
            .all(|(id, _)| fixtures::view(&state.application, id).has_succeeded())
    );
}

async fn wait_idle(state: &ApiState) {
    for _ in 0..300 {
        if state.application.active_runs(None).await.is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("Run tasks did not finish");
}

fn two_node_definition(second_command: &str) -> Value {
    json!({"objective":"frozen","entry":"first","agents":{},
        "ops":{"first":{"run":"sh -c 'sleep 0.1; printf once >> count.txt'","writes":["count.txt"]},"second":{"run":second_command}},
        "nodes":[{"id":"first","op":"first","plugins":[]},{"id":"second","op":"second","plugins":[]}],
        "edges":[{"from":"first","to":"second"}]})
}

async fn start_and_pause(state: &ApiState, app: &Router) -> String {
    start_and_pause_request(
        state,
        app,
        r#"{"graph":"fixture","input":{"original":true}}"#,
    )
    .await
}

async fn start_and_pause_request(state: &ApiState, app: &Router, body: &str) -> String {
    let (status, accepted) = call(app.clone(), "POST", "/trigger", Some(body)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let run = accepted["run"].as_str().unwrap().to_owned();
    for _ in 0..200 {
        let record = fixtures::Reader::new(&state.application)
            .load(&run)
            .unwrap()
            .unwrap();
        if record.active()
            && record.view.dispatches().iter().any(|dispatch| {
                !matches!(crate::load_host_completion_fact(
                &state.data_root.join("facts"), &dispatch.key,
            ).unwrap(), anchor_runtime::graph::CompletionFact::NotStarted)
            })
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let (status, paused) = call(app.clone(), "POST", &format!("/runs/{run}/pause"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(paused["operation"], "pause");
    assert!(paused.get("alias").is_none());
    wait_idle(state).await;
    let record = fixtures::Reader::new(&state.application)
        .load(&run)
        .unwrap()
        .unwrap();
    assert!(record.waits_reason("paused"));
    assert_eq!(record.results.len(), 1);
    run
}

#[tokio::test]
async fn pause_resume_uses_same_frozen_run_after_graph_edit_and_service_object_restart() {
    let (root, mut state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let mut original = two_node_definition("sh -c 'cat /in/first/count.txt > frozen.txt'");
    original["ops"]["second"]["writes"] = json!(["frozen.txt"]);
    write_graph_bundle(&state.bundle_root, &original).unwrap();
    let app = router(state.clone());
    let run = start_and_pause(&state, &app).await;
    let (status, _) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let before = fixtures::Reader::new(&state.application)
        .load(&run)
        .unwrap()
        .unwrap();
    let (status, updated) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":two_node_definition("sh -c 'exit 9'")}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        updated["definition"]["ops"]["second"]["run"],
        "sh -c 'exit 9'"
    );
    // Editing the catalog definition does not change the paused Run's durable
    // snapshot; only subsequently admitted Runs see the updated command.
    drop(app);
    state.application = RunApplication::new(state.data_root.clone(), state.catalog_root.clone())
        .with_configured_graph(state.graph_name.clone(), state.bundle_root.clone());
    let restarted = state.clone();
    let restarted_app = router(restarted.clone());
    let (status, _) = call(
        restarted_app.clone(),
        "POST",
        &format!("/runs/{run}/resume"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    wait_idle(&restarted).await;
    let after = fixtures::Reader::new(&state.application)
        .load(&run)
        .unwrap()
        .unwrap();
    assert!(after.has_succeeded());
    assert_eq!(after.snapshot, before.snapshot);
    assert_eq!(after.input, before.input);
    assert_eq!(after.results["first"], before.results["first"]);
    assert_eq!(after.passes["first"], 1);
    let (status, file) = call(
        restarted_app.clone(),
        "GET",
        &format!("/runs/{run}/files/second/frozen.txt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(file["text"], "once");
    assert_eq!(
        call(restarted_app, "POST", &format!("/runs/{run}/resume"), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn active_run_can_update_graph_and_finishes_from_its_admitted_snapshot() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let original = json!({"objective":"active snapshot","entry":"work","agents":{},
        "ops":{"work":{"run":"sh -c 'sleep 0.4; printf original > snapshot.txt'","writes":["snapshot.txt"]}},
        "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]});
    write_graph_bundle(&state.bundle_root, &original).unwrap();
    let app = router(state.clone());
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let run = accepted["run"].as_str().unwrap().to_owned();
    for _ in 0..200 {
        if !state
            .application
            .active_runs(Some("fixture"))
            .await
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let replacement = json!({"objective":"future snapshot","entry":"work","agents":{},
        "ops":{"work":{"run":"sh -c 'exit 9'"}},
        "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]});
    let (status, body) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":replacement}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_idle(&state).await;
    let record = fixtures::Reader::new(&state.application)
        .load(&run)
        .unwrap()
        .unwrap();
    assert!(record.has_succeeded());
    assert_eq!(record.snapshot.objective, "active snapshot");
    assert_eq!(
        record.snapshot.ops["work"]["run"],
        "sh -c 'sleep 0.4; printf original > snapshot.txt'"
    );
    let (status, file) = call(
        app,
        "GET",
        &format!("/runs/{run}/files/work/snapshot.txt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{file}");
    assert_eq!(file["text"], "original");
}

#[tokio::test]
async fn graph_delete_is_rejected_while_its_own_run_is_unfinished() {
    let (_root, state) = fixture();
    let graph = "plugin-source";
    let path = state.catalog_root.join(graph);
    let definition = json!({"objective":"plugin source","entry":"work","agents":{},
    "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]});
    write_graph_bundle(&path, &definition).unwrap();
    let snapshot = GraphSnapshot::admit(definition).unwrap();
    let mut record =
        GraphRunRecord::create_with_id(snapshot, json!({}), "plugin-dependent-run").unwrap();
    record.plugin_bindings.insert(
        "source-plugin".into(),
        anchor_runtime::graph::PluginBinding {
            id: "source-plugin".into(),
            digest: "pinned-digest".into(),
            resources: vec!["resource.txt".into()],
            mcp_servers: vec![],
            authorization_digest: "fixture-authority"
                .try_into()
                .expect("non-empty fixture authority"),
        },
    );
    record.plugin_bindings_initialized = true;
    fixtures::create(&state.application, &record);
    fixtures::control(&state.application, "plugin-dependent-run", "stop", "paused");
    let run_metadata = RunMetadata::child(
        record.run_id.clone(),
        graph.into(),
        record.graph_digest.clone(),
        &path,
    )
    .unwrap();
    metadata::save(&state.data_root, &run_metadata).unwrap();
    let app = router(state.clone());
    let (status, error) = call(app, "DELETE", "/graphs/plugin-source", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(error["error"].as_str().unwrap().contains("unfinished Run"));
    assert!(path.exists());
    assert!(
        state
            .application
            .records()
            .unwrap()
            .iter()
            .any(|(id, _)| id == "plugin-dependent-run")
    );
    assert!(
        state
            .application
            .metadata("plugin-dependent-run")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn configured_bundle_alias_shares_identity_for_edit_run_and_delete() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let app = router(state.clone());

    // The configured name ("fixture") and the bundle's directory name ("bundle")
    // must resolve to one bundle.
    let (status, alias) = call(app.clone(), "GET", "/graphs/bundle", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(alias["definition"]["objective"], "fixture");

    // A Run admitted through the configured name is visible through the alias.
    let active = json!({"objective":"alias active","entry":"work","agents":{},
    "ops":{"work":{"run":"sh -c 'sleep 0.3'"}},
    "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]});
    write_graph_bundle(&state.bundle_root, &active).unwrap();
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let run = accepted["run"].as_str().unwrap().to_owned();
    for _ in 0..200 {
        if !state
            .application
            .active_runs(Some("bundle"))
            .await
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    assert!(
        state
            .application
            .active_runs(Some("bundle"))
            .await
            .contains(&run)
    );
    wait_idle(&state).await;

    // Editing through the alias edits the one configured bundle.
    let replacement = json!({"objective":"aliased","entry":"work","agents":{},
    "ops":{"work":{"run":"true"}},
    "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]});
    let (status, updated) = call(
        app.clone(),
        "PUT",
        "/graphs/bundle",
        Some(&json!({"definition": replacement}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let (status, canonical) = call(app.clone(), "GET", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(canonical["definition"]["objective"], "aliased");

    // A caller may name the configured bundle by its configured alias. The
    // physical-directory alias must still be protected from deletion.
    let caller_path = state.catalog_root.join("caller");
    let caller = json!({"objective":"caller","entry":"call","agents":{},
    "ops":{"call":{"call":{"graph":"fixture","mode":"wait","input":{}}}},
    "nodes":[{"id":"call","op":"call","plugins":[]}],"edges":[]});
    write_graph_bundle(&caller_path, &caller).unwrap();
    let (status, error) = call(app.clone(), "DELETE", "/graphs/bundle", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(error["error"].as_str().unwrap().contains("caller"));
    assert!(state.bundle_root.exists());

    // Removing the call releases the physical-directory alias for deletion.
    let caller_without_call = json!({"objective":"caller","entry":"work","agents":{},
    "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]});
    let (status, _) = call(
        app.clone(),
        "PUT",
        "/graphs/caller",
        Some(&json!({"definition":caller_without_call}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(app.clone(), "DELETE", "/graphs/bundle", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!state.bundle_root.exists());
    let (status, _) = call(app, "GET", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn missing_configured_bundle_starts_lists_missing_and_can_be_recreated() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    std::fs::remove_dir_all(&state.bundle_root).unwrap();

    // Simulate a service restart whose configured bundle was deleted.
    let mut restarted = state.clone();
    restarted.application =
        RunApplication::new(state.data_root.clone(), state.catalog_root.clone())
            .with_configured_graph(state.graph_name.clone(), state.bundle_root.clone());
    restarted
        .application
        .recover_detached_at_startup()
        .await
        .unwrap();
    let app = router(restarted.clone());

    let (status, listed) = call(app.clone(), "GET", "/graphs", None).await;
    assert_eq!(status, StatusCode::OK);
    let entry = listed["graphs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|graph| graph["graph"] == "fixture")
        .unwrap();
    assert_eq!(entry["missing"], true);
    let (status, _) = call(app.clone(), "GET", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Other Graphs still serve, and the same name can be recreated.
    let (status, _) = call(app.clone(), "POST", "/graphs", Some(r#"{"name":"other"}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = call(app.clone(), "GET", "/graphs/other", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, recreated) = call(
        app.clone(),
        "POST",
        "/graphs",
        Some(r#"{"name":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{recreated}");
    assert!(state.bundle_root.join("graph.json").exists());
    let (status, _) = call(app, "GET", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn graph_delete_removes_only_its_run_data_and_preserves_other_graph_runs() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());

    let definition = |objective: &str| {
        json!({"objective":objective,"entry":"work","agents":{},
        "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]})
    };

    // Graph "target": one Run with facts, io-harness sidecars and a workspace.
    let target_path = state.catalog_root.join("target");
    let target_definition = definition("target");
    write_graph_bundle(&target_path, &target_definition).unwrap();
    let target_snapshot = GraphSnapshot::admit(target_definition).unwrap();
    let target_run =
        GraphRunRecord::create_with_id(target_snapshot, json!({}), "target-run").unwrap();
    fixtures::create(&state.application, &target_run);
    fixtures::finish(&state.application, &target_run.run_id);
    metadata::save(
        &state.data_root,
        &RunMetadata::child(
            "target-run".into(),
            "target".into(),
            target_run.graph_digest.clone(),
            &target_path,
        )
        .unwrap(),
    )
    .unwrap();
    let target_key = InvocationKey {
        run_id: "target-run".into(),
        graph_digest: target_run.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    };
    let hash = format!("{:x}", Sha256::digest(target_key.durable_key().as_bytes()));
    let workspace = state.workspace_root.join("target-run");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("stale.txt"), b"x").unwrap();
    let io_facts = state.data_root.join("io-harness").join("facts");
    std::fs::create_dir_all(&io_facts).unwrap();
    std::fs::write(io_facts.join(format!("np1-{hash}.json")), b"{}").unwrap();
    std::fs::write(io_facts.join(format!("np1-{hash}.model.json")), b"{}").unwrap();
    let local_grants = crate::local_inputs::fact_path(&state.data_root, "target-run");
    std::fs::create_dir_all(local_grants.parent().unwrap()).unwrap();
    std::fs::write(&local_grants, b"{}").unwrap();
    std::fs::write(io_facts.join(format!("np1-{hash}.recovery-7.json")), b"{}").unwrap();
    let io_store = state.data_root.join("io-harness").join("store");
    std::fs::create_dir_all(&io_store).unwrap();
    std::fs::write(io_store.join(format!("np1-{hash}.sqlite3-wal")), b"x").unwrap();
    let recordings = io_store.join(format!("np1-{hash}.recordings"));
    let pending_recording = recordings.join("00000000000000000001");
    std::fs::create_dir_all(&pending_recording).unwrap();
    std::fs::write(pending_recording.join("request.json"), b"{}").unwrap();

    // A child Run created by "target-run" belongs to another Graph and survives.
    let grandchild_path = state.catalog_root.join("grandchild");
    let grandchild_definition = definition("grandchild");
    write_graph_bundle(&grandchild_path, &grandchild_definition).unwrap();
    let grandchild_snapshot = GraphSnapshot::admit(grandchild_definition).unwrap();
    let grandchild_run =
        GraphRunRecord::create_with_id(grandchild_snapshot, json!({}), "grandchild-run").unwrap();
    fixtures::create(&state.application, &grandchild_run);
    let mut grandchild_meta = RunMetadata::child(
        "grandchild-run".into(),
        "grandchild".into(),
        grandchild_run.graph_digest.clone(),
        &grandchild_path,
    )
    .unwrap();
    grandchild_meta.graph_call = Some(crate::run_metadata::GraphCallSource {
        parent_run: "target-run".into(),
        parent_graph: "target".into(),
        parent_graph_digest: target_run.graph_digest.clone(),
        node: "work".into(),
        invocation: 1,
        mode: "wait".into(),
        root_run: "target-run".into(),
    });
    metadata::save(&state.data_root, &grandchild_meta).unwrap();

    let app = router(state.clone());
    let (status, _) = call(app.clone(), "DELETE", "/graphs/target", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Target Run and its own data are gone.
    assert!(!target_path.exists());
    assert!(state.application.metadata("target-run").unwrap().is_none());
    assert!(
        state
            .application
            .records()
            .unwrap()
            .iter()
            .all(|(id, _)| id != "target-run")
    );
    assert!(!workspace.exists());
    assert!(!io_facts.join(format!("np1-{hash}.json")).exists());
    assert!(!io_facts.join(format!("np1-{hash}.model.json")).exists());
    assert!(!recordings.exists());
    assert!(!local_grants.exists());
    assert!(
        !io_facts
            .join(format!("np1-{hash}.recovery-7.json"))
            .exists()
    );
    assert!(!io_store.join(format!("np1-{hash}.sqlite3-wal")).exists());

    // The child Run of another Graph and its bundle survive.
    assert!(
        state
            .application
            .records()
            .unwrap()
            .iter()
            .any(|(id, _)| id == "grandchild-run")
    );
    assert!(grandchild_path.exists());
}

#[tokio::test]
async fn graph_delete_is_rejected_while_a_current_graph_definition_calls_it() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let target_path = state.catalog_root.join("target");
    let caller_path = state.catalog_root.join("caller");
    let definition = |objective: &str| {
        json!({"objective":objective,"entry":"work","agents":{},
        "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]})
    };
    write_graph_bundle(&target_path, &definition("target")).unwrap();
    let target_run = GraphRunRecord::create_with_id(
        GraphSnapshot::admit(definition("target")).unwrap(),
        json!({}),
        "terminal-target-run",
    )
    .unwrap();
    fixtures::create(&state.application, &target_run);
    fixtures::finish(&state.application, &target_run.run_id);
    metadata::save(
        &state.data_root,
        &RunMetadata::child(
            target_run.run_id.clone(),
            "target".into(),
            target_run.graph_digest.clone(),
            &target_path,
        )
        .unwrap(),
    )
    .unwrap();
    let caller_definition = json!({"objective":"caller","entry":"call","agents":{},
    "ops":{"call":{"call":{"graph":"target","mode":"wait","input":{}}}},
    "nodes":[{"id":"call","op":"call","plugins":[]}],"edges":[]});
    write_graph_bundle(&caller_path, &caller_definition).unwrap();

    let app = router(state.clone());
    let (status, error) = call(app.clone(), "DELETE", "/graphs/target", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(error["error"].as_str().unwrap().contains("Graph `caller`"));
    assert!(target_path.exists());
    assert!(
        fixtures::Reader::new(&state.application)
            .load("terminal-target-run")
            .unwrap()
            .is_some()
    );

    // Removing the call from the current definition makes deletion available.
    let (status, updated) = call(
        app.clone(),
        "PUT",
        "/graphs/caller",
        Some(&json!({"definition":definition("caller")}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let (status, _) = call(app, "DELETE", "/graphs/target", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!target_path.exists());
}

#[tokio::test]
async fn unfinished_run_snapshot_blocks_delete_after_current_definition_is_edited() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let target_path = state.catalog_root.join("target");
    let caller_path = state.catalog_root.join("caller");
    let target_definition = json!({"objective":"target","entry":"work","agents":{},
    "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]});
    let call_definition = json!({"objective":"caller","entry":"call","agents":{},
    "ops":{"call":{"call":{"graph":"target","mode":"wait","input":{}}}},
    "nodes":[{"id":"call","op":"call","plugins":[]}],"edges":[]});
    let current_definition = json!({"objective":"caller","entry":"work","agents":{},
    "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]});
    write_graph_bundle(&target_path, &target_definition).unwrap();
    write_graph_bundle(&caller_path, &current_definition).unwrap();
    let parent = GraphRunRecord::create_with_id(
        GraphSnapshot::admit(call_definition).unwrap(),
        json!({}),
        "paused-caller-run",
    )
    .unwrap();
    fixtures::create(&state.application, &parent);
    fixtures::control(&state.application, "paused-caller-run", "stop", "paused");
    metadata::save(
        &state.data_root,
        &RunMetadata::child(
            parent.run_id.clone(),
            "caller".into(),
            parent.graph_digest.clone(),
            &caller_path,
        )
        .unwrap(),
    )
    .unwrap();

    let app = router(state.clone());
    let (status, error) = call(app.clone(), "DELETE", "/graphs/target", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(error["error"].as_str().unwrap().contains("unfinished Run"));
    assert!(target_path.exists());

    // Completed historical snapshots remain, but do not keep the target alive.
    fixtures::control(&state.application, &parent.run_id, "resume", "");
    fixtures::call(
        &state.application,
        &parent.run_id,
        "call",
        anchor_runtime::graph::validator::CallSubmission::Completed {
            child_run_id: "historical-child".into(),
            output: json!({}),
            commit: anchor_runtime::graph::CommitRef {
                id: "historical-call".into(),
                node_id: "call".into(),
                invocation: 1,
            },
        },
    );
    let (status, _) = call(app, "DELETE", "/graphs/target", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!target_path.exists());
    assert!(
        fixtures::Reader::new(&state.application)
            .load("paused-caller-run")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn graph_delete_waits_out_a_concurrent_admission_lease() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let target_path = state.catalog_root.join("target");
    write_graph_bundle(
        &target_path,
        &json!({"objective":"target","entry":"work","agents":{},
        "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]}),
    )
    .unwrap();

    // Stand in for an in-flight child admission holding the same path lease.
    let held = state
        .application
        .graph_admission_lease(&target_path)
        .unwrap();
    let delete_path = target_path.clone();
    let app = router(state.clone());
    let delete = tokio::spawn(async move { call(app, "DELETE", "/graphs/target", None).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    drop(held);

    let (status, _) = delete.await.unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!delete_path.exists());
}

#[tokio::test]
async fn graph_reference_edit_and_delete_are_serialized_by_catalog_gate() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let target_path = state.catalog_root.join("target");
    let caller_path = state.catalog_root.join("caller");
    let simple = |objective: &str| {
        json!({"objective":objective,"entry":"work","agents":{},
        "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]})
    };
    write_graph_bundle(&target_path, &simple("target")).unwrap();
    write_graph_bundle(&caller_path, &simple("caller")).unwrap();
    let app = router(state.clone());

    // Queue a caller edit before deletion while holding the shared gate. FIFO
    // lock order makes the deletion observe the newly installed reference.
    let gate = state.application.graph_catalog_mutation_guard().await;
    let caller_with_call = json!({"objective":"caller","entry":"call","agents":{},
    "ops":{"call":{"call":{"graph":"target","mode":"wait","input":{}}}},
    "nodes":[{"id":"call","op":"call","plugins":[]}],"edges":[]});
    let update_body = json!({"definition":caller_with_call}).to_string();
    let update_app = app.clone();
    let mut update =
        tokio::spawn(
            async move { call(update_app, "PUT", "/graphs/caller", Some(&update_body)).await },
        );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut update)
            .await
            .is_err()
    );
    let delete_app = app.clone();
    let mut delete =
        tokio::spawn(async move { call(delete_app, "DELETE", "/graphs/target", None).await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut delete)
            .await
            .is_err()
    );
    drop(gate);
    let (status, updated) = update.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{updated}");
    let (status, error) = delete.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(target_path.exists());
}

#[tokio::test]
async fn missing_host_config_or_shell_authority_reject_before_acceptance() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    unsafe {
        env::remove_var("ANCHOR_RUNNER_ALLOWED_COMMANDS");
    }
    let app = router(state.clone());
    let (status, _) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(state.application.records().unwrap().is_empty());
    set_host_env(root.path());
    unsafe {
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "cat");
    }
    let definition = two_node_definition("/bin/cat /in/first/count.txt");
    write_graph_bundle(&state.bundle_root, &definition).unwrap();
    let (status, _) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(state.application.records().unwrap().is_empty());
    // The operator authorizes the shell. It can run an absolute
    // system command inside its sandbox without granting host paths.
    set_host_env(root.path());
    assert_eq!(
        call(app, "POST", "/trigger", Some(r#"{"graph":"fixture"}"#))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    wait_idle(&state).await;
}

#[tokio::test]
async fn legacy_unfinished_run_quarantines_admission_and_graph_creation() {
    let (_root, state) = fixture();
    std::fs::create_dir_all(state.data_root.join("runs")).unwrap();
    std::fs::write(
        state.data_root.join("runs/legacy.json"),
        b"legacy bytes are never decoded",
    )
    .unwrap();
    let app = router(state);
    for (uri, body) in [
        ("/trigger", r#"{"graph":"fixture"}"#),
        ("/graphs", r#"{"name":"other"}"#),
    ] {
        let (status, rejected) = call(app.clone(), "POST", uri, Some(body)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{rejected}");
        assert!(
            rejected["error"]
                .as_str()
                .unwrap()
                .contains("legacy_run_record"),
            "{rejected}"
        );
    }
}

#[tokio::test]
async fn downstream_failure_does_not_reclassify_committed_upstream_result() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    write_graph_bundle(&state.bundle_root, &two_node_definition("sh -c 'exit 7'")).unwrap();
    let app = router(state.clone());
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    wait_idle(&state).await;
    let run = accepted["run"].as_str().unwrap();
    let (_, detail) = call(app, "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(detail["projection"]["outcome"], "failed");
    assert_eq!(detail["state"]["nodes"]["first"]["submitted"], true);
    assert_eq!(
        detail["state"]["nodes"]["first"]["exit_status"],
        Value::Null
    );
    assert!(
        chrono::DateTime::parse_from_rfc3339(detail["state"]["updated"].as_str().unwrap()).is_ok()
    );
}

#[tokio::test]
async fn a_changed_plugin_does_not_block_resume_but_mismatched_metadata_does() {
    use crate::plugin_library::Catalog;
    use anchor_graph_host::PluginCatalog;
    let (_root, state) = fixture();
    let plugin = state.catalog_root.join("plugins/fixture");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Fixture","description":"test"}"#,
    )
    .unwrap();
    let bindings = Catalog::new(&state.catalog_root)
        .resolve(&["fixture".into()])
        .unwrap();
    let snapshot = GraphSnapshot::admit(json!({"objective":"plugin","entry":"work",
        "agents":{"worker":{"model":"fixture","instructions":"fixture"}},"ops":{},
        "nodes":[{"id":"work","agent":"worker","plugins":["fixture"]}],"edges":[]}))
    .unwrap();
    let mut record =
        GraphRunRecord::create_with_id(snapshot, Value::Null, "paused-plugin").unwrap();
    record.plugin_bindings_initialized = true;
    record.plugin_bindings = bindings
        .into_iter()
        .map(|binding| (binding.id.clone(), binding))
        .collect();
    fixtures::create(&state.application, &record);
    fixtures::control(&state.application, "paused-plugin", "stop", "paused");
    let metadata_path = state.data_root.join("run-metadata/paused-plugin.json");
    std::fs::create_dir_all(metadata_path.parent().unwrap()).unwrap();
    let mut metadata = json!({"format":1,"run_id":"paused-plugin","graph":"fixture",
        "graph_digest":record.graph_digest,"bundle_source":state.bundle_root.canonicalize().unwrap(),
        "created":"2026-10-02T00:00:00Z","trigger_source":"manual"});
    std::fs::write(&metadata_path, metadata.to_string()).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Changed","description":"test"}"#,
    )
    .unwrap();
    let app = router(state.clone());
    let (status, error) = call(app.clone(), "POST", "/runs/paused-plugin/resume", None).await;
    // A Run uses the *current* Plugin: a Plugin that changed while the Run was
    // parked is an observation, not a refusal.
    assert!(
        !error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Plugin manifest changed"),
        "a changed Plugin must not block resume: {status} {error}"
    );
    metadata["graph_digest"] = json!("wrong-digest");
    std::fs::write(&metadata_path, metadata.to_string()).unwrap();
    let (status, error) = call(app, "POST", "/runs/paused-plugin/resume", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("identity metadata")
    );
    assert_eq!(
        fixtures::Reader::new(&state.application)
            .load("paused-plugin")
            .unwrap()
            .unwrap(),
        record
    );
    assert!(state.application.active_runs(None).await.is_empty());
}

#[tokio::test]
async fn waiting_parent_rejects_child_grant_drift_then_restores_same_child_without_replay() {
    struct RestoreLocalInputRoot(Option<std::ffi::OsString>);
    impl Drop for RestoreLocalInputRoot {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(value) => env::set_var("ANCHOR_RUNNER_LOCAL_INPUTS_ROOT", value),
                    None => env::remove_var("ANCHOR_RUNNER_LOCAL_INPUTS_ROOT"),
                }
            }
        }
    }

    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let _local_root_guard = RestoreLocalInputRoot(env::var_os("ANCHOR_RUNNER_LOCAL_INPUTS_ROOT"));
    let operator_root = root.path().join("operator-workspaces");
    let original_source = root.path().join("original-history");
    let changed_source = root.path().join("changed-history");
    for directory in [
        operator_root.join("fixture"),
        operator_root.join("child"),
        original_source.clone(),
        changed_source.clone(),
    ] {
        std::fs::create_dir_all(directory).unwrap();
    }
    std::fs::write(original_source.join("source.txt"), "original-evidence").unwrap();
    std::fs::write(changed_source.join("source.txt"), "changed-evidence").unwrap();
    std::fs::write(operator_root.join("fixture/local-inputs.json"), "{}").unwrap();
    let child_grant = operator_root.join("child/local-inputs.json");
    let original_grant = json!({"finish":{"history":original_source}}).to_string();
    std::fs::write(&child_grant, &original_grant).unwrap();
    unsafe {
        env::set_var("ANCHOR_RUNNER_LOCAL_INPUTS_ROOT", &operator_root);
    }

    let child_bundle = root.path().join("child");
    write_graph_bundle(&child_bundle, &json!({
        "objective":"child grant recovery", "entry":"slow", "agents":{},
        "ops":{
            "slow":{"run":"printf once >> count.txt; sleep 2","writes":["count.txt"]},
            "finish":{"run":"cat /local-inputs/history/source.txt > result.txt; cat /in/slow/count.txt","writes":["result.txt"]}
        },
        "nodes":[{"id":"slow","op":"slow","plugins":[]},{"id":"finish","op":"finish","plugins":[]}],
        "edges":[{"from":"slow","to":"finish"}]
    })).unwrap();
    write_graph_bundle(
        &state.bundle_root,
        &json!({
            "objective":"parent grant recovery", "entry":"call", "agents":{},
            "ops":{"call":{"call":{"graph":"child","mode":"wait"}}},
            "nodes":[{"id":"call","op":"call","plugins":[]}], "edges":[]
        }),
    )
    .unwrap();
    let app = router(state.clone());
    let (status, started) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let parent_id = started["run"].as_str().unwrap().to_owned();
    let store = fixtures::Reader::new(&state.application);
    let artifacts = HostArtifacts::new(
        state.data_root.join("artifacts"),
        state.workspace_root.clone(),
    );

    // Pause only after the first command has made its visible local effect.
    let mut child_id = None;
    let mut first_effect_observed = false;
    for _ in 0..500 {
        child_id = state
            .application
            .child_metadata(&parent_id)
            .unwrap()
            .first()
            .map(|metadata| metadata.run_id.clone());
        if let Some(id) = &child_id
            && let Some(child) = store.load(id).unwrap()
            && is_executing(&child)
            && let Some(cursor) = child.view.dispatches().first()
            && cursor.node_id == "slow"
            && std::fs::read(
                artifacts
                    .workspace_path(&cursor.key)
                    .unwrap()
                    .join("count.txt"),
            )
            .is_ok_and(|bytes| bytes == b"once")
        {
            first_effect_observed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        first_effect_observed,
        "child first command did not reach its visible effect; parent={:?}; child={:?}",
        store.load(&parent_id).unwrap(),
        child_id.as_ref().and_then(|id| store.load(id).unwrap())
    );
    let child_id = child_id.expect("wait child was not admitted");
    let (status, paused) = call(
        app.clone(),
        "POST",
        &format!("/runs/{child_id}/pause"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{paused}");
    wait_idle(&state).await;
    let parent_before = store.load(&parent_id).unwrap().unwrap();
    let child_before = store.load(&child_id).unwrap().unwrap();
    assert!(parent_before.waits_for_children());
    assert!(child_before.waits_reason("paused"));
    assert_eq!(child_before.results["slow"].len(), 1);
    let first_commit = child_before.results["slow"][0].commit.clone();
    let first_key = child_before.results["slow"][0].key.clone();
    assert_eq!(
        std::fs::read(
            artifacts
                .files_path(&first_commit)
                .unwrap()
                .join("count.txt")
        )
        .unwrap(),
        b"once"
    );
    let parent_record_path = kernel_record_path(&state, &parent_id);
    let child_record_path = kernel_record_path(&state, &child_id);
    let parent_bytes = std::fs::read(&parent_record_path).unwrap();
    let child_bytes = std::fs::read(&child_record_path).unwrap();

    std::fs::write(
        &child_grant,
        json!({"finish":{"history":changed_source}}).to_string(),
    )
    .unwrap();
    let (status, refused) = call(
        app.clone(),
        "POST",
        &format!("/runs/{parent_id}/resume"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(refused.to_string().contains("grants"), "{refused}");
    assert_eq!(std::fs::read(&parent_record_path).unwrap(), parent_bytes);
    assert_eq!(std::fs::read(&child_record_path).unwrap(), child_bytes);
    assert!(state.application.active_runs(None).await.is_empty());

    std::fs::write(&child_grant, original_grant).unwrap();
    let (status, resumed) = call(
        app.clone(),
        "POST",
        &format!("/runs/{parent_id}/resume"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
    wait_idle(&state).await;
    let parent = store.load(&parent_id).unwrap().unwrap();
    let child = store.load(&child_id).unwrap().unwrap();
    assert!(parent.has_succeeded(), "{:?}", parent.phase);
    assert!(child.has_succeeded(), "{:?}", child.phase);
    assert_eq!(
        parent
            .graph_calls
            .values()
            .next()
            .unwrap()
            .child_run_id
            .as_deref(),
        Some(child_id.as_str())
    );
    assert_eq!(child.results["slow"].len(), 1);
    assert_eq!(child.results["slow"][0].commit, first_commit);
    assert_eq!(
        std::fs::read(
            artifacts
                .workspace_path(&first_key)
                .unwrap()
                .join("count.txt")
        )
        .unwrap(),
        b"once"
    );
    let second_commit = &child.results["finish"][0].commit;
    assert_eq!(
        std::fs::read(
            artifacts
                .files_path(second_commit)
                .unwrap()
                .join("result.txt")
        )
        .unwrap(),
        b"original-evidence"
    );
    assert_eq!(child.results["finish"][0].completion.submission, "once");
}
