//! The harness data hub: the read side of recorded harness facts.
//!
//! Layers depend only downwards, and only L0 and L4 do IO:
//!
//! - L0 [`source`]: read-only kernel histories;
//! - L1 [`table`]: records projected into fact rows, kept up to date
//!   incrementally;
//! - L2 [`query`]: trajectory algebra, pure functions;
//! - L3 [`analyzers`]: plugins over L2 that return findings with evidence;
//! - L4 [`adapters`]: the command line, and [`mcp`]: the same reads as a
//!   read-only MCP server.
//!
//! The hub reads registered histories and bounded execution evidence, writes
//! nothing to any data root, and nothing depends on its statistical internals.

pub mod adapters;
pub mod analyzers;
pub mod mcp;
pub mod query;
pub mod source;
pub mod table;
pub mod window;
