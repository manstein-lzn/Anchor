//! Provider-free vertical spike for an Anchor ToolPort in io-harness.
//!
//! `io-harness` owns the model/tool loop. Anchor owns tool definitions and
//! execution through [`anchor_runtime_rig::ToolPort`]. The adapter makes that
//! boundary explicit, including conservative recovery semantics.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anchor_runtime_rig::{ToolError, ToolPort};
use io_harness::{Flow, Observer, RunEvent, Tool, ToolEffect, ToolFuture, ToolRecovery, ToolSpec};
use rig_core::completion::ToolDefinition;
use serde_json::Value;

/// The io-harness-owned AgentNode execution boundary used by the next host
/// integration slice. Each invocation opens the same durable SQLite Store by
/// path; Anchor Graph/Run facts remain outside this helper.
#[derive(Debug, Clone)]
pub struct IoHarnessNodeBackend {
    store_path: PathBuf,
    policy: io_harness::Policy,
}

impl IoHarnessNodeBackend {
    pub fn new(store_path: impl Into<PathBuf>, policy: io_harness::Policy) -> Self {
        Self {
            store_path: store_path.into(),
            policy,
        }
    }

    pub fn store_path(&self) -> &Path {
        &self.store_path
    }

    /// Start one io-harness run and return its durable run id/outcome.
    pub async fn start<P: io_harness::Provider>(
        &self,
        contract: &io_harness::TaskContract,
        provider: &P,
        tools: io_harness::Toolbox,
    ) -> io_harness::Result<io_harness::RunResult> {
        self.start_with_cancellation(
            contract,
            provider,
            tools,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
    }

    /// Start a run while observing an Anchor cancellation flag. io-harness
    /// honors cancellation at its durable step boundary, so an in-flight
    /// model/tool operation is allowed to settle before the run becomes
    /// resumable with `RunOutcome::Cancelled`.
    pub async fn start_with_cancellation<P: io_harness::Provider>(
        &self,
        contract: &io_harness::TaskContract,
        provider: &P,
        tools: io_harness::Toolbox,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> io_harness::Result<io_harness::RunResult> {
        let store = io_harness::Store::open(&self.store_path)?;
        let contract = contract.clone().with_tools(tools);
        let observer = AnchorCancellationObserver { cancellation };
        io_harness::run_with_observed(
            &contract,
            provider,
            &store,
            &self.policy,
            &io_harness::ApproveAll,
            &observer,
        )
        .await
    }

    /// Resume the exact io-harness run id after reopening its Store. The
    /// contract and ToolBox must be reconstructed from the same frozen node
    /// admission; this helper does not silently reload current Graph config.
    pub async fn resume<P: io_harness::Provider>(
        &self,
        contract: &io_harness::TaskContract,
        provider: &P,
        tools: io_harness::Toolbox,
        run_id: i64,
    ) -> io_harness::Result<io_harness::RunResult> {
        self.resume_with_cancellation(
            contract,
            provider,
            tools,
            run_id,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
    }

    /// Resume the same durable run while preserving the caller's cancellation
    /// flag and the io-harness step boundary semantics.
    pub async fn resume_with_cancellation<P: io_harness::Provider>(
        &self,
        contract: &io_harness::TaskContract,
        provider: &P,
        tools: io_harness::Toolbox,
        run_id: i64,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> io_harness::Result<io_harness::RunResult> {
        let store = io_harness::Store::open(&self.store_path)?;
        let contract = contract.clone().with_tools(tools);
        let observer = AnchorCancellationObserver { cancellation };
        io_harness::resume_with_observed(
            &contract,
            provider,
            &store,
            run_id,
            &self.policy,
            &io_harness::ApproveAll,
            &observer,
        )
        .await
    }
}

struct AnchorCancellationObserver {
    cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Observer for AnchorCancellationObserver {
    fn event(&self, _event: &RunEvent) -> Flow {
        if self.cancellation.load(std::sync::atomic::Ordering::Relaxed) {
            Flow::Cancel
        } else {
            Flow::Continue
        }
    }
}

/// An Anchor ToolPort exposed as one io-harness tool per Anchor definition.
///
/// The default declaration is deliberately conservative: calls are mutating
/// and indeterminate to prevent an interrupted external operation from being
/// replayed automatically. [`Self::read_only_fixture`] is an explicit fixture
/// escape hatch for tools whose implementation is known to be read-only.
pub struct AnchorToolAdapter {
    port: Arc<dyn ToolPort>,
    definition: ToolDefinition,
    read_only_fixture: bool,
}

impl AnchorToolAdapter {
    /// Build a conservative adapter for one named Anchor tool.
    pub fn new(port: Arc<dyn ToolPort>, name: &str) -> Result<Self, String> {
        Self::with_mode(port, name, false)
    }

    /// Build an adapter explicitly marked read-only/replayable for a fixture.
    pub fn read_only_fixture(port: Arc<dyn ToolPort>, name: &str) -> Result<Self, String> {
        Self::with_mode(port, name, true)
    }

    fn with_mode(
        port: Arc<dyn ToolPort>,
        name: &str,
        read_only_fixture: bool,
    ) -> Result<Self, String> {
        let definition = port
            .definitions()
            .into_iter()
            .find(|definition| definition.name == name)
            .ok_or_else(|| format!("Anchor tool `{name}` is not registered"))?;
        Ok(Self {
            port,
            definition,
            read_only_fixture,
        })
    }

    /// The Anchor definition represented by this adapter.
    pub fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn encode_results(
        results: Vec<rig_core::message::ToolResultContent>,
    ) -> Result<String, String> {
        if results
            .iter()
            .any(|result| matches!(result, rig_core::message::ToolResultContent::Image(_)))
        {
            return Err("rich image tool results are outside the io-harness JSON boundary".into());
        }
        if results.len() == 1 {
            let result = &results[0];
            if let Some(value) = result.as_json() {
                return serde_json::to_string(value).map_err(|error| error.to_string());
            }
            if let Some(text) = result.as_text() {
                return Ok(text.to_owned());
            }
        }
        serde_json::to_string(&results).map_err(|error| error.to_string())
    }
}

impl Tool for AnchorToolAdapter {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.definition.name.clone(),
            description: self.definition.description.clone(),
            parameters: self.definition.parameters.clone(),
        }
    }

    fn invoke<'a>(&'a self, arguments: &'a Value) -> ToolFuture<'a> {
        let name = self.definition.name.clone();
        let arguments = arguments.clone();
        Box::pin(async move {
            let results = self
                .port
                .call(&name, arguments)
                .await
                .map_err(|error| match error {
                    ToolError::Unknown(name) => {
                        io_harness::Error::Config(format!("unknown Anchor tool `{name}`"))
                    }
                    ToolError::Failed(message) => io_harness::Error::Config(message),
                })?;
            Self::encode_results(results).map_err(io_harness::Error::Config)
        })
    }

    fn effect(&self) -> ToolEffect {
        if self.read_only_fixture {
            ToolEffect::ReadOnly
        } else {
            ToolEffect::Mutating
        }
    }

    fn recovery(&self) -> ToolRecovery {
        if self.read_only_fixture {
            ToolRecovery::Replayable
        } else {
            ToolRecovery::Indeterminate
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::adapter::RigProviderAdapter;
    use anchor_runtime_rig::{ToolError, ToolPort};
    use io_harness::{
        ApproveAll, Policy, RunOutcome, Store, TaskContract, Tool, Toolbox, run_with,
    };
    use rig_core::completion::ToolDefinition;
    use rig_core::message::ToolResultContent;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};
    use serde_json::json;

    use super::AnchorToolAdapter;
    use super::IoHarnessNodeBackend;

    struct FakePort {
        calls: AtomicUsize,
    }

    impl ToolPort for FakePort {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "anchor_echo".into(),
                description: "Return the supplied JSON payload.".into(),
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
                if name != "anchor_echo" {
                    return Err(ToolError::Unknown(name.into()));
                }
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(vec![ToolResultContent::json(
                    json!({"echo": arguments["value"]}),
                )])
            })
        }
    }

    #[test]
    fn defaults_are_conservative_and_fixture_can_opt_in_to_read_only() {
        let port: Arc<dyn ToolPort> = Arc::new(FakePort {
            calls: AtomicUsize::new(0),
        });
        let adapter = AnchorToolAdapter::new(Arc::clone(&port), "anchor_echo").unwrap();
        assert_eq!(adapter.effect(), io_harness::ToolEffect::Mutating);
        assert_eq!(adapter.recovery(), io_harness::ToolRecovery::Indeterminate);
        let fixture = AnchorToolAdapter::read_only_fixture(port, "anchor_echo").unwrap();
        assert_eq!(fixture.effect(), io_harness::ToolEffect::ReadOnly);
        assert_eq!(fixture.recovery(), io_harness::ToolRecovery::Replayable);
    }

    #[test]
    fn rich_image_tool_results_fail_closed() {
        assert!(
            AnchorToolAdapter::encode_results(vec![ToolResultContent::Image(Default::default(),)])
                .is_err()
        );
    }

    #[tokio::test]
    async fn rig_model_io_loop_calls_anchor_once_and_sees_json_result() {
        let port = Arc::new(FakePort {
            calls: AtomicUsize::new(0),
        });
        let adapter = AnchorToolAdapter::new(port.clone(), "anchor_echo").unwrap();
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call("call-1", "anchor_echo", json!({"value":"from-anchor"})),
            MockTurn::text("done after observing {\"echo\":\"from-anchor\"}"),
        ]);
        let provider = RigProviderAdapter::new(model.clone().erase(), false);
        let contract = TaskContract::workspace("call the Anchor echo tool", ".")
            .with_tools(Toolbox::new().with(adapter))
            .with_max_steps(4);
        let policy = Policy::default()
            .layer("fixture")
            .allow_read("*")
            .allow_exec("*")
            .allow_write("*");
        let result = run_with(
            &contract,
            &provider,
            &Store::memory().unwrap(),
            &policy,
            &ApproveAll,
        )
        .await
        .unwrap();
        assert!(matches!(result.outcome, RunOutcome::Finished { .. }));
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);
        assert_eq!(model.request_count(), 2);
        let requests = model.requests();
        let serialized = serde_json::to_string(&requests[1]).unwrap();
        assert!(
            serialized.contains("from-anchor"),
            "tool result must reach the next model request: {serialized}"
        );
    }

    #[tokio::test]
    async fn backend_reopens_store_and_resumes_same_harness_run() {
        let temp = tempfile::tempdir().unwrap();
        let port = Arc::new(FakePort {
            calls: AtomicUsize::new(0),
        });
        let contract =
            TaskContract::workspace("resume an Anchor node", temp.path()).with_max_steps(1);
        let policy = Policy::default()
            .layer("fixture")
            .allow_read("*")
            .allow_exec("*")
            .allow_write("*");
        let backend = IoHarnessNodeBackend::new(temp.path().join("agent.sqlite"), policy);
        let first_model = MockCompletionModel::from_turns([MockTurn::tool_call(
            "call-1",
            "anchor_echo",
            json!({"value":"durable"}),
        )]);
        let first_provider = RigProviderAdapter::new(first_model.erase(), false);
        let first = backend
            .start(
                &contract,
                &first_provider,
                Toolbox::new().with(AnchorToolAdapter::new(port.clone(), "anchor_echo").unwrap()),
            )
            .await
            .unwrap();
        assert!(matches!(
            first.outcome,
            RunOutcome::StepCapReached { .. } | RunOutcome::Finished { .. }
        ));
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);

        let second_model = MockCompletionModel::from_turns([MockTurn::text("resumed")]);
        let second_provider = RigProviderAdapter::new(second_model.erase(), false);
        let resumed = backend
            .resume(
                &contract.clone().with_max_steps(3),
                &second_provider,
                Toolbox::new().with(AnchorToolAdapter::new(port.clone(), "anchor_echo").unwrap()),
                first.run_id,
            )
            .await
            .unwrap();
        assert!(matches!(resumed.outcome, RunOutcome::Finished { .. }));
        assert_eq!(
            port.calls.load(Ordering::SeqCst),
            1,
            "resume must not replay the tool"
        );
    }
}
