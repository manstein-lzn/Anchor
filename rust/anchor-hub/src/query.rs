//! L2: trajectory algebra. Pure functions over projected facts, no IO: tasks
//! and their steps, invocations joined across roots by their natural ids,
//! timelines and the gaps in them, grouping, repetition and distributions.
//! Analyzers see the data only through this module.

use std::collections::BTreeMap;
use std::ops::Range;

pub use anchor_facts::{AgentConfiguration, Failure, Grade, Outcome};
pub use anchor_kernel::{Action, Effect, PrincipalKind, Reason};
use serde::Serialize;

use crate::table::Body;
pub use crate::table::{Detail, Entry, Facts, Key, Row, Status};

/// Projected facts, and the time the caller considers now.
#[derive(Clone, Copy)]
pub struct Query<'a> {
    facts: Facts<'a>,
    now: Option<u64>,
}

impl<'a> Query<'a> {
    pub fn new(facts: Facts<'a>, now: Option<u64>) -> Self {
        Self { facts, now }
    }

    pub fn now(&self) -> Option<u64> {
        self.now
    }

    pub fn rows(&self) -> &'a [Row] {
        self.facts.rows
    }

    pub fn resources(&self) -> impl Iterator<Item = &'a Entry> {
        self.facts.resources.values()
    }

    pub fn configuration(&self, digest: &str) -> Option<&'a AgentConfiguration> {
        self.facts.configurations.get(digest)
    }

    /// A content-addressed environment description in the same fact root.
    pub fn manifest(&self, digest: &str) -> Option<&'a serde_json::Value> {
        self.facts.manifests.get(digest)
    }

    /// Every Agent configuration manifest, by digest.
    pub fn configurations(&self) -> impl Iterator<Item = (&'a str, &'a AgentConfiguration)> {
        self.facts
            .configurations
            .iter()
            .map(|(digest, manifest)| (digest.as_str(), manifest))
    }

    /// Task resources, each with its records in place order.
    pub fn tasks(&self) -> Vec<Task<'a>> {
        let mut rows: BTreeMap<(&str, &str), Vec<&'a Row>> = BTreeMap::new();
        for row in self.facts.rows {
            rows.entry((&row.key.root, &row.key.resource))
                .or_default()
                .push(row);
        }
        self.facts
            .resources
            .values()
            .filter(|entry| entry.status.is_some())
            .map(|entry| Task {
                entry,
                rows: rows
                    .remove(&(entry.root.as_str(), entry.id.as_str()))
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Every invocation either side knows of: a node result in a Run's
    /// history, observations of it, or both. One side alone is a dangling
    /// edge, kept so that it can be counted.
    pub fn invocations(&self) -> Vec<Invocation<'a>> {
        let mut found: BTreeMap<(String, String, u64), Invocation<'a>> = BTreeMap::new();
        for row in self.facts.rows {
            let (run, node, invocation) = match &row.body {
                Body::Observation(observation) if row.verdict.effect == Effect::Accepted => {
                    let id = &observation.invocation;
                    (id.run_id.as_str(), id.node_id.as_str(), id.invocation)
                }
                _ if row.verdict.effect == Effect::Accepted => match node_result(row) {
                    Some(id) => id,
                    None => continue,
                },
                _ => continue,
            };
            let entry = found
                .entry((run.to_owned(), node.to_owned(), invocation))
                .or_insert_with(|| Invocation {
                    run_id: run.to_owned(),
                    node_id: node.to_owned(),
                    invocation,
                    result: None,
                    traces: Vec::new(),
                    details: self
                        .facts
                        .details
                        .get(&(run.to_owned(), node.to_owned(), invocation))
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                });
            match &row.body {
                Body::Observation(observation) => entry.traces.push(Trace { row, observation }),
                _ => {
                    entry.result.get_or_insert(row);
                }
            }
        }
        found.into_values().collect()
    }
}

impl<'a> Query<'a> {
    /// The latest accepted grade of each graded Run, by Run id.
    pub fn grades(&self) -> BTreeMap<&'a str, Graded<'a>> {
        let mut grades = BTreeMap::new();
        for row in self.facts.rows {
            if let Body::Grade(grade) = &row.body
                && row.verdict.effect == Effect::Accepted
            {
                // Rows of one history come in place order: later wins.
                grades.insert(grade.run_id.as_str(), Graded { row, grade });
            }
        }
        grades
    }
}

