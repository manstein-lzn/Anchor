//! Executes one node request; graph scheduling remains in the shared Runner.
use super::{HostArtifacts, create_durable_directory, write_durable};
use crate::{local_inputs::LocalInputs, op, tool_host};
use anchor_runtime::graph::{
    CompletionFact, FileRunStore, GraphError, GraphRunRecord, InvocationKey, NodeCompletion,
    NodeExecutionCapabilities, NodeExecutionOutcome, NodeExecutionPort, NodeExecutionRequest,
    NodeKind, PluginBinding, RecoveryDecision, RunStore,
};
use anchor_runtime::{NetworkPolicy, SandboxError, SandboxPort, SandboxRequest, SandboxStatus};
use anchor_sandbox_bwrap::BubblewrapSandbox;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::atomic::Ordering,
    time::Duration,
};

mod contracts;
mod conversation;
mod media;

pub(crate) use contracts::{NodeConversationHint, NodeHostResolver, NodeImage, ToolResolution};

pub(crate) fn conversation_hint_for(
    metadata: &crate::application::RunMetadata,
    node: &str,
) -> Option<NodeConversationHint> {
    let binding = metadata.conversation.as_ref()?;
    let identity = serde_json::to_vec(&(&metadata.bundle_source, &binding.session, node))
        .expect("host conversation identity is serializable");
    Some(NodeConversationHint {
        key: format!("{:x}", Sha256::digest(identity)),
    })
}

pub(crate) struct HostNodes {
    pub(crate) sandbox: std::sync::Arc<BubblewrapSandbox>,
    pub(crate) allowed_commands: Vec<String>,
    pub(crate) artifacts: HostArtifacts,
    pub(crate) facts_root: PathBuf,
    pub(crate) io_resolver: std::sync::Arc<HostIoResolver>,
    pub(crate) mcp: tool_host::McpToolConfig,
    pub(crate) goose_nodes: Option<crate::goose_acp::GooseNodePort>,
}

/// Resolves only resources already frozen into a Graph Run. Tool clients and
/// sandbox access are owned by the returned ToolPort so the Goose MCP bridge
/// can bind them without borrowing HostNodes.
pub(crate) struct HostIoResolver {
    artifacts: HostArtifacts,
    sandbox: std::sync::Arc<BubblewrapSandbox>,
    tools: std::sync::Arc<tool_host::PluginToolHost>,
    mcp: tool_host::McpToolConfig,
    plugin_bindings: BTreeMap<String, PluginBinding>,
    run_store: FileRunStore,
    catalog_roots: Vec<PathBuf>,
    local_inputs: LocalInputs,
}

