//! Host-owned, allowlisted tool bindings for the runner.
//!
//! The fixture server deliberately has no filesystem, process, network, or
//! credential capability. A Plugin identity must name its server and the host
//! must independently enable that Plugin before any tool is advertised.

use crate::plugin_library::Catalog;
use anchor_graph_host::PluginDefinition;
use anchor_library::{
    FileOAuthTokenStore, OAuthBinding, OAuthClient, OAuthRefreshRequest, OAuthRefreshTransport,
    OAuthTokenResponse, OAuthTransportError,
};
use anchor_mcp_host::{McpHost, McpHostError, McpServerConfig, McpTransportConfig, Secret};
use anchor_runtime::{
    NetworkPolicy, ReadOnlyInput, SandboxEnvironment, SandboxError, SandboxRequest, ToolDefinition,
    ToolError, ToolName, ToolPort, ToolResultContent, graph::PluginBinding,
};
use anchor_sandbox_bwrap::BubblewrapSandbox;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io::Read,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

mod preparation_error;
pub(crate) use preparation_error::PreparationError;

pub const FAKE_SERVER_ID: &str = "anchor.fake";
pub const FAKE_ECHO_TOOL: &str = "anchor_fake__echo";

fn fake_binding() -> PluginBinding {
    PluginBinding {
        id: "fake-tools".into(),
        digest: "anchor.fake.echo.v1".into(),
        resources: Vec::new(),
        mcp_servers: vec![FAKE_SERVER_ID.into()],
        authorization_digest: "anchor.fake.echo.authorization.v1"
            .try_into()
            .expect("non-empty fixture authority"),
    }
}

pub(crate) fn is_fake_fixture(binding: &PluginBinding) -> bool {
    let fixture = fake_binding();
    binding == &fixture
}

#[derive(Debug, Clone, Default)]
pub struct McpToolConfig {
    servers: BTreeMap<String, ResolvedMcpServer>,
    // Declarations intentionally disabled by the existing optional environment
    // contract are different from missing host configuration.
    disabled: BTreeSet<String>,
    pub(crate) environment: crate::tool_environment::ToolEnvironment,
    oauth_library_root: Option<PathBuf>,
    pub(crate) startup_cancellation: Option<anchor_runtime::Cancellation>,
    pub(crate) startup_timeout: Option<std::time::Duration>,
    /// Non-secret observation of the configuration captured for this invocation.
    pub(crate) configuration_revisions: BTreeMap<String, u64>,
    /// Stable owner/target/source identities; excludes rotating secret values.
    pub(crate) configuration_bindings: BTreeMap<String, String>,
}

/// Everything one node invocation provides to a sandboxed MCP launch.
pub struct SandboxLaunch<'a> {
    pub sandbox: &'a BubblewrapSandbox,
    pub workspace: &'a Path,
    pub readonly_inputs: &'a [ReadOnlyInput],
    /// Prepared dependency views, keyed by Plugin. A declaration is the Plugin's
    /// own, so every server of that Plugin launches on the same view.
    pub plugin_views: &'a BTreeMap<String, crate::plugin_view::PreparedPlugin>,
}

/// The read-only authority one Plugin server launches with: the view its Plugin
/// declares. Wrapping it keeps "mount it and put it in the request" one step.
#[derive(Clone, Copy)]
enum ServerGrant<'a> {
    View(&'a crate::plugin_view::PreparedPlugin),
}

impl<'a> ServerGrant<'a> {
    /// The dependency view this server launches on, when it has one.
    fn view(&self) -> Option<&'a Path> {
        match self {
            Self::View(view) => view.view(),
        }
    }

    fn authorize(&self, sandbox: Arc<BubblewrapSandbox>) -> Result<Arc<BubblewrapSandbox>, String> {
        match self {
            Self::View(view) => view.authorize(sandbox),
        }
    }

    fn apply(&self, request: &mut SandboxRequest) {
        match self {
            Self::View(view) => view.apply(request),
        }
    }
}

/// The launcher's key for one Plugin server.
///
/// This string is also the prefix of every model-visible tool name
/// (`{key}_{tool}`), so it must stay within the tool-name charset. That means
/// the separator cannot be a character no provider accepts, and the key is
/// therefore *not* injective on its own: (plugin `a-b`, server `c`) and
/// (plugin `a`, server `b-c`) share one. Callers detect that collision and name
/// both pairs instead of silently launching one server twice.
fn server_key(plugin: &str, server: &str) -> String {
    format!("{plugin}-{server}")
}

#[derive(Clone)]
struct ResolvedMcpServer {
    plugin_id: String,
    name: String,
    plugin_directory: PathBuf,
    // The validated declaration identifies path templates before credentials
    // are injected; resolved credential bytes must never become templates.
    declaration: Value,
    config: Value,
}

