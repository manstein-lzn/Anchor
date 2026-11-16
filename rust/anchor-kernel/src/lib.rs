//! Anchor's constitutional kernel.
//!
//! > Unreliable proposers act freely on a bounded stage; deterministic code
//! > adjudicates, records and measures; only selected changes persist.
//!
//! The kernel does four things and nothing else:
//!
//! - **adjudicate**: a [`Submission`] is judged by the [`Validator`] of its
//!   target resource, after the laws every resource shares;
//! - **record**: every verdict, including refusals, is appended to the target's
//!   history in its [`Store`];
//! - **settle**: a task resource ends exactly once, within its budget, in one
//!   of a few [`Outcome`]s;
//! - **measure**: every adjudication produces one [`Point`] for the [`Probe`].
//!
//! It knows no Graph, model, protocol or workflow. A Graph is a validator of a
//! Run; a Plugin install is a validator returning `Pending`; an optimiser is a
//! proposer like any other.

mod kernel;
mod model;
mod store;

pub use kernel::{Authority, Clock, Kernel, KernelError, Probe, SystemClock, Validators};
pub use model::*;
pub use store::{
    FileStore, HistoryBatch, HistoryCursor, HistoryReadMetrics, HistoryReader, MemoryStore, Store,
    StoreError,
};
