use super::*;
use serde::{Deserialize, Serialize};

/// Coordinator-owned provenance; completion output never grants file access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Node,
    Fanout,
    Join,
    GraphCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactFreezeContext {
    pub kind: ArtifactKind,
    pub input_commits: Vec<CommitRef>,
}

pub trait ArtifactPort: Send + Sync {
    /// Must be idempotent for an InvocationKey. A retry after freeze succeeded
    /// but before the RunStore commit must resolve to the same immutable commit.
    fn freeze<'a>(
        &'a self,
        key: &'a InvocationKey,
        completion: &'a NodeCompletion,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>>;
    /// Preserve exact input provenance when the host supports linked files.
    /// Existing adapters retain their original freeze behavior.
    fn freeze_with_context<'a>(
        &'a self,
        key: &'a InvocationKey,
        completion: &'a NodeCompletion,
        _context: &'a ArtifactFreezeContext,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
        self.freeze(key, completion)
    }
    /// Read/materialize the exact fixed commit. Implementations must not follow
    /// a mutable workspace head or create/advance a commit as a side effect.
    fn resolve<'a>(
        &'a self,
        commit: &'a CommitRef,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>>;
    /// Stage the parent-visible committed input files selected by an Op.call
    /// `files` list into the child Run's read-only `/in/call` bundle. Hosts that
    /// cannot expose immutable file trees must fail closed when a selection is
    /// requested; an empty selection is always a no-op. Implementations must be
    /// idempotent for a given child Run so a wait retry never rewrites inputs.
    fn stage_call_inputs<'a>(
        &'a self,
        _child_run_id: &'a str,
        _parents: &'a [CommitRef],
        selections: &'a [CallFileSelection],
    ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
        Box::pin(async move {
            if selections.is_empty() {
                Ok(())
            } else {
                Err(GraphError::Unsupported(
                    "host ArtifactPort cannot stage Op.call input files".into(),
                ))
            }
        })
    }
    /// Copy selected files from a child result commit into the parent call node
    /// workspace `result/` directory, returning the copied relative paths. The
    /// call node's GraphCall commit is frozen after this future resolves, so a
    /// successful export is what makes the files readable downstream.
    fn export_call_result_files<'a>(
        &'a self,
        _call_key: &'a InvocationKey,
        _commit: &'a CommitRef,
        files: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            if files.is_empty() {
                Ok(Vec::new())
            } else {
                Err(GraphError::Unsupported(
                    "host ArtifactPort cannot export Op.call result files".into(),
                ))
            }
        })
    }
}

/// One already-validated `files` selection from an Op.call spec: the committed
/// source node, the source-relative path, and the child-relative `as` alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFileSelection {
    pub node: String,
    pub path: String,
    pub alias: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphCallOutcome {
    Waiting {
        child_run_id: String,
    },
    Completed {
        child_run_id: String,
        output: Value,
    },
    Detached {
        child_run_id: String,
    },
    Failed {
        child_run_id: Option<String>,
        reason: String,
    },
    Uncertain {
        child_run_id: Option<String>,
        reason: String,
    },
}

/// Host-owned admission/execution bridge for Op.call. Implementations MUST
/// atomically persist identity → child Run admission, return that same child
/// for every retry, and report `Uncertain` when they cannot prove the durable
/// fact. `Detached` is legal only after admission is durable. Child execution
/// itself uses the host's ordinary GraphRunner instance.
pub trait GraphCallPort: Send + Sync {
    fn call<'a>(
        &'a self,
        identity: &'a CallIdentity,
        spec: &'a Value,
        input: &'a Value,
        input_commits: &'a [CommitRef],
        cancellation: crate::Cancellation,
    ) -> Pin<Box<dyn Future<Output = Result<GraphCallOutcome, GraphError>> + Send + 'a>>;
}

