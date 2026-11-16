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

use std::collections::{BTreeMap, BTreeSet};

use anchor_facts::{AgentConfiguration, Grade};
use anchor_kernel::{Action, PrincipalKind, Record, Resource, Verdict};
use serde::Serialize;

use crate::source::{HistorySource, SourceCapabilities, SourceError};

pub use anchor_facts::SafeRunStatus as Status;
pub use anchor_facts::projection::{Body, Key, Row};

/// A resource as it stands now.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub namespace: String,
    pub provenance: anchor_facts::ProvenanceCategory,
    pub graph_digest: Option<String>,
    pub root: String,
    pub id: String,
    pub validator: String,
    pub version: u64,
    pub created_at: Option<u64>,
    pub settled_at: Option<u64>,
    pub graph: Option<String>,
    pub created_config: Option<String>,
    pub status: Option<Status>,
}

/// An invocation as the execution port names it: Run, node, invocation.
pub type InvocationId = (String, String, String, u64);

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
    pub sources: &'a BTreeMap<String, (String, SourceCapabilities)>,
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
    fn source(&mut self, root: &str, namespace: &str, capabilities: SourceCapabilities);
    fn retire_source(&mut self, root: &str);
    fn insert(&mut self, row: Row);
    fn resource(&mut self, entry: Entry);
    fn details(&mut self, invocation: InvocationId, details: Vec<Detail>);
    /// Associate a content-addressed manifest with the resource that supplied
    /// it. The association lets shared digests survive one source's removal.
    fn configuration_for(
        &mut self,
        root: &str,
        resource: &str,
        digest: String,
        manifest: AgentConfiguration,
    );
    fn manifest_for(
        &mut self,
        root: &str,
        resource: &str,
        digest: String,
        manifest: serde_json::Value,
    );
    /// Retire every projection belonging to one `(root, resource)` pair.
    fn retire(&mut self, root: &str, resource: &str);
    /// Remove content-addressed references supplied by this resource.
    fn clear_resource_refs(&mut self, root: &str, resource: &str);
    fn resource_entry(&self, root: &str, resource: &str) -> Option<Entry>;
    fn facts(&self) -> Facts<'_>;
}

#[derive(Clone, Default)]
pub struct MemoryTable {
    sources: BTreeMap<String, (String, SourceCapabilities)>,
    rows: Vec<Row>,
    resources: BTreeMap<(String, String), Entry>,
    configurations: BTreeMap<String, AgentConfiguration>,
    manifests: BTreeMap<String, serde_json::Value>,
    details: BTreeMap<InvocationId, Vec<Detail>>,
    manifest_refs: BTreeMap<String, BTreeSet<(String, String)>>,
    resource_refs: BTreeMap<(String, String), String>,
}

impl FactTable for MemoryTable {
    fn source(&mut self, root: &str, namespace: &str, capabilities: SourceCapabilities) {
        self.sources
            .insert(root.to_owned(), (namespace.to_owned(), capabilities));
    }
    fn retire_source(&mut self, root: &str) {
        self.sources.remove(root);
    }
    fn insert(&mut self, row: Row) {
        // Retirements can be followed by a cold re-project. Keep the visible
        // order canonical instead of depending on source/discovery order.
        match self
            .rows
            .binary_search_by(|existing| existing.key.cmp(&row.key))
        {
            Ok(index) => self.rows[index] = row,
            Err(index) => self.rows.insert(index, row),
        }
    }
    fn resource(&mut self, entry: Entry) {
        self.resources
            .insert((entry.root.clone(), entry.id.clone()), entry);
    }
    fn details(&mut self, invocation: InvocationId, details: Vec<Detail>) {
        self.details.insert(invocation, details);
    }
    fn configuration_for(
        &mut self,
        root: &str,
        resource: &str,
        digest: String,
        manifest: AgentConfiguration,
    ) {
        self.configurations.insert(digest.clone(), manifest);
        self.reference(root, resource, digest);
    }
    fn manifest_for(
        &mut self,
        root: &str,
        resource: &str,
        digest: String,
        manifest: serde_json::Value,
    ) {
        self.manifests.insert(digest.clone(), manifest);
        self.reference(root, resource, digest);
    }
    fn retire(&mut self, root: &str, resource: &str) {
        let owner = (root.to_owned(), resource.to_owned());
        self.rows
            .retain(|row| row.key.root != root || row.key.resource != resource);
        self.resources.remove(&owner);
        self.clear_resource_refs(root, resource);
    }
    fn clear_resource_refs(&mut self, root: &str, resource: &str) {
        let owner = (root.to_owned(), resource.to_owned());
        if let Some(digest) = self.resource_refs.remove(&owner) {
            let remove = self.manifest_refs.get_mut(&digest).is_some_and(|owners| {
                owners.remove(&owner);
                owners.is_empty()
            });
            if remove {
                self.manifest_refs.remove(&digest);
                self.configurations.remove(&digest);
                self.manifests.remove(&digest);
            }
        }
    }
    fn resource_entry(&self, root: &str, resource: &str) -> Option<Entry> {
        self.resources
            .get(&(root.to_owned(), resource.to_owned()))
            .cloned()
    }
    fn facts(&self) -> Facts<'_> {
        Facts {
            sources: &self.sources,
            rows: &self.rows,
            resources: &self.resources,
            configurations: &self.configurations,
            manifests: &self.manifests,
            details: &self.details,
        }
    }
}

