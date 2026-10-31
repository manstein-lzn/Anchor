use anchor_runtime::{
    ToolPort,
    graph::{GraphError, NodeExecutionRequest, PluginBinding},
};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeImage {
    pub(crate) data: Vec<u8>,
    pub(crate) media_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeConversationHint {
    pub(crate) key: String,
    pub(crate) cross_run: bool,
}

/// Why a resolver could not assemble a node's tools.
///
/// Most failures here are "this host cannot do that", but one of them is "the
/// Run recorded a fact this host can no longer honor". The second must reach the
/// Kernel scheduler as the class it is, so the resolver carries it instead of
/// flattening every cause into a string that the caller then relabels.
#[derive(Debug)]
pub(crate) enum ResolutionError {
    /// An explicit preparation check failed before any Agent tool execution.
    KnownFailure(String),
    /// The Run's own frozen fact cannot be honored.
    Graph(GraphError),
    /// This host cannot assemble the tools for another reason.
    Host(String),
}

impl std::fmt::Display for ResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Graph(error) => write!(formatter, "{error}"),
            Self::KnownFailure(reason) | Self::Host(reason) => formatter.write_str(reason),
        }
    }
}

impl From<String> for ResolutionError {
    fn from(reason: String) -> Self {
        Self::Host(reason)
    }
}

impl From<crate::tool_host::PreparationError> for ResolutionError {
    fn from(error: crate::tool_host::PreparationError) -> Self {
        match error {
            crate::tool_host::PreparationError::KnownFailure(reason) => Self::KnownFailure(reason),
            crate::tool_host::PreparationError::Unavailable(reason) => Self::Host(reason),
        }
    }
}

impl From<crate::plugin_view::Unusable> for ResolutionError {
    fn from(error: crate::plugin_view::Unusable) -> Self {
        match error {
            crate::plugin_view::Unusable::KnownFailure(reason) => Self::KnownFailure(reason),
            error => Self::Graph(error.into_graph_error()),
        }
    }
}

impl ResolutionError {
    pub(crate) fn into_graph_error(self) -> GraphError {
        match self {
            Self::Graph(error) => error,
            Self::KnownFailure(reason) | Self::Host(reason) => GraphError::Unsupported(reason),
        }
    }
}

/// The tools and Plugin identities prepared for the same invocation.
pub(crate) struct ResolvedNodeTools {
    pub(crate) tools: Arc<dyn ToolPort>,
    pub(crate) plugins: Vec<PluginBinding>,
    /// Derived from the same frozen disclosure catalog as the tools.
    pub(crate) plugin_instructions: String,
    /// The host path of the workspace the tools write into.
    pub(crate) workspace: std::path::PathBuf,
}

pub(crate) type ToolResolution<'resolver> =
    Pin<Box<dyn Future<Output = Result<ResolvedNodeTools, ResolutionError>> + Send + 'resolver>>;

pub(crate) trait NodeHostResolver: Send + Sync {
    fn prompt_images(&self, request: &NodeExecutionRequest) -> Result<Vec<NodeImage>, String>;
    fn conversation_hint(
        &self,
        request: &NodeExecutionRequest,
    ) -> Result<Option<NodeConversationHint>, String>;
    fn resolve_plugins(&self, ids: &[String]) -> Result<Vec<PluginBinding>, String>;
    fn workspace(&self, request: &NodeExecutionRequest) -> Result<PathBuf, String>;
    fn tools<'resolver>(
        &'resolver self,
        request: &'resolver NodeExecutionRequest,
    ) -> ToolResolution<'resolver>;
}