#[derive(Debug, Clone)]
pub struct NodeExecutionRequest {
    pub key: InvocationKey,
    /// Provider/model reference selected by the Graph for Agent nodes.
    pub model: Option<String>,
    pub task: String,
    pub instructions: String,
    pub routes: Vec<String>,
    pub input: Value,
    pub input_commits: Vec<CommitRef>,
    /// Secret-free Plugin resource identities resolved by the host.
    pub plugins: Vec<PluginBinding>,
    /// A cumulative provider-request budget, supported only when the node
    /// adapter advertises exact request accounting.
    pub max_provider_requests: Option<u64>,
    pub wall_time_limit_seconds: Option<f64>,
    /// Graph request intent only. The host adapter must apply its own network authorization.
    pub network: bool,
    pub kind: NodeKind,
    pub operation: Option<Value>,
    pub cancellation: crate::Cancellation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginBinding {
    pub id: String,
    pub digest: String,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
}

/// Persisted, non-secret facts for one unresolved tool call.
/// Tool arguments are not part of this recovery projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryAttempt {
    pub attempt_id: i64,
    pub step: u32,
    pub tool: String,
    pub started_at: String,
}

/// An explicit decision about an unresolved tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryDecision {
    Retry,
    Completed { observation: String },
    Abort,
}

/// The unresolved attempt is bound to the exact durable Graph invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRecovery {
    pub key: InvocationKey,
    pub attempt: RecoveryAttempt,
}

/// Durable operator decision for one exact Harness attempt. Keeping this in
/// the Graph Run makes resubmission idempotent after the pending list advances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySubmission {
    pub key: InvocationKey,
    pub attempt_id: i64,
    pub decision: RecoveryDecision,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompletionFact {
    NotStarted,
    /// The host has a durable execution id/checkpoint for this invocation and
    /// may re-enter its own resume protocol. This does not authorize replaying
    /// an uncertain external effect; the execution backend remains responsible
    /// for requiring an explicit recovery decision where needed.
    Resumable,
    Completed(NodeCompletion),
    Failed(String),
    Uncertain(String),
}

pub trait NodeExecutionPort: Send + Sync {
    fn capabilities(&self) -> NodeExecutionCapabilities;
    fn graph_call_port(&self) -> Option<&dyn GraphCallPort> {
        None
    }
    /// Resolve only public, immutable resource identity. Secrets and live
    /// clients stay in the host adapter. Default denies Plugin use.
    fn resolve_plugins(&self, _plugin_ids: &[String]) -> Result<Vec<PluginBinding>, GraphError> {
        Err(GraphError::Unsupported(
            "Plugin resolver is not configured".into(),
        ))
    }
    fn completion_fact<'a>(
        &'a self,
        key: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>>;
    /// Persist an operator decision before a waiting Graph Run is re-entered.
    /// Implementations must bind it to this invocation and attempt and reject
    /// conflicting resubmissions.
    fn record_recovery_decision(
        &self,
        _key: &InvocationKey,
        _attempt_id: i64,
        _decision: RecoveryDecision,
    ) -> Result<(), GraphError> {
        Err(GraphError::Unsupported(
            "node executor does not support tool recovery decisions".into(),
        ))
    }
    /// On `Completed` or `Failed`, the implementation MUST durably write the
    /// matching completion fact before resolving this future. If it cannot
    /// establish whether execution completed it must persist/return Uncertain.
    /// BudgetExhausted retains the same invocation's resumable checkpoint and
    /// MUST NOT publish a completion fact.
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NodeExecutionCapabilities {
    pub agent: bool,
    pub op_run: bool,
    /// True only when max_provider_requests is enforced as the cumulative
    /// provider request count, rather than translated to a turn count.
    pub exact_provider_request_budget: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeExecutionOutcome {
    Completed(NodeCompletion),
    /// Unknown external tool effects require an operator decision before this
    /// invocation can advance. The Graph cursor remains unchanged.
    WaitingRecovery {
        attempts: Vec<RecoveryAttempt>,
    },
    BudgetExhausted {
        model_requests: u64,
    },
    Cancelled,
    Interrupted {
        reason: String,
    },
    /// The operator explicitly chose to abort this invocation after an
    /// unresolved external tool effect. The containing Graph Run is terminal.
    Aborted,
    /// A known terminal node failure. The host has established that the
    /// invocation failed; the Graph Run records a terminal failure and never
    /// treats it as an invitation to replay the invocation.
    Failed {
        reason: String,
    },
}

pub trait RunControl: Send + Sync {
    fn pause_requested(&self) -> bool;
    fn stop_requested(&self) -> bool;
    fn cancellation(&self) -> crate::Cancellation;
}
