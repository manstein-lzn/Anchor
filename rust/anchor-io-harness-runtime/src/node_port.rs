//! Graph-level NodeExecutionPort adapter for the A70 io-harness boundary.
//!
//! This crate is deliberately host-neutral: the host supplies workspace and
//! ToolPort resolution, while io-harness owns the Agent loop and its SQLite
//! checkpoint. It is an integration spike, not production HostNodes wiring.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::adapter::RigProviderAdapter;
use crate::node_exec::{
    IoHarnessNodeExecution, IoHarnessNodeExecutionError, NodeExecutionLimits, fixture_policy,
    validate_anchor_tools,
};
use anchor_runtime_rig::graph::{
    CompletionFact, GraphError, InvocationKey, NodeCompletion, NodeExecutionCapabilities,
    NodeExecutionOutcome, NodeExecutionPort, NodeExecutionRequest, NodeKind, PluginBinding,
    RecoveryDecision,
};
use anchor_runtime_rig::{NodeRequest, ToolPort};
use io_harness::{Policy, RunOutcome, Store};
use rig_core::DynModel;
use rig_core::operation::Completion;
use serde_json::json;
use sha2::{Digest, Sha256};

pub trait NodeHostResolver: Send + Sync {
    /// Resolve public, immutable Plugin identity before a Run starts. Secrets
    /// and live clients remain in the host adapter; returned bindings are
    /// frozen into the Graph Run by its coordinator.
    fn resolve_plugins(&self, ids: &[String]) -> Result<Vec<PluginBinding>, String>;
    fn workspace(&self, request: &NodeExecutionRequest) -> Result<PathBuf, String>;
    fn tools<'a>(&'a self, request: &'a NodeExecutionRequest) -> ToolResolution<'a>;
}

pub type ToolResolution<'a> =
    Pin<Box<dyn Future<Output = Result<Arc<dyn ToolPort>, String>> + Send + 'a>>;