impl HostIoResolver {
    pub(crate) fn new(
        artifacts: HostArtifacts,
        sandbox: std::sync::Arc<BubblewrapSandbox>,
        tools: std::sync::Arc<tool_host::PluginToolHost>,
        mcp: tool_host::McpToolConfig,
        plugin_bindings: BTreeMap<String, PluginBinding>,
        run_store: FileRunStore,
        local_inputs: LocalInputs,
    ) -> Self {
        let mut catalog_roots = Vec::new();
        for root in [
            std::env::var_os("ANCHOR_RUNNER_BUNDLE_ROOT").map(PathBuf::from),
            std::env::var_os("ANCHOR_RUNNER_CATALOG_ROOT").map(PathBuf::from),
        ]
        .into_iter()
        .flatten()
        {
            if root.is_dir() && !catalog_roots.contains(&root) {
                catalog_roots.push(root.clone());
            }
            if let Ok(entries) = std::fs::read_dir(&root) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && !catalog_roots.contains(&path) {
                        catalog_roots.push(path);
                    }
                }
            }
        }
        Self {
            artifacts,
            sandbox,
            tools,
            mcp,
            plugin_bindings,
            run_store,
            catalog_roots,
            local_inputs,
        }
    }

    pub(crate) fn local_inputs_configured(&self) -> bool {
        self.local_inputs.configured()
    }

    pub(crate) fn validate_plugin_configuration(
        &self,
        bindings: &[PluginBinding],
    ) -> Result<(), String> {
        self.catalog_config(bindings)?
            .validate_bindings(bindings, true)
    }

    pub(crate) fn bind_local_inputs(
        &self,
        record: &GraphRunRecord,
        graph: Option<&str>,
    ) -> Result<(), String> {
        let existing = self
            .run_store
            .load(&record.run_id)
            .map_err(|error| error.to_string())?;
        if let Some(existing) = &existing
            && (existing.snapshot != record.snapshot || existing.input != record.input)
        {
            return Err("Run local input identity differs from the admitted snapshot/input".into());
        }
        self.local_inputs.freeze(
            &record.run_id,
            &record.graph_digest,
            &record.snapshot,
            graph,
            existing.is_none(),
        )
    }

    pub(crate) fn verify_local_inputs(&self, record: &GraphRunRecord) -> Result<(), String> {
        self.local_inputs.verify(record)?;
        if self.local_inputs.configured() {
            let metadata =
                crate::application::metadata::load(self.local_inputs.state_root(), &record.run_id)
                    .map_err(|error| format!("local input Run identity unavailable: {error:?}"))?
                    .ok_or("local input authorization requires immutable Run Graph metadata")?;
            if metadata.graph_digest != record.graph_digest {
                return Err(
                    "local input Run Graph metadata differs from the admitted snapshot".into(),
                );
            }
            self.local_inputs.freeze(
                &record.run_id,
                &record.graph_digest,
                &record.snapshot,
                Some(&metadata.graph),
                false,
            )?;
        }
        Ok(())
    }

    fn local_input_mounts(
        &self,
        key: &InvocationKey,
    ) -> Result<Vec<anchor_runtime::ReadOnlyInput>, String> {
        let record = self
            .run_store
            .load(&key.run_id)
            .map_err(|error| error.to_string())?
            .ok_or("local input mounts require a durable admitted Run")?;
        // Old Runs could not receive local inputs. Only their empty authority
        // may be recorded during migration; new grants require a new Run.
        if !self.local_inputs.configured() {
            self.local_inputs.freeze(
                &record.run_id,
                &record.graph_digest,
                &record.snapshot,
                None,
                false,
            )?;
        }
        self.verify_local_inputs(&record)?;
        let mut mounts = self.local_inputs.mounts(&record, key)?;
        if let Some(mount) = self.channel_inputs(key)?.mount {
            mounts.push(mount);
        }
        Ok(mounts)
    }

    fn catalog_config(
        &self,
        bindings: &[PluginBinding],
    ) -> Result<tool_host::McpToolConfig, String> {
        if bindings.iter().all(|binding| {
            binding
                .mcp_servers
                .iter()
                .all(|server| server == tool_host::FAKE_SERVER_ID)
        }) {
            return Ok(self.mcp.clone());
        }
        let mut last_error = None;
        for root in &self.catalog_roots {
            if let Err(error) = tool_host::McpToolConfig::plugin_mounts(root, bindings) {
                last_error = Some(error);
                continue;
            }
            match tool_host::McpToolConfig::from_catalog(root, bindings) {
                Ok(mut config) => {
                    config.environment = self.mcp.environment.clone();
                    return Ok(config);
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            "Plugin-bearing Agent requires an accepted bundle/catalog root".into()
        }))
    }

    fn oauth_owner_for_run(&self, run_id: &str) -> Result<Option<String>, String> {
        crate::application::metadata::load(self.local_inputs.state_root(), run_id)
            .map(|metadata| metadata.and_then(|metadata| metadata.oauth_owner))
            .map_err(|error| format!("OAuth Run owner is unavailable: {error:?}"))
    }

    fn plugin_mounts(
        &self,
        bindings: &[PluginBinding],
    ) -> Result<Vec<anchor_runtime::ReadOnlyInput>, String> {
        if bindings
            .iter()
            .all(|binding| binding.id == "fake-tools" && binding.resources.is_empty())
        {
            return Ok(Vec::new());
        }
        let mut last_error = None;
        for root in &self.catalog_roots {
            match tool_host::McpToolConfig::plugin_mounts(root, bindings) {
                Ok(mounts) => return Ok(mounts),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| "Plugin resource root is unavailable".into()))
    }
}

