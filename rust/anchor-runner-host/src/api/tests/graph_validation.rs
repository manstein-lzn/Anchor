use super::*;
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::test_support::Environment;

#[derive(Debug, PartialEq, Eq)]
enum Evidence {
    Directory(SystemTime, u32),
    File(SystemTime, u32, Vec<u8>),
    Link(SystemTime, PathBuf),
}

fn filesystem(root: &Path) -> BTreeMap<PathBuf, Evidence> {
    fn visit(path: &Path, evidence: &mut BTreeMap<PathBuf, Evidence>) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        let modified = metadata.modified().unwrap();
        let mode = metadata.permissions().mode();
        let item = if metadata.file_type().is_symlink() {
            Evidence::Link(modified, std::fs::read_link(path).unwrap())
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                visit(&entry.unwrap().path(), evidence);
            }
            Evidence::Directory(modified, mode)
        } else {
            Evidence::File(modified, mode, std::fs::read(path).unwrap())
        };
        evidence.insert(path.to_path_buf(), item);
    }
    let mut evidence = BTreeMap::new();
    visit(root, &mut evidence);
    evidence
}

fn install_skill(root: &Path, id: &str) -> PathBuf {
    if let Some(system) = anchor_graph_host::SystemPlugin::from_id(id) {
        crate::test_system_plugin(root, system);
        return root.join("plugins").join(id);
    }
    let plugin = root.join("plugins").join(id);
    std::fs::create_dir_all(plugin.join("skills/read")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Read","skills":"skills/"}"#,
    )
    .unwrap();
    std::fs::write(
        plugin.join("skills/read/SKILL.md"),
        "Read the declared resources.",
    )
    .unwrap();
    plugin
}

async fn validate(app: &Router, definition: &Value) -> (StatusCode, Value) {
    call(
        app.clone(),
        "POST",
        "/graph-validation",
        Some(&json!({"definition":definition}).to_string()),
    )
    .await
}

#[tokio::test]
async fn original_graphs_validate_without_provider_and_without_filesystem_changes() {
    let _env_guard = PROCESS_ENV.lock().await;
    let _environment = Environment::cleared(&[
        "ANCHOR_MODEL_API_KEY",
        "ANCHOR_MODEL_URL",
        "ANCHOR_MODEL_NAME",
        "ANCHOR_RUNNER_WORKSPACE_ROOT",
        "ANCHOR_RUNNER_ALLOWED_COMMANDS",
    ]);
    let (root, state) = fixture();
    install_skill(&state.catalog_root, "network");
    let app = router(state.clone());
    let before = filesystem(root.path());
    let cases = [
        (
            include_str!("../../../../../examples/graphs/revise-loop.json"),
            json!(["draft", "review", "done"]),
            "draft",
        ),
        (
            include_str!("../../../../../examples/graphs/survey-modular.json"),
            json!([
                "plan",
                "gather",
                "write/draft",
                "write/critique",
                "write/settle",
                "ship"
            ]),
            "plan",
        ),
    ];
    for (source, nodes, entry) in cases {
        let definition = serde_json::from_str(source).unwrap();
        let (status, result) = validate(&app, &definition).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result, json!({"valid":true,"nodes":nodes,"entry":entry}));
        assert_eq!(filesystem(root.path()), before);
        assert!(!state.data_root.exists());
        assert!(!state.workspace_root.exists());
        assert!(state.application.active_runs(None).await.is_empty());
    }
}

#[tokio::test]
async fn business_graph_examples_are_admitted_without_provider_or_side_effects() {
    let _env_guard = PROCESS_ENV.lock().await;
    let _environment = Environment::cleared(&[
        "ANCHOR_MODEL_API_KEY",
        "ANCHOR_MODEL_URL",
        "ANCHOR_MODEL_NAME",
        "ANCHOR_RUNNER_WORKSPACE_ROOT",
        "ANCHOR_RUNNER_ALLOWED_COMMANDS",
    ]);
    let (root, state) = fixture();
    install_skill(&state.catalog_root, "academic-research");
    install_skill(&state.catalog_root, "docmost");
    install_skill(&state.catalog_root, "wecom");
    install_skill(&state.catalog_root, "network");
    let app = router(state.clone());
    let before = filesystem(root.path());
    let cases = [
        (
            include_str!("../../../../../examples/graphs/deep-academic-research.json"),
            json!([
                "frame",
                "investigate",
                "challenge",
                "feedback",
                "synthesize",
                "review",
                "review-gate",
                "report"
            ]),
            "frame",
        ),
        (
            include_str!("../../../../../examples/graphs/rsi.json"),
            json!([
                "collect",
                "audit-context",
                "audit-fanout",
                "run-audit",
                "code-audit",
                "graph-audit",
                "plugin-audit",
                "research",
                "dependency-audit",
                "audit-join",
                "analyze",
                "review-fanout",
                "fact-review",
                "proposal-review",
                "review-join",
                "review",
                "gate",
                "publish"
            ]),
            "collect",
        ),
        (
            include_str!("../../../../../examples/graphs/weekly-work-report.json"),
            json!([
                "collect",
                "understand",
                "write",
                "review",
                "gate",
                "publish"
            ]),
            "collect",
        ),
        (
            include_str!("../../../../../examples/graphs/weekly-work-report-docmost.json"),
            json!(["validate", "report", "docmost"]),
            "validate",
        ),
        (
            include_str!("../../../../../examples/graphs/wecom-persistent-assistant.json"),
            json!(["wait_input", "assistant", "reply"]),
            "wait_input",
        ),
    ];
    for (source, nodes, entry) in cases {
        let definition = serde_json::from_str(source).unwrap();
        let (status, result) = validate(&app, &definition).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result, json!({"valid":true,"nodes":nodes,"entry":entry}));
        assert_eq!(filesystem(root.path()), before);
        assert!(!state.data_root.exists());
        assert!(!state.workspace_root.exists());
        assert!(state.application.active_runs(None).await.is_empty());
    }
}

