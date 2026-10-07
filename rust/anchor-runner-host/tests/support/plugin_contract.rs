use crate::fixture::{
    Host, Provider, Reply, command, complete, evidence, evidence_rejection, read_json,
};
use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use axum::{
    Router,
    extract::{Request, State},
    middleware::{self, Next},
    response::Response,
};
use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ErrorData, ListToolsResult, PaginatedRequestParams,
        ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::local::LocalSessionManager,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};
use tokio_util::sync::CancellationToken;

const PLUGIN: &str = "contract";
const REMOTE_TOOL: &str = "fixture_suffix";
const EXPOSED_TOOL: &str = "contract-loopback_fixture_suffix";
const SKILL: &str = "---\nname: contract-check\ndescription: Deterministic local Plugin contract.\n---\nRead resources/input.txt, call contract-loopback_fixture_suffix once, and save the checked text.\n";
const RESOURCE: &str = "plugin-runtime-evidence";
const CHECKED: &str = "plugin-runtime-evidence-checked";
const READONLY: &str = "plugin-read-only";

#[derive(Default)]
struct McpState {
    http_requests: AtomicUsize,
    tool_lists: AtomicUsize,
    tool_calls: AtomicUsize,
    effects: Mutex<Vec<Value>>,
}

#[derive(Clone)]
struct McpHandler {
    state: Arc<McpState>,
    effect_path: PathBuf,
}

fn input_schema() -> Value {
    json!({
        "type":"object",
        "properties":{"text":{"type":"string"}},
        "required":["text"],
        "additionalProperties":false
    })
}

impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.state.tool_lists.fetch_add(1, Ordering::SeqCst);
        Ok(ListToolsResult {
            tools: vec![Tool::new(
                REMOTE_TOOL,
                "Append -checked to text and persist one local fixture side effect.",
                Arc::new(serde_json::from_value(input_schema()).unwrap()),
            )],
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        self.state.tool_calls.fetch_add(1, Ordering::SeqCst);
        if request.name != REMOTE_TOOL {
            return Err(ErrorData::invalid_params("unknown fixture tool", None));
        }
        let arguments = request
            .arguments
            .ok_or_else(|| ErrorData::invalid_params("missing arguments", None))?;
        if arguments.len() != 1 {
            return Err(ErrorData::invalid_params("only text is supported", None));
        }
        let text = arguments
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| ErrorData::invalid_params("text must be a string", None))?;
        let mut effects = self.state.effects.lock().unwrap();
        let output = json!({"text":format!("{text}-checked"),"sequence":effects.len() + 1});
        let effect = json!({"tool":REMOTE_TOOL,"arguments":arguments,"output":output});
        let mut bytes = serde_json::to_vec(&effect).unwrap();
        bytes.push(b'\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.effect_path)
            .and_then(|mut file| file.write_all(&bytes).and_then(|()| file.sync_all()))
            .map_err(|_| ErrorData::internal_error("fixture effect write failed", None))?;
        effects.push(effect);
        Ok(CallToolResult::structured(output))
    }
}

async fn count_mcp_request(
    State(state): State<Arc<McpState>>,
    request: Request,
    next: Next,
) -> Response {
    state.http_requests.fetch_add(1, Ordering::SeqCst);
    next.run(request).await
}

struct McpFixture {
    endpoint: String,
    state: Arc<McpState>,
    effect_path: PathBuf,
    stop: CancellationToken,
    task: Option<thread::JoinHandle<()>>,
}

impl McpFixture {
    fn new(host: &Host) -> Self {
        let state = Arc::new(McpState::default());
        let effect_path = host.root.path().join("mcp-effects.jsonl");
        let handler = McpHandler {
            state: state.clone(),
            effect_path: effect_path.clone(),
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let shared = state.clone();
        let task = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let service: StreamableHttpService<McpHandler, LocalSessionManager> =
                        StreamableHttpService::new(
                            move || Ok(handler.clone()),
                            Default::default(),
                            StreamableHttpServerConfig::default()
                                .with_sse_keep_alive(None)
                                .with_cancellation_token(stopped.child_token()),
                        );
                    let app = Router::new()
                        .nest_service("/mcp", service)
                        .layer(middleware::from_fn_with_state(shared, count_mcp_request));
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    tokio::select! {
                        _ = async { axum::serve(listener, app).await.unwrap(); } => {},
                        _ = stopped.cancelled() => {},
                    }
                });
        });
        Self {
            endpoint: format!("http://{address}/mcp"),
            state,
            effect_path,
            stop,
            task: Some(task),
        }
    }

    fn snapshot(&self) -> Value {
        json!({
            "http_requests":self.state.http_requests.load(Ordering::SeqCst),
            "tool_lists":self.state.tool_lists.load(Ordering::SeqCst),
            "tool_calls":self.state.tool_calls.load(Ordering::SeqCst),
            "effects":self.state.effects.lock().unwrap().clone()
        })
    }
}

