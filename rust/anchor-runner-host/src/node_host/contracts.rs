use anchor_runtime::{
    ToolPort,
    graph::{NodeExecutionRequest, PluginBinding},
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
}

pub(crate) type ToolResolution<'resolver> =
    Pin<Box<dyn Future<Output = Result<Arc<dyn ToolPort>, String>> + Send + 'resolver>>;

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
