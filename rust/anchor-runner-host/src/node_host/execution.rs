//! Agent/Op dispatch and sandbox outcome settlement.
use super::*;

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
        // Kernel scheduler wraps the frozen Run input with upstream artifact metadata.
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
        let environment = self
            .io_resolver
            .tool_environment()
            .map_err(GraphError::Unsupported)?;
        let sandbox = environment
            .grant_sandbox(&self.sandbox)
            .map_err(GraphError::Unsupported)?
            .with_readonly_grants(&local_inputs)
            .and_then(|sandbox| sandbox.with_readonly_grants(std::slice::from_ref(&route_helper)))
            .map_err(|error| GraphError::Unsupported(error.to_string()))?;
        let network = self
            .io_resolver
            .mounted_plugins(&request)
            .map_err(GraphError::Unsupported)?
            .network;
        let mut sandbox_request = SandboxRequest {
            writable_inputs: Vec::new(),
            workspace: workspace.clone(),
            working_directory: None,
            command: argv,
            readonly_inputs,
            workspace_readonly: vec![],
            tool_dirs: vec![],
            devices: Vec::new(),
            environment: vec![
                anchor_runtime::SandboxEnvironment::new("ANCHOR_NODE", request.key.node_id.clone()),
                anchor_runtime::SandboxEnvironment::new("ANCHOR_ROUTES", request.routes.join(",")),
                anchor_runtime::SandboxEnvironment::new("ANCHOR_INPUT", input),
            ],
            network: if network {
                NetworkPolicy::Enabled
            } else {
                NetworkPolicy::Disabled
            },
            timeout: Duration::from_secs_f64(request.wall_time_limit_seconds.unwrap_or(3600.0)),
            max_output_bytes: 64 * 1024,
            spill: None,
            cancellation: request.cancellation.clone(),
        };
        // The deployment's operator-registered tool paths are this invocation's
        // authority: an Op.run reaches them by absolute path, and nothing else
        // grants them.
        op::add_route_helper(&mut sandbox_request, route_helper);
        environment.apply(&mut sandbox_request);
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
                let selection =
                    anchor_runtime::artifact::ArtifactSelection::new(request.writes.clone())
                        .map_err(GraphError::Unsupported)?;
                if let Err(reason) =
                    crate::artifacts::validate_selection(&workspace, &selection, &request.writes)
                {
                    write_durable(
                        &self
                            .facts_root
                            .join(format!("{}.failed", fact_stem(&request.key))),
                        reason.as_bytes(),
                    )?;
                    return Ok(NodeExecutionOutcome::Failed { reason });
                }
                let (submission, route) = read_op_route(&result.stdout);
                self.complete(&request.key, NodeCompletion {
                    submission,
                    route,
                    model_requests: 0,
                    output: json!({"stdout":result.stdout,"stderr":result.stderr,"exit_code":result.exit_code,"files":selection.paths}),
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
            host_operations: true,
            exact_provider_request_budget: false,
        }
    }
    fn resolve_plugins(&self, ids: &[String]) -> Result<Vec<PluginBinding>, GraphError> {
        self.io_resolver
            .resolve_plugins(ids)
            .map_err(GraphError::Unsupported)
    }
    fn invocation_plugin_authority(&self, record: &GraphRunRecord) -> Result<bool, GraphError> {
        crate::assistant_plugins::invocation_authority(
            self.io_resolver.local_inputs.state_root(),
            record,
        )
        .map_err(GraphError::Unsupported)
    }
    fn completion_fact<'a>(
        &'a self,
        key: &'a InvocationKey,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>>
    {
        Box::pin(async move {
            if let Some(fact) = self.assistant_completion_fact(key)? {
                return Ok(fact);
            }
            let host = load_host_completion_fact(&self.facts_root, key)?;
            if let CompletionFact::Completed(completion) = &host {
                self.settle_assistant_reply(key, completion)?;
            }
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
        key: &InvocationKey,
        attempt_id: i64,
        decision: RecoveryDecision,
    ) -> Result<(), GraphError> {
        self.goose_nodes
            .as_ref()
            .ok_or_else(|| {
                GraphError::CorruptRun("native recovery backend is not configured".into())
            })?
            .record_recovery_decision(key, attempt_id, decision)
    }
    fn observed_plugin_views(&self, key: &InvocationKey) -> BTreeMap<String, String> {
        self.io_resolver.observed_plugin_views(key)
    }
    fn observed_plugin_digests(&self, key: &InvocationKey) -> BTreeMap<String, String> {
        self.io_resolver.observed_plugin_digests(key)
    }
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let key = request.key.clone();
            let inputs = request.input_commits.clone();
            self.io_resolver
                .prepare_node_workspace(&key, &inputs)
                .map_err(GraphError::Unsupported)?;
            let lease = self.artifacts.workspaces().acquire(&key)?;
            let input_lease = self.artifacts.lease_inputs(&inputs)?;
            let outcome = self.execute_dispatch(request).await;
            drop(input_lease);
            match &outcome {
                Ok(NodeExecutionOutcome::Completed(completion)) => {
                    self.artifacts.publish_owned(
                        &key,
                        completion,
                        &anchor_runtime::graph::ArtifactFreezeContext {
                            kind: anchor_runtime::graph::ArtifactKind::Node,
                            input_commits: inputs,
                        },
                    )?;
                    lease.finish_published()?;
                }
                Ok(
                    NodeExecutionOutcome::Cancelled
                    | NodeExecutionOutcome::Suspended
                    | NodeExecutionOutcome::Failed { .. }
                    | NodeExecutionOutcome::Yielded { .. }
                    | NodeExecutionOutcome::BudgetExhausted { .. }
                    | NodeExecutionOutcome::Aborted,
                ) => lease.finish_interrupted()?,
                // The same invocation may inspect and resume. A different
                // writer cannot take an unresolved scene over.
                _ => (),
            }
            outcome
        })
    }
}

impl HostNodes {
    fn execute_dispatch<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>,
    > {
        if let Some(nodes) = &self.goose_nodes {
            match nodes.pending_recovery(&request.key) {
                Ok(attempts) if !attempts.is_empty() => {
                    return Box::pin(async move {
                        Ok(NodeExecutionOutcome::WaitingRecovery { attempts })
                    });
                }
                Err(error) => return Box::pin(async move { Err(error) }),
                _ => {}
            }
        }
        if request.kind == NodeKind::OpHost {
            return Box::pin(self.execute_host_operation(request));
        }
        match crate::assistant::source(self.io_resolver.local_inputs.state_root(), &request.key) {
            Ok(Some(_)) => return Box::pin(self.execute_assistant_request(request)),
            Ok(None) => {}
            Err(error) => return Box::pin(async move { Err(GraphError::Unsupported(error)) }),
        }
        self.execute_regular(request)
    }
}

impl HostNodes {
    pub(super) fn execute_regular<'a>(
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
        // Sandbox owns timeout/cancellation and waits for process cleanup.
        Box::pin(self.execute_node(request))
    }
}
