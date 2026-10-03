//! Host-owned, allowlisted tool bindings for the runner compatibility slice.
//!
//! The fixture server deliberately has no filesystem, process, network, or
//! credential capability. A Plugin identity must name its server and the host
//! must independently enable that Plugin before any tool is advertised.

use anchor_mcp_host::{McpHost, McpServerConfig, McpTransportConfig, Secret};
use anchor_runtime_rig::{ToolError, ToolPort, graph::PluginBinding};
use rig_agent::core::{
    completion::ToolDefinition,
    message::{ToolName, ToolResultContent},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    future::Future,
    pin::Pin,
    sync::Arc,
};

mod discovery;

pub const FAKE_SERVER_ID: &str = "anchor.fake";
pub const FAKE_ECHO_TOOL: &str = "anchor_fake__echo";
pub const MCP_SEARCH_TOOLS_TOOL: &str = "anchor_mcp__search_tools";
pub const MCP_CALL_TOOL: &str = "anchor_mcp__call_tool";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerEnvConfig {
    pub transport: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub bearer_token_env: Option<String>,
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct McpToolConfig {
    servers: BTreeMap<String, McpServerEnvConfig>,
}

impl McpToolConfig {
    pub fn from_environment() -> Result<Self, String> {
        let Some(raw) = env::var_os("ANCHOR_RUST_MCP_SERVERS") else {
            return Ok(Self::default());
        };
        let servers: BTreeMap<String, McpServerEnvConfig> =
            serde_json::from_str(&raw.to_string_lossy())
                .map_err(|e| format!("ANCHOR_RUST_MCP_SERVERS is invalid JSON: {e}"))?;
        for (id, config) in &servers {
            if id.trim().is_empty() || config.allowed_tools.is_empty() {
                return Err(format!(
                    "MCP server `{id}` needs a non-empty allowed_tools list"
                ));
            }
            if config.transport != "http" || config.endpoint.as_deref().unwrap_or("").is_empty() {
                return Err(format!(
                    "MCP server `{id}` requires transport=http and endpoint"
                ));
            }
        }
        Ok(Self { servers })
    }

    pub async fn bind(
        &self,
        bindings: &[PluginBinding],
        network: bool,
    ) -> Result<LiveMcpTools, String> {
        if !network
            && bindings
                .iter()
                .any(|b| b.mcp_servers.iter().any(|id| id != FAKE_SERVER_ID))
        {
            return Err("HTTP MCP requires node network=true and host configuration".into());
        }
        self.validate_bindings(bindings)?;
        let mut hosts = BTreeMap::new();
        for binding in bindings {
            for server_id in &binding.mcp_servers {
                if server_id == FAKE_SERVER_ID || hosts.contains_key(server_id) {
                    continue;
                }
                let config = self
                    .servers
                    .get(server_id)
                    .ok_or_else(|| format!("MCP server `{server_id}` is not configured"))?;
                let token = config
                    .bearer_token_env
                    .as_deref()
                    .map(|name| {
                        env::var(name)
                            .map(Secret::new)
                            .map_err(|_| format!("MCP credential environment `{name}` is missing"))
                    })
                    .transpose()?;
                let host = McpHost::connect(McpServerConfig {
                    server_id: server_id.clone(),
                    transport: McpTransportConfig::StreamableHttp {
                        endpoint: config.endpoint.clone().unwrap_or_default(),
                        bearer_token: token,
                    },
                    allowed_tools: config.allowed_tools.iter().cloned().collect(),
                })
                .await
                .map_err(|e| format!("MCP server `{server_id}` failed to bind: {e}"))?;
                hosts.insert(server_id.clone(), Arc::new(host));
            }
        }
        Ok(LiveMcpTools { hosts })
    }

    pub fn validate_bindings(&self, bindings: &[PluginBinding]) -> Result<(), String> {
        let mut seen_servers = BTreeSet::new();
        for binding in bindings {
            for server_id in &binding.mcp_servers {
                if server_id == FAKE_SERVER_ID || !seen_servers.insert(server_id) {
                    continue;
                }
                let config = self
                    .servers
                    .get(server_id)
                    .ok_or_else(|| format!("MCP server `{server_id}` is not configured"))?;
                let mut seen_tools = BTreeSet::new();
                for name in &config.allowed_tools {
                    if name.is_empty()
                        || name.len() > 64
                        || !name
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                        || name == FAKE_ECHO_TOOL
                        || name == MCP_SEARCH_TOOLS_TOOL
                        || name == MCP_CALL_TOOL
                        || name == crate::node_tools::RUN_TOOL_NAME
                        || !seen_tools.insert(name)
                    {
                        return Err(
                            "MCP tool names must be valid, unique per server, and not reserved"
                                .into(),
                        );
                    }
                }
                if let Some(name) = config.bearer_token_env.as_deref()
                    && env::var(name).is_err()
                {
                    return Err(format!("MCP credential environment `{name}` is missing"));
                }
            }
        }
        Ok(())
    }
}

pub struct LiveMcpTools {
    hosts: BTreeMap<String, Arc<McpHost>>,
}

impl ToolPort for LiveMcpTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        if self.hosts.is_empty() {
            return Vec::new();
        }

        discovery::definitions()
    }

    fn is_read_only(&self, name: &str) -> bool {
        name == MCP_SEARCH_TOOLS_TOOL
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            match name {
                MCP_SEARCH_TOOLS_TOOL => {
                    let result = discovery::search(&self.hosts, arguments)?;
                    Ok(vec![ToolResultContent::json(result)])
                }
                MCP_CALL_TOOL => self.call_tool(arguments).await,
                _ => Err(ToolError::Unknown(name.to_owned())),
            }
        })
    }
}

