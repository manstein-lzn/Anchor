//! Profile: where the cost of a task goes, for the reader deciding what to
//! change in a harness. Per graded task, or per Graph and node when nothing
//! is graded: how cost spreads across Runs of the same task, what drives it
//! (turns, context per turn, output), how agent time splits between the model
//! and the tools, and what separates the cheapest Runs from the dearest. The
//! numbers are the measurement; the analyzer judges nothing and finds
//! nothing.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{Analyzer, Need, Report, signals::route_label, unmet};
use crate::query::{Invocation, Query, Usage, agent_ms, group_by, summarize, usage};

pub struct Profile;

impl Analyzer for Profile {
    fn id(&self) -> &'static str {
        "profile"
    }
    fn version(&self) -> u32 {
        2
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Observations]
    }

    fn analyze(&self, query: &Query) -> Report {
        let mut report = Report {
            analyzer: self.id(),
            version: self.version(),
            not_applicable: unmet(query, self.needs()),
            summary: BTreeMap::new(),
            findings: Vec::new(),
        };
        if report.not_applicable.contains(&Need::Observations) {
            return report;
        }
        let invocations = query.invocations();
        let grades = query.grades();
        let by_run = group_by(invocations.iter().filter(|i| i.has_model_work()), |i| {
            i.run_id.as_str()
        });
        // A Run's scope: its graded task, or else the Graph and node of its
        // invocations.
        let mut scopes: BTreeMap<String, Vec<Measured>> = BTreeMap::new();
        for (run, observed) in &by_run {
            let scope = match grades.get(run) {
                Some(g) => format!("task {} v{}", g.grade.task, g.grade.task_version),
                None => format!(
                    "graph {}",
                    observed
                        .iter()
                        .find_map(|i| i.config())
                        .and_then(|d| query.configuration(d))
                        .map_or("?", |m| m.graph_digest.get(..12).unwrap_or("?"))
                ),
            };
            scopes.entry(scope).or_default().push(measure(
                run,
                observed,
                grades.get(run).map(|g| g.grade.score()),
            ));
        }
        let mut tasks = BTreeMap::new();
        let mut totals = (0u64, 0u64);
        let mut counted = (0usize, 0usize);
        for (scope, mut runs) in scopes {
            runs.sort_by_key(|r| (r.tokens.is_none(), r.tokens, r.run.clone()));
            totals.0 += runs.iter().filter_map(|r| r.tokens).sum::<u64>();
            totals.1 += excess(&runs);
            counted.0 += runs.iter().filter(|r| r.tokens.is_some()).count();
            counted.1 += runs.len();
            tasks.insert(scope, profile(&runs));
        }
        report.summary.insert("scopes", json!(tasks));
        report.summary.insert(
            "all",
            json!({
                "tokens": (counted.0 == counted.1 && counted.0 > 0).then_some(totals.0),
                "known_tokens": totals.0,
                "runs_with_complete_tokens": counted.0,
                "runs_with_incomplete_tokens": counted.1 - counted.0,
                // Tokens saved if every Run above its scope's median had cost
                // the median: an upper bound on what stopping earlier buys.
                "tokens_above_median": (counted.0 > 0).then_some(totals.1),
                "above_median_share": share(totals.1, totals.0),
            }),
        );
        report
    }
}

/// What one Run cost and how.
struct Measured {
    run: String,
    score: Option<f64>,
    tokens: Option<u64>,
    output_tokens: Option<u64>,
    turns: u64,
    calls: u64,
    agent_ms: Option<u64>,
    observed_agent_ms: u64,
    model_ms: Option<u64>,
    tool_ms: u64,
    first_input: Option<u64>,
    last_input: Option<u64>,
    compactions: u64,
    route: String,
    usage: Usage,
}

fn measure(run: &str, observed: &[&Invocation], score: Option<f64>) -> Measured {
    let turns: Vec<_> = observed.iter().flat_map(|i| i.turns()).collect();
    let calls: Vec<_> = observed.iter().flat_map(|i| i.calls()).collect();
    let usage = usage(observed.iter().copied());
    let context_complete = usage.details_complete() && usage.input_tokens().is_some();
    Measured {
        run: run.to_owned(),
        score,
        tokens: usage.total_tokens(),
        output_tokens: usage.output_tokens(),
        turns: usage.totals.messages,
        calls: calls.len() as u64,
        agent_ms: agent_ms(observed.iter().copied()),
        observed_agent_ms: observed.iter().map(|i| i.observed_agent_ms()).sum(),
        model_ms: usage.elapsed_ms(),
        tool_ms: observed
            .iter()
            .map(|i| crate::query::covered(i.calls().iter().filter_map(crate::query::Call::span)))
            .sum(),
        first_input: context_complete
            .then(|| turns.first().and_then(|t| t.input_tokens))
            .flatten(),
        last_input: context_complete
            .then(|| turns.last().and_then(|t| t.input_tokens))
            .flatten(),
        compactions: usage.totals.compaction_messages,
        route: observed
            .iter()
            .map(|i| route_label(&i.route()))
            .collect::<Vec<_>>()
            .join(" | "),
        usage,
    }
}

