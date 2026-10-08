use std::{future::Future, pin::Pin};

use serde_json::Value;

mod native;
pub use native::{EmptyToolName, ToolDefinition, ToolName, ToolResultContent};

pub trait ToolPort: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;

    fn is_read_only(&self, _name: &str) -> bool {
        false
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>>;
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool `{0}` is not registered")]
    Unknown(String),
    #[error("tool failed: {0}")]
    Failed(String),
}

#[cfg(test)]
mod tests;
