//! L0/L4 frozen authorized reads. Unlike ordinary Hub discovery, this entry
//! opens only explicitly registered roots and never reads optional evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anchor_facts::window::{
    ANALYZER_VERSION, EvidenceItem, EvidenceRequest, EvidenceResponse, ResourceScope,
    ResourceWindow, SCHEMA_VERSION, SignalPageEntry, SignalPageRequest, SignalPageResponse,
    SourceWindow, WindowCoverage, WindowManifest, WindowRequest, WindowResponse, WindowSource,
    effective_namespace, resource_run, source_fingerprint, supported_analyzer,
    validate_resource_id,
};
use anchor_kernel::{HistoryReadMetrics, Record, Resource};

use crate::analyzers::{Analyzer, signals::Signals};
use crate::query::Query;
use crate::source::{DataRoot, HistorySource, SourceError};
use crate::table::{Body, FactTable, MemoryTable, Projector};

/// Capture or replay an exact authorized prefix. Unavailable or inconsistent
/// sources fail the query; they are never reported as empty healthy data.
pub fn capture(request: &WindowRequest) -> Result<WindowResponse, String> {
    let frozen = load_window(request)?;
    let mut signals = Signals {
        limit: request.limit,
    }
    .window_table_version(
        &Query::new(frozen.table.facts(), Some(request.now_ms)),
        |key| in_delta(&frozen.manifest, key),
        frozen.manifest.analyzer_version,
    );
    signals.longitudinal = (frozen.manifest.schema_version == SCHEMA_VERSION).then(|| {
        crate::analyzers::longitudinal::summarize(
            &Query::new(frozen.table.facts(), Some(request.now_ms)),
            &frozen.manifest,
        )
    });
    frozen_applicability(&mut signals.applicability, &frozen.manifest);
    let coverage = WindowCoverage {
        sources: frozen.manifest.sources.len(),
        seen_resources: frozen.seen_resources,
        seen_records: frozen.seen_records,
        included_in_summary: signals.signals.len() + signals.exemplars.len(),
        investigated: 0,
        deferred: signals.omitted.values().sum(),
        unavailable: 0,
        io: Some(frozen.io),
        support_io: frozen.support_io,
    };
    Ok(WindowResponse {
        schema_version: SCHEMA_VERSION,
        manifest_digest: frozen.manifest.digest(),
        manifest: frozen.manifest,
        signals: serde_json::to_value(signals).map_err(|_| "signals cannot be encoded")?,
        coverage,
    })
}

/// The frozen request knows its grants; ordinary CLI discovery does not.
fn frozen_applicability(
    applicability: &mut anchor_facts::window::Applicability,
    manifest: &WindowManifest,
) {
    use anchor_facts::window::CapabilityStatus as Status;
    if manifest.schema_version != SCHEMA_VERSION {
        return;
    }
    for status in [
        &mut applicability.run_lifecycle,
        &mut applicability.observations,
        &mut applicability.grades,
    ] {
        if *status == Status::NotAuthorizedOrMissingSource {
            *status = Status::NotGranted;
        }
    }
    let missing_owner = |prefix: &str| {
        let views: Vec<_> = manifest
            .safe_views
            .iter()
            .filter(|view| view.source_instance.starts_with(prefix))
            .collect();
        !views.is_empty()
            && views
                .iter()
                .all(|view| view.owner_root_fingerprint.is_none() && view.resources.is_empty())
    };
    if applicability.run_lifecycle == Status::EmptyInScope && missing_owner("runs-view-") {
        applicability.run_lifecycle = Status::MissingSource;
        applicability.runs_without_observations = None;
        applicability.observations_without_runs = None;
    }
    if applicability.grades == Status::EmptyInScope && missing_owner("grades-view-") {
        applicability.grades = Status::MissingSource;
    }
}

