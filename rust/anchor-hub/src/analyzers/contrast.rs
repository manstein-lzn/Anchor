//! Contrast: graded Runs of the same task under different harnesses, side by
//! side. Runs fall into arms by one field of their AgentNode configuration
//! manifest (by default the Host executable digest); arms are compared on
//! quality first, then on what the Runs cost. This is the read side of a
//! controlled experiment: quality is the constraint, efficiency the goal.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::{Analyzer, Finding, Need, Report, unmet};
use crate::query::{Invocation, Key, Query, mann_whitney, summarize, two_proportions};

/// A drop of mean score larger than this is reported as a quality loss.
const QUALITY_TOLERANCE: f64 = 0.05;
/// Below this p-value a difference is reported.
const SIGNIFICANT: f64 = 0.05;

pub struct Contrast {
    /// The configuration manifest field that names a Run's arm.
    pub by: String,
    /// The arm the others are compared against, by a prefix of its value;
    /// the arm first seen when not given.
    pub reference: Option<String>,
}

impl Default for Contrast {
    fn default() -> Self {
        Self {
            by: "host_sha256".into(),
            reference: None,
        }
    }
}

/// What one graded Run cost and scored.
pub(super) struct Measured {
    pub(super) first_at: u64,
    pub(super) score: f64,
    pub(super) agent_ms: Option<f64>,
    observed_agent_ms: u64,
    pub(super) tokens: Option<f64>,
    pub(super) turns: f64,
    pub(super) calls: u64,
    pub(super) failed_calls: u64,
    pub(super) grade: Key,
}

