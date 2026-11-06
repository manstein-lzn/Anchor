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

use serde::Serialize;
use serde_json::json;

use super::{Analyzer, Finding, Need, Report, health::Health, stall::Stall, unmet};
use crate::query::{Invocation, Key, Outcome, Query, group_by, mann_whitney, summarize};

/// Signals kept per list unless told otherwise.
pub const LIMIT: usize = 20;
/// Evidence cited per signal, at most; the rest is counted.
const CITED: usize = 5;
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
        2
    }
    /// Grades are used when there are any: without them there is no front.
    fn needs(&self) -> &'static [Need] {
        &[Need::Observations]
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
    pub fn table(&self, query: &Query) -> Table {
        let invocations = query.invocations();
        let mut signals = merged(query);
        let front = front(query, &invocations, &mut signals);
        let mut exemplars = exemplars(query, &invocations);
        signals.sort_by(|a, b| {
            tier(b.code)
                .cmp(&tier(a.code))
                .then(b.runs.cmp(&a.runs))
                .then(b.weight.cmp(&a.weight))
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut omitted = BTreeMap::new();
        for signal in signals
            .iter()
            .skip(self.limit)
            .chain(exemplars.iter().skip(self.limit))
        {
            *omitted.entry(signal.code).or_insert(0) += 1;
        }
        signals.truncate(self.limit);
        exemplars.truncate(self.limit);
        Table {
            not_applicable: unmet(query, self.needs()),
            signals,
            exemplars,
            omitted,
            front,
        }
    }
}

/// Order of attention: what the Runs achieved, then what they wasted, then
/// how slow they were, then the integrity of the records themselves.
fn tier(code: &str) -> u8 {
    match code {
        "dominated_harness" | "check_failing" | "task_not_done" => 3,
        "tool_failures"
        | "command_failures"
        | "call_corrected"
        | "repeated_failure"
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

/// The Run a record belongs to, by the resource naming the Runtime, the
/// recorder and the grader use: `run/{run}`, `trace/{run}/…`, `grade/{run}`.
fn run_of(key: &Key) -> Option<&str> {
    let mut parts = key.resource.split('/');
    match (parts.next(), parts.next()) {
        (Some("run" | "trace" | "grade"), Some(run)) => Some(run),
        _ => None,
    }
}

/// How many distinct Runs the records belong to.
fn runs_of<'k>(keys: impl IntoIterator<Item = &'k Key>) -> usize {
    keys.into_iter()
        .filter_map(run_of)
        .collect::<BTreeSet<_>>()
        .len()
}

/// Health and stall findings, merged by kind, place and what they say.
fn merged(query: &Query) -> Vec<Signal> {
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
    let mut found: BTreeMap<(&'static str, String, String), Merged> = BTreeMap::new();
    for report in [Health.analyze(query), Stall::default().analyze(query)] {
        let source = report.analyzer;
        for finding in report.findings {
            let entry = found
                .entry((
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
    found
        .into_iter()
        .map(|((code, subject, shape), merged)| {
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
                runs: runs_of(&merged.evidence),
                coordinates,
                coordinates_total,
                evidence_total: merged.evidence.len(),
                evidence: merged.evidence.into_iter().take(CITED).collect(),
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
) -> BTreeMap<String, Vec<Harness>> {
    let by_run = group_by(invocations.iter(), |i| i.run_id.as_str());
    let mut tasks: BTreeMap<String, BTreeMap<Vec<String>, Vec<Measured>>> = BTreeMap::new();
    for (run, graded) in query.grades() {
        let Some(observed) = by_run.get(run) else {
            continue;
        };
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
            .entry(format!(
                "{} v{}",
                graded.grade.task, graded.grade.task_version
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
    let mut front = BTreeMap::new();
    for (task, harnesses) in tasks {
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
                evidence: runs.iter().map(|r| r.grade.clone()).take(CITED).collect(),
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
fn exemplars(query: &Query, invocations: &[Invocation]) -> Vec<Signal> {
    let grades = query.grades();
    let mut groups: BTreeMap<(String, String), Vec<(&Invocation, u64)>> = BTreeMap::new();
    for invocation in invocations {
        let lives = invocation.lifecycles();
        if lives.is_empty() || lives.iter().any(|l| l.outcome != Outcome::Completed) {
            continue;
        }
        let scope = match grades.get(invocation.run_id.as_str()) {
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
            .entry((scope, invocation.node_id.clone()))
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
        let subject = format!("{scope} node {node}");
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
            evidence: invocation.evidence().into_iter().take(CITED).collect(),
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
