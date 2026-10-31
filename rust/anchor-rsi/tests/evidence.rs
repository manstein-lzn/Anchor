use anchor_kernel::{FileStore, Outcome, Phase, Reason, Store};
use anchor_rsi::{
    ecosystem::Ecosystem,
    evidence::{Config, Evidence},
    mcp::RsiService,
};
use anchor_runtime::graph::{
    CommitRef, GraphRunRecord, GraphSnapshot, InvocationKey, NodeCompletion, RunResult, validator,
};
use serde_json::{Value, json};
use std::{fs, path::Path, sync::Arc, time::Duration};

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}
fn setup() -> (tempfile::TempDir, Config) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let data = temp.path().join("data");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&data).unwrap();
    write(&source, "src/new.rs", "one\ntwo\nthree\n");
    write(
        &source,
        "Cargo.toml",
        "[dependencies]\nserde=\"1\"\n[dev-dependencies]\ntempfile=\"3\"\n",
    );
    write(
        &source,
        "pyproject.toml",
        "[project]\ndependencies=[\"httpx>=0.27\"]\n",
    );
    write(
        &source,
        "package.json",
        r#"{"dependencies":{"react":"19"}}"#,
    );
    let evidence = temp.path().join("evidence");
    (
        temp,
        Config {
            source,
            data,
            rust_state: None,
            previous: None,
            evidence,
        },
    )
}

fn graph_record(run_id: &str, nodes: &[&str]) -> GraphRunRecord {
    let graph_nodes = nodes
        .iter()
        .map(|id| json!({"id":id,"agent":"worker"}))
        .collect::<Vec<_>>();
    let edges = nodes
        .windows(2)
        .map(|pair| json!({"from":pair[0],"to":pair[1]}))
        .collect::<Vec<_>>();
    let snapshot = GraphSnapshot::from_authoring(json!({
        "objective":"weekly review of Anchor",
        "entry":nodes[0],
        "agents":{"worker":{"model":"models.default","instructions":"private prompt"}},
        "nodes":graph_nodes,
        "edges":edges
    }))
    .unwrap();
    GraphRunRecord::create_with_id(snapshot, Value::Null, run_id).unwrap()
}

fn run_result(
    record: &GraphRunRecord,
    node: &str,
    invocation: u64,
    sequence: u64,
    output: Value,
) -> RunResult {
    RunResult {
        node_id: node.into(),
        key: InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: node.into(),
            invocation,
        },
        plugin_views: Default::default(),
        plugin_digests: Default::default(),
        completion: NodeCompletion {
            submission: String::new(),
            route: None,
            model_requests: 0,
            output,
        },
        interruption: None,
        commit: CommitRef {
            id: format!("artifact-{node}-{invocation}"),
            node_id: node.into(),
            invocation,
        },
        sequence,
    }
}

fn store_run(state: &Path, record: &GraphRunRecord, phase: Phase) {
    let mut resource = validator::run_resource(record, 100).unwrap();
    resource.task.as_mut().unwrap().phase = phase;
    FileStore::new(state.join("kernel-runs"))
        .unwrap()
        .create(&resource)
        .unwrap();
}

#[test]
fn discovers_new_graphs_and_dependencies_and_freezes_source() {
    let (_temp, config) = setup();
    write(
        &config.data,
        "catalog/new-graph/graph.json",
        r#"{"objective":"new graph","agents":{},"nodes":[]}"#,
    );
    write(
        &config.data,
        "library/plugins/new/plugin.json",
        r#"{"description":"public plugin","api_key":"do-not-copy"}"#,
    );
    write(&config.data, "library/skills/new/SKILL.md", "public skill");
    write(
        &config.data,
        "state/schedules.json",
        r#"{"schedules":[{"graph":"new-graph","enabled":true}],"api_key":"schedule-secret"}"#,
    );
    let evidence = Evidence::collect(config.clone()).unwrap();
    assert_eq!(evidence.index("graphs", 0, 20).unwrap()["count"], 1);
    assert_eq!(evidence.index("plugins", 0, 20).unwrap()["count"], 2);
    assert!(evidence.dependencies.iter().any(|d| d.name == "httpx"));
    assert!(evidence.dependencies.iter().any(|d| d.name == "serde"));
    assert!(evidence.dependencies.iter().any(|d| d.name == "react"));
    let schedules = evidence
        .read("runs/schedules.json", 0, 200)
        .unwrap()
        .to_string();
    assert!(schedules.contains("new-graph"));
    assert!(!schedules.contains("schedule-secret"));
    fs::write(config.source.join("src/new.rs"), "changed").unwrap();
    assert_eq!(
        evidence.read("code/src/new.rs", 1, 1).unwrap()["lines"],
        json!([{"line":2,"text":"two"}])
    );
    let page = evidence.read("code/src/new.rs", 0, 1).unwrap();
    assert_eq!(page["truncated"], true);
    assert_eq!(page["next_offset"], 1);
    let plugin = evidence
        .read("plugins/plugins/new/plugin.json", 0, 200)
        .unwrap()
        .to_string();
    assert!(!plugin.contains("do-not-copy"));
    assert!(plugin.contains("REDACTED"));
}

