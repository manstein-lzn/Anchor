//! Host-owned, allowlisted tool bindings for the runner.
//!
//! The fixture server deliberately has no filesystem, process, network, or
//! credential capability. A Plugin identity must name its server and the host
//! must independently enable that Plugin before any tool is advertised.

use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use anchor_mcp_host::{McpHost, McpServerConfig, McpTransportConfig, Secret};
use anchor_runtime_rig::{
    NetworkPolicy, ReadOnlyInput, SandboxEnvironment, SandboxRequest, ToolError, ToolPort,
    graph::PluginBinding,
};
use anchor_sandbox_bwrap::BubblewrapSandbox;
use rig_agent::core::{
    completion::ToolDefinition,
    message::{ToolName, ToolResultContent},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

pub const FAKE_SERVER_ID: &str = "anchor.fake";
pub const FAKE_ECHO_TOOL: &str = "anchor_fake__echo";

#[derive(Debug, Clone, Default)]
pub struct McpToolConfig {
    servers: BTreeMap<String, ResolvedMcpServer>,
    // Declarations intentionally disabled by the existing optional environment
    // contract are different from missing host configuration.
    disabled: BTreeSet<String>,
    pub(crate) environment: crate::tool_environment::ToolEnvironment,
}

#[derive(Debug, Clone)]
struct ResolvedMcpServer {
    plugin_id: String,
    name: String,
    plugin_directory: PathBuf,
    config: Value,
}

impl McpToolConfig {
    /// Resolve the canonical Plugin MCP declarations used by the admitted
    /// Graph. The catalog owns parsing, path checks, and environment expansion;
    /// this adapter only turns those definitions into live host connections.
    pub fn from_catalog(
        root: impl Into<PathBuf>,
        bindings: &[PluginBinding],
    ) -> Result<Self, String> {
        let root = root.into();
        let catalog = FilePluginCatalog::new(root);
        let mut servers = BTreeMap::new();
        let mut disabled = BTreeSet::new();
        for binding in bindings {
            if binding
                .mcp_servers
                .iter()
                .all(|server| server == FAKE_SERVER_ID)
            {
                continue;
            }
            let definitions = catalog
                .mcp_servers(&binding.id, true)
                .map_err(|error| format!("Plugin `{}` MCP catalog failed: {error}", binding.id))?;
            let declared = catalog
                .mcp_servers(&binding.id, false)
                .map_err(|error| format!("Plugin `{}` MCP catalog failed: {error}", binding.id))?
                .into_iter()
                .map(|definition| definition.name)
                .collect::<BTreeSet<_>>();
            let mut by_name = definitions
                .into_iter()
                .map(|definition| (definition.name.clone(), definition))
                .collect::<BTreeMap<_, _>>();
            for server_name in &binding.mcp_servers {
                if server_name == FAKE_SERVER_ID {
                    continue;
                }
                let key = format!("{}-{server_name}", binding.id);
                let Some(definition) = by_name.remove(server_name) else {
                    if declared.contains(server_name) {
                        disabled.insert(key);
                        continue;
                    }
                    return Err(format!(
                        "Plugin `{}` MCP server `{server_name}` is not present in the catalog",
                        binding.id
                    ));
                };
                if servers
                    .insert(
                        key.clone(),
                        ResolvedMcpServer {
                            plugin_id: binding.id.clone(),
                            name: server_name.clone(),
                            plugin_directory: catalog
                                .plugin_directory(&binding.id)
                                .map_err(|error| error.to_string())?,
                            config: definition.config,
                        },
                    )
                    .is_some()
                {
                    return Err(format!("duplicate MCP server key `{key}`"));
                }
            }
        }
        Ok(Self {
            servers,
            disabled,
            environment: Default::default(),
        })
    }

    /// Resolve the same immutable Plugin directories as the catalog and map
    /// them into the node namespace. The fake fixture has no filesystem
    /// resources and therefore does not need a mount.
    pub fn plugin_mounts(
        root: impl Into<PathBuf>,
        bindings: &[PluginBinding],
    ) -> Result<Vec<ReadOnlyInput>, String> {
        let root = root.into();
        let catalog = FilePluginCatalog::new(root);
        let mut mounts = Vec::new();
        for binding in bindings {
            if binding.id == "fake-tools" && binding.resources.is_empty() {
                continue;
            }
            let current = catalog
                .resolve(std::slice::from_ref(&binding.id))
                .map_err(|error| {
                    format!("Plugin `{}` resource lookup failed: {error}", binding.id)
                })?
                .into_iter()
                .next()
                .ok_or_else(|| format!("Plugin `{}` is not installed", binding.id))?;
            if current != *binding {
                return Err(format!(
                    "Plugin `{}` changed since Run admission",
                    binding.id
                ));
            }
            mounts.push(ReadOnlyInput::new(
                catalog
                    .plugin_directory(&binding.id)
                    .map_err(|error| error.to_string())?,
                format!("/plugins/{}", binding.id),
            ));
        }
        Ok(mounts)
    }

    #[allow(dead_code)]
    pub async fn bind(
        &self,
        bindings: &[PluginBinding],
        network: bool,
    ) -> Result<LiveMcpTools, String> {
        self.bind_inner(bindings, network, None).await
    }

    /// Bind the same manifest MCP servers while giving stdio servers a
    /// command created inside the host Bubblewrap boundary.
    #[cfg(test)]
    pub async fn bind_with_sandbox(
        &self,
        bindings: &[PluginBinding],
        network: bool,
        sandbox: &BubblewrapSandbox,
        workspace: &Path,
    ) -> Result<LiveMcpTools, String> {
        self.bind_with_node_inputs(bindings, network, sandbox, workspace, &[])
            .await
    }

    async fn bind_with_node_inputs(
        &self,
        bindings: &[PluginBinding],
        network: bool,
        sandbox: &BubblewrapSandbox,
        workspace: &Path,
        readonly_inputs: &[ReadOnlyInput],
    ) -> Result<LiveMcpTools, String> {
        self.bind_inner(
            bindings,
            network,
            Some((sandbox, workspace, readonly_inputs)),
        )
        .await
    }

    async fn bind_inner(
        &self,
        bindings: &[PluginBinding],
        network: bool,
        sandbox: Option<(&BubblewrapSandbox, &Path, &[ReadOnlyInput])>,
    ) -> Result<LiveMcpTools, String> {
        self.validate_bindings(bindings, network)?;
        let mut hosts = BTreeMap::new();
        for binding in bindings {
            for server_id in &binding.mcp_servers {
                if server_id == FAKE_SERVER_ID {
                    continue;
                }
                let key = format!("{}-{server_id}", binding.id);
                if self.disabled.contains(&key) {
                    continue;
                }
                if hosts.contains_key(&key) {
                    continue;
                }
                let definition = self
                    .servers
                    .get(&key)
                    .ok_or_else(|| format!("MCP server `{key}` is not configured"))?;
                let host = self.connect_one(&key, definition, network, sandbox).await?;
                hosts.insert(key, Arc::new(host));
            }
        }
        let live = LiveMcpTools { hosts };
        live.validate_exposed_names()?;
        Ok(live)
    }

    pub fn validate_bindings(
        &self,
        bindings: &[PluginBinding],
        network: bool,
    ) -> Result<(), String> {
        let mut seen_servers = BTreeSet::new();
        for binding in bindings {
            for server_id in &binding.mcp_servers {
                if server_id == FAKE_SERVER_ID {
                    continue;
                }
                if !seen_servers.insert((binding.id.as_str(), server_id.as_str())) {
                    continue;
                }
                let key = format!("{}-{server_id}", binding.id);
                if self.disabled.contains(&key) {
                    continue;
                }
                let definition = self
                    .servers
                    .get(&key)
                    .ok_or_else(|| format!("MCP server `{key}` is not configured"))?;
                let transport = transport_name(&definition.config)?;
                if matches!(transport, "http" | "sse") && !network {
                    return Err("HTTP MCP requires node network=true".into());
                }
            }
        }
        Ok(())
    }

    async fn connect_one(
        &self,
        key: &str,
        definition: &ResolvedMcpServer,
        network: bool,
        sandbox: Option<(&BubblewrapSandbox, &Path, &[ReadOnlyInput])>,
    ) -> Result<McpHost, String> {
        let transport = transport_name(&definition.config)?;
        match transport {
            "http" => {
                let (endpoint, bearer_token, headers) = http_config(&definition.config)?;
                McpHost::connect(McpServerConfig {
                    server_id: key.into(),
                    transport: McpTransportConfig::StreamableHttp {
                        endpoint,
                        bearer_token,
                        headers,
                    },
                })
                .await
                .map_err(|error| format!("MCP server `{key}` failed to bind: {error}"))
            }
            "sse" => Err(format!(
                "MCP server `{key}` uses legacy SSE, which RMCP 2.2 does not provide"
            )),
            "stdio" => {
                let (sandbox, workspace, readonly_inputs) = sandbox.ok_or_else(|| {
                    format!("MCP server `{key}` requires the host sandbox launcher")
                })?;
                let (mut program, args, environment, working_directory) = stdio_config(
                    &definition.config,
                    &definition.plugin_directory,
                    &definition.plugin_id,
                )?;
                if let Some(command) = definition.config.get("command").and_then(Value::as_str)
                    && Path::new(command).is_absolute()
                    && !Path::new(command).starts_with(&definition.plugin_directory)
                {
                    // Standalone entrypoints use their granted mount; interpreters
                    // keep their explicit path so PATH cannot select another version.
                    program = self
                        .environment
                        .visible_entrypoint(Path::new(command))
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_else(|| command.to_owned());
                }
                let mut request = SandboxRequest::new(workspace.to_path_buf(), {
                    let mut argv = vec![program.clone()];
                    argv.extend(args.clone());
                    argv
                });
                request.working_directory = Some(working_directory.into());
                request.readonly_inputs.push(ReadOnlyInput::new(
                    &definition.plugin_directory,
                    format!("/plugins/{}", definition.plugin_id),
                ));
                for input in readonly_inputs {
                    if !request.readonly_inputs.contains(input) {
                        request.readonly_inputs.push(input.clone());
                    }
                }
                request.environment = environment;
                self.environment.apply(&mut request);
                request.network = if network {
                    NetworkPolicy::Enabled
                } else {
                    NetworkPolicy::Disabled
                };
                let command = sandbox.isolated_command(request).map_err(|error| {
                    format!("MCP server `{key}` sandbox launch rejected: {error}")
                })?;
                McpHost::connect_sandboxed_stdio(key.into(), command)
                    .await
                    .map_err(|error| format!("MCP server `{key}` failed to bind: {error}"))
            }
            other => Err(format!(
                "Plugin `{}` server `{}` has unsupported transport `{other}`",
                definition.plugin_id, definition.name
            )),
        }
    }
}

fn transport_name(config: &Value) -> Result<&str, String> {
    if config.get("command").is_some() {
        return Ok(config
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("stdio"));
    }
    Ok(config.get("type").and_then(Value::as_str).unwrap_or("http"))
}

type HttpConfig = (String, Option<Secret>, BTreeMap<String, String>);

fn http_config(config: &Value) -> Result<HttpConfig, String> {
    let endpoint = config
        .get("url")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "MCP HTTP server requires a non-empty url".to_owned())?
        .to_owned();
    let mut headers = BTreeMap::new();
    if let Some(values) = config.get("headers").and_then(Value::as_object) {
        for (name, value) in values {
            let value = value
                .as_str()
                .ok_or_else(|| format!("MCP HTTP header `{name}` must be a string"))?;
            headers.insert(name.clone(), value.to_owned());
        }
    }
    let authorization = headers
        .keys()
        .find(|name| name.eq_ignore_ascii_case("authorization"))
        .cloned();
    let bearer_token = authorization.and_then(|name| {
        headers.remove(&name).and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .map(|token| Secret::new(token.to_owned()))
        })
    });
    Ok((endpoint, bearer_token, headers))
}

