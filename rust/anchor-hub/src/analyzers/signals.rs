//! Signals: what the other analyzers found and where harnesses stand against
//! each other, reduced to a short ranked table. The hub does the work that
//! grows with the records; whoever reads the table spends attention only on
//! the signals in it, however many Graphs, Plugins and Runs there are.
//!
//! - Findings of one kind at one place are merged: a hundred idle gaps of one
//!   node are one signal, not a hundred.
//! - Each signal has an id derived from its kind and place, so a later round
//!   can tell whether the signal it acted on went away.
//! - Each signal carries the coordinates of the records it rests on in the
//!   harness space: configuration, Graph, node, model, tools, Host.
//! - Per graded task, the harnesses that no other harness beats on quality,
//!   tokens and agent time at once form the front; the beaten ones are
//!   signals.
//! - Exemplars are the positive outliers: completed invocations far cheaper
//!   than their peers, or taking a route their peers do not.
//! - The table is bounded; what did not fit is counted by kind, never
//!   dropped silently.

use std::collections::{BTreeMap, BTreeSet};

use anchor_facts::window::MAX_SIGNAL_CITATIONS as CITED;
use serde::Serialize;
use serde_json::json;

use super::{Analyzer, Finding, Need, Report, health::Health, stall::Stall, unmet};
use crate::query::{Invocation, Key, Outcome, Query, group_by, mann_whitney, summarize};

/// Signals kept per list unless told otherwise.
pub const LIMIT: usize = 20;
/// Graded Runs a harness needs on a task before it is placed on the front.
const MIN_RUNS: usize = 3;
/// Completed peers an invocation needs before it can stand out among them.
const PEERS: usize = 5;
/// Below this p-value a difference in tokens or agent time counts.
const SIGNIFICANT: f64 = 0.05;
/// A mean score higher by more than this counts.
const SCORE_MARGIN: f64 = 0.05;

pub struct Signals {
    /// Signals and exemplars kept, each.
    pub limit: usize,
}

impl Default for Signals {
    fn default() -> Self {
        Self { limit: LIMIT }
    }
}

/// Where a configuration sits in the harness space. Digests other than the
/// configuration's own are shortened; the configuration digest is kept whole
/// so that its manifest can be looked up.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Coordinate {
    pub config: String,
    pub graph: Option<String>,
    pub node: Option<String>,
    pub model: Option<String>,
    pub tools: Option<String>,
    pub envelope: Option<String>,
    pub host: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Signal {
    /// The same kind at the same place keeps its id across rounds.
    pub id: String,
    pub code: &'static str,
    /// The analyzer the signal came from.
    pub source: &'static str,
    pub subject: String,
    pub detail: String,
    /// How many occurrences, Runs or peers it rests on.
    pub weight: usize,
    /// How many distinct Runs it touches: the reach of the problem, which
    /// ranks signals of one tier. Occurrences pile up in one Run; Runs do
    /// not.
    pub runs: usize,
    pub coordinates: Vec<Coordinate>,
    /// The configurations it rests on, listed or not.
    pub coordinates_total: usize,
    pub evidence: Vec<Key>,
    /// The records it rests on, cited or not.
    pub evidence_total: usize,
}

/// A harness on a task's front.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Harness {
    /// The configurations its Runs' AgentNodes ran under.
    pub configs: Vec<String>,
    pub runs: usize,
    pub mean_score: f64,
    pub mean_tokens: f64,
    pub mean_agent_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Table {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitudinal: Option<anchor_facts::longitudinal::Longitudinal>,
    pub applicability: anchor_facts::window::Applicability,
    pub not_applicable: Vec<Need>,
    pub signals: Vec<Signal>,
    pub exemplars: Vec<Signal>,
    /// Signals and exemplars that did not fit, by code.
    pub omitted: BTreeMap<&'static str, usize>,
    /// Per graded task, the harnesses no other harness beats.
    pub front: BTreeMap<String, Vec<Harness>>,
}

