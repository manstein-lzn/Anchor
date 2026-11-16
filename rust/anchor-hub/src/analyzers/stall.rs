//! Stall candidates: Runs without recent records, and within invocations the
//! stretches where nothing was observed, tool calls and model turns far
//! slower than their peers, and same-shape failure stretches across requests.

use std::collections::BTreeMap;

use serde_json::json;

use super::{Analyzer, Finding, Mode, Need, Report, unmet};
use crate::query::{Activity, Call, Invocation, Query, Status, group_by, repeats, summarize};

pub struct Stall {
    /// An unsettled task with nothing recorded for this long is idle.
    pub idle_ms: u64,
    /// A gap, tool call or model turn is flagged only when it is at least
    /// this long ...
    pub floor_ms: u64,
    /// ... and at least this many times the median of its peers.
    pub factor: u64,
    /// The minimum same-shape failure stretch worth reporting as a weak clue.
    pub repeats: usize,
}

impl Default for Stall {
    fn default() -> Self {
        Self {
            idle_ms: 15 * 60_000,
            floor_ms: 30_000,
            factor: 5,
            repeats: 3,
        }
    }
}

impl Analyzer for Stall {
    fn id(&self) -> &'static str {
        "stall"
    }
    fn version(&self) -> u32 {
        2
    }
    fn needs(&self) -> &'static [Need] {
        &[Need::Time, Need::Observations]
    }
    fn mode(&self) -> Mode {
        Mode::Follow
    }

    fn analyze(&self, query: &Query) -> Report {
        let mut report = Report {
            analyzer: self.id(),
            version: self.version(),
            not_applicable: unmet(query, self.needs()),
            summary: BTreeMap::new(),
            findings: Vec::new(),
        };
        report.summary.insert(
            "thresholds",
            json!({"idle_ms": self.idle_ms, "floor_ms": self.floor_ms, "factor": self.factor, "repeats": self.repeats, "now": query.now()}),
        );
        let invocations = query.invocations();
        if !report.not_applicable.contains(&Need::Time) {
            self.tasks(query, &invocations, &mut report);
        }
        if report.not_applicable.is_empty() {
            self.gaps(&invocations, &mut report);
            self.slow(&invocations, &mut report);
        }
        self.failure_stretches(&invocations, &mut report);
        report
    }
}

impl Stall {
    /// The bar for one item among peers with median `median`.
    fn bar(&self, median: u64) -> u64 {
        self.floor_ms.max(median.saturating_mul(self.factor))
    }

    fn tasks(&self, query: &Query, invocations: &[Invocation], report: &mut Report) {
        let Some(now) = query.now() else {
            return;
        };
        let mut idle = 0;
        let mut waiting_input = 0;
        for task in query.tasks() {
            let Some(status) = task.entry.status else {
                continue;
            };
            let run = task.entry.id.strip_prefix("run/");
            let observed: Vec<_> = invocations
                .iter()
                .filter(|invocation| {
                    task.namespace == invocation.namespace
                        && run == Some(invocation.run_id.as_str())
                })
                .collect();
            if status == Status::Executing
                && observed
                    .iter()
                    .any(|invocation| invocation.waiting_for_input())
            {
                waiting_input += 1;
                continue;
            }
            let last_observed = observed
                .iter()
                .flat_map(|invocation| &invocation.traces)
                .filter(|trace| trace.row.verdict.is_accepted())
                .filter_map(|trace| trace.row.at)
                .max();
            let Some(last) = task.last_at().into_iter().chain(last_observed).max() else {
                continue;
            };
            if status.is_settled() || now.saturating_sub(last) < self.idle_ms {
                continue;
            }
            idle += 1;
            let code = if status == Status::Executing {
                "idle_task"
            } else {
                "parked_task"
            };
            report.findings.push(Finding {
                code,
                subject: task.entry.id.clone(),
                detail: format!(
                    "{} with nothing recorded for {} min; current activity unknown, not proof of a stall",
                    json!(status).to_string().trim_matches('"'),
                    now.saturating_sub(last) / 60_000
                ),
                evidence: task
                    .rows
                    .last()
                    .map(|r| r.key.clone())
                    .into_iter()
                    .collect(),
            });
        }
        report.summary.insert("idle_tasks", json!(idle));
        report
            .summary
            .insert("waiting_input_tasks", json!(waiting_input));
    }

    fn gaps(&self, invocations: &[Invocation], report: &mut Report) {
        let all: Vec<u64> = invocations
            .iter()
            .filter(|invocation| invocation.timeline_complete())
            .flat_map(|i| i.gaps())
            .map(|g| g.ms())
            .collect();
        let Some(summary) = summarize(all) else {
            return;
        };
        let bar = self.bar(summary.median);
        report
            .summary
            .insert("gap_ms", json!({"distribution": summary, "bar": bar}));
        for invocation in invocations
            .iter()
            .filter(|invocation| invocation.timeline_complete())
        {
            let calls = invocation.calls();
            for gap in invocation.gaps().into_iter().filter(|g| g.ms() >= bar) {
                report.findings.push(Finding {
                    code: "idle_gap",
                    subject: invocation.key(),
                    detail: format!(
                        "{} s with no model turn or tool call observed, after {} and before {}",
                        gap.ms() / 1000,
                        describe(gap.after, &calls),
                        describe(gap.before, &calls)
                    ),
                    evidence: invocation.evidence(),
                });
            }
        }
    }