/// A grade and the record it came from.
#[derive(Clone, Copy)]
pub struct Graded<'a> {
    pub row: &'a Row,
    pub grade: &'a Grade,
}

/// Runs name their task `run/{run_id}`, and each node result fills the slot
/// `{node}#{invocation}`: the Runtime's naming. A record that does not follow
/// it still belongs to its task; it just joins no observation.
pub fn node_result(row: &Row) -> Option<(&str, &str, u64)> {
    let run = row.key.resource.strip_prefix("run/")?;
    let (node, invocation) = row.slot.as_deref()?.rsplit_once('#')?;
    Some((run, node, invocation.parse().ok()?))
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

pub struct Task<'a> {
    pub entry: &'a Entry,
    pub rows: Vec<&'a Row>,
}

impl Task<'_> {
    /// The last recorded moment of the task: its last stamped record, or its
    /// creation.
    pub fn last_at(&self) -> Option<u64> {
        self.rows
            .iter()
            .filter_map(|row| row.at)
            .max()
            .or(self.entry.created_at)
    }

    /// From creation to the last stamped record.
    pub fn span_ms(&self) -> Option<u64> {
        Some(self.last_at()?.saturating_sub(self.entry.created_at?))
    }

    /// Each record with the time since the task's previous record, or since
    /// its creation for the first. When steps run one after another this is
    /// how long the recorded step took, scheduling included; with parallel
    /// steps it is only an upper bound.
    pub fn steps(&self) -> Vec<(&Row, Option<u64>)> {
        let mut previous = self.entry.created_at;
        self.rows
            .iter()
            .map(|&row| {
                let since = row.at.zip(previous).map(|(at, p)| at.saturating_sub(p));
                previous = row.at.or(previous);
                (row, since)
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Invocations
// ---------------------------------------------------------------------------

pub struct Trace<'a> {
    pub row: &'a Row,
    pub observation: &'a anchor_facts::Observation,
}

pub struct Invocation<'a> {
    pub run_id: String,
    pub node_id: String,
    pub invocation: u64,
    /// The accepted node result in the Run's history.
    pub result: Option<&'a Row>,
    /// Accepted observation history: Host state edges and Goose attempts.
    pub traces: Vec<Trace<'a>>,
    /// Its tool calls at the value level, from the execution port's
    /// evidence, in call order; empty where none was found.
    pub details: &'a [Detail],
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Lifecycle {
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub outcome: Outcome,
    pub model_turns: u64,
    pub tool_calls: u64,
    pub tool_calls_dropped: u64,
    pub resumed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageCoverage {
    Complete,
    LegacyComplete,
    Partial,
    Unknown,
}

/// The reported portion remains visible even when a field or an attempt is
/// incomplete. Cost comparisons use the checked accessors, not those sums.
#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub coverage: UsageCoverage,
    pub attempts: usize,
    pub legacy_attempts: usize,
    pub unknown_attempts: usize,
    pub totals: anchor_facts::ModelUsage,
}

impl Default for Usage {
    fn default() -> Self {
        Self {
            coverage: UsageCoverage::Unknown,
            attempts: 0,
            legacy_attempts: 0,
            unknown_attempts: 0,
            totals: anchor_facts::ModelUsage {
                complete: true,
                ..Default::default()
            },
        }
    }
}

impl Usage {
    pub fn complete(&self) -> bool {
        matches!(
            self.coverage,
            UsageCoverage::Complete | UsageCoverage::LegacyComplete
        )
    }

    pub fn details_complete(&self) -> bool {
        self.complete() && self.totals.omitted_turns == 0
    }

    fn field<T: Copy>(&self, value: Option<T>, reported: u64) -> Option<T> {
        (self.complete() && reported == self.totals.messages && reported > 0)
            .then_some(value)
            .flatten()
    }

    pub fn total_tokens(&self) -> Option<u64> {
        self.field(
            self.totals.total_tokens,
            self.totals.reported_turns.total_tokens,
        )
    }

    pub fn input_tokens(&self) -> Option<u64> {
        self.field(
            self.totals.input_tokens,
            self.totals.reported_turns.input_tokens,
        )
    }

    pub fn output_tokens(&self) -> Option<u64> {
        self.field(
            self.totals.output_tokens,
            self.totals.reported_turns.output_tokens,
        )
    }

    pub fn cache_read_tokens(&self) -> Option<u64> {
        self.field(
            self.totals.cache_read_tokens,
            self.totals.reported_turns.cache_read_tokens,
        )
    }

    pub fn cost_usd(&self) -> Option<f64> {
        self.field(self.totals.cost_usd, self.totals.reported_turns.cost_usd)
    }

    pub fn elapsed_ms(&self) -> Option<u64> {
        self.field(
            self.totals.elapsed_ms,
            self.totals.reported_turns.elapsed_ms,
        )
    }

    fn add(&mut self, usage: &anchor_facts::ModelUsage) {
        self.totals.complete &= usage.complete;
        self.totals.messages += usage.messages;
        self.totals.sampled_turns += usage.sampled_turns;
        self.totals.omitted_turns += usage.omitted_turns;
        self.totals.compaction_messages += usage.compaction_messages;
        macro_rules! merge {
            ($($field:ident),+) => {$({
                if let Some(value) = usage.$field {
                    *self.totals.$field.get_or_insert(Default::default()) += value;
                }
                self.totals.reported_turns.$field += usage.reported_turns.$field;
            })+};
        }
        merge!(
            input_tokens,
            output_tokens,
            total_tokens,
            cache_read_tokens,
            cache_write_tokens,
            cost_usd,
            elapsed_ms
        );
    }

    fn refresh(&mut self) {
        self.coverage = if self.attempts == 0 || self.totals.messages == 0 {
            UsageCoverage::Unknown
        } else if self.totals.complete && self.unknown_attempts == 0 {
            if self.legacy_attempts == 0 {
                UsageCoverage::Complete
            } else {
                UsageCoverage::LegacyComplete
            }
        } else {
            UsageCoverage::Partial
        };
    }
}

/// Combine attempts without adding their retained ModelTurn samples a second
/// time. Missing observations with a positive model-request stamp stay unknown.
pub fn usage<'a>(invocations: impl IntoIterator<Item = &'a Invocation<'a>>) -> Usage {
    let mut combined = Usage::default();
    for invocation in invocations {
        let one = invocation.usage();
        combined.attempts += one.attempts;
        combined.legacy_attempts += one.legacy_attempts;
        combined.unknown_attempts += one.unknown_attempts;
        combined.add(&one.totals);
    }
    combined.refresh();
    combined
}

/// Complete lifecycle time, independent of token/detail coverage. Ordinary
/// zero-cost Ops and Host input-wait edges carry no model time to add.
pub fn agent_ms<'a>(invocations: impl IntoIterator<Item = &'a Invocation<'a>>) -> Option<u64> {
    let mut times = invocations
        .into_iter()
        .filter(|invocation| invocation.has_model_work())
        .map(Invocation::agent_ms)
        .peekable();
    times.peek()?;
    times.try_fold(0u64, |total, one| total.checked_add(one?))
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Turn<'a> {
    pub index: u64,
    pub message_id: Option<&'a str>,
    pub observed_at_ms: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub time_to_first_token_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub compaction: bool,
}