impl Drop for McpFixture {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

fn plugin_graph() -> Value {
    json!({
        "objective":"Read a mounted Plugin and invoke its loopback MCP tool",
        "entry":"worker",
        "agents":{"worker":{
            "model":"models.worker",
            "instructions":"Read the Plugin Skill and resource, call its MCP tool once, save the checked text, and finish via final_result.",
            "network":true,
            "wall_time_limit_seconds":30
        }},
        "nodes":[{"id":"worker","agent":"worker","plugins":[PLUGIN]}],
        "edges":[]
    })
}

fn install_plugin(host: &Host, endpoint: &str) -> Value {
    let bundle = host.root.path().join("bundle");
    let plugin = bundle.join("plugins").join(PLUGIN);
    fs::create_dir_all(plugin.join("skills/check")).unwrap();
    fs::create_dir_all(plugin.join("resources")).unwrap();
    fs::write(
        plugin.join("plugin.json"),
        json!({"name":"Runtime contract fixture","description":"Local-only deterministic Plugin.","skills":"./skills"}).to_string(),
    )
    .unwrap();
    fs::write(
        plugin.join(".mcp.json"),
        json!({"mcpServers":{"loopback":{"type":"http","url":endpoint}}}).to_string(),
    )
    .unwrap();
    fs::write(plugin.join("skills/check/SKILL.md"), SKILL).unwrap();
    fs::write(plugin.join("resources/input.txt"), RESOURCE).unwrap();
    let binding = FilePluginCatalog::new(&bundle)
        .resolve(&[PLUGIN.into()])
        .unwrap()
        .remove(0);
    let summary = json!({
        "id":binding.id,"digest":binding.digest,
        "resources":binding.resources,"mcp_servers":binding.mcp_servers
    });
    assert_eq!(
        summary["resources"],
        json!([
            ".mcp.json",
            "plugin.json",
            "resources/input.txt",
            "skills/check/SKILL.md"
        ])
    );
    assert_eq!(summary["mcp_servers"], json!(["loopback"]));
    fs::write(
        bundle.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[summary]}).to_string(),
    )
    .unwrap();
    summary
}

fn tool_result(request: &Value, call_id: &str, tool: &str) -> Value {
    let message = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == call_id)
        .unwrap_or_else(|| panic!("missing actual tool result for {call_id}: {request}"));
    let prefix = format!("<external_content>\n\n[{tool}]\n");
    let content = message["content"]
        .as_str()
        .unwrap()
        .strip_prefix(&prefix)
        .and_then(|content| content.strip_suffix("\n\n</external_content>"))
        .unwrap_or_else(|| panic!("missing Harness external-content envelope: {message}"));
    serde_json::from_str(content)
        .unwrap_or_else(|error| panic!("invalid tool JSON ({error}): {message}"))
}

