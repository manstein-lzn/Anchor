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
    time::Duration,
};

use futures::StreamExt;
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
pub mod sandbox;
pub use checkpoint::{CheckpointStore, CheckpointStoreError, FileCheckpointStore};
pub use sandbox::{
    NetworkPolicy, NoopSandbox, ReadOnlyInput, SandboxError, SandboxPort, SandboxRequest,
    SandboxResult, SandboxStatus,
};

const CHECKPOINT_FORMAT: u32 = 3;

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
    /// The last protocol step issued by the driver. Rig keeps the state
    /// machine's pending phase private; retaining the issued step lets a host
    /// re-submit a model request after a process dies before its response.
    #[serde(default)]
    pending_step: Option<AgentRunStep>,
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
            pending_step: None,
        }
    }

    pub fn pending_step(&self) -> Option<&AgentRunStep> {
        self.pending_step.as_ref()
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

/// Host-owned request/response observation hook. It receives metadata and
/// normalized provider outcomes, never credentials or prompt contents.
pub trait CompletionObserver: Send + Sync {
    fn request_started(&self, provider: &str);
    fn request_finished(&self, provider: &str, response: &CompletionResponse);
    fn request_failed(&self, provider: &str, error: &rig_agent::core::error::ProviderError);
}

/// Decorator that adds observation without changing the CompletionPort
/// contract or making the kernel depend on a logging/telemetry framework.
pub struct ObservedCompletionPort<P, O> {
    inner: P,
    observer: Arc<O>,
    provider: String,
}

impl<P, O> ObservedCompletionPort<P, O> {
    pub fn new(inner: P, provider: impl Into<String>, observer: Arc<O>) -> Self {
        Self {
            inner,
            observer,
            provider: provider.into(),
        }
    }
}

impl<P, O> CompletionPort for ObservedCompletionPort<P, O>
where
    P: CompletionPort,
    O: CompletionObserver + 'static,
{
    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
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
        let provider = self.provider.clone();
        let observer = Arc::clone(&self.observer);
        Box::pin(async move {
            observer.request_started(&provider);
            match self.inner.complete(request).await {
                Ok(response) => {
                    observer.request_finished(&provider, &response);
                    Ok(response)
                }
                Err(error) => {
                    observer.request_failed(&provider, &error);
                    Err(error)
                }
            }
        })
    }
}

