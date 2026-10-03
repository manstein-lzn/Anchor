use super::*;
use crate::application::{RunMetadata, metadata};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tower::ServiceExt;

static PROCESS_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn set_host_env(root: &std::path::Path, state_root: &std::path::Path) {
    // The subprocess host currently obtains sandbox configuration from
    // process configuration. Serialize this test and set only isolated
    // temporary paths; production API requests cannot mutate them.
    unsafe {
        env::set_var("ANCHOR_RUNNER_STATE_ROOT", state_root);
        env::set_var("ANCHOR_RUNNER_WORKSPACE_ROOT", root.join("workspaces"));
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh,cat");
        env::set_var("ANCHOR_RUNNER_CATALOG_ROOT", root);
        env::remove_var("ANCHOR_MODEL_API_KEY");
        env::remove_var("ANCHOR_MODEL_URL");
        env::remove_var("ANCHOR_MODEL_NAME");
    }
}

fn fixture() -> (tempfile::TempDir, ApiState) {
    let root = tempdir().unwrap();
    let bundle = root.path().join("bundle");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("graph.json"), r#"{"objective":"fixture","entry":"work","agents":{},"ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]}"#).unwrap();
    std::fs::write(
        bundle.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let state = ApiState {
        bundle_root: bundle.clone(),
        catalog_root: root.path().to_path_buf(),
        application: RunApplication::new(root.path().join("state"), root.path().to_path_buf())
            .with_configured_graph("fixture".into(), bundle),
        data_root: root.path().join("state"),
        workspace_root: root.path().join("workspaces"),
        graph_name: "fixture".into(),
        loopback: true,
        api_keys: Vec::new(),
    };
    (root, state)
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

#[tokio::test]
async fn non_loopback_projection_requires_configured_bearer_key() {
    let (_root, mut state) = fixture();
    state.loopback = false;
    state.api_keys = vec!["secret-test-key".into()];
    let app = router(state);
    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let accepted = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header(header::AUTHORIZATION, "Bearer secret-test-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
}

#[tokio::test]
async fn rust_host_serves_workbench_before_key_entry_but_keeps_api_protected() {
    let (root, mut state) = fixture();
    state.loopback = false;
    state.api_keys = vec!["secret-test-key".into()];
    let web_root = root.path().join("web");
    std::fs::create_dir_all(web_root.join("assets")).unwrap();
    std::fs::write(web_root.join("index.html"), "<html>Anchor</html>").unwrap();
    std::fs::write(web_root.join("assets/app.js"), "console.log('app')").unwrap();
    let app = router_with_web_root(state, web_root);

    for path in ["/", "/assets/app.js"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/graphs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let accepted = app
        .oneshot(
            Request::builder()
                .uri("/graphs")
                .header(header::AUTHORIZATION, "Bearer secret-test-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
}

#[tokio::test]
async fn trigger_persists_run_and_projects_list_detail_and_control() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(_root.path(), &state.data_root);
    let mut definition: Value =
        serde_json::from_slice(&std::fs::read(state.bundle_root.join("graph.json")).unwrap())
            .unwrap();
    definition["ops"]["work"]["run"] = json!("sh -c 'printf rust-artifact > report.txt'");
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
        if status == StatusCode::OK
            && value["state"]["status"] != "running"
            && value["state"]["status"] != "ready"
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let (status, runs) = call(app.clone(), "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runs["runs"][0]["run"], run);
    let (status, detail) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["state"]["status"], "completed");
    assert_eq!(detail["state"]["nodes"]["work"]["files"][0], "report.txt");
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(run)
        .unwrap()
        .unwrap();
    let key = &record.results["work"][0].key;
    let io_store = state.data_root.join("io-harness/store");
    std::fs::create_dir_all(&io_store).unwrap();
    let stem = format!("np1-{:x}", Sha256::digest(key.durable_key().as_bytes()));
    let trace_store = io_harness::Store::open(io_store.join(format!("{stem}.sqlite3"))).unwrap();
    let trace_run = trace_store.start_run("trace fixture", "workspace").unwrap();
    trace_store
        .record_step_turn(
            trace_run,
            &io_harness::AssistantTurn::new(
                1,
                Some("Inspecting the task now."),
                vec![io_harness::ToolCall {
                    name: "anchor_run".into(),
                    arguments: json!({"command":"cat report.txt"}),
                }],
            ),
        )
        .unwrap();
    trace_store
        .record_observations(
            trace_run,
            &[io_harness::context::Observation::new(
                1,
                io_harness::context::ObsKind::Tool,
                Some("anchor_run".into()),
                "tool returned report.txt",
                io_harness::context::Origin::Tool,
            )],
        )
        .unwrap();
    std::fs::write(io_store.join(format!("{stem}.run")), trace_run.to_string()).unwrap();
    drop(trace_store);
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
    let (status, body) = call(
        app.clone(),
        "GET",
        &format!("/runs/{run}/files/work/report.txt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], "rust-artifact");
    let download = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/runs/{run}/files/work/report.txt?download=1"))
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
    let (status, timeline) = call(app.clone(), "GET", "/timeline?days=30", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(timeline["runs"][0]["run"], run);
    assert_eq!(timeline["capabilities"]["scheduling"], false);
    let traversal = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/runs/{run}/files/work/%2e%2e/manifest.json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(traversal.status(), StatusCode::BAD_REQUEST);
    wait_idle(&state).await;
    let (status, _) = call(app.clone(), "POST", &format!("/runs/{run}/pause"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(app.clone(), "DELETE", &format!("/runs/{run}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(app, "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn op_call_wait_projects_durable_child_run_through_host_api() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(
        child.join("graph.json"),
        r#"{"objective":"child objective","entry":"work","agents":{},"ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]}"#,
    ).unwrap();
    std::fs::write(
        child.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let parent = json!({
        "objective":"parent",
        "entry":"call",
        "agents":{},
        "ops":{"call":{"call":{"graph":"child","mode":"wait","input":{"from":"parent"}}}},
        "nodes":[{"id":"call","op":"call","plugins":[]}],
        "edges":[]
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
    let mut child_id = String::new();
    for _ in 0..200 {
        let parent_record = FileRunStore::new(state.data_root.join("runs"))
            .load(&parent_id)
            .unwrap()
            .unwrap();
        if let Some(call) = parent_record.graph_calls.values().next() {
            child_id = call.child_run_id.clone().unwrap_or_default();
        }
        if !child_id.is_empty()
            && FileRunStore::new(state.data_root.join("runs"))
                .load(&child_id)
                .unwrap()
                .is_some_and(|record| record.status == RunStatus::Completed)
            && parent_record.status == RunStatus::Completed
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !child_id.is_empty(),
        "parent did not admit child: {:?}",
        FileRunStore::new(state.data_root.join("runs"))
            .load(&parent_id)
            .unwrap()
            .unwrap()
    );
    let store = FileRunStore::new(state.data_root.join("runs"));
    let child_record = store.load(&child_id).unwrap().unwrap();
    assert_eq!(child_record.status, RunStatus::Completed);
    assert_eq!(child_record.input["from"], "parent");
    let metadata = state.application.metadata(&child_id).unwrap().unwrap();
    assert_eq!(metadata.graph, "child");
    assert_eq!(metadata.trigger_source, "graph_call");
    let (status, detail) = call(app.clone(), "GET", &format!("/runs/{child_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["graph"], "child");
    let (status, listing) = call(app, "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listing["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["run"] == child_id && run["graph"] == "child")
    );
}

#[tokio::test]
async fn op_call_detach_dispatches_child_and_projects_parent_relation() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(child.join("graph.json"), r#"{"objective":"child","entry":"work","agents":{},"ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]}"#).unwrap();
    std::fs::write(
        child.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let parent = json!({
        "objective":"parent", "entry":"call", "agents":{},
        "ops":{"call":{"call":{"graph":"child","mode":"detach"}}},
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
    let mut child_id = None;
    for _ in 0..200 {
        let record = FileRunStore::new(state.data_root.join("runs"))
            .load(&parent_id)
            .unwrap()
            .unwrap();
        if record.status == RunStatus::Completed {
            child_id = record
                .graph_calls
                .values()
                .next()
                .and_then(|call| call.child_run_id.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let child_id = child_id.expect("detach should durably admit a child");
    for _ in 0..200 {
        if FileRunStore::new(state.data_root.join("runs"))
            .load(&child_id)
            .unwrap()
            .is_some_and(|child| child.status == RunStatus::Completed)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let child_record = FileRunStore::new(state.data_root.join("runs"))
        .load(&child_id)
        .unwrap()
        .unwrap();
    assert_eq!(child_record.status, RunStatus::Completed);
    let metadata = state.application.metadata(&child_id).unwrap().unwrap();
    assert_eq!(metadata.trigger_source, "graph_call");
    let (status, detail) = call(app, "GET", &format!("/runs/{child_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["graph"], "child");
    assert_eq!(detail["state"]["trigger"]["mode"], "detach");
    assert_eq!(detail["state"]["trigger"]["run"], parent_id);
    let parent_detail_app = router(state.clone());
    let (status, parent_detail) = call(
        parent_detail_app,
        "GET",
        &format!("/runs/{parent_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parent_detail["calls"][0]["run"], child_id);
    assert_eq!(parent_detail["calls"][0]["mode"], "detach");
}

#[tokio::test]
async fn startup_recovery_dispatches_only_ready_detached_children() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
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
    let snapshot = anchor_runtime_rig::graph::GraphSnapshot::admit(definition).unwrap();
    let store = FileRunStore::new(state.data_root.join("runs"));
    let parent_digest = "parent-digest".to_owned();
    for (id, mode) in [("detached-ready", "detach"), ("waiting-ready", "wait")] {
        let mut record = GraphRunRecord::create_with_id(snapshot.clone(), json!({}), id).unwrap();
        record.plugin_bindings_initialized = true;
        store.save(&record).unwrap();
        let metadata = RunMetadata::graph_call_child(
            id.to_owned(),
            "child".into(),
            record.graph_digest.clone(),
            &child_bundle,
            crate::application::metadata::GraphCallSource {
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
        if store.load("detached-ready").unwrap().unwrap().status == RunStatus::Completed {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        store.load("detached-ready").unwrap().unwrap().status,
        RunStatus::Completed
    );
    assert_eq!(
        store.load("waiting-ready").unwrap().unwrap().status,
        RunStatus::Ready
    );
}

#[tokio::test]
async fn paused_wait_child_resumes_independently_then_releases_parent() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
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
    let store = FileRunStore::new(state.data_root.join("runs"));
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
                .is_some_and(|child| child.status == RunStatus::Running)
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
        if store.load(&child_id).unwrap().unwrap().status == RunStatus::Paused {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        store.load(&child_id).unwrap().unwrap().status,
        RunStatus::Paused
    );
    assert_eq!(
        store.load(&parent_id).unwrap().unwrap().status,
        RunStatus::WaitingCall
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
        if store.load(&parent_id).unwrap().unwrap().status == RunStatus::Completed {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        store.load(&child_id).unwrap().unwrap().status,
        RunStatus::Completed
    );
    assert_eq!(
        store.load(&parent_id).unwrap().unwrap().status,
        RunStatus::Completed
    );
}

#[tokio::test]
async fn stopping_wait_parent_stops_only_its_wait_child() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
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
    let store = FileRunStore::new(state.data_root.join("runs"));
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
                .is_some_and(|child| child.status == RunStatus::Running)
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
        if store.load(&parent_id).unwrap().unwrap().status == RunStatus::Stopped
            && store.load(&child_id).unwrap().unwrap().status == RunStatus::Stopped
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        store.load(&parent_id).unwrap().unwrap().status,
        RunStatus::Stopped
    );
    assert_eq!(
        store.load(&child_id).unwrap().unwrap().status,
        RunStatus::Stopped
    );
}

#[tokio::test]
async fn stopping_detach_parent_does_not_cancel_detached_child() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
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
    let store = FileRunStore::new(state.data_root.join("runs"));
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
                .is_some_and(|child| child.status == RunStatus::Running)
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
        if store.load(&child_id).unwrap().unwrap().status == RunStatus::Completed {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        store.load(&parent_id).unwrap().unwrap().status,
        RunStatus::Stopped
    );
    assert_eq!(
        store.load(&child_id).unwrap().unwrap().status,
        RunStatus::Completed
    );
}

#[tokio::test]
async fn distinct_parent_call_identities_run_same_target_concurrently() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
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
                FileRunStore::new(state.data_root.join("runs"))
                    .load(id)
                    .unwrap()
                    .is_some_and(|record| record.status == RunStatus::Running)
            })
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(child_ids.len(), 2);
    assert_ne!(child_ids[0], child_ids[1]);
    let store = FileRunStore::new(state.data_root.join("runs"));
    for _ in 0..200 {
        if child_ids.iter().all(|id| {
            store
                .load(id)
                .unwrap()
                .is_some_and(|record| record.status == RunStatus::Completed)
        }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        child_ids
            .iter()
            .all(|id| store.load(id).unwrap().unwrap().status == RunStatus::Completed)
    );
}

#[tokio::test]
async fn stopped_and_budget_stopped_runs_block_graph_retrigger_but_allow_update() {
    let (_root, state) = fixture();
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let store = FileRunStore::new(state.data_root.join("runs"));
    for (run_id, status) in [
        ("stopped-child", RunStatus::Stopped),
        ("budget-child", RunStatus::BudgetStopped),
    ] {
        let mut record =
            GraphRunRecord::create_with_id(bundle.snapshot.clone(), json!({}), run_id).unwrap();
        record.status = status;
        if status == RunStatus::BudgetStopped {
            let node_id = record.snapshot.entry.clone();
            record.invocations.insert(node_id.clone(), 1);
            record.passes.insert(node_id.clone(), 1);
            record.cursor = Some(anchor_runtime_rig::graph::RunCursor {
                node_id: node_id.clone(),
                key: anchor_runtime_rig::graph::InvocationKey {
                    run_id: record.run_id.clone(),
                    graph_digest: record.graph_digest.clone(),
                    node_id,
                    invocation: 1,
                },
                input_commits: vec![],
                prepared_input: Value::Null,
            });
        }
        record.plugin_bindings_initialized = true;
        store.save(&record).unwrap();
        let metadata = RunMetadata::graph_call_child(
            run_id.into(),
            state.graph_name.clone(),
            record.graph_digest.clone(),
            &state.bundle_root,
            crate::application::metadata::GraphCallSource {
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
    let (status, _) = call(
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
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn admission_is_per_graph_and_same_graph_is_atomic() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(_root.path(), &state.data_root);
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
            .all(|(_, record)| record.status == RunStatus::Completed)
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
        "ops":{"first":{"run":"sh -c 'sleep 0.1; printf once >> count.txt'"},"second":{"run":second_command}},
        "nodes":[{"id":"first","op":"first","plugins":[]},{"id":"second","op":"second","plugins":[]}],
        "edges":[{"from":"first","to":"second"}]})
}

async fn start_and_pause(state: &ApiState, app: &Router) -> String {
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture","input":{"original":true}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let run = accepted["run"].as_str().unwrap().to_owned();
    for _ in 0..200 {
        let record = FileRunStore::new(state.data_root.join("runs"))
            .load(&run)
            .unwrap()
            .unwrap();
        if record.cursor.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let (status, _) = call(app.clone(), "POST", &format!("/runs/{run}/pause"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    wait_idle(state).await;
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(&run)
        .unwrap()
        .unwrap();
    assert_eq!(record.status, RunStatus::Paused);
    assert_eq!(record.results.len(), 1);
    run
}

#[tokio::test]
async fn pause_resume_uses_same_frozen_run_after_graph_edit_and_service_object_restart() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let original = two_node_definition("sh -c 'cat /in/first/count.txt > frozen.txt'");
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
    let before = FileRunStore::new(state.data_root.join("runs"))
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
    let mut restarted = state.clone();
    restarted.application =
        RunApplication::new(state.data_root.clone(), state.catalog_root.clone())
            .with_configured_graph(state.graph_name.clone(), state.bundle_root.clone());
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
    let after = FileRunStore::new(state.data_root.join("runs"))
        .load(&run)
        .unwrap()
        .unwrap();
    assert_eq!(after.status, RunStatus::Completed);
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
    set_host_env(root.path(), &state.data_root);
    let original = json!({"objective":"active snapshot","entry":"work","agents":{},
        "ops":{"work":{"run":"sh -c 'sleep 0.4; printf original > snapshot.txt'"}},
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
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(&run)
        .unwrap()
        .unwrap();
    assert_eq!(record.status, RunStatus::Completed);
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
    record.status = RunStatus::Paused;
    record.plugin_bindings.insert(
        "source-plugin".into(),
        anchor_runtime_rig::graph::PluginBinding {
            id: "source-plugin".into(),
            digest: "pinned-digest".into(),
            resources: vec!["resource.txt".into()],
            mcp_servers: vec![],
        },
    );
    record.plugin_bindings_initialized = true;
    FileRunStore::new(state.data_root.join("runs"))
        .save(&record)
        .unwrap();
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
    set_host_env(root.path(), &state.data_root);
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
    set_host_env(root.path(), &state.data_root);
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
    set_host_env(root.path(), &state.data_root);

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
    let mut target_run =
        GraphRunRecord::create_with_id(target_snapshot, json!({}), "target-run").unwrap();
    target_run.status = RunStatus::Completed;
    target_run.invocations.insert("work".into(), 1);
    FileRunStore::new(state.data_root.join("runs"))
        .save(&target_run)
        .unwrap();
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
    std::fs::write(io_facts.join(format!("np1-{hash}.recovery-7.json")), b"{}").unwrap();
    let io_store = state.data_root.join("io-harness").join("store");
    std::fs::create_dir_all(&io_store).unwrap();
    std::fs::write(io_store.join(format!("np1-{hash}.sqlite3-wal")), b"x").unwrap();

    // A child Run created by "target-run" belongs to another Graph and survives.
    let grandchild_path = state.catalog_root.join("grandchild");
    let grandchild_definition = definition("grandchild");
    write_graph_bundle(&grandchild_path, &grandchild_definition).unwrap();
    let grandchild_snapshot = GraphSnapshot::admit(grandchild_definition).unwrap();
    let grandchild_run =
        GraphRunRecord::create_with_id(grandchild_snapshot, json!({}), "grandchild-run").unwrap();
    FileRunStore::new(state.data_root.join("runs"))
        .save(&grandchild_run)
        .unwrap();
    let mut grandchild_meta = RunMetadata::child(
        "grandchild-run".into(),
        "grandchild".into(),
        grandchild_run.graph_digest.clone(),
        &grandchild_path,
    )
    .unwrap();
    grandchild_meta.graph_call = Some(crate::application::metadata::GraphCallSource {
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
    set_host_env(root.path(), &state.data_root);
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
    let mut target_run = target_run;
    target_run.status = RunStatus::Completed;
    FileRunStore::new(state.data_root.join("runs"))
        .save(&target_run)
        .unwrap();
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
        FileRunStore::new(state.data_root.join("runs"))
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
    set_host_env(root.path(), &state.data_root);
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
    let mut parent = GraphRunRecord::create_with_id(
        GraphSnapshot::admit(call_definition).unwrap(),
        json!({}),
        "paused-caller-run",
    )
    .unwrap();
    parent.status = RunStatus::Paused;
    FileRunStore::new(state.data_root.join("runs"))
        .save(&parent)
        .unwrap();
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
    parent.status = RunStatus::Completed;
    FileRunStore::new(state.data_root.join("runs"))
        .save(&parent)
        .unwrap();
    let (status, _) = call(app, "DELETE", "/graphs/target", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!target_path.exists());
    assert!(
        FileRunStore::new(state.data_root.join("runs"))
            .load("paused-caller-run")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn graph_delete_waits_out_a_concurrent_admission_lease() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
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
    set_host_env(root.path(), &state.data_root);
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
async fn missing_host_config_and_path_command_reject_before_acceptance() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
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
    set_host_env(root.path(), &state.data_root);
    let mut definition = two_node_definition("/bin/cat /in/first/count.txt");
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
    definition["ops"]["second"]["run"] = json!("cat /in/first/count.txt");
    write_graph_bundle(&state.bundle_root, &definition).unwrap();
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
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let record = GraphRunRecord::create_with_id(bundle.snapshot, Value::Null, "legacy").unwrap();
    FileRunStore::new(state.data_root.join("runs"))
        .save(&record)
        .unwrap();
    let app = router(state);
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/trigger",
            Some(r#"{"graph":"fixture"}"#)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(app, "POST", "/graphs", Some(r#"{"name":"other"}"#))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn waiting_recovery_projects_context_and_resume_continues_same_run() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    unsafe {
        env::set_var("ANCHOR_MODEL_API_KEY", "test-key");
        env::set_var("ANCHOR_MODEL_URL", "http://127.0.0.1:1/v1");
        env::set_var("ANCHOR_MODEL_NAME", "test-model");
    }
    let snapshot = GraphSnapshot::admit(json!({
        "objective":"recovery fixture",
        "entry":"work",
        "agents":{"worker":{"model":"fixture","instructions":"fixture"}},
        "ops":{},
        "nodes":[{"id":"work","agent":"worker","plugins":[]}],
        "edges":[]
    }))
    .unwrap();
    let mut record = GraphRunRecord::create_with_id(snapshot, Value::Null, "recovery-run").unwrap();
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    };
    record.invocations.insert("work".into(), 1);
    record.passes.insert("work".into(), 1);
    record.cursor = Some(anchor_runtime_rig::graph::RunCursor {
        node_id: "work".into(),
        key: key.clone(),
        input_commits: vec![],
        prepared_input: json!({"input":null,"committed_inputs":[]}),
    });
    record.status = RunStatus::WaitingRecovery;
    record
        .recovery
        .push(anchor_runtime_rig::graph::PendingRecovery {
            key: key.clone(),
            attempt: anchor_runtime_rig::graph::RecoveryAttempt {
                attempt_id: 1,
                step: 3,
                tool: "anchor_publish".into(),
                started_at: "2026-10-03T00:00:00.000Z".into(),
            },
        });
    let harness_root = state.data_root.join("io-harness");
    let store_root = harness_root.join("store");
    std::fs::create_dir_all(&store_root).unwrap();
    let stem = format!("np1-{:x}", Sha256::digest(key.durable_key().as_bytes()));
    let harness_path = store_root.join(format!("{stem}.sqlite3"));
    let harness = io_harness::Store::open(&harness_path).unwrap();
    let harness_run = harness.start_run("uncertain publish", "workspace").unwrap();
    let attempt_id = harness
        .open_attempt(
            harness_run,
            3,
            "anchor_publish",
            io_harness::ToolRecovery::Indeterminate,
        )
        .unwrap()
        .unwrap();
    let sibling_attempt_id = harness
        .open_attempt(
            harness_run,
            4,
            "anchor_update",
            io_harness::ToolRecovery::Indeterminate,
        )
        .unwrap()
        .unwrap();
    record.recovery[0].attempt.attempt_id = attempt_id;
    record
        .recovery
        .push(anchor_runtime_rig::graph::PendingRecovery {
            key,
            attempt: anchor_runtime_rig::graph::RecoveryAttempt {
                attempt_id: sibling_attempt_id,
                step: 4,
                tool: "anchor_update".into(),
                started_at: "2026-10-03T00:00:01.000Z".into(),
            },
        });
    std::fs::write(
        store_root.join(format!("{stem}.run")),
        harness_run.to_string(),
    )
    .unwrap();
    drop(harness);
    let store = FileRunStore::new(state.data_root.join("runs"));
    store.save(&record).unwrap();
    let metadata_path = state.data_root.join("run-metadata/recovery-run.json");
    std::fs::create_dir_all(metadata_path.parent().unwrap()).unwrap();
    std::fs::write(
        metadata_path,
        json!({"format":1,"run_id":"recovery-run","graph":"fixture",
            "graph_digest":record.graph_digest,"bundle_source":state.bundle_root.canonicalize().unwrap(),
            "created":"2026-10-03T00:00:00Z","trigger_source":"manual"}).to_string(),
    )
    .unwrap();

    let app = router(state.clone());
    let (status, detail) = call(app.clone(), "GET", "/runs/recovery-run", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["state"]["status"], "waiting_recovery");
    assert_eq!(
        detail["state"]["recovery"][0]["attempt"]["attempt_id"],
        attempt_id
    );
    assert_eq!(detail["state"]["recovery"][0]["key"]["node_id"], "work");

    // Resume is the normal user path. The host records synthetic tool
    // observations in the same io-harness run, then lets the Agent inspect the
    // workspace/external state and decide how to continue. The old operator
    // decision panel is not required to unlock this Run.
    let (status, _) = call(app.clone(), "POST", "/runs/recovery-run/resume", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    drop(root);
}

#[tokio::test]
async fn downstream_failure_does_not_reclassify_committed_upstream_result() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
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
    assert_eq!(detail["state"]["status"], "failed");
    assert_eq!(detail["state"]["nodes"]["first"]["submitted"], true);
    assert_eq!(
        detail["state"]["nodes"]["first"]["exit_status"],
        Value::Null
    );
    assert_eq!(detail["state"]["updated"], "");
}

#[tokio::test]
async fn changed_plugin_and_mismatched_metadata_block_resume_without_starting_work() {
    use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
    let (_root, state) = fixture();
    let plugin = state.bundle_root.join("plugins/fixture");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.json"), "{}").unwrap();
    let bindings = FilePluginCatalog::new(&state.bundle_root)
        .resolve(&["fixture".into()])
        .unwrap();
    let snapshot = GraphSnapshot::admit(json!({"objective":"plugin","entry":"work",
        "agents":{"worker":{"model":"fixture","instructions":"fixture"}},"ops":{},
        "nodes":[{"id":"work","agent":"worker","plugins":["fixture"]}],"edges":[]}))
    .unwrap();
    let mut record =
        GraphRunRecord::create_with_id(snapshot, Value::Null, "paused-plugin").unwrap();
    record.status = RunStatus::Paused;
    record.plugin_bindings_initialized = true;
    record.plugin_bindings = bindings
        .into_iter()
        .map(|binding| (binding.id.clone(), binding))
        .collect();
    FileRunStore::new(state.data_root.join("runs"))
        .save(&record)
        .unwrap();
    let metadata_path = state.data_root.join("run-metadata/paused-plugin.json");
    std::fs::create_dir_all(metadata_path.parent().unwrap()).unwrap();
    let mut metadata = json!({"format":1,"run_id":"paused-plugin","graph":"fixture",
        "graph_digest":record.graph_digest,"bundle_source":state.bundle_root.canonicalize().unwrap(),
        "created":"2026-10-02T00:00:00Z","trigger_source":"manual"});
    std::fs::write(&metadata_path, metadata.to_string()).unwrap();
    std::fs::write(plugin.join("plugin.json"), r#"{"changed":true}"#).unwrap();
    let app = router(state.clone());
    let (status, error) = call(app.clone(), "POST", "/runs/paused-plugin/resume", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("Plugin manifest changed")
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
        FileRunStore::new(state.data_root.join("runs"))
            .load("paused-plugin")
            .unwrap()
            .unwrap(),
        record
    );
    assert!(state.application.active_runs(None).await.is_empty());
}