impl Turn<'_> {
    /// When the model was working on this turn, as far as the usage report
    /// alone tells: its elapsed time reaching back from when it was observed.
    /// The report can arrive late; see [`Invocation::turn_spans`].
    pub fn reported_span(&self) -> Option<Range<u64>> {
        let end = self.observed_at_ms?;
        Some(end.saturating_sub(self.elapsed_ms?)..end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Call<'a> {
    pub tool: &'a str,
    pub tool_call_id: Option<&'a str>,
    pub model_request: u64,
    pub arguments_shape: &'a str,
    /// The family a `path` argument belonged to, when the tool took one.
    pub path: Option<anchor_facts::PathClass>,
    /// The program a command tool was asked to run.
    pub program: Option<&'a str>,
    /// How that command ended; `None` when unrecorded or not a command.
    pub command: Option<anchor_facts::CommandEnd>,
    pub started_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    pub ok: bool,
    pub failure: Option<Failure>,
}

impl Call<'_> {
    pub fn span(&self) -> Option<Range<u64>> {
        Some(self.started_at_ms?..self.ended_at_ms?.max(self.started_at_ms?))
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.span().map(|span| span.end - span.start)
    }

    /// Whether the call did what was asked: the tool accepted it and, for a
    /// command, the command ran and exited 0. A command whose end was not
    /// recorded counts as done, as it did before ends were recorded.
    pub fn done(&self) -> bool {
        self.ok && self.command.is_none_or(|end| end.clean())
    }
}

