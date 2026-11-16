//! Composition of Host-backed channel/task adapters and sandbox-local tools.
use crate::{
    application::RunApplication,
    node_host::{NodeToolAssembly, ToolAssembly},
};
use anchor_runtime::{NetworkPolicy, ToolPort};
use std::{future::Future, pin::Pin, sync::Arc};

pub(crate) fn new(application: RunApplication) -> Arc<dyn NodeToolAssembly> {
    Arc::new(Assembly(application))
}

struct Assembly(RunApplication);
impl NodeToolAssembly for Assembly {
    fn prepare_services<'a>(
        &'a self,
        request: &'a anchor_runtime::graph::NodeExecutionRequest,
        bindings: &'a [anchor_runtime::graph::PluginBinding],
        mcp: &'a crate::tool_host::McpToolConfig,
        network: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<String>> + Send + 'a>> {
        Box::pin(async move {
            if !network
                || !mcp.has_local_codex_bridge()
                || request
                    .cancellation
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return Vec::new();
            }
            let Some(grant) = self.0.authorized_codex_bridge(&request.key, bindings) else {
                return Vec::new();
            };
            // Dependency startup is idempotent; no Codex task or message is replayed.
            if let Err(reason) = grant.service.start().await {
                eprintln!("Codex Bridge dependency unavailable: {reason}");
            }
            vec!["codex-codex".into()]
        })
    }
    fn authorize_assistant_input<'a>(
        &'a self,
        key: &'a anchor_runtime::graph::InvocationKey,
        input: &'a anchor_platform_session::AssistantInput,
    ) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + 'a>> {
        Box::pin(async move {
            use anchor_platform_session::{AssistantInput, SessionError, TurnStatus};
            let AssistantInput::Task(task) = input else {
                return Ok(true);
            };
            if task.result.content.get("subscription").is_none() {
                return Ok(true);
            }
            // Completed Turns are historical receipts, not new consumption. Later
            // revocation must not turn their recorded completion into a yield.
            if task.turn.status == TurnStatus::Completed {
                return Ok(true);
            }
            // Publication uses temporary files and replaces the Graph before its
            // permission receipt. Wait for that same catalog boundary to settle.
            let _catalog = self.0.graph_catalog_mutation_guard().await;
            let source = crate::assistant::source(&self.0.data_root, key)?
                .ok_or("subscription input has no trusted assistant source")?;
            let sessions = crate::assistant::sessions(&self.0.data_root)?;
            if sessions
                .get_turn(&source.owner, &source.session, &task.turn.id)
                .map_err(|error| error.to_string())?
                .status
                == TurnStatus::Completed
            {
                return Ok(true);
            }
            match self
                .0
                .authorize_subscription_result(&source.owner, &source.session, &task.result)
            {
                Ok(()) => Ok(true),
                Err(crate::application::ApplicationError::Storage(error)) => Err(error),
                Err(_) => {
                    sessions
                        .cancel_task_result(
                            &source.owner,
                            &source.session,
                            &task.result.id,
                            "subscription authority withdrawn",
                        )
                        .map_err(|error| error.to_string())?;
                    if sessions
                        .get_turn(&source.owner, &source.session, &task.turn.id)
                        .map_err(|error| error.to_string())?
                        .status
                        == TurnStatus::Running
                    {
                        match sessions.finish_turn(
                            &source.owner,
                            &source.session,
                            &task.turn.id,
                            TurnStatus::Stopped,
                            None,
                        ) {
                            Ok(_) => {}
                            Err(SessionError::Conflict(_))
                                if sessions
                                    .get_turn(&source.owner, &source.session, &task.turn.id)
                                    .map_err(|error| error.to_string())?
                                    .status
                                    != TurnStatus::Running => {}
                            Err(error) => return Err(error.to_string()),
                        }
                    }
                    Ok(false)
                }
            }
        })
    }
    fn assemble(&self, input: ToolAssembly<'_>) -> Result<Arc<dyn ToolPort>, String> {
        let application = match input.runtime {
            Some(snapshot) => self.0.clone().with_frozen_runtime_snapshot(snapshot),
            None => self.0.clone(),
        };
        let commands_allowed = application
            .task_commands_allowed(&input.request.key.run_id)
            .map_err(|error| format!("task command authorization: {error:?}"))?;
        let spill = crate::node_tools::SpillDirectory::new(
            application
                .data_root
                .join("spill")
                .join(&input.request.key.run_id)
                .join(
                    input
                        .workspace
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| input.request.key.node_id.clone()),
                ),
            "/spill",
        );
        let plugins = if input.envelope.network == NetworkPolicy::Enabled
            && input.mcp.has_local_codex_bridge()
        {
            let authority = application.clone();
            let key = input.request.key.clone();
            let bindings = input.bindings.to_vec();
            crate::codex_bridge_tools::wrap(
                input.plugins,
                Arc::new(move || authority.authorized_codex_bridge(&key, &bindings)),
                input.request.cancellation.clone(),
                input.disclosure,
            )
        } else {
            input.plugins
        };
        let plugins = crate::channel_tools::wrap(
            plugins,
            input.bindings,
            input.request.key.clone(),
            application.clone(),
            input.workspace.into(),
            input.readonly_inputs.into(),
            input.request.cancellation.clone(),
        );
        let plugins = crate::graph_task_tools::wrap(
            plugins,
            input.bindings,
            input.request.key.clone(),
            application.clone(),
            crate::application::plugin_candidates::CandidateTestContext {
                sandbox: input.envelope.sandbox.clone(),
                workspace: input.workspace.into(),
                access: crate::dependency_provision::DependencyAccess {
                    network: input.envelope.network == NetworkPolicy::Enabled,
                    cancellation: input.request.cancellation.clone(),
                },
            },
            input.workspace_root.into(),
            input.disclosure,
        );
        let plugins = match application.authorized_project(&input.request.key) {
            Ok(grant) => {
                let sandbox = input
                    .envelope
                    .command_sandbox()
                    .with_project_workspace(&grant.path, &grant.exclude)
                    .map_err(|error| error.to_string())?;
                let tools = crate::node_tools::NodeTools::new(
                    Arc::new(crate::project_tools::NoTools),
                    Arc::new(sandbox),
                    grant.path.clone(),
                    vec![],
                    input.request.cancellation.clone(),
                )
                .with_environment(input.envelope.environment.clone())
                .with_network(input.envelope.network == NetworkPolicy::Enabled)
                .with_timeout_ceiling(input.envelope.timeout_ceiling)
                .with_excluded_paths(grant.exclude.clone())
                .with_command_authority(commands_allowed);
                let key = input.request.key.clone();
                crate::project_tools::wrap(
                    plugins,
                    Arc::new(tools),
                    grant,
                    Arc::new(move || application.authorized_project(&key)),
                    input.request.cancellation.clone(),
                )
            }
            Err(_) => plugins,
        };
        Ok(Arc::new(
            crate::node_tools::NodeTools::new(
                plugins,
                input.envelope.sandbox.clone(),
                input.workspace.into(),
                input.readonly_inputs.into(),
                input.request.cancellation.clone(),
            )
            .with_spill(spill)
            .with_envelope(input.envelope)
            .with_command_authority(commands_allowed),
        ))
    }
}
