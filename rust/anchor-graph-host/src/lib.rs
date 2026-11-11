//! Standalone Graph bundle loading and minimal in-process host adapter for
//! recursive Graph calls.
//!
//! The host supplies Run access, artifacts and execution through small ports.
//! Lifecycle and progress belong to the shared Kernel; this crate owns Call
//! admission identity, input staging, Session handoff and result export.

mod bundle;
mod dependencies;
mod graph_call;
mod graph_catalog;
mod mcp;
mod plugin_authorization;
mod plugin_catalog;
mod plugin_configuration;
mod resources;
mod system_plugins;

pub use bundle::{
    BundlePluginReference, FileGraphBundleLoader, GraphBundleManifest, LoadedGraphBundle,
};
pub use dependencies::{
    DEPENDENCIES_DIRECTORY, MutablePaths, PluginDependencies, STORE_DIRECTORY, VIEW_TOKEN,
    VIEWS_DIRECTORY, mutable_paths_from_manifest, plugin_view_path, resolved_library_root,
    store_root, views_root,
};
pub use graph_call::{
    CallRun, CallRunAccess, ChildExecutionPort, InProcessGraphHost, child_run_id,
};
pub use graph_catalog::{ChildRunControl, GraphCatalog, PluginReferenceLeaseFuture};
pub use mcp::{McpServerDefinition, channel_identity_headers, host_environment_references};
pub use plugin_catalog::{
    ChannelDefinition, FilePluginCatalog, PluginCatalog, PluginDefinition, PluginMetadata,
    SystemPluginInspection,
};
pub use plugin_configuration::{
    PluginConfiguration, PluginConfigurationField, PluginConfigurationFieldType,
    mcp_environment_fields,
};
pub use system_plugins::{SystemPlugin, all_system_plugins};

use anchor_runtime::graph::GraphError;

fn invalid(message: String) -> GraphError {
    GraphError::InvalidSnapshot(message)
}

#[cfg(test)]
mod plugin_test_support;
