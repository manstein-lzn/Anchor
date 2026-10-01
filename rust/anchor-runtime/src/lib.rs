//! Experimental Rust runtime kernel for Anchor.
//!
//! Rig owns the serializable agent state machine; Anchor owns node identity,
//! provider selection, tools and cancellation. No graph scheduler or platform
//! API is included here.

use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use rig_agent::{
    core::{
        DynModel,
        completion::{
            CompletionRequest, CompletionResponse, Message, ProviderCapabilities, ToolDefinition,
        },
        message::{ToolResultContent, UserContent},
        operation::Completion,
    },
    run::{AgentRun, AgentRunStep, ModelTurn, RunSpec, prepare::prepare_request},
};
use serde::{Deserialize, Serialize};

pub mod checkpoint;
pub use checkpoint::{CheckpointStore, CheckpointStoreError, FileCheckpointStore};

const CHECKPOINT_FORMAT: u32 = 2;

/// A cancellation flag owned by the host.
pub type Cancellation = Arc<AtomicBool>;

/// Minimal input contract for one AgentNode invocation.
#[derive(Debug, Clone)]
pub struct NodeRequest {
    pub execution_id: String,
    pub task: String,
    pub instructions: String,
    pub routes: Vec<String>,
    pub max_turns: usize,
    pub workspace: PathBuf,
    pub cancellation: Cancellation,
}

impl NodeRequest {
    pub fn prompt(&self) -> String {
        let routes = if self.routes.is_empty() {
            "(no route)".to_owned()
        } else {
            self.routes.join(", ")
        };
        format!(
            "Task:\n{}\n\nInstructions:\n{}\n\nAllowed routes: {}\n\nReturn a JSON object with `summary` and an optional `route`. The route must be one of the allowed routes.",
            self.task, self.instructions, routes
        )
    }
}

/// Durable identity and Rig state for one AgentNode attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCheckpoint {
    format: u32,
    pub node_id: String,
    pub invocation: u32,
    pub run_spec: RunSpec,
    pub run: AgentRun,
}

impl AgentCheckpoint {
    pub fn start(
        node_id: impl Into<String>,
        invocation: u32,
        task: impl Into<String>,
        max_turns: usize,
    ) -> Self {
        let mut run_spec = RunSpec::new();
        run_spec.max_turns = Some(max_turns);
        Self {
            format: CHECKPOINT_FORMAT,
            node_id: node_id.into(),
            invocation,
            run_spec,
            run: AgentRun::new(Message::user(task.into())).max_turns(max_turns),
        }
    }

    /// Build a checkpoint from the node boundary without persisting host-only
    /// handles such as the cancellation flag or workspace path.
    pub fn from_request(
        node_id: impl Into<String>,
        invocation: u32,
        request: &NodeRequest,
    ) -> Self {
        Self::start(node_id, invocation, request.prompt(), request.max_turns)
    }

    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn decode(
        bytes: &[u8],
        expected_node: &str,
        expected_invocation: u32,
    ) -> Result<Self, CheckpointError> {
        let checkpoint: Self = serde_json::from_slice(bytes)?;
        if checkpoint.format != CHECKPOINT_FORMAT {
            return Err(CheckpointError::UnsupportedFormat(checkpoint.format));
        }
        if checkpoint.node_id != expected_node || checkpoint.invocation != expected_invocation {
            return Err(CheckpointError::IdentityMismatch);
        }
        Ok(checkpoint)
    }
}

/// Port used by the node executor to send a prepared Rig request.
pub trait CompletionPort: Send + Sync {
    fn capabilities(&self) -> ProviderCapabilities;
    fn complete<'a>(
        &'a self,
        request: CompletionRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<CompletionResponse, rig_agent::core::error::ProviderError>>
                + Send
                + 'a,
        >,
    >;
}

/// Adapter for any Rig completion model.
#[derive(Clone)]
pub struct RigCompletionPort {
    model: DynModel<Completion>,
    capabilities: ProviderCapabilities,
}

impl RigCompletionPort {
    pub fn new(model: DynModel<Completion>) -> Self {
        Self {
            capabilities: ProviderCapabilities::default(),
            model,
        }
    }

    pub fn with_capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Construct an OpenAI-compatible client from host-supplied credentials.
    /// The credential is held only by Rig's live client and is never part of a
    /// checkpoint. `wire_api` accepts `chat` or `responses`.
    pub fn openai_compatible(
        api_key: impl Into<rig_core::wire::Secret>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        wire_api: &str,
    ) -> Result<Self, ProviderConfigError> {
        let config =
            rig_core::providers::openai::OpenAIConfig::new(api_key).with_base_url(base_url);
        let client = config.client();
        let model = model.into();
        let model = match wire_api {
            "chat" => client.chat(model).erase(),
            "responses" => client.responses(model).erase(),
            other => return Err(ProviderConfigError::UnsupportedWire(other.to_owned())),
        };
        Ok(Self::new(model))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderConfigError {
    #[error("unsupported Rig provider wire `{0}`; expected `chat` or `responses`")]
    UnsupportedWire(String),
}

impl CompletionPort for RigCompletionPort {
    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities
    }

    fn complete<'a>(
        &'a self,
        request: CompletionRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<CompletionResponse, rig_agent::core::error::ProviderError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(self.model.call(request))
    }
}