impl LiveMcpTools {
    async fn call_tool(&self, arguments: Value) -> Result<Vec<ToolResultContent>, ToolError> {
        let object = arguments
            .as_object()
            .ok_or_else(|| ToolError::Failed("MCP tool call arguments must be an object".into()))?;
        if object.len() != 3
            || !object.contains_key("server_id")
            || !object.contains_key("tool_name")
            || !object.contains_key("arguments")
        {
            return Err(ToolError::Failed(
                "MCP tool call requires only server_id, tool_name, and arguments".into(),
            ));
        }
        let server_id = object
            .get("server_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ToolError::Failed("MCP server_id must not be empty".into()))?;
        let tool_name = object
            .get("tool_name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ToolError::Failed("MCP tool_name must not be empty".into()))?;
        let tool_arguments = object
            .get("arguments")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| ToolError::Failed("MCP tool arguments must be a JSON object".into()))?;

        let host = self
            .hosts
            .get(server_id)
            .ok_or_else(|| ToolError::Unknown(format!("MCP server `{server_id}`")))?;
        host.call_rig(tool_name, tool_arguments)
            .await
            .map_err(|error| ToolError::Failed(error.to_string()))
    }
}

pub struct CombinedPluginTools {
    fake: BoundPluginTools,
    pub(crate) live: LiveMcpTools,
}

impl CombinedPluginTools {
    pub fn new(fake: BoundPluginTools, live: LiveMcpTools) -> Self {
        Self { fake, live }
    }
}

impl ToolPort for CombinedPluginTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.fake.definitions();
        definitions.extend(self.live.definitions());
        definitions
    }

    fn is_read_only(&self, name: &str) -> bool {
        name == MCP_SEARCH_TOOLS_TOOL
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if name == FAKE_ECHO_TOOL {
                self.fake.call(name, arguments).await
            } else {
                self.live.call(name, arguments).await
            }
        })
    }
}

/// One deterministic provider-free tool server supplied by the host.
#[derive(Clone, Copy)]
pub struct FakeEchoServer;

