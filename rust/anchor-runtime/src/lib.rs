use std::sync::{Arc, atomic::AtomicBool};

pub mod graph;
pub mod sandbox;
pub mod tools;

pub use sandbox::{
    NetworkPolicy, NoopSandbox, ReadOnlyInput, SandboxEnvironment, SandboxError, SandboxPort,
    SandboxRequest, SandboxResult, SandboxStatus, SpillPolicy,
};
pub use tools::{EmptyToolName, ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent};

/// A cancellation flag owned by the host.
pub type Cancellation = Arc<AtomicBool>;
