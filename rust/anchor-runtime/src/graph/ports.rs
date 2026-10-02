use super::*;

pub trait ArtifactPort: Send + Sync {
    /// Must be idempotent for an InvocationKey. A retry after freeze succeeded
    /// but before the RunStore commit must resolve to the same immutable commit.
    fn freeze<'a>(
        &'a self,
        key: &'a InvocationKey,
        completion: &'a NodeCompletion,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>>;
    /// Read/materialize the exact fixed commit. Implementations must not follow
    /// a mutable workspace head or create/advance a commit as a side effect.
    fn resolve<'a>(
        &'a self,
        commit: &'a CommitRef,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>>;
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
    /// Python max_steps is a cumulative provider-request budget. Implementors
    /// must honor this exact meaning; it is not Rig max_turns.
    pub max_provider_requests: Option<u64>,
    pub wall_time_limit_seconds: Option<f64>,
    /// Graph request intent only. The host adapter must apply its own network authorization.
    pub network: bool,
    pub kind: NodeKind,
    pub operation: Option<Value>,
    pub cancellation: crate::Cancellation,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompletionFact {
    NotStarted,
    Completed(NodeCompletion),
    Failed(String),
    Uncertain(String),
}

pub trait NodeExecutionPort: Send + Sync {
    fn capabilities(&self) -> NodeExecutionCapabilities;
    fn completion_fact<'a>(
        &'a self,
        key: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>>;
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
    BudgetExhausted {
        model_requests: u64,
    },
    Cancelled,
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