/// What occupied a stretch of an invocation's time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "activity", rename_all = "snake_case")]
pub enum Activity {
    /// Waiting for model turn `index`.
    Turn { index: u64 },
    /// Tool call number `index` (0-based, in recorded order).
    Tool { index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Span {
    pub start: u64,
    pub end: u64,
    pub activity: Activity,
}

/// A stretch of an invocation in which neither a model turn nor a tool call
/// was observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Gap {
    pub start: u64,
    pub end: u64,
    /// The activity that ended just before the gap, if any.
    pub after: Option<Activity>,
    /// The activity that started right after it, if any.
    pub before: Option<Activity>,
}

impl Gap {
    pub fn ms(&self) -> u64 {
        self.end - self.start
    }

    /// Where in the invocation the gap lies.
    pub fn place(&self) -> GapPlace {
        match (self.after, self.before) {
            (None, _) => GapPlace::Startup,
            (_, None) => GapPlace::Teardown,
            (Some(Activity::Tool { .. }), Some(Activity::Turn { .. })) => GapPlace::ToolToTurn,
            (Some(Activity::Turn { .. }), Some(Activity::Tool { .. })) => GapPlace::TurnToTool,
            _ => GapPlace::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapPlace {
    /// From the invocation's start to its first observed activity.
    Startup,
    /// From a tool result to the next model request.
    ToolToTurn,
    /// From a model response to the tool call it asked for.
    TurnToTool,
    /// From the last observed activity to the invocation's end.
    Teardown,
    Other,
}

/// The total length covered by `spans`, counting overlaps once.
pub fn covered(spans: impl IntoIterator<Item = Range<u64>>) -> u64 {
    let mut spans: Vec<Range<u64>> = spans.into_iter().collect();
    spans.sort_by_key(|s| s.start);
    let (mut total, mut cursor) = (0, 0);
    for span in spans {
        let start = span.start.max(cursor);
        if span.end > start {
            total += span.end - start;
            cursor = span.end;
        }
    }
    total
}

impl<'a> Invocation<'a> {
    pub fn key(&self) -> String {
        format!("{}/{}#{}", self.run_id, self.node_id, self.invocation)
    }

    pub fn waiting_for_input(&self) -> bool {
        self.result.is_none()
            && self
                .facts()
                .filter_map(|fact| match fact {
                    anchor_facts::Fact::InputWait { waiting } => Some(*waiting),
                    _ => None,
                })
                .last()
                == Some(true)
    }

    pub fn has_model_observations(&self) -> bool {
        self.facts().any(model_observation)
    }

    pub fn has_model_work(&self) -> bool {
        self.has_model_observations() || self.result.is_some_and(|row| row.cost > 0)
    }

    pub fn observed_agent_ms(&self) -> u64 {
        self.lifecycles()
            .iter()
            .map(|life| life.ended_at_ms.saturating_sub(life.started_at_ms))
            .sum()
    }

    pub fn agent_ms(&self) -> Option<u64> {
        if self.usage().unknown_attempts > 0 {
            return None;
        }
        let mut attempts = self.model_traces().peekable();
        attempts.peek()?;
        attempts
            .all(|trace| {
                trace
                    .observation
                    .facts
                    .iter()
                    .any(|fact| matches!(fact, anchor_facts::Fact::Lifecycle { .. }))
            })
            .then(|| self.observed_agent_ms())
    }

    pub fn timeline_complete(&self) -> bool {
        self.usage().details_complete()
            && self
                .turns()
                .iter()
                .all(|turn| turn.observed_at_ms.is_some() && turn.elapsed_ms.is_some())
            && self
                .calls()
                .iter()
                .all(|call| call.started_at_ms.is_some() && call.ended_at_ms.is_some())
            && self
                .lifecycles()
                .iter()
                .all(|life| life.tool_calls_dropped == 0)
    }

    pub fn usage(&self) -> Usage {
        let mut usage = Usage::default();
        for trace in self.model_traces() {
            if usage.attempts == 0
                && trace
                    .observation
                    .facts
                    .iter()
                    .any(|fact| matches!(fact, anchor_facts::Fact::Lifecycle { resumed: true, .. }))
            {
                // A retained session resumed work whose prior attempt was not
                // recorded. Session accumulated counters cannot fill that gap.
                usage.unknown_attempts += 1;
            }
            usage.attempts += 1;
            if let Some(total) = trace.observation.facts.iter().find_map(|fact| match fact {
                anchor_facts::Fact::ModelUsage { usage } => Some(usage),
                _ => None,
            }) {
                usage.add(total);
            } else {
                usage.add(&legacy_usage(trace));
                usage.legacy_attempts += 1;
            }
        }
        if usage.attempts == 0 && self.result.is_some_and(|row| row.cost > 0) {
            usage.unknown_attempts = 1;
        }
        usage.refresh();
        usage
    }

    fn model_traces(&self) -> impl Iterator<Item = &Trace<'a>> {
        self.traces
            .iter()
            .filter(|trace| trace.observation.facts.iter().any(model_observation))
    }

    fn facts(&self) -> impl Iterator<Item = &'a anchor_facts::Fact> + '_ {
        self.traces.iter().flat_map(|t| t.observation.facts.iter())
    }

    /// The configuration the observations were recorded under.
    pub fn config(&self) -> Option<&'a str> {
        self.traces.iter().find_map(|t| t.row.config.as_deref())
    }

    /// Every record this invocation is known by.
    pub fn evidence(&self) -> Vec<Key> {
        self.result
            .into_iter()
            .chain(self.traces.iter().map(|t| t.row))
            .map(|row| row.key.clone())
            .collect()
    }

    pub fn lifecycles(&self) -> Vec<Lifecycle> {
        self.facts()
            .filter_map(|fact| match *fact {
                anchor_facts::Fact::Lifecycle {
                    started_at_ms,
                    ended_at_ms,
                    outcome,
                    model_turns,
                    tool_calls,
                    tool_calls_dropped,
                    resumed,
                } => Some(Lifecycle {
                    started_at_ms,
                    ended_at_ms,
                    outcome,
                    model_turns,
                    tool_calls,
                    tool_calls_dropped,
                    resumed,
                }),
                _ => None,
            })
            .collect()
    }

    pub fn turns(&self) -> Vec<Turn<'a>> {
        self.facts()
            .filter_map(|fact| match fact {
                anchor_facts::Fact::ModelTurn {
                    index,
                    message_id,
                    observed_at_ms,
                    elapsed_ms,
                    time_to_first_token_ms,
                    input_tokens,
                    output_tokens,
                    total_tokens,
                    cache_read_tokens,
                    compaction,
                    ..
                } => Some(Turn {
                    index: *index,
                    message_id: message_id.as_deref(),
                    observed_at_ms: *observed_at_ms,
                    elapsed_ms: *elapsed_ms,
                    time_to_first_token_ms: *time_to_first_token_ms,
                    input_tokens: *input_tokens,
                    output_tokens: *output_tokens,
                    total_tokens: *total_tokens,
                    cache_read_tokens: *cache_read_tokens,
                    compaction: *compaction,
                }),
                _ => None,
            })
            .collect()
    }

