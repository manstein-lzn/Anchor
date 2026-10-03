//! AgentNode command tools delegated to the host-authorized Bubblewrap adapter.

use anchor_runtime_rig::{
    Cancellation, NetworkPolicy, ReadOnlyInput, SandboxError, SandboxPort, SandboxRequest,
    SandboxStatus, ToolError, ToolPort,
};
use anchor_sandbox_bwrap::BubblewrapSandbox;
use rig_agent::core::{
    completion::ToolDefinition,
    message::{ToolName, ToolResultContent},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub(crate) const RUN_TOOL_NAME: &str = "anchor_run";

/// AgentNode tools own their Plugin port and sandbox adapter so they can cross
/// the async resolver boundary into the io-harness worker runtime.
pub(crate) struct NodeTools {
    inner: Arc<dyn ToolPort>,
    sandbox: Arc<BubblewrapSandbox>,
    workspace: PathBuf,
    readonly_inputs: Vec<ReadOnlyInput>,
    cancellation: Cancellation,
}

impl NodeTools {
    pub fn new(
        inner: Arc<dyn ToolPort>,
        sandbox: Arc<BubblewrapSandbox>,
        workspace: PathBuf,
        readonly_inputs: Vec<ReadOnlyInput>,
        cancellation: Cancellation,
    ) -> Self {
        Self {
            inner,
            sandbox,
            workspace,
            readonly_inputs,
            cancellation,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArguments {
    command: Vec<String>,
}

fn not_executed(error: impl Into<String>) -> Vec<ToolResultContent> {
    vec![ToolResultContent::json(json!({
        "status": "not_executed",
        "error": error.into()
    }))]
}

impl ToolPort for NodeTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.inner.definitions();
        definitions.push(ToolDefinition::new(
            ToolName::new(RUN_TOOL_NAME).expect("static tool name"),
            "Run an authorized command as an argv array in the node sandbox. The working directory is /workspace; node inputs are read-only under /in.",
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "array", "items": {"type": "string"}, "minItems": 1}
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        ));
        definitions
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if name != RUN_TOOL_NAME {
                return self.inner.call(name, arguments).await;
            }
            if self.cancellation.load(Ordering::Relaxed) {
                return Err(ToolError::Failed("anchor_run was cancelled".into()));
            }
            let arguments: RunArguments = match serde_json::from_value(arguments) {
                Ok(arguments) => arguments,
                Err(error) => {
                    return Ok(not_executed(format!(
                        "invalid anchor_run arguments: {error}"
                    )));
                }
            };
            let result = match self
                .sandbox
                .run(SandboxRequest {
                    workspace: self.workspace.clone(),
                    command: arguments.command,
                    readonly_inputs: self.readonly_inputs.clone(),
                    workspace_readonly: Vec::new(),
                    tool_dirs: Vec::new(),
                    environment: Vec::new(),
                    network: NetworkPolicy::Disabled,
                    timeout: Duration::from_secs(30),
                    max_output_bytes: 64 * 1024,
                    spill: None,
                    cancellation: self.cancellation.clone(),
                })
                .await
            {
                Ok(result) => result,
                // This error is raised by the adapter before any process
                // launch, so the model can safely correct its command.
                Err(SandboxError::InvalidRequest(reason)) => return Ok(not_executed(reason)),
                Err(error) => return Err(ToolError::Failed(error.to_string())),
            };
            let status = match result.status {
                SandboxStatus::Completed => "completed",
                SandboxStatus::TimedOut => "timed_out",
                SandboxStatus::Cancelled => {
                    return Err(ToolError::Failed("anchor_run was cancelled".into()));
                }
                SandboxStatus::NotExecuted => {
                    return Err(ToolError::Failed("anchor_run was not executed".into()));
                }
            };
            Ok(vec![ToolResultContent::json(json!({
                "status": status,
                "exit_code": result.exit_code,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "incomplete": result.incomplete
            }))])
        })
    }
}

#[cfg(test)]
mod tests;
