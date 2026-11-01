//! L1: projection. Records become fact rows, one per submission, keyed by
//! data root, resource and place. Only harness facts are kept: who submitted,
//! when, under which configuration, how it was judged, and for observation
//! records the observed facts, for grade records the named checks. Submission
//! content of any other kind, outcome
//! details and evidence are never projected, so fact rows hold no business
//! content. Beside them, and never mixed into them, a table may hold the
//! execution port's own record of what tool calls asked and answered, cut to
//! a bounded head ([`Detail`]): the value level a reader drills into once the
//! facts point at a call. A table can be dropped and rebuilt from its sources
//! at any time.

use std::collections::BTreeMap;

use anchor_facts::{AgentConfiguration, Grade, Observation};
use anchor_kernel::{Action, Outcome, Phase, PrincipalKind, Record, Resource, Verdict, Waiter};
use serde::Serialize;

use crate::source::{HistorySource, SourceError};

/// Where a record is: its data root, its resource, and its 1-based place in
/// that resource's history. Records need no other number.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Key {
    pub root: String,
    pub resource: String,
    pub place: usize,
}

/// A task's phase without its free-text details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
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

impl Status {
    pub fn of(phase: &Phase) -> Self {
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

/// What a record says beyond its kernel fields.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "body", rename_all = "snake_case")]
pub enum Body {
    /// Kernel fields only.
    Kernel,
    /// An execution port's observations of one invocation.
    Observation(Observation),
    /// A grade of one Run.
    Grade(Grade),
    /// An observation or grade record this hub's vocabulary cannot read.
    Unreadable,
}

/// One submission.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub key: Key,
    pub at: Option<u64>,
    pub principal: String,
    pub principal_kind: PrincipalKind,
    pub config: Option<String>,
    pub cost: u64,
    pub action: Action,
    pub slot: Option<String>,
    pub verdict: Verdict,
    /// The task's status after this record, for task resources.
    pub status: Option<Status>,
    #[serde(flatten)]
    pub body: Body,
}

/// A resource as it stands now.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub root: String,
    pub id: String,
    pub validator: String,
    pub version: u64,
    pub created_at: Option<u64>,
    pub created_config: Option<String>,
    pub status: Option<Status>,
}

/// An invocation as the execution port names it: Run, node, invocation.
pub type InvocationId = (String, String, u64);

/// What one tool call asked and what it answered, read from the execution
/// port's own evidence of the invocation rather than from the kernel. Facts
/// record shapes and classes only; this is the value level beside them, cut
/// to a bounded head, for drilling into a call the facts point at. It is
/// never needed to read the facts and never mixed into them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Detail {
    pub tool: String,
    pub tool_call_id: Option<String>,
    /// The arguments as JSON, cut to a head.
    pub arguments: String,
    pub ok: bool,
    /// The program a command tool was asked to run.
    pub program: Option<String>,
    /// How that command ended.
    pub command: Option<anchor_facts::CommandEnd>,
    /// The head of what the call answered: a command's standard error, or
    /// its standard output when that is empty, or the refusal text.
    pub answer: String,
}

/// Everything projected so far, as the trajectory algebra reads it.
#[derive(Clone, Copy)]
pub struct Facts<'a> {
    /// In projection order: by root, then resource, then place.
    pub rows: &'a [Row],
    pub resources: &'a BTreeMap<(String, String), Entry>,
    /// Configuration manifests by digest.
    pub configurations: &'a BTreeMap<String, AgentConfiguration>,
    /// Other immutable configuration manifests (currently invocation envelopes).
    pub manifests: &'a BTreeMap<String, serde_json::Value>,
    /// Per invocation, the details of its tool calls in call order, where
    /// the execution port's evidence was found.
    pub details: &'a BTreeMap<InvocationId, Vec<Detail>>,
}

/// Storage of projected facts. The in-memory table is the first backend.
pub trait FactTable {
    fn insert(&mut self, row: Row);
    fn resource(&mut self, entry: Entry);
    fn configuration(&mut self, digest: String, manifest: AgentConfiguration);
    fn manifest(&mut self, digest: String, manifest: serde_json::Value);
    fn details(&mut self, invocation: InvocationId, details: Vec<Detail>);
    fn facts(&self) -> Facts<'_>;
}

#[derive(Default)]
pub struct MemoryTable {
    rows: Vec<Row>,
    resources: BTreeMap<(String, String), Entry>,
    configurations: BTreeMap<String, AgentConfiguration>,
    manifests: BTreeMap<String, serde_json::Value>,
    details: BTreeMap<InvocationId, Vec<Detail>>,
}

impl FactTable for MemoryTable {
    fn insert(&mut self, row: Row) {
        self.rows.push(row);
    }
    fn resource(&mut self, entry: Entry) {
        self.resources
            .insert((entry.root.clone(), entry.id.clone()), entry);
    }
    fn configuration(&mut self, digest: String, manifest: AgentConfiguration) {
        self.configurations.insert(digest, manifest);
    }
    fn manifest(&mut self, digest: String, manifest: serde_json::Value) {
        self.manifests.insert(digest, manifest);
    }
    fn details(&mut self, invocation: InvocationId, details: Vec<Detail>) {
        self.details.insert(invocation, details);
    }
    fn facts(&self) -> Facts<'_> {
        Facts {
            rows: &self.rows,
            resources: &self.resources,
            configurations: &self.configurations,
            manifests: &self.manifests,
            details: &self.details,
        }
    }
}

