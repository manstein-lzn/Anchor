//! Executes one node request; graph scheduling remains in the shared Runner.
use super::{HostArtifacts, create_durable_directory, write_durable};
use crate::tool_host;
use anchor_io_harness_runtime::node_port::{NodeHostResolver, ToolResolution};
use anchor_runtime_rig::graph::{
    CompletionFact, GraphError, InvocationKey, NodeCompletion, NodeExecutionCapabilities,
    NodeExecutionOutcome, NodeExecutionPort, NodeExecutionRequest, NodeKind, PluginBinding,
    RecoveryDecision,
};
use anchor_runtime_rig::{NetworkPolicy, SandboxPort, SandboxRequest, SandboxStatus};
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

pub(crate) struct HostNodes {
    pub(crate) sandbox: std::sync::Arc<BubblewrapSandbox>,
    pub(crate) allowed_commands: Vec<String>,
    pub(crate) artifacts: HostArtifacts,
    pub(crate) facts_root: PathBuf,
    pub(crate) io_resolver: std::sync::Arc<HostIoResolver>,
    pub(crate) mcp: tool_host::McpToolConfig,
    pub(crate) io_nodes:
        Option<anchor_io_harness_runtime::node_port::IoHarnessNodePort<HostIoResolver>>,
}

/// Resolves only resources already frozen into a Graph Run. Tool clients and
/// sandbox access are owned by the returned ToolPort so the io-harness worker
/// can bind them inside its dedicated runtime without borrowing HostNodes.
pub(crate) struct HostIoResolver {
    artifacts: HostArtifacts,
    sandbox: std::sync::Arc<BubblewrapSandbox>,
    tools: std::sync::Arc<tool_host::PluginToolHost>,
    mcp: tool_host::McpToolConfig,
    plugin_bindings: BTreeMap<String, PluginBinding>,
}

impl HostIoResolver {
    pub(crate) fn new(
        artifacts: HostArtifacts,
        sandbox: std::sync::Arc<BubblewrapSandbox>,
        tools: std::sync::Arc<tool_host::PluginToolHost>,
        mcp: tool_host::McpToolConfig,
        plugin_bindings: BTreeMap<String, PluginBinding>,
    ) -> Self {
        Self {
            artifacts,
            sandbox,
            tools,
            mcp,
            plugin_bindings,
        }
    }
}