impl std::fmt::Debug for ResolvedMcpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedMcpServer")
            .field("plugin_id", &self.plugin_id)
            .field("name", &self.name)
            .field("plugin_directory", &self.plugin_directory)
            .field(
                "config_keys",
                &self
                    .config
                    .as_object()
                    .map(|config| config.keys().collect::<Vec<_>>()),
            )
            .finish()
    }
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
        let catalog = Catalog::new(root.clone());
        let mut definitions = BTreeMap::new();
        // This is preliminary wiring validation. Its caller owns any admission
        // publication lease; invocation preparation uses `from_definitions`
        // under the shared Host preparation lease.
        for binding in bindings.iter().filter(|binding| !is_fake_fixture(binding)) {
            let definition = catalog
                .definition(&binding.id)
                .map_err(|error| format!("Plugin `{}` MCP catalog failed: {error}", binding.id))?;
            definitions.insert(binding.id.clone(), definition);
        }
        Self::from_definitions(&root, bindings, &definitions)
    }

    /// Turn one lease-protected catalog read into live connection declarations.
    /// This method never reloads a Plugin manifest or re-resolves its directory.
    pub(crate) fn from_definitions(
        root: &Path,
        bindings: &[PluginBinding],
        definitions: &BTreeMap<String, PluginDefinition>,
    ) -> Result<Self, String> {
        Self::from_definitions_for_owner(root, bindings, definitions, None)
    }

    pub(crate) fn from_definitions_for_owner(
        root: &Path,
        bindings: &[PluginBinding],
        definitions: &BTreeMap<String, PluginDefinition>,
        credentials: Option<(&Path, &str)>,
    ) -> Result<Self, String> {
        Self::from_definitions_for_owner_execution(root, bindings, definitions, credentials)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn from_definitions_for_owner_execution(
        root: &Path,
        bindings: &[PluginBinding],
        definitions: &BTreeMap<String, PluginDefinition>,
        credentials: Option<(&Path, &str)>,
    ) -> Result<Self, PreparationError> {
        let catalog = Catalog::new(root);
        let mut servers: BTreeMap<String, ResolvedMcpServer> = BTreeMap::new();
        let mut disabled = BTreeSet::new();
        let mut configuration_revisions = BTreeMap::new();
        let mut configuration_bindings = BTreeMap::new();
        for binding in bindings {
            if is_fake_fixture(binding) {
                continue;
            }
            let declaration = definitions
                .get(&binding.id)
                .ok_or_else(|| format!("Plugin `{}` MCP definition is unavailable", binding.id))?;
            if !crate::plugin_library::ExpectedDefinition::Authorization(binding)
                .matches(declaration)
            {
                return Err(PreparationError::KnownFailure(format!(
                    "Plugin `{}` Host authorization changed before MCP assembly",
                    binding.id
                )));
            }
            let definitions = if let Some((state, owner)) = credentials {
                // Resolve all of one Plugin's consumers under the same lease so
                // a rotation cannot split its remote and local MCP identities.
                let snapshot = crate::credential_store::environment_snapshot_for_execution(
                    state,
                    owner,
                    declaration,
                )?;
                configuration_revisions.insert(binding.id.clone(), snapshot.revision);
                configuration_bindings.insert(binding.id.clone(), snapshot.binding_digest);
                let mut resolved = Vec::new();
                for server in &declaration.mcp_servers {
                    let values = snapshot.environments.get(&server.name);
                    let mut single = declaration.clone();
                    single.mcp_servers = vec![server.clone()];
                    let environment =
                        |key: &str| values.and_then(|values| values.get(key)).cloned();
                    resolved.extend(
                        catalog
                            .resolve_mcp_servers_with(&single, &environment)
                            .map_err(|error| {
                                format!("Plugin `{}` MCP catalog failed: {error}", binding.id)
                            })?,
                    );
                }
                resolved
            } else {
                catalog.resolve_mcp_servers(declaration).map_err(|error| {
                    format!("Plugin `{}` MCP catalog failed: {error}", binding.id)
                })?
            };
            let declared = declaration
                .mcp_servers
                .iter()
                .map(|definition| definition.name.clone())
                .collect::<BTreeSet<_>>();
            let mut by_name = definitions
                .into_iter()
                .map(|definition| (definition.name.clone(), definition))
                .collect::<BTreeMap<_, _>>();
            for server_name in &binding.mcp_servers {
                if server_name == FAKE_SERVER_ID {
                    continue;
                }
                let key = server_key(&binding.id, server_name);
                let Some(definition) = by_name.remove(server_name) else {
                    if declared.contains(server_name) {
                        disabled.insert(key);
                        continue;
                    }
                    return Err(format!(
                        "Plugin `{}` MCP server `{server_name}` is not present in the catalog",
                        binding.id
                    )
                    .into());
                };
                // Two different servers may share the key; that would launch one
                // of them twice and hide the other, so it is refused by naming
                // both pairs rather than reporting a bare duplicate.
                if let Some(existing) = servers.get(&key) {
                    return Err(format!(
                        "Plugin servers `{}/{}` and `{}/{}` share the launcher key `{key}`; rename one",
                        existing.plugin_id, existing.name, binding.id, server_name
                    ).into());
                }
                servers.insert(
                    key.clone(),
                    ResolvedMcpServer {
                        plugin_id: binding.id.clone(),
                        name: server_name.clone(),
                        plugin_directory: declaration.directory.clone(),
                        declaration: declaration
                            .mcp_servers
                            .iter()
                            .find(|server| &server.name == server_name)
                            .ok_or_else(|| {
                                format!("Plugin `{}` MCP declaration is unavailable", binding.id)
                            })?
                            .config
                            .clone(),
                        config: definition.config,
                    },
                );
            }
        }
        Ok(Self {
            servers,
            disabled,
            environment: Default::default(),
            oauth_library_root: Some(root.to_path_buf()),
            startup_cancellation: None,
            startup_timeout: None,
            configuration_revisions,
            configuration_bindings,
        })
    }

    pub(crate) fn declares_stdio(&self, binding: &PluginBinding) -> bool {
        self.servers.values().any(|server| {
            server.plugin_id == binding.id
                && transport_name(&server.config).is_ok_and(|transport| transport == "stdio")
        })
    }

    pub(crate) async fn authorize_oauth(
        &mut self,
        owner: Option<String>,
        network_allowed: bool,
    ) -> Result<(), String> {
        let oauth_servers = self
            .servers
            .iter()
            .filter(|(_, server)| is_oauth_config(&server.config))
            .map(|(key, server)| (key.clone(), server.clone()))
            .collect::<Vec<_>>();
        if oauth_servers.is_empty() {
            return Ok(());
        }
        if !network_allowed {
            return Err("HTTP MCP requires node network=true".into());
        }
        let owner = owner.ok_or_else(|| {
            "OAuth Plugin access requires an owner-bound Run authorization".to_owned()
        })?;
        let library_root = self
            .oauth_library_root
            .clone()
            .ok_or_else(|| "OAuth Plugin Library root is unavailable".to_owned())?;
        for (key, server) in oauth_servers {
            let mut server = server;
            let resource = server
                .config
                .get("oauth_resource")
                .and_then(Value::as_str)
                .or_else(|| server.config.get("url").and_then(Value::as_str))
                .ok_or_else(|| format!("OAuth MCP server `{key}` requires a resource URL"))?;
            let provider = format!("{:x}", Sha256::digest(resource.as_bytes()));
            let binding = OAuthBinding::new(provider, &server.plugin_id, &server.name, &owner)
                .map_err(|_| format!("OAuth MCP server `{key}` has an invalid binding"))?;
            let store = FileOAuthTokenStore::new(&library_root);
            let token = tokio::task::spawn_blocking(move || {
                OAuthClient::new(store, ReqwestOAuthRefreshTransport)
                    .access_token(&binding)
                    .map_err(|failure| format!("OAuth MCP server authorization failed: {failure}"))
            })
            .await
            .map_err(|_| format!("OAuth MCP server `{key}` authorization worker failed"))??;
            let headers = server
                .config
                .get_mut("headers")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| format!("OAuth MCP server `{key}` headers are invalid"))?;
            headers.insert(
                "Authorization".into(),
                Value::String(format!("Bearer {}", token.expose_secret())),
            );
            self.servers.insert(key, server);
        }
        Ok(())
    }

    #[cfg(test)]
    pub async fn bind(
        &self,
        bindings: &[PluginBinding],
        network: bool,
    ) -> Result<LiveMcpTools, String> {
        self.bind_inner(bindings, network, None)
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn bind_with_node_inputs(
        &self,
        bindings: &[PluginBinding],
        network: bool,
        launch: SandboxLaunch<'_>,
    ) -> Result<LiveMcpTools, String> {
        self.bind_inner(bindings, network, Some(launch))
            .await
            .map_err(|error| error.to_string())
    }

    /// Probe through exactly the same transport and sandbox adapter as a node.
    /// A protocol success proves initialization/tool discovery, not a business
    /// operation or (for local servers) the validity of their upstream secrets.
    pub(crate) async fn test_connections(
        &self,
        bindings: &[PluginBinding],
        launch: SandboxLaunch<'_>,
    ) -> Vec<Value> {
        let mut results = Vec::new();
        for binding in bindings {
            for target in &binding.mcp_servers {
                let key = server_key(&binding.id, target);
                if self.servers.get(&key).is_some_and(|server| {
                    transport_name(&server.config).is_ok_and(|value| value == "sse")
                }) {
                    results.push(json!({"target":target,"status":"unsupported",
                        "message":"当前运行时不支持旧版 SSE MCP，请使用 Streamable HTTP。"}));
                    continue;
                }
                let mut single = binding.clone();
                single.mcp_servers = vec![target.clone()];
                let connection = self
                    .bind_with_node_inputs(
                        &[single],
                        true,
                        SandboxLaunch {
                            sandbox: launch.sandbox,
                            workspace: launch.workspace,
                            readonly_inputs: launch.readonly_inputs,
                            plugin_views: launch.plugin_views,
                        },
                    )
                    .await;
                let result = match connection {
                    Ok(live) if live.hosts.is_empty() => {
                        let _ = live.shutdown().await;
                        json!({"target":target,"status":"unsupported",
                            "message":"该 MCP 目标当前被配置为禁用，未建立连接。"})
                    }
                    Ok(live) => {
                        let count: usize = live.hosts.values().map(|host| host.tools().len()).sum();
                        if live.shutdown().await.is_ok() {
                            json!({"target":target,"status":"verified","tool_count":count,
                                "message":"已完成 MCP 初始化与工具发现；未调用业务工具。"})
                        } else {
                            json!({"target":target,"status":"failed",
                                "message":"工具发现已完成，但连接清理失败，请检查服务状态。"})
                        }
                    }
                    Err(_) => json!({"target":target,"status":"failed",
                        "message":"连接或工具发现失败，请检查服务地址、凭据与服务状态。"}),
                };
                results.push(result);
            }
        }
        results
    }

    async fn bind_inner(
        &self,
        bindings: &[PluginBinding],
        network: bool,
        launch: Option<SandboxLaunch<'_>>,
    ) -> Result<LiveMcpTools, PreparationError> {
        let sandbox = launch.as_ref().map(|launch| launch.sandbox);
        self.validate_bindings(bindings, network)?;
        let mut hosts = BTreeMap::new();
        let mut owners = BTreeMap::new();
        for binding in bindings {
            for server_id in &binding.mcp_servers {
                let key = server_key(&binding.id, server_id);
                // A server this host will not launch is skipped.
                let skipped = server_id == FAKE_SERVER_ID || self.disabled.contains(&key);
                if skipped {
                    continue;
                }
                if hosts.contains_key(&key) {
                    continue;
                }
                let connected = async {
                    let definition = self
                        .servers
                        .get(&key)
                        .ok_or_else(|| format!("MCP server `{key}` is not configured"))?;
                    // A server launches in a sandbox that authorizes exactly its
                    // Plugin's view; a Plugin without a declaration keeps the host's
                    // authority.
                    let grant = launch
                        .as_ref()
                        .and_then(|launch| launch.plugin_views.get(&binding.id))
                        .map(ServerGrant::View);
                    // The launcher mounts the Plugin directory for this server, so
                    // the invocation must grant exactly that mount. Nothing else
                    // authorizes it: the Run policy no longer lists per-Run trees.
                    let plugin_mount = ReadOnlyInput::new(
                        &definition.plugin_directory,
                        format!("/plugins/{}", definition.plugin_id),
                    );
                    let authorized = match sandbox {
                        Some(sandbox) => {
                            let base = Arc::new(sandbox.clone());
                            let base = base
                                .with_readonly_grants(std::slice::from_ref(&plugin_mount))
                                .map_err(|error| error.to_string())?;
                            match grant {
                                Some(grant) => Some(grant.authorize(Arc::new(base))?),
                                None => Some(Arc::new(base)),
                            }
                        }
                        None => None,
                    };
                    let server_launch = launch.as_ref().map(|launch| SandboxLaunch {
                        sandbox: authorized.as_deref().unwrap_or(launch.sandbox),
                        workspace: launch.workspace,
                        readonly_inputs: launch.readonly_inputs,
                        plugin_views: launch.plugin_views,
                    });
                    self.connect_one(&key, definition, network, server_launch, grant)
                        .await
                }
                .await;
                let host = match connected {
                    Ok(host) => host,
                    Err(reason) => {
                        let cleanup = LiveMcpTools {
                            hosts,
                            owners,
                            _environment: self.environment.clone(),
                        }
                        .shutdown()
                        .await
                        .err();
                        return Err(reason.cleanup(cleanup));
                    }
                };
                let declaration = self
                    .servers
                    .get(&key)
                    .expect("connected server declaration");
                for tool in host.tools() {
                    owners.insert(
                        exposed_tool_name(&key, &tool.name),
                        crate::plugin_disclosure::ToolOwner {
                            plugin: declaration.plugin_id.clone(),
                            server: declaration.name.clone(),
                            tool: tool.name,
                        },
                    );
                }
                hosts.insert(key, Arc::new(host));
            }
        }
        let live = LiveMcpTools {
            hosts,
            owners,
            _environment: self.environment.clone(),
        };
        if self
            .startup_cancellation
            .as_ref()
            .is_some_and(|cancellation| cancellation.load(std::sync::atomic::Ordering::Acquire))
        {
            let reason = "MCP tool startup was cancelled".to_owned();
            let cleanup = live.shutdown().await.err();
            return Err(PreparationError::Unavailable(reason).cleanup(cleanup));
        }
        if let Err(reason) = live.validate_exposed_names() {
            let cleanup = live.shutdown().await.err();
            return Err(PreparationError::Unavailable(reason).cleanup(cleanup));
        }
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
                    if !is_fake_fixture(binding) {
                        return Err(format!(
                            "Plugin `{}` has an invalid fake fixture binding",
                            binding.id
                        ));
                    }
                    continue;
                }
                if !seen_servers.insert((binding.id.as_str(), server_id.as_str())) {
                    continue;
                }
                let key = server_key(&binding.id, server_id);
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
        sandbox: Option<SandboxLaunch<'_>>,
        grant: Option<ServerGrant<'_>>,
    ) -> Result<McpHost, PreparationError> {
        let timeout = self
            .startup_timeout
            .unwrap_or(std::time::Duration::from_secs(120));
        let transport = transport_name(&definition.config)?;
        match transport {
            "http" => {
                let (endpoint, bearer_token, headers) = http_config(&definition.config)?;
                let connect = McpHost::connect(McpServerConfig {
                    server_id: key.into(),
                    transport: McpTransportConfig::StreamableHttp {
                        endpoint,
                        bearer_token,
                        headers,
                    },
                });
                let result = if let Some(cancellation) = &self.startup_cancellation {
                    tokio::select! {
                        result = tokio::time::timeout(timeout, connect) => result
                            .map_err(|_| format!("MCP server `{key}` startup timed out"))?,
                        _ = async {
                            while !cancellation.load(std::sync::atomic::Ordering::Acquire) {
                                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            }
                        } => return Err(format!("MCP server `{key}` startup was cancelled").into()),
                    }
                } else {
                    tokio::time::timeout(timeout, connect)
                        .await
                        .map_err(|_| format!("MCP server `{key}` startup timed out"))?
                };
                result.map_err(|error| {
                    PreparationError::Unavailable(format!(
                        "MCP server `{key}` failed to bind: {error}"
                    ))
                })
            }
            "sse" => Err(format!(
                "MCP server `{key}` uses legacy SSE, which RMCP 2.2 does not provide"
            )
            .into()),
            "stdio" => {
                let tool_environment = self.environment.for_plugin(
                    &definition.plugin_id,
                    grant.and_then(|grant| grant.view()).is_none(),
                );
                let SandboxLaunch {
                    sandbox,
                    workspace,
                    readonly_inputs,
                    ..
                } = sandbox.ok_or_else(|| {
                    format!("MCP server `{key}` requires the host sandbox launcher")
                })?;
                let config = named_view(
                    &definition.config,
                    &definition.declaration,
                    grant.and_then(|grant| grant.view()),
                    &definition.plugin_id,
                )?;
                let (mut program, args, environment, working_directory) = stdio_config(
                    &config,
                    &definition.declaration,
                    &definition.plugin_directory,
                    &definition.plugin_id,
                )?;
                if grant.and_then(|grant| grant.view()).is_none()
                    && let Some(command) = definition.config.get("command").and_then(Value::as_str)
                    && Path::new(command).is_absolute()
                    && !Path::new(command).starts_with(&definition.plugin_directory)
                {
                    // Standalone entrypoints use their granted mount; interpreters
                    // keep their explicit path so PATH cannot select another version.
                    program = tool_environment
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
                // A server with a grant runs in exactly that environment or view
                // and must not also inherit the operator's global tool aggregate;
                // every other server keeps today's behaviour.
                match grant {
                    Some(grant) => {
                        tool_environment.apply(&mut request);
                        grant.apply(&mut request);
                    }
                    None => tool_environment.apply(&mut request),
                }
                request.network = if network {
                    NetworkPolicy::Enabled
                } else {
                    NetworkPolicy::Disabled
                };
                let sandbox = tool_environment
                    .grant_sandbox(sandbox)
                    .map_err(|error| format!("MCP server `{key}` tool grant failed: {error}"))?;
                let sandbox = if grant.and_then(|grant| grant.view()).is_none() {
                    tool_environment
                        .grant_mcp_python(&sandbox, &mut request)
                        .map_err(|error| {
                            format!("MCP server `{key}` Python grant failed: {error}")
                        })?
                } else {
                    sandbox
                };
                let command = sandbox.isolated_command(request).map_err(|error| {
                    let reason = format!("MCP server `{key}` sandbox launch rejected: {error}");
                    match error {
                        SandboxError::InvalidRequest(_) => PreparationError::KnownFailure(reason),
                        _ => PreparationError::Unavailable(reason),
                    }
                })?;
                McpHost::connect_sandboxed_stdio_controlled(
                    key.into(),
                    command,
                    self.startup_cancellation
                        .clone()
                        .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false))),
                    timeout,
                )
                .await
                .map_err(|error| {
                    let error = match error {
                        McpHostError::Initialize(reason) => PreparationError::KnownFailure(
                            format!("MCP initialization failed: {reason}"),
                        ),
                        error => PreparationError::Unavailable(error.to_string()),
                    };
                    error.context(&format!("MCP server `{key}` failed to bind"))
                })
            }
            other => Err(format!(
                "Plugin `{}` server `{}` has unsupported transport `{other}`",
                definition.plugin_id, definition.name
            )
            .into()),
        }
    }
}

