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
struct Fixture(Arc<AtomicUsize>);
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
        Ok(ListToolsResult { tools: ["echo", "hidden"].into_iter().map(|name|
            Tool::new(name.to_owned(), "fixture".to_owned(), Arc::new(serde_json::from_value(json!({"type":"object","properties":{"value":{}},"required":["value"]})).unwrap()))
        ).collect(), next_cursor: None, meta: None })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        self.0.fetch_add(1, Ordering::SeqCst);
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
    let cancellation = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = Fixture(calls.clone());
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
            allowed_tools: BTreeSet::from(["echo".into()]),
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
        assert_eq!(
            tools
                .definitions()
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["echo"]
        );
        assert!(matches!(
            tools.call("hidden", json!({"value":0})).await,
            Err(ToolError::Unknown(_))
        ));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        let result = tools
            .call("echo", json!({"value":{"answer":42}}))
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
                .call("echo", json!({"value":"structured-only"}))
                .await
                .unwrap(),
            vec![ToolResultContent::json(json!("structured-only"))]
        );
        assert!(matches!(
            tools.call("echo", json!({"value":"error"})).await,
            Err(ToolError::Failed(_))
        ));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    })
    .await
    .expect("HTTP integration must finish");
}

#[tokio::test]
async fn network_policy_and_ambiguous_tool_names_reject_before_connect() {
    let endpoint = "http://127.0.0.1:1/mcp";
    let bindings = [binding("fixture")];
    let result = config(endpoint, &["echo"]).bind(&bindings, false).await;
    assert!(matches!(result, Err(error) if error.contains("network=true")));
    for names in [
        vec![""],
        vec!["invalid.name"],
        vec![FAKE_ECHO_TOOL],
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
    assert!(matches!(result, Err(error) if error.contains("valid, unique")));
    assert!(
        config
            .validate_bindings(&[binding("fixture"), binding("fixture")])
            .is_ok()
    );
}

#[tokio::test]
async fn deployment_binding_connects_once_per_server_and_rejects_missing_inventory() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let fixture = fixture(false).await;
        let config = config(&fixture.endpoint, &["echo"]);
        let bindings = [binding("fixture"), binding("fixture")];
        let tools = config.bind(&bindings, true).await.unwrap();
        assert_eq!(tools.hosts.len(), 1);
        assert_eq!(tools.definitions().len(), 1);
        tools.call("echo", json!({"value":1})).await.unwrap();
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        let mut missing = config;
        missing.servers.get_mut("fixture").unwrap().allowed_tools = vec!["missing".into()];
        assert!(missing.bind(&bindings, true).await.is_err());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("HTTP binding must finish");
}