impl NodeHostResolver for HostIoResolver {
    fn prompt_images(&self, request: &NodeExecutionRequest) -> Result<Vec<NodeImage>, String> {
        Ok(self
            .channel_inputs(&request.key)?
            .images
            .into_iter()
            .map(|image| NodeImage {
                data: image.bytes,
                media_type: image.media_type,
            })
            .collect())
    }
    fn conversation_hint(
        &self,
        request: &NodeExecutionRequest,
    ) -> Result<Option<NodeConversationHint>, String> {
        self.conversation(&request.key)
            .map(|conversation| conversation.map(|value| NodeConversationHint { key: value.key }))
    }

    fn resolve_plugins(&self, ids: &[String]) -> Result<Vec<PluginBinding>, String> {
        ids.iter()
            .map(|id| {
                self.plugin_bindings
                    .get(id)
                    .cloned()
                    .ok_or_else(|| format!("Plugin `{id}` is not bound by the admitted bundle"))
            })
            .collect()
    }

    fn workspace(&self, request: &NodeExecutionRequest) -> Result<PathBuf, String> {
        self.artifacts
            .prepare_workspace(&request.key, &request.input_commits)
            .map_err(|error| error.to_string())
    }

    fn tools<'a>(&'a self, request: &'a NodeExecutionRequest) -> ToolResolution<'a> {
        Box::pin(async move {
            let admitted = self
                .run_store
                .load(&request.key.run_id)
                .map_err(|error| format!("load admitted Run Plugin bindings: {error}"))?
                .ok_or_else(|| {
                    format!(
                        "Run `{}` has no durable Plugin binding record",
                        request.key.run_id
                    )
                })?;
            if admitted.graph_digest != request.key.graph_digest
                || !admitted.plugin_bindings_initialized
            {
                return Err(format!(
                    "Run `{}` has no matching initialized Plugin binding record",
                    request.key.run_id
                ));
            }
            for binding in &request.plugins {
                let Some(frozen) = admitted.plugin_bindings.get(&binding.id) else {
                    return Err(format!(
                        "Plugin `{}` is not bound by the admitted Run",
                        binding.id,
                    ));
                };
                if frozen != binding {
                    return Err(format!(
                        "Plugin `{}` differs from the frozen Run binding",
                        binding.id
                    ));
                }
            }
            let workspace = self.workspace(request)?;
            let mut local_inputs = self.local_input_mounts(&request.key)?;
            let conversation = self.conversation(&request.key)?;
            if let Some(conversation) = &conversation
                && let Some(mount) = self.previous_mount(&request.key, conversation)?
            {
                local_inputs.push(mount);
            }
            let sandbox = std::sync::Arc::new(
                self.sandbox
                    .with_readonly_grants(&local_inputs)
                    .map_err(|error| error.to_string())?,
            );
            let mut readonly_inputs = self
                .artifacts
                .input_mounts(
                    &request.input_commits,
                    &request.key.run_id,
                    &request.key.graph_digest,
                )
                .map_err(|error| error.to_string())?;
            readonly_inputs.extend(self.plugin_mounts(&request.plugins)?);
            readonly_inputs.extend(local_inputs);
            let mut mcp = self.catalog_config(&request.plugins)?;
            mcp.authorize_oauth(
                self.oauth_owner_for_run(&request.key.run_id)?,
                request.network,
            )
            .await?;
            let plugins = self
                .tools
                .assemble_with_sandbox(
                    &mcp,
                    &request.plugins,
                    request.network,
                    &sandbox,
                    &workspace,
                    &readonly_inputs,
                )
                .await?;
            let tools = std::sync::Arc::new(
                crate::node_tools::NodeTools::new(
                    crate::channel_tools::wrap(
                        plugins,
                        &request.plugins,
                        request.key.clone(),
                        self.local_inputs.state_root().to_path_buf(),
                        conversation.as_ref().map(|value| value.reply_node.clone()),
                        workspace.clone(),
                        readonly_inputs.clone(),
                        request.cancellation.clone(),
                    ),
                    sandbox,
                    workspace,
                    readonly_inputs,
                    request.cancellation.clone(),
                )
                .with_environment(self.mcp.environment.clone())
                .with_network(request.network),
            ) as std::sync::Arc<dyn anchor_runtime::ToolPort>;
            Ok(match conversation {
                Some(conversation) => self.conversation_tools(tools, &request.key, conversation),
                None => tools,
            })
        })
    }
}

