use serde_json;
use std::io;

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("invalid graph snapshot: {0}")]
    InvalidSnapshot(String),
    #[error("unsupported graph capability: {0}")]
    Unsupported(String),
    #[error("snapshot decode failed: {0}")]
    SnapshotDecode(serde_json::Error),
    #[error("run decode failed: {0}")]
    RunDecode(serde_json::Error),
    #[error("run store I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("corrupt graph run: {0}")]
    CorruptRun(String),
    #[error("run ID is not a safe file name: {0}")]
    InvalidRunId(String),
    #[error("run record conflict: supplied state differs from durable state")]
    RunConflict,
    #[error("run `{0}` already has an active writer")]
    RunBusy(String),
    #[error("invalid node route: {0}")]
    InvalidRoute(String),
}