/// Optional streaming extension. The complete response is still returned to
/// the AgentRun only after the stream reaches its provider finish event.
pub trait StreamingCompletionPort: CompletionPort {
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<rig_core::streaming::CompletionStream, Box<rig_agent::core::error::ProviderError>>;
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

impl StreamingCompletionPort for RigCompletionPort {
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<rig_core::streaming::CompletionStream, Box<rig_agent::core::error::ProviderError>>
    {
        self.model.stream(request).map_err(Box::new)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StreamingError {
    #[error("stream provider failed: {0}")]
    Provider(Box<rig_agent::core::error::ProviderError>),
    #[error("stream cancelled by host")]
    Cancelled,
    #[error("stream timed out after {0:?}")]
    Timeout(Duration),
}

impl From<rig_agent::core::error::ProviderError> for StreamingError {
    fn from(error: rig_agent::core::error::ProviderError) -> Self {
        Self::Provider(Box::new(error))
    }
}

/// Consume one provider stream while exposing normalized Rig events to the
/// host. Dropping or cancelling before `finish` leaves the caller's checkpoint
/// pending; no partial response is committed to `AgentRun`.
pub async fn stream_completion<P: StreamingCompletionPort>(
    provider: &P,
    request: CompletionRequest,
    cancellation: &Cancellation,
    timeout: Option<Duration>,
    mut observe: impl FnMut(&rig_core::streaming::StreamEvent),
) -> Result<CompletionResponse, StreamingError> {
    let future = async {
        let mut stream = provider.stream(request).map_err(|error| *error)?;
        loop {
            let Some(item) = stream.next().await else {
                break;
            };
            match item? {
                rig_core::streaming::Item::Event(event) => observe(&event),
                rig_core::streaming::Item::Unknown(_) => {}
            }
            if cancellation.load(Ordering::Relaxed) {
                return Err(StreamingError::Cancelled);
            }
        }
        Ok(stream.finish().await?)
    };
    tokio::pin!(future);
    let cancel = async {
        while !cancellation.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }
    };
    tokio::pin!(cancel);
    match timeout {
        Some(timeout) => {
            tokio::select! {
                result = &mut future => result,
                _ = &mut cancel => Err(StreamingError::Cancelled),
                _ = tokio::time::sleep(timeout) => Err(StreamingError::Timeout(timeout)),
            }
        }
        None => {
            tokio::select! {
                result = &mut future => result,
                _ = &mut cancel => Err(StreamingError::Cancelled),
            }
        }
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

/// Optional host policy for one node attempt. `None` preserves provider/tool
/// behavior; a timeout drops the in-flight future and leaves its pending step
/// in the checkpoint for a deliberate recovery decision.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExecutionPolicy {
    pub model_timeout: Option<Duration>,
    pub tool_timeout: Option<Duration>,
}

struct ExecutionContext<'a> {
    policy: ExecutionPolicy,
    cancellation: &'a Cancellation,
    routes: &'a [String],
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
    #[error("checkpoint persistence failed: {0}")]
    Checkpoint(Box<CheckpointStoreError>),
    #[error("{phase} timed out after {timeout:?}")]
    Timeout {
        phase: &'static str,
        timeout: Duration,
    },
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

impl From<CheckpointStoreError> for NodeError {
    fn from(error: CheckpointStoreError) -> Self {
        Self::Checkpoint(Box::new(error))
    }
}

struct NoopCheckpointStore;

impl CheckpointStore for NoopCheckpointStore {
    fn save(&self, _key: &str, _checkpoint: &AgentCheckpoint) -> Result<(), CheckpointStoreError> {
        Ok(())
    }

    fn load(
        &self,
        _key: &str,
        _expected_node: &str,
        _expected_invocation: u32,
    ) -> Result<Option<AgentCheckpoint>, CheckpointStoreError> {
        Ok(None)
    }

    fn delete(&self, _key: &str) -> Result<(), CheckpointStoreError> {
        Ok(())
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
        Self::execute_with_policy(
            checkpoint,
            completion,
            tools,
            ExecutionPolicy::default(),
            cancellation,
            routes,
        )
        .await
    }

    /// Drive a node while persisting protocol state at every external I/O
    /// boundary. The key is supplied by the host and is opaque to the runner.
    pub async fn execute_with_store<C: CompletionPort, T: ToolPort, S: CheckpointStore>(
        checkpoint: &mut AgentCheckpoint,
        completion: &C,
        tools: &T,
        store: &S,
        store_key: &str,
        cancellation: &Cancellation,
        routes: &[String],
    ) -> Result<NodeOutcome, NodeError> {
        let context = ExecutionContext {
            policy: ExecutionPolicy::default(),
            cancellation,
            routes,
        };
        Self::execute_with_store_context(checkpoint, completion, tools, store, store_key, &context)
            .await
    }

    pub async fn execute_with_policy<C: CompletionPort, T: ToolPort>(
        checkpoint: &mut AgentCheckpoint,
        completion: &C,
        tools: &T,
        policy: ExecutionPolicy,
        cancellation: &Cancellation,
        routes: &[String],
    ) -> Result<NodeOutcome, NodeError> {
        let context = ExecutionContext {
            policy,
            cancellation,
            routes,
        };
        Self::execute_with_store_context(
            checkpoint,
            completion,
            tools,
            &NoopCheckpointStore,
            "noop",
            &context,
        )
        .await
    }

    async fn execute_with_store_context<C: CompletionPort, T: ToolPort, S: CheckpointStore>(
        checkpoint: &mut AgentCheckpoint,
        completion: &C,
        tools: &T,
        store: &S,
        store_key: &str,
        context: &ExecutionContext<'_>,
    ) -> Result<NodeOutcome, NodeError> {
        let mut requests = 0;
        loop {
            if context.cancellation.load(Ordering::Relaxed) {
                store.save(store_key, checkpoint)?;
                return Ok(NodeOutcome {
                    status: NodeStatus::Cancelled,
                    submission: String::new(),
                    route: None,
                    model_requests: requests,
                    reason: "cancelled by host".to_owned(),
                });
            }
            let step = if let Some(step) = checkpoint.pending_step.clone() {
                step
            } else {
                let step = checkpoint.run.next_step()?;
                checkpoint.pending_step = Some(step.clone());
                step
            };
            match step {
                AgentRunStep::CallModel {
                    prompt,
                    history,
                    turn,
                } => {
                    store.save(store_key, checkpoint)?;
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
                    let response = if let Some(timeout) = context.policy.model_timeout {
                        tokio::time::timeout(timeout, completion.complete(request))
                            .await
                            .map_err(|_| NodeError::Timeout {
                                phase: "model request",
                                timeout,
                            })??
                    } else {
                        completion.complete(request).await?
                    };
                    requests += 1;
                    let turn = ModelTurn::from_response(&response, &prepared);
                    let outcome = checkpoint.run.model_response(turn)?;
                    checkpoint.pending_step = None;
                    match outcome {
                        rig_agent::run::ModelTurnOutcome::Continue { .. }
                        | rig_agent::run::ModelTurnOutcome::TurnRetried => {}
                        rig_agent::run::ModelTurnOutcome::NeedsResolution(context) => {
                            return Err(NodeError::InvalidResult(format!(
                                "model requested an unavailable tool: {context:?}"
                            )));
                        }
                    }
                    store.save(store_key, checkpoint)?;
                }
                AgentRunStep::CallTools { calls } => {
                    store.save(store_key, checkpoint)?;
                    let mut results = Vec::with_capacity(calls.len());
                    for call in calls {
                        if context.cancellation.load(Ordering::Relaxed) {
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
                            let name = call.tool_call.function.name.to_string();
                            let future =
                                tools.call(&name, call.tool_call.function.arguments.clone());
                            let value = if let Some(timeout) = context.policy.tool_timeout {
                                tokio::time::timeout(timeout, future)
                                    .await
                                    .map_err(|_| NodeError::Timeout {
                                        phase: "tool execution",
                                        timeout,
                                    })?
                                    .map_err(|source| NodeError::Tool {
                                        name: name.clone(),
                                        source,
                                    })?
                            } else {
                                future.await.map_err(|source| NodeError::Tool {
                                    name: name.clone(),
                                    source,
                                })?
                            };
                            UserContent::tool_result(
                                call.tool_call.id,
                                call.tool_call.function.name,
                                value,
                            )
                        };
                        results.push(result);
                    }
                    checkpoint.run.tool_results(results)?;
                    checkpoint.pending_step = None;
                    store.save(store_key, checkpoint)?;
                }
                AgentRunStep::Done(response) => {
                    store.save(store_key, checkpoint)?;
                    return parse_outcome(response.output, requests, context.routes);
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
