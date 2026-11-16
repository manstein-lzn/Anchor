//! Content-free Hub rows shared by the projector and its process clients.

use anchor_kernel::{Action, PrincipalKind, Verdict};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub root: String,
    pub resource: String,
    pub place: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "body", rename_all = "snake_case")]
pub enum Body {
    Kernel,
    Observation(crate::Observation),
    Grade(crate::Grade),
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub key: Key,
    pub namespace: String,
    #[serde(default)]
    pub provenance: crate::ProvenanceCategory,
    pub at: Option<u64>,
    pub principal: String,
    pub principal_kind: PrincipalKind,
    pub config: Option<String>,
    pub cost: u64,
    pub action: Action,
    pub slot: Option<String>,
    pub verdict: Verdict,
    pub status: Option<crate::SafeRunStatus>,
    #[serde(flatten)]
    pub body: Body,
}

impl Row {
    /// A round trip closes nested schemas too: adding fields to a body or a
    /// Kernel enum must not silently let unrecognized content through the wire.
    pub fn from_wire(value: &Value) -> Result<Self, String> {
        let row: Self = serde_json::from_value(value.clone())
            .map_err(|_| "invalid content-free evidence row")?;
        match &row.body {
            Body::Observation(body) if body.version != crate::VERSION => {
                return Err("unsupported evidence observation version".into());
            }
            Body::Grade(body) if body.version != crate::VERSION => {
                return Err("unsupported evidence grade version".into());
            }
            Body::Unreadable => return Err("unreadable evidence body".into()),
            _ => {}
        }
        let mut encoded = serde_json::to_value(&row).map_err(|_| "evidence row encoding failed")?;
        // Remove this fallback after retained pre-provenance windows retire.
        if value.get("provenance").is_none() {
            encoded
                .as_object_mut()
                .expect("row is an object")
                .remove("provenance");
        }
        if encoded != *value {
            return Err("evidence row contains unsupported fields".into());
        }
        Ok(row)
    }
}