pub fn signal_page(request: &SignalPageRequest) -> Result<SignalPageResponse, String> {
    request.validate()?;
    let frozen = load_window(&request.window)?;
    let signals = Signals { limit: usize::MAX }.window_table_version(
        &Query::new(frozen.table.facts(), Some(request.window.now_ms)),
        |key| in_delta(&frozen.manifest, key),
        frozen.manifest.analyzer_version,
    );
    let total = signals.signals.len() + signals.exemplars.len();
    let entries = signals
        .signals
        .into_iter()
        .map(|signal| (false, signal))
        .chain(signals.exemplars.into_iter().map(|signal| (true, signal)))
        .skip(request.offset)
        .take(request.limit)
        .map(|(exemplar, signal)| {
            Ok(SignalPageEntry {
                exemplar,
                signal: serde_json::to_value(signal).map_err(|_| "signal cannot be encoded")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let next = request.offset.saturating_add(entries.len());
    Ok(SignalPageResponse {
        schema_version: SCHEMA_VERSION,
        manifest_digest: frozen.manifest.digest(),
        offset: request.offset,
        total,
        next_offset: (next < total).then_some(next),
        entries,
        io: frozen.io,
    })
}

/// Read selected structured rows from exactly the previously captured window.
/// All history and identity checks are shared with summary replay.
pub fn evidence(request: &EvidenceRequest) -> Result<EvidenceResponse, String> {
    request.validate()?;
    let frozen = load_window(&request.window)?;
    let facts = frozen.table.facts();
    let items = request
        .keys
        .iter()
        .map(|key| {
            let row = facts
                .rows
                .iter()
                .find(|row| {
                    row.key.root == key.root
                        && row.key.resource == key.resource
                        && row.key.place == key.place
                })
                .ok_or("frozen evidence key was not projected")?;
            Ok(EvidenceItem {
                key: key.clone(),
                row: serde_json::to_value(row).map_err(|_| "evidence row cannot be encoded")?,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(EvidenceResponse {
        schema_version: SCHEMA_VERSION,
        manifest_digest: frozen.manifest.digest(),
        items,
    })
}

struct FrozenWindow {
    manifest: WindowManifest,
    table: MemoryTable,
    seen_resources: usize,
    seen_records: usize,
    io: HistoryReadMetrics,
    support_io: Option<HistoryReadMetrics>,
}

fn load_window(request: &WindowRequest) -> Result<FrozenWindow, String> {
    request.validate()?;
    let views = request
        .manifest
        .as_ref()
        .map_or(&request.safe_views, |manifest| &manifest.safe_views);
    let support_history = request.manifest.as_ref().map_or(
        request.support_history || request.advance_from.is_some(),
        |m| m.support_history,
    );
    let mut support_io = support_history.then(HistoryReadMetrics::default);
    let mut sources = request.sources.clone();
    for source in &mut sources {
        normalize_scope(&mut source.scope);
    }
    sources.sort_by(|a, b| a.source_instance.cmp(&b.source_instance));
    let excluded = |namespace: &str, run: &str| {
        request
            .exclude_runs
            .iter()
            .any(|id| id == run || id == &format!("{namespace}::{run}"))
    };
    let replay = request.manifest.is_some();
    let baseline = request.manifest.as_ref().or(request.advance_from.as_ref());
    let previous = baseline
        .map(|manifest| validate_manifest(request, &sources, manifest, replay))
        .transpose()?;
    if request
        .advance_from
        .as_ref()
        .is_some_and(|manifest| manifest.schema_version != SCHEMA_VERSION)
    {
        return Err("a legacy window requires one fresh physical capture before advancing".into());
    }
    let mut table = MemoryTable::default();
    let mut projector = Projector::default();
    let mut windows = Vec::with_capacity(sources.len());
    let mut seen_records = 0;
    let mut seen_resources = 0;
    let mut io = HistoryReadMetrics::default();
    let mut physical_roots = BTreeSet::new();
    for source in &sources {
        if let Some(view) = views
            .iter()
            .find(|view| view.source_instance == source.source_instance)
        {
            if source.join_namespace != view.join_namespace || source.scope != view.scope {
                return Err("safe view authorization changed".into());
            }
            if let Some(parent) = request.advance_from.as_ref().and_then(|m| {
                m.safe_views
                    .iter()
                    .find(|v| v.source_instance == view.source_instance)
            }) {
                view.continues(parent)?;
            }
            seen_resources += view.resources.len();
            seen_records += view
                .resources
                .iter()
                .map(|p| p.through - p.after)
                .sum::<usize>();
            let allowed = |run: &str| !excluded(&view.join_namespace, run);
            if view.runs.iter().any(|run| !allowed(&run.run_id))
                || view.grades.iter().any(|grade| !allowed(&grade.run_id))
            {
                if replay
                    && baseline.is_some_and(|manifest| manifest.analyzer_version < ANALYZER_VERSION)
                {
                    return Err("legacy safe-view exclusions changed the frozen ranking; capture a new window".into());
                }
                // Keep the original owner snapshot for replay; only the query
                // projection omits excluded Run/Grade records, as facts do.
                let mut projection = view.clone();
                projection.runs.retain(|run| allowed(&run.run_id));
                projection.grades.retain(|grade| allowed(&grade.run_id));
                crate::table::project_safe_view(&projection, &mut table);
            } else {
                crate::table::project_safe_view(view, &mut table);
            }
            windows.push(SourceWindow {
                source_instance: view.source_instance.clone(),
                join_namespace: view.join_namespace.clone(),
                root_fingerprint: view.root_fingerprint.clone(),
                scope: view.scope.clone(),
                resources: view.resources.clone(),
            });
            continue;
        }
        if source.source_instance.starts_with("runs-view-")
            || source.source_instance.starts_with("grades-view-")
        {
            return Err("authorized safe view is missing".into());
        }
        let fingerprint = root_fingerprint(&source.root)?;
        if !physical_roots.insert(fingerprint.clone()) {
            return Err("window contains duplicate physical sources".into());
        }
        let previous = previous
            .as_ref()
            .and_then(|sources| sources.get(source.source_instance.as_str()).copied());
        if previous.is_some_and(|window| window.root_fingerprint != fingerprint) {
            return Err("frozen source identity changed".into());
        }
        check_links(&source.root)?;
        let reader = DataRoot::open(&source.root, &source.source_instance)
            .map_err(|_| "authorized source is unavailable".to_owned())?;
        // Scope precedes both projection and configuration association. In
        // particular, a trace does not implicitly authorize its config resource.
        let ids: Vec<String> = match previous.filter(|_| replay) {
            Some(window) => window
                .resources
                .iter()
                .map(|position| position.resource.clone())
                .collect(),
            None => match &source.scope {
                ResourceScope::All {} => reader
                    .list()
                    .map_err(|_| "authorized source cannot be listed".to_owned())?,
                ResourceScope::Resources { ids } => ids.clone(),
            },
        };
        if !replay
            && previous.is_some_and(|window| {
                window
                    .resources
                    .iter()
                    .any(|position| ids.binary_search(&position.resource).is_err())
            })
        {
            return Err("a previously captured resource disappeared".into());
        }
        let mut restricted = FrozenSource {
            name: source.source_instance.clone(),
            namespace: effective_namespace(&source.join_namespace).to_owned(),
            capabilities: reader.capabilities(),
            resources: BTreeMap::new(),
        };
        let bounds: BTreeMap<_, _> = previous
            .into_iter()
            .flat_map(|window| &window.resources)
            .map(|position| (position.resource.as_str(), position))
            .collect();
        let mut positions = Vec::with_capacity(ids.len());
        for id in &ids {
            validate_resource_id(id)?;
            let bound = bounds.get(id.as_str()).copied();
            let (snapshot, mut records, mut position) = read_position(&reader, id, bound, replay)?;
            let delta_records = position.through - position.after;
            if replay && support_history && position.end_cursor.is_some() {
                // Strict replay already checked this full prefix. Reuse it for
                // support instead of reading and parsing the same history twice.
                if position.prefix_digest.is_none() {
                    position.prefix_digest = Some(digest(&records)?);
                }
            } else if support_history {
                let support_reader = DataRoot::open(&source.root, &source.source_instance)
                    .map_err(|_| "authorized support unavailable")?;
                let end = if replay {
                    position.end_cursor.as_ref()
                } else {
                    bound.and_then(|p| p.end_cursor.as_ref())
                };
                let prefix = if let Some(end) = end {
                    support_reader
                        .history_window(id, None, Some(end))
                        .map_err(|_| "frozen resource history or snapshot changed")?
                        .records
                } else {
                    Vec::new()
                };
                if let Some(bound) = bound {
                    let full = if replay {
                        &prefix[..position.through.min(prefix.len())]
                    } else {
                        &prefix[..]
                    };
                    if full.len() < bound.through
                        || bound.prefix_digest.as_ref().is_some_and(|expected| {
                            digest(&full[..bound.through]).as_ref() != Ok(expected)
                        })
                    {
                        return Err("frozen resource history or snapshot changed".into());
                    }
                }
                let mut all = prefix;
                if replay {
                    all.truncate(position.after);
                }
                all.extend(records);
                position.prefix_digest = Some(digest(&all)?);
                records = all;
                support_io
                    .as_mut()
                    .expect("enabled")
                    .accumulate(&support_reader.read_metrics());
            } else if replay {
                records.drain(..position.after);
            }
            seen_resources += 1;
            seen_records += delta_records;
            if !resource_run(id).is_some_and(|run| excluded(&restricted.namespace, run)) {
                let mut projected_snapshot = snapshot;
                // A delta has no trustworthy last-activity timestamp when it
                // has no task rows. Do not turn creation time into a new idle
                // finding; clock-only analysis is explicitly outside this view.
                if !support_history && position.after > 0 && records.is_empty() {
                    projected_snapshot.task = None;
                }
                restricted.resources.insert(
                    id.clone(),
                    FrozenResource {
                        snapshot: projected_snapshot,
                        after: if support_history { 0 } else { position.after },
                        records,
                    },
                );
            }
            positions.push(position);
        }
        // No writer lease is acquired. Check the root identity again instead
        // of claiming an atomic snapshot across resources or sources.
        if root_fingerprint(&source.root)? != fingerprint {
            return Err("authorized source changed while reading".into());
        }
        projector
            .pull(&restricted, &mut table)
            .map_err(|_| "frozen projection is unavailable".to_owned())?;
        io.accumulate(&reader.read_metrics());
        windows.push(SourceWindow {
            source_instance: source.source_instance.clone(),
            join_namespace: source.join_namespace.clone(),
            root_fingerprint: fingerprint,
            scope: source.scope.clone(),
            resources: positions,
        });
    }
    let facts = table.facts();
    if facts.rows.iter().any(|row| match &row.body {
        Body::Unreadable => true,
        Body::Observation(observation) => observation.version != anchor_facts::VERSION,
        Body::Grade(grade) => grade.version != anchor_facts::VERSION,
        Body::Kernel => false,
    }) || facts
        .configurations
        .values()
        .any(|configuration| !configuration.is_supported_version())
    {
        return Err("authorized structured fact schema is unavailable".into());
    }
    let analyzer = Signals {
        limit: request.limit,
    };
    if analyzer.version() != ANALYZER_VERSION {
        return Err("window analyzer version is unavailable".into());
    }
    let manifest = WindowManifest {
        schema_version: request
            .manifest
            .as_ref()
            .map_or(SCHEMA_VERSION, |manifest| manifest.schema_version),
        analyzer_version: request
            .manifest
            .as_ref()
            .map_or(ANALYZER_VERSION, |manifest| manifest.analyzer_version),
        now_ms: request.now_ms,
        limit: request.limit,
        sources: windows,
        safe_views: views.clone(),
        support_history,
        baseline_at_ms: request
            .manifest
            .as_ref()
            .and_then(|m| m.baseline_at_ms)
            .or_else(|| request.advance_from.as_ref().map(|m| m.now_ms)),
        parent_manifest_digest: request
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.parent_manifest_digest.clone())
            .or_else(|| request.advance_from.as_ref().map(WindowManifest::digest)),
        exclude_runs: request.exclude_runs.clone(),
    };
    if request
        .manifest
        .as_ref()
        .is_some_and(|expected| expected != &manifest)
    {
        return Err("frozen manifest changed".into());
    }
    Ok(FrozenWindow {
        manifest,
        table,
        seen_resources,
        seen_records,
        io,
        support_io,
    })
}

// Replay retains the checked full prefix; `position` still describes the delta.
fn read_position(
    reader: &DataRoot,
    id: &str,
    bound: Option<&ResourceWindow>,
    replay: bool,
) -> Result<(Resource, Vec<Record>, ResourceWindow), String> {
    if replay {
        let bound = bound.ok_or("frozen resource is missing")?;
        // Strict replay always validates the full prefix, even for a delta.
        let batch = reader
            .history_window(id, None, bound.end_cursor.as_ref())
            .map_err(|error| format!("frozen history is unavailable: {error}"))?;
        if batch.records.len() < bound.through {
            return Err("frozen history was truncated".into());
        }
        if bound
            .prefix_digest
            .as_ref()
            .is_some_and(|expected| digest(&batch.records).as_ref() != Ok(expected))
        {
            return Err("frozen resource history or snapshot changed".into());
        }
        let mut records = batch.records;
        records.truncate(bound.through);
        let snapshot = if bound.end_cursor.is_some() {
            batch.resource
        } else {
            reconstruct(id, &batch.resource, &records)?
        };
        let position = ResourceWindow {
            resource: id.into(),
            after: bound.after,
            through: bound.through,
            history_digest: digest(&records[bound.after..])?,
            snapshot_digest: digest(&snapshot)?,
            prefix_digest: bound.prefix_digest.clone(),
            start_cursor: bound.start_cursor.clone(),
            end_cursor: bound.end_cursor.clone(),
        };
        if position != *bound {
            return Err("frozen resource history or snapshot changed".into());
        }
        return Ok((snapshot, records, position));
    }
    let start = bound.and_then(|position| position.end_cursor.as_ref());
    let batch = reader
        .history_window(id, start, None)
        .map_err(|error| format!("authorized history is unavailable: {error}"))?;
    let snapshot_digest = digest(&batch.resource)?;
    if bound.is_some_and(|previous| {
        batch.records.is_empty() && previous.snapshot_digest != snapshot_digest
    }) {
        return Err("unchanged history has a changed snapshot".into());
    }
    let position = ResourceWindow {
        resource: id.into(),
        after: start.map_or(0, |cursor| cursor.place()),
        through: batch.cursor.place(),
        history_digest: digest(&batch.records)?,
        snapshot_digest,
        prefix_digest: None,
        start_cursor: start.cloned(),
        end_cursor: Some(batch.cursor),
    };
    Ok((batch.resource, batch.records, position))
}

fn normalize_scope(scope: &mut ResourceScope) {
    if let ResourceScope::Resources { ids } = scope {
        ids.sort();
    }
}

fn validate_manifest<'a>(
    request: &WindowRequest,
    sources: &[WindowSource],
    manifest: &'a WindowManifest,
    replay: bool,
) -> Result<BTreeMap<&'a str, &'a SourceWindow>, String> {
    manifest.validate_sources(sources)?;
    if !supported_analyzer(manifest.schema_version, manifest.analyzer_version)
        || (replay && (manifest.now_ms != request.now_ms || manifest.limit != request.limit))
        || (!replay && manifest.now_ms > request.now_ms)
        || (replay && manifest.exclude_runs != request.exclude_runs)
        || (!replay
            && manifest
                .exclude_runs
                .iter()
                .any(|run| request.exclude_runs.binary_search(run).is_err()))
    {
        return Err("frozen window parameters changed".into());
    }
    Ok(manifest
        .sources
        .iter()
        .map(|window| (window.source_instance.as_str(), window))
        .collect())
}

fn digest(value: &(impl serde::Serialize + ?Sized)) -> Result<String, String> {
    let value = serde_json::to_value(value).map_err(|_| "window cannot be encoded")?;
    Ok(anchor_facts::text_digest(
        &anchor_facts::canonical(&value).to_string(),
    ))
}

/// Rebuild state from the bounded history, never the latest mutable state.
/// An after snapshot can also record Kernel budget settlement on a refused
/// submission, so verdict alone is not a state-version boundary.
fn reconstruct(id: &str, current: &Resource, records: &[Record]) -> Result<Resource, String> {
    if current.id != id {
        return Err("authorized resource identity changed".into());
    }
    let mut version = 0_u64;
    let mut snapshot = None;
    for record in records {
        if record.submission.target != id
            || (record.verdict.is_accepted() && record.after.is_none())
        {
            return Err("authorized history has a state gap".into());
        }
        if let Some(after) = &record.after {
            version = version.checked_add(1).ok_or("resource version overflow")?;
            if after.id != id
                || after.version != version
                || after.validator != current.validator
                || after.created != current.created
            {
                return Err("authorized history has an inconsistent snapshot".into());
            }
            snapshot = Some(after.clone());
        }
    }
    let mut snapshot = match snapshot {
        Some(snapshot) => snapshot,
        None if current.version == 0 => current.clone(),
        None => return Err("initial resource snapshot is unavailable".into()),
    };
    if current.version < snapshot.version {
        return Err("current resource is behind its history".into());
    }
    if let Some(task) = &mut snapshot.task {
        task.budget.spent = records.iter().fold(0_u64, |spent, record| {
            spent.saturating_add(record.stamp.cost())
        });
    }
    Ok(snapshot)
}

fn root_fingerprint(path: &Path) -> Result<String, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "authorized source is unavailable".to_owned())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("authorized source must be a directory without a link".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(source_fingerprint(metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err("physical source identity is unavailable on this platform".into())
    }
}

/// HistoryReader encodes logical IDs. Inspect directory metadata without
/// duplicating its encoder or opening any unauthorized resource bodies.
fn check_links(root: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(root).map_err(|_| "authorized source cannot be listed")? {
        let entry = entry.map_err(|_| "authorized source cannot be listed")?;
        let kind = entry
            .file_type()
            .map_err(|_| "authorized source metadata is unavailable")?;
        if kind.is_symlink() {
            return Err("structured source contains a link".into());
        }
        if kind.is_dir() {
            for file in ["resource.json", "history.jsonl"] {
                match std::fs::symlink_metadata(entry.path().join(file)) {
                    Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                        return Err("structured resource contains a non-regular file".into());
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err("structured resource metadata is unavailable".into()),
                }
            }
        }
    }
    Ok(())
}

/// Only already-authorized, bounded data can enter the ordinary projector.
struct FrozenSource {
    name: String,
    namespace: String,
    capabilities: crate::source::SourceCapabilities,
    resources: BTreeMap<String, FrozenResource>,
}

struct FrozenResource {
    snapshot: Resource,
    after: usize,
    records: Vec<Record>,
}

impl HistorySource for FrozenSource {
    fn capabilities(&self) -> crate::source::SourceCapabilities {
        self.capabilities
    }
    fn namespace(&self) -> &str {
        &self.namespace
    }
    fn root(&self) -> &str {
        &self.name
    }

    fn list(&self) -> Result<Vec<String>, SourceError> {
        Ok(self.resources.keys().cloned().collect())
    }

    fn load(&self, id: &str) -> Result<Option<Resource>, SourceError> {
        Ok(self
            .resources
            .get(id)
            .map(|resource| resource.snapshot.clone()))
    }

    fn starting_after(&self, id: &str) -> usize {
        self.resources.get(id).map_or(0, |resource| resource.after)
    }

    fn history_after(&self, id: &str, after: usize) -> Result<Vec<Record>, SourceError> {
        Ok(self.resources.get(id).map_or_else(Vec::new, |resource| {
            resource
                .records
                .iter()
                .skip(after.saturating_sub(resource.after))
                .cloned()
                .collect()
        }))
    }
}

/// Context rows are statistics only. Current discovery citations must be delta
/// coordinates, so support participation never opens historical drill-down.
fn in_delta(manifest: &WindowManifest, key: &crate::table::Key) -> bool {
    manifest.sources.iter().any(|source| {
        source.source_instance == key.root
            && source.resources.iter().any(|p| {
                p.resource == key.resource && key.place > p.after && key.place <= p.through
            })
    })
}