    pub fn calls(&self) -> Vec<Call<'a>> {
        self.facts()
            .filter_map(|fact| match fact {
                anchor_facts::Fact::ToolCall {
                    tool,
                    tool_call_id,
                    model_request,
                    arguments_shape,
                    path,
                    program,
                    command,
                    started_at_ms,
                    ended_at_ms,
                    ok,
                    failure,
                    ..
                } => Some(Call {
                    tool,
                    tool_call_id: tool_call_id.as_deref(),
                    model_request: *model_request,
                    arguments_shape,
                    path: *path,
                    program: program.as_deref(),
                    command: *command,
                    started_at_ms: *started_at_ms,
                    ended_at_ms: *ended_at_ms,
                    ok: *ok,
                    failure: *failure,
                }),
                _ => None,
            })
            .collect::<Vec<_>>()
            .into_iter()
            .enumerate()
            .map(|(index, mut call)| {
                // Records from before commands had ends: the evidence has
                // them, when it is there and lines up with the facts.
                if let Some(detail) = self.details.get(index)
                    && detail.tool == call.tool
                {
                    call.program = call.program.or(detail.program.as_deref());
                    call.command = call.command.or(detail.command);
                }
                call
            })
            .collect()
    }

    /// The tool calls with their details, where both are known.
    pub fn detailed(&self) -> Vec<(usize, Call<'a>, Option<&'a Detail>)> {
        self.calls()
            .into_iter()
            .enumerate()
            .map(|(index, call)| {
                let detail = self.details.get(index).filter(|d| d.tool == call.tool);
                (index, call, detail)
            })
            .collect()
    }