#[tokio::test]
async fn catalog_plugins_are_resolved_and_invalid_resources_are_rejected_readonly() {
    let _env_guard = PROCESS_ENV.lock().await;

    let (root, state) = fixture();
    let plugin = install_skill(&state.catalog_root, "academic-research");
    install_skill(&state.catalog_root, "network");
    let app = router(state.clone());
    let original: Value = serde_json::from_str(include_str!(
        "../../../../../examples/graphs/plugin-research.json"
    ))
    .unwrap();
    let before = filesystem(root.path());
    let (status, result) = validate(&app, &original).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        result,
        json!({"valid":true,"nodes":["research"],"entry":"research"})
    );
    assert_eq!(filesystem(root.path()), before);

    let mut missing = original.clone();
    missing["nodes"][0]["plugins"] = json!(["not-installed"]);
    let (status, result) = validate(&app, &missing).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{result}");
    assert_eq!(result["valid"], false);
    assert!(result["error"].is_string());
    assert_eq!(filesystem(root.path()), before);

    std::fs::write(plugin.join(".env"), "PRIVATE=not-a-public-resource").unwrap();
    let before_invalid = filesystem(root.path());
    let (status, result) = validate(&app, &original).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{result}");
    assert_eq!(result["valid"], false);
    assert_eq!(filesystem(root.path()), before_invalid);
    assert!(!state.data_root.exists());
}

#[tokio::test]
async fn operator_library_has_the_same_precedence_as_graph_save() {
    let _env_guard = PROCESS_ENV.lock().await;

    let (root, mut state) = fixture();
    let catalog_plugin = install_skill(&state.catalog_root, "academic-research");
    std::fs::write(catalog_plugin.join("plugin.json"), r#"{"name":12}"#).unwrap();
    let library = root.path().join("operator-library");
    install_skill(&library, "academic-research");
    install_skill(&library, "network");
    let original = state.application.clone();
    state.application =
        state
            .application
            .clone()
            .with_host_configuration(Arc::new(crate::HostConfiguration {
                library_root: Some(library),
                bwrap: state.application.bwrap.clone(),
                ..Default::default()
            }));
    let app = router(state.clone());
    let definition = serde_json::from_str(include_str!(
        "../../../../../examples/graphs/plugin-research.json"
    ))
    .unwrap();
    let before = filesystem(root.path());
    let (status, result) = validate(&app, &definition).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(filesystem(root.path()), before);

    state.application = original;
    let app = router(state.clone());
    let (status, result) = validate(&app, &definition).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{result}");
    assert_eq!(result["valid"], false);
    assert_eq!(filesystem(root.path()), before);
}

#[tokio::test]
async fn validation_does_not_launch_commands_mcp_or_expand_transport_secrets() {
    use std::{io::ErrorKind, net::TcpListener};

    let _env_guard = PROCESS_ENV.lock().await;
    let _environment = Environment::cleared(&[
        "ANCHOR_MODEL_API_KEY",
        "ANCHOR_MODEL_URL",
        "ANCHOR_RUNNER_ALLOWED_COMMANDS",
        "ANCHOR_VALIDATION_REQUIRED_SECRET",
    ]);
    let (root, state) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let plugin = install_skill(&state.catalog_root, "demo");
    install_skill(&state.catalog_root, "network");
    let marker = root.path().join("must-not-be-created");
    let command = format!("touch {}", marker.display());
    std::fs::write(
        plugin.join(".mcp.json"),
        json!({"mcpServers":{
            "stdio":{"command":"sh","args":["-c",command],
                "env":{"SECRET":"${ANCHOR_VALIDATION_REQUIRED_SECRET}"}},
            "http":{"type":"http","url":format!("http://{}/mcp", listener.local_addr().unwrap()),
                "headers":{"X-Secret":"${ANCHOR_VALIDATION_REQUIRED_SECRET}"}}
        }})
        .to_string(),
    )
    .unwrap();
    let definition = json!({"objective":"static only","agents":{"reader":{"model":"unconfigured"}},
        "ops":{"finish":{"run":command}},
        "nodes":[{"id":"read","agent":"reader","plugins":["demo"]},{"id":"finish","op":"finish"}],
        "edges":[{"from":"read","to":"finish"}]});
    let app = router(state.clone());
    let before = filesystem(root.path());
    let (status, result) = validate(&app, &definition).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        result,
        json!({"valid":true,"nodes":["read","finish"],"entry":"read"})
    );
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    assert!(!marker.exists());
    assert_eq!(filesystem(root.path()), before);
    assert!(!state.data_root.exists());
    assert!(!state.workspace_root.exists());
}

