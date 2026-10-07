//! Host-owned MCP connections. Server configuration, credentials and live
//! clients stay in this crate; only tools from an admitted Plugin server are exposed.

use std::{collections::BTreeMap, fmt, sync::Arc};

use anchor_runtime_rig::ToolResultContent;
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo, Tool},
    service::{RoleClient, RunningService},
    transport::StreamableHttpClientTransport,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

mod contents;
mod images;
pub use images::{ImageBudget, validate_image_bytes};

/// A credential whose value is never included in Debug output or serde state.
#[derive(Clone, Default)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

/// An explicit host launch/connection description. The Plugin chooses the
/// server id; commands, URLs, headers and keys are supplied by the host.
#[derive(Clone)]
pub enum McpTransportConfig {
    Stdio {
        program: String,
        args: Vec<String>,
        env: BTreeMap<String, Secret>,
    },
    StreamableHttp {
        endpoint: String,
        bearer_token: Option<Secret>,
        headers: BTreeMap<String, String>,
    },
    /// Legacy MCP SSE is represented so hosts can report it explicitly. The
    /// pinned RMCP 2.2 client does not implement that transport.
    Sse {
        endpoint: String,
        bearer_token: Option<Secret>,
        headers: BTreeMap<String, String>,
    },
}

impl fmt::Debug for McpTransportConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio { program, args, env } => f
                .debug_struct("Stdio")
                .field("program", program)
                .field("args", &format!("[{} arguments redacted]", args.len()))
                .field("env", &env.keys().collect::<Vec<_>>())
                .finish(),
            Self::StreamableHttp {
                endpoint,
                bearer_token,
                ..
            } => f
                .debug_struct("StreamableHttp")
                .field("endpoint", &redacted_endpoint(endpoint))
                .field("bearer_token", &bearer_token.as_ref().map(|_| "[REDACTED]"))
                .finish(),
            Self::Sse {
                endpoint,
                bearer_token,
                ..
            } => f
                .debug_struct("Sse")
                .field("endpoint", &redacted_endpoint(endpoint))
                .field("bearer_token", &bearer_token.as_ref().map(|_| "[REDACTED]"))
                .finish(),
        }
    }
}

fn redacted_endpoint(_endpoint: &str) -> &'static str {
    // Endpoint userinfo/query parameters can contain credentials as well.
    "[configured endpoint]"
}

fn custom_headers(
    headers: &BTreeMap<String, String>,
) -> Result<std::collections::HashMap<http::HeaderName, http::HeaderValue>, McpHostError> {
    headers
        .iter()
        .map(|(name, value)| {
            let name = http::HeaderName::try_from(name).map_err(|error| {
                McpHostError::Connect(format!("invalid MCP header name: {error}"))
            })?;
            let value = http::HeaderValue::try_from(value).map_err(|error| {
                McpHostError::Connect(format!("invalid MCP header value: {error}"))
            })?;
            Ok((name, value))
        })
        .collect()
}

/// Host-owned binding for one declared Plugin MCP server.
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    pub server_id: String,
    pub transport: McpTransportConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDescription {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCallResult {
    pub content: Vec<Value>,
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

#[derive(Debug, Error)]
pub enum McpHostError {
    #[error("MCP server id must be non-empty")]
    InvalidBinding,
    #[error("MCP transport connection failed: {0}")]
    Connect(String),
    #[error("MCP transport is unsupported by the pinned RMCP client: {0}")]
    UnsupportedTransport(String),
    #[error(
        "stdio MCP launch is unsupported without a host-provided sandboxed launcher; launch it inside Sandbox and bind its RMCP service with bind_service"
    )]
    StdioRequiresSandboxLauncher,
    #[error("MCP tool inventory failed: {0}")]
    Inventory(String),
    #[error("MCP tool is not bound by the Plugin manifest: {0}")]
    NotBound(String),
    #[error("MCP tool call failed: {0}")]
    Call(String),
    #[error("MCP response could not be represented as JSON: {0}")]
    Encode(String),
}