    /// The tools called, in recorded order, consecutive calls of one tool
    /// counted together: the route the model took.
    pub fn route(&self) -> Vec<(&'a str, usize)> {
        let mut route: Vec<(&'a str, usize)> = Vec::new();
        for call in self.calls() {
            match route.last_mut() {
                Some((tool, n)) if *tool == call.tool => *n += 1,
                _ => route.push((call.tool, 1)),
            }
        }
        route
    }

    /// When the model worked on each turn, by turn index. A turn's usage
    /// report may only be observed after the tool calls it asked for have
    /// run (Goose reports usage once the turn is processed), so a turn ends
    /// no later than the first call answering it starts, and its elapsed
    /// time reaches back from there.
    pub fn turn_spans(&self) -> Vec<(u64, Range<u64>)> {
        self.traces
            .iter()
            .flat_map(|trace| {
                trace.observation.facts.iter().filter_map(|fact| {
                    let anchor_facts::Fact::ModelTurn {
                        index,
                        observed_at_ms: Some(observed),
                        elapsed_ms: Some(elapsed),
                        ..
                    } = fact
                    else {
                        return None;
                    };
                    let first_call = trace
                        .observation
                        .facts
                        .iter()
                        .filter_map(|fact| match fact {
                            anchor_facts::Fact::ToolCall {
                                model_request,
                                started_at_ms,
                                ..
                            } if model_request == index => *started_at_ms,
                            _ => None,
                        })
                        .min();
                    let end = first_call.map_or(*observed, |start| start.min(*observed));
                    Some((*index, end.saturating_sub(*elapsed)..end))
                })
            })
            .collect()
    }

    /// Model turns and tool calls with known times, by start.
    pub fn timeline(&self) -> Vec<Span> {
        let mut spans: Vec<Span> = self
            .turn_spans()
            .into_iter()
            .map(|(index, span)| Span {
                start: span.start,
                end: span.end,
                activity: Activity::Turn { index },
            })
            .chain(self.calls().iter().enumerate().filter_map(|(index, call)| {
                let span = call.span()?;
                Some(Span {
                    start: span.start,
                    end: span.end,
                    activity: Activity::Tool { index },
                })
            }))
            .collect();
        spans.sort_by_key(|span| (span.start, span.end));
        spans
    }

    /// The stretches between lifecycle start and end that no observed turn
    /// or call covers. Each lifecycle (a resume starts a new one) is its own
    /// window; activity outside every window is ignored.
    pub fn gaps(&self) -> Vec<Gap> {
        let timeline = self.timeline();
        let mut gaps = Vec::new();
        for life in self.lifecycles() {
            let mut cursor = life.started_at_ms;
            let mut after = None;
            for span in timeline
                .iter()
                .filter(|s| s.end > life.started_at_ms && s.start < life.ended_at_ms)
            {
                if span.start > cursor {
                    gaps.push(Gap {
                        start: cursor,
                        end: span.start,
                        after,
                        before: Some(span.activity),
                    });
                }
                if span.end >= cursor {
                    cursor = span.end;
                    after = Some(span.activity);
                }
            }
            if life.ended_at_ms > cursor {
                gaps.push(Gap {
                    start: cursor,
                    end: life.ended_at_ms,
                    after,
                    before: None,
                });
            }
        }
        gaps
    }
}

fn model_observation(fact: &anchor_facts::Fact) -> bool {
    matches!(
        fact,
        anchor_facts::Fact::Lifecycle { .. }
            | anchor_facts::Fact::ModelUsage { .. }
            | anchor_facts::Fact::ModelTurn { .. }
            | anchor_facts::Fact::ToolCall { .. }
    )
}

// Remove this fallback when retained histories all carry ModelUsage. Old
// observations are not rewritten: equality only proves detail-count coverage;
// optional cache, time and cost fields keep their own reporting counts.
fn legacy_usage(trace: &Trace<'_>) -> anchor_facts::ModelUsage {
    use anchor_facts::{Fact, ModelUsage};
    let mut usage = ModelUsage::default();
    let mut lives = Vec::new();
    for fact in &trace.observation.facts {
        match fact {
            Fact::Lifecycle {
                model_turns,
                outcome,
                ..
            } => lives.push((*model_turns, *outcome)),
            Fact::ModelTurn {
                input_tokens,
                output_tokens,
                total_tokens,
                cache_read_tokens,
                cache_write_tokens,
                cost_usd,
                elapsed_ms,
                compaction,
                ..
            } => {
                usage.sampled_turns += 1;
                usage.compaction_messages += u64::from(*compaction);
                macro_rules! add {
                    ($field:ident, $value:expr) => {
                        if let Some(value) = $value {
                            *usage.$field.get_or_insert(Default::default()) += value;
                            usage.reported_turns.$field += 1;
                        }
                    };
                }
                add!(input_tokens, *input_tokens);
                add!(output_tokens, *output_tokens);
                add!(total_tokens, *total_tokens);
                add!(cache_read_tokens, *cache_read_tokens);
                add!(cache_write_tokens, *cache_write_tokens);
                add!(cost_usd, *cost_usd);
                add!(elapsed_ms, *elapsed_ms);
            }
            _ => {}
        }
    }
    usage.messages = lives
        .iter()
        .map(|(messages, _)| *messages)
        .sum::<u64>()
        .max(usage.sampled_turns);
    usage.omitted_turns = usage.messages.saturating_sub(usage.sampled_turns);
    usage.complete =
        lives.as_slice() == [(usage.sampled_turns, Outcome::Completed)] && usage.sampled_turns > 0;
    usage
}

// ---------------------------------------------------------------------------
// Generic operators
// ---------------------------------------------------------------------------

pub fn group_by<T, K: Ord>(
    items: impl IntoIterator<Item = T>,
    key: impl Fn(&T) -> K,
) -> BTreeMap<K, Vec<T>> {
    let mut groups: BTreeMap<K, Vec<T>> = BTreeMap::new();
    for item in items {
        groups.entry(key(&item)).or_default().push(item);
    }
    groups
}

/// Maximal stretches of at least `min` consecutive items with equal keys.
pub fn repeats<T, K: PartialEq>(
    items: &[T],
    min: usize,
    key: impl Fn(&T) -> K,
) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut start = 0;
    for end in 1..=items.len() {
        if end == items.len() || key(&items[end]) != key(&items[start]) {
            if end - start >= min.max(1) {
                found.push(start..end);
            }
            start = end;
        }
    }
    found
}