impl NodeHostResolver for HostIoResolver {
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
        let workspace = self
            .artifacts
            .workspace_path(&request.key)
            .map_err(|error| error.to_string())?;
        create_durable_directory(&workspace).map_err(|error| error.to_string())?;
        Ok(workspace)
    }

    fn tools<'a>(&'a self, request: &'a NodeExecutionRequest) -> ToolResolution<'a> {
        Box::pin(async move {
            for binding in &request.plugins {
                let Some(frozen) = self.plugin_bindings.get(&binding.id) else {
                    return Err(format!(
                        "Plugin `{}` is not bound by the admitted bundle",
                        binding.id
                    ));
                };
                if frozen != binding {
                    return Err(format!(
                        "Plugin `{}` differs from the frozen bundle binding",
                        binding.id
                    ));
                }
            }
            let workspace = self.workspace(request)?;
            let readonly_inputs = self
                .artifacts
                .input_mounts(
                    &request.input_commits,
                    &request.key.run_id,
                    &request.key.graph_digest,
                )
                .map_err(|error| error.to_string())?;
            let plugins = self
                .tools
                .assemble(&self.mcp, &request.plugins, request.network)
                .await?;
            Ok(std::sync::Arc::new(crate::node_tools::NodeTools::new(
                plugins,
                std::sync::Arc::clone(&self.sandbox),
                workspace,
                readonly_inputs,
                request.cancellation.clone(),
            ))
                as std::sync::Arc<dyn anchor_runtime_rig::ToolPort>)
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

fn merge_agent_completion_facts(rig: CompletionFact, io_harness: CompletionFact) -> CompletionFact {
    match (&rig, &io_harness) {
        (CompletionFact::NotStarted, _) => io_harness,
        (_, CompletionFact::NotStarted) => rig,
        _ if rig == io_harness => rig,
        _ => CompletionFact::Uncertain(
            "legacy Rig and io-harness node facts conflict; automatic replay is refused".into(),
        ),
    }
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
        let workspace = self.artifacts.workspace_path(&request.key)?;
        create_durable_directory(&workspace)?;
        let readonly_inputs = self.artifacts.input_mounts(
            &request.input_commits,
            &request.key.run_id,
            &request.key.graph_digest,
        )?;
        if !request.plugins.is_empty() || request.network {
            return Err(GraphError::Unsupported(
                "Op.run currently requires no Plugin and network=false".into(),
            ));
        }
        let command = request
            .operation
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| GraphError::Unsupported("Op.run must be a command string".into()))?;
        let argv = shlex::split(command)
            .filter(|args| !args.is_empty())
            .ok_or_else(|| {
                GraphError::Unsupported("Op.run has empty or malformed command arguments".into())
            })?;
        self.start(&request.key)?;
        let result = self
            .sandbox
            .run(SandboxRequest {
                workspace,
                command: argv,
                readonly_inputs,
                workspace_readonly: vec![],
                tool_dirs: vec![],
                environment: vec![],
                network: NetworkPolicy::Disabled,
                timeout: Duration::from_secs_f64(request.wall_time_limit_seconds.unwrap_or(3600.0)),
                max_output_bytes: 64 * 1024,
                spill: None,
                cancellation: request.cancellation.clone(),
            })
            .await
            .map_err(|e| GraphError::Unsupported(e.to_string()))?;
        match result.status {
            SandboxStatus::Completed if result.exit_code == Some(0) => self.complete(&request.key, NodeCompletion {
                submission: "anchor-runner-host".into(), route: None, model_requests: 0,
                output: json!({"stdout":result.stdout,"stderr":result.stderr,"exit_code":result.exit_code}),
            }),
            SandboxStatus::Cancelled => Ok(NodeExecutionOutcome::Cancelled),
            _ => {
                let reason = format!("{} (exit_code={:?})", result.reason, result.exit_code);
                write_durable(&self.facts_root.join(format!("{}.failed", fact_stem(&request.key))), reason.as_bytes())?;
                Ok(NodeExecutionOutcome::Failed { reason })
            }
        }
    }
}

impl NodeExecutionPort for HostNodes {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: self.io_nodes.is_some(),
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
            let rig = load_host_completion_fact(&self.facts_root, key)?;
            let io_harness = match &self.io_nodes {
                Some(nodes) => nodes.completion_fact(key).await?,
                None => CompletionFact::NotStarted,
            };
            Ok(merge_agent_completion_facts(rig, io_harness))
        })
    }
    fn record_recovery_decision(
        &self,
        key: &InvocationKey,
        attempt_id: i64,
        decision: RecoveryDecision,
    ) -> Result<(), GraphError> {
        let nodes = self.io_nodes.as_ref().ok_or_else(|| {
            GraphError::Unsupported("io-harness Agent runtime is not configured".into())
        })?;
        nodes.record_recovery_decision(key, attempt_id, decision)
    }
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>,
    > {
        if request.kind == NodeKind::Agent {
            let Some(nodes) = &self.io_nodes else {
                return Box::pin(async {
                    Err(GraphError::Unsupported(
                        "io-harness Agent runtime requires configured Rig provider credentials/model".into(),
                    ))
                });
            };
            // io-harness owns the durable wall-time budget and observes
            // cancellation at step boundaries. Do not race it with an outer
            // timeout that would detach its blocking worker.
            return nodes.execute(request);
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
mod io_resolver_tests {
    use super::*;
    use anchor_io_harness_runtime::node_port::IoHarnessNodePort;
    use anchor_runtime_rig::graph::{
        GraphRunRecord, GraphRunner, GraphSnapshot, InvocationKey, RunControl, RunStatus, RunStore,
    };
    use anchor_sandbox_bwrap::BubblewrapPolicy;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};
    use serde_json::json;
    use std::sync::atomic::AtomicBool;

    struct NoControl;

    impl RunControl for NoControl {
        fn pause_requested(&self) -> bool {
            false
        }

        fn stop_requested(&self) -> bool {
            false
        }

        fn cancellation(&self) -> anchor_runtime_rig::Cancellation {
            std::sync::Arc::new(AtomicBool::new(false))
        }
    }

    #[tokio::test]
    async fn io_resolver_returns_owned_anchor_tools_from_frozen_plugin_bindings() {
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
        );

        assert_eq!(
            resolver.resolve_plugins(&["fake-tools".into()]).unwrap(),
            bindings
        );
        assert!(resolver.resolve_plugins(&["missing".into()]).is_err());

        let request = NodeExecutionRequest {
            key: InvocationKey {
                run_id: "resolver-test".into(),
                graph_digest: "digest".into(),
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

    #[test]
    fn legacy_rig_facts_are_reused_or_fenced_during_io_harness_migration() {
        let root = tempfile::tempdir().unwrap();
        let key = InvocationKey {
            run_id: "legacy-run".into(),
            graph_digest: "graph".into(),
            node_id: "agent".into(),
            invocation: 1,
        };
        let started = root.path().join(format!("{}.started", fact_stem(&key)));
        std::fs::write(started, b"started\n").unwrap();
        let legacy = load_host_completion_fact(root.path(), &key).unwrap();
        assert!(matches!(legacy, CompletionFact::Uncertain(_)));
        assert!(matches!(
            merge_agent_completion_facts(legacy, CompletionFact::NotStarted),
            CompletionFact::Uncertain(_)
        ));

        let completion = NodeCompletion {
            submission: "already-done".into(),
            route: None,
            model_requests: 1,
            output: serde_json::Value::Null,
        };
        assert_eq!(
            merge_agent_completion_facts(
                CompletionFact::Completed(completion.clone()),
                CompletionFact::NotStarted,
            ),
            CompletionFact::Completed(completion.clone())
        );
        let conflicting = NodeCompletion {
            submission: "different-result".into(),
            ..completion.clone()
        };
        assert!(matches!(
            merge_agent_completion_facts(
                CompletionFact::Completed(completion),
                CompletionFact::Completed(conflicting),
            ),
            CompletionFact::Uncertain(_)
        ));
    }

    #[tokio::test]
    async fn graph_runner_executes_io_agent_with_owned_plugin_and_bubblewrap_tools() {
        let root = tempfile::tempdir().unwrap();
        let workspace_root = root.path().join("workspaces");
        let artifacts_root = root.path().join("artifacts");
        let state_root = root.path().join("state");
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
        let plugin_tools = tool_host::PluginToolHost::new(["fake-tools".to_owned()]);
        let binding = plugin_tools
            .resolve_fixture_plugins(&["fake-tools".into()])
            .unwrap()
            .remove(0);
        let resolver = std::sync::Arc::new(HostIoResolver::new(
            HostArtifacts::new(artifacts_root.clone(), workspace_root),
            std::sync::Arc::clone(&sandbox),
            std::sync::Arc::new(plugin_tools),
            tool_host::McpToolConfig::default(),
            BTreeMap::from([(binding.id.clone(), binding.clone())]),
        ));
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call(
                "echo-1",
                tool_host::FAKE_ECHO_TOOL,
                json!({"value":"ready"}),
            ),
            MockTurn::tool_call(
                "run-1",
                crate::node_tools::RUN_TOOL_NAME,
                json!({"command":["sh","-c","printf sandbox-ok > result.txt"]}),
            ),
            MockTurn::text(r#"{"summary":"host-tools-ok"}"#),
        ])
        .erase();
        let io_nodes = IoHarnessNodePort::new_with_default_policy(
            state_root.join("io-harness/facts"),
            state_root.join("io-harness/store"),
            model,
            std::sync::Arc::clone(&resolver),
        );
        let host_nodes = HostNodes {
            sandbox,
            allowed_commands: vec!["sh".into()],
            artifacts: HostArtifacts::new(artifacts_root.clone(), root.path().join("workspaces")),
            facts_root: state_root.join("facts"),
            io_resolver: resolver,
            mcp: tool_host::McpToolConfig::default(),
            io_nodes: Some(io_nodes),
        };
        let snapshot = GraphSnapshot::admit(json!({
            "objective":"io-host-tool-integration",
            "entry":"work",
            "agents":{"worker":{"model":"fixture","instructions":"Use the provided tools."}},
            "ops":{},
            "nodes":[{"id":"work","agent":"worker","plugins":["fake-tools"]}],
            "edges":[]
        }))
        .unwrap();
        let record =
            GraphRunRecord::create_with_id(snapshot, serde_json::Value::Null, "io-host-run")
                .unwrap();
        let store = anchor_runtime_rig::graph::FileRunStore::new(state_root.join("runs"));
        let artifacts = HostArtifacts::new(artifacts_root, root.path().join("workspaces"));

        let completed = GraphRunner::new(&store, &artifacts, &host_nodes, &NoControl)
            .run(record)
            .await
            .unwrap();
        assert_eq!(completed.status, RunStatus::Completed);
        let commit = &completed.results["work"][0].commit;
        assert!(
            artifacts
                .files_path(commit)
                .unwrap()
                .join("result.txt")
                .is_file()
        );
        assert_eq!(
            std::fs::read(artifacts.files_path(commit).unwrap().join("result.txt")).unwrap(),
            b"sandbox-ok"
        );
    }

    #[tokio::test]
    async fn graph_runner_restart_resumes_io_harness_without_replaying_anchor_run() {
        let root = tempfile::tempdir().unwrap();
        let workspace_root = root.path().join("workspaces");
        let artifacts_root = root.path().join("artifacts");
        let state_root = root.path().join("state");
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
        let plugin_tools =
            std::sync::Arc::new(tool_host::PluginToolHost::new(Vec::<String>::new()));
        let artifacts = HostArtifacts::new(artifacts_root.clone(), workspace_root.clone());
        let resolver = std::sync::Arc::new(HostIoResolver::new(
            artifacts.clone(),
            std::sync::Arc::clone(&sandbox),
            plugin_tools,
            tool_host::McpToolConfig::default(),
            BTreeMap::new(),
        ));
        let build_nodes = |model| HostNodes {
            sandbox: std::sync::Arc::clone(&sandbox),
            allowed_commands: vec!["sh".into()],
            artifacts: artifacts.clone(),
            facts_root: state_root.join("facts"),
            io_resolver: std::sync::Arc::clone(&resolver),
            mcp: tool_host::McpToolConfig::default(),
            io_nodes: Some(IoHarnessNodePort::new_with_default_policy(
                state_root.join("io-harness/facts"),
                state_root.join("io-harness/store"),
                model,
                std::sync::Arc::clone(&resolver),
            )),
        };
        let snapshot = GraphSnapshot::admit(json!({
            "objective":"resume the same io-harness Agent invocation",
            "entry":"work",
            "agents":{"worker":{"model":"fixture","instructions":"Use anchor_run once to append one line containing called to /workspace/effects.txt, then return a JSON summary."}},
            "ops":{},
            "nodes":[{"id":"work","agent":"worker"}],
            "edges":[]
        }))
        .unwrap();
        let record = GraphRunRecord::create_with_id(
            snapshot,
            serde_json::Value::Null,
            "io-harness-restart-run",
        )
        .unwrap();
        let store = anchor_runtime_rig::graph::FileRunStore::new(state_root.join("runs"));
        let command = json!({
            "command":["sh","-c","printf 'called\\n' >> effects.txt"]
        });
        let interrupted_nodes = build_nodes(
            MockCompletionModel::from_turns([MockTurn::tool_call(
                "run-once",
                crate::node_tools::RUN_TOOL_NAME,
                command,
            )])
            .erase(),
        );

        let first = GraphRunner::new(&store, &artifacts, &interrupted_nodes, &NoControl)
            .run(record)
            .await;
        assert!(
            first.is_err(),
            "the missing scripted provider turn simulates interruption"
        );
        let interrupted = store
            .load("io-harness-restart-run")
            .unwrap()
            .expect("Graph Run cursor must be durable before Agent dispatch");
        assert_eq!(interrupted.status, RunStatus::Running);
        assert_eq!(interrupted.cursor.as_ref().unwrap().key.invocation, 1);
        assert!(matches!(
            interrupted_nodes
                .completion_fact(&interrupted.cursor.as_ref().unwrap().key)
                .await
                .unwrap(),
            CompletionFact::Resumable
        ));

        // A new host/provider instance re-enters the same Harness run. Since
        // the failed provider call followed a mutating tool, io-harness requires
        // an explicit recovery decision; it must not replay the tool.
        let restarted_nodes =
            build_nodes(MockCompletionModel::text(r#"{"summary":"resumed"}"#).erase());
        let resumed = GraphRunner::new(&store, &artifacts, &restarted_nodes, &NoControl)
            .run(interrupted)
            .await;
        assert!(matches!(
            resumed,
            Err(anchor_runtime_rig::graph::GraphError::Unsupported(message))
                if message.contains("requires recovery")
        ));
        let still_pending = store
            .load("io-harness-restart-run")
            .unwrap()
            .expect("recovery-required Run must retain its cursor");
        assert_eq!(still_pending.status, RunStatus::Running);
        assert_eq!(still_pending.invocations["work"], 1);
        let workspace = artifacts
            .workspace_path(&still_pending.cursor.unwrap().key)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.join("effects.txt")).unwrap(),
            "called\n"
        );
    }
}