impl FakeEchoServer {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            ToolName::new(FAKE_ECHO_TOOL).expect("static name"),
            "Return the supplied JSON value unchanged. This is a local test fixture.",
            json!({
                "type": "object",
                "properties": {"value": {}},
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    fn call(&self, arguments: Value) -> Result<Vec<ToolResultContent>, ToolError> {
        let object = arguments
            .as_object()
            .ok_or_else(|| ToolError::Failed("fake echo arguments must be a JSON object".into()))?;
        let value = object
            .get("value")
            .ok_or_else(|| ToolError::Failed("fake echo requires `value`".into()))?;
        if object.len() != 1 {
            return Err(ToolError::Failed(
                "fake echo accepts only the `value` argument".into(),
            ));
        }
        Ok(vec![ToolResultContent::json(value.clone())])
    }
}

/// AgentNode ToolPort assembled from the Plugin identities in a single node
/// request and the host's explicit Plugin allowlist.
#[derive(Clone)]
pub struct PluginToolHost {
    plugin_ids: BTreeSet<String>,
    server: FakeEchoServer,
}

impl PluginToolHost {
    pub fn new(allowed_plugin_ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            plugin_ids: allowed_plugin_ids.into_iter().collect(),
            server: FakeEchoServer,
        }
    }

    /// Create a synthetic, secret-free fixture binding. The caller must ensure
    /// this exact fixture id was enabled by host configuration. It is not a
    /// filesystem Plugin manifest and must not be used as one.
    #[allow(dead_code)]
    pub fn resolve_fixture_plugins(
        &self,
        plugin_ids: &[String],
    ) -> Result<Vec<PluginBinding>, String> {
        plugin_ids
            .iter()
            .map(|id| {
                if !self.plugin_ids.contains(id) || id != "fake-tools" {
                    return Err(format!(
                        "Plugin `{id}` is not enabled by the fake tool host"
                    ));
                }
                Ok(PluginBinding {
                    id: id.clone(),
                    digest: "anchor.fake.echo.v1".into(),
                    resources: Vec::new(),
                    mcp_servers: vec![FAKE_SERVER_ID.into()],
                })
            })
            .collect()
    }

    pub fn for_bindings(&self, bindings: &[PluginBinding]) -> Result<BoundPluginTools, ToolError> {
        let mut enabled = false;
        for binding in bindings {
            if binding
                .mcp_servers
                .iter()
                .any(|server| server == FAKE_SERVER_ID)
            {
                if !self.plugin_ids.contains(&binding.id) {
                    return Err(ToolError::Unknown(binding.id.clone()));
                }
                enabled = true;
            }
        }
        Ok(BoundPluginTools {
            server: self.server,
            enabled,
        })
    }

    /// Resolve the Plugin tools for one admitted node request into an owned
    /// port. The returned trait object retains the exact fake/live bindings
    /// and is suitable for an asynchronous node-host resolver.
    pub async fn assemble(
        &self,
        mcp: &McpToolConfig,
        bindings: &[PluginBinding],
        network: bool,
    ) -> Result<Arc<dyn ToolPort>, String> {
        let fake = self
            .for_bindings(bindings)
            .map_err(|error| error.to_string())?;
        let live = mcp.bind(bindings, network).await?;
        Ok(Arc::new(CombinedPluginTools::new(fake, live)))
    }
}

pub struct BoundPluginTools {
    server: FakeEchoServer,
    enabled: bool,
}

impl ToolPort for BoundPluginTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        if self.enabled {
            vec![self.server.definition()]
        } else {
            Vec::new()
        }
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if !self.enabled || name != self.server.definition().name {
                return Err(ToolError::Unknown(name.to_owned()));
            }
            self.server.call(arguments)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn explicitly_enabled_plugin_advertises_and_calls_fake_echo() {
        let host = PluginToolHost::new(["fake-tools".to_owned()]);
        let bindings = host
            .resolve_fixture_plugins(&["fake-tools".into()])
            .expect("host-enabled fixture resolves");
        let tools = host.for_bindings(&bindings).expect("fixture binds");
        assert_eq!(tools.definitions().len(), 1);
        let result = tools
            .call(
                "anchor_fake__echo",
                json!({"value": {"message": "provider-free"}}),
            )
            .await
            .expect("fake tool call succeeds");
        assert_eq!(
            result[0].as_json(),
            Some(&json!({"message": "provider-free"}))
        );
    }

    #[tokio::test]
    async fn plugin_host_assembles_owned_trait_object_for_node_resolvers() {
        let host = PluginToolHost::new(["fake-tools".to_owned()]);
        let bindings = host
            .resolve_fixture_plugins(&["fake-tools".into()])
            .expect("fixture binding resolves");
        let tools = host
            .assemble(&McpToolConfig::default(), &bindings, false)
            .await
            .expect("owned plugin port assembles");

        assert!(
            tools
                .definitions()
                .iter()
                .any(|tool| tool.name == FAKE_ECHO_TOOL)
        );
        let result = tools
            .call("anchor_fake__echo", json!({"value": "owned"}))
            .await
            .expect("owned fixture call succeeds");
        assert_eq!(result[0].as_json(), Some(&json!("owned")));
    }

    #[tokio::test]
    async fn disabled_plugin_unknown_tool_and_path_like_arguments_are_rejected() {
        let host = PluginToolHost::new(std::iter::empty());
        assert!(
            host.resolve_fixture_plugins(&["fake-tools".into()])
                .is_err()
        );

        let host = PluginToolHost::new(["fake-tools".to_owned()]);
        let bindings = host
            .resolve_fixture_plugins(&["fake-tools".into()])
            .unwrap();
        let tools = host.for_bindings(&bindings).unwrap();
        assert!(tools.call("anything", json!({})).await.is_err());
        assert!(
            tools
                .call("anchor_fake__echo", json!({"path": "/etc/passwd"}))
                .await
                .is_err()
        );
        assert!(
            tools
                .call(
                    "anchor_fake__echo",
                    json!({"value": "hello", "path": "/etc/passwd"})
                )
                .await
                .is_err()
        );
        assert!(
            tools
                .call(
                    "anchor_fake__echo",
                    json!({"value": {"path": "/etc/passwd"}})
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn live_mcp_binding_fails_closed_without_deployment_config() {
        let fake_host = PluginToolHost::new(std::iter::empty());
        let real_binding = PluginBinding {
            id: "docmost".into(),
            digest: "digest".into(),
            resources: Vec::new(),
            mcp_servers: vec!["docmost".into()],
        };
        assert!(
            fake_host
                .for_bindings(std::slice::from_ref(&real_binding))
                .unwrap()
                .definitions()
                .is_empty()
        );
        let config = McpToolConfig::default();
        let result = config.bind(&[real_binding], true).await;
        assert!(matches!(result, Err(error) if error.contains("not configured")));
    }
}

#[cfg(test)]
mod http_tests;