/// A distribution, by nearest rank.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Summary {
    pub n: usize,
    pub min: u64,
    pub median: u64,
    pub p90: u64,
    pub max: u64,
    pub mean: f64,
    /// Sample standard deviation; 0 for a single value.
    pub sd: f64,
}

pub fn summarize(values: impl IntoIterator<Item = u64>) -> Option<Summary> {
    let mut values: Vec<u64> = values.into_iter().collect();
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let mean = values.iter().map(|&v| v as f64).sum::<f64>() / values.len() as f64;
    let rank =
        |q: f64| values[((q * values.len() as f64).ceil() as usize).clamp(1, values.len()) - 1];
    Some(Summary {
        n: values.len(),
        min: values[0],
        median: rank(0.5),
        p90: rank(0.9),
        max: values[values.len() - 1],
        mean,
        sd: if values.len() < 2 {
            0.0
        } else {
            (values
                .iter()
                .map(|&v| (v as f64 - mean).powi(2))
                .sum::<f64>()
                / (values.len() - 1) as f64)
                .sqrt()
        },
    })
}

// ---------------------------------------------------------------------------
// Tests between samples
// ---------------------------------------------------------------------------

/// A two-sided Mann-Whitney U test of whether two samples come from the same
/// distribution, by the normal approximation with a tie correction. `None`
/// when either sample is empty.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RankTest {
    /// U of the first sample.
    pub u: f64,
    pub z: f64,
    pub p: f64,
}