#[test]
fn missing_required_roots_fail_and_optional_roots_record_limits() {
    let (_temp, mut config) = setup();
    config.source = config.source.join("missing");
    assert!(
        Evidence::collect(config.clone())
            .err()
            .unwrap()
            .contains("required source")
    );
    config.source = config.source.parent().unwrap().into();
    config.data = config.data.join("missing");
    assert!(
        Evidence::collect(config.clone())
            .err()
            .unwrap()
            .contains("required data")
    );
    config.data = config.data.parent().unwrap().into();
    config.previous = Some(config.data.join("missing"));
    let evidence = Evidence::collect(config).unwrap();
    assert!(evidence.issues.iter().any(|issue| {
        issue
            .to_string()
            .contains("previous-report root is missing")
    }));
}

#[test]
fn run_metadata_excludes_prompts_and_records_seven_day_basis() {
    let (_temp, mut config) = setup();
    let state = config.data.join("state");
    let mut record = graph_record("run-new", &["work"]);
    record.input = json!({"prompt":"private prompt","conversation":"private conversation"});
    store_run(&state, &record, Phase::Executing);
    write(
        &state,
        "kernel-run-observations/run-new.json",
        &json!({"updated":chrono::Utc::now().to_rfc3339()}).to_string(),
    );
    config.rust_state = Some(state.clone());
    let evidence_root = config.evidence.clone();
    let evidence = Evidence::collect(config).unwrap();
    let entry = evidence
        .entries
        .values()
        .find(|entry| entry.domain == "runs" && entry.path != "runs/schedules.json")
        .unwrap();
    let page = evidence.read(&entry.path, 0, 200).unwrap().to_string();
    // Prompt-ish and body fields stay out of the projection.
    for private in [
        "private exit details",
        "private prompt",
        "private conversation",
        "private cursor",
        "private model text",
        &state.display().to_string(),
    ] {
        assert!(!page.contains(private), "{private}");
    }
    let projected: Value =
        serde_json::from_str(&fs::read_to_string(evidence_root.join(&entry.frozen_file)).unwrap())
            .unwrap();
    assert_eq!(projected["status"], "running");
    assert_eq!(projected["error"], Value::Null);
    assert!(projected.get("reason").is_none());
    assert_eq!(projected["source_field_presence"]["started"], true);
    assert_eq!(projected["source_field_presence"]["updated"], true);
    assert_eq!(projected["results"], json!({}));
    assert!(projected.get("recovery").is_none());
    assert_eq!(projected["parallel"], Value::Null);
    assert_eq!(projected["source_locator"], "kernel-runs/run/run-new");
    assert!(page.contains("within_last_seven_days"));
    assert!(page.contains("true"));
}