fn fact_stem(key: &InvocationKey) -> String {
    format!("nf1-{:x}", Sha256::digest(key.durable_key().as_bytes()))
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, GraphError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn load_host_completion_fact(
    root: &Path,
    key: &InvocationKey,
) -> Result<CompletionFact, GraphError> {
    let mut stems = vec![fact_stem(key)];
    // The former host admitted only single, simple node names. Preserve its
    // result/uncertainty facts without using graph paths as filesystem paths.
    if [&key.run_id, &key.graph_digest, &key.node_id]
        .iter()
        .all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    {
        stems.push(key.durable_key().replace(':', "_"));
    }
    for stem in stems {
        if let Some(bytes) = read_optional(&root.join(format!("{stem}.json")))? {
            return serde_json::from_slice(&bytes)
                .map(CompletionFact::Completed)
                .map_err(|e| GraphError::CorruptRun(e.to_string()));
        }
        if let Some(bytes) = read_optional(&root.join(format!("{stem}.failed")))? {
            return String::from_utf8(bytes)
                .map(CompletionFact::Failed)
                .map_err(|e| GraphError::CorruptRun(e.to_string()));
        }
        if read_optional(&root.join(format!("{stem}.started")))?.is_some() {
            return Ok(CompletionFact::Uncertain(
                "node was durably started but has no terminal result; automatic replay is refused"
                    .into(),
            ));
        }
    }
    Ok(CompletionFact::NotStarted)
}

/// Read the deterministic routing marker written by the anchor-route Op helper.
/// The graph kernel remains responsible for checking that the selected route
/// is an actual outgoing edge, and for requiring a choice when there are
/// multiple exits.
fn read_op_route(stdout: &str) -> (String, Option<String>) {
    let lines = stdout.lines().collect::<Vec<_>>();
    let Some((index, first)) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| !line.trim().is_empty())
    else {
        return (String::new(), None);
    };
    let Some(target) = first.trim().strip_prefix("ANCHOR_ROUTE:") else {
        return (stdout.trim().to_owned(), None);
    };
    let submission = lines[index + 1..].join("\n").trim().to_owned();
    (submission, Some(target.trim().to_owned()))
}

impl HostNodes {
    fn start(&self, key: &InvocationKey) -> Result<(), GraphError> {
        create_durable_directory(&self.facts_root)?;
        let path = self.facts_root.join(format!("{}.started", fact_stem(key)));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(b"started\n")?;
        file.sync_all()?;
        std::fs::File::open(&self.facts_root)?.sync_all()?;
        Ok(())
    }

    fn complete(
        &self,
        key: &InvocationKey,
        completion: NodeCompletion,
    ) -> Result<NodeExecutionOutcome, GraphError> {
        let bytes =
            serde_json::to_vec(&completion).map_err(|e| GraphError::CorruptRun(e.to_string()))?;
        write_durable(
            &self.facts_root.join(format!("{}.json", fact_stem(key))),
            &bytes,
        )?;
        Ok(NodeExecutionOutcome::Completed(completion))
    }