pub fn mann_whitney(a: &[f64], b: &[f64]) -> Option<RankTest> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let mut all: Vec<(f64, bool)> = a
        .iter()
        .map(|&v| (v, true))
        .chain(b.iter().map(|&v| (v, false)))
        .collect();
    all.sort_by(|x, y| x.0.total_cmp(&y.0));
    let (n1, n2) = (a.len() as f64, b.len() as f64);
    let n = n1 + n2;
    let (mut rank_a, mut ties, mut i) = (0.0, 0.0, 0);
    while i < all.len() {
        let mut j = i;
        while j + 1 < all.len() && all[j + 1].0 == all[i].0 {
            j += 1;
        }
        // Ranks i+1 ..= j+1 share their mean.
        let rank = (i + j + 2) as f64 / 2.0;
        let t = (j - i + 1) as f64;
        ties += t * t * t - t;
        rank_a += rank * all[i..=j].iter().filter(|(_, first)| *first).count() as f64;
        i = j + 1;
    }
    let u = rank_a - n1 * (n1 + 1.0) / 2.0;
    let variance = n1 * n2 / 12.0 * ((n + 1.0) - ties / (n * (n - 1.0)).max(1.0));
    if variance <= 0.0 {
        return Some(RankTest { u, z: 0.0, p: 1.0 });
    }
    let z = (u - n1 * n2 / 2.0) / variance.sqrt();
    Some(RankTest {
        u,
        z,
        p: (2.0 * (1.0 - normal_cdf(z.abs()))).clamp(0.0, 1.0),
    })
}

/// Evidence against "the mean of the pushed values is at most zero", as the
/// wealth of a gambler who bets on each value before seeing it (testing by
/// betting, with the aGRAPA bet of Waudby-Smith and Ramdas). Values must not
/// fall below `floor` (negative). Under that hypothesis the wealth is a
/// nonnegative supermartingale, so the chance that it *ever* reaches 1/α is
/// at most α: it may be read after every value and the reading stopped at
/// any time, which a p-value may not.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wealth {
    floor: f64,
    value: f64,
    n: f64,
    sum: f64,
    squares: f64,
}

impl Wealth {
    pub fn new(floor: f64) -> Self {
        assert!(floor < 0.0, "a floor below zero");
        Self {
            floor,
            value: 1.0,
            n: 0.0,
            sum: 0.0,
            squares: 0.0,
        }
    }

    /// Bet on `x` with what the earlier values suggest, then see it.
    pub fn push(&mut self, x: f64) {
        let x = x.max(self.floor);
        let mean = self.sum / (self.n + 1.0);
        let variance = (self.floor * self.floor / 4.0 + self.squares) / (self.n + 1.0);
        // Stake at most three quarters of what the floor would take: wealth
        // can shrink, never vanish.
        let bet = (mean / (variance + mean * mean)).clamp(0.0, 0.75 / -self.floor);
        self.value *= 1.0 + bet * x;
        self.squares += (x - mean).powi(2);
        self.sum += x;
        self.n += 1.0;
    }

    /// The e-value so far.
    pub fn value(&self) -> f64 {
        self.value
    }

    /// How many values were pushed.
    pub fn n(&self) -> usize {
        self.n as usize
    }
}

/// Which of several e-values may be rejected together while the expected
/// share of false rejections stays at most `alpha` (e-BH, Wang and Ramdas),
/// whatever the dependence between them. Indices into `evalues`.
pub fn e_bh(evalues: &[f64], alpha: f64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..evalues.len()).collect();
    order.sort_by(|&a, &b| evalues[b].total_cmp(&evalues[a]));
    let k = evalues.len() as f64;
    let kept = (1..=order.len())
        .rev()
        .find(|&rank| evalues[order[rank - 1]] >= k / (alpha * rank as f64))
        .unwrap_or(0);
    let mut chosen = order[..kept].to_vec();
    chosen.sort();
    chosen
}

/// The standard normal distribution function (Abramowitz and Stegun 7.1.26,
/// absolute error below 1.5e-7).
pub fn normal_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs() / std::f64::consts::SQRT_2);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-(x * x) / 2.0).exp();
    if x >= 0.0 {
        0.5 * (1.0 + erf)
    } else {
        0.5 * (1.0 - erf)
    }
}

/// A two-sided test of whether two proportions differ, by the pooled normal
/// approximation. `None` when either has no trials.
pub fn two_proportions(hits_a: u64, n_a: u64, hits_b: u64, n_b: u64) -> Option<f64> {
    if n_a == 0 || n_b == 0 {
        return None;
    }
    let (pa, pb) = (hits_a as f64 / n_a as f64, hits_b as f64 / n_b as f64);
    let pooled = (hits_a + hits_b) as f64 / (n_a + n_b) as f64;
    let se = (pooled * (1.0 - pooled) * (1.0 / n_a as f64 + 1.0 / n_b as f64)).sqrt();
    if se == 0.0 {
        return Some(1.0);
    }
    Some((2.0 * (1.0 - normal_cdf(((pa - pb) / se).abs()))).clamp(0.0, 1.0))
}
