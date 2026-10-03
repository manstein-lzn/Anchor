//! Stable workspace crate for Anchor's Rust-native io-harness boundary.
//!
//! The modules are intentionally layered:
//! - [`adapter`] converts io-harness requests to Rig provider calls;
//! - [`node`] owns Anchor ToolPort adaptation and the io-harness Store;
//! - [`node_exec`] freezes one Anchor `NodeRequest` into the io contract;
//! - [`node_port`] bridges host-owned resolution into Graph `NodeExecutionPort`.
//!
//! io-harness is the only Agent loop. Rig is only the provider transport.

pub mod adapter;
pub mod node;
pub mod node_exec;
pub mod node_port;
