//! Provider-free tests using real loopback TCP and the public RMCP server.
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
    extra_tool_count: usize,
    schema_padding_bytes: usize,
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
        let schema = || {
            Arc::new(
                serde_json::from_value(json!({
                    "type":"object",
                    "description":"x".repeat(self.schema_padding_bytes),
                    "properties":{"value":{"type":"string"}},
                    "required":["value"],
                    "additionalProperties":false
                }))
                .unwrap(),
            )
        };
        let mut tools: Vec<Tool> = [
            ("echo", "Echo the supplied value."),
            ("hidden", "A hidden fixture tool."),
            (
                "weather_lookup",
                "Retrieve current weather conditions for a city; 查询当前天气情况。",
            ),
            ("weather_forecast", "Return a weather forecast for a city."),
        ]
        .into_iter()
        .map(|(name, description)| Tool::new(name, description, schema()))
        .collect();
        tools.extend((0..self.extra_tool_count).map(|index| {
            Tool::new(
                format!("catalogue_tool_{index:04}"),
                "A tool in the large-catalogue disclosure fixture.",
                schema(),
            )
        }));
        Ok(ListToolsResult {
            tools,
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
        let mut result = CallToolResult::success(vec![
            ContentBlock::text("HTTP reply"),
            ContentBlock::image("aGVsbG8=", "image/png"),
        ]);
        if value == "structured-only" {
            result.content.clear();
        }
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
    fixture_with_extra_tools(require_auth, 0).await
}

async fn fixture_with_extra_tools(require_auth: bool, extra_tool_count: usize) -> HttpFixture {
    fixture_with_catalogue(require_auth, extra_tool_count, 0).await
}

async fn fixture_with_catalogue(
    require_auth: bool,
    extra_tool_count: usize,
    schema_padding_bytes: usize,
) -> HttpFixture {
    let cancellation = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = Fixture {
        calls: calls.clone(),
        extra_tool_count,
        schema_padding_bytes,
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
fn config(endpoint: &str, names: &[&str]) -> McpToolConfig {
    McpToolConfig {
        servers: BTreeMap::from([(
            "fixture".into(),
            McpServerEnvConfig {
                transport: "http".into(),
                endpoint: Some(endpoint.into()),
                bearer_token_env: None,
                allowed_tools: names.iter().map(|s| (*s).into()).collect(),
            },
        )]),
    }
}

#[tokio::test]
async fn authenticated_http_result_reaches_combined_toolport_without_losing_content() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let fixture = fixture(true).await;
        let host_config = |token: &str| McpServerConfig {
            server_id: "fixture".into(),
            allowed_tools: BTreeSet::from([
                "echo".into(),
                "weather_lookup".into(),
                "weather_forecast".into(),
            ]),
            transport: McpTransportConfig::StreamableHttp {
                endpoint: fixture.endpoint.clone(),
                bearer_token: Some(Secret::new(token)),
            },
        };
        assert!(McpHost::connect(host_config("wrong-token")).await.is_err());
        let host = McpHost::connect(host_config("fixture-token"))
            .await
            .unwrap();
        let live = LiveMcpTools {
            hosts: BTreeMap::from([("fixture".into(), Arc::new(host))]),
        };
        let fake_host = PluginToolHost::new(std::iter::empty());
        let bindings = [binding("fixture")];
        let tools = CombinedPluginTools::new(fake_host.for_bindings(&bindings).unwrap(), live);
        let definitions = tools.definitions();
        assert_eq!(definitions.len(), 2);
        assert_eq!(
            definitions
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            [MCP_SEARCH_TOOLS_TOOL, MCP_CALL_TOOL]
        );
        assert!(tools.is_read_only(MCP_SEARCH_TOOLS_TOOL));
        assert!(!tools.is_read_only(MCP_CALL_TOOL));
        assert!(definitions.iter().all(|definition| {
            !["echo", "weather_lookup", "weather_forecast", "hidden"]
                .contains(&definition.name.as_str())
        }));

        let first_page = tools
            .call(MCP_SEARCH_TOOLS_TOOL, json!({"query":"weather","limit":1}))
            .await
            .unwrap();
        let first_page = first_page[0].as_json().unwrap();
        assert_eq!(first_page["total_matches"], 2);
        assert_eq!(first_page["tools"][0]["tool_name"], "weather_forecast");
        assert_eq!(first_page["tools"][0]["server_id"], "fixture");
        assert_eq!(first_page["has_more"], true);
        assert_eq!(first_page["next_offset"], 1);
        let second_page = tools
            .call(
                MCP_SEARCH_TOOLS_TOOL,
                json!({"query":"weather","offset":1,"limit":1}),
            )
            .await
            .unwrap();
        let second_page = second_page[0].as_json().unwrap();
        assert_eq!(second_page["tools"][0]["tool_name"], "weather_lookup");
        assert_eq!(second_page["has_more"], false);
        let chinese_search = tools
            .call(MCP_SEARCH_TOOLS_TOOL, json!({"query":"当前天气"}))
            .await
            .unwrap();
        let chinese_search = chinese_search[0].as_json().unwrap();
        assert_eq!(chinese_search["total_matches"], 1);
        assert_eq!(chinese_search["tools"][0]["tool_name"], "weather_lookup");

        let echo_schema = tools
            .call(MCP_SEARCH_TOOLS_TOOL, json!({"query":"echo"}))
            .await
            .unwrap();
        let echo_schema = echo_schema[0].as_json().unwrap();
        assert_eq!(
            echo_schema["tools"][0]["input_schema"],
            json!({
                "type":"object",
                "description":"",
                "properties":{"value":{"type":"string"}},
                "required":["value"],
                "additionalProperties":false
            })
        );
        assert!(matches!(
            tools
                .call(MCP_SEARCH_TOOLS_TOOL, json!({"query":"   "}))
                .await,
            Err(ToolError::Failed(_))
        ));

        assert!(matches!(
            tools.call("hidden", json!({"value":0})).await,
            Err(ToolError::Unknown(_))
        ));
        assert!(matches!(
            tools
                .call(
                    MCP_CALL_TOOL,
                    json!({"server_id":"fixture","tool_name":"hidden","arguments":{"value":0}})
                )
                .await,
            Err(ToolError::Failed(_))
        ));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        let result = tools
            .call(
                MCP_CALL_TOOL,
                json!({
                    "server_id":"fixture",
                    "tool_name":"echo",
                    "arguments":{"value":{"answer":42}}
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            result,
            vec![
                ToolResultContent::json(json!({"answer":42})),
                ToolResultContent::text("HTTP reply"),
                ToolResultContent::image_base64(
                    "aGVsbG8=",
                    Some(rig_agent::core::message::ImageMediaType::PNG),
                    None
                )
            ]
        );
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            tools
                .call(
                    MCP_CALL_TOOL,
                    json!({
                        "server_id":"fixture",
                        "tool_name":"echo",
                        "arguments":{"value":"structured-only"}
                    })
                )
                .await
                .unwrap(),
            vec![ToolResultContent::json(json!("structured-only"))]
        );
        assert!(matches!(
            tools
                .call(
                    MCP_CALL_TOOL,
                    json!({
                        "server_id":"fixture",
                        "tool_name":"echo",
                        "arguments":{"value":"error"}
                    })
                )
                .await,
            Err(ToolError::Failed(_))
        ));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    })
    .await
    .expect("HTTP integration must finish");
}

#[tokio::test]
async fn network_policy_and_tool_name_validation_happen_before_connect() {
    let endpoint = "http://127.0.0.1:1/mcp";
    let bindings = [binding("fixture")];
    let result = config(endpoint, &["echo"]).bind(&bindings, false).await;
    assert!(matches!(result, Err(error) if error.contains("network=true")));
    for names in [
        vec![""],
        vec!["invalid.name"],
        vec![FAKE_ECHO_TOOL],
        vec![MCP_SEARCH_TOOLS_TOOL],
        vec![MCP_CALL_TOOL],
        vec![crate::node_tools::RUN_TOOL_NAME],
        vec!["echo", "echo"],
    ] {
        let result = config(endpoint, &names).bind(&bindings, true).await;
        assert!(matches!(result, Err(error) if error.contains("valid, unique")));
    }
    let mut config = config(endpoint, &["echo"]);
    config
        .servers
        .insert("second".into(), config.servers["fixture"].clone());
    let result = config
        .bind(&[binding("fixture"), binding("second")], true)
        .await;
    assert!(
        result.is_err(),
        "the duplicate endpoint fixture is unreachable"
    );
    assert!(
        config
            .validate_bindings(&[binding("fixture"), binding("second")])
            .is_ok(),
        "server ids disambiguate equal MCP tool names"
    );
    assert!(
        config
            .validate_bindings(&[binding("fixture"), binding("fixture")])
            .is_ok()
    );
}

#[tokio::test]
async fn deployment_binding_connects_once_per_server_and_rejects_missing_inventory() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let first_fixture = fixture(false).await;
        let second_fixture = fixture(false).await;
        let mut config = config(&first_fixture.endpoint, &["echo"]);
        config.servers.insert(
            "second".into(),
            McpServerEnvConfig {
                endpoint: Some(second_fixture.endpoint.clone()),
                ..config.servers["fixture"].clone()
            },
        );
        let bindings = [binding("fixture"), binding("fixture"), binding("second")];
        let tools = config.bind(&bindings, true).await.unwrap();
        assert_eq!(tools.hosts.len(), 2);
        assert_eq!(tools.definitions().len(), 2);
        assert!(matches!(
            tools.call("echo", json!({"value":1})).await,
            Err(ToolError::Unknown(_))
        ));
        tools
            .call(
                MCP_CALL_TOOL,
                json!({"server_id":"fixture","tool_name":"echo","arguments":{"value":1}}),
            )
            .await
            .unwrap();
        assert_eq!(first_fixture.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_fixture.calls.load(Ordering::SeqCst), 0);
        tools
            .call(
                MCP_CALL_TOOL,
                json!({"server_id":"second","tool_name":"echo","arguments":{"value":2}}),
            )
            .await
            .unwrap();
        assert_eq!(first_fixture.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_fixture.calls.load(Ordering::SeqCst), 1);
        let mut missing = config;
        missing.servers.get_mut("fixture").unwrap().allowed_tools = vec!["missing".into()];
        assert!(missing.bind(&[binding("fixture")], true).await.is_err());
        assert_eq!(first_fixture.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_fixture.calls.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("HTTP binding must finish");
}

#[tokio::test]
async fn large_mcp_inventory_keeps_provider_catalogue_small_and_search_pages_bounded() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let tool_count = 512;
        let fixture = fixture_with_extra_tools(false, tool_count).await;
        let config = McpToolConfig {
            servers: BTreeMap::from([(
                "fixture".into(),
                McpServerEnvConfig {
                    transport: "http".into(),
                    endpoint: Some(fixture.endpoint.clone()),
                    bearer_token_env: None,
                    allowed_tools: (0..tool_count)
                        .map(|index| format!("catalogue_tool_{index:04}"))
                        .collect(),
                },
            )]),
        };
        let tools = config.bind(&[binding("fixture")], true).await.unwrap();
        let definitions = tools.definitions();
        assert_eq!(definitions.len(), 2);
        assert!(
            definitions
                .iter()
                .all(|definition| !definition.name.starts_with("catalogue_tool_"))
        );
        assert!(serde_json::to_vec(&definitions).unwrap().len() < 2_000);

        let result = tools
            .call(
                MCP_SEARCH_TOOLS_TOOL,
                json!({"query":"catalogue","limit":8}),
            )
            .await
            .unwrap();
        let result = result[0].as_json().unwrap();
        assert_eq!(result["total_matches"], tool_count);
        assert_eq!(result["tools"].as_array().unwrap().len(), 8);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["next_offset"], 8);
    })
    .await
    .expect("large inventory should stay local until matching tool schemas are requested");
}