impl MemoryTable {
    fn reference(&mut self, root: &str, resource: &str, digest: String) {
        let owner = (root.to_owned(), resource.to_owned());
        self.manifest_refs
            .entry(digest.clone())
            .or_default()
            .insert(owner.clone());
        self.resource_refs.insert(owner, digest);
    }

    pub fn clear_details(&mut self) {
        self.details.clear();
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
pub fn details_of(
    namespace: &str,
    evidence: &serde_json::Value,
) -> Option<(InvocationId, Vec<Detail>)> {
    let key = &evidence["key"];
    let id = (
        namespace.to_owned(),
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
#[derive(Clone, Default)]
pub struct Projector {
    read: BTreeMap<(String, String), Read>,
}

#[derive(Clone)]
struct Read {
    place: usize,
    created: Option<anchor_kernel::Creation>,
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
        let mut listed = source.list()?;
        listed.sort();
        table.source(&root, source.namespace(), source.capabilities());
        let listed_set: BTreeSet<String> = listed.iter().cloned().collect();

        // A successful enumeration is the boundary at which old resources
        // become known to be absent. Enumeration/read failures return before
        // this point and therefore cannot masquerade as an empty root.
        let retired: Vec<String> = self
            .read
            .keys()
            .filter(|(seen_root, id)| seen_root == &root && !listed_set.contains(id))
            .map(|(_, id)| id.clone())
            .collect();
        for id in retired {
            self.retire(source, table, &id);
            source.forget(&id);
        }

        for id in listed {
            // Removed since it was listed.
            let Some(resource) = source.load(&id)? else {
                self.retire(source, table, &id);
                source.forget(&id);
                continue;
            };
            let previous = table.resource_entry(&root, &id);
            let read_before = self.read.get(&(root.clone(), id.clone()));
            let changed = previous.as_ref().is_some_and(|entry| {
                entry.validator != resource.validator || resource.version < entry.version
            }) || read_before.is_some_and(|read| read.created != resource.created);
            let truncated = match read_before {
                Some(read) => source.history_len(&id)? < read.place,
                None => false,
            };
            if source.take_reset(&id) || changed || truncated {
                self.retire(source, table, &id);
            } else if previous.is_none() && read_before.is_some() {
                // Keep the table and cursor in lockstep even if a caller
                // supplied a partially initialized table.
                self.retire(source, table, &id);
            }

            table.clear_resource_refs(&root, &id);
            if resource.validator == anchor_facts::CONFIGURATION
                && let Some(digest) = id.strip_prefix("config/")
                && let Ok(manifest) =
                    serde_json::from_value::<AgentConfiguration>(resource.state.clone())
                // Content-addressed: a manifest that does not hash to its
                // name is not that configuration.
                && manifest.digest() == digest
            {
                table.configuration_for(&root, &id, digest.to_owned(), manifest);
            } else if resource.validator == anchor_facts::CONFIGURATION
                && let Some(digest) = id.strip_prefix("config/")
                && anchor_facts::text_digest(&anchor_facts::canonical(&resource.state).to_string())
                    == digest
            {
                table.manifest_for(&root, &id, digest.to_owned(), resource.state.clone());
            }
            let read = self
                .read
                .entry((root.clone(), id.clone()))
                .or_insert_with(|| Read {
                    place: source.starting_after(&id),
                    created: resource.created.clone(),
                });
            for record in source.history_after(&id, read.place)? {
                read.place += 1;
                added += 1;
                table.insert(row(
                    &root,
                    source.namespace(),
                    &id,
                    read.place,
                    &resource.validator,
                    record,
                ));
            }
            table.resource(entry(&root, source.namespace(), &resource));
        }
        Ok(added)
    }

    fn retire(&mut self, source: &dyn HistorySource, table: &mut dyn FactTable, id: &str) {
        let root = source.root();
        self.read.remove(&(root.to_owned(), id.to_owned()));
        table.retire(root, id);
    }

    /// Retire every resource from a root that was removed from discovery.
    pub fn retire_root(&mut self, root: &str, table: &mut dyn FactTable) {
        table.retire_source(root);
        let ids: Vec<String> = self
            .read
            .keys()
            .filter(|(seen_root, _)| seen_root == root)
            .map(|(_, id)| id.clone())
            .collect();
        for id in ids {
            self.read.remove(&(root.to_owned(), id.clone()));
            table.retire(root, &id);
        }
    }
}

fn row(
    root: &str,
    namespace: &str,
    id: &str,
    place: usize,
    validator: &str,
    record: Record,
) -> Row {
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
        namespace: namespace.to_owned(),
        provenance: anchor_facts::ProvenanceCategory::Unknown,
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

fn entry(root: &str, namespace: &str, resource: &Resource) -> Entry {
    Entry {
        namespace: namespace.to_owned(),
        provenance: anchor_facts::ProvenanceCategory::Unknown,
        graph_digest: resource
            .state
            .get("graph_digest")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        root: root.to_owned(),
        id: resource.id.clone(),
        validator: resource.validator.clone(),
        version: resource.version,
        settled_at: None,
        graph: None,
        created_at: resource.created.as_ref().and_then(|c| c.at),
        created_config: resource.created.as_ref().and_then(|c| c.config.clone()),
        status: resource.phase().map(Status::of),
    }
}

/// Project only owner-exported whitelist fields, never a raw Run snapshot.
pub fn project_safe_view(view: &anchor_facts::SafeViewSnapshot, table: &mut MemoryTable) {
    use anchor_kernel::{Effect, Reason, Verification};
    table.source(
        &view.source_instance,
        &view.join_namespace,
        SourceCapabilities {
            runs: view.source_instance.starts_with("runs-view-"),
            grades: view.source_instance.starts_with("grades-view-"),
            observations: false,
        },
    );
    let row =
        |fact: &anchor_facts::view::FactRef, at, provenance, config, cost, slot, status, body| {
            Row {
                key: Key {
                    root: fact.source_instance.clone(),
                    resource: fact.resource.clone(),
                    place: fact.place,
                },
                namespace: view.join_namespace.clone(),
                provenance,
                at,
                principal: "anchor/safe-view".into(),
                principal_kind: PrincipalKind::Code,
                config,
                cost,
                action: Action::Write,
                slot,
                verdict: Verdict {
                    effect: Effect::Accepted,
                    verification: Verification::NotApplicable,
                    reason: Reason::Unclassified,
                },
                status,
                body,
            }
        };
    for run in &view.runs {
        table.resource(Entry {
            namespace: view.join_namespace.clone(),
            provenance: run.provenance,
            graph: Some(run.graph.clone()),
            graph_digest: Some(run.graph_digest.clone()),
            root: view.source_instance.clone(),
            id: run.fact_ref.resource.clone(),
            validator: "validator/graph".into(),
            version: run.fact_ref.place as u64,
            created_at: run.created_at_ms,
            settled_at: run.settled_at_ms,
            created_config: None,
            status: Some(run.status),
        });
        if run.fact_ref.place > 0 {
            let slot = run.accepted_slots.first();
            table.insert(row(
                &run.fact_ref,
                slot.and_then(|s| s.at_ms).or(run.last_at_ms),
                run.provenance,
                slot.and_then(|s| s.config.clone()),
                slot.map_or(0, |s| s.cost),
                slot.map(|s| format!("{}#{}", s.node_id, s.invocation)),
                Some(run.status),
                Body::Kernel,
            ));
        }
    }
    for grade in &view.grades {
        table.insert(row(
            &grade.fact_ref,
            grade.at_ms,
            grade.provenance,
            Some(grade.grader_config_digest.clone()),
            0,
            None,
            None,
            Body::Grade(Grade {
                version: anchor_facts::VERSION,
                run_id: grade.run_id.clone(),
                task: grade.task.clone(),
                task_version: grade.task_version,
                checks: grade.checks.clone(),
            }),
        ));
    }
}