#[cfg(unix)]
#[test]
fn rejects_credentials_symlinks_unknown_absolute_and_parent_paths() {
    use std::os::unix::fs::symlink;
    let (temp, config) = setup();
    write(&config.source, ".env", "secret-environment-value");
    write(&config.source, "secrets.json", r#"{"token":"private"}"#);
    write(
        &config.source,
        ".claude/worktrees/old-checkout/src/example.rs",
        "stale-checkout-code",
    );
    write(
        &config.source,
        "src/example.rs",
        "Bearer literal-token-123456\napi_key = \"key-literal\";",
    );
    write(temp.path(), "outside.txt", "outside-private");
    symlink(
        temp.path().join("outside.txt"),
        config.source.join("src/escape.txt"),
    )
    .unwrap();
    let evidence = Evidence::collect(config.clone()).unwrap();
    for path in [
        "code/.env",
        "code/secrets.json",
        "code/.claude/worktrees/old-checkout/src/example.rs",
        "code/src/escape.txt",
        "/etc/passwd",
        "code/../outside.txt",
        "unknown/file",
    ] {
        assert!(evidence.read(path, 0, 20).is_err());
    }
    let clean = evidence
        .read("code/src/example.rs", 0, 20)
        .unwrap()
        .to_string();
    assert!(!clean.contains("literal-token-123456"));
    assert!(!clean.contains("key-literal"));
    assert!(
        evidence
            .issues
            .iter()
            .any(|issue| issue.to_string().contains("symlink"))
    );
    assert!(
        Evidence::collect(config).is_err(),
        "nonempty evidence root cannot be reused"
    );
}

#[tokio::test]
async fn tool_audit_records_reads_and_rejections_without_secret_arguments() {
    let (_temp, config) = setup();
    let evidence_root = config.evidence.clone();
    let service = RsiService {
        evidence: Arc::new(Evidence::collect(config).unwrap()),
        ecosystem: Arc::new(Ecosystem::new().unwrap()),
    };
    service
        .dispatch(
            "rsi_read",
            json!({"path":"code/src/new.rs","offset":1,"limit":1}),
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .dispatch("rsi_read", json!({"path":"/secret/literal-token-private"}))
            .await
            .unwrap()["status"],
        "invalid_request"
    );
    let audit = fs::read_to_string(evidence_root.join("tool-calls.jsonl")).unwrap();
    assert!(audit.contains("code/src/new.rs"));
    assert!(!audit.contains("literal-token-private"));
    assert_eq!(audit.lines().count(), 2);
    let index = service
        .dispatch("rsi_index", json!({"domain":"code","limit":1}))
        .await
        .unwrap();
    assert_eq!(index["files"].as_array().unwrap().len(), 1);
    assert_eq!(index["next_offset"], 1);
    assert_eq!(
        service
            .dispatch("rsi_read", json!({"path":"code/src/new.rs","host":"/tmp"}))
            .await
            .unwrap()["status"],
        "invalid_request"
    );
    let ecosystem = service
        .dispatch("rsi_ecosystem", json!({"offset":1000,"limit":1}))
        .await
        .unwrap();
    let path = ecosystem["evidence_path"].as_str().unwrap();
    assert!(service.evidence.read(path, 0, 200).is_ok());
}

#[tokio::test]
async fn failed_network_is_evidence_not_success() {
    let research = Ecosystem::with_timeout(Duration::from_nanos(1)).unwrap();
    let result = research.fetch("https://pypi.org/pypi/httpx/json").await;
    assert_eq!(result["status"], "error");
    assert!(result.get("error").is_some());
    let denied = research.fetch("http://127.0.0.1:1/private").await;
    assert_eq!(denied["status"], "error");
    assert!(denied["error"].as_str().unwrap().contains("allowlist"));
}

#[test]
fn long_lines_cannot_bypass_read_pagination() {
    let (_temp, config) = setup();
    write(&config.source, "src/long.txt", &"x".repeat(65537));
    let evidence = Evidence::collect(config).unwrap();
    assert!(
        evidence
            .read("code/src/long.txt", 0, 200)
            .unwrap_err()
            .contains("64 KiB")
    );
}

#[tokio::test]
async fn real_http_mcp_service_lists_and_reads_frozen_evidence() {
    // Workspace feature unification enables RMCP's provider-free TLS client.
    // Use the same explicit crypto provider as the production MCP adapter.
    let _ = rustls::crypto::ring::default_provider().install_default();
    use rmcp::{
        ServiceExt,
        model::{CallToolRequestParams, ClientInfo},
        transport::{
            StreamableHttpClientTransport, StreamableHttpServerConfig, StreamableHttpService,
            streamable_http_server::session::local::LocalSessionManager,
        },
    };
    let (_temp, config) = setup();
    let service = RsiService {
        evidence: Arc::new(Evidence::collect(config).unwrap()),
        ecosystem: Arc::new(Ecosystem::new().unwrap()),
    };
    let mcp: StreamableHttpService<RsiService, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(service.clone()),
        Default::default(),
        StreamableHttpServerConfig::default().with_sse_keep_alive(None),
    );
    let app = axum::Router::new().nest_service("/mcp", mcp);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_uri(endpoint))
        .await
        .unwrap();
    assert_eq!(client.peer().list_all_tools().await.unwrap().len(), 3);
    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("rsi_read").with_arguments(
                json!({"path":"code/src/new.rs","limit":1})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    let value: Value = result.structured_content.unwrap();
    assert_eq!(value["lines"][0]["text"], "one");
    let _ = client.cancel().await;
    server.abort();
}

