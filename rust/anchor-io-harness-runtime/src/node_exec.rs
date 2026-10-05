//! Anchor `NodeRequest` execution through the io-harness loop.
//!
//! io-harness owns the model/tool loop; Anchor owns the node request, ToolPort,
//! and the completed NodeOutcome.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::node::{AnchorToolAdapter, IoHarnessNodeBackend};
use anchor_runtime_rig::graph::{RecoveryAttempt, RecoveryDecision};
use anchor_runtime_rig::{NodeError, NodeOutcome, NodeRequest, NodeStatus, ToolPort};
use base64::{Engine, engine::general_purpose::STANDARD};
use io_harness::{
    ApproveAll, Policy, RecoveryDecision as HarnessRecoveryDecision, RunOutcome, Store,
    TaskContract, ToolMask, Toolbox, Verification,
};
use serde_json::Value;

/// Errors preserve a non-completed Harness outcome instead of claiming that a
/// node finished. The outcome is retained for callers that need recovery UI or
/// a persisted Anchor run state.
#[derive(Debug, thiserror::Error)]
pub enum IoHarnessNodeExecutionError {
    #[error("node request was cancelled before io-harness started")]
    CancelledBeforeStart,
    #[error("could not register Anchor ToolPort: {0}")]
    ToolRegistration(String),
    #[error("io-harness execution failed: {0}")]
    Harness(#[from] io_harness::Error),
    #[error("io-harness stopped without completing the node: {outcome:?}")]
    Incomplete { run_id: i64, outcome: RunOutcome },
    #[error("io-harness run {run_id} awaits operator recovery: {attempts:?}")]
    AwaitingRecovery {
        run_id: i64,
        attempts: Vec<RecoveryAttempt>,
    },
    #[error("completed io-harness run has no assistant summary")]
    MissingSummary,
    #[error("completed io-harness run selected unknown route {route:?}")]
    InvalidRoute { route: String },
    #[error("could not construct Anchor completion schema: {0}")]
    CompletionSchema(String),
    #[error("completed io-harness output does not satisfy the Anchor completion schema")]
    InvalidSummary,
    #[error("invalid node image: {0}")]
    InvalidImage(String),
}

impl From<IoHarnessNodeExecutionError> for NodeError {
    fn from(error: IoHarnessNodeExecutionError) -> Self {
        NodeError::InvalidResult(error.to_string())
    }
}

/// Adapter that freezes the Anchor node admission into one io-harness contract.
#[derive(Debug, Clone)]
pub struct IoHarnessNodeExecution {
    backend: IoHarnessNodeBackend,
    policy: Policy,
    images: Vec<io_harness::Media>,
}

/// Image bytes already validated and frozen by the trusted host resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeImage {
    pub data: Vec<u8>,
    pub media_type: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeExecutionLimits {
    pub wall_time: Option<Duration>,
}

impl IoHarnessNodeExecution {
    pub fn new(store_path: impl Into<std::path::PathBuf>, policy: Policy) -> Self {
        Self {
            backend: IoHarnessNodeBackend::new(store_path, policy.clone()),
            policy,
            images: Vec::new(),
        }
    }