#[test]
fn agent_reads_readonly_plugin_and_calls_loopback_mcp_before_artifact_commit() {
    let host = Host::new(&plugin_graph());
    let mcp = McpFixture::new(&host);
    let summary = install_plugin(&host, &mcp.endpoint);
    let provider = Provider::new([(
        "fixture-worker",
        vec![
            command(
                "set -eu; cat /plugins/contract/skills/check/SKILL.md > skill.txt; cat /plugins/contract/resources/input.txt > resource.txt; if (printf corrupt > /plugins/contract/skills/check/SKILL.md) 2>/dev/null; then exit 41; fi; if (printf corrupt > /plugins/contract/resources/input.txt) 2>/dev/null; then exit 42; fi; if (printf corrupt > /plugins/contract/unexpected.txt) 2>/dev/null; then exit 43; fi; printf plugin-read-only > readonly.txt; cat skill.txt; printf '\n'; cat resource.txt; printf '\n'; cat readonly.txt",
            ),
            Reply::Tool(EXPOSED_TOOL, json!({"text":RESOURCE})),
            command("printf '%s' plugin-runtime-evidence-checked > checked.txt"),
            complete(None),
        ],
    )]);
    let response = host.run(&provider);
    assert_eq!(response["kind"], "run", "{response}");
    assert_eq!(response["status"], "completed", "{response}");
    let saved = host.record();
    assert_eq!(saved["results"]["worker"].as_array().unwrap().len(), 1);
    assert_eq!(saved["plugin_bindings"][PLUGIN], summary);

    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    assert!(
        requests[0]["messages"]
            .to_string()
            .contains("/plugins/contract/skills/check/SKILL.md")
    );
    for request in &requests {
        assert_eq!(request["model"], "fixture-worker");
        let tools = request["tools"].as_array().unwrap();
        for name in ["anchor_run", "final_result", EXPOSED_TOOL] {
            assert!(tools.iter().any(|tool| tool["function"]["name"] == name));
        }
        let registered = tools
            .iter()
            .find(|tool| tool["function"]["name"] == EXPOSED_TOOL)
            .unwrap();
        assert_eq!(registered["function"]["parameters"], input_schema());
        assert!(
            !tools
                .iter()
                .any(|tool| tool["function"]["name"] == REMOTE_TOOL)
        );
    }
    let read_result = tool_result(&requests[1], "call-1", "anchor_run");
    assert_eq!(read_result["exit_code"], 0);
    assert_eq!(
        read_result["stdout"],
        format!("{SKILL}\n{RESOURCE}\n{READONLY}")
    );
    let mcp_result = tool_result(&requests[2], "call-2", EXPOSED_TOOL);
    assert_eq!(mcp_result, json!({"text":CHECKED,"sequence":1}));
    assert_eq!(
        tool_result(&requests[3], "call-3", "anchor_run")["exit_code"],
        0
    );

    let snapshot = mcp.snapshot();
    assert!(snapshot["http_requests"].as_u64().unwrap() > 0);
    assert_eq!(snapshot["tool_lists"], 1);
    assert_eq!(snapshot["tool_calls"], 1);
    let effects: Vec<Value> = fs::read_to_string(&mcp.effect_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        effects,
        vec![json!({"tool":REMOTE_TOOL,"arguments":{"text":RESOURCE},"output":mcp_result})]
    );
    assert_eq!(snapshot["effects"], json!(effects));

    let manifest = read_json(host.artifact(&saved, "worker").join("manifest.json"));
    for (name, expected) in [
        ("skill.txt", SKILL),
        ("resource.txt", RESOURCE),
        ("readonly.txt", READONLY),
        ("checked.txt", CHECKED),
    ] {
        assert_eq!(host.file(&saved, "worker", name), expected.as_bytes());
        assert_eq!(manifest["files"][name]["bytes"], expected.len());
        assert_eq!(
            manifest["files"][name]["sha256"],
            format!("{:x}", Sha256::digest(expected.as_bytes()))
        );
    }
    let plugin = host.root.path().join("bundle/plugins/contract");
    assert_eq!(
        fs::read(plugin.join("skills/check/SKILL.md")).unwrap(),
        SKILL.as_bytes()
    );
    assert_eq!(
        fs::read(plugin.join("resources/input.txt")).unwrap(),
        RESOURCE.as_bytes()
    );
    assert!(!plugin.join("unexpected.txt").exists());
    provider.assert_consumed();
    evidence(
        "plugin-loopback-mcp-artifact",
        &host,
        &provider,
        json!({
            "plugin":summary,"mcp":snapshot,"mcp_result":mcp_result,
            "artifact_manifest":manifest,"readonly_skill_and_resource":true
        }),
    );
}

#[test]
fn plugin_resource_digest_drift_is_rejected_before_provider_or_mcp_calls() {
    let host = Host::new(&plugin_graph());
    let mcp = McpFixture::new(&host);
    let summary = install_plugin(&host, &mcp.endpoint);
    let provider = Provider::new([("fixture-worker", vec![])]);
    fs::write(
        host.root
            .path()
            .join("bundle/plugins/contract/resources/input.txt"),
        "drifted-resource",
    )
    .unwrap();
    let changed = FilePluginCatalog::new(host.root.path().join("bundle"))
        .resolve(&[PLUGIN.into()])
        .unwrap()
        .remove(0);
    assert_ne!(summary["digest"], changed.digest);
    assert_eq!(summary["resources"], json!(changed.resources));
    assert_eq!(summary["mcp_servers"], json!(changed.mcp_servers));
    let response = host.run(&provider);
    assert_eq!(response["kind"], "rejected", "{response}");
    assert!(
        response["reason"]
            .as_str()
            .unwrap()
            .contains("Plugin `contract` summary does not match bundled resources"),
        "{response}"
    );
    assert!(provider.requests().is_empty());
    assert_eq!(
        mcp.snapshot(),
        json!({"http_requests":0,"tool_lists":0,"tool_calls":0,"effects":[]})
    );
    assert!(!mcp.effect_path.exists());
    assert!(!host.root.path().join("state/runs/fixture.json").exists());
    provider.assert_consumed();
    evidence_rejection(
        "plugin-resource-drift",
        &host,
        &provider,
        response,
        json!({
            "provider_requests":0,"mcp":mcp.snapshot(),"run_record_created":false
        }),
    );
}
