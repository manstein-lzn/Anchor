//! Host-owned MCP connections. Server configuration, credentials and live
//! clients stay in this crate; only manifest-authorized tool calls are exposed.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo, Tool},
    service::{RoleClient, RunningService},
    transport::StreamableHttpClientTransport,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

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

/// An explicit host launch/connection description. The manifest chooses the
/// server id and allowed tool names; it cannot provide commands, URLs or keys.
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
            } => f
                .debug_struct("StreamableHttp")
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

/// Host-owned binding for one declared Plugin MCP server.
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    pub server_id: String,
    pub transport: McpTransportConfig,
    /// Exact names declared by the Plugin manifest. Every name must exist on
    /// the remote server at bind time; undeclared tools are never callable.
    pub allowed_tools: BTreeSet<String>,
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
    #[error("MCP server id and allowed tool list must be non-empty")]
    InvalidBinding,
    #[error("MCP transport connection failed: {0}")]
    Connect(String),
    #[error(
        "stdio MCP launch is unsupported without a host-provided sandboxed launcher; launch it inside Sandbox and bind its RMCP service with bind_service"
    )]
    StdioRequiresSandboxLauncher,
    #[error("MCP tool inventory failed: {0}")]
    Inventory(String),
    #[error("manifest-declared MCP tool is missing: {0}")]
    MissingTool(String),
    #[error("MCP tool is not bound by the Plugin manifest: {0}")]
    NotBound(String),
    #[error("MCP tool call failed: {0}")]
    Call(String),
    #[error("MCP response could not be represented as JSON: {0}")]
    Encode(String),
}

/// Connected MCP host adapter bound to an immutable manifest tool allowlist.
/// The live RMCP service is intentionally neither serializable nor Debug.
pub struct McpHost {
    server_id: String,
    allowed_tools: BTreeSet<String>,
    inventory: BTreeMap<String, Tool>,
    service: RunningService<RoleClient, ClientInfo>,
}

impl fmt::Debug for McpHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpHost")
            .field("server_id", &self.server_id)
            .field("allowed_tools", &self.allowed_tools)
            .field("inventory", &self.inventory.keys().collect::<Vec<_>>())
            .field("service", &"[live client redacted]")
            .finish()
    }
}

impl McpHost {
    pub async fn connect(config: McpServerConfig) -> Result<Self, McpHostError> {
        if config.server_id.trim().is_empty() || config.allowed_tools.is_empty() {
            return Err(McpHostError::InvalidBinding);
        }
        let service = match &config.transport {
            McpTransportConfig::Stdio { .. } => {
                return Err(McpHostError::StdioRequiresSandboxLauncher);
            }
            McpTransportConfig::StreamableHttp {
                endpoint,
                bearer_token,
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
                ClientInfo::default()
                    .serve(StreamableHttpClientTransport::from_config(transport))
                    .await
                    .map_err(|e| McpHostError::Connect(e.to_string()))?
            }
        };
        Self::bind_service(config.server_id, config.allowed_tools, service).await
    }

    /// Bind an already established public RMCP service. This keeps the same
    /// manifest checks available to embedding hosts and deterministic tests.
    pub async fn bind_service(
        server_id: String,
        allowed_tools: BTreeSet<String>,
        service: RunningService<RoleClient, ClientInfo>,
    ) -> Result<Self, McpHostError> {
        if server_id.trim().is_empty() || allowed_tools.is_empty() {
            return Err(McpHostError::InvalidBinding);
        }
        let tools = service
            .peer()
            .list_all_tools()
            .await
            .map_err(|e| McpHostError::Inventory(e.to_string()))?;
        let mut inventory = BTreeMap::new();
        for tool in tools {
            let name = tool.name.to_string();
            if allowed_tools.contains(&name) {
                inventory.insert(name, tool);
            }
        }
        for name in &allowed_tools {
            if !inventory.contains_key(name) {
                return Err(McpHostError::MissingTool(name.clone()));
            }
        }
        Ok(Self {
            server_id,
            allowed_tools,
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

    /// Convert only the manifest-bound remote tools to Rig DynamicTools.
    ///
    /// This preserves Rig's native MCP result handling and liveness checks;
    /// tools advertised by the server but omitted from the manifest are
    /// intentionally never registered with the model.
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
    /// Keep MCP structured data and rich content through Rig's public adapter.
    pub async fn call_rig(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Vec<rig_core::message::ToolResultContent>, McpHostError> {
        let response = self.call_raw(tool_name, arguments).await?;
        if response.is_error.unwrap_or(false) {
            return Err(McpHostError::Call("remote tool reported failure".into()));
        }
        rig_rmcp::mcp_result_output(&response)
            .map(|output| output.into_content())
            .map_err(|_| McpHostError::Encode("unsupported tool result".into()))
    }

    async fn call_raw(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<rmcp::model::CallToolResult, McpHostError> {
        if !self.allowed_tools.contains(tool_name) || !self.inventory.contains_key(tool_name) {
            return Err(McpHostError::NotBound(tool_name.to_owned()));
        }
        let arguments = arguments
            .as_object()
            .cloned()
            .ok_or_else(|| McpHostError::Call("MCP tool arguments must be a JSON object".into()))?;
        self.service
            .peer()
            .call_tool(CallToolRequestParams::new(tool_name.to_owned()).with_arguments(arguments))
            .await
            .map_err(|_| McpHostError::Call("transport request failed".into()))
    }
}

/// Publicly re-export Rig's adapter type so the host can register a bound
/// inventory as Rig DynamicTools without implementing tool conversion itself.
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
        let host = McpHost::bind_service(
            "fixture".into(),
            BTreeSet::from(["echo".to_owned()]),
            service,
        )
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
            ["echo"]
        );
        let result = host
            .call("echo", serde_json::json!({"value": "hello"}))
            .await
            .expect("bound call succeeds");
        assert!(!result.is_error);
        assert!(result.content[0].to_string().contains("hello"));
        assert!(matches!(
            host.call("outside", serde_json::json!({})).await,
            Err(McpHostError::NotBound(_))
        ));
        let rig_tools = host.rig_tools();
        assert_eq!(rig_tools.len(), 1);
        assert_eq!(rig_tools[0].name(), "echo");
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
            },
            allowed_tools: BTreeSet::from(["echo".to_owned()]),
        })
        .await
        .expect("HTTP MCP binding");
        assert_eq!(
            host.tools()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["echo"]
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
    async fn missing_manifest_tool_rejects_binding() {
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
        let result = McpHost::bind_service(
            "fixture".into(),
            BTreeSet::from(["not-advertised".to_owned()]),
            service,
        )
        .await;
        assert!(matches!(result, Err(McpHostError::MissingTool(_))));
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
            allowed_tools: BTreeSet::from(["echo".into()]),
        };
        assert!(matches!(
            McpHost::connect(config).await,
            Err(McpHostError::StdioRequiresSandboxLauncher)
        ));
    }

    #[test]
    fn credentials_are_redacted_and_not_in_returnable_state() {
        let config = McpTransportConfig::StreamableHttp {
            endpoint: "https://user:pass@mcp.invalid/mcp?token=query-secret".into(),
            bearer_token: Some(Secret::new("super-secret-token")),
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