fn stdio_config(
    config: &Value,
    plugin_directory: &Path,
    plugin_id: &str,
) -> Result<(String, Vec<String>, Vec<SandboxEnvironment>, String), String> {
    let command = config
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "MCP stdio server requires a non-empty command".to_owned())?;
    let program = visible_plugin_path(command, plugin_directory, plugin_id)
        .or_else(|| {
            let candidate = plugin_directory.join(command);
            candidate
                .is_file()
                .then(|| format!("/plugins/{plugin_id}/{}", command.replace('\\', "/")))
        })
        .unwrap_or_else(|| {
            Path::new(command)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(command)
                .to_owned()
        });
    let args = config
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(|value| {
                            visible_plugin_path(value, plugin_directory, plugin_id)
                                .or_else(|| {
                                    let candidate = plugin_directory.join(value);
                                    candidate.is_file().then(|| {
                                        format!("/plugins/{plugin_id}/{}", value.replace('\\', "/"))
                                    })
                                })
                                .unwrap_or_else(|| {
                                    visible_plugin_value(value, plugin_directory, plugin_id)
                                })
                        })
                        .ok_or_else(|| "MCP stdio args must be strings".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let environment = config
        .get("env")
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| {
                            SandboxEnvironment::new(
                                key,
                                visible_plugin_value(value, plugin_directory, plugin_id),
                            )
                        })
                        .ok_or_else(|| format!("MCP stdio environment `{key}` must be a string"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let working_directory = config
        .get("cwd")
        .and_then(Value::as_str)
        .map(Path::new)
        .unwrap_or(plugin_directory)
        .to_path_buf();
    let working_directory = if working_directory.is_absolute() {
        working_directory
    } else {
        plugin_directory.join(working_directory)
    };
    let working_directory = working_directory
        .canonicalize()
        .map_err(|error| format!("MCP working directory cannot be resolved: {error}"))?;
    if !working_directory.starts_with(plugin_directory) || !working_directory.is_dir() {
        return Err("MCP working directory must be inside its Plugin bundle".into());
    }
    let working_directory = visible_plugin_path(
        &working_directory.to_string_lossy(),
        plugin_directory,
        plugin_id,
    )
    .ok_or_else(|| "MCP working directory is not visible in the Plugin mount".to_owned())?;
    Ok((program, args, environment, working_directory))
}

fn visible_plugin_path(value: &str, plugin_directory: &Path, plugin_id: &str) -> Option<String> {
    let path = Path::new(value);
    let relative = path.strip_prefix(plugin_directory).ok()?;
    Some(
        Path::new("/plugins")
            .join(plugin_id)
            .join(relative)
            .to_string_lossy()
            .into_owned(),
    )
}

fn visible_plugin_value(value: &str, plugin_directory: &Path, plugin_id: &str) -> String {
    let directory = plugin_directory.to_string_lossy();
    value.replace(directory.as_ref(), &format!("/plugins/{plugin_id}"))
}

pub struct LiveMcpTools {
    hosts: BTreeMap<String, Arc<McpHost>>,
}

impl ToolPort for LiveMcpTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.hosts
            .iter()
            .flat_map(|(server_id, host)| {
                host.tools().into_iter().filter_map(|tool| {
                    ToolName::new(exposed_tool_name(server_id, &tool.name))
                        .ok()
                        .map(|name| {
                            ToolDefinition::new(
                                name,
                                tool.description.unwrap_or_default(),
                                tool.input_schema,
                            )
                        })
                })
            })
            .collect()
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let Some((host, remote_name)) = self.resolve(name) else {
                return Err(ToolError::Unknown(name.to_owned()));
            };
            host.call_rig(&remote_name, arguments)
                .await
                .map_err(|error| ToolError::Failed(error.to_string()))
        })
    }
}