impl Analyzer for Signals {
    fn id(&self) -> &'static str {
        "signals"
    }
    fn version(&self) -> u32 {
        anchor_facts::window::ANALYZER_VERSION
    }
    /// Grades are used when there are any: without them there is no front.
    fn needs(&self) -> &'static [Need] {
        &[Need::Observations, Need::Grades, Need::Runs]
    }

    fn analyze(&self, query: &Query) -> Report {
        let table = self.table(query);
        let mut report = Report {
            analyzer: self.id(),
            version: self.version(),
            not_applicable: table.not_applicable,
            summary: BTreeMap::new(),
            findings: Vec::new(),
        };
        report
            .summary
            .insert("applicability", json!(table.applicability));
        report.summary.insert("front", json!(table.front));
        report.summary.insert("omitted", json!(table.omitted));
        report.summary.insert(
            "signals",
            json!(
                table
                    .signals
                    .iter()
                    .chain(&table.exemplars)
                    .map(|s| json!({"id": s.id, "code": s.code, "subject": s.subject, "weight": s.weight, "runs": s.runs, "coordinates": s.coordinates}))
                    .collect::<Vec<_>>()
            ),
        );
        report.findings = table
            .signals
            .into_iter()
            .chain(table.exemplars)
            .map(|s| Finding {
                code: s.code,
                subject: s.subject,
                detail: format!("[{}] {}", s.id, s.detail),
                evidence: s.evidence,
            })
            .collect();
        report
    }
}

impl Signals {
    /// Frozen discovery reserves exploration space for unclassified behavior.
    /// Sampling uses identities, not success, quality, cost or failure labels.
    pub fn window_table(&self, query: &Query) -> Table {
        self.window_table_in(query, |_| true)
    }
    pub fn window_table_in(&self, query: &Query, allowed: impl Fn(&Key) -> bool) -> Table {
        self.window_table_version(query, allowed, self.version())
    }

    pub(crate) fn window_table_version(
        &self,
        query: &Query,
        allowed: impl Fn(&Key) -> bool,
        version: u32,
    ) -> Table {
        // Delete this branch after retained v4 windows and continuations retire.
        // Their persisted offsets must retain cap-before-filter ordering.
        let legacy = version < anchor_facts::window::ANALYZER_VERSION;
        let mut table = if legacy {
            let mut table = Signals { limit: usize::MAX }.table_in(query, &|_| true, false);
            retain_citations(&mut table, &allowed);
            table.bound(self.limit);
            table
        } else {
            self.table_in(query, &allowed, true)
        };
        let mut ordinary = query
            .invocations()
            .into_iter()
            .filter(|invocation| {
                !matches!(
                    invocation.provenance,
                    anchor_facts::ProvenanceCategory::Experiment
                        | anchor_facts::ProvenanceCategory::DreamingMeta
                ) && invocation.evidence().iter().any(&allowed)
            })
            .map(|invocation| {
                let identity = invocation.qualified_key();
                (
                    anchor_facts::text_digest(&format!(
                        "ordinary_invocation_sample_v1\n{identity}"
                    )),
                    invocation,
                )
            })
            .collect::<Vec<_>>();
        ordinary.sort_by(|(left, _), (right, _)| left.cmp(right));
        let eligible = ordinary.len();
        let sample_limit = self.limit.div_ceil(2).min(eligible);
        let positive_limit = self.limit.saturating_sub(sample_limit);
        for signal in table.exemplars.iter().skip(positive_limit) {
            *table.omitted.entry(signal.code).or_default() += 1;
        }
        table.exemplars.truncate(positive_limit);
        if eligible > sample_limit {
            table
                .omitted
                .insert("ordinary_sample", eligible - sample_limit);
        }
        table.exemplars.extend(ordinary.into_iter().take(sample_limit).enumerate().map(|(rank, (digest, invocation))| {
            let evidence = invocation.evidence();
            Signal {
                id: id("ordinary_sample", &invocation.qualified_key()), code: "ordinary_sample", source: "stable_sampler_v1",
                subject: invocation.key(),
                detail: format!("Neutral ordinary sample; no quality or defect assertion. Rule: sha256(ordinary_invocation_sample_v1 + invocation identity), ascending; rank {} of {eligible}; identity digest {digest}. Frozen window coordinates define the eligible population.", rank + 1),
                weight: eligible, runs: 1,
                coordinates: situate(query, invocation.config().map(str::to_owned)).0,
                coordinates_total: usize::from(invocation.config().is_some()),
                evidence_total: evidence.len(), evidence: evidence.into_iter().filter(|key| legacy || allowed(key)).take(CITED).collect(),
            }
        }));
        if legacy {
            retain_citations(&mut table, &allowed);
        }
        table
    }