impl Analyzer for Contrast {
    fn id(&self) -> &'static str {
        "contrast"
    }
    fn version(&self) -> u32 {
        2
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Time, Need::Observations, Need::Grades]
    }

    fn analyze(&self, query: &Query) -> Report {
        let mut report = Report {
            analyzer: self.id(),
            version: self.version(),
            not_applicable: unmet(query, self.needs()),
            summary: BTreeMap::new(),
            findings: Vec::new(),
        };
        if !report.not_applicable.is_empty() {
            return report;
        }
        report.summary.insert("by", json!(self.by));
        let (tasks, unassigned) = self.runs(query);
        report
            .summary
            .insert("runs_without_one_arm", json!(unassigned));
        // The reference arm is the one named, or else the one first seen.
        let mut first_seen: BTreeMap<&str, u64> = BTreeMap::new();
        for arms in tasks.values() {
            for (arm, runs) in arms {
                let first = runs.iter().map(|r| r.first_at).min().unwrap_or(u64::MAX);
                let entry = first_seen.entry(arm.as_str()).or_insert(first);
                *entry = (*entry).min(first);
            }
        }
        let mut order: Vec<&str> = first_seen.keys().copied().collect();
        order.sort_by_key(|arm| (first_seen[arm], *arm));
        if let Some(prefix) = &self.reference {
            let Some(at) = order
                .iter()
                .position(|arm| arm.starts_with(prefix.as_str()))
            else {
                report.summary.insert("reference_missing", json!(prefix));
                return report;
            };
            let named = order.remove(at);
            order.insert(0, named);
        }
        report.summary.insert("arms", json!(order));
        let Some(&reference) = order.first() else {
            return report;
        };

        let mut per_task = BTreeMap::new();
        // Each Run's cost relative to its task's reference mean, pooled.
        let mut pooled: BTreeMap<&str, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
        let mut failures: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for (task, arms) in &tasks {
            let mut row = BTreeMap::new();
            for (arm, runs) in arms {
                let entry = failures.entry(arm.as_str()).or_default();
                entry.0 += runs.iter().map(|r| r.failed_calls).sum::<u64>();
                entry.1 += runs.iter().map(|r| r.calls).sum::<u64>();
                row.insert(arm.clone(), describe(runs));
            }
            let Some(base) = arms.get(reference) else {
                per_task.insert(task.clone(), json!({"arms": row}));
                continue;
            };
            let base_ms = mean_present(base.iter().filter_map(|r| r.agent_ms));
            let base_tokens = mean_present(base.iter().filter_map(|r| r.tokens));
            let mut against = BTreeMap::new();
            for (arm, runs) in arms {
                let normalized = pooled.entry(arm.as_str()).or_default();
                if let Some(base_ms) = base_ms {
                    normalized.0.extend(
                        runs.iter()
                            .filter_map(|r| r.agent_ms.map(|ms| ms / base_ms.max(1.0))),
                    );
                }
                if let Some(base_tokens) = base_tokens {
                    normalized.1.extend(
                        runs.iter()
                            .filter_map(|r| r.tokens.map(|tokens| tokens / base_tokens.max(1.0))),
                    );
                }
                if arm == reference {
                    continue;
                }
                let score_change =
                    mean(runs.iter().map(|r| r.score)) - mean(base.iter().map(|r| r.score));
                if score_change < -QUALITY_TOLERANCE {
                    report.findings.push(Finding {
                        code: "contrast_quality_loss",
                        subject: format!("{task}: {}", short(arm)),
                        detail: format!(
                            "mean score {:+.3} against {}",
                            score_change,
                            short(reference)
                        ),
                        evidence: runs.iter().map(|r| r.grade.clone()).take(50).collect(),
                    });
                }
                against.insert(
                    arm.clone(),
                    json!({
                        "score_change": round(score_change),
                        "agent_ms_ratio": mean_present(runs.iter().filter_map(|r| r.agent_ms)).zip(base_ms).map(|(ours, base)| round(ours / base.max(1.0))),
                        "agent_ms_p": test(base, runs, |r| r.agent_ms),
                        "tokens_ratio": mean_present(runs.iter().filter_map(|r| r.tokens)).zip(base_tokens).map(|(ours, base)| round(ours / base.max(1.0))),
                        "tokens_p": test(base, runs, |r| r.tokens),
                        "turns_ratio": round(mean(runs.iter().map(|r| r.turns)) / mean(base.iter().map(|r| r.turns)).max(1.0)),
                    }),
                );
            }
            per_task.insert(
                task.clone(),
                json!({"arms": row, "against_reference": against}),
            );
        }
        report.summary.insert("tasks", json!(per_task));

        let mut overall = BTreeMap::new();
        let (base_ms, base_tokens) = pooled.get(reference).cloned().unwrap_or_default();
        let base_failures = failures.get(reference).copied().unwrap_or_default();
        for arm in &order {
            let (ms, tokens) = pooled.get(arm).cloned().unwrap_or_default();
            let (failed, calls) = failures.get(arm).copied().unwrap_or_default();
            let runs: Vec<_> = tasks
                .values()
                .filter_map(|arms| arms.get(*arm))
                .flatten()
                .collect();
            let mut line = json!({
                "runs": runs.len(),
                "agent_ms_relative": mean_present(ms.iter().copied()).map(round),
                "runs_with_complete_agent_time": runs.iter().filter(|r| r.agent_ms.is_some()).count(),
                "tokens_relative": mean_present(tokens.iter().copied()).map(round),
                "runs_with_complete_tokens": runs.iter().filter(|r| r.tokens.is_some()).count(),
                "tool_calls": calls,
                "tool_failure_rate": round(failed as f64 / calls.max(1) as f64),
            });
            if *arm != reference {
                let ms_test = mann_whitney(&base_ms, &ms);
                let tokens_test = mann_whitney(&base_tokens, &tokens);
                let failure_p = two_proportions(base_failures.0, base_failures.1, failed, calls);
                line["agent_ms_p"] = json!(ms_test.map(|t| round(t.p)));
                line["tokens_p"] = json!(tokens_test.map(|t| round(t.p)));
                line["tool_failure_p"] = json!(failure_p.map(round));
                for (what, p, value, samples) in [
                    (
                        "agent time",
                        ms_test.map(|t| t.p),
                        mean(ms.iter().copied()),
                        ms.len(),
                    ),
                    (
                        "tokens",
                        tokens_test.map(|t| t.p),
                        mean(tokens.iter().copied()),
                        tokens.len(),
                    ),
                ] {
                    if p.is_some_and(|p| p < SIGNIFICANT) {
                        report.findings.push(Finding {
                            code: "contrast_efficiency",
                            subject: short(arm).to_owned(),
                            detail: format!(
                                "{what} {:.1}% of {} over {} graded Runs (p={:.3})",
                                value * 100.0,
                                short(reference),
                                samples,
                                p.unwrap_or(1.0)
                            ),
                            evidence: Vec::new(),
                        });
                    }
                }
                if failure_p.is_some_and(|p| p < SIGNIFICANT) {
                    report.findings.push(Finding {
                        code: "contrast_tool_failures",
                        subject: short(arm).to_owned(),
                        detail: format!(
                            "tool failure rate {:.3} against {:.3} for {} (p={:.4})",
                            failed as f64 / calls.max(1) as f64,
                            base_failures.0 as f64 / base_failures.1.max(1) as f64,
                            short(reference),
                            failure_p.unwrap_or(1.0)
                        ),
                        evidence: Vec::new(),
                    });
                }
            }
            overall.insert(arm.to_string(), line);
        }
        report.summary.insert("overall", json!(overall));
        report
    }
}