#[tokio::test]
async fn mcp_search_rejects_schemas_or_pages_that_exceed_disclosure_byte_limits() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let one = fixture_with_catalogue(false, 1, 13 * 1024).await;
        let config = McpToolConfig {
            servers: BTreeMap::from([(
                "fixture".into(),
                McpServerEnvConfig {
                    transport: "http".into(),
                    endpoint: Some(one.endpoint.clone()),
                    bearer_token_env: None,
                    allowed_tools: vec!["catalogue_tool_0000".into()],
                },
            )]),
        };
        let tools = config.bind(&[binding("fixture")], true).await.unwrap();
        assert!(matches!(
            tools.call(MCP_SEARCH_TOOLS_TOOL, json!({"query":"catalogue"})).await,
            Err(ToolError::Failed(message)) if message.contains("disclosure limit")
        ));

        let many = fixture_with_catalogue(false, 3, 10 * 1024).await;
        let config = McpToolConfig {
            servers: BTreeMap::from([(
                "fixture".into(),
                McpServerEnvConfig {
                    transport: "http".into(),
                    endpoint: Some(many.endpoint.clone()),
                    bearer_token_env: None,
                    allowed_tools: (0..3)
                        .map(|index| format!("catalogue_tool_{index:04}"))
                        .collect(),
                },
            )]),
        };
        let tools = config.bind(&[binding("fixture")], true).await.unwrap();
        assert!(matches!(
            tools.call(
                MCP_SEARCH_TOOLS_TOOL,
                json!({"query":"catalogue","limit":3})
            ).await,
            Err(ToolError::Failed(message)) if message.contains("search result")
        ));
    })
    .await
    .expect("oversized remote schemas are rejected without partial disclosure");
}