    /// Attach the same host-frozen images to every request of this invocation,
    /// including exact-run resume and native Session turns.
    pub fn with_images(
        mut self,
        images: Vec<NodeImage>,
    ) -> Result<Self, IoHarnessNodeExecutionError> {
        self.images = images
            .into_iter()
            .map(|image| {
                if !io_harness::IMAGE_MEDIA_TYPES.contains(&image.media_type.as_str()) {
                    return Err(IoHarnessNodeExecutionError::InvalidImage(format!(
                        "unsupported image media type {:?}: expected one of {}",
                        image.media_type,
                        io_harness::IMAGE_MEDIA_TYPES.join(", "),
                    )));
                }
                // Media::image's 5 MiB convenience limit is vendor-agnostic.
                // Our Rig transport uses OpenAI wires; the trusted host owns
                // Anchor's 10 MiB per-image quota and validates the bytes.
                // Harness still enforces its 20 MiB aggregate request bound.
                Ok(io_harness::Media {
                    media_type: image.media_type,
                    base64: STANDARD.encode(image.data),
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(self)
    }

    pub fn backend(&self) -> &IoHarnessNodeBackend {
        &self.backend
    }

    /// Start an Anchor AgentNode through the io-harness backend.
    pub async fn start<P: io_harness::Provider>(
        &self,
        request: &NodeRequest,
        provider: &P,
        port: Arc<dyn ToolPort>,
    ) -> Result<NodeOutcome, IoHarnessNodeExecutionError> {
        self.start_with_limits(request, provider, port, NodeExecutionLimits::default())
            .await
    }

    pub async fn start_with_limits<P: io_harness::Provider>(
        &self,
        request: &NodeRequest,
        provider: &P,
        port: Arc<dyn ToolPort>,
        limits: NodeExecutionLimits,
    ) -> Result<NodeOutcome, IoHarnessNodeExecutionError> {
        self.run(request, provider, port, None, None, limits).await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn start_conversation_with_limits<P: io_harness::Provider>(
        &self,
        request: &NodeRequest,
        provider: &P,
        port: Arc<dyn ToolPort>,
        limits: NodeExecutionLimits,
        session: &mut io_harness::Session,
        store: &Store,
        observer: &dyn io_harness::Observer,
    ) -> Result<NodeOutcome, IoHarnessNodeExecutionError> {
        if request.cancellation.load(Ordering::Relaxed) {
            return Err(IoHarnessNodeExecutionError::CancelledBeforeStart);
        }
        let contract = frozen_contract(request, limits)?
            .with_images(self.images.clone())
            .with_conversational_turns(false)
            .with_tools(anchor_tools(port)?);
        let result = session
            .turn_bounded_observed(
                &contract,
                provider,
                store,
                &self.policy,
                &ApproveAll,
                observer,
            )
            .await?;
        self.outcome(request, result.run_id, result.outcome)
    }

    /// Resume a previously persisted io-harness run using the same frozen
    /// request admission and run id.
    pub async fn resume<P: io_harness::Provider>(
        &self,
        request: &NodeRequest,
        provider: &P,
        port: Arc<dyn ToolPort>,
        run_id: i64,
    ) -> Result<NodeOutcome, IoHarnessNodeExecutionError> {
        self.resume_with_limits(
            request,
            provider,
            port,
            run_id,
            NodeExecutionLimits::default(),
        )
        .await
    }

    pub async fn resume_with_limits<P: io_harness::Provider>(
        &self,
        request: &NodeRequest,
        provider: &P,
        port: Arc<dyn ToolPort>,
        run_id: i64,
        limits: NodeExecutionLimits,
    ) -> Result<NodeOutcome, IoHarnessNodeExecutionError> {
        self.run(request, provider, port, Some(run_id), None, limits)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn resume_with_recovery<P: io_harness::Provider>(
        &self,
        request: &NodeRequest,
        provider: &P,
        port: Arc<dyn ToolPort>,
        run_id: i64,
        attempt_id: i64,
        decision: RecoveryDecision,
        limits: NodeExecutionLimits,
    ) -> Result<NodeOutcome, IoHarnessNodeExecutionError> {
        self.run(
            request,
            provider,
            port,
            Some(run_id),
            Some((attempt_id, decision)),
            limits,
        )
        .await
    }

    async fn run<P: io_harness::Provider>(
        &self,
        request: &NodeRequest,
        provider: &P,
        port: Arc<dyn ToolPort>,
        resume_id: Option<i64>,
        recovery: Option<(i64, RecoveryDecision)>,
        limits: NodeExecutionLimits,
    ) -> Result<NodeOutcome, IoHarnessNodeExecutionError> {
        if request.cancellation.load(Ordering::Relaxed) {
            return Err(IoHarnessNodeExecutionError::CancelledBeforeStart);
        }

        let contract = frozen_contract(request, limits)?.with_images(self.images.clone());
        let tools = anchor_tools(port)?;
        let result = match (resume_id, recovery) {
            (Some(run_id), Some((attempt_id, decision))) => {
                let store = Store::open(self.backend.store_path())?;
                let contract = contract.clone().with_tools(tools);
                let decision = match decision {
                    RecoveryDecision::Retry => HarnessRecoveryDecision::Retry,
                    RecoveryDecision::Completed { observation } => {
                        HarnessRecoveryDecision::Completed { observation }
                    }
                    RecoveryDecision::Abort => HarnessRecoveryDecision::Abort,
                };
                io_harness::resume_with_recovery(
                    &contract,
                    provider,
                    &store,
                    run_id,
                    attempt_id,
                    decision,
                    &self.policy,
                    &ApproveAll,
                )
                .await?
            }
            (Some(run_id), None) => {
                self.backend
                    .resume_with_cancellation(
                        &contract,
                        provider,
                        tools,
                        run_id,
                        Arc::clone(&request.cancellation),
                    )
                    .await?
            }
            (None, None) => {
                self.backend
                    .start_with_cancellation(
                        &contract,
                        provider,
                        tools,
                        Arc::clone(&request.cancellation),
                    )
                    .await?
            }
            (None, Some(_)) => unreachable!("recovery requires a persisted run id"),
        };

        self.outcome(request, result.run_id, result.outcome)
    }

    fn outcome(
        &self,
        request: &NodeRequest,
        run_id: i64,
        outcome: RunOutcome,
    ) -> Result<NodeOutcome, IoHarnessNodeExecutionError> {
        if !matches!(outcome, RunOutcome::Finished { .. }) {
            if let RunOutcome::AwaitingRecovery { attempt_id, .. } = outcome {
                let store = Store::open(self.backend.store_path())?;
                let attempt = store
                    .open_attempts(run_id)?
                    .into_iter()
                    .find(|attempt| attempt.id == attempt_id)
                    .ok_or_else(|| {
                        io_harness::Error::Config(format!(
                            "missing open recovery attempt {attempt_id}"
                        ))
                    })?;
                return Err(IoHarnessNodeExecutionError::AwaitingRecovery {
                    run_id,
                    attempts: vec![RecoveryAttempt {
                        attempt_id: attempt.id,
                        step: attempt.step,
                        tool: attempt.tool,
                        started_at: attempt.started_at,
                    }],
                });
            }
            return Err(IoHarnessNodeExecutionError::Incomplete { run_id, outcome });
        }

        let store = Store::open(self.backend.store_path())?;
        let turns = store.step_turns(run_id)?;
        let final_turn = turns
            .last()
            .filter(|turn| {
                turn.calls.is_empty()
                    && turn
                        .text
                        .as_deref()
                        .is_some_and(|text| !text.trim().is_empty())
            })
            .ok_or(IoHarnessNodeExecutionError::MissingSummary)?;
        let text = final_turn
            .text
            .as_deref()
            .ok_or(IoHarnessNodeExecutionError::InvalidSummary)?
            .trim();
        let (submission, route) = summary_and_route(text, &request.routes)?;

        Ok(NodeOutcome {
            status: NodeStatus::Completed,
            submission,
            route,
            model_requests: store.provider_calls(run_id)?.len(),
            reason: format!("io-harness finished run {run_id}"),
        })
    }
}

fn frozen_contract(
    request: &NodeRequest,
    limits: NodeExecutionLimits,
) -> Result<TaskContract, IoHarnessNodeExecutionError> {
    let max_steps = request.max_turns.clamp(1, u32::MAX as usize) as u32;
    let output_schema = completion_schema(&request.routes)?;
    let mut contract = TaskContract::workspace(&request.task, &request.workspace)
        .with_verification(Verification::None)
        .with_max_steps(max_steps)
        .with_tool_mask(anchor_tool_mask())
        .with_output_schema(output_schema);
    if let Some(wall_time) = limits.wall_time {
        contract = contract.with_time_budget(wall_time);
    }
    if !request.instructions.trim().is_empty() {
        contract.instructions.push(request.instructions.clone());
    }
    let routes = if request.routes.is_empty() {
        "(no route)".to_owned()
    } else {
        request.routes.join(", ")
    };
    contract
        .instructions
        .push(format!("Allowed Anchor routes: {routes}"));
    contract.instructions.push(
        "When this node is complete, call final_result with a non-empty summary and a legal route when required. Call it alone after inspecting all business tool results. Completion is separate from business tool use; ordinary text does not complete the node.".into()
    );
    Ok(contract)
}

fn completion_schema(
    routes: &[String],
) -> Result<io_harness::schema::OutputSchema, IoHarnessNodeExecutionError> {
    let mut properties = serde_json::Map::from_iter([(
        "summary".into(),
        serde_json::json!({"type":"string","minLength":1}),
    )]);
    if !routes.is_empty() {
        properties.insert(
            "route".into(),
            serde_json::json!({"type":"string","enum":routes}),
        );
    } else {
        // A supplied non-null route with no outgoing edge is a correctable
        // completion error, not an ignored extra field or a post-loop failure.
        properties.insert("route".into(), serde_json::json!({"type":"null"}));
    }
    let mut required = vec![serde_json::json!("summary")];
    if routes.len() > 1 {
        required.push(serde_json::json!("route"));
    }
    io_harness::schema::OutputSchema::new(serde_json::json!({
        "type":"object",
        "properties":properties,
        "required":required,
        "additionalProperties":true
    }))
    .map_err(|error| IoHarnessNodeExecutionError::CompletionSchema(error.to_string()))
}

/// io-harness workspace tools execute against the host workspace directly.
/// Anchor owns filesystem/process access through its ToolPort and Bubblewrap
/// adapter, so the harness-native capabilities must never become a bypass.
fn anchor_tool_mask() -> ToolMask {
    ToolMask::withholding([
        io_harness::tools::WRITE_FILE_TOOL,
        io_harness::tools::EDIT_FILE_TOOL,
        io_harness::tools::PATCH_FILE_TOOL,
        io_harness::tools::CHECK_TOOL,
        io_harness::tools::LSP_DEFINITION_TOOL,
        io_harness::tools::LSP_REFERENCES_TOOL,
        io_harness::tools::LSP_SYMBOLS_TOOL,
        io_harness::tools::LSP_HOVER_TOOL,
        io_harness::tools::LSP_RENAME_TOOL,
        io_harness::tools::BROWSER_NAVIGATE_TOOL,
        io_harness::tools::BROWSER_READ_TOOL,
        io_harness::tools::BROWSER_SCREENSHOT_TOOL,
        io_harness::tools::BROWSER_CLICK_TOOL,
        io_harness::tools::BROWSER_TYPE_TOOL,
        io_harness::tools::BROWSER_SCROLL_TOOL,
        io_harness::tools::EXEC_TOOL,
        io_harness::tools::SHELL_TOOL,
        io_harness::tools::SHELL_START_TOOL,
        io_harness::tools::SHELL_POLL_TOOL,
        io_harness::tools::SHELL_KILL_TOOL,
        io_harness::tools::RUN_PROGRAM_TOOL,
        io_harness::tools::GREP_TOOL,
        io_harness::tools::FIND_TOOL,
        io_harness::tools::LIST_DIR_TOOL,
        io_harness::tools::READ_FILE_TOOL,
        io_harness::tools::GIT_LOG_TOOL,
        io_harness::tools::GIT_STATUS_TOOL,
        io_harness::tools::GIT_DIFF_TOOL,
        io_harness::tools::GIT_ADD_TOOL,
        io_harness::tools::GIT_COMMIT_TOOL,
        io_harness::tools::GIT_BRANCH_TOOL,
        io_harness::tools::GIT_WORKTREE_TOOL,
        io_harness::tools::VIEW_IMAGE_TOOL,
        io_harness::tools::XLSX_READ_TOOL,
        io_harness::tools::XLSX_SHEETS_TOOL,
        io_harness::tools::XLSX_WRITE_TOOL,
        io_harness::tools::XLSX_SET_CELL_TOOL,
        io_harness::tools::DOCX_READ_TOOL,
        io_harness::tools::DOCX_WRITE_TOOL,
        io_harness::tools::PPTX_READ_TOOL,
        io_harness::tools::PDF_READ_TOOL,
        io_harness::tools::PDF_WRITE_TOOL,
        io_harness::tools::PDF_WATERMARK_TOOL,
        io_harness::tools::PDF_FILL_FORM_TOOL,
        io_harness::tools::BARCODE_DECODE_TOOL,
        io_harness::tools::REMEMBER_TOOL,
        io_harness::tools::FORGET_TOOL,
        io_harness::tools::TODO_WRITE_TOOL,
        io_harness::ASK_QUESTION_TOOL,
        io_harness::ASK_QUESTIONS_TOOL,
        io_harness::PROPOSE_PLAN_TOOL,
        io_harness::tools::READ_SKILL_TOOL,
        io_harness::tools::EXPAND_TOOLS_TOOL,
    ])
}

fn anchor_tools(port: Arc<dyn ToolPort>) -> Result<Toolbox, IoHarnessNodeExecutionError> {
    let mut tools = Toolbox::new();
    for definition in port.definitions() {
        if definition.name == crate::completion::TOOL_NAME {
            return Err(IoHarnessNodeExecutionError::ToolRegistration(
                "tool name `final_result` is reserved for Anchor node completion".into(),
            ));
        }
        let adapter = AnchorToolAdapter::new(Arc::clone(&port), &definition.name)
            .map_err(IoHarnessNodeExecutionError::ToolRegistration)?;
        tools = tools.with(adapter);
    }
    tools
        .validate()
        .map_err(|error| IoHarnessNodeExecutionError::ToolRegistration(error.to_string()))?;
    Ok(tools)
}

pub(crate) fn validate_anchor_tools(
    port: Arc<dyn ToolPort>,
) -> Result<(), IoHarnessNodeExecutionError> {
    anchor_tools(port).map(|_| ())
}

fn summary_and_route(
    text: &str,
    routes: &[String],
) -> Result<(String, Option<String>), IoHarnessNodeExecutionError> {
    let schema = completion_schema(routes)?;
    let value = schema
        .validate_text(text)
        .map_err(|_| IoHarnessNodeExecutionError::InvalidSummary)?;
    let object = value
        .as_object()
        .ok_or(IoHarnessNodeExecutionError::InvalidSummary)?;
    let submission = object
        .get("summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .ok_or(IoHarnessNodeExecutionError::InvalidSummary)?
        .to_owned();
    let route = object
        .get("route")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(route) = &route
        && !routes.iter().any(|allowed| allowed == route)
    {
        return Err(IoHarnessNodeExecutionError::InvalidRoute {
            route: route.clone(),
        });
    }
    if routes.len() > 1 && route.is_none() {
        return Err(IoHarnessNodeExecutionError::InvalidSummary);
    }
    Ok((submission, route))
}

/// A permissive fixture policy for provider-free tests and local spikes.
pub fn fixture_policy() -> Policy {
    Policy::default()
        .layer("fixture")
        .allow_read("*")
        .allow_exec("*")
        .allow_write("*")
}

/// The default approval is deliberately kept at the backend boundary.
#[allow(dead_code)]
fn _approval_type_is_public() -> ApproveAll {
    ApproveAll
}

#[cfg(test)]
mod tests {
    mod completion_protocol;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::adapter::RigProviderAdapter;
    use anchor_runtime_rig::{Cancellation, ToolError, ToolPort};
    use io_harness::RunOutcome;
    use rig_core::completion::ToolDefinition;
    use rig_core::message::ToolResultContent;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};
    use serde_json::json;

    use super::{
        IoHarnessNodeExecution, IoHarnessNodeExecutionError, NodeExecutionLimits, fixture_policy,
        frozen_contract, validate_anchor_tools,
    };

    struct FakePort {
        calls: AtomicUsize,
        cancel_on_call: Option<std::sync::Arc<AtomicBool>>,
        tool_name: &'static str,
    }

    impl anchor_runtime_rig::ToolPort for FakePort {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: self.tool_name.into(),
                description: "Echo a value through Anchor.".into(),
                parameters: json!({"type":"object","properties":{"value":{"type":"string"}}}),
            }]
        }

        fn call<'a>(
            &'a self,
            name: &'a str,
            arguments: serde_json::Value,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>>
        {
            Box::pin(async move {
                if name != self.tool_name {
                    return Err(ToolError::Unknown(name.to_owned()));
                }
                self.calls.fetch_add(1, Ordering::SeqCst);
                if let Some(cancellation) = &self.cancel_on_call {
                    cancellation.store(true, Ordering::SeqCst);
                }
                Ok(vec![ToolResultContent::json(json!({
                    "echo": arguments["value"]
                }))])
            })
        }
    }

    struct DirectMcpPort {
        remote_calls: AtomicUsize,
    }

    impl ToolPort for DirectMcpPort {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "fixture_remote_read".into(),
                description: "Read a record by its id.".into(),
                parameters: json!({
                    "type":"object",
                    "properties":{"id":{"type":"string"}},
                    "required":["id"],
                    "additionalProperties":false
                }),
            }]
        }

        fn call<'a>(
            &'a self,
            name: &'a str,
            arguments: serde_json::Value,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>>
        {
            Box::pin(async move {
                match name {
                    "fixture_remote_read" if arguments["id"] == "item-1" => {
                        self.remote_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(vec![ToolResultContent::json(json!({"value":"found"}))])
                    }
                    _ => Err(ToolError::Unknown(name.to_owned())),
                }
            })
        }
    }