/// Tool execution port. Implementations can enforce Anchor's sandbox,
/// credentials and idempotency policy outside the runtime kernel.
pub trait ToolPort: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;
    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>>;
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool `{0}` is not registered")]
    Unknown(String),
    #[error("tool failed: {0}")]
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeOutcome {
    pub status: NodeStatus,
    pub submission: String,
    pub route: Option<String>,
    pub model_requests: usize,
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("rig run failed: {0}")]
    Rig(Box<rig_agent::run::PromptError>),
    #[error("model request failed: {0}")]
    Provider(Box<rig_agent::core::error::ProviderError>),
    #[error("tool `{name}` failed: {source}")]
    Tool { name: String, source: ToolError },
    #[error("structured AgentNode result is invalid: {0}")]
    InvalidResult(String),
}

impl From<rig_agent::run::PromptError> for NodeError {
    fn from(error: rig_agent::run::PromptError) -> Self {
        Self::Rig(Box::new(error))
    }
}

impl From<rig_agent::core::error::ProviderError> for NodeError {
    fn from(error: rig_agent::core::error::ProviderError) -> Self {
        Self::Provider(Box::new(error))
    }
}

/// Provider-free AgentNode driver. Hosts may persist the checkpoint at each
/// boundary and resume it without serializing a provider client or tool handler.
pub struct NodeExecutor;

impl NodeExecutor {
    pub async fn execute<C: CompletionPort, T: ToolPort>(
        checkpoint: &mut AgentCheckpoint,
        completion: &C,
        tools: &T,
        cancellation: &Cancellation,
        routes: &[String],
    ) -> Result<NodeOutcome, NodeError> {
        let mut requests = 0;
        loop {
            if cancellation.load(Ordering::Relaxed) {
                return Ok(NodeOutcome {
                    status: NodeStatus::Cancelled,
                    submission: String::new(),
                    route: None,
                    model_requests: requests,
                    reason: "cancelled by host".to_owned(),
                });
            }
            match checkpoint.run.next_step()? {
                AgentRunStep::CallModel {
                    prompt,
                    history,
                    turn,
                } => {
                    let definitions = tools.definitions();
                    checkpoint.run.advertise_tools(turn, definitions.clone());
                    let prepared = prepare_request(
                        &checkpoint.run_spec,
                        &completion.capabilities(),
                        &history,
                        definitions,
                        None,
                        None,
                    )
                    .map_err(|error| NodeError::InvalidResult(error.to_string()))?;
                    let request = prepared.clone().apply(CompletionRequest::new(prompt));
                    let response = completion.complete(request).await?;
                    requests += 1;
                    let turn = ModelTurn::from_response(&response, &prepared);
                    match checkpoint.run.model_response(turn)? {
                        rig_agent::run::ModelTurnOutcome::Continue { .. }
                        | rig_agent::run::ModelTurnOutcome::TurnRetried => {}
                        rig_agent::run::ModelTurnOutcome::NeedsResolution(context) => {
                            return Err(NodeError::InvalidResult(format!(
                                "model requested an unavailable tool: {context:?}"
                            )));
                        }
                    }
                }
                AgentRunStep::CallTools { calls } => {
                    let mut results = Vec::with_capacity(calls.len());
                    for call in calls {
                        if cancellation.load(Ordering::Relaxed) {
                            return Ok(NodeOutcome {
                                status: NodeStatus::Cancelled,
                                submission: String::new(),
                                route: None,
                                model_requests: requests,
                                reason: "cancelled before tool execution".to_owned(),
                            });
                        }
                        let result = if let Some(result) = call.preresolved_result {
                            result
                        } else {
                            let value = tools
                                .call(
                                    &call.tool_call.function.name,
                                    call.tool_call.function.arguments.clone(),
                                )
                                .await
                                .map_err(|source| NodeError::Tool {
                                    name: call.tool_call.function.name.to_string(),
                                    source,
                                })?;
                            UserContent::tool_result(
                                call.tool_call.id,
                                call.tool_call.function.name,
                                value,
                            )
                        };
                        results.push(result);
                    }
                    checkpoint.run.tool_results(results)?;
                }
                AgentRunStep::Done(response) => {
                    return parse_outcome(response.output, requests, routes);
                }
            }
        }
    }
}

fn parse_outcome(
    output: String,
    model_requests: usize,
    routes: &[String],
) -> Result<NodeOutcome, NodeError> {
    #[derive(Deserialize)]
    struct ResultShape {
        summary: String,
        route: Option<String>,
    }
    let parsed: ResultShape = serde_json::from_str(&output)
        .map_err(|error| NodeError::InvalidResult(error.to_string()))?;
    if let Some(route) = &parsed.route
        && !routes.iter().any(|allowed| allowed == route)
    {
        return Err(NodeError::InvalidResult(format!(
            "route `{route}` is not allowed"
        )));
    }
    Ok(NodeOutcome {
        status: NodeStatus::Completed,
        submission: parsed.summary,
        route: parsed.route,
        model_requests,
        reason: "model completed".to_owned(),
    })
}

/// A checkpoint could not be parsed or did not belong to the requested node attempt.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("unsupported Anchor checkpoint format: {0}")]
    UnsupportedFormat(u32),
    #[error("checkpoint identity does not match the requested node invocation")]
    IdentityMismatch,
    #[error(transparent)]
    Decode(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests;