/// Characters kept of an argument or an answer.
pub const HEAD: usize = 600;

/// The first [`HEAD`] characters of `text`, marked when cut.
pub fn head(text: &str) -> String {
    match text.char_indices().nth(HEAD) {
        Some((end, _)) => format!("{}…[{} chars]", &text[..end], text.chars().count()),
        None => text.to_owned(),
    }
}

/// The tool call details of one invocation's evidence, as the Goose
/// execution port writes it: `{key: {run_id, node_id, invocation},
/// tool_calls: [{tool, tool_call_id, arguments, result}]}`.
pub fn details_of(evidence: &serde_json::Value) -> Option<(InvocationId, Vec<Detail>)> {
    let key = &evidence["key"];
    let id = (
        key["run_id"].as_str()?.to_owned(),
        key["node_id"].as_str()?.to_owned(),
        key["invocation"].as_u64()?,
    );
    let calls = evidence["tool_calls"].as_array()?;
    let details = calls
        .iter()
        .map(|call| {
            let result = &call["result"];
            let ok = result["ok"].as_bool().unwrap_or(false);
            let output = &result["output"];
            let body = output["output"]
                .as_array()
                .or_else(|| output.as_array())
                .and_then(|contents| contents.iter().find(|c| c["type"] == "json"))
                .map(|content| &content["value"]);
            let answer = match body {
                Some(body) => {
                    let stderr = body["stderr"].as_str().unwrap_or_default();
                    if stderr.is_empty() {
                        body["stdout"].as_str().unwrap_or_default().to_owned()
                    } else {
                        stderr.to_owned()
                    }
                }
                None if ok => output.to_string(),
                None => result["error"]
                    .as_str()
                    .map_or_else(|| result["error"].to_string(), str::to_owned),
            };
            Detail {
                tool: call["tool"].as_str().unwrap_or_default().to_owned(),
                tool_call_id: call["tool_call_id"].as_str().map(str::to_owned),
                arguments: head(&call["arguments"].to_string()),
                ok,
                program: anchor_facts::command_program(&call["arguments"]),
                command: body.and_then(anchor_facts::command_end),
                answer: head(&answer),
            }
        })
        .collect();
    Some((id, details))
}

/// Projects sources into a table, remembering per resource how far it has
/// read so that a later pull adds only what was appended since.
#[derive(Default)]
pub struct Projector {
    read: BTreeMap<(String, String), usize>,
}

impl Projector {
    /// Returns how many rows were added.
    pub fn pull(
        &mut self,
        source: &dyn HistorySource,
        table: &mut dyn FactTable,
    ) -> Result<usize, SourceError> {
        let root = source.root().to_owned();
        let mut added = 0;
        for id in source.list()? {
            // Removed since it was listed.
            let Some(resource) = source.load(&id)? else {
                continue;
            };
            if resource.validator == anchor_facts::CONFIGURATION
                && let Some(digest) = id.strip_prefix("config/")
                && let Ok(manifest) =
                    serde_json::from_value::<AgentConfiguration>(resource.state.clone())
                // Content-addressed: a manifest that does not hash to its
                // name is not that configuration.
                && manifest.digest() == digest
            {
                table.configuration(digest.to_owned(), manifest);
            } else if resource.validator == anchor_facts::CONFIGURATION
                && let Some(digest) = id.strip_prefix("config/")
                && anchor_facts::text_digest(&anchor_facts::canonical(&resource.state).to_string())
                    == digest
            {
                table.manifest(digest.to_owned(), resource.state.clone());
            }
            let read = self.read.entry((root.clone(), id.clone())).or_default();
            for record in source.history_after(&id, *read)? {
                *read += 1;
                added += 1;
                table.insert(row(&root, &id, *read, &resource.validator, record));
            }
            table.resource(entry(&root, &resource));
        }
        Ok(added)
    }
}

fn row(root: &str, id: &str, place: usize, validator: &str, record: Record) -> Row {
    let body = match validator {
        anchor_facts::OBSERVATION => serde_json::from_value(record.submission.content)
            .map(Body::Observation)
            .unwrap_or(Body::Unreadable),
        anchor_facts::GRADE => serde_json::from_value(record.submission.content)
            .map(Body::Grade)
            .unwrap_or(Body::Unreadable),
        _ => Body::Kernel,
    };
    let principal = record.stamp.principal();
    Row {
        key: Key {
            root: root.to_owned(),
            resource: id.to_owned(),
            place,
        },
        at: record.stamp.at(),
        principal: principal.id.clone(),
        principal_kind: principal.kind,
        config: principal.config.clone(),
        cost: record.stamp.cost(),
        action: record.submission.action,
        slot: record.submission.slot,
        verdict: record.verdict,
        status: record
            .after
            .as_ref()
            .and_then(Resource::phase)
            .map(Status::of),
        body,
    }
}

fn entry(root: &str, resource: &Resource) -> Entry {
    Entry {
        root: root.to_owned(),
        id: resource.id.clone(),
        validator: resource.validator.clone(),
        version: resource.version,
        created_at: resource.created.as_ref().and_then(|c| c.at),
        created_config: resource.created.as_ref().and_then(|c| c.config.clone()),
        status: resource.phase().map(Status::of),
    }
}