/// Read the durable, public io-harness transcript for one exact Agent invocation.
/// The API projection intentionally returns only model turns and observations; it
/// does not expose SQLite internals or Harness run IDs.
pub fn trace_messages(
    io_store_root: &Path,
    key: &InvocationKey,
) -> Result<Vec<serde_json::Value>, String> {
    let stem = format!("np1-{:x}", Sha256::digest(key.durable_key().as_bytes()));
    let sidecar = io_store_root.join(format!("{stem}.run"));
    let run_id = match std::fs::read_to_string(sidecar) {
        Ok(value) => value
            .trim()
            .parse::<i64>()
            .map_err(|error| format!("invalid io-harness run id: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let store = Store::open(io_store_root.join(format!("{stem}.sqlite3")))
        .map_err(|error| error.to_string())?;
    let turns = store
        .step_turns(run_id)
        .map_err(|error| error.to_string())?;
    let observations = store
        .observations(run_id)
        .map_err(|error| error.to_string())?;
    let mut messages = Vec::new();
    for turn in turns {
        let commands = turn
            .calls
            .iter()
            .map(|call| {
                format!(
                    "{} {}",
                    call.name,
                    serde_json::to_string(&call.arguments).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>();
        if turn.text.as_ref().is_some_and(|text| !text.is_empty()) || !commands.is_empty() {
            messages.push(json!({
                "role":"assistant",
                "text":turn.text.unwrap_or_default(),
                "commands":commands
            }));
        }
        for observation in observations.iter().filter(|item| item.step == turn.step) {
            if observation.kind == io_harness::context::ObsKind::Message {
                continue;
            }
            messages.push(json!({"role":"tool","text":observation.text}));
        }
    }
    Ok(messages)
}

pub struct IoHarnessNodePort<R> {
    facts_root: PathBuf,
    io_store_root: PathBuf,
    provider: RigProviderAdapter,
    resolver: Arc<R>,
    policy: Policy,
}

impl<R> Clone for IoHarnessNodePort<R> {
    fn clone(&self) -> Self {
        Self {
            facts_root: self.facts_root.clone(),
            io_store_root: self.io_store_root.clone(),
            provider: self.provider.clone(),
            resolver: Arc::clone(&self.resolver),
            policy: self.policy.clone(),
        }
    }
}

impl<R: NodeHostResolver> IoHarnessNodePort<R> {
    pub fn new(
        facts_root: impl Into<PathBuf>,
        io_store_root: impl Into<PathBuf>,
        model: DynModel<Completion>,
        resolver: Arc<R>,
        policy: Policy,
    ) -> Self {
        Self {
            facts_root: facts_root.into(),
            io_store_root: io_store_root.into(),
            provider: RigProviderAdapter::new(model, false),
            resolver,
            policy,
        }
    }

    /// Construct the normal host policy: io-harness built-in host tools remain
    /// masked by the frozen TaskContract, while all executable capabilities
    /// are supplied through the host's Anchor ToolPort.
    pub fn new_with_default_policy(
        facts_root: impl Into<PathBuf>,
        io_store_root: impl Into<PathBuf>,
        model: DynModel<Completion>,
        resolver: Arc<R>,
    ) -> Self {
        Self::new(
            facts_root,
            io_store_root,
            model,
            resolver,
            Policy::default(),
        )
    }

    pub fn fixture(
        facts_root: impl Into<PathBuf>,
        io_store_root: impl Into<PathBuf>,
        model: DynModel<Completion>,
        resolver: Arc<R>,
    ) -> Self {
        Self::new(facts_root, io_store_root, model, resolver, fixture_policy())
    }

    fn stem(key: &InvocationKey) -> String {
        format!("np1-{:x}", Sha256::digest(key.durable_key().as_bytes()))
    }

    fn started_path(&self, key: &InvocationKey) -> PathBuf {
        self.facts_root.join(format!("{}.started", Self::stem(key)))
    }

    fn completion_path(&self, key: &InvocationKey) -> PathBuf {
        self.facts_root.join(format!("{}.json", Self::stem(key)))
    }

    fn failed_path(&self, key: &InvocationKey) -> PathBuf {
        self.facts_root.join(format!("{}.failed", Self::stem(key)))
    }

    fn io_run_path(&self, key: &InvocationKey) -> PathBuf {
        self.io_store_root
            .join(format!("{}.sqlite3", Self::stem(key)))
    }

    fn io_run_id_path(&self, key: &InvocationKey) -> PathBuf {
        self.io_store_root.join(format!("{}.run", Self::stem(key)))
    }

    fn recovery_intent_path(&self, key: &InvocationKey, attempt_id: i64) -> PathBuf {
        self.facts_root
            .join(format!("{}.recovery-{attempt_id}.json", Self::stem(key)))
    }

    fn read_recovery_intent(
        &self,
        key: &InvocationKey,
        attempt_id: i64,
    ) -> Result<Option<RecoveryIntent>, GraphError> {
        match std::fs::read(self.recovery_intent_path(key, attempt_id)) {
            Ok(bytes) => {
                let value: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|error| GraphError::CorruptRun(error.to_string()))?;
                RecoveryIntent::from_value(value).map(Some)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn ensure_roots(&self) -> Result<(), GraphError> {
        Self::create_durable_directory(&self.facts_root)?;
        Self::create_durable_directory(&self.io_store_root)?;
        Ok(())
    }

    fn create_durable_directory(path: &Path) -> Result<(), GraphError> {
        let existed = path.exists();
        std::fs::create_dir_all(path)?;
        if !existed {
            for ancestor in path
                .ancestors()
                .filter(|ancestor| !ancestor.as_os_str().is_empty())
            {
                std::fs::File::open(ancestor)?.sync_all()?;
            }
            if path.is_relative() {
                std::fs::File::open(".")?.sync_all()?;
            }
        }
        Ok(())
    }

    fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), GraphError> {
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        let file = std::fs::OpenOptions::new().read(true).open(&tmp)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    fn write_recovery_intent_once(
        &self,
        key: &InvocationKey,
        attempt_id: i64,
        intent: &RecoveryIntent,
    ) -> Result<(), GraphError> {
        let path = self.recovery_intent_path(key, attempt_id);
        let bytes = serde_json::to_vec(&intent.to_value())
            .map_err(|error| GraphError::CorruptRun(error.to_string()))?;
        let temp = path.with_extension(format!("tmp-{}-{attempt_id}", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        use std::io::Write;
        file.write_all(&bytes)?;
        file.sync_all()?;
        match std::fs::hard_link(&temp, &path) {
            Ok(()) => {
                std::fs::remove_file(&temp)?;
                std::fs::File::open(&self.facts_root)?.sync_all()?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::remove_if_present(&temp)?;
                if self.read_recovery_intent(key, attempt_id)?.as_ref() == Some(intent) {
                    Ok(())
                } else {
                    Err(GraphError::Unsupported(format!(
                        "conflicting recovery decision for attempt {attempt_id}"
                    )))
                }
            }
            Err(error) => {
                Self::remove_if_present(&temp)?;
                Err(error.into())
            }
        }
    }

    fn mark_started(&self, key: &InvocationKey) -> Result<(), GraphError> {
        self.ensure_roots()?;
        let path = self.started_path(key);
        let result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match result {
            Ok(file) => {
                file.sync_all()?;
                std::fs::File::open(&self.facts_root)?.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // A prior attempt may have stopped after io-harness persisted
                // its run id. The marker is the durable uncertainty fence; a
                // same-key resume must keep it rather than treating it as a
                // duplicate invocation.
                let file = std::fs::OpenOptions::new().read(true).open(path)?;
                file.sync_all()?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn store_run_id(&self, key: &InvocationKey, run_id: i64) -> Result<(), GraphError> {
        Self::write_atomic(&self.io_run_id_path(key), run_id.to_string().as_bytes())
    }

    fn stored_run_id(&self, key: &InvocationKey) -> Result<Option<i64>, GraphError> {
        match std::fs::read_to_string(self.io_run_id_path(key)) {
            Ok(value) => value
                .trim()
                .parse::<i64>()
                .map(Some)
                .map_err(|error| GraphError::CorruptRun(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn remove_if_present(path: &Path) -> Result<(), GraphError> {
        match std::fs::remove_file(path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn completion_fact_inner(&self, key: &InvocationKey) -> Result<CompletionFact, GraphError> {
        match std::fs::read(self.completion_path(key)) {
            Ok(bytes) => {
                return serde_json::from_slice(&bytes)
                    .map(CompletionFact::Completed)
                    .map_err(|error| GraphError::CorruptRun(error.to_string()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        match std::fs::read(self.failed_path(key)) {
            Ok(bytes) => {
                return String::from_utf8(bytes)
                    .map(CompletionFact::Failed)
                    .map_err(|error| GraphError::CorruptRun(error.to_string()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if self.stored_run_id(key)?.is_some() || self.discover_io_run_id(key)?.is_some() {
            return Ok(CompletionFact::Resumable);
        }
        if self.started_path(key).exists() {
            return Ok(CompletionFact::Uncertain(
                "io-harness invocation started without a terminal Anchor fact; replay is refused"
                    .into(),
            ));
        }
        Ok(CompletionFact::NotStarted)
    }

    fn node_request(&self, request: &NodeExecutionRequest, workspace: PathBuf) -> NodeRequest {
        NodeRequest {
            execution_id: request.key.durable_key(),
            task: request.task.clone(),
            instructions: request.instructions.clone(),
            routes: request.routes.clone(),
            max_turns: usize::MAX,
            workspace,
            cancellation: request.cancellation.clone(),
        }
    }

    fn write_known_failure(
        &self,
        key: &InvocationKey,
        reason: impl Into<String>,
    ) -> Result<NodeExecutionOutcome, GraphError> {
        let reason = reason.into();
        self.ensure_roots()?;
        Self::write_atomic(&self.failed_path(key), reason.as_bytes())?;
        Self::remove_if_present(&self.started_path(key))?;
        Self::remove_if_present(&self.io_run_id_path(key))?;
        Ok(NodeExecutionOutcome::Failed { reason })
    }

    /// A provider/storage error can be returned after io-harness has created
    /// the SQLite run row but before it can return a `RunResult`. Each node
    /// invocation owns its Store file, so the newest row is the only safe
    /// candidate to persist for later recovery.
    fn discover_io_run_id(&self, key: &InvocationKey) -> Result<Option<i64>, GraphError> {
        let path = self.io_run_path(key);
        if !path.exists() {
            return Ok(None);
        }
        let store =
            Store::open(path).map_err(|error| GraphError::Unsupported(error.to_string()))?;
        let mut runs = store
            .runs()
            .map_err(|error| GraphError::Unsupported(error.to_string()))?;
        Ok(runs.drain(..).max())
    }

    fn provider_request_count(&self, key: &InvocationKey, run_id: i64) -> Result<u64, GraphError> {
        let store = Store::open(self.io_run_path(key))
            .map_err(|error| GraphError::Unsupported(error.to_string()))?;
        let calls = store
            .provider_calls(run_id)
            .map_err(|error| GraphError::Unsupported(error.to_string()))?;
        u64::try_from(calls.len()).map_err(|error| {
            GraphError::Unsupported(format!("provider request count overflow: {error}"))
        })
    }

    async fn execute_agent(
        &self,
        request: NodeExecutionRequest,
    ) -> Result<NodeExecutionOutcome, GraphError> {
        if request.kind != NodeKind::Agent {
            return self.write_known_failure(
                &request.key,
                "io-harness NodeExecutionPort spike accepts Agent nodes only",
            );
        }
        if request.max_provider_requests.is_some() {
            return self.write_known_failure(
                &request.key,
                "io-harness spike does not claim exact cumulative provider budget support",
            );
        }
        if request.cancellation.load(Ordering::Relaxed) {
            return Ok(NodeExecutionOutcome::Cancelled);
        }
        let workspace = self
            .resolver
            .workspace(&request)
            .map_err(GraphError::Unsupported)?;
        let tools = self
            .resolver
            .tools(&request)
            .await
            .map_err(GraphError::Unsupported)?;
        if let Err(error) = validate_anchor_tools(Arc::clone(&tools)) {
            return self.write_known_failure(&request.key, error.to_string());
        }
        self.mark_started(&request.key)?;
        let execution =
            IoHarnessNodeExecution::new(self.io_run_path(&request.key), self.policy.clone());
        let node_request = self.node_request(&request, workspace);
        let limits = NodeExecutionLimits {
            wall_time: request
                .wall_time_limit_seconds
                .map(Duration::try_from_secs_f64)
                .transpose()
                .map_err(|error| {
                    GraphError::Unsupported(format!("invalid Agent wall-time limit: {error}"))
                })?,
        };
        // The Graph may be interrupted after io-harness created its SQLite
        // run row but before this adapter could publish the `.run` sidecar.
        // Each invocation owns its database, so the newest row is the same
        // durable run and must be resumed instead of starting a second one.
        let stored_run_id = self.stored_run_id(&request.key)?;
        let run_id = match stored_run_id {
            Some(run_id) => Some(run_id),
            None => {
                let discovered = self.discover_io_run_id(&request.key)?;
                if let Some(run_id) = discovered {
                    self.store_run_id(&request.key, run_id)?;
                }
                discovered
            }
        };
        let result = match run_id {
            Some(run_id) => {
                let intent = self.find_recovery_intent(&request.key, run_id)?;
                if let Some((attempt_id, intent)) = intent {
                    let store = Store::open(self.io_run_path(&request.key))
                        .map_err(|error| GraphError::Unsupported(error.to_string()))?;
                    let open = store
                        .open_attempts(run_id)
                        .map_err(|error| GraphError::Unsupported(error.to_string()))?;
                    if open.iter().any(|attempt| attempt.id == attempt_id) {
                        if let RecoveryDecision::Completed { observation } = &intent.decision {
                            let attempt = open
                                .iter()
                                .find(|attempt| attempt.id == attempt_id)
                                .expect("checked above");
                            let expected = format!("\n[{}]\n{}\n", attempt.tool, observation);
                            let already_recorded = store
                                .observations(run_id)
                                .map_err(|error| GraphError::Unsupported(error.to_string()))?
                                .iter()
                                .any(|item| {
                                    item.step == attempt.step
                                        && item.target.as_deref() == Some(attempt.tool.as_str())
                                        && item.text == expected
                                });
                            if already_recorded {
                                store
                                    .resolve_attempt(attempt_id, "completed")
                                    .map_err(|error| GraphError::Unsupported(error.to_string()))?;
                                execution
                                    .resume_with_limits(
                                        &node_request,
                                        &self.provider,
                                        tools,
                                        run_id,
                                        limits,
                                    )
                                    .await
                            } else {
                                execution
                                    .resume_with_recovery(
                                        &node_request,
                                        &self.provider,
                                        tools,
                                        run_id,
                                        attempt_id,
                                        intent.decision.clone(),
                                        limits,
                                    )
                                    .await
                            }
                        } else {
                            execution
                                .resume_with_recovery(
                                    &node_request,
                                    &self.provider,
                                    tools,
                                    run_id,
                                    attempt_id,
                                    intent.decision.clone(),
                                    limits,
                                )
                                .await
                        }
                    } else if matches!(intent.decision, RecoveryDecision::Abort) {
                        // Keep the durable intent and run id as a tombstone.
                        // The Graph terminal fact is owned by the coordinator;
                        // deleting this before that write would make a crash in
                        // the handoff look like a fresh resumable run.
                        return Ok(NodeExecutionOutcome::Aborted);
                    } else {
                        // Retry is safe to re-enter after its durable close. For
                        // Completed, the native adapter repairs the observation/
                        // resolution crash window before reaching this point.
                        Self::remove_if_present(
                            &self.recovery_intent_path(&request.key, attempt_id),
                        )?;
                        execution
                            .resume_with_limits(
                                &node_request,
                                &self.provider,
                                tools,
                                run_id,
                                limits,
                            )
                            .await
                    }
                } else {
                    let store = Store::open(self.io_run_path(&request.key))
                        .map_err(|error| GraphError::Unsupported(error.to_string()))?;
                    let open = store
                        .open_attempts(run_id)
                        .map_err(|error| GraphError::Unsupported(error.to_string()))?;
                    if let Some(attempt) = open.into_iter().next() {
                        return Ok(NodeExecutionOutcome::WaitingRecovery {
                            attempts: vec![anchor_runtime_rig::graph::RecoveryAttempt {
                                attempt_id: attempt.id,
                                step: attempt.step,
                                tool: attempt.tool,
                                started_at: attempt.started_at,
                            }],
                        });
                    }
                    execution
                        .resume_with_limits(&node_request, &self.provider, tools, run_id, limits)
                        .await
                }
            }
            None => {
                execution
                    .start_with_limits(&node_request, &self.provider, tools, limits)
                    .await
            }
        };
        match result {
            Ok(outcome) => {
                let completion = NodeCompletion {
                    submission: outcome.submission.clone(),
                    route: outcome.route,
                    model_requests: outcome.model_requests as u64,
                    output: json!({"summary": outcome.submission}),
                };
                Self::write_atomic(
                    &self.completion_path(&request.key),
                    &serde_json::to_vec(&completion)
                        .map_err(|error| GraphError::CorruptRun(error.to_string()))?,
                )?;
                Self::remove_if_present(&self.started_path(&request.key))?;
                Self::remove_if_present(&self.io_run_id_path(&request.key))?;
                self.clear_recovery_intents(&request.key, None)?;
                Ok(NodeExecutionOutcome::Completed(completion))
            }
            Err(IoHarnessNodeExecutionError::Incomplete { run_id, outcome }) => {
                self.store_run_id(&request.key, run_id)?;
                match outcome {
                    RunOutcome::StepCapReached { .. }
                    | RunOutcome::TimeBudgetExceeded { .. }
                    | RunOutcome::CostBudgetExceeded { .. } => {
                        let model_requests = self.provider_request_count(&request.key, run_id)?;
                        Self::remove_if_present(&self.started_path(&request.key))?;
                        self.clear_recovery_intents(&request.key, None)?;
                        Ok(NodeExecutionOutcome::BudgetExhausted { model_requests })
                    }
                    RunOutcome::Cancelled { .. } => {
                        Self::remove_if_present(&self.started_path(&request.key))?;
                        self.clear_recovery_intents(&request.key, None)?;
                        Ok(NodeExecutionOutcome::Cancelled)
                    }
                    RunOutcome::Denied { .. } => {
                        // Abort is an operator decision with a durable Harness
                        // resolution. Preserve the intent/run id until the
                        // Graph's durable terminal fact is committed; NodePort
                        // has no acknowledgement hook that could safely clear it.
                        Ok(NodeExecutionOutcome::Aborted)
                    }
                    other => Err(GraphError::Unsupported(format!(
                        "io-harness run {run_id} is incomplete and requires recovery: {other:?}"
                    ))),
                }
            }
            Err(IoHarnessNodeExecutionError::AwaitingRecovery { run_id, attempts }) => {
                self.store_run_id(&request.key, run_id)?;
                let retain = attempts.first().map(|attempt| attempt.attempt_id);
                self.clear_recovery_intents(&request.key, retain)?;
                Ok(NodeExecutionOutcome::WaitingRecovery { attempts })
            }
            Err(IoHarnessNodeExecutionError::CancelledBeforeStart) => {
                Self::remove_if_present(&self.started_path(&request.key))?;
                Ok(NodeExecutionOutcome::Cancelled)
            }
            Err(error @ IoHarnessNodeExecutionError::ToolRegistration(_))
            | Err(error @ IoHarnessNodeExecutionError::CompletionSchema(_))
            | Err(error @ IoHarnessNodeExecutionError::MissingSummary)
            | Err(error @ IoHarnessNodeExecutionError::InvalidRoute { .. })
            | Err(error @ IoHarnessNodeExecutionError::InvalidSummary) => {
                self.write_known_failure(&request.key, error.to_string())
            }
            Err(IoHarnessNodeExecutionError::Harness(error)) => {
                let run_id = self
                    .stored_run_id(&request.key)?
                    .or(self.discover_io_run_id(&request.key)?);
                if let Some(run_id) = run_id {
                    self.store_run_id(&request.key, run_id)?;
                    let store = Store::open(self.io_run_path(&request.key))
                        .map_err(|store_error| GraphError::Unsupported(store_error.to_string()))?;
                    let open = store
                        .open_attempts(run_id)
                        .map_err(|store_error| GraphError::Unsupported(store_error.to_string()))?;
                    if let Some(attempt) = open.into_iter().next() {
                        return Ok(NodeExecutionOutcome::WaitingRecovery {
                            attempts: vec![anchor_runtime_rig::graph::RecoveryAttempt {
                                attempt_id: attempt.id,
                                step: attempt.step,
                                tool: attempt.tool,
                                started_at: attempt.started_at,
                            }],
                        });
                    }
                }
                Err(GraphError::Unsupported(format!(
                    "io-harness execution outcome is uncertain; resume is required (run_id={run_id:?}): {error}"
                )))
            }
        }
    }

    fn find_recovery_intent(
        &self,
        key: &InvocationKey,
        run_id: i64,
    ) -> Result<Option<(i64, RecoveryIntent)>, GraphError> {
        let store = Store::open(self.io_run_path(key))
            .map_err(|error| GraphError::Unsupported(error.to_string()))?;
        let attempts = store
            .open_attempts(run_id)
            .map_err(|error| GraphError::Unsupported(error.to_string()))?;
        for attempt in attempts {
            if let Some(intent) = self.read_recovery_intent(key, attempt.id)? {
                if intent.run_id != run_id || intent.attempt_id != attempt.id {
                    return Err(GraphError::CorruptRun(
                        "recovery intent identity mismatch".into(),
                    ));
                }
                return Ok(Some((attempt.id, intent)));
            }
        }
        // Include closed attempts: a process may have died after Harness
        // durably resolved the decision but before Anchor consumed the result.
        // The open-attempt query above is intentionally authoritative; closed
        // pending decisions are discovered from intent files below.
        let prefix = format!("{}.recovery-", Self::stem(key));
        for entry in std::fs::read_dir(&self.facts_root)
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(&prefix) {
                continue;
            }
            let Some(id) = name
                .strip_prefix(&prefix)
                .and_then(|s| s.strip_suffix(".json"))
                .and_then(|s| s.parse::<i64>().ok())
            else {
                continue;
            };
            if let Some(intent) = self.read_recovery_intent(key, id)? {
                if intent.run_id != run_id || intent.attempt_id != id {
                    return Err(GraphError::CorruptRun(
                        "recovery intent identity mismatch".into(),
                    ));
                }
                return Ok(Some((id, intent)));
            }
        }
        Ok(None)
    }

    fn clear_recovery_intents(
        &self,
        key: &InvocationKey,
        retain: Option<i64>,
    ) -> Result<(), GraphError> {
        let prefix = format!("{}.recovery-", Self::stem(key));
        let entries = match std::fs::read_dir(&self.facts_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name
                .strip_prefix(&prefix)
                .and_then(|s| s.strip_suffix(".json"))
                .and_then(|s| s.parse::<i64>().ok())
            else {
                continue;
            };
            if Some(id) != retain {
                Self::remove_if_present(&entry.path())?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
struct RecoveryIntent {
    run_id: i64,
    attempt_id: i64,
    decision: RecoveryDecision,
}

impl RecoveryIntent {
    fn to_value(&self) -> serde_json::Value {
        let decision = match &self.decision {
            RecoveryDecision::Retry => json!({"decision":"retry"}),
            RecoveryDecision::Completed { observation } => {
                json!({"decision":"completed","observation":observation})
            }
            RecoveryDecision::Abort => json!({"decision":"abort"}),
        };
        json!({"run_id":self.run_id,"attempt_id":self.attempt_id,"decision":decision})
    }

    fn from_value(value: serde_json::Value) -> Result<Self, GraphError> {
        let run_id = value
            .get("run_id")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| GraphError::CorruptRun("recovery intent has invalid run_id".into()))?;
        let attempt_id = value
            .get("attempt_id")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| {
                GraphError::CorruptRun("recovery intent has invalid attempt_id".into())
            })?;
        let decision_value = value
            .get("decision")
            .ok_or_else(|| GraphError::CorruptRun("recovery intent has no decision".into()))?;
        let decision = match decision_value
            .get("decision")
            .and_then(serde_json::Value::as_str)
        {
            Some("retry") => RecoveryDecision::Retry,
            Some("abort") => RecoveryDecision::Abort,
            Some("completed") => RecoveryDecision::Completed {
                observation: decision_value
                    .get("observation")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        GraphError::CorruptRun("completed recovery has no observation".into())
                    })?
                    .to_owned(),
            },
            _ => return Err(GraphError::CorruptRun("unknown recovery decision".into())),
        };
        Ok(Self {
            run_id,
            attempt_id,
            decision,
        })
    }
}

fn apply_abort(store: &Store, run_id: i64, attempt_id: i64) -> Result<(), GraphError> {
    let open = store
        .open_attempts(run_id)
        .map_err(|error| GraphError::Unsupported(error.to_string()))?;
    if open.iter().any(|attempt| attempt.id == attempt_id) {
        store
            .resolve_attempt(attempt_id, "abort")
            .map_err(|error| GraphError::Unsupported(error.to_string()))?;
    }
    store
        .finish_run(run_id, "denied")
        .map_err(|error| GraphError::Unsupported(error.to_string()))?;
    Ok(())
}

impl<R: NodeHostResolver + 'static> NodeExecutionPort for IoHarnessNodePort<R> {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: true,
            op_run: false,
            exact_provider_request_budget: false,
        }
    }

    fn resolve_plugins(&self, ids: &[String]) -> Result<Vec<PluginBinding>, GraphError> {
        self.resolver
            .resolve_plugins(ids)
            .map_err(GraphError::Unsupported)
    }

    fn completion_fact<'a>(
        &'a self,
        key: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
        Box::pin(async move { self.completion_fact_inner(key) })
    }

    fn record_recovery_decision(
        &self,
        key: &InvocationKey,
        attempt_id: i64,
        decision: RecoveryDecision,
    ) -> Result<(), GraphError> {
        self.ensure_roots()?;
        let run_id = self
            .stored_run_id(key)?
            .or(self.discover_io_run_id(key)?)
            .ok_or_else(|| {
                GraphError::Unsupported("no persisted io-harness run for recovery".into())
            })?;
        let store = Store::open(self.io_run_path(key))
            .map_err(|error| GraphError::Unsupported(error.to_string()))?;
        let intent = RecoveryIntent {
            run_id,
            attempt_id,
            decision,
        };
        if let Some(existing) = self.read_recovery_intent(key, attempt_id)? {
            if existing == intent {
                if matches!(intent.decision, RecoveryDecision::Abort) {
                    apply_abort(&store, run_id, attempt_id)?;
                }
                return Ok(());
            }
            return Err(GraphError::Unsupported(format!(
                "conflicting recovery decision for attempt {attempt_id}"
            )));
        }
        let attempt = store
            .open_attempts(run_id)
            .map_err(|error| GraphError::Unsupported(error.to_string()))?
            .into_iter()
            .find(|attempt| attempt.id == attempt_id)
            .ok_or_else(|| {
                GraphError::Unsupported(format!(
                    "attempt {attempt_id} is not open in io-harness run {run_id}"
                ))
            })?;
        // Revalidate the attempt immediately before the durable intent write.
        // The host serializes decisions per Run; this protects against stale UI/API input.
        if !store
            .open_attempts(run_id)
            .map_err(|error| GraphError::Unsupported(error.to_string()))?
            .iter()
            .any(|candidate| candidate.id == attempt.id)
        {
            return Err(GraphError::Unsupported(format!(
                "attempt {attempt_id} is no longer open"
            )));
        }
        self.write_recovery_intent_once(key, attempt_id, &intent)?;
        if matches!(intent.decision, RecoveryDecision::Abort) {
            apply_abort(&store, run_id, attempt_id)?;
        }
        Ok(())
    }

    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>> {
        // io-harness 0.86's Store deliberately uses RefCell and therefore its
        // execution future is not Send. NodeExecutionPort is shared by the
        // graph coordinator, whose future must be Send. Keep the non-Send
        // harness loop on a dedicated blocking thread and expose only the
        // thread-safe result to the coordinator.
        let worker = self.clone();
        Box::pin(async move {
            let join = tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| {
                        GraphError::Unsupported(format!(
                            "could not create io-harness worker runtime: {error}"
                        ))
                    })?;
                runtime.block_on(worker.execute_agent(request))
            });
            join.await.map_err(|error| {
                GraphError::Unsupported(format!("io-harness worker thread failed: {error}"))
            })?
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use anchor_runtime_rig::graph::{InvocationKey, NodeExecutionRequest};
    use anchor_runtime_rig::{Cancellation, ToolError};
    use rig_core::completion::ToolDefinition;
    use rig_core::message::ToolResultContent;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};
    use serde_json::json;

    use super::*;

    fn seed_recovery(
        dir: &Path,
        model: rig_core::DynModel<Completion>,
        calls: Arc<AtomicUsize>,
    ) -> (
        IoHarnessNodePort<FixtureResolver>,
        NodeExecutionRequest,
        i64,
    ) {
        let workspace = dir.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let port = IoHarnessNodePort::fixture(
            dir.join("facts"),
            dir.join("io"),
            model,
            Arc::new(FixtureResolver {
                workspace: workspace.clone(),
                calls,
                bindings: vec![],
            }),
        );
        let req = request(
            &workspace,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        std::fs::create_dir_all(&port.io_store_root).unwrap();
        let store = Store::open(port.io_run_path(&req.key)).unwrap();
        let run_id = store
            .start_run("call anchor_echo", workspace.to_str().unwrap())
            .unwrap();
        let attempt_id = store
            .open_attempt(
                run_id,
                1,
                "anchor_echo",
                io_harness::ToolRecovery::Indeterminate,
            )
            .unwrap()
            .unwrap();
        port.store_run_id(&req.key, run_id).unwrap();
        (port, req, attempt_id)
    }

    struct FixtureResolver {
        workspace: PathBuf,
        calls: Arc<AtomicUsize>,
        bindings: Vec<PluginBinding>,
    }

    impl NodeHostResolver for FixtureResolver {
        fn resolve_plugins(&self, ids: &[String]) -> Result<Vec<PluginBinding>, String> {
            ids.iter()
                .map(|id| {
                    self.bindings
                        .iter()
                        .find(|binding| binding.id == *id)
                        .cloned()
                        .ok_or_else(|| format!("fixture has no Plugin binding for `{id}`"))
                })
                .collect()
        }

        fn workspace(&self, _request: &NodeExecutionRequest) -> Result<PathBuf, String> {
            Ok(self.workspace.clone())
        }

        fn tools<'a>(&'a self, _request: &'a NodeExecutionRequest) -> ToolResolution<'a> {
            Box::pin(async move {
                Ok(Arc::new(EchoPort {
                    calls: Arc::clone(&self.calls),
                }) as Arc<dyn ToolPort>)
            })
        }
    }

    struct EchoPort {
        calls: Arc<AtomicUsize>,
    }

    impl ToolPort for EchoPort {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "anchor_echo".into(),
                description: "Echo a value".into(),
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
                    json!({"echo":arguments["value"]}),
                )])
            })
        }
    }

    fn request(_workspace: &Path, cancellation: Cancellation) -> NodeExecutionRequest {
        NodeExecutionRequest {
            key: InvocationKey {
                run_id: "run-1".into(),
                graph_digest: "graph-1".into(),
                node_id: "agent".into(),
                invocation: 1,
            },
            model: Some("fixture".into()),
            task: "call anchor_echo".into(),
            instructions: "Return JSON summary=ok and route=next".into(),
            routes: vec!["next".into()],
            input: json!({}),
            input_commits: vec![],
            plugins: vec![],
            max_provider_requests: None,
            wall_time_limit_seconds: Some(60.0),
            network: false,
            kind: NodeKind::Agent,
            operation: None,
            cancellation,
        }
    }

    #[tokio::test]
    async fn graph_request_publishes_anchor_completion_fact_after_io_finish() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call("call-1", "anchor_echo", json!({"value":"port"})),
            MockTurn::text(r#"{"summary":"ok","route":"next"}"#),
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = Arc::new(FixtureResolver {
            workspace: dir.path().join("workspace"),
            calls: Arc::clone(&calls),
            bindings: Vec::new(),
        });
        std::fs::create_dir_all(&resolver.workspace).unwrap();
        let port = IoHarnessNodePort::fixture(
            dir.path().join("facts"),
            dir.path().join("io"),
            model.erase(),
            resolver,
        );
        let outcome = port
            .execute(request(
                &dir.path().join("workspace"),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ))
            .await
            .unwrap();
        assert!(matches!(outcome, NodeExecutionOutcome::Completed(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            port.completion_fact(&InvocationKey {
                run_id: "run-1".into(),
                graph_digest: "graph-1".into(),
                node_id: "agent".into(),
                invocation: 1,
            })
            .await
            .unwrap(),
            CompletionFact::Completed(_)
        ));
    }

    #[tokio::test]
    async fn provider_error_keeps_started_fact_and_recovery_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockCompletionModel::from_turns([]);
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = Arc::new(FixtureResolver {
            workspace: dir.path().join("workspace"),
            calls,
            bindings: Vec::new(),
        });
        std::fs::create_dir_all(&resolver.workspace).unwrap();
        let port = IoHarnessNodePort::fixture(
            dir.path().join("facts"),
            dir.path().join("io"),
            model.erase(),
            resolver,
        );
        let req = request(
            &dir.path().join("workspace"),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let error = port.execute(req.clone()).await.unwrap_err();
        assert!(matches!(error, GraphError::Unsupported(message) if message.contains("uncertain")));
        assert!(matches!(
            port.completion_fact(&req.key).await.unwrap(),
            CompletionFact::Resumable
        ));
        assert!(
            port.io_run_id_path(&req.key).exists(),
            "the io-harness run id must be recoverable"
        );
        assert!(!port.completion_path(&req.key).exists());
        assert!(!port.failed_path(&req.key).exists());

        let second = port.execute(req).await.unwrap_err();
        assert!(
            matches!(second, GraphError::Unsupported(ref message) if message.contains("run 1") && message.contains("requires recovery")),
            "unexpected retry error: {second}"
        );
        assert!(!second.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn reopened_node_port_requires_recovery_without_replaying_tool() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = Arc::new(FixtureResolver {
            workspace: dir.path().join("workspace"),
            calls: Arc::clone(&calls),
            bindings: Vec::new(),
        });
        std::fs::create_dir_all(&resolver.workspace).unwrap();
        let facts = dir.path().join("facts");
        let io = dir.path().join("io");
        let req = request(
            &resolver.workspace,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );

        let interrupted = IoHarnessNodePort::fixture(
            facts.clone(),
            io.clone(),
            MockCompletionModel::from_turns([MockTurn::tool_call(
                "call-1",
                "anchor_echo",
                json!({"value":"persisted"}),
            )])
            .erase(),
            Arc::clone(&resolver),
        );
        assert!(interrupted.execute(req.clone()).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            interrupted.completion_fact(&req.key).await.unwrap(),
            CompletionFact::Resumable
        ));

        let restarted = IoHarnessNodePort::fixture(
            facts,
            io,
            MockCompletionModel::text(r#"{"summary":"resumed","route":"next"}"#).erase(),
            resolver,
        );
        let error = restarted.execute(req.clone()).await.unwrap_err();
        assert!(
            matches!(error, GraphError::Unsupported(message) if message.contains("requires recovery"))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            restarted.completion_fact(&req.key).await.unwrap(),
            CompletionFact::Resumable
        ));
    }

    #[tokio::test]
    async fn missing_run_id_sidecar_recovers_the_existing_sqlite_run() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (port, req, attempt_id) = seed_recovery(
            dir.path(),
            MockCompletionModel::from_turns([]).erase(),
            calls.clone(),
        );
        std::fs::remove_file(port.io_run_id_path(&req.key)).unwrap();

        let outcome = port.execute(req.clone()).await.unwrap();
        assert!(matches!(
            outcome,
            NodeExecutionOutcome::WaitingRecovery { attempts }
                if attempts.len() == 1 && attempts[0].attempt_id == attempt_id
        ));
        let store = Store::open(port.io_run_path(&req.key)).unwrap();
        assert_eq!(store.runs().unwrap(), vec![1]);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(port.stored_run_id(&req.key).unwrap(), Some(1));
    }

    #[tokio::test]
    async fn undecided_open_attempt_is_projected_as_waiting_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (port, req, attempt_id) = seed_recovery(
            dir.path(),
            MockCompletionModel::from_turns([]).erase(),
            calls.clone(),
        );
        assert!(
            matches!(port.execute(req).await.unwrap(), NodeExecutionOutcome::WaitingRecovery { attempts }
            if attempts.len() == 1 && attempts[0].attempt_id == attempt_id && attempts[0].tool == "anchor_echo")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn completed_decision_resumes_without_replaying_tool_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (port, req, attempt_id) = seed_recovery(
            dir.path(),
            MockCompletionModel::text(r#"{"summary":"ok","route":"next"}"#).erase(),
            calls.clone(),
        );
        let decision = RecoveryDecision::Completed {
            observation: "confirmed externally".into(),
        };
        port.record_recovery_decision(&req.key, attempt_id, decision.clone())
            .unwrap();
        port.record_recovery_decision(&req.key, attempt_id, decision)
            .unwrap();
        let result = port.execute(req.clone()).await.unwrap();
        assert!(matches!(result, NodeExecutionOutcome::Completed(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let store = Store::open(port.io_run_path(&req.key)).unwrap();
        assert!(store.open_attempts(1).unwrap().is_empty());
        assert!(
            store
                .observations(1)
                .unwrap()
                .iter()
                .any(|row| row.text.contains("confirmed externally"))
        );
    }

    #[tokio::test]
    async fn retry_decision_replays_tool_and_abort_does_not_resume_model() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (port, req, attempt_id) = seed_recovery(
            dir.path(),
            MockCompletionModel::from_turns([
                MockTurn::tool_call("retry-call", "anchor_echo", json!({"value":"retry"})),
                MockTurn::text(r#"{"summary":"ok","route":"next"}"#),
            ])
            .erase(),
            calls.clone(),
        );
        port.record_recovery_decision(&req.key, attempt_id, RecoveryDecision::Retry)
            .unwrap();
        assert!(matches!(
            port.execute(req).await.unwrap(),
            NodeExecutionOutcome::Completed(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let abort_dir = tempfile::tempdir().unwrap();
        let abort_calls = Arc::new(AtomicUsize::new(0));
        let (abort_port, abort_req, abort_attempt) = seed_recovery(
            abort_dir.path(),
            MockCompletionModel::from_turns([]).erase(),
            abort_calls.clone(),
        );
        abort_port
            .record_recovery_decision(&abort_req.key, abort_attempt, RecoveryDecision::Abort)
            .unwrap();
        assert!(matches!(
            abort_port.execute(abort_req.clone()).await.unwrap(),
            NodeExecutionOutcome::Aborted
        ));
        // The adapter cannot know that the graph's terminal fact has reached
        // durable storage, so the Abort intent remains a tombstone and a
        // repeated call returns the same terminal result without re-entering.
        assert!(matches!(
            abort_port.execute(abort_req.clone()).await.unwrap(),
            NodeExecutionOutcome::Aborted
        ));
        assert!(
            abort_port
                .recovery_intent_path(&abort_req.key, abort_attempt)
                .exists()
        );
        assert!(abort_port.io_run_id_path(&abort_req.key).exists());
        assert_eq!(abort_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn recovery_decision_rejects_wrong_attempt_and_conflicting_payload() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (port, req, attempt_id) = seed_recovery(
            dir.path(),
            MockCompletionModel::from_turns([]).erase(),
            calls,
        );
        assert!(
            port.record_recovery_decision(&req.key, attempt_id + 999, RecoveryDecision::Retry)
                .is_err()
        );
        let first = RecoveryDecision::Completed {
            observation: "fact A".into(),
        };
        port.record_recovery_decision(&req.key, attempt_id, first)
            .unwrap();
        assert!(
            port.record_recovery_decision(
                &req.key,
                attempt_id,
                RecoveryDecision::Completed {
                    observation: "fact B".into()
                }
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn completed_observation_crash_window_is_repaired_without_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (port, req, attempt_id) = seed_recovery(
            dir.path(),
            MockCompletionModel::text(r#"{"summary":"ok","route":"next"}"#).erase(),
            calls.clone(),
        );
        let decision = RecoveryDecision::Completed {
            observation: "observed once".into(),
        };
        port.record_recovery_decision(&req.key, attempt_id, decision.clone())
            .unwrap();
        // Simulate a crash after the native Harness observation write but before
        // resolve_attempt; reopening uses the same SQLite and intent files.
        let store = Store::open(port.io_run_path(&req.key)).unwrap();
        store
            .record_observations(
                1,
                &[io_harness::context::Observation::new(
                    1,
                    io_harness::context::ObsKind::Tool,
                    Some("anchor_echo".into()),
                    "\n[anchor_echo]\nobserved once\n",
                    io_harness::context::Origin::Tool,
                )],
            )
            .unwrap();
        drop(store);
        drop(port);
        let reopened = IoHarnessNodePort::fixture(
            dir.path().join("facts"),
            dir.path().join("io"),
            MockCompletionModel::text(r#"{"summary":"ok","route":"next"}"#).erase(),
            Arc::new(FixtureResolver {
                workspace: dir.path().join("workspace"),
                calls: calls.clone(),
                bindings: vec![],
            }),
        );
        assert!(matches!(
            reopened.execute(req.clone()).await.unwrap(),
            NodeExecutionOutcome::Completed(_)
        ));
        let store = Store::open(reopened.io_run_path(&req.key)).unwrap();
        assert_eq!(
            store
                .observations(1)
                .unwrap()
                .iter()
                .filter(|row| row.text.contains("observed once"))
                .count(),
            1
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn provider_request_count_reads_cumulative_harness_calls() {
        let dir = tempfile::tempdir().unwrap();
        let port = IoHarnessNodePort::fixture(
            dir.path().join("facts"),
            dir.path().join("io"),
            MockCompletionModel::from_turns([]).erase(),
            Arc::new(FixtureResolver {
                workspace: dir.path().join("workspace"),
                calls: Arc::new(AtomicUsize::new(0)),
                bindings: Vec::new(),
            }),
        );
        let key = InvocationKey {
            run_id: "run-1".into(),
            graph_digest: "graph-1".into(),
            node_id: "agent".into(),
            invocation: 1,
        };
        std::fs::create_dir_all(&port.io_store_root).unwrap();
        let store = Store::open(port.io_run_path(&key)).unwrap();
        let run_id = store.start_run("goal", "workspace").unwrap();
        for step in 1..=3 {
            store
                .record_provider_call(
                    run_id,
                    &io_harness::ProviderCall {
                        step,
                        provider: "fixture".into(),
                        model: Some("mock".into()),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        drop(store);

        assert_eq!(port.provider_request_count(&key, run_id).unwrap(), 3);
    }

    #[tokio::test]
    async fn known_admission_failure_publishes_failed_fact() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockCompletionModel::from_turns([]);
        let resolver = Arc::new(FixtureResolver {
            workspace: dir.path().join("workspace"),
            calls: Arc::new(AtomicUsize::new(0)),
            bindings: Vec::new(),
        });
        let port = IoHarnessNodePort::fixture(
            dir.path().join("facts"),
            dir.path().join("io"),
            model.erase(),
            resolver,
        );
        let mut req = request(
            &dir.path().join("workspace"),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        req.kind = NodeKind::OpRun;
        let outcome = port.execute(req.clone()).await.unwrap();
        assert!(matches!(outcome, NodeExecutionOutcome::Failed { .. }));
        assert!(matches!(
            port.completion_fact(&req.key).await.unwrap(),
            CompletionFact::Failed(_)
        ));
        assert!(!port.started_path(&req.key).exists());
    }

    #[test]
    fn graph_plugin_resolution_uses_host_owned_frozen_bindings() {
        let binding = PluginBinding {
            id: "research-tools".into(),
            digest: "sha256:fixture".into(),
            resources: vec!["SKILL.md".into()],
            mcp_servers: vec!["research-mcp".into()],
        };
        let port = IoHarnessNodePort::fixture(
            std::env::temp_dir().join("anchor-io-plugin-facts"),
            std::env::temp_dir().join("anchor-io-plugin-store"),
            MockCompletionModel::from_turns([]).erase(),
            Arc::new(FixtureResolver {
                workspace: std::env::temp_dir().join("anchor-io-plugin-workspace"),
                calls: Arc::new(AtomicUsize::new(0)),
                bindings: vec![binding.clone()],
            }),
        );

        assert_eq!(
            port.resolve_plugins(&["research-tools".into()]).unwrap(),
            vec![binding]
        );
        assert!(matches!(
            port.resolve_plugins(&["unbound".into()]),
            Err(GraphError::Unsupported(message)) if message.contains("unbound")
        ));
    }
}