impl Contrast {
    /// Graded Runs by task and arm, and how many Runs had no single arm.
    pub(super) fn runs(
        &self,
        query: &Query,
    ) -> (BTreeMap<String, BTreeMap<String, Vec<Measured>>>, usize) {
        let invocations = query.invocations();
        let by_run = crate::query::group_by(&invocations, |i| i.run_id.as_str());
        let mut tasks: BTreeMap<String, BTreeMap<String, Vec<Measured>>> = BTreeMap::new();
        let mut unassigned = 0;
        for (run, graded) in query.grades() {
            let observed: Vec<&&Invocation> = by_run
                .get(run)
                .map(|v| v.iter().filter(|i| i.has_model_observations()).collect())
                .unwrap_or_default();
            let arms: BTreeSet<String> = observed
                .iter()
                .map(|i| self.arm(query, i.config()))
                .collect();
            let (Some(arm), 1) = (arms.first(), arms.len()) else {
                unassigned += 1;
                continue;
            };
            let lifecycles: Vec<_> = observed.iter().flat_map(|i| i.lifecycles()).collect();
            let calls: Vec<_> = observed.iter().flat_map(|i| i.calls()).collect();
            let usage =
                crate::query::usage(by_run.get(run).into_iter().flat_map(|v| v.iter().copied()));
            tasks
                .entry(format!(
                    "{} v{}",
                    graded.grade.task, graded.grade.task_version
                ))
                .or_default()
                .entry(arm.clone())
                .or_default()
                .push(Measured {
                    first_at: lifecycles
                        .iter()
                        .map(|l| l.started_at_ms)
                        .min()
                        .unwrap_or(0),
                    score: graded.grade.score(),
                    agent_ms: crate::query::agent_ms(
                        by_run.get(run).into_iter().flat_map(|v| v.iter().copied()),
                    )
                    .map(|ms| ms as f64),
                    observed_agent_ms: observed.iter().map(|i| i.observed_agent_ms()).sum(),
                    tokens: usage.total_tokens().map(|tokens| tokens as f64),
                    turns: usage.totals.messages as f64,
                    calls: calls.len() as u64,
                    failed_calls: calls.iter().filter(|c| !c.ok).count() as u64,
                    grade: graded.row.key.clone(),
                });
        }
        (tasks, unassigned)
    }

    fn arm(&self, query: &Query, config: Option<&str>) -> String {
        config
            .and_then(|digest| query.configuration(digest))
            .and_then(|manifest| serde_json::to_value(manifest).ok())
            .and_then(|manifest| manifest.get(&self.by).cloned())
            .map(|value| match value {
                Value::String(text) if !text.is_empty() => text,
                Value::String(_) | Value::Null => "unrecorded".into(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| "unrecorded".into())
    }
}

fn describe(runs: &[Measured]) -> Value {
    json!({
        "runs": runs.len(),
        "mean_score": round(mean(runs.iter().map(|r| r.score))),
        "pass_rate": round(runs.iter().filter(|r| r.score >= 1.0).count() as f64 / runs.len().max(1) as f64),
        "agent_ms": summarize(runs.iter().filter_map(|r| r.agent_ms.map(|ms| ms as u64))),
        "observed_agent_ms": summarize(runs.iter().map(|r| r.observed_agent_ms)),
        "runs_with_incomplete_agent_time": runs.iter().filter(|r| r.agent_ms.is_none()).count(),
        "tokens": summarize(runs.iter().filter_map(|r| r.tokens.map(|tokens| tokens as u64))),
        "runs_with_incomplete_tokens": runs.iter().filter(|r| r.tokens.is_none()).count(),
        "turns": summarize(runs.iter().map(|r| r.turns as u64)),
        "tool_failures": runs.iter().map(|r| r.failed_calls).sum::<u64>(),
        "tool_calls": runs.iter().map(|r| r.calls).sum::<u64>(),
    })
}

fn test(a: &[Measured], b: &[Measured], value: impl Fn(&Measured) -> Option<f64>) -> Value {
    let a: Vec<f64> = a.iter().filter_map(&value).collect();
    let b: Vec<f64> = b.iter().filter_map(&value).collect();
    json!(mann_whitney(&a, &b).map(|t| round(t.p)))
}

fn mean_present(values: impl Iterator<Item = f64>) -> Option<f64> {
    let values: Vec<_> = values.collect();
    (!values.is_empty()).then(|| mean(values.into_iter()))
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    if n == 0 { 0.0 } else { sum / n as f64 }
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Digests are long; twelve characters tell arms apart in text.
fn short(arm: &str) -> &str {
    arm.get(..12).unwrap_or(arm)
}