    async fn execute_node(
        &self,
        request: NodeExecutionRequest,
    ) -> Result<NodeExecutionOutcome, GraphError> {
        if request.kind != NodeKind::OpRun {
            return Err(GraphError::Unsupported(
                "HostNodes deterministic executor accepts Op.run only".into(),
            ));
        }
        let workspace = self
            .artifacts
            .prepare_workspace(&request.key, &request.input_commits)?;
        let local_inputs = self
            .io_resolver
            .local_input_mounts(&request.key)
            .map_err(GraphError::Unsupported)?;
        let mut readonly_inputs = self.artifacts.input_mounts(
            &request.input_commits,
            &request.key.run_id,
            &request.key.graph_digest,
        )?;
        readonly_inputs.extend(local_inputs.clone());
        if !request.plugins.is_empty() {
            return Err(GraphError::Unsupported(
                "Op.run cannot execute Plugins".into(),
            ));
        }
        let command = request
            .operation
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| GraphError::Unsupported("Op.run must be a command string".into()))?;
        // GraphRunner wraps the frozen Run input with upstream artifact metadata.
        // Business commands receive only the effective Graph input.
        let run_input = request.input.get("input").ok_or_else(|| {
            GraphError::Unsupported("Op.run request is missing effective Graph input".into())
        })?;
        let input = serde_json::to_string(run_input).map_err(|error| {
            GraphError::Unsupported(format!("Op.run input is not JSON: {error}"))
        })?;
        let argv =
            op::shell_command(command, &self.allowed_commands).map_err(GraphError::Unsupported)?;
        let route_helper = op::route_helper_mount().map_err(GraphError::Unsupported)?;
        let sandbox = self
            .sandbox
            .with_readonly_grants(&local_inputs)
            .and_then(|sandbox| sandbox.with_readonly_grants(std::slice::from_ref(&route_helper)))
            .map_err(|error| GraphError::Unsupported(error.to_string()))?;
        let mut sandbox_request = SandboxRequest {
            workspace,
            working_directory: None,
            command: argv,
            readonly_inputs,
            workspace_readonly: vec![],
            tool_dirs: vec![],
            environment: vec![
                anchor_runtime::SandboxEnvironment::new("ANCHOR_NODE", request.key.node_id.clone()),
                anchor_runtime::SandboxEnvironment::new("ANCHOR_ROUTES", request.routes.join(",")),
                anchor_runtime::SandboxEnvironment::new("ANCHOR_INPUT", input),
            ],
            network: if request.network {
                NetworkPolicy::Enabled
            } else {
                NetworkPolicy::Disabled
            },
            timeout: Duration::from_secs_f64(request.wall_time_limit_seconds.unwrap_or(3600.0)),
            max_output_bytes: 64 * 1024,
            spill: None,
            cancellation: request.cancellation.clone(),
        };
        self.mcp.environment.apply(&mut sandbox_request);
        op::add_route_helper(&mut sandbox_request, route_helper);
        self.start(&request.key)?;
        let result = match sandbox.run(sandbox_request).await {
            Ok(result) => result,
            Err(SandboxError::InvalidRequest(reason)) => {
                write_durable(
                    &self
                        .facts_root
                        .join(format!("{}.failed", fact_stem(&request.key))),
                    reason.as_bytes(),
                )?;
                return Ok(NodeExecutionOutcome::Failed { reason });
            }
            Err(error) => return Err(GraphError::Unsupported(error.to_string())),
        };
        match result.status {
            SandboxStatus::Completed if result.exit_code == Some(0) => {
                let (submission, route) = read_op_route(&result.stdout);
                self.complete(&request.key, NodeCompletion {
                    submission,
                    route,
                    model_requests: 0,
                    output: json!({"stdout":result.stdout,"stderr":result.stderr,"exit_code":result.exit_code}),
                })
            }
            SandboxStatus::Cancelled => Ok(NodeExecutionOutcome::Cancelled),
            _ => {
                let mut reason = format!("{} (exit_code={:?})", result.reason, result.exit_code);
                for output in [&result.stdout, &result.stderr] {
                    if !output.trim().is_empty() {
                        reason.push('\n');
                        reason.push_str(output.trim());
                    }
                }
                write_durable(
                    &self
                        .facts_root
                        .join(format!("{}.failed", fact_stem(&request.key))),
                    reason.as_bytes(),
                )?;
                Ok(NodeExecutionOutcome::Failed { reason })
            }
        }
    }
}