fn share(part: u64, whole: u64) -> Value {
    if whole == 0 {
        Value::Null
    } else {
        json!((part as f64 / whole as f64 * 1000.0).round() / 1000.0)
    }
}

/// Tokens above the median, summed over the Runs above it.
fn excess(runs: &[Measured]) -> u64 {
    let Some(stats) = summarize(runs.iter().filter_map(|r| r.tokens)) else {
        return 0;
    };
    runs.iter()
        .filter_map(|r| r.tokens.map(|t| t.saturating_sub(stats.median)))
        .sum()
}

/// Pearson correlation; `None` when either side does not vary.
fn correlation(pairs: &[(f64, f64)]) -> Option<f64> {
    let n = pairs.len() as f64;
    if pairs.len() < 3 {
        return None;
    }
    let (mx, my) = (
        pairs.iter().map(|p| p.0).sum::<f64>() / n,
        pairs.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in pairs {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx).powi(2);
        syy += (y - my).powi(2);
    }
    (sxx > 0.0 && syy > 0.0).then(|| (sxy / (sxx * syy).sqrt() * 1000.0).round() / 1000.0)
}

/// One Run in a line, for the cheapest and dearest of a scope.
fn line(run: &Measured) -> Value {
    json!({
        "run": run.run,
        "score": run.score,
        "tokens": run.tokens,
        "turns": run.turns,
        "calls": run.calls,
        "agent_ms": run.agent_ms,
        "observed_agent_ms": run.observed_agent_ms,
        "route": run.route,
        "usage": run.usage,
    })
}

/// `runs` sorted by tokens.
fn profile(runs: &[Measured]) -> Value {
    let comparable: Vec<_> = runs.iter().filter(|r| r.tokens.is_some()).collect();
    let tokens: u64 = comparable.iter().filter_map(|r| r.tokens).sum();
    let agent_ms: Option<u64> = runs.iter().map(|r| r.agent_ms).sum();
    let stats = summarize(runs.iter().filter_map(|r| r.tokens));
    let scored: Vec<f64> = runs.iter().filter_map(|r| r.score).collect();
    let growth: Vec<f64> = runs
        .iter()
        .filter_map(|r| Some(r.last_input? as f64 / r.first_input?.max(1) as f64))
        .collect();
    json!({
        "runs": runs.len(),
        "runs_with_complete_tokens": comparable.len(),
        "runs_with_incomplete_tokens": runs.len() - comparable.len(),
        "mean_score": (!scored.is_empty()).then(|| (scored.iter().sum::<f64>() / scored.len() as f64 * 1000.0).round() / 1000.0),
        "tokens": stats,
        "spread": stats.map(|s| (s.max as f64 / s.min.max(1) as f64 * 10.0).round() / 10.0),
        "turns": summarize(runs.iter().map(|r| r.turns)),
        "calls": summarize(runs.iter().map(|r| r.calls)),
        "agent_ms": summarize(runs.iter().filter_map(|r| r.agent_ms)),
        "observed_agent_ms": summarize(runs.iter().map(|r| r.observed_agent_ms)),
        "runs_with_incomplete_agent_time": runs.iter().filter(|r| r.agent_ms.is_none()).count(),
        // What cost moves with across the scope's Runs.
        "turns_tokens_correlation": correlation(&runs.iter().filter_map(|r| Some((r.turns as f64, r.tokens? as f64))).collect::<Vec<_>>()),
        "score_tokens_correlation": correlation(&runs.iter().filter_map(|r| Some((r.score?, r.tokens? as f64))).collect::<Vec<_>>()),
        // How much of the context each turn re-reads: the last turn's input
        // over the first's.
        "context_growth": summarize(growth.iter().map(|g| (g * 10.0).round() as u64)).map(|s| json!({"median": s.median as f64 / 10.0, "max": s.max as f64 / 10.0})),
        "output_share": comparable.iter().map(|r| r.output_tokens).collect::<Option<Vec<_>>>().map(|values| share(values.iter().sum(), tokens)),
        "model_share": runs.iter().map(|r| r.model_ms).sum::<Option<u64>>().zip(agent_ms).map(|(model, agent)| share(model, agent)),
        "tool_share": agent_ms.map(|agent| share(runs.iter().map(|r| r.tool_ms).sum(), agent)),
        "compactions": runs.iter().map(|r| r.compactions).sum::<u64>(),
        "tokens_above_median": excess(runs),
        "cheapest": comparable.first().map(|r| line(r)),
        "dearest": comparable.last().map(|r| line(r)),
        "incomplete": runs.iter().filter(|r| r.tokens.is_none()).map(line).collect::<Vec<_>>(),
    })
}
