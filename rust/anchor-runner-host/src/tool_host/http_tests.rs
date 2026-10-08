//! Provider-free tests using loopback TCP and the public RMCP server.

use super::*;
use axum::{
    Router,
    extract::Request,
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::local::LocalSessionManager,
    },
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Fixture {
    calls: Arc<AtomicUsize>,
}

impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("runner-http-fixture", "1"))
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema: Arc<serde_json::Map<String, Value>> = Arc::new(
            serde_json::from_value(json!({
                "type":"object",
                "properties":{"value":{"type":"string"}},
                "required":["value"],
                "additionalProperties":false
            }))
            .unwrap(),
        );
        Ok(ListToolsResult {
            tools: vec![
                Tool::new("echo", "Echo the supplied value.", schema.clone()),
                Tool::new("hidden", "A hidden fixture tool.", schema),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let value = request.arguments.unwrap().get("value").cloned().unwrap();
        if value == "error" {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "fixture error",
            )]));
        }
        let mut result = CallToolResult::success(vec![ContentBlock::json(value.clone())?]);
        result.structured_content = Some(value);
        Ok(result)
    }
}

struct HttpFixture {
    endpoint: String,
    calls: Arc<AtomicUsize>,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.task.abort();
    }
}

async fn fixture(require_auth: bool) -> HttpFixture {
    let cancellation = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = Fixture {
        calls: calls.clone(),
    };
    let service: StreamableHttpService<Fixture, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Default::default(),
        StreamableHttpServerConfig::default()
            .with_sse_keep_alive(None)
            .with_cancellation_token(cancellation.child_token()),
    );
    async fn auth(request: Request, next: Next) -> Response {
        if request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            != Some("Bearer fixture-token")
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        next.run(request).await
    }
    let app = Router::new().nest_service("/mcp", service);
    let app = if require_auth {
        app.layer(middleware::from_fn(auth))
    } else {
        app
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    HttpFixture {
        endpoint,
        calls,
        cancellation,
        task,
    }
}

fn binding(server: &str) -> PluginBinding {
    PluginBinding {
        id: "fixture-plugin".into(),
        digest: "fixture-digest".into(),
        resources: vec![],
        mcp_servers: vec![server.into()],
    }
}

fn config(endpoint: &str, _names: &[&str]) -> McpToolConfig {
    McpToolConfig {
        environment: Default::default(),
        disabled: Default::default(),
        oauth_library_root: None,
        servers: BTreeMap::from([(
            "fixture-plugin-fixture".into(),
            ResolvedMcpServer {
                plugin_id: "fixture-plugin".into(),
                name: "fixture".into(),
                plugin_directory: PathBuf::from("/tmp/fixture-plugin"),
                config: json!({"type":"http", "url":endpoint, "headers":{}}),
            },
        )]),
    }
}

#[tokio::test]
async fn authenticated_http_tools_are_exposed_as_server_prefixed_names() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let fixture = fixture(true).await;
        let host = McpHost::connect(McpServerConfig {
            server_id: "fixture".into(),
            transport: McpTransportConfig::StreamableHttp {
                endpoint: fixture.endpoint.clone(),
                bearer_token: Some(Secret::new("fixture-token")),
                headers: BTreeMap::new(),
            },
        })
        .await
        .unwrap();
        let tools = LiveMcpTools {
            hosts: BTreeMap::from([("fixture-plugin-fixture".into(), Arc::new(host))]),
        };
        let definitions = tools.definitions();
        assert_eq!(
            definitions
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            [
                "fixture-plugin-fixture_echo",
                "fixture-plugin-fixture_hidden",
            ]
        );
        assert!(!tools.is_read_only("fixture-plugin-fixture_echo"));
        assert!(matches!(
            tools.call("echo", json!({"value": 1})).await,
            Err(ToolError::Unknown(_))
        ));
        let result = tools
            .call(
                "fixture-plugin-fixture_echo",
                json!({"value": {"answer": 42}}),
            )
            .await
            .unwrap();
        assert_eq!(result[0].as_json(), Some(&json!({"answer": 42})));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("HTTP integration must finish");
}

#[tokio::test]
async fn wrong_credentials_fail_before_tool_registration() {
    let fixture = fixture(true).await;
    let result = McpHost::connect(McpServerConfig {
        server_id: "fixture".into(),
        transport: McpTransportConfig::StreamableHttp {
            endpoint: fixture.endpoint.clone(),
            bearer_token: Some(Secret::new("wrong-token")),
            headers: BTreeMap::new(),
        },
    })
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn network_policy_and_tool_name_validation_happen_before_connect() {
    let endpoint = "http://127.0.0.1:1/mcp";
    let bindings = [binding("fixture")];
    let result = config(endpoint, &["echo"]).bind(&bindings, false).await;
    assert!(matches!(result, Err(error) if error.contains("network=true")));
    let missing = McpToolConfig::default()
        .bind(&bindings, true)
        .await
        .err()
        .unwrap();
    assert!(missing.contains("not configured"));
}

#[tokio::test]
async fn each_server_gets_its_own_prefix_and_duplicate_bindings_connect_once() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let first = fixture(false).await;
        let second = fixture(false).await;
        let mut host_config = config(&first.endpoint, &["echo"]);
        host_config.servers.insert(
            "fixture-plugin-second".into(),
            ResolvedMcpServer {
                plugin_id: "fixture-plugin".into(),
                name: "second".into(),
                plugin_directory: PathBuf::from("/tmp/fixture-plugin"),
                config: json!({"type":"http", "url":second.endpoint, "headers":{}}),
            },
        );
        let mut first_binding = binding("fixture");
        first_binding.mcp_servers.push("second".into());
        let tools = host_config
            .bind(&[first_binding, binding("fixture")], true)
            .await
            .unwrap();
        assert_eq!(
            tools
                .definitions()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            [
                "fixture-plugin-fixture_echo",
                "fixture-plugin-fixture_hidden",
                "fixture-plugin-second_echo",
                "fixture-plugin-second_hidden",
            ]
        );
        tools
            .call("fixture-plugin-second_echo", json!({"value": 2}))
            .await
            .unwrap();
        tools
            .call("fixture-plugin-fixture_echo", json!({"value": 1}))
            .await
            .unwrap();
        assert_eq!(first.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second.calls.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("HTTP binding must finish");
}

#[tokio::test]
async fn stdio_requires_the_sandbox_launcher() {
    let mut config = McpToolConfig::default();
    config.servers.insert(
        "fixture-plugin-stdio".into(),
        ResolvedMcpServer {
            plugin_id: "fixture-plugin".into(),
            name: "stdio".into(),
            plugin_directory: PathBuf::from("/tmp/fixture-plugin"),
            config: json!({"type":"stdio", "command":"fixture-mcp", "args":[], "env":{}}),
        },
    );
    let error = config.bind(&[binding("stdio")], false).await.err().unwrap();
    assert!(error.contains("sandbox launcher"));
}
