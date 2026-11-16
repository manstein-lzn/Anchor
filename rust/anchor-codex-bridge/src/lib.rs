//! Local HTTP MCP bridge between an Anchor Engineering Manager Graph and `codex app-server`.

pub mod client;
pub mod config;
pub mod git;
mod manager;
pub mod mcp;
mod protocol;
pub mod server;
pub mod state;

pub use config::{BridgeConfig, ConfigError};
pub use server::{RunningBridge, start_server};

#[cfg(test)]
mod tests;