#[test]
fn deployment_catalog_graphs_and_state_root_schedules_are_collected() {
    let (_temp, mut config) = setup();
    write(
        &config.data,
        "catalog/rsi/graph.json",
        r#"{"objective":"weekly review","entry":"collect","nodes":[]}"#,
    );
    write(
        &config.data,
        "catalog/wecom/graph.json",
        r#"{"objective":"assistant","entry":"start","nodes":[]}"#,
    );
    // The Rust state root may be handed over as the state directory itself.
    let state = config.data.join("state");
    config.rust_state = Some(state.clone());
    write(
        &state,
        "schedules.json",
        r#"{"schedules":[{"graph":"rsi","enabled":true}]}"#,
    );
    let evidence_root = config.evidence.clone();
    let evidence = Evidence::collect(config).unwrap();
    assert_eq!(evidence.index("graphs", 0, 20).unwrap()["count"], 2);
    assert!(
        !evidence
            .issues
            .iter()
            .any(|issue| issue["path"] == "runs/schedules.json"),
        "{:?}",
        evidence.issues
    );
    // The schedule snapshot is resolved from the state root itself, not from a
    // nested `state/` directory.
    let entry = evidence
        .entries
        .values()
        .find(|entry| entry.path == "runs/schedules.json")
        .unwrap();
    let projected: Value =
        serde_json::from_str(&fs::read_to_string(evidence_root.join(&entry.frozen_file)).unwrap())
            .unwrap();
    assert_eq!(projected["status"], "ok");
    assert_eq!(projected["snapshot"]["schedules"][0]["graph"], "rsi");
    assert!(
        projected["source"]
            .as_str()
            .unwrap()
            .ends_with("state/schedules.json"),
        "{}",
        projected["source"]
    );
}

#[test]
fn run_projection_reports_timestamps_activation_and_failure() {
    let (_temp, mut config) = setup();
    let state = config.data.join("state");
    let mut record = graph_record("run-8", &["collect", "gate"]);
    record.started = Some("2026-10-09T10:00:00.000Z".into());
    record.sequence = 42;
    record.input = json!({"secret":"do-not-copy"});
    record.invocations.insert("collect".into(), 2);
    record.results.insert(
        "fork".into(),
        vec![run_result(
            &record,
            "fork",
            1,
            3,
            json!({
                "activation_id":"parallel:fork:1",
                "fanout":"fork",
                "join":"join",
                "branches":[["left"],["right"]]
            }),
        )],
    );
    record.results.insert(
        "join".into(),
        vec![run_result(
            &record,
            "join",
            1,
            9,
            json!({
                "activation_id":"parallel:fork:1",
                "branches":[
                    {"branch_id":"parallel:fork:1:branch:0","entry":"left","output":"left","status":"completed"},
                    {"branch_id":"parallel:fork:1:branch:1","entry":"right","output":"right","status":"failed"}
                ]
            }),
        )],
    );
    record.results.insert(
        "collect".into(),
        vec![run_result(
            &record,
            "collect",
            2,
            5,
            json!({"exit_code":0,"stdout":"private body"}),
        )],
    );
    store_run(
        &state,
        &record,
        Phase::Settled {
            result: Outcome::Failed {
                reason: Reason::Unclassified,
                detail: "worker failed: Goose ended without a validated final_result".into(),
            },
        },
    );
    write(
        &state,
        "kernel-run-observations/run-8.json",
        r#"{"updated":"2026-10-09T10:02:30.500Z"}"#,
    );
    config.rust_state = Some(state);
    let evidence_root = config.evidence.clone();
    let evidence = Evidence::collect(config).unwrap();
    let entry = evidence
        .entries
        .values()
        .find(|entry| entry.domain == "runs" && entry.path != "runs/schedules.json")
        .unwrap();
    let projected: Value =
        serde_json::from_str(&fs::read_to_string(evidence_root.join(&entry.frozen_file)).unwrap())
            .unwrap();
    assert_eq!(projected["format"], 15);
    assert_eq!(projected["time_basis"], "declared timestamp");
    assert_eq!(projected["duration_ms"], 150500);
    assert_eq!(projected["graph_entry"], "collect");
    assert_eq!(projected["graph_nodes"], json!(["collect", "gate"]));
    assert!(
        projected["graph"]
            .as_str()
            .unwrap()
            .starts_with(&format!("collect@{}", &record.graph_digest[..12])),
        "{}",
        projected["graph"]
    );
    assert_eq!(
        projected["error"],
        "worker failed: Goose ended without a validated final_result"
    );
    assert!(projected.get("recovery").is_none());
    assert_eq!(projected["fanout"]["fanout_node"], "fork");
    assert_eq!(projected["fanout"]["join_node"], "join");
    assert_eq!(projected["join"]["branches"][0]["status"], "completed");
    assert_eq!(projected["join"]["branches"][1]["status"], "failed");
    assert_eq!(projected["results"]["collect"]["attempts"], 1);
    assert_eq!(projected["results"]["collect"]["last_exit_code"], 0);
    assert!(projected.get("cursor").is_none());
    assert!(!projected.to_string().contains("do-not-copy"));
    assert!(!projected.to_string().contains("private body"));
}
