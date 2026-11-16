//! The read-only frozen-window protocol shared by the Host and Hub process.
//! Source paths are input capabilities, never part of a manifest or response.

use std::{collections::BTreeSet, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 3;
// Delete v2 replay after retained pre-namespace local windows are retired.
pub const PREVIOUS_SCHEMA_VERSION: u32 = 2;
// Delete v1 reading after retained local v1 acceptance windows are retired;
// new captures never write that format.
pub const LEGACY_SCHEMA_VERSION: u32 = 1;
/// The Signals semantics pinned by this protocol version.
pub const ANALYZER_VERSION: u32 = 5;
// Remove v4 replay once retained local v4 windows and continuations are retired.
pub const PREVIOUS_ANALYZER_VERSION: u32 = 4;

pub fn supported_analyzer(schema: u32, analyzer: u32) -> bool {
    analyzer == ANALYZER_VERSION
        || analyzer == PREVIOUS_ANALYZER_VERSION
        || (schema != SCHEMA_VERSION && analyzer == 3)
}
pub const MAX_EVIDENCE_KEYS: usize = 16;
/// Evidence and origin samples exposed per signal, not a pending-work limit.
pub const MAX_SIGNAL_CITATIONS: usize = 5;
/// Input and output framing limit for the one-request process protocol.
pub const MAX_TRANSPORT_BYTES: usize = 64 * 1024 * 1024;

/// Stable physical root identity, calculated by each process from metadata.
/// Metadata is supplied by the caller; this vocabulary performs no IO.
pub fn source_fingerprint(device: u64, inode: u64) -> String {
    digest(&serde_json::json!({"version": 1, "device": device, "inode": inode}))
}

pub fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest(value: &Value) -> String {
    crate::text_digest(&crate::canonical(value).to_string())
}

fn valid_source_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\', ':']) && !id.chars().any(char::is_control)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowRequest {
    pub schema_version: u32,
    pub sources: Vec<WindowSource>,
    pub manifest: Option<WindowManifest>,
    /// Advance from a previously closed window, instead of replaying it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advance_from: Option<WindowManifest>,
    /// Host-owned Run identities omitted before analysis, never resource grants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_runs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safe_views: Vec<crate::SafeViewSnapshot>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub support_history: bool,
    pub now_ms: u64,
    pub limit: usize,
}