fn is_oauth_config(config: &Value) -> bool {
    config.get("auth").and_then(Value::as_str) == Some("oauth")
        || config.get("oauth_resource").is_some()
}

#[derive(Debug, Deserialize)]
struct RefreshTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    token_type: Option<String>,
    scope: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RefreshTokenFailure {
    error: Option<String>,
}

#[derive(Clone)]
struct ReqwestOAuthRefreshTransport;

impl OAuthRefreshTransport for ReqwestOAuthRefreshTransport {
    fn refresh(
        &self,
        request: &OAuthRefreshRequest<'_>,
    ) -> Result<OAuthTokenResponse, OAuthTransportError> {
        let client_id = request.metadata().client_id().unwrap_or_default();
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", request.refresh_token().expose_secret()),
            ("client_id", client_id),
        ];
        if let Some(resource) = request.metadata().resource() {
            form.push(("resource", resource));
        }
        let endpoint = match url::Url::parse(request.metadata().token_endpoint()) {
            Ok(endpoint) => endpoint,
            Err(_) => return Err(OAuthTransportError::RequestFailed),
        };
        let client = crate::oauth_http::blocking_client(&endpoint)
            .map_err(|_| OAuthTransportError::RequestFailed)?;
        let response = client
            .post(endpoint)
            .form(&form)
            .send()
            .map_err(|_| OAuthTransportError::RequestFailed)?;
        let success = response.status().is_success();
        let bytes =
            read_oauth_response(response).map_err(|_| OAuthTransportError::RequestFailed)?;
        if !success {
            let revoked = serde_json::from_slice::<RefreshTokenFailure>(&bytes)
                .ok()
                .and_then(|failure| failure.error)
                .is_some_and(|failure| failure == "invalid_grant");
            return Err(if revoked {
                OAuthTransportError::Revoked
            } else {
                OAuthTransportError::RequestFailed
            });
        }
        let token: RefreshTokenResponse =
            serde_json::from_slice(&bytes).map_err(|_| OAuthTransportError::RequestFailed)?;
        let mut response =
            OAuthTokenResponse::new(token.access_token, token.refresh_token, token.expires_in)
                .map_err(|_| OAuthTransportError::RequestFailed)?;
        if let Some(token_type) = token.token_type {
            if !token_type.eq_ignore_ascii_case("bearer") {
                return Err(OAuthTransportError::RequestFailed);
            }
            response = response
                .with_token_type(token_type)
                .map_err(|_| OAuthTransportError::RequestFailed)?;
        }
        if let Some(scope) = token.scope {
            response = response
                .with_scope(scope)
                .map_err(|_| OAuthTransportError::RequestFailed)?;
        }
        Ok(response)
    }
}

