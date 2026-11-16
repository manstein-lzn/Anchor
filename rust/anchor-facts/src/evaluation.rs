//! Registered deterministic quality suites and exact references to accepted facts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const GRADE_ROOT: &str = "kernel-grades";
pub const FRAME_BYTES: usize = 4 * 1024 * 1024;
pub const SUITE_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub id: String,
    pub node_id: String,
    pub invocation: u64,
}

pub fn digest(value: &(impl Serialize + ?Sized)) -> Result<String, String> {
    let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
    Ok(crate::text_digest(&crate::canonical(&value).to_string()))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRef {
    pub resource: String,
    pub version: u64,
    pub digest: String,
}

impl PolicyRef {
    pub fn validate(&self) -> Result<(), String> {
        crate::window::validate_resource_id(&self.resource)?;
        if self.version == 0 || !crate::window::is_digest(&self.digest) {
            return Err("invalid versioned policy reference".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Development,
    Holdout,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteCase {
    pub id: String,
    pub split: Split,
    pub input: Value,
    pub expected: Value,
}

/// Protected process-start configuration, never supplied by the graded Run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedSuite {
    pub schema_version: u32,
    pub id: String,
    pub version: u32,
    pub holdout_batch: String,
    pub cases: Vec<SuiteCase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
}

impl ProtectedSuite {
    pub fn deliverable_path(&self) -> &str {
        self.output_file.as_deref().unwrap_or("result.json")
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1
            || !name(&self.id)
            || self.version == 0
            || !name(&self.holdout_batch)
            || self.cases.len() > 128
            || self.output_file.as_deref().is_some_and(|file| !name(file))
        {
            return Err("invalid protected suite identity or bounds".into());
        }
        let mut ids = BTreeSet::new();
        let mut inputs = BTreeMap::new();
        let (mut development, mut holdout) = (false, false);
        for case in &self.cases {
            if !name(&case.id) || !ids.insert(&case.id) {
                return Err("duplicate or invalid protected case identity".into());
            }
            let input = digest(&case.input)?;
            if inputs
                .insert(input, &case.split)
                .is_some_and(|previous| *previous != case.split)
            {
                return Err("development and holdout inputs overlap".into());
            }
            development |= case.split == Split::Development;
            holdout |= case.split == Split::Holdout;
        }
        if !development || !holdout {
            return Err("protected suite requires development and holdout cases".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactRef {
    pub resource: String,
    /// One-based position in the full history, including unsuccessful facts.
    pub place: u64,
    pub config_digest: String,
    pub record_digest: String,
}

impl FactRef {
    pub fn at(
        resource: &str,
        index: usize,
        record: &anchor_kernel::Record,
    ) -> Result<Self, String> {
        Ok(Self {
            resource: resource.into(),
            place: index as u64 + 1,
            config_digest: record.stamp.config().unwrap_or_default().into(),
            record_digest: digest(record)?,
        })
    }

    pub fn verify(&self, record: &anchor_kernel::Record) -> Result<(), String> {
        if self.place == 0
            || !crate::window::is_digest(&self.record_digest)
            || !record.verdict.is_accepted()
            || record.submission.target != self.resource
            || record.stamp.config().unwrap_or_default() != self.config_digest
            || digest(record)? != self.record_digest
        {
            return Err("exact accepted fact identity mismatch".into());
        }
        crate::window::validate_resource_id(&self.resource)
    }
}

pub fn name(value: &str) -> bool {
    value.len() <= 256 && !value.contains('/') && crate::window::validate_resource_id(value).is_ok()
}

#[cfg(test)]
mod tests;
