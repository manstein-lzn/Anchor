//! Content-free, owner-exported discovery views. These are not authoritative Run storage.
use crate::window::{ResourceScope, ResourceWindow, is_digest, valid_namespace};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceCategory {
    Production,
    Experiment,
    DreamingMeta,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeRunStatus {
    Executing,
    WaitingHuman,
    WaitingChild,
    WaitingEffect,
    Done,
    Failed(anchor_kernel::Reason),
    Escalated,
    Unknown,
    Aborted,
}
impl SafeRunStatus {
    pub fn of(phase: &anchor_kernel::Phase) -> Self {
        use anchor_kernel::{Outcome, Phase, Waiter};
        match phase {
            Phase::Executing => Self::Executing,
            Phase::Waiting { waiter } => match waiter {
                Waiter::Human { .. } => Self::WaitingHuman,
                Waiter::Child { .. } => Self::WaitingChild,
                Waiter::Effect { .. } => Self::WaitingEffect,
            },
            Phase::Settled { result } => match result {
                Outcome::Done => Self::Done,
                Outcome::Failed { reason, .. } => Self::Failed(*reason),
                Outcome::Escalated { .. } => Self::Escalated,
                Outcome::Unknown { .. } => Self::Unknown,
                Outcome::Aborted { .. } => Self::Aborted,
            },
        }
    }
    pub fn is_settled(self) -> bool {
        !matches!(
            self,
            Self::Executing | Self::WaitingHuman | Self::WaitingChild | Self::WaitingEffect
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactRef {
    pub source_instance: String,
    pub resource: String,
    pub place: usize,
    pub record_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafeNodeSlot {
    pub node_id: String,
    pub invocation: u64,
    pub at_ms: Option<u64>,
    pub cost: u64,
    pub config: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafeRunView {
    pub run_id: String,
    pub graph: String,
    pub graph_digest: String,
    pub status: SafeRunStatus,
    pub created_at_ms: Option<u64>,
    pub last_at_ms: Option<u64>,
    pub settled_at_ms: Option<u64>,
    pub accepted_slots: Vec<SafeNodeSlot>,
    pub provenance: ProvenanceCategory,
    pub fact_ref: FactRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafeGradeView {
    pub run_id: String,
    pub task: String,
    pub task_version: u32,
    pub grader_config_digest: String,
    pub checks: Vec<crate::Check>,
    pub at_ms: Option<u64>,
    pub provenance: ProvenanceCategory,
    pub fact_ref: FactRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafeViewSnapshot {
    pub source_instance: String,
    pub join_namespace: String,
    pub root_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_root_fingerprint: Option<String>,
    pub scope: ResourceScope,
    pub resources: Vec<ResourceWindow>,
    pub runs: Vec<SafeRunView>,
    pub grades: Vec<SafeGradeView>,
    pub view_digest: String,
}
/// All exported fields, including their source positions, are digest-bound.
impl SafeViewSnapshot {
    pub fn digest(&self) -> String {
        let mut value = serde_json::to_value(self).expect("safe view serializes");
        value.as_object_mut().expect("object").remove("view_digest");
        crate::text_digest(&crate::canonical(&value).to_string())
    }
    pub fn seal(&mut self) {
        self.view_digest = self.digest();
    }
    pub fn validate(&self) -> Result<(), String> {
        self.scope.validate()?;
        if !valid_namespace(&self.source_instance)
            || !valid_namespace(&self.join_namespace)
            || !is_digest(&self.root_fingerprint)
            || self
                .owner_root_fingerprint
                .as_deref()
                .is_some_and(|d| !is_digest(d))
            || self.view_digest != self.digest()
            || (!self.runs.is_empty() && !self.grades.is_empty())
        {
            return Err("invalid frozen safe view".into());
        }
        let mut previous = None;
        for resource in &self.resources {
            crate::window::validate_resource_id(&resource.resource)?;
            if resource.after > resource.through
                || previous.is_some_and(|id: &str| id >= resource.resource.as_str())
                || !is_digest(&resource.history_digest)
                || !is_digest(&resource.snapshot_digest)
                || resource
                    .prefix_digest
                    .as_deref()
                    .is_some_and(|d| !is_digest(d))
            {
                return Err("invalid safe view positions".into());
            }
            if let ResourceScope::Resources { ids } = &self.scope
                && !ids.contains(&resource.resource)
            {
                return Err("safe view outside scope".into());
            }
            previous = Some(resource.resource.as_str());
        }
        let mut seen = std::collections::BTreeSet::new();
        for (run, fact) in self
            .runs
            .iter()
            .map(|v| (&v.run_id, &v.fact_ref))
            .chain(self.grades.iter().map(|v| (&v.run_id, &v.fact_ref)))
        {
            crate::window::validate_resource_id(run)?;
            if run.contains('/')
                || fact.source_instance != self.source_instance
                || !is_digest(&fact.record_digest)
                || !self
                    .resources
                    .iter()
                    .any(|p| p.resource == fact.resource && fact.place <= p.through)
                || !seen.insert((&fact.resource, fact.place))
                || crate::window::resource_run(&fact.resource) != Some(run)
            {
                return Err("invalid safe view fact reference".into());
            }
        }
        if self.runs.iter().any(|v| {
            !v.fact_ref.resource.starts_with("run/")
                || !is_digest(&v.graph_digest)
                || v.accepted_slots.len() > 1
                || v.accepted_slots.iter().any(|s| {
                    s.node_id.is_empty()
                        || s.node_id.contains(['/', '#'])
                        || s.node_id.chars().any(char::is_control)
                        || s.invocation == 0
                        || s.config.as_deref().is_some_and(|d| !is_digest(d))
                })
        }) || self.grades.iter().any(|v| {
            !v.fact_ref.resource.starts_with("grade/")
                || v.fact_ref.place == 0
                || !is_digest(&v.grader_config_digest)
        }) {
            return Err("invalid safe view identity".into());
        }
        Ok(())
    }

    /// An advance can append accepted records, not rewrite its frozen support.
    pub fn continues(&self, parent: &Self) -> Result<(), String> {
        if self.source_instance != parent.source_instance
            || self.join_namespace != parent.join_namespace
            || self.root_fingerprint != parent.root_fingerprint
            || self.scope != parent.scope
            || parent
                .owner_root_fingerprint
                .as_ref()
                .is_some_and(|id| self.owner_root_fingerprint.as_ref() != Some(id))
        {
            return Err("frozen safe view source identity changed".into());
        }
        for old in &parent.resources {
            let next = self
                .resources
                .iter()
                .find(|n| n.resource == old.resource)
                .ok_or("a previously captured resource disappeared")?;
            if next.after != old.through
                || next.through < old.through
                || next.start_cursor != old.end_cursor
                || (next.through == old.through
                    && (next.prefix_digest != old.prefix_digest
                        || next.snapshot_digest != old.snapshot_digest))
            {
                return Err("frozen safe view prefix changed".into());
            }
        }
        if parent
            .runs
            .iter()
            .filter(|r| r.fact_ref.place > 0)
            .any(|r| !self.runs.contains(r))
            || parent.grades.iter().any(|g| !self.grades.contains(g))
        {
            return Err("frozen safe view accepted record changed".into());
        }
        Ok(())
    }
}