    fn slow(&self, invocations: &[Invocation], report: &mut Report) {
        let calls: Vec<(&Invocation, Call)> = invocations
            .iter()
            .flat_map(|i| i.calls().into_iter().map(move |c| (i, c)))
            .collect();
        for (tool, calls) in group_by(&calls, |(_, c)| c.tool.to_owned()) {
            let Some(summary) = summarize(calls.iter().filter_map(|(_, c)| c.duration_ms())) else {
                continue;
            };
            let bar = self.bar(summary.median);
            for (invocation, call) in calls {
                if let Some(ms) = call.duration_ms().filter(|&ms| ms >= bar) {
                    report.findings.push(Finding {
                        code: "slow_tool_call",
                        subject: format!("tool {tool}"),
                        detail: format!(
                            "{} s in {}, median {} ms",
                            ms / 1000,
                            invocation.key(),
                            summary.median
                        ),
                        evidence: invocation.evidence(),
                    });
                }
            }
        }
        let turns: Vec<_> = invocations
            .iter()
            .flat_map(|i| i.turns().into_iter().map(move |t| (i, t)))
            .collect();
        if let Some(summary) = summarize(turns.iter().filter_map(|(_, t)| t.elapsed_ms)) {
            let bar = self.bar(summary.median);
            for (invocation, turn) in turns {
                if let Some(ms) = turn.elapsed_ms.filter(|&ms| ms >= bar) {
                    report.findings.push(Finding {
                        code: "slow_model_turn",
                        subject: invocation.key(),
                        detail: format!(
                            "turn {} took {} s, median {} ms",
                            turn.index,
                            ms / 1000,
                            summary.median
                        ),
                        evidence: invocation.evidence(),
                    });
                }
            }
        }
    }

    fn failure_stretches(&self, invocations: &[Invocation], report: &mut Report) {
        for invocation in invocations {
            let calls = invocation.calls();
            for stretch in repeats(&calls, self.repeats, |c| (c.tool, c.arguments_shape, c.ok)) {
                let first = &calls[stretch.start];
                if first.ok {
                    continue;
                }
                let requests = calls[stretch.clone()]
                    .iter()
                    .map(|call| call.model_request)
                    .collect::<std::collections::BTreeSet<_>>();
                // Calls in one model response are parallel alternatives, not
                // sequential retries observed by the model.
                if requests.len() < 2 {
                    continue;
                }
                let failures = group_by(&calls[stretch.clone()], |call| {
                    json!(call.failure)
                        .as_str()
                        .unwrap_or("unclassified")
                        .to_owned()
                });
                let failure = if failures.len() == 1 {
                    failures.into_keys().next().expect("nonempty stretch")
                } else {
                    failures
                        .into_iter()
                        .map(|(class, calls)| format!("{class}: {}", calls.len()))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let programs = calls[stretch.clone()]
                    .iter()
                    .filter_map(|call| call.program)
                    .collect::<std::collections::BTreeSet<_>>();
                let shell_programs = calls[stretch.clone()]
                    .iter()
                    .filter_map(|call| call.shell_program)
                    .collect::<std::collections::BTreeSet<_>>();
                let hints = if programs.len() > 1 || shell_programs.len() > 1 {
                    let mut details = Vec::new();
                    if !programs.is_empty() {
                        details.push(format!(
                            "program hints: {}",
                            programs.into_iter().collect::<Vec<_>>().join(", ")
                        ));
                    }
                    if !shell_programs.is_empty() {
                        details.push(format!(
                            "shell-program hints: {}",
                            shell_programs.into_iter().collect::<Vec<_>>().join(", ")
                        ));
                    }
                    format!(
                        "; {} (command identity is not established)",
                        details.join("; ")
                    )
                } else {
                    String::new()
                };
                report.findings.push(Finding {
                    code: "consecutive_same_shape_failures",
                    subject: invocation.key(),
                    detail: format!(
                        "{} consecutive failed {} calls shared one argument shape ({}) across {} model requests; operations/targets may differ, retry loop unconfirmed{}",
                        stretch.len(),
                        first.tool,
                        failure,
                        requests.len(),
                        hints,
                    ),
                    evidence: invocation.evidence(),
                });
            }
        }
    }
}

fn describe(activity: Option<Activity>, calls: &[Call]) -> String {
    match activity {
        None => "the invocation boundary".into(),
        Some(Activity::Turn { index }) => format!("model turn {index}"),
        Some(Activity::Tool { index }) => match calls.get(index) {
            Some(call) => format!("tool call {}", call.tool),
            None => format!("tool call {index}"),
        },
    }
}
