//! Historical experimental Run provenance and shared request-derived Run identity.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Retained provenance, not execution authorization. Remove only after historical
/// experimental Run metadata is retired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentRunContext {
    pub schema_version: u32,
    pub experiment: String,
    pub assignment: String,
    pub manifest_digest: String,
    pub snapshot_digest: String,
    pub input_digest: String,
    /// Identifies the retained Host-enforced execution profile; absent for the
    /// earlier isolated plumbing proof, which has no production authority.
    pub execution_closure_digest: Option<String>,
}

pub fn run_id(request_id: &str) -> String {
    format!("request-{:x}", Sha256::digest(request_id.as_bytes()))
}