fn read_oauth_response(response: reqwest::blocking::Response) -> Result<Vec<u8>, ()> {
    let mut bytes = Vec::new();
    response
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > 128 * 1024 {
        return Err(());
    }
    Ok(bytes)
}

/// Name the Plugin's own view inside one server declaration.
///
/// The token is a deployment path, so only the host that *granted* the view may
/// substitute it: the path written here is exactly the path granted to this
/// launch, which is why the command the sandbox authorizes and the environment it
/// mounts can never be two different things.
fn named_view(
    config: &Value,
    declaration: &Value,
    view: Option<&Path>,
    plugin: &str,
) -> Result<Value, String> {
    let token = format!("${{{}}}", anchor_graph_host::VIEW_TOKEN);
    let mut named = config.clone();
    let mut needs_view = false;
    map_declared_paths(&mut named, declaration, |template, value| {
        if !template.contains(&token) {
            return value.to_owned();
        }
        needs_view = true;
        view.map(|view| value.replace(&token, &view.to_string_lossy()))
            .unwrap_or_else(|| value.to_owned())
    });
    if needs_view && view.is_none() {
        return Err(format!(
            "Plugin `{plugin}` names ${{{}}} but this host has no usable dependency view for it; install the Plugin on a deployment that authorizes dependency installation, or repair its view",
            anchor_graph_host::VIEW_TOKEN
        ));
    }
    Ok(named)
}