impl NodeExecutionPort for HostNodes {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: self.goose_nodes.is_some(),
            op_run: true,
            exact_provider_request_budget: false,
        }
    }
    fn resolve_plugins(&self, ids: &[String]) -> Result<Vec<PluginBinding>, GraphError> {
        self.io_resolver
            .resolve_plugins(ids)
            .map_err(GraphError::Unsupported)
    }
    fn completion_fact<'a>(
        &'a self,
        key: &'a InvocationKey,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>>
    {
        Box::pin(async move {
            let host = load_host_completion_fact(&self.facts_root, key)?;
            if crate::run_data::legacy_invocation_exists(
                self.io_resolver.local_inputs.state_root(),
                key,
            )
            .map_err(GraphError::Unsupported)?
            {
                return Ok(CompletionFact::Uncertain(
                    "legacy Agent state is not resumable by the standard Goose Host".into(),
                ));
            }
            if let Some(nodes) = &self.goose_nodes {
                let goose = nodes.completion_fact(key)?;
                return Ok(match (&host, &goose) {
                    (CompletionFact::NotStarted, _) => goose,
                    (_, CompletionFact::NotStarted) => host,
                    _ if host == goose => host,
                    _ => CompletionFact::Uncertain("host and Goose node facts conflict".into()),
                });
            }
            Ok(host)
        })
    }
    fn record_recovery_decision(
        &self,
        _key: &InvocationKey,
        _attempt_id: i64,
        _decision: RecoveryDecision,
    ) -> Result<(), GraphError> {
        Err(GraphError::Unsupported(
            "legacy Agent recovery is disabled in the standard Goose Host".into(),
        ))
    }
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>,
    > {
        if request.kind == NodeKind::Agent {
            match crate::run_data::legacy_invocation_exists(
                self.io_resolver.local_inputs.state_root(),
                &request.key,
            ) {
                Ok(false) => {}
                Ok(true) => {
                    return Box::pin(async {
                        Err(GraphError::Unsupported(
                            "legacy Agent state cannot be executed as a Goose invocation".into(),
                        ))
                    });
                }
                Err(error) => return Box::pin(async move { Err(GraphError::Unsupported(error)) }),
            }
            if let Some(nodes) = &self.goose_nodes {
                return Box::pin(nodes.execute(request));
            }
            return Box::pin(async {
                Err(GraphError::Unsupported(
                    "Agent execution requires a configured Goose runtime".into(),
                ))
            });
        }
        Box::pin(async move {
            let cancellation = request.cancellation.clone();
            let timeout =
                Duration::try_from_secs_f64(request.wall_time_limit_seconds.unwrap_or(3600.0))
                    .map_err(|_| GraphError::Unsupported("invalid node wall time".into()))?;
            let cancel = async {
                while !cancellation.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            };
            tokio::select! {
                biased;
                _ = cancel => Ok(NodeExecutionOutcome::Cancelled),
                result = tokio::time::timeout(timeout, self.execute_node(request)) => result
                    .map_err(|_| GraphError::Unsupported("node wall time exceeded; unfinished effects are uncertain".into()))?,
            }
        })
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::*;
    use anchor_runtime::graph::GraphSnapshot;
    use anchor_sandbox_bwrap::BubblewrapPolicy;
    use std::sync::atomic::AtomicBool;

    #[tokio::test]
    async fn resolver_returns_owned_anchor_tools_from_frozen_plugin_bindings() {
        let root = tempfile::tempdir().unwrap();
        let workspace_root = root.path().join("workspaces");
        let artifacts_root = root.path().join("artifacts");
        std::fs::create_dir_all(&workspace_root).unwrap();
        std::fs::create_dir_all(&artifacts_root).unwrap();
        let sandbox = std::sync::Arc::new(
            BubblewrapSandbox::new(
                BubblewrapPolicy::new("bwrap", ["sh"])
                    .authorize_workspace_root(&workspace_root)
                    .authorize_readonly_input_root(&artifacts_root)
                    .authorize_readonly_destination_root("/in"),
            )
            .unwrap(),
        );
        let tools = tool_host::PluginToolHost::new(["fake-tools".to_owned()]);
        let bindings = tools
            .resolve_fixture_plugins(&["fake-tools".into()])
            .unwrap();
        let store = FileRunStore::new(root.path().join("runs"));
        let snapshot = anchor_runtime::graph::GraphSnapshot::admit(json!({
            "objective":"resolver test","entry":"agent","agents":{"worker":{"model":"fixture"}},
            "ops":{},"nodes":[{"id":"agent","agent":"worker","plugins":["fake-tools"]}],"edges":[]
        }))
        .unwrap();
        let mut admitted = GraphRunRecord::create(snapshot, serde_json::Value::Null).unwrap();
        admitted.plugin_bindings = bindings
            .iter()
            .cloned()
            .map(|b| (b.id.clone(), b))
            .collect();
        admitted.plugin_bindings_initialized = true;
        store.save(&admitted).unwrap();
        let resolver = HostIoResolver::new(
            HostArtifacts::new(artifacts_root, workspace_root),
            sandbox,
            std::sync::Arc::new(tools),
            tool_host::McpToolConfig::default(),
            bindings
                .iter()
                .cloned()
                .map(|binding| (binding.id.clone(), binding))
                .collect(),
            store,
            LocalInputs::new(root.path().to_path_buf(), None).unwrap(),
        );

        assert_eq!(
            resolver.resolve_plugins(&["fake-tools".into()]).unwrap(),
            bindings
        );
        assert!(resolver.resolve_plugins(&["missing".into()]).is_err());

        let request = NodeExecutionRequest {
            key: InvocationKey {
                run_id: admitted.run_id,
                graph_digest: admitted.graph_digest,
                node_id: "agent".into(),
                invocation: 1,
            },
            model: Some("fixture".into()),
            task: "inspect inputs".into(),
            instructions: "use Anchor tools".into(),
            routes: Vec::new(),
            input: serde_json::Value::Null,
            input_commits: Vec::new(),
            plugins: bindings,
            max_provider_requests: None,
            wall_time_limit_seconds: Some(30.0),
            network: false,
            kind: NodeKind::Agent,
            operation: None,
            cancellation: std::sync::Arc::new(AtomicBool::new(false)),
        };
        let workspace = resolver.workspace(&request).unwrap();
        let tools = resolver.tools(&request).await.unwrap();
        let names = tools
            .definitions()
            .into_iter()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>();
        assert!(names.iter().any(|name| name == tool_host::FAKE_ECHO_TOOL));
        assert!(
            names
                .iter()
                .any(|name| name == crate::node_tools::RUN_TOOL_NAME)
        );
        assert!(workspace.is_dir());

        let mut drifted = request;
        drifted.plugins[0].digest = "sha256:drift".into();
        assert!(resolver.tools(&drifted).await.is_err());
    }

    #[tokio::test]
    async fn agent_command_tools_receive_only_the_nodes_frozen_local_inputs() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let config = root.path().join("operator-workspaces");
        let workspace = root.path().join("workspaces");
        let artifacts = root.path().join("artifacts");
        let private = root.path().join("private-inputs");
        for path in [&workspace, &artifacts, &private, &config.join("weekly")] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(private.join("history.txt"), "agent-local-evidence").unwrap();
        std::fs::write(
            config.join("weekly/local-inputs.json"),
            json!({"agent":{"history":private}}).to_string(),
        )
        .unwrap();
        let snapshot = GraphSnapshot::admit(json!({
            "objective":"agent local inputs", "entry":"agent", "agents":{"worker":{"model":"fixture"}},
            "nodes":[{"id":"agent","agent":"worker"},{"id":"other","agent":"worker"}],
            "edges":[{"from":"agent","to":"other"}]
        })).unwrap();
        let mut record =
            GraphRunRecord::create_with_id(snapshot, json!({}), "agent-local-inputs").unwrap();
        record.plugin_bindings_initialized = true;
        let local = LocalInputs::new(state.clone(), Some(config.clone())).unwrap();
        local
            .freeze(
                &record.run_id,
                &record.graph_digest,
                &record.snapshot,
                Some("weekly"),
                true,
            )
            .unwrap();
        let store = FileRunStore::new(state.join("runs"));
        store.save(&record).unwrap();
        crate::application::metadata::save(
            &state,
            &crate::application::RunMetadata::new(
                record.run_id.clone(),
                "weekly".into(),
                record.graph_digest.clone(),
                root.path(),
            )
            .unwrap(),
        )
        .unwrap();
        let sandbox = std::sync::Arc::new(
            BubblewrapSandbox::new(
                BubblewrapPolicy::new("bwrap", ["sh"])
                    .authorize_workspace_root(&workspace)
                    .authorize_readonly_input_root(&artifacts)
                    .authorize_readonly_destination_root("/in"),
            )
            .unwrap(),
        );
        let resolver = HostIoResolver::new(
            HostArtifacts::new(artifacts, workspace),
            sandbox,
            std::sync::Arc::new(tool_host::PluginToolHost::new(Vec::<String>::new())),
            tool_host::McpToolConfig::default(),
            BTreeMap::new(),
            store,
            local,
        );
        let mut request = NodeExecutionRequest {
            key: InvocationKey {
                run_id: record.run_id,
                graph_digest: record.graph_digest,
                node_id: "agent".into(),
                invocation: 1,
            },
            model: Some("fixture".into()),
            task: "read evidence".into(),
            instructions: String::new(),
            routes: Vec::new(),
            input: json!({}),
            input_commits: Vec::new(),
            plugins: Vec::new(),
            max_provider_requests: None,
            wall_time_limit_seconds: Some(30.0),
            network: false,
            kind: NodeKind::Agent,
            operation: None,
            cancellation: std::sync::Arc::new(AtomicBool::new(false)),
        };
        let tools = resolver.tools(&request).await.unwrap();
        let result = tools.call(crate::node_tools::RUN_TOOL_NAME, json!({"command":["sh","-c","cat /local-inputs/history/history.txt; ! touch /local-inputs/history/forbidden"]})).await.unwrap();
        assert_eq!(result[0].as_json().unwrap()["exit_code"], 0);
        assert_eq!(
            result[0].as_json().unwrap()["stdout"],
            "agent-local-evidence"
        );
        assert!(!private.join("forbidden").exists());
        request.key.node_id = "other".into();
        let tools = resolver.tools(&request).await.unwrap();
        let result = tools
            .call(
                crate::node_tools::RUN_TOOL_NAME,
                json!({"command":["sh","-c","test ! -e /local-inputs/history/history.txt"]}),
            )
            .await
            .unwrap();
        assert_eq!(result[0].as_json().unwrap()["exit_code"], 0);
        std::fs::write(
            config.join("weekly/local-inputs.json"),
            json!({"other":{"history":private}}).to_string(),
        )
        .unwrap();
        assert!(resolver.tools(&request).await.is_err());
    }
}
