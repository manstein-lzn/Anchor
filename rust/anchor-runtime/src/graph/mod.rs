//! Durable, serial coordinator for an already-expanded Anchor graph snapshot.
//!
//! This module owns graph/run facts only. Agent checkpoints, providers, tools,
//! and host execution remain behind `NodeExecutionPort`.

pub(crate) use serde_json::Value;
pub(crate) use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    future::Future,
    io::{self, Write},
    path::PathBuf,
    pin::Pin,
    sync::atomic::Ordering,
    time::{SystemTime, UNIX_EPOCH},
};

mod error;
pub use error::GraphError;
mod model;
pub use model::*;
mod admission;
mod authoring;
mod logic;
pub(crate) use logic::*;
mod state;
pub use state::*;
mod store;
pub use store::*;
mod ports;
pub use ports::*;
mod runner;
pub use runner::*;
mod parallel;

#[cfg(test)]
mod tests;
