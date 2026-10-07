use std::sync::{Arc, atomic::AtomicBool};

#[cfg(all(test, feature = "rig-legacy"))]
use rig_agent::run::output::OutputMode;

#[cfg(feature = "rig-legacy")]
pub mod checkpoint;
pub mod graph;
pub mod sandbox;
pub mod tools;

#[cfg(feature = "rig-legacy")]
mod rig_legacy;
#[cfg(feature = "rig-legacy")]
pub use rig_legacy::*;

pub use sandbox::{
    NetworkPolicy, NoopSandbox, ReadOnlyInput, SandboxEnvironment, SandboxError, SandboxPort,
    SandboxRequest, SandboxResult, SandboxStatus, SpillPolicy,
};
pub use tools::{EmptyToolName, ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent};

/// A cancellation flag owned by the host.
pub type Cancellation = Arc<AtomicBool>;

#[cfg(all(test, feature = "rig-legacy"))]
mod tests;