#[tokio::test]
async fn existing_graph_lease_and_unfinished_run_do_not_conflict_with_validation() {
    let _env_guard = PROCESS_ENV.lock().await;

    let (root, state) = fixture();
    let lease = state
        .application
        .graph_admission_lease(&state.bundle_root)
        .unwrap();
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let record = GraphRunRecord::create_with_id(bundle.snapshot, json!({}), "unfinished").unwrap();
    fixtures::create(&state.application, &record);
    let app = router(state.clone());
    let before = filesystem(root.path());
    let (status, result) = validate(&app, &bundle.authoring_definition).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        result,
        json!({"valid":true,"nodes":["work"],"entry":"work"})
    );
    assert_eq!(filesystem(root.path()), before);
    assert_eq!(state.application.records().unwrap().len(), 1);
    drop(lease);
}

#[tokio::test]
async fn malformed_input_and_semantic_errors_have_distinct_readonly_results() {
    let _env_guard = PROCESS_ENV.lock().await;

    let (root, state) = fixture();
    let app = router(state.clone());
    let before = filesystem(root.path());
    for body in [
        "{",
        "[]",
        "null",
        "{}",
        r#"{"definition":null}"#,
        r#"{"definition":"text"}"#,
    ] {
        let (status, result) = call(app.clone(), "POST", "/graph-validation", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
        assert_eq!(result["valid"], false);
        assert!(result["error"].is_string());
        assert_eq!(filesystem(root.path()), before);
    }
    let invalid = [
        json!({}),
        json!({"objective":"empty","nodes":[]}),
        json!({"objective":"missing role","nodes":[{"id":"work","agent":"missing"}],"edges":[]}),
        json!({"objective":"invalid plugin owner","ops":{"work":{"run":"true"}},
            "nodes":[{"id":"work","op":"work","plugins":["missing"]}],"edges":[]}),
        serde_json::from_str(include_str!(
            "../../../../../examples/graphs/one-search.json"
        ))
        .unwrap(),
    ];
    for definition in invalid {
        let (status, result) = validate(&app, &definition).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{result}");
        assert_eq!(result["valid"], false);
        assert!(
            result["error"]
                .as_str()
                .is_some_and(|error| !error.is_empty())
        );
        assert_eq!(filesystem(root.path()), before);
    }
}

#[tokio::test]
async fn graph_validation_does_not_mutate() {
    let _env_guard = PROCESS_ENV.lock().await;

    let (root, state) = fixture();
    let definition = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap()
        .authoring_definition;
    let app = router(state);
    let before = filesystem(root.path());
    for expected in [StatusCode::OK, StatusCode::OK] {
        let request = Request::builder()
            .method("POST")
            .uri("/graph-validation")
            .header(header::CONTENT_TYPE, "application/json");
        let response = app
            .clone()
            .oneshot(
                request
                    .body(Body::from(json!({"definition":definition}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let result: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["valid"], true);
        assert_eq!(filesystem(root.path()), before);
    }
}

#[tokio::test]
async fn http_validate_create_and_update_share_host_model_reference_admission() {
    let (_root, mut state) = fixture();
    state.application = state
        .application
        .with_model_configuration(state.model_configuration.clone(), None);
    let app = router(state.clone());
    let valid = json!({"entry":"work", "agents":{"worker":{}},
        "nodes":[{"id":"work","agent":"worker"}], "edges":[]});
    let (status, created) = call(
        app.clone(),
        "POST",
        "/graphs",
        Some(&json!({"name":"saved-model", "definition":valid}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    for model in ["claude-sonnet-4-5", "default"] {
        let mut invalid = valid.clone();
        invalid["agents"]["worker"]["model"] = json!(model);
        for (method, path, body) in [
            ("POST", "/graph-validation", json!({"definition":invalid})),
            (
                "POST",
                "/graphs",
                json!({"name":"invalid-model","definition":invalid}),
            ),
            ("PUT", "/graphs/saved-model", json!({"definition":invalid})),
        ] {
            let (status, result) = call(app.clone(), method, path, Some(&body.to_string())).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{result}");
            let error = result["error"].as_str().unwrap();
            assert!(
                error.contains(model)
                    && error.contains(crate::model_configuration::MODEL_REFERENCES),
                "{result}"
            );
        }
    }
    assert_eq!(
        state.application.read_graph("saved-model").unwrap()["definition"],
        GraphSnapshot::normalize_network_authoring(valid, false)
    );
    assert!(
        !state
            .application
            .graph_path_checked("invalid-model")
            .unwrap()
            .exists()
    );
}