    struct NamedPort(Vec<&'static str>);

    impl anchor_runtime_rig::ToolPort for NamedPort {
        fn definitions(&self) -> Vec<ToolDefinition> {
            self.0
                .iter()
                .map(|name| ToolDefinition {
                    name: (*name).into(),
                    description: "fixture".into(),
                    parameters: json!({"type":"object"}),
                })
                .collect()
        }

        fn call<'a>(
            &'a self,
            name: &'a str,
            _arguments: serde_json::Value,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>>
        {
            Box::pin(async move { Err(ToolError::Unknown(name.to_owned())) })
        }
    }

    #[test]
    fn invalid_anchor_tool_names_fail_before_run_start() {
        let reserved: Arc<dyn anchor_runtime_rig::ToolPort> =
            Arc::new(NamedPort(vec!["read_file"]));
        assert!(
            validate_anchor_tools(reserved)
                .unwrap_err()
                .to_string()
                .contains("built-in")
        );

        let duplicate: Arc<dyn anchor_runtime_rig::ToolPort> =
            Arc::new(NamedPort(vec!["same", "same"]));
        assert!(
            validate_anchor_tools(duplicate)
                .unwrap_err()
                .to_string()
                .contains("both named")
        );
    }

    fn request(
        workspace: &std::path::Path,
        cancellation: Cancellation,
    ) -> anchor_runtime_rig::NodeRequest {
        anchor_runtime_rig::NodeRequest {
            execution_id: "exec-1".into(),
            task: "use the echo tool and report the result".into(),
            instructions: "Keep the answer concise.".into(),
            routes: vec!["next".into()],
            max_turns: 4,
            workspace: workspace.to_owned(),
            cancellation,
        }
    }

    #[test]
    fn frozen_contract_withholds_harness_native_host_tools() {
        let dir = tempfile::tempdir().unwrap();
        let req = request(dir.path(), std::sync::Arc::new(AtomicBool::new(false)));
        let contract = frozen_contract(&req, NodeExecutionLimits::default()).unwrap();
        for name in ["exec", "shell", "write_file", "read_file", "git_commit"] {
            assert!(contract.tool_mask.withholds(name), "{name} must be masked");
        }
        assert!(
            !contract.tool_mask.withholds("anchor_echo"),
            "Anchor ToolPort tools must remain callable through the host adapter"
        );
        let schema = contract.output_schema.unwrap();
        assert_eq!(
            schema.as_value()["required"],
            serde_json::json!(["summary"])
        );
        assert_eq!(
            schema.as_value()["properties"]["route"]["enum"],
            serde_json::json!(["next"])
        );
        assert_eq!(schema.as_value()["additionalProperties"], true);
    }

    #[test]
    fn graph_wall_time_becomes_durable_harness_budget() {
        let dir = tempfile::tempdir().unwrap();
        let req = request(dir.path(), std::sync::Arc::new(AtomicBool::new(false)));
        let contract = frozen_contract(
            &req,
            NodeExecutionLimits {
                wall_time: Some(Duration::from_secs(45)),
            },
        )
        .unwrap();
        assert_eq!(contract.max_duration, Some(Duration::from_secs(45)));
    }

    #[tokio::test]
    async fn node_request_reaches_rig_and_anchor_tool_then_completes() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call("call-1", "anchor_echo", json!({"value":"from-node"})),
            MockTurn::tool_call(
                "completion-1",
                "final_result",
                json!({"summary":"echo observed","route":"next"}),
            ),
        ]);
        let provider = RigProviderAdapter::new(model.clone().erase(), false);
        let port = std::sync::Arc::new(FakePort {
            calls: AtomicUsize::new(0),
            cancel_on_call: None,
            tool_name: "anchor_echo",
        });
        let execution =
            IoHarnessNodeExecution::new(dir.path().join("runs.sqlite3"), fixture_policy());
        let cancellation = std::sync::Arc::new(AtomicBool::new(false));
        let outcome = execution
            .start(&request(dir.path(), cancellation), &provider, port.clone())
            .await
            .unwrap();
        assert_eq!(outcome.status, anchor_runtime_rig::NodeStatus::Completed);
        assert_eq!(outcome.submission, "echo observed");
        assert_eq!(outcome.route.as_deref(), Some("next"));
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);
        let serialized = model
            .requests()
            .iter()
            .map(|request| serde_json::to_string(request).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(serialized.contains("use the echo tool"));
        assert!(serialized.contains("Keep the answer concise."));
        assert!(serialized.contains("Allowed Anchor routes: next"));
    }

    #[tokio::test]
    async fn direct_mcp_tools_are_registered_with_their_prefixed_schema() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call("call-1", "fixture_remote_read", json!({"id":"item-1"})),
            MockTurn::tool_call(
                "completion-1",
                "final_result",
                json!({"summary":"record found","route":"next"}),
            ),
        ]);
        let provider = RigProviderAdapter::new(model.clone().erase(), false);
        let port = Arc::new(DirectMcpPort {
            remote_calls: AtomicUsize::new(0),
        });
        let execution =
            IoHarnessNodeExecution::new(dir.path().join("runs.sqlite3"), fixture_policy());
        let outcome = execution
            .start(
                &request(dir.path(), Arc::new(AtomicBool::new(false))),
                &provider,
                port.clone(),
            )
            .await
            .unwrap();

        assert_eq!(outcome.status, anchor_runtime_rig::NodeStatus::Completed);
        assert_eq!(outcome.submission, "record found");
        assert_eq!(port.remote_calls.load(Ordering::SeqCst), 1);
        let requests = model.requests();
        assert_eq!(requests.len(), 2);
        let initial = serde_json::to_string(&requests[0]).unwrap();
        assert!(initial.contains("fixture_remote_read"));
        let after_call = serde_json::to_string(&requests[1]).unwrap();
        assert!(after_call.contains("found"));
    }

    #[tokio::test]
    async fn malformed_wrapped_completion_is_corrected_by_harness_output_schema() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockCompletionModel::from_turns([
            MockTurn::text(
                "```json\n{\"summary\":\"first attempt\",\"route\":\"next\",\"extra\":true}\n```",
            ),
            MockTurn::tool_call(
                "completion-1",
                "final_result",
                json!({"summary":"corrected","route":"next"}),
            ),
        ]);
        let provider = RigProviderAdapter::new(model.clone().erase(), false);
        let execution =
            IoHarnessNodeExecution::new(dir.path().join("runs.sqlite3"), fixture_policy());
        let outcome = execution
            .start(
                &request(dir.path(), std::sync::Arc::new(AtomicBool::new(false))),
                &provider,
                std::sync::Arc::new(FakePort {
                    calls: AtomicUsize::new(0),
                    cancel_on_call: None,
                    tool_name: "anchor_echo",
                }),
            )
            .await
            .unwrap();

        assert_eq!(outcome.submission, "corrected");
        assert_eq!(outcome.route.as_deref(), Some("next"));
        assert_eq!(model.request_count(), 2);
        let requests = model.requests();
        assert!(requests[0].output_schema.is_none());
        let initial = serde_json::to_string(&requests[0]).unwrap();
        assert!(initial.contains("additionalProperties"));
        assert!(initial.contains("summary"));
        assert!(initial.contains("required"));
        assert!(
            serde_json::to_string(&requests[1])
                .unwrap()
                .contains("output shape")
        );
    }

    #[tokio::test]
    async fn cancelled_request_never_publishes_completed_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockCompletionModel::text("should not run");
        let provider = RigProviderAdapter::new(model.clone().erase(), false);
        let execution =
            IoHarnessNodeExecution::new(dir.path().join("runs.sqlite3"), fixture_policy());
        let cancellation = std::sync::Arc::new(AtomicBool::new(true));
        let error = execution
            .start(
                &request(dir.path(), cancellation),
                &provider,
                std::sync::Arc::new(FakePort {
                    calls: AtomicUsize::new(0),
                    cancel_on_call: None,
                    tool_name: "anchor_echo",
                }),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IoHarnessNodeExecutionError::CancelledBeforeStart
        ));
        assert_eq!(model.request_count(), 0);
    }

    #[tokio::test]
    async fn in_run_cancellation_stops_before_publishing_completion() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call("call-1", "anchor_echo", json!({"value":"cancel-me"})),
            MockTurn::tool_call(
                "completion-1",
                "final_result",
                json!({"summary":"must not publish","route":"next"}),
            ),
        ]);
        let provider = RigProviderAdapter::new(model.clone().erase(), false);
        let cancellation = std::sync::Arc::new(AtomicBool::new(false));
        let port = std::sync::Arc::new(FakePort {
            calls: AtomicUsize::new(0),
            cancel_on_call: Some(std::sync::Arc::clone(&cancellation)),
            tool_name: "anchor_echo",
        });
        let execution =
            IoHarnessNodeExecution::new(dir.path().join("runs.sqlite3"), fixture_policy());
        let error = execution
            .start(&request(dir.path(), cancellation), &provider, port.clone())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IoHarnessNodeExecutionError::Incomplete {
                outcome: RunOutcome::Cancelled { .. },
                ..
            }
        ));
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);
        assert_eq!(model.request_count(), 1);
    }

    #[tokio::test]
    async fn step_cap_is_explicitly_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockCompletionModel::from_turns([MockTurn::tool_call(
            "call-1",
            "anchor_echo",
            json!({"value":"unfinished"}),
        )]);
        let provider = RigProviderAdapter::new(model.erase(), false);
        let execution =
            IoHarnessNodeExecution::new(dir.path().join("runs.sqlite3"), fixture_policy());
        let mut req = request(dir.path(), std::sync::Arc::new(AtomicBool::new(false)));
        req.max_turns = 1;
        let error = execution
            .start(
                &req,
                &provider,
                std::sync::Arc::new(FakePort {
                    calls: AtomicUsize::new(0),
                    cancel_on_call: None,
                    tool_name: "anchor_echo",
                }),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IoHarnessNodeExecutionError::Incomplete {
                outcome: RunOutcome::StepCapReached { .. },
                ..
            }
        ));
    }
}
