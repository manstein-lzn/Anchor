//! Exact advisory data selected by the Host, never a capability or tool schema.
use serde::{Deserialize, Serialize};

pub const INPUT_DISCOVERY: &str = "input-discovery";
pub const CONTENT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuidanceScope {
    Graphs { graphs: Vec<String> },
    Global,
}

impl GuidanceScope {
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Graphs { graphs } = self
            && (graphs.is_empty()
                || graphs.iter().any(|graph| !crate::evaluation::name(graph))
                || graphs.windows(2).any(|pair| pair[0] >= pair[1]))
        {
            return Err("guidance scope requires sorted unique Graph identities".into());
        }
        Ok(())
    }
}

/// Read-only identity for retained advisory snapshots; remove with those facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationRef {
    pub resource: String,
    pub version: u64,
    pub execute: crate::evaluation::FactRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputGuidance {
    pub schema_version: u32,
    pub slot: String,
    pub content: String,
    pub content_digest: String,
    pub scope: Option<GuidanceScope>,
    pub activation: Option<ActivationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<crate::evaluation::ArtifactRef>,
}

impl InputGuidance {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1
            || self.slot != INPUT_DISCOVERY
            || self.content.trim().is_empty()
            || self.content.len() > CONTENT_BYTES
            || self.content.contains('\0')
            || crate::text_digest(&self.content) != self.content_digest
            || self.scope.is_some() != (self.activation.is_some() || self.candidate.is_some())
            || (self.activation.is_some() && self.candidate.is_some())
        {
            return Err("invalid input-discovery guidance snapshot".into());
        }
        if let Some(scope) = &self.scope {
            scope.validate()?;
        }
        if let Some(reference) = &self.activation
            && (reference.version == 0
                || !reference.resource.starts_with("guidance/input-discovery/")
                || reference.execute.place == 0
                || !reference.execute.resource.starts_with("change/")
                || !crate::window::is_digest(&reference.execute.record_digest))
        {
            return Err("invalid input-discovery activation reference".into());
        }
        if let Some(candidate) = &self.candidate
            && (candidate.invocation != 1
                || !candidate
                    .node_id
                    .strip_prefix("dreaming-candidate-")
                    .is_some_and(crate::window::is_digest)
                || !crate::evaluation::name(&candidate.id))
        {
            return Err("invalid candidate input guidance reference".into());
        }
        Ok(())
    }

    pub fn identity(&self) -> serde_json::Value {
        serde_json::json!({"schema_version":self.schema_version,"slot":self.slot,
            "content_digest":self.content_digest,"scope":self.scope,"activation":self.activation,"candidate":self.candidate})
    }
}
