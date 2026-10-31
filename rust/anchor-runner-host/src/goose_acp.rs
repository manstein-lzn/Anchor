mod bridge;
mod completion;
pub(crate) mod configuration;
mod conversation;
pub(crate) use conversation::remove as remove_conversation;
mod elicitation;
mod facts;
mod media;
pub(crate) mod pilot;
pub(crate) mod pilot_interaction;
mod trace;
/// Test-only: lets a test seed the in-process trace of one invocation and
/// assert what the channel progress projection reports for it.
#[cfg(test)]
pub(crate) use trace::LiveTrace;
pub(crate) use trace::{live_notifications, trace_messages};
mod recovery;
mod session;
mod transport;
mod usage;

use crate::{
    create_durable_directory,
    node_host::{
        HostIoResolver, NodeHostResolver, ResolutionError, ResolvedNodeTools, ToolResolution,
    },
    write_durable,
};
use anchor_runtime::{
    Cancellation,
    graph::{
        CompletionFact, GraphError, InvocationKey, NodeCompletion, NodeExecutionOutcome,
        NodeExecutionRequest,
    },
};
use anchor_sandbox_bwrap::BubblewrapSandbox;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use transport::AcpConnection;

/// What the model was shown besides its instructions: Plugin instructions
/// and tool definitions, as digests for the configuration manifest.
struct Disclosed {
    plugin_instructions_sha256: String,
    tools_sha256: String,
}

pub(crate) struct GooseNodePort {
    binary: PathBuf,
    binary_sha256: String,
    model: String,
    models: ExecutionModels,
    fixture: bool,
    /// How this port's Goose sandbox reaches the bridge and the model proxy.
    relay: configuration::RelaySettings,
    facts: PathBuf,
    process_root: PathBuf,
    resolver: Arc<HostIoResolver>,
    sandbox: Arc<BubblewrapSandbox>,
    /// Harness facts about each invocation; absent if its store is unavailable.
    recorder: Option<Arc<crate::fact_recorder::FactRecorder>>,
}

