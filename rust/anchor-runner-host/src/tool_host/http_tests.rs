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
        authorization_digest: "fixture-authority".try_into().unwrap(),
    }
}

fn config(endpoint: &str, _names: &[&str]) -> McpToolConfig {
    McpToolConfig {
        environment: Default::default(),
        disabled: Default::default(),
        oauth_library_root: None,
        startup_cancellation: None,
        startup_timeout: None,
        configuration_revisions: BTreeMap::new(),
        configuration_bindings: BTreeMap::new(),
        servers: BTreeMap::from([(
            "fixture-plugin-fixture".into(),
            ResolvedMcpServer {
                plugin_id: "fixture-plugin".into(),
                name: "fixture".into(),
                plugin_directory: PathBuf::from("/tmp/fixture-plugin"),
                declaration: json!({"type":"http", "url":endpoint, "headers":{}}),
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
            owners: BTreeMap::new(),
            hosts: BTreeMap::from([("fixture-plugin-fixture".into(), Arc::new(host))]),
            _environment: Default::default(),
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
async fn candidate_http_startup_cancels_while_initialize_is_pending_and_can_retry() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let waiting = entered.clone();
    let app = Router::new().route(
        "/mcp",
        axum::routing::post(move || {
            let waiting = waiting.clone();
            async move {
                waiting.notify_one();
                std::future::pending::<StatusCode>().await
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut candidate = config(&endpoint, &[]);
    candidate.startup_cancellation = Some(cancellation.clone());
    let cancel = async {
        entered.notified().await;
        cancellation.store(true, Ordering::Release);
    };
    let bindings = [binding("fixture")];
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(candidate.bind(&bindings, true), cancel)
    })
    .await
    .expect("candidate HTTP initialization must respond to cancellation");
    assert!(result.err().unwrap().contains("startup was cancelled"));
    task.abort();
    let _ = task.await;
    let healthy = fixture(false).await;
    let mut retry = config(&healthy.endpoint, &[]);
    retry.startup_cancellation = Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let tools = retry.bind(&bindings, true).await.unwrap();
    assert!(
        tools
            .definitions()
            .iter()
            .any(|tool| tool.name == "fixture-plugin-fixture_echo")
    );
    tools.shutdown().await.unwrap();
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
                declaration: json!({"type":"http", "url":second.endpoint, "headers":{}}),
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
            declaration: json!({"type":"stdio", "command":"fixture-mcp", "args":[], "env":{}}),
            config: json!({"type":"stdio", "command":"fixture-mcp", "args":[], "env":{}}),
        },
    );
    let error = config.bind(&[binding("stdio")], false).await.err().unwrap();
    assert!(error.contains("sandbox launcher"));
}

fn identity(sender: &str) -> anchor_platform_session::ChannelIdentity {
    anchor_platform_session::ChannelIdentity {
        source: "wecom".into(),
        account: Some("corp".into()),
        conversation_kind: anchor_platform_session::ChannelConversationKind::Single,
        conversation_id: sender.into(),
        sender_id: sender.into(),
    }
}

fn identity_config(endpoint: &str) -> McpToolConfig {
    let mut config = config(endpoint, &[]);
    let declaration = json!({"type":"http","url":endpoint,"headers":{},
        "channel_identity_headers":{"X-Channel-Sender":"sender_id","X-Channel-Kind":"conversation_kind"}});
    let server = config.servers.get_mut("fixture-plugin-fixture").unwrap();
    server.declaration = declaration.clone();
    server.config = declaration;
    config.disabled.insert("fixture-plugin-fixture".into());
    config
}

#[tokio::test]
async fn declared_channel_identity_reaches_only_the_declaring_server_from_the_host() {
    let seen = Arc::new(std::sync::Mutex::new(
        Vec::<(Option<String>, Option<String>)>::new(),
    ));
    let fixture = {
        let cancellation = CancellationToken::new();
        let handler = Fixture {
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let service: StreamableHttpService<Fixture, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(handler.clone()),
                Default::default(),
                StreamableHttpServerConfig::default()
                    .with_sse_keep_alive(None)
                    .with_cancellation_token(cancellation.child_token()),
            );
        let record = seen.clone();
        let app = Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn(move |request: Request, next: Next| {
                let record = record.clone();
                let header = |name: &str| {
                    request
                        .headers()
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned)
                };
                let observed = (header("x-channel-sender"), header("x-channel-kind"));
                record.lock().unwrap().push(observed);
                async move { next.run(request).await }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (endpoint, cancellation, task)
    };
    let bindings = [binding("fixture")];

    // Without an attested message the declaring server never launches.
    let mut config = identity_config(&fixture.0);
    config.attest_channel_identity(None);
    let live = config.bind(&bindings, true).await.unwrap();
    assert!(live.definitions().is_empty());
    live.shutdown().await.unwrap();
    assert!(seen.lock().unwrap().is_empty());

    // A header value that cannot be sent keeps it disabled as well.
    let mut config = identity_config(&fixture.0);
    config.attest_channel_identity(Some(&identity("bad\nsender")));
    let live = config.bind(&bindings, true).await.unwrap();
    assert!(live.definitions().is_empty());
    live.shutdown().await.unwrap();

    let mut config = identity_config(&fixture.0);
    config.attest_channel_identity(Some(&identity("alice")));
    let live = config.bind(&bindings, true).await.unwrap();
    assert_eq!(live.definitions().len(), 2);
    live.call("fixture-plugin-fixture_echo", json!({"value":"x"}))
        .await
        .unwrap();
    live.shutdown().await.unwrap();
    let requests = seen.lock().unwrap().clone();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|request| request == &(Some("alice".to_owned()), Some("single".to_owned())))
    );

    // A server without the declaration receives no channel identity.
    seen.lock().unwrap().clear();
    let mut plain = config_plain(&fixture.0);
    plain.attest_channel_identity(Some(&identity("alice")));
    let live = plain.bind(&bindings, true).await.unwrap();
    assert_eq!(live.definitions().len(), 2);
    live.shutdown().await.unwrap();
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|request| request == &(None, None))
    );
    fixture.1.cancel();
    fixture.2.abort();
}

#[tokio::test]
async fn a_connection_test_never_connects_a_server_that_requires_channel_identity() {
    // The endpoint is never contacted: the server stays disabled before transport.
    let config = identity_config("http://127.0.0.1:9/mcp");
    let workspace = tempfile::tempdir().unwrap();
    let sandbox = BubblewrapSandbox::new(
        anchor_sandbox_bwrap::BubblewrapPolicy::new("bwrap", ["sh"])
            .authorize_workspace_root(workspace.path()),
    )
    .unwrap();
    let results = config
        .test_connections(
            &[binding("fixture")],
            SandboxLaunch {
                sandbox: &sandbox,
                workspace: workspace.path(),
                readonly_inputs: &[],
                plugin_views: &BTreeMap::new(),
            },
        )
        .await;
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["status"], "unsupported");
    assert!(
        results[0]["message"]
            .as_str()
            .unwrap()
            .contains("渠道成员身份"),
        "{results:?}"
    );
}

fn config_plain(endpoint: &str) -> McpToolConfig {
    config(endpoint, &[])
}