fn injected_environment(config: &Value, name: &str) -> bool {
    ["env_vars", "optional_env_vars"].iter().any(|field| {
        config
            .get(field)
            .and_then(Value::as_array)
            .is_some_and(|names| names.iter().any(|value| value.as_str() == Some(name)))
    })
}

fn plugin_path_template(value: &str) -> bool {
    [
        "PLUGIN_ROOT",
        "CODEX_PLUGIN_ROOT",
        "CLAUDE_PLUGIN_ROOT",
        anchor_graph_host::VIEW_TOKEN,
    ]
    .iter()
    .any(|name| value.contains(&format!("${{{name}}}")) || value.contains(&format!("${{{name}:-")))
}

/// Only declaration-owned process paths can name mounts. Headers and explicit
/// environment injection slots are opaque transport data, never path templates.
fn map_declared_paths(
    config: &mut Value,
    declaration: &Value,
    mut map: impl FnMut(&str, &str) -> String,
) {
    for key in ["command", "cwd"] {
        if let Some(template) = declaration.get(key).and_then(Value::as_str)
            && let Some(Value::String(value)) = config.get_mut(key)
        {
            *value = map(template, value);
        }
    }
    if let Some(templates) = declaration.get("args").and_then(Value::as_array)
        && let Some(values) = config.get_mut("args").and_then(Value::as_array_mut)
    {
        for (template, value) in templates.iter().zip(values) {
            if let Some(template) = template.as_str()
                && let Value::String(value) = value
            {
                *value = map(template, value);
            }
        }
    }
    if let Some(templates) = declaration.get("env").and_then(Value::as_object)
        && let Some(values) = config.get_mut("env").and_then(Value::as_object_mut)
    {
        for (key, template) in templates {
            if !injected_environment(declaration, key)
                && let Some(template) = template.as_str()
                && let Some(Value::String(value)) = values.get_mut(key)
            {
                *value = map(template, value);
            }
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
    declaration: &Value,
    plugin_directory: &Path,
    plugin_id: &str,
) -> Result<(String, Vec<String>, Vec<SandboxEnvironment>, String), String> {
    let command = config
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "MCP stdio server requires a non-empty command".to_owned())?;
    // A relative command is a Plugin resource; an absolute one is the server's
    // own path (an interpreter, or a directory inside a dependency environment)
    // and must be passed through unchanged. Joining an absolute path into the
    // Plugin directory happens to resolve to the absolute path again, which
    // silently rewrote every absolute command into `/plugins/<id>/<abs>`.
    let program = visible_plugin_path(command, plugin_directory, plugin_id)
        .or_else(|| {
            if Path::new(command).is_absolute() {
                return None;
            }
            let candidate = plugin_directory.join(command);
            candidate
                .is_file()
                .then(|| format!("/plugins/{plugin_id}/{}", command.replace('\\', "/")))
        })
        .unwrap_or_else(|| {
            if Path::new(command).is_absolute() {
                return command.to_owned();
            }
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
                .enumerate()
                .map(|(index, item)| {
                    item.as_str()
                        .map(|value| {
                            visible_plugin_path(value, plugin_directory, plugin_id)
                                .or_else(|| {
                                    // Same rule as the command: only a relative
                                    // argument can name a Plugin resource.
                                    if Path::new(value).is_absolute() {
                                        return None;
                                    }
                                    let candidate = plugin_directory.join(value);
                                    candidate.is_file().then(|| {
                                        format!("/plugins/{plugin_id}/{}", value.replace('\\', "/"))
                                    })
                                })
                                .unwrap_or_else(|| {
                                    if declaration
                                        .get("args")
                                        .and_then(Value::as_array)
                                        .and_then(|values| values.get(index))
                                        .and_then(Value::as_str)
                                        .is_some_and(plugin_path_template)
                                    {
                                        visible_plugin_value(value, plugin_directory, plugin_id)
                                    } else {
                                        value.to_owned()
                                    }
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
                            let path = !injected_environment(declaration, key)
                                && declaration
                                    .get("env")
                                    .and_then(|values| values.get(key))
                                    .and_then(Value::as_str)
                                    .is_some_and(plugin_path_template);
                            SandboxEnvironment::new(
                                key,
                                if path {
                                    visible_plugin_value(value, plugin_directory, plugin_id)
                                } else {
                                    value.to_owned()
                                },
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
    owners: BTreeMap<String, crate::plugin_disclosure::ToolOwner>,
    // Host-owned launcher files must live as long as the formal MCP server.
    _environment: crate::tool_environment::ToolEnvironment,
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
            host.call_contents(&remote_name, arguments)
                .await
                .map_err(|error| ToolError::Failed(error.to_string()))
        })
    }
}

impl LiveMcpTools {
    pub(crate) async fn shutdown(self) -> Result<(), String> {
        let mut errors = Vec::new();
        for host in self.hosts.into_values() {
            match Arc::try_unwrap(host) {
                Ok(host) => {
                    if let Err(error) = host.shutdown().await {
                        errors.push(error.to_string());
                    }
                }
                Err(_) => errors.push("MCP probe still has an active reference".to_owned()),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
    fn validate_exposed_names(&self) -> Result<(), String> {
        let mut names = BTreeSet::new();
        for (server_id, host) in &self.hosts {
            for tool in host.tools() {
                let exposed = exposed_tool_name(server_id, &tool.name);
                if crate::tool_disclosure::is_meta(&exposed)
                    || exposed == "final_result"
                    || exposed == FAKE_ECHO_TOOL
                    || exposed == crate::tool_names::RUN_TOOL_NAME
                    || exposed == crate::tool_names::SEND_TOOL
                    || exposed == crate::tool_names::SEND_FILE_TOOL
                    || exposed == crate::tool_names::IMAGE_TOOL
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

/// The Plugin MCP naming contract exposes one model tool as
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
    pub(crate) fn tool_owners(&self) -> BTreeMap<String, crate::plugin_disclosure::ToolOwner> {
        let mut owners = self.live.owners.clone();
        if self.fake.enabled {
            owners.insert(
                FAKE_ECHO_TOOL.into(),
                crate::plugin_disclosure::ToolOwner {
                    plugin: "fake-tools".into(),
                    server: FAKE_SERVER_ID.into(),
                    tool: FAKE_ECHO_TOOL.into(),
                },
            );
        }
        owners
    }
    pub fn new(fake: BoundPluginTools, live: LiveMcpTools) -> Self {
        Self { fake, live }
    }

    pub(crate) async fn shutdown(self) -> Result<(), String> {
        self.live.shutdown().await
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
                Ok(fake_binding())
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
                if !is_fake_fixture(binding) || !self.plugin_ids.contains(&binding.id) {
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
    #[cfg(test)]
    pub async fn assemble(
        &self,
        mcp: &McpToolConfig,
        bindings: &[PluginBinding],
        network: bool,
    ) -> Result<Arc<CombinedPluginTools>, String> {
        self.assemble_for_execution(mcp, bindings, network, None)
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn assemble_for_execution(
        &self,
        mcp: &McpToolConfig,
        bindings: &[PluginBinding],
        network: bool,
        launch: Option<SandboxLaunch<'_>>,
    ) -> Result<Arc<CombinedPluginTools>, PreparationError> {
        let fake = self
            .for_bindings(bindings)
            .map_err(|error| error.to_string())?;
        let live = mcp.bind_inner(bindings, network, launch).await?;
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
mod oauth_tests;

#[cfg(test)]
mod preparation_tests;

#[cfg(test)]
mod path_template_tests;

#[cfg(test)]
mod python_runtime_tests;

#[cfg(test)]
mod tests {
    use anchor_graph_host::PluginCatalog;

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
            authorization_digest: "fixture-authority".try_into().unwrap(),
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
        let binding = crate::plugin_library::Catalog::new(root.path())
            .resolve(&["research".into()])
            .unwrap()
            .remove(0);
        let config = McpToolConfig::from_catalog(root.path(), &[binding]).unwrap();
        assert!(config.servers.contains_key("research-papers"));
        assert_eq!(
            config.servers["research-papers"].config["url"],
            "http://127.0.0.1:9/mcp"
        );
    }

    #[test]
    fn fake_fixture_binding_does_not_require_a_filesystem_catalog() {
        let config = McpToolConfig::from_catalog("/does/not/exist", &[fake_binding()]).unwrap();
        assert!(config.servers.is_empty());
        let mut legacy = serde_json::to_value(fake_binding()).unwrap();
        legacy["authorization_digest"] = json!("");
        let error = serde_json::from_value::<PluginBinding>(legacy)
            .unwrap_err()
            .to_string();
        assert!(error.contains("old authorization format"), "{error}");
        assert!(error.contains("offline migration required"), "{error}");
    }

    #[tokio::test]
    async fn optional_unconfigured_mcp_is_absent_without_disabling_the_plugin() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/optional");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.json"),
            r#"{"name":"Optional","mcpServers":{"api":{"command":"bash5","args":["server.sh","inspect"],"optional_env_vars":["ANCHOR_TEST_UNCONFIGURED_OPTIONAL_MCP_7F90"]}}}"#).unwrap();
        let binding = crate::plugin_library::Catalog::new(root.path())
            .resolve(&["optional".into()])
            .unwrap()
            .remove(0);
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

    fn stdio_fixture_binary() -> PathBuf {
        static BINARY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        BINARY
            .get_or_init(|| {
                let executable = std::env::current_exe().unwrap();
                let profile = executable.parent().unwrap().parent().unwrap();
                let target = profile.parent().unwrap();
                let mut build = std::process::Command::new("cargo");
                build
                    .args([
                        "build",
                        "--offline",
                        "-p",
                        "anchor-mcp-host",
                        "--example",
                        "http_fixture",
                    ])
                    .arg("--manifest-path")
                    .arg(
                        Path::new(env!("CARGO_MANIFEST_DIR"))
                            .parent()
                            .unwrap()
                            .join("Cargo.toml"),
                    )
                    .env("CARGO_TARGET_DIR", target);
                if profile.file_name().is_some_and(|name| name == "release") {
                    build.arg("--release");
                }
                let output = build.output().unwrap();
                assert!(
                    output.status.success(),
                    "native MCP fixture build failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                profile
                    .join("examples/http_fixture")
                    .canonicalize()
                    .unwrap()
            })
            .clone()
    }

    const STDIO_FIXTURE: &str = r#"set -eu
mode="$1"
if [ "$mode" = echo ]; then
    [ "$PWD" = /plugins/research/nested ]
    dependency_root="$PYTHONPATH"
    . "$dependency_root/installed_dependency.sh"
    exec ../mcp-fixture --stdio echo --prefix "$prefix"
fi
exec ./mcp-fixture --stdio inspect
"#;

    #[tokio::test]
    async fn stdio_mcp_receives_only_its_nodes_readonly_grants() {
        use anchor_sandbox_bwrap::BubblewrapPolicy;
        let shell = std::fs::canonicalize("/usr/bin/bash").unwrap();
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/reader");
        let workspace = root.path().join("workspace");
        let other_workspace = root.path().join("other-workspace");
        let private = root.path().join("private");
        for path in [&plugin, &workspace, &other_workspace, &private] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(private.join("history.txt"), "mcp-local-evidence").unwrap();
        std::fs::copy(stdio_fixture_binary(), plugin.join("mcp-fixture")).unwrap();
        std::fs::write(plugin.join("server.sh"), STDIO_FIXTURE).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            json!({
                "name":"Reader", "mcpServers":{"stdio":{"command":shell,"args":["server.sh","inspect"]}}
            })
            .to_string(),
        )
        .unwrap();
        let binding = Catalog::new(root.path())
            .resolve(&["reader".into()])
            .unwrap()
            .remove(0);
        let config =
            McpToolConfig::from_catalog(root.path(), std::slice::from_ref(&binding)).unwrap();
        let sandbox = BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", [shell.file_name().unwrap().to_str().unwrap()])
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
                SandboxLaunch {
                    sandbox: &node_sandbox,
                    workspace: &workspace,
                    readonly_inputs: &grants,
                    plugin_views: &BTreeMap::new(),
                },
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
            .bind_with_node_inputs(
                &[binding],
                false,
                SandboxLaunch {
                    sandbox: &sandbox,
                    workspace: &other_workspace,
                    readonly_inputs: &[],
                    plugin_views: &BTreeMap::new(),
                },
            )
            .await
            .unwrap();
        let result = tools.call("reader-stdio_inspect", json!({})).await.unwrap();
        let value: Value = serde_json::from_str(result[0].as_text().unwrap()).unwrap();
        assert_eq!(value, json!({"visible":false}));
    }

    #[tokio::test]
    async fn stdio_mcp_runs_inside_the_plugin_mount_and_exposes_direct_tools() {
        let shell = std::fs::canonicalize("/usr/bin/bash").unwrap();
        use anchor_sandbox_bwrap::BubblewrapPolicy;
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/research");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::copy(stdio_fixture_binary(), plugin.join("mcp-fixture")).unwrap();
        std::fs::write(plugin.join("server.sh"), STDIO_FIXTURE).unwrap();
        std::fs::create_dir(plugin.join("nested")).unwrap();
        let mut permissions = std::fs::metadata(plugin.join("server.sh"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(plugin.join("server.sh"), permissions).unwrap();
        let environment = root.path().join("external-env");
        let tool = root.path().join("tools/bash");
        let imports = root.path().join("external-modules");
        // Another installation sorts first on PATH; it must not replace the
        // explicitly requested interpreter of this MCP server.
        let decoy = root.path().join("decoy-env");
        let decoy_tool = root.path().join("tools/a-decoy");
        std::fs::create_dir_all(decoy.join("bin")).unwrap();
        std::fs::create_dir_all(&decoy_tool).unwrap();
        std::fs::write(decoy.join("bin/bash"), "#!/bin/sh\nexit 88\n").unwrap();
        std::fs::set_permissions(
            decoy.join("bin/bash"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("bash", decoy.join("bin/bash5")).unwrap();
        std::fs::write(
            decoy_tool.join("tool.json"),
            json!({
                "entrypoint":decoy.join("bin/bash"), "environment":decoy
            })
            .to_string(),
        )
        .unwrap();
        std::fs::create_dir_all(environment.join("bin")).unwrap();
        std::fs::create_dir_all(&tool).unwrap();
        std::fs::create_dir(&imports).unwrap();
        std::os::unix::fs::symlink("bash5", environment.join("bin/bash")).unwrap();
        std::os::unix::fs::symlink(&shell, environment.join("bin/bash5")).unwrap();
        std::fs::write(
            imports.join("installed_dependency.sh"),
            "prefix='dependency:'",
        )
        .unwrap();
        std::fs::write(tool.join("tool.json"), json!({
            "entrypoint":environment.join("bin/bash"), "environment":environment, "imports":[imports]
        }).to_string()).unwrap();
        let standalone = root.path().join("standalone-launcher");
        let standalone_tool = root.path().join("tools/standalone");
        std::fs::create_dir(&standalone_tool).unwrap();
        std::fs::write(
            &standalone,
            format!("#!/bin/sh\nexec '{}' \"$@\"\n", shell.display()),
        )
        .unwrap();
        std::fs::set_permissions(&standalone, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            standalone_tool.join("tool.json"),
            json!({"entrypoint":standalone}).to_string(),
        )
        .unwrap();
        std::fs::write(plugin.join("plugin.json"), json!({
            "name":"Research", "mcpServers":{
                "stdio":{
                    "command": environment.join("bin/bash"), "args":["../server.sh","echo"], "cwd":"nested"
                },
                "versioned":{
                    "command":environment.join("bin/bash5"), "args":["../server.sh","echo"], "cwd":"nested"
                },
                "system":{
                    "command":shell, "args":["../server.sh","echo"], "cwd":"nested"
                },
                "standalone":{
                    "command":standalone, "args":["../server.sh","echo"], "cwd":"nested"
                }
            }
        }).to_string()).unwrap();
        let binding = Catalog::new(root.path())
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
                BubblewrapPolicy::new("bwrap", [shell.file_name().unwrap().to_str().unwrap()])
                    .authorize_workspace_root(&workspace)
                    .authorize_readonly_input_root(root.path())
                    .authorize_readonly_destination_root("/plugins"),
            ),
        )
        .unwrap();
        let tools = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            config.bind_with_node_inputs(
                std::slice::from_ref(&binding),
                false,
                SandboxLaunch {
                    sandbox: &sandbox,
                    workspace: &workspace,
                    readonly_inputs: &[],
                    plugin_views: &BTreeMap::new(),
                },
            ),
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
            "research-versioned_echo",
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
        let ungranted = root.path().join("bash5");
        std::fs::copy(&standalone, &ungranted).unwrap();
        config
            .servers
            .get_mut("research-standalone")
            .unwrap()
            .config["command"] = json!(ungranted);
        assert!(matches!(
            config.bind_with_node_inputs(&[binding], false, SandboxLaunch { sandbox: &sandbox, workspace: &workspace, readonly_inputs: &[], plugin_views: &BTreeMap::new() }).await,
            Err(error) if error.contains("sandbox launch rejected") && error.contains("command must select")
        ));
    }
}

#[cfg(test)]
mod http_tests;