impl LiveMcpTools {
    fn validate_exposed_names(&self) -> Result<(), String> {
        let mut names = BTreeSet::new();
        for (server_id, host) in &self.hosts {
            for tool in host.tools() {
                let exposed = exposed_tool_name(server_id, &tool.name);
                if exposed == FAKE_ECHO_TOOL
                    || exposed == crate::node_tools::RUN_TOOL_NAME
                    || exposed == crate::channel_tools::SEND_TOOL
                    || exposed == crate::channel_tools::IMAGE_TOOL
                {
                    return Err(format!(
                        "MCP tool `{exposed}` is reserved by the Anchor node host"
                    ));
                }
                if !names.insert(exposed.clone()) {
                    return Err(format!(
                        "MCP tool name collision after server prefix: `{exposed}`"
                    ));
                }
            }
        }
        Ok(())
    }

    fn resolve(&self, name: &str) -> Option<(Arc<McpHost>, String)> {
        self.hosts.iter().find_map(|(server_id, host)| {
            host.tools().into_iter().find_map(|tool| {
                (exposed_tool_name(server_id, &tool.name) == name)
                    .then(|| (Arc::clone(host), tool.name))
            })
        })
    }
}

/// Python's MCPToolset.prefixed(name) exposes one model tool as
/// `<server>_<tool>`. Keeping the mapping explicit avoids a second
/// search/call protocol and leaves the remote input schema unchanged.
fn exposed_tool_name(server_id: &str, tool_name: &str) -> String {
    format!("{server_id}_{tool_name}")
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
        if name == FAKE_ECHO_TOOL {
            self.fake.is_read_only(name)
        } else {
            self.live.is_read_only(name)
        }
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
    #[allow(dead_code)]
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

    /// Assemble Plugin tools for a node whose stdio MCP servers must be
    /// launched through the host's Bubblewrap sandbox.
    pub async fn assemble_with_sandbox(
        &self,
        mcp: &McpToolConfig,
        bindings: &[PluginBinding],
        network: bool,
        sandbox: &BubblewrapSandbox,
        workspace: &Path,
        readonly_inputs: &[ReadOnlyInput],
    ) -> Result<Arc<dyn ToolPort>, String> {
        let fake = self
            .for_bindings(bindings)
            .map_err(|error| error.to_string())?;
        let live = mcp
            .bind_with_node_inputs(bindings, network, sandbox, workspace, readonly_inputs)
            .await?;
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

    #[test]
    fn catalog_uses_plugin_server_keys_and_does_not_require_tool_allowlists() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/research");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"Research","mcpServers":{"papers":{"type":"http","url":"http://127.0.0.1:9/mcp"}}}"#,
        )
        .unwrap();
        let binding = PluginBinding {
            id: "research".into(),
            digest: String::new(),
            resources: vec!["plugin.json".into()],
            mcp_servers: vec!["papers".into()],
        };
        let config = McpToolConfig::from_catalog(root.path(), &[binding]).unwrap();
        assert!(config.servers.contains_key("research-papers"));
        assert_eq!(
            config.servers["research-papers"].config["url"],
            "http://127.0.0.1:9/mcp"
        );
    }

    #[test]
    fn fake_fixture_binding_does_not_require_a_filesystem_catalog() {
        let binding = PluginBinding {
            id: "fake-tools".into(),
            digest: "anchor.fake.echo.v1".into(),
            resources: Vec::new(),
            mcp_servers: vec![FAKE_SERVER_ID.into()],
        };
        let config = McpToolConfig::from_catalog("/does/not/exist", &[binding]).unwrap();
        assert!(config.servers.is_empty());
    }

    #[tokio::test]
    async fn optional_unconfigured_mcp_is_absent_without_disabling_the_plugin() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/optional");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.json"),
            r#"{"name":"Optional","mcpServers":{"api":{"command":"python3","args":["server.py"],"optional_env_vars":["ANCHOR_TEST_UNCONFIGURED_OPTIONAL_MCP_7F90"]}}}"#).unwrap();
        let binding = PluginBinding {
            id: "optional".into(),
            digest: String::new(),
            resources: vec!["plugin.json".into()],
            mcp_servers: vec!["api".into()],
        };
        let config =
            McpToolConfig::from_catalog(root.path(), std::slice::from_ref(&binding)).unwrap();
        assert!(config.servers.is_empty());
        assert!(config.disabled.contains("optional-api"));
        assert!(
            config
                .bind(std::slice::from_ref(&binding), false)
                .await
                .unwrap()
                .definitions()
                .is_empty()
        );
        assert!(
            McpToolConfig::from_catalog(
                root.path(),
                &[PluginBinding {
                    mcp_servers: vec!["not-declared".into()],
                    ..binding
                }]
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn stdio_mcp_receives_only_its_nodes_readonly_grants() {
        use anchor_sandbox_bwrap::BubblewrapPolicy;
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/reader");
        let workspace = root.path().join("workspace");
        let other_workspace = root.path().join("other-workspace");
        let private = root.path().join("private");
        for path in [&plugin, &workspace, &other_workspace, &private] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(private.join("history.txt"), "mcp-local-evidence").unwrap();
        std::fs::write(plugin.join("server.py"), r#"import json, os, sys
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"reader","version":"1"}}
    elif method == "tools/list":
        result = {"tools":[{"name":"inspect","description":"inspect granted input","inputSchema":{"type":"object","properties":{}}}]}
    elif method == "tools/call":
        path = "/local-inputs/history/history.txt"
        value = {"visible": os.path.exists(path)}
        if value["visible"]:
            value["text"] = open(path).read()
            try:
                open("/local-inputs/history/forbidden", "w").write("changed")
                value["readonly"] = False
            except OSError:
                value["readonly"] = True
        result = {"content":[{"type":"text","text":json.dumps(value)}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}), flush=True)
"#).unwrap();
        std::fs::write(plugin.join("plugin.json"), json!({
            "name":"Reader", "mcpServers":{"stdio":{"command":"/usr/bin/python3","args":["server.py"]}}
        }).to_string()).unwrap();
        let binding = FilePluginCatalog::new(root.path())
            .resolve(&["reader".into()])
            .unwrap()
            .remove(0);
        let config =
            McpToolConfig::from_catalog(root.path(), std::slice::from_ref(&binding)).unwrap();
        let sandbox = BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["python3"])
                .authorize_workspace_root(&workspace)
                .authorize_workspace_root(&other_workspace)
                .authorize_readonly_input_root(&plugin)
                .authorize_readonly_destination_root("/plugins"),
        )
        .unwrap();
        let grants = [ReadOnlyInput::new(&private, "/local-inputs/history")];
        let node_sandbox = sandbox.with_readonly_grants(&grants).unwrap();
        let tools = config
            .bind_with_node_inputs(
                std::slice::from_ref(&binding),
                false,
                &node_sandbox,
                &workspace,
                &grants,
            )
            .await
            .unwrap();
        let result = tools.call("reader-stdio_inspect", json!({})).await.unwrap();
        let value: Value = serde_json::from_str(result[0].as_text().unwrap()).unwrap();
        assert_eq!(
            value,
            json!({"visible":true,"text":"mcp-local-evidence","readonly":true})
        );
        assert!(!private.join("forbidden").exists());
        let tools = config
            .bind_with_sandbox(&[binding], false, &sandbox, &other_workspace)
            .await
            .unwrap();
        let result = tools.call("reader-stdio_inspect", json!({})).await.unwrap();
        let value: Value = serde_json::from_str(result[0].as_text().unwrap()).unwrap();
        assert_eq!(value, json!({"visible":false}));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_mcp_runs_inside_the_plugin_mount_and_exposes_direct_tools() {
        use anchor_sandbox_bwrap::BubblewrapPolicy;
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/research");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("server.py"),
            r#"import json, os, sys
from installed_dependency import prefix
if os.getcwd() != "/plugins/research/nested":
    raise SystemExit(f"wrong cwd: {os.getcwd()}")
for line in sys.stdin:
    open("/workspace/mcp-log", "a").write("line\\n")
    request = json.loads(line)
    method = request.get("method")
    if "id" not in request:
        continue
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
    elif method == "tools/list":
        result = {"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}}}}]}
    elif method == "tools/call":
        result = {"content":[{"type":"text","text":prefix + request["params"]["arguments"]["value"]}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}), flush=True)
    open("/workspace/mcp-log", "a").write(method + "\\n")
"#,
        )
        .unwrap();
        std::fs::create_dir(plugin.join("nested")).unwrap();
        let mut permissions = std::fs::metadata(plugin.join("server.py"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(plugin.join("server.py"), permissions).unwrap();
        let environment = root.path().join("external-env");
        let tool = root.path().join("tools/python");
        let imports = root.path().join("external-modules");
        // Another installation sorts first on PATH; it must not replace the
        // explicitly requested interpreter of this MCP server.
        let decoy = root.path().join("decoy-env");
        let decoy_tool = root.path().join("tools/a-decoy");
        std::fs::create_dir_all(decoy.join("bin")).unwrap();
        std::fs::create_dir_all(&decoy_tool).unwrap();
        std::fs::write(decoy.join("bin/python"), "#!/bin/sh\nexit 88\n").unwrap();
        std::fs::set_permissions(
            decoy.join("bin/python"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("python", decoy.join("bin/python3")).unwrap();
        std::fs::write(
            decoy_tool.join("tool.json"),
            json!({
                "entrypoint":decoy.join("bin/python"), "environment":decoy
            })
            .to_string(),
        )
        .unwrap();
        std::fs::create_dir_all(environment.join("bin")).unwrap();
        std::fs::create_dir_all(&tool).unwrap();
        std::fs::create_dir(&imports).unwrap();
        std::os::unix::fs::symlink("/usr/bin/python3", environment.join("bin/python")).unwrap();
        std::fs::write(
            imports.join("installed_dependency.py"),
            "prefix = 'dependency:'",
        )
        .unwrap();
        std::fs::write(tool.join("tool.json"), json!({
            "entrypoint":environment.join("bin/python"), "environment":environment, "imports":[imports]
        }).to_string()).unwrap();
        let standalone = root.path().join("standalone-launcher");
        let standalone_tool = root.path().join("tools/standalone");
        std::fs::create_dir(&standalone_tool).unwrap();
        std::fs::write(&standalone, "#!/bin/sh\nexec /usr/bin/python3 \"$@\"\n").unwrap();
        std::fs::set_permissions(&standalone, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            standalone_tool.join("tool.json"),
            json!({"entrypoint":standalone}).to_string(),
        )
        .unwrap();
        std::fs::write(plugin.join("plugin.json"), json!({
            "name":"Research", "mcpServers":{
                "stdio":{
                    "command": environment.join("bin/python"), "args":["../server.py"], "cwd":"nested"
                },
                "system":{
                    "command":"/usr/bin/python3", "args":["../server.py"], "cwd":"nested"
                },
                "standalone":{
                    "command":standalone, "args":["../server.py"], "cwd":"nested"
                }
            }
        }).to_string()).unwrap();
        let binding = FilePluginCatalog::new(root.path())
            .resolve(&["research".into()])
            .unwrap()
            .remove(0);
        let mut config =
            McpToolConfig::from_catalog(root.path(), std::slice::from_ref(&binding)).unwrap();
        config.environment = crate::tool_environment::ToolEnvironment::load(root.path()).unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sandbox = BubblewrapSandbox::new(
            config.environment.authorize(
                BubblewrapPolicy::new("bwrap", ["python3"])
                    .authorize_workspace_root(&workspace)
                    .authorize_readonly_input_root(root.path())
                    .authorize_readonly_destination_root("/plugins"),
            ),
        )
        .unwrap();
        let tools = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            config.bind_with_sandbox(std::slice::from_ref(&binding), false, &sandbox, &workspace),
        )
        .await
        {
            Ok(result) => {
                result.unwrap_or_else(|error| panic!("stdio MCP handshake failed: {error}"))
            }
            Err(_) => panic!(
                "stdio MCP handshake timed out; child log: {:?}",
                std::fs::read_to_string(workspace.join("mcp-log"))
            ),
        };
        for name in [
            "research-stdio_echo",
            "research-system_echo",
            "research-standalone_echo",
        ] {
            assert!(
                tools
                    .definitions()
                    .iter()
                    .any(|definition| definition.name == name)
            );
            let result = tools.call(name, json!({"value":"hello"})).await.unwrap();
            assert_eq!(result[0].as_text(), Some("dependency:hello"), "{name}");
        }
        let ungranted = root.path().join("python3");
        std::fs::copy(&standalone, &ungranted).unwrap();
        config
            .servers
            .get_mut("research-standalone")
            .unwrap()
            .config["command"] = json!(ungranted);
        assert!(matches!(
            config.bind_with_sandbox(&[binding], false, &sandbox, &workspace).await,
            Err(error) if error.contains("sandbox launch rejected") && error.contains("command must select")
        ));
    }
}

#[cfg(test)]
mod http_tests;