enum ExecutionModels {
    Fixture(Box<configuration::ModelRegistry>),
    Managed(crate::model_configuration::ModelConfiguration),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fact {
    version: u32,
    key: InvocationKey,
    binary_sha256: String,
    session_id: Option<String>,
    completion: Option<NodeCompletion>,
    reason: Option<String>,
    #[serde(default)]
    model_binding: Option<String>,
    #[serde(default)]
    tool_observation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    conversation_scope: Option<String>,
    /// Provider classification of this invocation's latest failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_failure: Option<String>,
}

fn read_fact(path: &Path) -> Result<Option<Fact>, GraphError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| GraphError::CorruptRun("invalid Goose fact path".into()))?;
    let file = match crate::resource_read::open_resource(path.parent().unwrap(), name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() > 8 * 1024 * 1024 {
        return Err(GraphError::CorruptRun(
            "Goose fact exceeds its size limit".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(GraphError::CorruptRun(
            "Goose fact exceeds its size limit".into(),
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| GraphError::CorruptRun(error.to_string()))
}

/// Read the durable execution claim without preparing a provider or loading a
/// native Session. Retention must also cover interrupted, uncommitted history.
pub(crate) fn has_invocation_fact(state: &Path, key: &InvocationKey) -> Result<bool, GraphError> {
    let stem = crate::host_fs::invocation_stem(key);
    let mut started = false;
    for (directory, version) in [("goose-acp", 2), ("goose-acp-spike", 1)] {
        if let Some(fact) = read_fact(&state.join(directory).join(format!("{stem}.json")))? {
            if fact.version != version || fact.key != *key {
                return Err(GraphError::CorruptRun(
                    "Goose invocation identity changed".into(),
                ));
            }
            started = true;
        }
    }
    Ok(started)
}

struct CancelTools(Cancellation);

impl Drop for CancelTools {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

pub(crate) fn runtime_mode() -> String {
    env::var("ANCHOR_RUNNER_AGENT_RUNTIME").unwrap_or_else(|_| "goose".into())
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn store_fact(path: &Path, fact: &Fact) -> Result<(), GraphError> {
    let temporary = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&temporary)?;
    file.write_all(
        &serde_json::to_vec_pretty(fact)
            .map_err(|error| GraphError::CorruptRun(error.to_string()))?,
    )?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

/// The Goose process must opt into exactly how it reaches the network.
///
/// An isolated sandbox is its own decision; sharing the host network needs the
/// explicit `ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1`, because a shared network is not
/// loopback-only OS isolation.
const NETWORK_OPT_IN_ERROR: &str = "Goose requires explicit ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1 to share the host network, or ANCHOR_GOOSE_LOCAL_NETWORK=1 to run in an isolated network namespace behind the sandbox relay";

fn network_opt_in(local_network: bool, shared_authorized: bool) -> bool {
    local_network || shared_authorized
}

/// Why one Goose invocation did not produce a node completion.
///
/// This decides whether the Run may resume the invocation. A definitive ACP
/// protocol or completion-contract violation is known to have produced no
/// completion, so a resume could only replay work that already happened; those
/// become terminal node failures. Everything else (transport, session, tools,
/// host IO, cleanup) leaves the outcome unknown, so the invocation stays
/// resumable and a human or the agent must inspect the scene first.
#[derive(Debug)]
enum InvocationFailure {
    /// Deterministic completion-contract violation reported by the agent layer.
    Contract(String),
    /// Unknown or externally caused failure; the invocation stays resumable.
    Uncertain(String),
    /// The model provider rejected the prompt, as classified by validated ACP
    /// error metadata (`authentication`, `credits_exhausted`, `provider_error`).
    /// Ordinary Graphs keep it resumable like `Uncertain`; a persistent
    /// assistant ends the Turn with a Host reply instead of stopping.
    Provider { kind: &'static str, reason: String },
}

impl InvocationFailure {
    fn reason(&self) -> &str {
        match self {
            Self::Contract(reason) | Self::Uncertain(reason) | Self::Provider { reason, .. } => {
                reason
            }
        }
    }
}

/// Plain string errors default to resumable: only the explicit contract checks
/// below may turn an invocation into a terminal failure.
impl From<String> for InvocationFailure {
    fn from(reason: String) -> Self {
        Self::Uncertain(reason)
    }
}

impl From<&str> for InvocationFailure {
    fn from(reason: &str) -> Self {
        Self::Uncertain(reason.to_owned())
    }
}

/// Map a failed invocation to a node outcome.
///
/// The fixture spike keeps its historical behaviour (any non-cancel failure is
/// terminal) because it exists to exercise spike negatives. Native invocations
/// only fail terminally when the agent layer itself reported a contract
/// violation; unknown outcomes stay `Interrupted` so they can be resumed after
/// inspection.
fn failure_outcome(
    fixture: bool,
    cancelled: bool,
    failure: InvocationFailure,
) -> NodeExecutionOutcome {
    if cancelled {
        return NodeExecutionOutcome::Cancelled;
    }
    if fixture {
        return NodeExecutionOutcome::Failed {
            reason: failure.reason().to_owned(),
        };
    }
    match failure {
        InvocationFailure::Contract(reason) => NodeExecutionOutcome::Failed { reason },
        InvocationFailure::Uncertain(reason) | InvocationFailure::Provider { reason, .. } => {
            NodeExecutionOutcome::Interrupted { reason }
        }
    }
}

async fn resolve_node_tools(
    mut resolving: ToolResolution<'_>,
    cancellation: &Cancellation,
    tool_cancellation: &Cancellation,
    deadline: Option<tokio::time::Instant>,
) -> Result<Option<ResolvedNodeTools>, ResolutionError> {
    // Keep the resolver's completed result separate from interruption of its
    // wait. A completed error must never cause this future to be polled again.
    let result = transport::bounded(
        async { Ok::<_, String>(resolving.as_mut().await) },
        cancellation,
        deadline,
    )
    .await;
    match result {
        Ok(resolved) => resolved.map(Some),
        Err(error) => {
            // Only an interrupted, unfinished resolver needs draining. It owns
            // any MCP child and must receive cancellation before reaping it.
            tool_cancellation.store(true, Ordering::SeqCst);
            let _ = resolving.await;
            if cancellation.load(Ordering::Acquire) {
                Ok(None)
            } else {
                Err(ResolutionError::Host(error))
            }
        }
    }
}

impl GooseNodePort {
    pub(crate) fn from_env(
        state: &Path,
        work_root: &Path,
        resolver: Arc<HostIoResolver>,
        sandbox: Arc<BubblewrapSandbox>,
    ) -> Result<Option<Self>, String> {
        let mode = runtime_mode();
        if !matches!(mode.as_str(), "goose-acp-spike" | "goose") {
            return Err(format!("unknown Agent runtime `{mode}`"));
        }
        let fixture = mode == "goose-acp-spike";
        let models = if fixture {
            ExecutionModels::Fixture(Box::new(configuration::ModelRegistry::from_env(true)?))
        } else {
            // Provider settings may be saved after the Host starts. The native
            // runtime is available independently; each invocation resolves its
            // own model immediately before execution.
            if env::var_os("ANCHOR_GOOSE_BINARY").is_none() {
                return Ok(None);
            }
            ExecutionModels::Managed(crate::model_configuration::ModelConfiguration::from_env(
                state,
            )?)
        };
        if state.join("io-harness").exists() {
            return Err(
                "Goose requires a separate state root; existing io-harness state is not migrated"
                    .into(),
            );
        }
        // Opt-in isolation: the sandbox keeps its own network namespace and reaches
        // the bridge through the in-sandbox relay instead of sharing host networking.
        let (binary, binary_sha256) = configuration::binary()?;
        let relay = configuration::RelaySettings::from_env(&binary)?;
        let shared_authorized = env::var("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK").as_deref() == Ok("1");
        if !network_opt_in(relay.is_isolated(), shared_authorized) {
            return Err(NETWORK_OPT_IN_ERROR.into());
        }
        let model = match &models {
            ExecutionModels::Fixture(models) => models.resolve(None)?.model,
            ExecutionModels::Managed(_) => String::new(),
        };
        let facts = state.join(if fixture {
            "goose-acp-spike"
        } else {
            "goose-acp"
        });
        if fixture && state.join("goose-acp").exists() {
            return Err("Goose native state cannot silently switch to the spike".into());
        }
        if !fixture && state.join("goose-acp-spike").exists() {
            return Err(
                "Goose native requires a new state root; spike facts are not migrated".into(),
            );
        }
        let process_root = work_root.join(".goose-process");
        create_durable_directory(&facts).map_err(|error| error.to_string())?;
        create_durable_directory(&process_root).map_err(|error| error.to_string())?;
        // Recording is best effort and never a reason to refuse execution.
        let recorder = match crate::fact_recorder::FactRecorder::shared(state) {
            Ok(recorder) => Some(recorder),
            Err(error) => {
                eprintln!("Harness fact recording unavailable: {error}");
                None
            }
        };
        Ok(Some(Self {
            binary,
            binary_sha256,
            model,
            models,
            fixture,
            relay,
            facts,
            process_root,
            resolver,
            sandbox,
            recorder,
        }))
    }

    pub(crate) fn completion_fact(
        &self,
        key: &InvocationKey,
    ) -> Result<CompletionFact, GraphError> {
        let Some(fact) = read_fact(
            &self
                .facts
                .join(format!("{}.json", crate::host_fs::invocation_stem(key))),
        )?
        else {
            return Ok(CompletionFact::NotStarted);
        };
        if fact.version != if self.fixture { 1 } else { 2 }
            || fact.key != *key
            || fact.binary_sha256 != self.binary_sha256
        {
            return Err(GraphError::CorruptRun(
                "Goose invocation identity changed".into(),
            ));
        }
        Ok(match fact.completion {
            Some(completion) => CompletionFact::Completed(completion),
            None if !self.fixture && fact.version == 2 => CompletionFact::Resumable,
            None => CompletionFact::Failed(
                fact.reason.unwrap_or_else(|| {
                    "Goose invocation started without a durable node completion; automatic load/replay is refused in this spike".into()
                }),
            ),
        })
    }

    /// The provider classification recorded by this invocation's latest
    /// failed attempt, if the provider rejected it.
    pub(crate) fn provider_failure(
        &self,
        key: &InvocationKey,
    ) -> Result<Option<String>, GraphError> {
        Ok(read_fact(
            &self
                .facts
                .join(format!("{}.json", crate::host_fs::invocation_stem(key))),
        )?
        .filter(|fact| fact.key == *key && fact.completion.is_none())
        .and_then(|fact| fact.provider_failure))
    }

    pub(crate) async fn execute(
        &self,
        mut request: NodeExecutionRequest,
    ) -> Result<NodeExecutionOutcome, GraphError> {
        match self.completion_fact(&request.key)? {
            CompletionFact::Completed(completion) => {
                return Ok(NodeExecutionOutcome::Completed(completion));
            }
            CompletionFact::NotStarted => {}
            CompletionFact::Resumable if !self.fixture => {}
            _ => {
                return Err(GraphError::Unsupported(
                    "Goose automatic replay is refused".into(),
                ));
            }
        }
        if request.cancellation.load(Ordering::Relaxed) {
            return Ok(NodeExecutionOutcome::Cancelled);
        }
        let started_at_ms = crate::fact_recorder::now_ms();
        if self.fixture
            && request
                .model
                .as_deref()
                .is_some_and(|model| model != self.model)
        {
            return Err(GraphError::Unsupported(
                "Goose spike only accepts its explicitly configured fixture model".into(),
            ));
        }
        let conversation_hint = self
            .resolver
            .conversation_hint(&request)
            .map_err(GraphError::Unsupported)?;
        let prompt_images = self
            .resolver
            .prompt_images(&request)
            .map_err(GraphError::Unsupported)?;
        if request.max_provider_requests.is_some()
            || (self.fixture
                && (!request.plugins.is_empty()
                    || conversation_hint
                        .as_ref()
                        .is_some_and(|hint| hint.cross_run)
                    || !prompt_images.is_empty()))
        {
            return Err(GraphError::Unsupported(
                if self.fixture { "Goose spike does not support Plugins, conversation continuation, media or exact cumulative budgets" }
                else { "Goose does not support configured cumulative request budgets" }.into(),
            ));
        }
        let timeout = request
            .wall_time_limit_seconds
            .or(self.fixture.then_some(30.0))
            .map(Duration::try_from_secs_f64)
            .transpose()
            .map_err(|_| GraphError::Unsupported("invalid Goose wall-time limit".into()))?;
        if timeout.is_some_and(|timeout| {
            timeout.is_zero() || (self.fixture && timeout > Duration::from_secs(180))
        }) {
            return Err(GraphError::Unsupported(
                "Goose spike wall time must be within 0–180 seconds".into(),
            ));
        }
        let deadline = timeout.map(|timeout| tokio::time::Instant::now() + timeout);
        let stem = crate::host_fs::invocation_stem(&request.key);
        let fact_path = self.facts.join(format!("{stem}.json"));
        let retained = read_fact(&fact_path)?;
        let retained_binding = retained
            .as_ref()
            .and_then(|fact| fact.model_binding.as_deref());
        let models = match &self.models {
            ExecutionModels::Fixture(models) => models.as_ref().clone(),
            ExecutionModels::Managed(configuration) => configuration
                .execution_snapshot(retained_binding, request.model.as_deref())
                .map_err(GraphError::Unsupported)?,
        };
        let binding = models
            .resolve(request.model.as_deref())
            .map_err(GraphError::Unsupported)?;
        let resumed = retained.is_some();
        let mut conversation = match conversation_hint.filter(|_| !self.fixture) {
            Some(hint) => {
                match conversation::ConversationScope::open(
                    &self.process_root,
                    &hint.key,
                    &request.key,
                    &self.facts,
                    &self.binary_sha256,
                    &binding.identity,
                    retained.as_ref(),
                    &self
                        .resolver
                        .conversation_predecessors(&request.key)
                        .map_err(GraphError::Unsupported)?,
                ) {
                    Ok(scope) => Some(scope),
                    // Identity rejection precedes this invocation's model and
                    // tools. It is a known node failure, not an unknown effect.
                    Err(GraphError::CorruptRun(reason)) => {
                        return Ok(NodeExecutionOutcome::Failed { reason });
                    }
                    Err(error) => return Err(error),
                }
            }
            None if retained
                .as_ref()
                .is_some_and(|fact| fact.conversation_scope.is_some()) =>
            {
                return Err(GraphError::CorruptRun(
                    "Goose invocation lost its conversation identity".into(),
                ));
            }
            None => None,
        };
        let directory = conversation.as_ref().map_or_else(
            || self.process_root.join(&stem),
            |scope| scope.root.join("process"),
        );
        crate::host_fs::directory(&directory).map_err(GraphError::Unsupported)?;
        crate::host_fs::directory(&directory.join("config")).map_err(GraphError::Unsupported)?;
        write_durable(
            &directory.join("config/config.yaml"),
            b"GOOSE_MODE: auto\nextensions:\n  developer:\n    name: developer\n    type: builtin\n    enabled: false\n",
        )?;
        let mut tool_request = request.clone();
        tool_request.cancellation = Arc::new(AtomicBool::new(false));
        let _stop_tools = CancelTools(tool_request.cancellation.clone());
        let resolved = match resolve_node_tools(
            self.resolver.tools(&tool_request),
            &request.cancellation,
            &tool_request.cancellation,
            deadline,
        )
        .await
        {
            Ok(Some(resolved)) => resolved,
            Ok(None) => return Ok(NodeExecutionOutcome::Cancelled),
            Err(ResolutionError::KnownFailure(reason)) => {
                return Ok(NodeExecutionOutcome::Failed { reason });
            }
            Err(error) => return Err(error.into_graph_error()),
        };
        // Tools, Plugin identities and persistent summary come from one Host preparation.
        // This includes Session mounts and Library updates since dispatch.
        request.plugins = resolved.plugins;
        let plugin_instructions_sha256 = anchor_facts::text_digest(&resolved.plugin_instructions);
        let definitions = resolved.tools.definitions();
        let tools_sha256 = anchor_facts::tools_digest(definitions.iter().map(|tool| {
            (
                tool.name.as_str(),
                tool.description.as_str(),
                &tool.parameters,
            )
        }));
        let mut random = [0u8; 32];
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let token = format!("{:x}", Sha256::digest(random));
        let bridge = bridge::Bridge::start(
            resolved.tools,
            bridge::NodeBridgeContext {
                routes: request.routes.clone(),
                plugin_instructions: resolved.plugin_instructions,
                observation_path: (!self.fixture).then(|| fact_path.clone()),
                outputs: bridge::DeclaredOutputs {
                    workspace: resolved.workspace,
                    writes: request.writes.clone(),
                },
            },
            tool_request.cancellation.clone(),
            models.model_upstream(),
            self.fixture,
            token,
        )
        .await
        .map_err(GraphError::Unsupported)?;
        // Expose the same bridge on a UNIX socket so a sandbox that shares no
        // network with the host can reach it through `anchor-net-relay`. The path
        // must stay short: the kernel caps a socket path at `sun_path`.
        let bridge_socket = std::env::temp_dir().join(format!(
            "anchor-bridge-{}.sock",
            &format!("{:x}", Sha256::digest(directory.display().to_string()))[..16]
        ));
        bridge
            .expose_on_unix_socket(&bridge_socket)
            .map_err(GraphError::Unsupported)?;
        let transport = self.relay.transport(bridge_socket.clone());
        let endpoint = transport.endpoint(&bridge.url);
        // State the node boundary every turn through Goose's persistent
        // instructions: mounts, network, budget and the host authorization rule.
        let mut environment = models.environment(&binding, &endpoint, &bridge.token);
        if !self.fixture {
            // Only AgentNode opts into Goose's native completion boundary.
            // Sandbox environments are cleared before these explicit values;
            // Pilot and the legacy spike transport never receive this setting.
            environment.push(anchor_runtime::SandboxEnvironment::new(
                "GOOSE_EXTERNAL_COMPLETION_TOOL",
                "anchor__final_result",
            ));
        }
        environment.push(anchor_runtime::SandboxEnvironment::new(
            "GOOSE_MOIM_MESSAGE_TEXT",
            configuration::boundary_text(&configuration::BoundaryFacts {
                isolated: self.relay.is_isolated(),
                wall_clock: timeout,
                disclosure: crate::tool_disclosure::forced(),
            }),
        ));
        let command = configuration::command(
            &self.sandbox,
            &directory,
            &self.binary,
            environment,
            request.cancellation.clone(),
            &transport,
        )
        .map_err(GraphError::Unsupported)?;
        let mut fact = retained.unwrap_or_else(|| Fact {
            version: if self.fixture { 1 } else { 2 },
            key: request.key.clone(),
            binary_sha256: self.binary_sha256.clone(),
            session_id: conversation.as_ref().and_then(|scope| scope.session_id()),
            completion: None,
            reason: None,
            model_binding: Some(binding.identity.clone()),
            tool_observation: conversation
                .as_ref()
                .and_then(|scope| scope.previous_observation.clone()),
            conversation_scope: conversation.as_ref().map(|scope| scope.scope.clone()),
            provider_failure: None,
        });
        if !self.fixture && fact.model_binding.as_deref() != Some(binding.identity.as_str()) {
            bridge.close().await.map_err(GraphError::Unsupported)?;
            return Err(GraphError::Unsupported("Goose model or endpoint binding changed; retained invocation cannot silently switch".into()));
        }
        // A resumed attempt is not yet classified; never let an earlier
        // attempt's provider rejection describe this one.
        fact.provider_failure = None;
        store_fact(&fact_path, &fact)?;
        if let Some(scope) = &mut conversation {
            scope.claim(&fact).map_err(GraphError::Unsupported)?;
        }
        let live = match trace::LiveTrace::open(&fact_path) {
            Ok(live) => live,
            Err(error) => {
                bridge.close().await.map_err(GraphError::Unsupported)?;
                return Err(GraphError::Unsupported(error));
            }
        };
        let mut connection = AcpConnection::spawn(command)
            .await
            .map_err(GraphError::Unsupported)?;
        let mut evidence = json!({"version":1,"runtime":if self.fixture {"goose-acp-spike"} else {"goose"},"key":request.key,
            "binary_sha256":self.binary_sha256,"model":binding.model,"notifications":[],
            "fixture_transport":if self.fixture {Some("loopback")} else {None},"real_model_calls":null,
            "model_binding":binding.identity,"resume":resumed,"continuation":!resumed && fact.session_id.is_some(),"budget_enforcement":false});
        let mut result = self
            .prompt(
                &mut connection,
                &request,
                &bridge,
                &endpoint,
                &binding,
                deadline,
                &fact_path,
                &mut fact,
                &mut evidence,
                conversation.as_mut(),
                &prompt_images,
                &live,
            )
            .await;
        if result.is_err()
            && let Some(session_id) = &fact.session_id
        {
            let _ = connection
                .notify("session/cancel", json!({"sessionId":session_id}))
                .await;
        }
        let close = connection.close().await;
        let tool_close = bridge.close().await;
        evidence["provider_requests"] = if self.fixture {
            json!(bridge.state.provider_calls.load(Ordering::SeqCst))
        } else {
            Value::Null
        };
        evidence["tool_calls"] = json!(bridge.state.calls.lock().await.clone());
        evidence["tool_calls_dropped"] = json!(bridge.state.dropped_calls.load(Ordering::SeqCst));
        evidence["process_directory"] = json!(directory);
        evidence["bridge_socket"] = json!(bridge_socket);
        evidence["sandbox_network"] = json!(if self.relay.is_isolated() {
            "isolated"
        } else {
            "shared"
        });
        evidence["close_error"] = json!(close.as_ref().err());
        evidence["tool_close_error"] = json!(tool_close.as_ref().err());
        if request.cancellation.load(Ordering::Relaxed) {
            result = Err("Goose invocation was cancelled before publication".into());
        } else if let Some(error) = close.err().or_else(|| tool_close.err()) {
            result = Err(format!("Goose cleanup failed: {error}").into());
        }
        let outcome = match &result {
            Ok(_) => anchor_facts::Outcome::Completed,
            Err(_) if request.cancellation.load(Ordering::Relaxed) => {
                anchor_facts::Outcome::Cancelled
            }
            Err(InvocationFailure::Uncertain(_) | InvocationFailure::Provider { .. })
                if !self.fixture =>
            {
                anchor_facts::Outcome::Interrupted
            }
            Err(_) => anchor_facts::Outcome::Failed,
        };
        evidence["fact_recording"] = self
            .record_facts(
                &request,
                &binding,
                Disclosed {
                    plugin_instructions_sha256,
                    tools_sha256,
                },
                &evidence,
                started_at_ms,
                outcome,
            )
            .await;
        trace::store_evidence(&self.facts.join(format!("{stem}.evidence.json")), &evidence)?;
        if !self.fixture {
            fact = serde_json::from_slice(&fs::read(&fact_path)?)
                .map_err(|error| GraphError::CorruptRun(error.to_string()))?;
        }
        match result {
            Ok(completion) => {
                fact.completion = Some(completion.clone());
                fact.reason = None;
                fact.provider_failure = None;
                store_fact(&fact_path, &fact)?;
                Ok(NodeExecutionOutcome::Completed(completion))
            }
            Err(failure) => {
                let reason = failure.reason().to_owned();
                fact.provider_failure = match &failure {
                    InvocationFailure::Provider { kind, .. }
                        if !request.cancellation.load(Ordering::Relaxed) =>
                    {
                        Some((*kind).to_owned())
                    }
                    _ => None,
                };
                fact.reason = Some(format!(
                    "{reason}; native Goose history retained; {}",
                    if self.fixture {
                        "automatic replay is refused"
                    } else {
                        "resume must inspect existing facts before continuing"
                    }
                ));
                store_fact(&fact_path, &fact)?;
                Ok(failure_outcome(
                    self.fixture,
                    request.cancellation.load(Ordering::Relaxed),
                    failure,
                ))
            }
        }
    }

    /// Record this invocation's harness facts under the configuration the
    /// host froze for it. Never fails the invocation.
    async fn record_facts(
        &self,
        request: &NodeExecutionRequest,
        binding: &configuration::ModelBinding,
        disclosed: Disclosed,
        evidence: &Value,
        started_at_ms: u64,
        outcome: anchor_facts::Outcome,
    ) -> Value {
        let Some(recorder) = self.recorder.clone() else {
            return json!({"unavailable": true});
        };
        let mut configuration = anchor_facts::AgentConfiguration {
            version: anchor_facts::VERSION,
            graph_digest: request.key.graph_digest.clone(),
            node_id: request.key.node_id.clone(),
            model: binding.model.clone(),
            model_binding: binding.identity.clone(),
            runtime_sha256: self.binary_sha256.clone(),
            instructions_sha256: anchor_facts::text_digest(&format!(
                "{}\n{}",
                request.instructions, request.task
            )),
            plugin_instructions_sha256: disclosed.plugin_instructions_sha256,
            plugins: request
                .plugins
                .iter()
                .map(|plugin| anchor_facts::PluginIdentity {
                    id: plugin.id.clone(),
                    digest: plugin.digest.clone(),
                    authorization: plugin.authorization_digest.to_string(),
                })
                .collect(),
            routes: request.routes.clone(),
            // Filled in off the async runtime below: hashing may still be
            // finishing on the first invocation.
            host_sha256: String::new(),
            tools_sha256: disclosed.tools_sha256,
        };
        let invocation = anchor_facts::Invocation {
            run_id: request.key.run_id.clone(),
            graph_digest: request.key.graph_digest.clone(),
            node_id: request.key.node_id.clone(),
            invocation: request.key.invocation,
        };
        let facts = facts::observations(
            evidence,
            started_at_ms,
            crate::fact_recorder::now_ms(),
            outcome,
        );
        match tokio::task::spawn_blocking(move || {
            configuration.host_sha256 = crate::fact_recorder::host_sha256();
            recorder.record(&configuration, &invocation, facts)
        })
        .await
        {
            Ok(recording) => json!(recording),
            Err(error) => json!({"error": error.to_string()}),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn prompt(
        &self,
        connection: &mut AcpConnection,
        request: &NodeExecutionRequest,
        bridge: &bridge::Bridge,
        endpoint: &str,
        binding: &configuration::ModelBinding,
        deadline: Option<tokio::time::Instant>,
        fact_path: &Path,
        fact: &mut Fact,
        evidence: &mut Value,
        conversation: Option<&mut conversation::ConversationScope>,
        prompt_images: &[crate::node_host::NodeImage],
        live: &trace::LiveTrace,
    ) -> Result<NodeCompletion, InvocationFailure> {
        let restored = fact.session_id.clone();
        let opened = session::open(
            connection,
            bridge,
            endpoint,
            restored.as_deref(),
            binding,
            &request.cancellation,
            deadline,
            session::ClientCapabilities {
                custom_notifications: !self.fixture,
                form_elicitation: false,
            },
        )
        .await?;
        evidence["initialize"] = opened.initialize;
        let history = opened.history;
        evidence["restored_history"] = json!(history);
        evidence["session"] = opened.response;
        let session_id = opened.id;
        fact.session_id = Some(session_id.clone());
        store_fact(fact_path, fact).map_err(|error| error.to_string())?;
        if let Some(scope) = conversation {
            scope.claim(fact)?;
        }
        live.restore(&session_id, &history)?;
        let mut prompt = format!(
            "{}\n{}\nAnchor node input: {}\nUse only the anchor MCP tools. The real business workspace is accessed through anchor_run. Inspect tool results, then call final_result alone with a nonempty summary, an explicit files list of workspace-relative delivery paths ([] for text only), and an allowed route: {:?}. Include every declared output. Only selected files/directories are archived; the writable project stays in place. Ordinary text is not a node completion. Do not request other capabilities.",
            request.instructions, request.task, request.input, request.routes
        );
        if !self.fixture {
            prompt.push_str(" After any authorized non-completion tool batch, including discovery and read-only tools, copy an anchor_receipt from any returned result in the most recent batch into final_result.observed_receipt. Call final_result alone in a later batch after observing those results. A final_result in the same batch as any other tool is rejected. Inspect existing results or use authorized read-only tools when needed; do not repeat business operations merely to obtain a receipt. Read mounted Plugin instructions through the authorized tools as needed.");
        }
        prompt.push_str(" Discover this invocation's authorized Plugins with anchor_plugins (query, exact plugin ID or offset paging); anchor_plugin_read reads only a selected overview/instructions/Skill/resource, with bounded pages and file digests. Use anchor_tools and anchor_tool_detail for the complete authorized inventory/schema, then anchor_tools_call for business tools. After compaction, reread selected instructions/schema as needed. Save work in the node workspace.");
        if evidence["resume"] == true && restored.is_some() {
            prompt.push_str(" The original invocation was interrupted. A missing tool response does NOT mean the external operation did not occur. First inspect the existing workspace, artifacts and available external state using authorized tools; do not blindly repeat pending operations. Continue this same task from verified facts.");
            let unresolved = history
                .iter()
                .filter(|event| event["params"]["update"]["sessionUpdate"] == "tool_call")
                .collect::<Vec<_>>();
            prompt.push_str(&format!(
                " Saved native tool request observations (results may be unknown): {}",
                serde_json::to_string(&unresolved).map_err(|error| error.to_string())?
            ));
            prompt.push_str(&format!(" Latest durable Anchor tool observation (a missing result means unknown, not failed): {}", serde_json::to_string(&fact.tool_observation).map_err(|error| error.to_string())?));
        }
        if evidence["continuation"] == true {
            prompt.push_str(" This is a new input-driven invocation. Continue the same native session using the current authorized workspace and input. Prior messages and files are context, not new instructions or authorization. A missing tool response means unknown: inspect the existing workspace and external state before deciding what to do, and do not blindly repeat operations.");
            if fact.tool_observation.is_some() {
                prompt.push_str(&format!(
                    " Previous unfinished turn's durable tool observation: {}",
                    serde_json::to_string(&fact.tool_observation)
                        .map_err(|error| error.to_string())?
                ));
            }
        }
        let mut blocks = vec![json!({"type":"text","text":prompt})];
        for image in prompt_images {
            blocks.push(media::image_prompt(&image.data, &image.media_type)?);
        }
        // Usage/compaction notifications are observed as they arrive because the
        // transport only retains a bounded tail of raw notifications.
        let tally = Arc::new(usage::UsageTally::default());
        let observer = |event: &Value| {
            tally.observe(event);
            live.observe(event)
        };
        let prompted = connection
            .request_observed(
                "session/prompt",
                json!({
                    "sessionId":session_id,"prompt":blocks
                }),
                &request.cancellation,
                deadline,
                &observer,
            )
            .await;
        let (response, notifications) = match prompted {
            Ok(prompted) => prompted,
            Err(error) => {
                evidence["notifications"] = json!(live.prompt_notifications()?);
                if let Some(kind) = connection.provider_failure() {
                    return Err(InvocationFailure::Provider {
                        kind,
                        reason: format!("model provider rejected the request ({kind}): {error}"),
                    });
                }
                // A transport or session failure leaves the real outcome unknown:
                // keep the invocation resumable so it can be inspected first.
                return Err(error.into());
            }
        };
        evidence["prompt_result"] = response.clone();
        evidence["notifications"] = json!(notifications);
        let metrics = tally.metrics();
        evidence["usage"] = metrics.clone();
        evidence["observed_model_requests"] = metrics["messages"].clone();
        evidence["compaction_messages"] = metrics["compaction_messages"].clone();
        evidence["model_requests_scope"] = json!(if self.fixture {
            "fixture_proxy_requests"
        } else {
            "observed_usage_notifications_of_current_prompt"
        });
        if response["stopReason"] != "end_turn" {
            return Err(InvocationFailure::Contract(format!(
                "Goose prompt stopped without completion: {}",
                response["stopReason"]
            )));
        }
        if self.fixture && *bridge.state.after_completion.lock().await {
            return Err(InvocationFailure::Contract(
                "Goose attempted tools after final_result".into(),
            ));
        }
        let output = bridge
            .state
            .completion
            .lock()
            .await
            .clone()
            .ok_or_else(|| {
                // The agent ended its turn without the completion tool. That is
                // recoverable and deliberately stays resumable: the retained
                // native session can be asked to inspect the scene and finish
                // (the media fail-closed scenarios depend on this). Only
                // protocol-level violations that no resume can repair are
                // classified as `Contract`.
                InvocationFailure::Uncertain("Goose ended without a validated final_result".into())
            })?;
        Ok(NodeCompletion {
            submission: output["summary"].as_str().unwrap().to_owned(),
            route: output["route"].as_str().map(str::to_owned),
            model_requests: if self.fixture {
                bridge.state.provider_calls.load(Ordering::SeqCst)
            } else {
                metrics["messages"].as_u64().unwrap_or(0)
            },
            output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completed_tool_resolution_errors_are_returned_without_repolling() {
        for cause in [
            ResolutionError::Host("MCP command is not authorized".into()),
            ResolutionError::Graph(GraphError::CorruptRun("frozen Plugin changed".into())),
            ResolutionError::KnownFailure("MCP startup rejected".into()),
        ] {
            let cancellation = Cancellation::default();
            let tool_cancellation = Cancellation::default();
            let expected = cause.to_string();
            let graph_error = matches!(cause, ResolutionError::Graph(_));
            let known_failure = matches!(cause, ResolutionError::KnownFailure(_));
            let resolving = Box::pin(async move { Err(cause) });
            let error = resolve_node_tools(resolving, &cancellation, &tool_cancellation, None)
                .await
                .err()
                .expect("resolver failure must be returned");
            assert!(error.to_string().contains(&expected));
            assert_eq!(
                matches!(error, ResolutionError::Graph(GraphError::CorruptRun(_))),
                graph_error
            );
            assert_eq!(
                matches!(error, ResolutionError::KnownFailure(_)),
                known_failure
            );
            assert!(!tool_cancellation.load(Ordering::Acquire));
        }
    }

    #[tokio::test]
    async fn interrupted_tool_resolution_is_cancelled_and_drained_before_returning() {
        for expired in [false, true] {
            let cancellation = Cancellation::default();
            let tool_cancellation = Cancellation::default();
            let settled = Arc::new(AtomicBool::new(false));
            let (started, entered) = tokio::sync::oneshot::channel();
            let operation_stop = tool_cancellation.clone();
            let operation_settled = settled.clone();
            let resolving = Box::pin(async move {
                started.send(()).unwrap();
                while !operation_stop.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                tokio::task::yield_now().await;
                operation_settled.store(true, Ordering::SeqCst);
                Err(ResolutionError::Host("MCP child reaped".into()))
            });
            let outer_stop = cancellation.clone();
            let cancel = async move {
                entered.await.unwrap();
                if !expired {
                    outer_stop.store(true, Ordering::SeqCst);
                }
            };
            let deadline = expired.then(|| tokio::time::Instant::now() + Duration::from_millis(20));
            let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(
                    resolve_node_tools(resolving, &cancellation, &tool_cancellation, deadline),
                    cancel,
                )
            })
            .await
            .expect("interrupted resolver must settle");
            assert!(tool_cancellation.load(Ordering::Acquire));
            assert!(settled.load(Ordering::Acquire));
            if expired {
                assert!(
                    result
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("deadline exceeded")
                );
            } else {
                assert!(result.unwrap().is_none());
            }
        }
    }

    #[test]
    fn goose_network_opt_in_needs_one_explicit_mode() {
        assert!(!network_opt_in(false, false));
        assert!(network_opt_in(true, false));
        assert!(network_opt_in(false, true));
        assert!(network_opt_in(true, true));
        assert!(NETWORK_OPT_IN_ERROR.contains("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1"));
    }

    #[test]
    fn contract_violations_fail_the_node_instead_of_leaving_it_resumable() {
        for reason in [
            "Goose prompt stopped without completion: max_tokens",
            "Goose attempted tools after final_result",
        ] {
            assert!(
                matches!(
                    failure_outcome(false, false, InvocationFailure::Contract(reason.to_owned())),
                    NodeExecutionOutcome::Failed { .. }
                ),
                "{reason}"
            );
        }
    }

    #[test]
    fn provider_rejections_stay_resumable_for_ordinary_graphs() {
        // Design card 004 D9A: only a persistent assistant turns a provider
        // rejection into a Host reply; an ordinary Graph keeps it resumable.
        let failure = InvocationFailure::Provider {
            kind: "provider_error",
            reason: "model provider rejected the request (provider_error)".into(),
        };
        assert!(failure.reason().contains("provider_error"));
        assert!(matches!(
            failure_outcome(false, false, failure),
            NodeExecutionOutcome::Interrupted { .. }
        ));
    }

    #[test]
    fn uncertain_invocations_stay_resumable_and_cancellation_wins() {
        for reason in [
            "transport failed",
            "Goose ended without a validated final_result",
        ] {
            assert!(matches!(
                failure_outcome(false, false, InvocationFailure::Uncertain(reason.into())),
                NodeExecutionOutcome::Interrupted { .. }
            ));
        }
        assert!(matches!(
            failure_outcome(
                false,
                true,
                InvocationFailure::Uncertain("transport failed".into())
            ),
            NodeExecutionOutcome::Cancelled
        ));
        assert!(matches!(
            failure_outcome(false, true, InvocationFailure::Contract("bad stop".into())),
            NodeExecutionOutcome::Cancelled
        ));
    }

    #[test]
    fn fixture_spike_keeps_terminal_failures() {
        assert!(matches!(
            failure_outcome(
                true,
                false,
                InvocationFailure::Uncertain("transport failed".into())
            ),
            NodeExecutionOutcome::Failed { .. }
        ));
    }

    #[test]
    fn plain_errors_default_to_uncertain() {
        assert!(matches!(
            InvocationFailure::from("transport failed".to_owned()),
            InvocationFailure::Uncertain(_)
        ));
        assert!(matches!(
            InvocationFailure::from("transport failed"),
            InvocationFailure::Uncertain(_)
        ));
        assert_eq!(
            InvocationFailure::Uncertain("transport failed".into()).reason(),
            "transport failed"
        );
    }
}
