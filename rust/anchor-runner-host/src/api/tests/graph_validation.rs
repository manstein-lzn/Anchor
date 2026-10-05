use super::*;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

struct Environment(Vec<(&'static str, Option<OsString>)>);

impl Environment {
    fn cleared(names: &[&'static str]) -> Self {
        let previous = names
            .iter()
            .map(|name| (*name, env::var_os(name)))
            .collect();
        for name in names {
            unsafe { env::remove_var(name) };
        }
        Self(previous)
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            unsafe {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
    }
}

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
        "ANCHOR_RUNNER_LIBRARY_ROOT",
        "ANCHOR_MODEL_API_KEY",
        "ANCHOR_MODEL_URL",
        "ANCHOR_MODEL_NAME",
        "ANCHOR_RUNNER_STATE_ROOT",
        "ANCHOR_RUNNER_WORKSPACE_ROOT",
        "ANCHOR_RUNNER_ALLOWED_COMMANDS",
    ]);
    let (root, state) = fixture();
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
async fn four_original_business_graphs_are_admitted_without_provider_or_side_effects() {
    let _env_guard = PROCESS_ENV.lock().await;
    let _environment = Environment::cleared(&[
        "ANCHOR_RUNNER_LIBRARY_ROOT",
        "ANCHOR_MODEL_API_KEY",
        "ANCHOR_MODEL_URL",
        "ANCHOR_MODEL_NAME",
        "ANCHOR_RUNNER_STATE_ROOT",
        "ANCHOR_RUNNER_WORKSPACE_ROOT",
        "ANCHOR_RUNNER_ALLOWED_COMMANDS",
    ]);
    let (root, state) = fixture();
    install_skill(&state.catalog_root, "academic-research");
    install_skill(&state.catalog_root, "docmost");
    install_skill(&state.catalog_root, "wecom");
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
                "publish",
                "docmost"
            ]),
            "collect",
        ),
        (
            include_str!("../../../../../examples/graphs/wecom-assistant.json"),
            json!(["assistant"]),
            "assistant",
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
    let _environment = Environment::cleared(&["ANCHOR_RUNNER_LIBRARY_ROOT"]);
    let (root, state) = fixture();
    let plugin = install_skill(&state.catalog_root, "academic-research");
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
    let _environment = Environment::cleared(&["ANCHOR_RUNNER_LIBRARY_ROOT"]);
    let (root, state) = fixture();
    let catalog_plugin = install_skill(&state.catalog_root, "academic-research");
    std::fs::write(catalog_plugin.join("plugin.json"), r#"{"name":12}"#).unwrap();
    let library = root.path().join("operator-library");
    install_skill(&library, "academic-research");
    unsafe { env::set_var("ANCHOR_RUNNER_LIBRARY_ROOT", &library) };
    let app = router(state.clone());
    let definition = serde_json::from_str(include_str!(
        "../../../../../examples/graphs/plugin-research.json"
    ))
    .unwrap();
    let before = filesystem(root.path());
    let (status, result) = validate(&app, &definition).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(filesystem(root.path()), before);

    unsafe { env::remove_var("ANCHOR_RUNNER_LIBRARY_ROOT") };
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
        "ANCHOR_RUNNER_LIBRARY_ROOT",
        "ANCHOR_MODEL_API_KEY",
        "ANCHOR_MODEL_URL",
        "ANCHOR_RUNNER_ALLOWED_COMMANDS",
        "ANCHOR_VALIDATION_REQUIRED_SECRET",
    ]);
    let (root, state) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let plugin = install_skill(&state.catalog_root, "demo");
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
    let _environment = Environment::cleared(&["ANCHOR_RUNNER_LIBRARY_ROOT"]);
    let (root, state) = fixture();
    let lease = state
        .application
        .graph_admission_lease(&state.bundle_root)
        .unwrap();
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let record = GraphRunRecord::create_with_id(bundle.snapshot, json!({}), "unfinished").unwrap();
    FileRunStore::new(state.data_root.join("runs"))
        .save(&record)
        .unwrap();
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
    let _environment = Environment::cleared(&["ANCHOR_RUNNER_LIBRARY_ROOT"]);
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
async fn graph_validation_uses_existing_bearer_auth_without_mutation() {
    let _env_guard = PROCESS_ENV.lock().await;
    let _environment = Environment::cleared(&["ANCHOR_RUNNER_LIBRARY_ROOT"]);
    let (root, mut state) = fixture();
    state.loopback = false;
    let secret = "v".repeat(32);
    state.api_keys = vec![secret.clone()];
    let definition = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap()
        .authoring_definition;
    let app = router(state);
    let before = filesystem(root.path());
    for (authorization, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("Bearer wrong".to_owned()), StatusCode::UNAUTHORIZED),
        (Some(format!("Bearer {secret}")), StatusCode::OK),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/graph-validation")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(authorization) = authorization {
            request = request.header(header::AUTHORIZATION, authorization);
        }
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
        if expected == StatusCode::OK {
            assert_eq!(result["valid"], true);
        } else {
            assert!(result["error"].is_string());
        }
        assert_eq!(filesystem(root.path()), before);
    }
}