/// Connected MCP host adapter bound to one immutable Plugin server inventory.
/// The live RMCP service is intentionally neither serializable nor Debug.
pub struct McpHost {
    server_id: String,
    inventory: BTreeMap<String, Tool>,
    service: RunningService<RoleClient, ClientInfo>,
}

impl fmt::Debug for McpHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpHost")
            .field("server_id", &self.server_id)
            .field("inventory", &self.inventory.keys().collect::<Vec<_>>())
            .field("service", &"[live client redacted]")
            .finish()
    }
}

impl McpHost {
    pub async fn connect(config: McpServerConfig) -> Result<Self, McpHostError> {
        if config.server_id.trim().is_empty() {
            return Err(McpHostError::InvalidBinding);
        }
        let service = match &config.transport {
            McpTransportConfig::Stdio { .. } => {
                return Err(McpHostError::StdioRequiresSandboxLauncher);
            }
            McpTransportConfig::StreamableHttp {
                endpoint,
                bearer_token,
                headers,
            } => {
                if endpoint.trim().is_empty() {
                    return Err(McpHostError::InvalidBinding);
                }
                let mut transport =
                    rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                        Arc::<str>::from(endpoint.as_str()),
                    );
                if let Some(token) = bearer_token {
                    transport.auth_header = Some(token.expose().to_owned());
                }
                transport.custom_headers = custom_headers(headers)?;
                ClientInfo::default()
                    .serve(StreamableHttpClientTransport::from_config(transport))
                    .await
                    .map_err(|e| McpHostError::Connect(e.to_string()))?
            }
            McpTransportConfig::Sse { .. } => {
                return Err(McpHostError::UnsupportedTransport(
                    "legacy SSE transport is not provided by RMCP 2.2; use streamable HTTP".into(),
                ));
            }
        };
        Self::bind_service(config.server_id, service).await
    }

    /// Connect a stdio MCP server through a command that was already created
    /// by the host sandbox. The command is intentionally supplied by the
    /// caller: this adapter never spawns an arbitrary Plugin command itself.
    /// The RMCP child transport owns the process and terminates it when the
    /// live service is dropped.
    pub async fn connect_sandboxed_stdio(
        server_id: String,
        command: tokio::process::Command,
    ) -> Result<Self, McpHostError> {
        if server_id.trim().is_empty() {
            return Err(McpHostError::InvalidBinding);
        }
        let (transport, _stderr) = rmcp::transport::TokioChildProcess::builder(command)
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| McpHostError::Connect(error.to_string()))?;
        let service = ClientInfo::default()
            .serve(transport)
            .await
            .map_err(|error| McpHostError::Connect(error.to_string()))?;
        Self::bind_service(server_id, service).await
    }

    /// Bind an already established public RMCP service. This keeps the same
    /// manifest checks available to embedding hosts and deterministic tests.
    pub async fn bind_service(
        server_id: String,
        service: RunningService<RoleClient, ClientInfo>,
    ) -> Result<Self, McpHostError> {
        if server_id.trim().is_empty() {
            return Err(McpHostError::InvalidBinding);
        }
        let tools = service
            .peer()
            .list_all_tools()
            .await
            .map_err(|e| McpHostError::Inventory(e.to_string()))?;
        let inventory = tools
            .into_iter()
            .map(|tool| (tool.name.to_string(), tool))
            .collect();
        Ok(Self {
            server_id,
            inventory,
            service,
        })
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn tools(&self) -> Vec<ToolDescription> {
        self.inventory
            .iter()
            .map(|(name, tool)| ToolDescription {
                name: name.clone(),
                description: tool.description.as_ref().map(ToString::to_string),
                input_schema: serde_json::to_value(tool.input_schema.as_ref())
                    .unwrap_or(Value::Null),
            })
            .collect()
    }

    /// Convert the bound remote tools to Rig DynamicTools.
    ///
    /// This preserves Rig's native MCP result handling and liveness checks;
    /// The catalog selects the server; the MCP handshake supplies its tools.
    #[cfg(feature = "rig-legacy")]
    pub fn rig_tools(&self) -> Vec<rig_core::tool::DynamicTool> {
        self.inventory
            .values()
            .cloned()
            .map(|definition| {
                let tool = McpTool::from_mcp_server(definition, self.service.peer().clone());
                rig_core::tool::DynamicTool::from(tool)
            })
            .collect()
    }

    pub async fn call(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolCallResult, McpHostError> {
        let response = self.call_raw(tool_name, arguments).await?;
        let content = response
            .content
            .into_iter()
            .map(|part| serde_json::to_value(part).map_err(|e| McpHostError::Encode(e.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ToolCallResult {
            content,
            structured_content: response.structured_content,
            is_error: response.is_error.unwrap_or(false),
        })
    }
    pub async fn call_contents(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Vec<ToolResultContent>, McpHostError> {
        let response = self.call_raw(tool_name, arguments).await?;
        if response.is_error.unwrap_or(false) {
            return Err(McpHostError::Call("remote tool reported failure".into()));
        }
        contents::result_contents(&response)
    }

    #[cfg(feature = "rig-legacy")]
    pub async fn call_rig(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Vec<rig_core::message::ToolResultContent>, McpHostError> {
        self.call_contents(tool_name, arguments).await
    }

    async fn call_raw(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<rmcp::model::CallToolResult, McpHostError> {
        if !self.inventory.contains_key(tool_name) {
            return Err(McpHostError::NotBound(tool_name.to_owned()));
        }
        let arguments = match arguments {
            Value::Null => serde_json::Map::new(),
            Value::Object(arguments) => arguments,
            _ => {
                return Err(McpHostError::Call(
                    "MCP tool arguments must be a JSON object".into(),
                ));
            }
        };
        self.service
            .peer()
            .call_tool(CallToolRequestParams::new(tool_name.to_owned()).with_arguments(arguments))
            .await
            .map_err(|_| McpHostError::Call("transport request failed".into()))
    }
}

/// Publicly re-export Rig's adapter type so the host can register a bound
/// inventory as Rig DynamicTools without implementing tool conversion itself.
#[cfg(feature = "rig-legacy")]
pub use rig_rmcp::{McpTool, tools_from_server};

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use rmcp::{
        RoleServer, ServerHandler,
        model::{
            CallToolResult, ContentBlock, ErrorData, Implementation, ProtocolVersion,
            ServerCapabilities, ServerInfo,
        },
        service::RequestContext,
    };
    use tokio_util::sync::CancellationToken;

    #[derive(Clone)]
    struct Fixture;

    impl ServerHandler for Fixture {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
                .with_protocol_version(ProtocolVersion::LATEST)
                .with_server_info(Implementation::new("anchor-mcp-fixture", "0.1"))
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<rmcp::model::ListToolsResult, ErrorData> {
            Ok(rmcp::model::ListToolsResult {
                tools: vec![
                    Tool::new(
                        "echo".to_owned(),
                        "echo input".to_owned(),
                        Arc::new(serde_json::Map::new()),
                    ),
                    Tool::new(
                        "outside".to_owned(),
                        "must not bind".to_owned(),
                        Arc::new(serde_json::Map::new()),
                    ),
                ],
                next_cursor: None,
                meta: None,
            })
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResult, ErrorData> {
            if let Some(response) = request
                .arguments
                .as_ref()
                .and_then(|arguments| arguments.get("response"))
            {
                return serde_json::from_value(response.clone())
                    .map_err(|_| ErrorData::invalid_params("invalid fixture response", None));
            }
            let value = request
                .arguments
                .as_ref()
                .and_then(|args| args.get("value"))
                .cloned()
                .unwrap_or(Value::Null);
            Ok(CallToolResult::success(vec![ContentBlock::json(value)?]))
        }
    }

    async fn connected_fixture() -> (McpHost, tokio::task::JoinHandle<()>) {
        let (client_io, server_io) = tokio::io::duplex(16_384);
        let task = tokio::spawn(async move {
            if let Ok(running) = Fixture.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let service = ClientInfo::default()
            .serve(client_io)
            .await
            .expect("fake MCP server handshake");
        let host = McpHost::bind_service("fixture".into(), service)
            .await
            .expect("manifest binding");
        (host, task)
    }

    #[tokio::test]
    async fn manifest_inventory_is_bound_and_tool_call_runs() {
        let (host, server) = connected_fixture().await;
        assert_eq!(
            host.tools()
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["echo", "outside"]
        );
        let result = host
            .call("echo", serde_json::json!({"value": "hello"}))
            .await
            .expect("bound call succeeds");
        assert!(!result.is_error);
        assert!(result.content[0].to_string().contains("hello"));
        let outside = host
            .call("outside", serde_json::json!({}))
            .await
            .expect("all inventory tools are bound");
        assert!(!outside.is_error);
        #[cfg(feature = "rig-legacy")]
        {
            let rig_tools = host.rig_tools();
            assert_eq!(rig_tools.len(), 2);
            assert_eq!(rig_tools[0].name(), "echo");
        }
        server.abort();
    }

    #[tokio::test]
    async fn call_contents_uses_bound_rmcp_service_and_preserves_structured_results() {
        let (host, server) = connected_fixture().await;
        let response = CallToolResult::structured(serde_json::json!({"value": "native"}));
        let contents = host
            .call_contents("echo", serde_json::json!({"response": response}))
            .await
            .unwrap();
        assert_eq!(
            contents,
            vec![ToolResultContent::json(
                serde_json::json!({"value": "native"})
            )]
        );
        let literal = host
            .call_contents("echo", serde_json::json!({"value": "literal"}))
            .await
            .unwrap();
        assert_eq!(literal[0].as_text(), Some("\"literal\""));
        server.abort();
    }

    #[tokio::test]
    async fn call_contents_keeps_remote_failure_inventory_and_argument_guards() {
        let (host, server) = connected_fixture().await;
        let response = CallToolResult::error(vec![ContentBlock::text("remote failure")]);
        assert!(matches!(
            host.call_contents("echo", serde_json::json!({"response": response})).await,
            Err(McpHostError::Call(message)) if message == "remote tool reported failure"
        ));
        assert!(matches!(
            host.call_contents("not_bound", Value::Null).await,
            Err(McpHostError::NotBound(name)) if name == "not_bound"
        ));
        assert!(matches!(
            host.call_contents("echo", serde_json::json!([1, 2])).await,
            Err(McpHostError::Call(message)) if message.contains("JSON object")
        ));
        server.abort();
    }

    #[cfg(not(feature = "rig-legacy"))]
    #[tokio::test]
    async fn invalid_native_image_is_rejected_and_raw_call_keeps_complete_content() {
        let (host, server) = connected_fixture().await;
        let response = CallToolResult::success(vec![ContentBlock::image("AAEC", "image/png")]);
        let arguments = serde_json::json!({"response": response});
        assert!(matches!(
            host.call_contents("echo", arguments.clone()).await,
            Err(McpHostError::Encode(message)) if message.contains("invalid image")
        ));
        let raw = host.call("echo", arguments).await.unwrap();
        assert_eq!(
            raw.content,
            vec![serde_json::json!({"type": "image", "data": "AAEC", "mimeType": "image/png"})]
        );
        server.abort();
    }

    #[cfg(not(feature = "rig-legacy"))]
    #[tokio::test]
    async fn valid_native_image_remains_typed_through_the_bound_mcp_service() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let (host, server) = connected_fixture().await;
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let data = STANDARD.encode(bytes.into_inner());
        let response = CallToolResult::success(vec![
            ContentBlock::text("before"),
            ContentBlock::image(&data, "image/png"),
            ContentBlock::text("after"),
        ]);
        let contents = host
            .call_contents("echo", serde_json::json!({"response":response}))
            .await
            .unwrap();
        assert_eq!(
            contents,
            vec![
                ToolResultContent::text("before"),
                ToolResultContent::image(&data, "image/png"),
                ToolResultContent::text("after")
            ]
        );
        server.abort();
    }

    #[cfg(feature = "rig-legacy")]
    #[tokio::test]
    async fn legacy_call_rig_remains_an_alias_with_typed_media_support() {
        let (host, server) = connected_fixture().await;
        let response = CallToolResult::success(vec![ContentBlock::image("AAEC", "image/png")]);
        let arguments = serde_json::json!({"response": response});
        let contents = host.call_contents("echo", arguments.clone()).await.unwrap();
        assert!(matches!(contents[0], ToolResultContent::Image(_)));
        assert_eq!(host.call_rig("echo", arguments).await.unwrap(), contents);
        server.abort();
    }

    #[tokio::test]
    async fn streamable_http_transport_binds_manifest_tools_and_calls_over_http() {
        let cancellation = CancellationToken::new();
        let service: rmcp::transport::StreamableHttpService<
            Fixture,
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
        > = rmcp::transport::StreamableHttpService::new(
            || Ok(Fixture),
            Default::default(),
            rmcp::transport::StreamableHttpServerConfig::default()
                .with_sse_keep_alive(None)
                .with_cancellation_token(cancellation.child_token()),
        );
        let router = Router::new().nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let host = McpHost::connect(McpServerConfig {
            server_id: "fixture-http".into(),
            transport: McpTransportConfig::StreamableHttp {
                endpoint: format!("http://{address}/mcp"),
                bearer_token: None,
                headers: BTreeMap::new(),
            },
        })
        .await
        .expect("HTTP MCP binding");
        assert_eq!(
            host.tools()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["echo", "outside"]
        );
        let result = host
            .call("echo", serde_json::json!({"value":"http"}))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result
                .content
                .iter()
                .any(|part| part.to_string().contains("http"))
        );
        let _ = host.service.cancel().await;
        cancellation.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn inventory_binding_does_not_require_a_tool_allowlist() {
        let (client_io, server_io) = tokio::io::duplex(16_384);
        let task = tokio::spawn(async move {
            if let Ok(running) = Fixture.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let service = ClientInfo::default()
            .serve(client_io)
            .await
            .expect("handshake");
        let result = McpHost::bind_service("fixture".into(), service).await;
        let host = result.expect("inventory binding");
        assert_eq!(host.tools().len(), 2);
        task.abort();
    }

    #[tokio::test]
    async fn stdio_connect_fails_closed_without_a_sandbox_launcher() {
        let config = McpServerConfig {
            server_id: "untrusted".into(),
            transport: McpTransportConfig::Stdio {
                program: "/definitely/not/launched".into(),
                args: Vec::new(),
                env: BTreeMap::new(),
            },
        };
        assert!(matches!(
            McpHost::connect(config).await,
            Err(McpHostError::StdioRequiresSandboxLauncher)
        ));
    }

    #[tokio::test]
    async fn legacy_sse_is_reported_as_unsupported_by_rmcp_2() {
        let config = McpServerConfig {
            server_id: "legacy-sse".into(),
            transport: McpTransportConfig::Sse {
                endpoint: "http://127.0.0.1:9/sse".into(),
                bearer_token: None,
                headers: BTreeMap::new(),
            },
        };
        assert!(matches!(
            McpHost::connect(config).await,
            Err(McpHostError::UnsupportedTransport(message)) if message.contains("legacy SSE")
        ));
    }

    #[test]
    fn credentials_are_redacted_and_not_in_returnable_state() {
        let config = McpTransportConfig::StreamableHttp {
            endpoint: "https://user:pass@mcp.invalid/mcp?token=query-secret".into(),
            bearer_token: Some(Secret::new("super-secret-token")),
            headers: BTreeMap::new(),
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("super-secret-token"));
        assert!(!debug.contains("query-secret"));
        assert!(!debug.contains("user:pass"));
        assert!(debug.contains("REDACTED"));

        let secret = Secret::new("private-env-value");
        assert!(!format!("{secret:?}").contains("private-env-value"));

        let safe_record = ToolDescription {
            name: "echo".into(),
            description: Some("echo input".into()),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let persisted = serde_json::to_string(&safe_record).expect("safe state serializes");
        assert!(!persisted.contains("super-secret-token"));
        assert!(!persisted.contains("private-env-value"));
    }
}