    pub fn table(&self, query: &Query) -> Table {
        self.table_in(query, &|_| true, true)
    }

    fn table_in(
        &self,
        query: &Query,
        allowed: &impl Fn(&Key) -> bool,
        complete_support: bool,
    ) -> Table {
        let invocations = query.invocations();
        let mut signals = merged(query, allowed, complete_support);
        let observed: BTreeSet<_> = invocations
            .iter()
            .filter(|i| i.has_model_observations())
            .map(|i| i.run_key())
            .collect();
        for task in query.tasks() {
            let Some(run) = task.entry.id.strip_prefix("run/") else {
                continue;
            };
            if matches!(
                task.provenance,
                anchor_facts::ProvenanceCategory::Experiment
                    | anchor_facts::ProvenanceCategory::DreamingMeta
            ) || observed.contains(&(task.namespace, run))
            {
                continue;
            }
            let evidence: Vec<_> = task
                .rows
                .iter()
                .filter(|r| r.slot.is_some() || r.status.is_some_and(|s| s.is_settled()))
                .map(|r| r.key.clone())
                .collect();
            if evidence.is_empty() {
                continue;
            }
            signals.push(Signal{id:id("runs_without_observations",&format!("{}::{run}",task.namespace)),code:"runs_without_observations",source:"signals",subject:format!("{}::run/{run}",task.namespace),detail:"Accepted Run outcome or node slots have no model observations; observed work denominator is incomplete".into(),weight:1,runs:1,coordinates:vec![],coordinates_total:0,evidence_total:evidence.len(),evidence:evidence.into_iter().filter(allowed).take(CITED).collect()});
        }
        let front = front(query, &invocations, &mut signals, allowed);
        let mut exemplars = exemplars(query, &invocations, allowed);
        // Historical rows support statistics, but cannot consume current citation slots.
        signals.retain(|signal| !signal.evidence.is_empty());
        exemplars.retain(|signal| !signal.evidence.is_empty());
        signals.sort_by(|a, b| {
            tier(b.code)
                .cmp(&tier(a.code))
                .then(b.runs.cmp(&a.runs))
                .then(b.weight.cmp(&a.weight))
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut table = Table {
            longitudinal: None,
            applicability: applicability(query, &invocations),
            not_applicable: unmet(query, self.needs()),
            signals,
            exemplars,
            omitted: BTreeMap::new(),
            front,
        };
        table.bound(self.limit);
        table
    }
}

impl Table {
    fn bound(&mut self, limit: usize) {
        for signal in self
            .signals
            .iter()
            .skip(limit)
            .chain(self.exemplars.iter().skip(limit))
        {
            *self.omitted.entry(signal.code).or_default() += 1;
        }
        self.signals.truncate(limit);
        self.exemplars.truncate(limit);
    }
}

fn retain_citations(table: &mut Table, allowed: &impl Fn(&Key) -> bool) {
    for list in [&mut table.signals, &mut table.exemplars] {
        for signal in list.iter_mut() {
            signal.evidence.retain(allowed);
        }
        list.retain(|signal| !signal.evidence.is_empty());
    }
}

/// Order of attention: what the Runs achieved, then what they wasted, then
/// how slow they were, then the integrity of the records themselves.
fn tier(code: &str) -> u8 {
    match code {
        "dominated_harness" | "check_failing" | "task_not_done" => 3,
        "tool_failures"
        | "command_failures"
        | "same_tool_after_failure"
        | "consecutive_same_shape_failures"
        | "invocation_not_completed"
        | "startup_heavy"
        | "not_accepted"
        | "idle_task" => 2,
        "idle_gap" | "slow_tool_call" | "slow_model_turn" | "parked_task" | "command_nonzero" => 1,
        _ => 0,
    }
}

fn id(code: &str, subject: &str) -> String {
    anchor_facts::text_digest(&format!("{code}\n{subject}"))[..12].to_owned()
}

/// Digests are long; twelve characters tell them apart.
fn short(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

/// The place a finding is about, at the grain signals are kept: an
/// invocation stands for its node, a task for all tasks of its kind.
fn place(subject: &str) -> String {
    if subject.starts_with("run/") {
        return "run/*".into();
    }
    if let Some((head, number)) = subject.rsplit_once('#')
        && number.parse::<u64>().is_ok()
        && let Some((_, node)) = head.rsplit_once('/')
    {
        return format!("node {node}");
    }
    subject.to_owned()
}

/// The coordinates of up to [`CITED`] configurations, and how many there
/// are.
fn situate(query: &Query, configs: impl IntoIterator<Item = String>) -> (Vec<Coordinate>, usize) {
    let configs: BTreeSet<String> = configs.into_iter().collect();
    let total = configs.len();
    let listed = configs
        .into_iter()
        .take(CITED)
        .map(|digest| {
            let manifest = query.configuration(&digest);
            Coordinate {
                graph: manifest.map(|m| short(&m.graph_digest).to_owned()),
                node: manifest.map(|m| m.node_id.clone()),
                model: manifest.map(|m| m.model.clone()),
                tools: manifest.map(|m| short(&m.tools_sha256).to_owned()),
                envelope: manifest.map(|m| short(&m.envelope_sha256).to_owned()),
                host: manifest.map(|m| short(&m.host_sha256).to_owned()),
                config: digest,
            }
        })
        .collect();
    (listed, total)
}

/// A route as text: `anchor_read×3 → anchor_edit → final_result`.
pub fn route_label(route: &[(&str, usize)]) -> String {
    route
        .iter()
        .map(|(tool, n)| match n {
            1 => (*tool).to_owned(),
            n => format!("{tool}×{n}"),
        })
        .collect::<Vec<_>>()
        .join(" → ")
}

/// A finding's detail with its counts and times taken out: what it says,
/// rather than how much of it there was. Two findings of one kind at one
/// place that say the same thing are the same signal; one that fails for
/// another class than its neighbour is not.
fn shape(detail: &str) -> String {
    let mut shaped = String::with_capacity(detail.len());
    let mut number = false;
    for letter in detail.chars() {
        if letter.is_ascii_digit() {
            number = true;
        } else {
            if number {
                shaped.push('N');
                number = false;
            }
            shaped.push(letter);
        }
    }
    if number {
        shaped.push('N');
    }
    shaped
}

/// How many distinct Runs the records belong to.
fn runs_of<'k>(query: &Query, keys: impl IntoIterator<Item = &'k Key>) -> usize {
    keys.into_iter()
        .filter_map(|key| {
            Some((
                query.namespace(&key.root)?,
                anchor_facts::window::resource_run(&key.resource)?,
            ))
        })
        .collect::<BTreeSet<_>>()
        .len()
}

/// Health and stall findings, merged by kind, place and what they say.
fn merged(query: &Query, allowed: &impl Fn(&Key) -> bool, complete_support: bool) -> Vec<Signal> {
    struct Merged {
        source: &'static str,
        count: usize,
        detail: String,
        evidence: BTreeSet<Key>,
    }
    let config_of: BTreeMap<&Key, &str> = query
        .rows()
        .iter()
        .filter_map(|row| Some((&row.key, row.config.as_deref()?)))
        .collect();
    let mut found: BTreeMap<
        (
            anchor_facts::ProvenanceCategory,
            &'static str,
            String,
            String,
        ),
        Merged,
    > = BTreeMap::new();
    for provenance in [
        anchor_facts::ProvenanceCategory::Production,
        anchor_facts::ProvenanceCategory::Unknown,
    ] {
        for report in query.with_provenance(provenance, |q| {
            [
                if complete_support {
                    Health.analyze_for_signals(q)
                } else {
                    Health.analyze(q)
                },
                Stall::default().analyze(q),
            ]
        }) {
            let source = report.analyzer;
            for finding in report.findings {
                if finding.evidence.is_empty() {
                    continue;
                }
                let entry = found
                    .entry((
                        provenance,
                        finding.code,
                        place(&finding.subject),
                        shape(&finding.detail),
                    ))
                    .or_insert_with(|| Merged {
                        source,
                        count: 0,
                        detail: finding.detail.clone(),
                        evidence: BTreeSet::new(),
                    });
                entry.count += 1;
                entry.evidence.extend(finding.evidence);
            }
        }
    }
    found
        .into_iter()
        .map(|((provenance, code, subject, shape), merged)| {
            let subject = if provenance == anchor_facts::ProvenanceCategory::Unknown {
                subject
            } else {
                format!("production:{subject}")
            };
            let configs = merged
                .evidence
                .iter()
                .filter_map(|key| config_of.get(key).map(|c| (*c).to_owned()));
            let (coordinates, coordinates_total) = situate(query, configs);
            Signal {
                id: id(&format!("{code}\n{shape}"), &subject),
                code,
                source: merged.source,
                detail: match merged.count {
                    1 => merged.detail,
                    n => format!("{n} occurrences, for example: {}", merged.detail),
                },
                weight: merged.count.max(merged.evidence.len()),
                runs: runs_of(query, &merged.evidence),
                coordinates,
                coordinates_total,
                evidence_total: merged.evidence.len(),
                evidence: merged
                    .evidence
                    .into_iter()
                    .filter(allowed)
                    .take(CITED)
                    .collect(),
                subject,
            }
        })
        .collect()
}

/// What one graded Run scored and cost.
struct Measured {
    score: f64,
    tokens: f64,
    agent_ms: f64,
    grade: Key,
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    if n == 0 { 0.0 } else { sum / n as f64 }
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Whether harness `b` beats harness `a`: no worse on mean score, tokens and
/// agent time, and better on one of them by more than noise.
fn dominates(b: &[Measured], a: &[Measured]) -> bool {
    let axis =
        |runs: &[Measured], f: fn(&Measured) -> f64| -> Vec<f64> { runs.iter().map(f).collect() };
    let (score_a, score_b) = (
        mean(a.iter().map(|r| r.score)),
        mean(b.iter().map(|r| r.score)),
    );
    let (tokens_a, tokens_b) = (axis(a, |r| r.tokens), axis(b, |r| r.tokens));
    let (ms_a, ms_b) = (axis(a, |r| r.agent_ms), axis(b, |r| r.agent_ms));
    let lower = |x: &[f64], y: &[f64]| mean(y.iter().copied()) < mean(x.iter().copied());
    let not_higher = |x: &[f64], y: &[f64]| mean(y.iter().copied()) <= mean(x.iter().copied());
    let p = |x: &[f64], y: &[f64]| mann_whitney(x, y).map_or(1.0, |t| t.p);
    score_b >= score_a
        && not_higher(&tokens_a, &tokens_b)
        && not_higher(&ms_a, &ms_b)
        && (score_b - score_a > SCORE_MARGIN
            || (lower(&tokens_a, &tokens_b) && p(&tokens_a, &tokens_b) < SIGNIFICANT)
            || (lower(&ms_a, &ms_b) && p(&ms_a, &ms_b) < SIGNIFICANT))
}

fn label(configs: &[String]) -> String {
    configs
        .iter()
        .map(|c| short(c))
        .collect::<Vec<_>>()
        .join("+")
}

/// Per graded task: Runs grouped by the configurations of their observed
/// invocations; the harnesses with enough Runs that no other beats form the
/// front, and each beaten one becomes a signal.
fn front(
    query: &Query,
    invocations: &[Invocation],
    signals: &mut Vec<Signal>,
    allowed: &impl Fn(&Key) -> bool,
) -> BTreeMap<String, Vec<Harness>> {
    let by_run = group_by(invocations.iter(), |i| i.run_key());
    let provenances = query.provenances();
    type TaskIdentity = (
        anchor_facts::ProvenanceCategory,
        String,
        u32,
        Option<String>,
    );
    let mut tasks: BTreeMap<TaskIdentity, BTreeMap<Vec<String>, Vec<Measured>>> = BTreeMap::new();
    for (run, graded) in query.grades() {
        let provenance = provenances.get(&run).copied().unwrap_or_default();
        if matches!(
            provenance,
            anchor_facts::ProvenanceCategory::Experiment
                | anchor_facts::ProvenanceCategory::DreamingMeta
        ) {
            continue;
        }
        let Some(observed) = by_run.get(&run) else {
            continue;
        };
        if observed.iter().any(|invocation| {
            invocation.graph_conflict()
                || (invocation.has_model_observations()
                    && invocation
                        .config()
                        .and_then(|digest| query.configuration(digest))
                        .is_none_or(|config| !config.is_stable()))
        }) {
            continue;
        }
        let Some(tokens) = crate::query::usage(observed.iter().copied()).total_tokens() else {
            continue;
        };
        let Some(agent_ms) = crate::query::agent_ms(observed.iter().copied()) else {
            continue;
        };
        let configs: BTreeSet<String> = observed
            .iter()
            .filter_map(|i| i.config())
            .map(str::to_owned)
            .collect();
        if configs.is_empty() {
            continue;
        }
        tasks
            .entry((
                provenance,
                graded.grade.task.clone(),
                graded.grade.task_version,
                graded.row.config.clone(),
            ))
            .or_default()
            .entry(configs.into_iter().collect())
            .or_default()
            .push(Measured {
                score: graded.grade.score(),
                tokens: tokens as f64,
                agent_ms: agent_ms as f64,
                grade: graded.row.key.clone(),
            });
    }
    let mut semantics: BTreeMap<_, usize> = BTreeMap::new();
    for (provenance, task, version, _) in tasks.keys() {
        *semantics
            .entry((*provenance, task.clone(), *version))
            .or_default() += 1;
    }
    let mut front = BTreeMap::new();
    for ((provenance, task, version, grader), harnesses) in tasks {
        let task = if provenance == anchor_facts::ProvenanceCategory::Unknown
            && semantics[&(provenance, task.clone(), version)] == 1
        {
            format!("{task} v{version}")
        } else {
            format!(
                "{}:{task} v{version} [grader:{}]",
                if provenance == anchor_facts::ProvenanceCategory::Production {
                    "production"
                } else {
                    "unknown"
                },
                grader.as_deref().unwrap_or("unknown")
            )
        };
        let placed: Vec<(Vec<String>, Vec<Measured>)> = harnesses
            .into_iter()
            .filter(|(_, runs)| runs.len() >= MIN_RUNS)
            .collect();
        let mut kept = Vec::new();
        for (index, (configs, runs)) in placed.iter().enumerate() {
            let beaten = placed
                .iter()
                .enumerate()
                .find(|(other, (_, rivals))| *other != index && dominates(rivals, runs));
            let Some((_, (by, rivals))) = beaten else {
                kept.push(Harness {
                    configs: configs.clone(),
                    runs: runs.len(),
                    mean_score: round(mean(runs.iter().map(|r| r.score))),
                    mean_tokens: mean(runs.iter().map(|r| r.tokens)).round(),
                    mean_agent_ms: mean(runs.iter().map(|r| r.agent_ms)).round(),
                });
                continue;
            };
            let ratio = |f: fn(&Measured) -> f64| {
                mean(rivals.iter().map(f)) / mean(runs.iter().map(f)).max(1.0) * 100.0
            };
            let subject = format!("task {task}: harness {}", label(configs));
            let (coordinates, coordinates_total) = situate(query, configs.iter().cloned());
            signals.push(Signal {
                id: id("dominated_harness", &subject),
                code: "dominated_harness",
                source: "signals",
                detail: format!(
                    "beaten by harness {}: mean score {:.3} against {:.3}, {:.0}% of the tokens and {:.0}% of the agent time, over {} and {} graded Runs",
                    label(by),
                    mean(rivals.iter().map(|r| r.score)),
                    mean(runs.iter().map(|r| r.score)),
                    ratio(|r| r.tokens),
                    ratio(|r| r.agent_ms),
                    rivals.len(),
                    runs.len()
                ),
                weight: runs.len(),
                runs: runs.len(),
                coordinates,
                coordinates_total,
                evidence: runs.iter().map(|r| r.grade.clone()).filter(allowed).take(CITED).collect(),
                evidence_total: runs.len(),
                subject,
            });
        }
        front.insert(task, kept);
    }
    front
}

/// Completed invocations of one node that stand out among their peers: in
/// one graded task among Runs that passed, or in one Graph when ungraded.
fn exemplars(
    query: &Query,
    invocations: &[Invocation],
    allowed: &impl Fn(&Key) -> bool,
) -> Vec<Signal> {
    let grades = query.grades();
    let mut groups: BTreeMap<(String, String), Vec<(&Invocation, u64)>> = BTreeMap::new();
    for invocation in invocations {
        if matches!(
            invocation.provenance,
            anchor_facts::ProvenanceCategory::Experiment
                | anchor_facts::ProvenanceCategory::DreamingMeta
        ) {
            continue;
        }
        let lives = invocation.lifecycles();
        if lives.is_empty() || lives.iter().any(|l| l.outcome != Outcome::Completed) {
            continue;
        }
        let scope = match grades.get(&invocation.run_key()) {
            Some(graded) if !graded.grade.passed() => continue,
            Some(graded) => format!("task {} v{}", graded.grade.task, graded.grade.task_version),
            None => format!(
                "graph {}",
                invocation
                    .config()
                    .and_then(|digest| query.configuration(digest))
                    .map_or("?", |m| short(&m.graph_digest))
            ),
        };
        let Some(tokens) = invocation.usage().total_tokens() else {
            continue;
        };
        groups
            .entry((
                format!(
                    "{:?}:{scope}:{}",
                    invocation.provenance,
                    grades
                        .get(&invocation.run_key())
                        .and_then(|g| g.row.config.as_deref())
                        .unwrap_or("ungraded")
                ),
                invocation.node_id.clone(),
            ))
            .or_default()
            .push((invocation, tokens));
    }
    let mut found: Vec<(f64, Signal)> = Vec::new();
    for ((scope, node), peers) in groups {
        if peers.len() < PEERS {
            continue;
        }
        let Some(stats) = summarize(peers.iter().map(|(_, tokens)| *tokens)) else {
            continue;
        };
        let median = stats.median.max(1);
        let display_scope = scope
            .strip_prefix("Unknown:")
            .and_then(|s| s.strip_suffix(":ungraded"))
            .unwrap_or(&scope);
        let subject = format!("{display_scope} node {node}");
        let exemplar = |code: &'static str, invocation: &Invocation, detail: String| Signal {
            id: id(code, &subject),
            code,
            source: "signals",
            subject: subject.clone(),
            detail,
            weight: peers.len(),
            runs: 1,
            coordinates: situate(query, invocation.config().map(str::to_owned)).0,
            coordinates_total: usize::from(invocation.config().is_some()),
            evidence: invocation
                .evidence()
                .into_iter()
                .filter(allowed)
                .take(CITED)
                .collect(),
            evidence_total: invocation.evidence().len(),
        };
        let &(cheapest, tokens) = peers
            .iter()
            .min_by_key(|(i, tokens)| (*tokens, i.key()))
            .expect("enough peers");
        if tokens * 2 <= median {
            found.push((
                tokens as f64 / median as f64,
                exemplar(
                    "frugal_route",
                    cheapest,
                    format!(
                        "{} took {tokens} tokens against a median of {median} over {} completed peers; route: {}",
                        cheapest.key(),
                        peers.len(),
                        route_label(&cheapest.route())
                    ),
                ),
            ));
        }
        // A route nobody else took, beside one that most peers take, at no
        // more than the usual cost.
        let signature = |i: &Invocation| {
            i.route()
                .iter()
                .map(|(tool, _)| *tool)
                .collect::<Vec<_>>()
                .join(" → ")
        };
        let routes = group_by(peers.iter(), |(i, _)| signature(i));
        let common = routes.values().map(Vec::len).max().unwrap_or(0);
        if common * 2 < peers.len() {
            continue;
        }
        let rare = routes
            .values()
            .filter(|takers| takers.len() == 1)
            .map(|takers| *takers[0])
            .filter(|(_, tokens)| *tokens <= median)
            .filter(|(i, _)| i.key() != cheapest.key() || tokens * 2 > median)
            .min_by_key(|(i, tokens)| (*tokens, i.key()));
        if let Some((rare, rare_tokens)) = rare {
            found.push((
                rare_tokens as f64 / median as f64,
                exemplar(
                    "rare_route",
                    rare,
                    format!(
                        "{} is the only one of {} completed peers to take this route ({rare_tokens} tokens, median {median}): {}",
                        rare.key(),
                        peers.len(),
                        route_label(&rare.route())
                    ),
                ),
            ));
        }
    }
    found.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    found.into_iter().map(|(_, signal)| signal).collect()
}

/// Capability availability is independent of whether efficiency signals exist.
pub fn applicability(
    query: &Query,
    invocations: &[Invocation],
) -> anchor_facts::window::Applicability {
    use anchor_facts::window::{Applicability, CapabilityStatus as Status};
    let tasks = query.tasks();
    let runs: BTreeSet<_> = tasks
        .iter()
        .filter_map(|task| {
            task.entry
                .id
                .strip_prefix("run/")
                .map(|run| (task.namespace, run))
        })
        .collect();
    let observed = group_by(
        invocations
            .iter()
            .filter(|invocation| invocation.has_model_observations()),
        |invocation| invocation.run_key(),
    );
    let measured = group_by(
        invocations
            .iter()
            .filter(|invocation| invocation.has_model_work()),
        |invocation| invocation.run_key(),
    );
    let grades = query.grades();
    let grade_conflicts = query.grade_conflicts().len();
    let graph_conflicts = invocations
        .iter()
        .filter(|invocation| invocation.graph_conflict())
        .count();
    let legacy_v1_configurations = query
        .configurations()
        .filter(|(_, config)| config.version == anchor_facts::LEGACY_CONFIGURATION_VERSION)
        .count();
    let missing_observations = runs
        .iter()
        .filter(|run| !observed.contains_key(*run))
        .count();
    let missing_runs = observed.keys().filter(|run| !runs.contains(*run)).count();
    let population: BTreeSet<_> = runs
        .iter()
        .copied()
        .chain(observed.keys().copied())
        .collect();
    let graded_runs = population
        .iter()
        .filter(|run| grades.contains_key(*run))
        .count();
    let ungraded = population.len().saturating_sub(graded_runs);
    let status = |available: bool, empty: bool, partial: bool| {
        if !available {
            Status::NotAuthorizedOrMissingSource
        } else if empty {
            Status::EmptyInScope
        } else if partial {
            Status::PartialCoverage
        } else {
            Status::Analyzable
        }
    };
    Applicability {
        run_lifecycle: status(
            query.has_run_source(),
            runs.is_empty(),
            missing_observations > 0,
        ),
        observations: if graph_conflicts > 0 {
            Status::Conflict
        } else {
            status(
                query.has_observation_source(),
                observed.is_empty(),
                query.has_run_source() && (missing_runs > 0 || missing_observations > 0),
            )
        },
        grades: if grade_conflicts > 0 {
            Status::Conflict
        } else if !query.has_grade_source() {
            Status::NotAuthorizedOrMissingSource
        } else if grades.is_empty() {
            Status::EmptyInScope
        } else if graded_runs == 0 {
            Status::NoMatchingGrades
        } else if ungraded > 0 {
            Status::PartialCoverage
        } else {
            Status::Analyzable
        },
        stable_configurations: if graph_conflicts > 0 {
            Status::Conflict
        } else {
            status(
                query.configurations().next().is_some(),
                observed.is_empty(),
                legacy_v1_configurations > 0
                    || invocations.iter().any(|invocation| {
                        invocation.has_model_observations()
                            && invocation
                                .config()
                                .and_then(|digest| query.configuration(digest))
                                .is_none_or(|configuration| !configuration.is_stable())
                    }),
            )
        },
        runs_without_observations: query.has_run_source().then_some(missing_observations),
        observations_without_runs: query.has_run_source().then_some(missing_runs),
        ungraded_runs: (query.has_run_source() || !population.is_empty()).then_some(ungraded),
        graded_runs,
        complete_token_runs: measured
            .values()
            .filter(|invocations| {
                crate::query::usage(invocations.iter().copied())
                    .total_tokens()
                    .is_some()
            })
            .count(),
        complete_cost_runs: measured
            .values()
            .filter(|invocations| {
                crate::query::usage(invocations.iter().copied())
                    .cost_usd()
                    .is_some()
            })
            .count(),
        legacy_v1_configurations,
        grade_conflicts,
        graph_conflicts,
    }
}
