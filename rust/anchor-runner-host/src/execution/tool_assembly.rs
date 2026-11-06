//! Composition of Host-backed channel/task adapters and sandbox-local tools.
use crate::{
    application::RunApplication,
    node_host::{NodeToolAssembly, ToolAssembly},
};
use anchor_runtime::{NetworkPolicy, ToolPort};
use std::sync::Arc;

pub(crate) fn new(application: RunApplication) -> Arc<dyn NodeToolAssembly> {
    Arc::new(Assembly(application))
}

struct Assembly(RunApplication);
impl NodeToolAssembly for Assembly {
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
        let plugins = crate::channel_tools::wrap(
            input.plugins,
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
            application,
            crate::application::plugin_candidates::CandidateTestContext {
                sandbox: input.envelope.sandbox.clone(),
                workspace: input.workspace.into(),
                access: crate::dependency_provision::DependencyAccess {
                    network: input.envelope.network == NetworkPolicy::Enabled,
                    cancellation: input.request.cancellation.clone(),
                },
            },
            input.disclosure,
        );
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