impl WindowRequest {
    /// Wire rules only. The reader still owns physical history checks, and the
    /// Host still owns source authorization and post-response identity checks.
    pub fn validate(&self) -> Result<(), String> {
        validate_excluded_runs(&self.exclude_runs)?;
        if !supported_schema(self.schema_version)
            || (self.schema_version == LEGACY_SCHEMA_VERSION && self.advance_from.is_some())
            || (self.manifest.is_some() && self.advance_from.is_some())
        {
            return Err("unsupported or ambiguous window request".into());
        }
        let mut source_ids = BTreeSet::new();
        let mut namespaces = BTreeSet::new();
        for source in &self.sources {
            if !valid_source_id(&source.source_instance)
                || !source_ids.insert(&source.source_instance)
            {
                return Err("window requires unique opaque source identities".into());
            }
            if (!source.join_namespace.is_empty() && !valid_namespace(&source.join_namespace))
                || (self.manifest.is_none() && !valid_namespace(&source.join_namespace))
            {
                return Err("window requires an installation join namespace".into());
            }
            source.scope.validate()?;
            namespaces.insert(effective_namespace(&source.join_namespace));
        }
        if namespaces.len() > 1 && self.exclude_runs.iter().any(|run| !run.contains("::")) {
            return Err("excluded Run identity is ambiguous across namespaces".into());
        }
        for views in [
            self.safe_views.as_slice(),
            self.manifest
                .as_ref()
                .map_or(&[], |m| m.safe_views.as_slice()),
        ] {
            let mut view_ids = BTreeSet::new();
            for view in views {
                view.validate()?;
                if !view_ids.insert(&view.source_instance) {
                    return Err("duplicate safe view".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSource {
    /// An opaque identity assigned by the Host, not a filesystem location.
    pub source_instance: String,
    /// Installation join domain, independent of this physical source root.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub join_namespace: String,
    pub root: PathBuf,
    pub scope: ResourceScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceScope {
    /// The entire explicitly registered structured root, not child roots.
    All {},
    /// Exact logical resource IDs. An empty vector grants no resources.
    Resources { ids: Vec<String> },
}

pub fn validate_resource_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 512
        || id.contains(['*', '?', '\\', ':'])
        || id.chars().any(char::is_control)
        || id.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        Err("scope requires exact logical resource IDs".into())
    } else {
        Ok(())
    }
}

impl ResourceScope {
    /// Resource IDs are logical names, not paths or matching expressions.
    pub fn validate(&self) -> Result<(), String> {
        let Self::Resources { ids } = self else {
            return Ok(());
        };
        let mut unique = BTreeSet::new();
        for id in ids {
            validate_resource_id(id)?;
            if !unique.insert(id) {
                return Err("scope contains duplicate resource IDs".into());
            }
        }
        Ok(())
    }

    pub fn equivalent(&self, other: &Self) -> bool {
        if self.validate().is_err() || other.validate().is_err() {
            return false;
        }
        match (self, other) {
            (Self::All {}, Self::All {}) => true,
            (Self::Resources { ids: left }, Self::Resources { ids: right }) => {
                left.iter().collect::<BTreeSet<_>>() == right.iter().collect()
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowManifest {
    pub schema_version: u32,
    pub analyzer_version: u32,
    pub now_ms: u64,
    pub limit: usize,
    pub sources: Vec<SourceWindow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safe_views: Vec<crate::SafeViewSnapshot>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub support_history: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_manifest_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_runs: Vec<String>,
}

impl WindowManifest {
    pub fn digest(&self) -> String {
        digest(&serde_json::to_value(self).expect("window manifest serializes"))
    }

    /// Pure shape/position rules shared by the reader and its Host adapter.
    /// Authorization, physical cursor validation and replay are not done here.
    pub fn validate_positions(&self) -> Result<(), String> {
        validate_excluded_runs(&self.exclude_runs)?;
        if !supported_schema(self.schema_version)
            || self
                .parent_manifest_digest
                .as_deref()
                .is_some_and(|digest| !is_digest(digest))
            || (self.schema_version == LEGACY_SCHEMA_VERSION
                && self.parent_manifest_digest.is_some())
        {
            return Err("invalid window schema or parent digest".into());
        }
        let mut previous_source: Option<&str> = None;
        for source in &self.sources {
            if !valid_source_id(&source.source_instance)
                || previous_source.is_some_and(|id| id >= source.source_instance.as_str())
                || !is_digest(&source.root_fingerprint)
                || (self.schema_version == SCHEMA_VERSION
                    && !valid_namespace(&source.join_namespace))
                || (!source.join_namespace.is_empty() && !valid_namespace(&source.join_namespace))
            {
                return Err("invalid or unordered source identity".into());
            }
            source.scope.validate()?;
            previous_source = Some(&source.source_instance);
            let mut previous_resource: Option<&str> = None;
            for position in &source.resources {
                if !self
                    .safe_views
                    .iter()
                    .any(|view| view.source_instance == source.source_instance)
                {
                    position
                        .validate_for(self.schema_version, self.parent_manifest_digest.is_some())?;
                }
                if previous_resource.is_some_and(|id| id >= position.resource.as_str()) {
                    return Err("invalid frozen resource order".into());
                }
                previous_resource = Some(&position.resource);
            }
        }
        for view in &self.safe_views {
            view.validate()?;
            if !self.sources.iter().any(|source| {
                source.source_instance == view.source_instance
                    && source.join_namespace == view.join_namespace
                    && source.root_fingerprint == view.root_fingerprint
                    && source.scope == view.scope
                    && source.resources == view.resources
            }) {
                return Err("safe view manifest binding changed".into());
            }
        }
        Ok(())
    }

    /// Bind logical grants; physical root identities remain process-local.
    pub fn validate_sources(&self, sources: &[WindowSource]) -> Result<(), String> {
        self.validate_positions()?;
        if self.sources.len() != sources.len() {
            return Err("frozen source count changed".into());
        }
        for window in &self.sources {
            let source = sources
                .iter()
                .find(|source| source.source_instance == window.source_instance)
                .ok_or("frozen source is not authorized")?;
            if !window.scope.equivalent(&source.scope)
                || window.join_namespace != source.join_namespace
            {
                return Err("frozen source scope changed".into());
            }
            if let ResourceScope::Resources { ids } = &source.scope
                && !self
                    .safe_views
                    .iter()
                    .any(|v| v.source_instance == source.source_instance)
                && ids.iter().map(String::as_str).collect::<BTreeSet<_>>()
                    != window
                        .resources
                        .iter()
                        .map(|p| p.resource.as_str())
                        .collect()
            {
                return Err("frozen resources do not match authorized scope".into());
            }
        }
        Ok(())
    }
}

pub fn supported_schema(version: u32) -> bool {
    matches!(
        version,
        LEGACY_SCHEMA_VERSION | PREVIOUS_SCHEMA_VERSION | SCHEMA_VERSION
    )
}

pub fn valid_namespace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Legacy manifests cannot reconstruct an installation identity. Retire this
/// fallback when retained v1/v2 windows have been retired.
pub fn effective_namespace(namespace: &str) -> &str {
    if namespace.is_empty() {
        "legacy"
    } else {
        namespace
    }
}

pub fn validate_excluded_runs(runs: &[String]) -> Result<(), String> {
    let mut previous: Option<&str> = None;
    for run in runs {
        let id = if let Some((namespace, id)) = run.split_once("::") {
            if !valid_namespace(namespace) {
                return Err("invalid excluded Run namespace".into());
            }
            id
        } else {
            run
        };
        validate_resource_id(id)?;
        if id.contains('/') || previous.is_some_and(|previous| previous >= run.as_str()) {
            return Err("excluded Runs must be sorted unique exact identities".into());
        }
        previous = Some(run);
    }
    Ok(())
}

/// The Run-bearing resource names shared by the recorder and graders.
pub fn resource_run(resource: &str) -> Option<&str> {
    let mut parts = resource.split('/');
    match (parts.next(), parts.next()) {
        (Some("run" | "trace" | "grade"), Some(run)) => Some(run),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceWindow {
    pub source_instance: String,
    /// Installation join domain, independent of this physical source root.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub join_namespace: String,
    /// Opaque physical directory identity; contains no path.
    pub root_fingerprint: String,
    pub scope: ResourceScope,
    pub resources: Vec<ResourceWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceWindow {
    pub resource: String,
    /// Exclusive lower and inclusive upper complete-record positions.
    /// Protocol v1 supports full-prefix capture only. V2 can freeze a delta.
    pub after: usize,
    pub through: usize,
    /// Canonical digest of the complete records in `(after, through]`.
    pub history_digest: String,
    /// Canonical digest of the resource reconstructed at the upper bound.
    pub snapshot_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_cursor: Option<anchor_kernel::HistoryCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_cursor: Option<anchor_kernel::HistoryCursor>,
}

impl ResourceWindow {
    pub fn validate_for(&self, schema: u32, has_parent: bool) -> Result<(), String> {
        validate_resource_id(&self.resource)?;
        if self.after > self.through
            || !is_digest(&self.history_digest)
            || !is_digest(&self.snapshot_digest)
        {
            return Err("invalid frozen resource positions or digests".into());
        }
        let valid = match schema {
            LEGACY_SCHEMA_VERSION => {
                self.after == 0
                    && self.start_cursor.is_none()
                    && self.end_cursor.is_none()
                    && !has_parent
            }
            PREVIOUS_SCHEMA_VERSION | SCHEMA_VERSION => {
                self.end_cursor.as_ref().is_some_and(|end| {
                    end.resource() == self.resource
                        && end.place() == self.through
                        && self.start_cursor.as_ref().map_or(self.after == 0, |start| {
                            start.resource() == self.resource
                                && start.place() == self.after
                                && start.offset() <= end.offset()
                        })
                        && (has_parent || (self.after == 0 && self.start_cursor.is_none()))
                })
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err("invalid frozen physical cursors".into())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowResponse {
    pub schema_version: u32,
    pub manifest: WindowManifest,
    pub manifest_digest: String,
    pub signals: Value,
    pub coverage: WindowCoverage,
}

/// A bounded continuation over the exact signal ranking of an old window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalPageRequest {
    pub schema_version: u32,
    pub window: WindowRequest,
    pub offset: usize,
    pub limit: usize,
}

impl SignalPageRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION
            || !(1..=40).contains(&self.limit)
            || self.window.manifest.is_none()
            || self.window.advance_from.is_some()
        {
            return Err("signal pagination requires a frozen window and a limit in 1..=40".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalPageResponse {
    pub schema_version: u32,
    pub manifest_digest: String,
    pub offset: usize,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub entries: Vec<SignalPageEntry>,
    pub io: anchor_kernel::HistoryReadMetrics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalPageEntry {
    pub exemplar: bool,
    pub signal: Value,
}

pub use crate::projection::Key as EvidenceKey;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRequest {
    pub schema_version: u32,
    /// Evidence reads require a previously frozen manifest.
    pub window: WindowRequest,
    pub keys: Vec<EvidenceKey>,
}

impl EvidenceRequest {
    pub fn validate(&self) -> Result<(), String> {
        if !supported_schema(self.schema_version) {
            return Err("unsupported evidence request version".into());
        }
        let manifest = self
            .window
            .manifest
            .as_ref()
            .ok_or("evidence requires a frozen window manifest")?;
        if self.keys.len() > MAX_EVIDENCE_KEYS {
            return Err("evidence request exceeds the key limit".into());
        }
        let mut unique = BTreeSet::new();
        for key in &self.keys {
            if !unique.insert(key) {
                return Err("evidence request contains duplicate keys".into());
            }
            let position = manifest
                .sources
                .iter()
                .find(|source| source.source_instance == key.root)
                .and_then(|source| {
                    source
                        .resources
                        .iter()
                        .find(|position| position.resource == key.resource)
                })
                .ok_or("evidence key is outside the frozen window")?;
            if key.place <= position.after || key.place > position.through {
                return Err("evidence key is outside the frozen positions".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceResponse {
    pub schema_version: u32,
    pub manifest_digest: String,
    pub items: Vec<EvidenceItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceItem {
    pub key: EvidenceKey,
    /// Only the existing content-free Hub Row projection, not a raw Record.
    pub row: Value,
}

/// Counts describe this frozen authorized window, not the whole installation.
/// Logical coverage is distinct from measured application-level reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowCoverage {
    pub sources: usize,
    pub seen_resources: usize,
    pub seen_records: usize,
    /// Signal and exemplar entries returned, not raw resources investigated.
    pub included_in_summary: usize,
    /// Evidence drill-downs; a summary capture always reports zero.
    pub investigated: usize,
    /// Signal and exemplar entries omitted by the requested list limit.
    pub deferred: usize,
    /// A successful exact capture has none; unavailable input fails instead.
    pub unavailable: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io: Option<anchor_kernel::HistoryReadMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_io: Option<anchor_kernel::HistoryReadMetrics>,
}

/// Discovery describes unavailable denominators, never invented zero coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    NotGranted,
    MissingSource,
    /// Generic CLI queries cannot infer a Host authorization decision.
    NotAuthorizedOrMissingSource,
    EmptyInScope,
    NoMatchingGrades,
    PartialCoverage,
    Conflict,
    Analyzable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Applicability {
    pub run_lifecycle: CapabilityStatus,
    pub observations: CapabilityStatus,
    pub grades: CapabilityStatus,
    pub stable_configurations: CapabilityStatus,
    pub runs_without_observations: Option<usize>,
    pub observations_without_runs: Option<usize>,
    pub ungraded_runs: Option<usize>,
    pub graded_runs: usize,
    pub complete_token_runs: usize,
    pub complete_cost_runs: usize,
    pub legacy_v1_configurations: usize,
    pub grade_conflicts: usize,
    pub graph_conflicts: usize,
}
