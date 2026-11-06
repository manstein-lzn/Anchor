//! The harness health report: what was recorded, how submissions were
//! judged, how tools and model turns performed, and the integrity of the
//! records themselves. Each irregularity becomes a finding with its records.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::{Analyzer, Finding, Need, Report, unmet};
use crate::query::{
    Call, Effect, GapPlace, Invocation, Key, Outcome, Query, Status, covered, group_by,
    node_result, summarize, usage,
};

/// Evidence cited per finding, at most. Signals count the Runs a finding
/// reaches from its evidence, so this bounds that count too.
const CITED: usize = 2000;
/// A check failing in any of a task's graded Runs is reported, once the task
/// has at least this many: a check that fails now and then is still a
/// quality gap, and the rate says how big.
const FAILING_RUNS: usize = 3;
/// A configuration whose invocations spend this share of their time before
/// the first model request is reported.
const STARTUP_SHARE: f64 = 0.1;

pub struct Health;

impl Analyzer for Health {
    fn id(&self) -> &'static str {
        "health"
    }
    fn version(&self) -> u32 {
        2
    }
    /// Without these, the sections that use them are left out; the kernel
    /// sections never need them.
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
        let invocations = query.invocations();
        records(query, &mut report);
        tasks(query, &mut report);
        if !report.not_applicable.contains(&Need::Observations) {
            coverage(query, &invocations, &mut report);
            tools(&invocations, &mut report);
            turns(&invocations, &mut report);
            if !report.not_applicable.contains(&Need::Time) {
                steps(query, &invocations, &mut report);
            }
            configurations(query, &invocations, &mut report);
        }
        if !report.not_applicable.contains(&Need::Grades) {
            quality(query, &invocations, &mut report);
        }
        report
    }
}

fn cite(keys: impl IntoIterator<Item = Key>) -> Vec<Key> {
    keys.into_iter().take(CITED).collect()
}

fn share(part: usize, whole: usize) -> Value {
    if whole == 0 {
        Value::Null
    } else {
        json!((part as f64 / whole as f64 * 1000.0).round() / 1000.0)
    }
}

/// Kernel fields: how many records, how many stamped, and every verdict that
/// was not an acceptance.
fn records(query: &Query, report: &mut Report) {
    let rows = query.rows();
    let roots: BTreeSet<&str> = rows.iter().map(|r| r.key.root.as_str()).collect();
    report.summary.insert(
        "records",
        json!({
            "rows": rows.len(),
            "roots": roots.len(),
            "stamped_time": share(rows.iter().filter(|r| r.at.is_some()).count(), rows.len()),
            "stamped_config": share(rows.iter().filter(|r| r.config.is_some()).count(), rows.len()),
        }),
    );
    let verdicts = group_by(rows, |row| {
        format!(
            "{}/{}/{}",
            json!(row.action).as_str().unwrap_or_default(),
            json!(row.verdict.effect).as_str().unwrap_or_default(),
            json!(row.verdict.reason).as_str().unwrap_or_default()
        )
    });
    report.summary.insert(
        "verdicts",
        json!(
            verdicts
                .iter()
                .map(|(k, v)| (k.clone(), v.len()))
                .collect::<BTreeMap<_, _>>()
        ),
    );
    let refused = group_by(
        rows.iter().filter(|r| r.verdict.effect != Effect::Accepted),
        |row| {
            (
                kind(&row.key.resource).to_owned(),
                json!(row.verdict.effect)
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                json!(row.verdict.reason)
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        },
    );
    for ((kind, effect, reason), rows) in refused {
        report.findings.push(Finding {
            code: "not_accepted",
            subject: format!("{kind}/*"),
            detail: format!("{} submission(s) {effect}: {reason}", rows.len()),
            evidence: cite(rows.iter().map(|r| r.key.clone())),
        });
    }
}

/// A resource's kind: its id up to the first `/`.
fn kind(id: &str) -> &str {
    id.split('/').next().unwrap_or(id)
}

fn tasks(query: &Query, report: &mut Report) {
    let tasks = query.tasks();
    let statuses = group_by(&tasks, |task| {
        json!(task.entry.status)
            .to_string()
            .trim_matches('"')
            .to_owned()
    });
    let settled: Vec<u64> = tasks
        .iter()
        .filter(|t| t.entry.status.is_some_and(Status::is_settled))
        .filter_map(|t| t.span_ms())
        .collect();
    report.summary.insert(
        "tasks",
        json!({
            "count": tasks.len(),
            "status": statuses.iter().map(|(k, v)| (k.clone(), v.len())).collect::<BTreeMap<_, _>>(),
            "settled_span_ms": summarize(settled),
        }),
    );
    for task in &tasks {
        if let Some(status) = task.entry.status
            && status.is_settled()
            && status != Status::Done
        {
            report.findings.push(Finding {
                code: "task_not_done",
                subject: task.entry.id.clone(),
                detail: format!("settled as {}", json!(status)),
                evidence: cite(task.rows.last().map(|r| r.key.clone())),
            });
        }
    }
}

/// Whether both sides of every invocation were recorded, and whether the
/// observations agree with themselves.
fn coverage(query: &Query, invocations: &[Invocation], report: &mut Report) {
    let observed_runs: BTreeSet<&str> = invocations
        .iter()
        .filter(|i| i.has_model_observations())
        .map(|i| i.run_id.as_str())
        .collect();
    let known_runs: BTreeSet<String> = query
        .tasks()
        .iter()
        .filter_map(|t| t.entry.id.strip_prefix("run/").map(str::to_owned))
        .collect();
    let mut unobserved = Vec::new();
    let mut orphans = Vec::new();
    for invocation in invocations {
        match (invocation.result, !invocation.has_model_observations()) {
            // A node that spent model requests in a Run whose other
            // invocations were observed: the recorder missed this one.
            (Some(result), true)
                if result.cost > 0 && observed_runs.contains(invocation.run_id.as_str()) =>
            {
                unobserved.push(invocation)
            }
            (None, false)
                if known_runs.contains(&invocation.run_id) && !invocation.waiting_for_input() =>
            {
                orphans.push(invocation)
            }
            _ => {}
        }
        for trace in &invocation.traces {
            let facts = &trace.observation.facts;
            let turns = facts
                .iter()
                .filter(|f| matches!(f, anchor_facts::Fact::ModelTurn { .. }))
                .count() as u64;
            let recorded_calls = facts
                .iter()
                .filter(|f| matches!(f, anchor_facts::Fact::ToolCall { .. }))
                .count() as u64;
            let omitted_turns = facts
                .iter()
                .find_map(|f| match f {
                    anchor_facts::Fact::ModelUsage { usage } => Some(usage.omitted_turns),
                    _ => None,
                })
                .unwrap_or(0);
            for fact in facts {
                let anchor_facts::Fact::Lifecycle {
                    model_turns,
                    tool_calls,
                    tool_calls_dropped,
                    outcome,
                    ..
                } = fact
                else {
                    continue;
                };
                let calls = recorded_calls + tool_calls_dropped;
                if turns + omitted_turns != *model_turns || calls != *tool_calls {
                    report.findings.push(Finding {
                        code: "counts_disagree",
                        subject: invocation.key(),
                        detail: format!(
                            "lifecycle reports {} turns and {} calls; {} turns and {} calls recorded",
                            model_turns, tool_calls, turns, calls
                        ),
                        evidence: invocation.evidence(),
                    });
                }
                if *tool_calls_dropped > 0 {
                    report.findings.push(Finding {
                        code: "tool_calls_dropped",
                        subject: invocation.key(),
                        detail: format!("{} tool call(s) not recorded", tool_calls_dropped),
                        evidence: invocation.evidence(),
                    });
                }
                if *outcome != Outcome::Completed {
                    report.findings.push(Finding {
                        code: "invocation_not_completed",
                        subject: invocation.key(),
                        detail: format!("ended {}", json!(outcome)),
                        evidence: invocation.evidence(),
                    });
                }
            }
        }
        let usage = invocation.usage();
        if invocation.has_model_observations() && usage.total_tokens().is_none() {
            report.findings.push(Finding {
                code: "usage_incomplete",
                subject: invocation.key(),
                detail: format!("model usage {:?}; {} observed turn(s), {} detail(s) omitted; complete token total unknown", usage.coverage, usage.totals.messages, usage.totals.omitted_turns),
                evidence: invocation.evidence(),
            });
        }
        let unclosed: Vec<Call> = invocation
            .calls()
            .into_iter()
            .filter(|c| c.ended_at_ms.is_none() || c.started_at_ms.is_none())
            .collect();
        if !unclosed.is_empty() {
            report.findings.push(Finding {
                code: "call_without_time",
                subject: invocation.key(),
                detail: format!("{} tool call(s) lack a start or end time", unclosed.len()),
                evidence: invocation.evidence(),
            });
        }
    }
    report.summary.insert(
        "invocations",
        json!({
            "count": invocations.len(),
            "observed": invocations.iter().filter(|i| i.has_model_observations()).count(),
            "with_result": invocations.iter().filter(|i| i.result.is_some()).count(),
            "resumed": invocations.iter().filter(|i| i.lifecycles().len() > 1 || i.lifecycles().iter().any(|l| l.resumed)).count(),
            "unobserved_with_model_requests": unobserved.len(),
            "observed_without_result": orphans.len(),
            "runs_observed": observed_runs.len(),
            "runs_known": known_runs.len(),
        }),
    );
    for invocation in unobserved {
        report.findings.push(Finding {
            code: "unobserved_invocation",
            subject: invocation.key(),
            detail: format!(
                "node result spent {} model request(s) but no observation was recorded",
                invocation.result.map_or(0, |r| r.cost)
            ),
            evidence: invocation.evidence(),
        });
    }
    for invocation in orphans {
        let outcomes: Vec<Value> = invocation
            .lifecycles()
            .iter()
            .map(|l| json!(l.outcome))
            .collect();
        report.findings.push(Finding {
            code: "observation_without_result",
            subject: invocation.key(),
            detail: format!(
                "observed (outcome {}) but its Run recorded no result for it",
                json!(outcomes)
            ),
            evidence: invocation.evidence(),
        });
    }
}

/// Tool efficiency: frequency, success, failure classes, duration, and calls
/// the model had to correct.
/// How a command ended, as a class name.
fn command_class(end: anchor_facts::CommandEnd) -> &'static str {
    use anchor_facts::CommandStatus;
    match end.status {
        CommandStatus::NotStarted => "not_started",
        CommandStatus::TimedOut => "timed_out",
        CommandStatus::Exited if end.exit_code == Some(0) => "clean",
        CommandStatus::Exited => "exit_nonzero",
    }
}

fn tools(invocations: &[Invocation], report: &mut Report) {
    let calls: Vec<(&Invocation, Call)> = invocations
        .iter()
        .flat_map(|i| i.calls().into_iter().map(move |c| (i, c)))
        .collect();
    let by_tool = group_by(&calls, |(_, call)| call.tool.to_owned());
    let mut summary = BTreeMap::new();
    for (tool, calls) in &by_tool {
        let failed: Vec<_> = calls.iter().filter(|(_, c)| !c.ok).collect();
        let failures = group_by(&failed, |(_, c)| {
            json!(c.failure)
                .as_str()
                .unwrap_or("unclassified")
                .to_owned()
        });
        // For command tools: how the commands ended, by program. A call the
        // host accepted can still have run nothing, been stopped or failed.
        let mut programs: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
        for (_, call) in calls.iter().filter(|(_, c)| c.ok) {
            let (Some(program), Some(end)) = (call.program, call.command) else {
                continue;
            };
            *programs
                .entry(program)
                .or_default()
                .entry(command_class(end).to_owned())
                .or_insert(0) += 1;
        }
        summary.insert(
            tool.clone(),
            json!({
                "commands": programs,
                "calls": calls.len(),
                "ok": calls.len() - failed.len(),
                // Accepted and, for a command, run to a clean exit.
                "done": calls.iter().filter(|(_, c)| c.done()).count(),
                "success_rate": share(calls.len() - failed.len(), calls.len()),
                "failures": failures.iter().map(|(k, v)| (k.clone(), v.len())).collect::<BTreeMap<_, _>>(),
                "duration_ms": summarize(calls.iter().filter_map(|(_, c)| c.duration_ms())),
                "shapes": calls.iter().map(|(_, c)| c.arguments_shape).collect::<BTreeSet<_>>().len(),
                // Where the `path` arguments of this tool pointed, and what
                // happened there: the only handle on the value of a rejected
                // path, whose value itself is never recorded.
                "paths": group_by(
                    calls.iter().filter_map(|(_, c)| Some((c.path?, c))),
                    |(class, _)| *class,
                )
                    .into_iter()
                    .map(|(class, of)| {
                        let failed = of.iter().filter(|(_, call)| !call.ok).count();
                        (format!("{class:?}").to_lowercase(), json!({"calls": of.len(), "failed": failed}))
                    })
                    .collect::<BTreeMap<_, _>>(),
            }),
        );
        // Commands that the host accepted but that did not run to a clean
        // exit, by program and how they ended. Exiting nonzero is how many
        // programs answer (`grep` finding nothing), so it is its own kind,
        // apart from a program that never started or was stopped.
        let ended = group_by(
            calls.iter().filter(|(_, c)| c.ok).filter_map(|(i, c)| {
                let end = c.command?;
                (!end.clean()).then_some((i, c.program.unwrap_or("?"), command_class(end)))
            }),
            |(_, program, class)| (program.to_string(), class.to_string()),
        );
        for ((program, class), ended) in ended {
            let of_program = calls
                .iter()
                .filter(|(_, c)| c.program == Some(program.as_str()))
                .count();
            let mut keys: Vec<Key> = ended.iter().flat_map(|(i, _, _)| i.evidence()).collect();
            keys.dedup();
            let runs: BTreeSet<&str> = ended.iter().map(|(i, _, _)| i.run_id.as_str()).collect();
            report.findings.push(Finding {
                code: if class == "exit_nonzero" {
                    "command_nonzero"
                } else {
                    "command_failures"
                },
                subject: format!("tool {tool} program {program}"),
                detail: format!(
                    "{} of {of_program} {program} command(s) {}, in {} Run(s); the calls themselves succeeded",
                    ended.len(),
                    match class.as_str() {
                        "not_started" => "never started in the sandbox",
                        "timed_out" => "were stopped at the time limit",
                        _ => "exited nonzero",
                    },
                    runs.len(),
                ),
                evidence: cite(keys),
            });
        }
        for (class, failed) in failures {
            let mut keys: Vec<Key> = failed.iter().flat_map(|(i, _)| i.evidence()).collect();
            keys.dedup();
            // The path families the failing calls named; the path itself is
            // not recorded, so this is as close to the value as the records
            // can come.
            let paths: Vec<String> = {
                let mut seen: Vec<String> = failed
                    .iter()
                    .filter_map(|(_, c)| c.path)
                    .map(|class| format!("{class:?}").to_lowercase())
                    .collect();
                seen.sort();
                seen.dedup();
                seen
            };
            report.findings.push(Finding {
                code: "tool_failures",
                subject: format!("tool {tool}"),
                detail: format!(
                    "{} of {} call(s) failed: {class}{}",
                    failed.len(),
                    calls.len(),
                    if paths.is_empty() {
                        String::new()
                    } else {
                        format!(", on {} path(s)", paths.join(", "))
                    }
                ),
                evidence: cite(keys),
            });
        }
    }
    report.summary.insert("tools", json!(summary));

    // A failed call followed, in the same invocation, by a successful call of
    // the same tool: the model recovered from the refusal. Arguments of another
    // shape mean it changed how it asked; the same shape means it changed only
    // the value, which is what a rejected path looks like from here. Both are
    // recoveries, so both count, and the count is of failed calls rather than
    // of the invocations they happened in.
    let mut corrected: BTreeMap<String, (usize, usize, Vec<&Invocation>)> = BTreeMap::new();
    for invocation in invocations {
        let calls = invocation.calls();
        for (index, call) in calls.iter().enumerate() {
            if call.ok {
                continue;
            }
            let Some(later) = calls[index + 1..]
                .iter()
                .find(|later| later.tool == call.tool && later.done())
            else {
                continue;
            };
            let entry = corrected.entry(call.tool.to_owned()).or_default();
            entry.2.push(invocation);
            if later.arguments_shape == call.arguments_shape {
                entry.1 += 1;
            } else {
                entry.0 += 1;
            }
        }
    }
    for (tool, (reshaped, revalued, invocations)) in corrected {
        let mut keys: Vec<Key> = invocations.iter().flat_map(|i| i.evidence()).collect();
        keys.dedup();
        report.findings.push(Finding {
            code: "call_corrected",
            subject: format!("tool {tool}"),
            detail: format!(
                "{} failed call(s) were followed by a successful call of the same tool: \
                 {reshaped} with arguments of another shape, {revalued} with arguments of the same shape",
                reshaped + revalued
            ),
            evidence: cite(keys),
        });
    }
}

/// Model turns: how many per invocation, their size and their latency, and
/// where an invocation's time went.
fn turns(invocations: &[Invocation], report: &mut Report) {
    let observed: Vec<&Invocation> = invocations
        .iter()
        .filter(|i| i.has_model_observations())
        .collect();
    let turns: Vec<_> = observed.iter().flat_map(|i| i.turns()).collect();
    let usage = usage(invocations.iter());
    let timeline_complete =
        usage.details_complete() && observed.iter().all(|i| i.timeline_complete());
    report.summary.insert("usage", json!(usage));
    let (mut model, mut tool, mut total) = (0_u64, 0_u64, 0_u64);
    let mut gaps: BTreeMap<GapPlace, Vec<u64>> = BTreeMap::new();
    for invocation in &observed {
        total += invocation
            .lifecycles()
            .iter()
            .map(|l| l.ended_at_ms.saturating_sub(l.started_at_ms))
            .sum::<u64>();
        model += covered(invocation.turn_spans().into_iter().map(|(_, span)| span));
        tool += covered(invocation.calls().iter().filter_map(Call::span));
        for gap in invocation.gaps() {
            gaps.entry(gap.place()).or_default().push(gap.ms());
        }
    }
    let part = |ms: u64| {
        if total == 0 {
            Value::Null
        } else {
            json!((ms as f64 / total as f64 * 1000.0).round() / 1000.0)
        }
    };
    report.summary.insert(
        "turns",
        json!({
            "per_invocation": summarize(observed.iter().map(|i| i.usage().totals.messages)),
            "detail_scope": "bounded_samples",
            "sampled_turns": usage.totals.sampled_turns,
            "omitted_turns": usage.totals.omitted_turns,
            "total_tokens": summarize(observed.iter().filter(|i| i.usage().total_tokens().is_some()).flat_map(|i| i.turns()).filter_map(|t| t.total_tokens)),
            "input_tokens": summarize(observed.iter().filter(|i| i.usage().input_tokens().is_some()).flat_map(|i| i.turns()).filter_map(|t| t.input_tokens)),
            "output_tokens": summarize(observed.iter().filter(|i| i.usage().output_tokens().is_some()).flat_map(|i| i.turns()).filter_map(|t| t.output_tokens)),
            "elapsed_ms": summarize(observed.iter().filter(|i| i.usage().elapsed_ms().is_some()).flat_map(|i| i.turns()).filter_map(|t| t.elapsed_ms)),
            "time_to_first_token_ms": summarize(turns.iter().filter_map(|t| t.time_to_first_token_ms)),
            "cache_read_share": usage.cache_read_tokens().zip(usage.input_tokens()).map(|(cache, input)| share(cache as usize, input as usize)),
            "compactions": usage.complete().then_some(usage.totals.compaction_messages),
        }),
    );
    // Shares of invocation time: the model working, tools running, and the
    // harness between them, by where the gap lies.
    report.summary.insert(
        "time",
        json!({
            "scope": "recorded_invocation_lifecycles",
            "invocation_ms": total,
            "model_share": usage.elapsed_ms().map(part),
            "observed_model_share": part(model),
            "tool_share": part(tool),
            "timeline_complete": timeline_complete,
            "unobserved_share": timeline_complete.then(|| part(gaps.values().flatten().sum())),
            "uncovered_observation_share": part(gaps.values().flatten().sum()),
            "unobserved": gaps.iter().map(|(place, ms)| (
                json!(place).as_str().unwrap_or_default().to_owned(),
                json!({"share": part(ms.iter().sum()), "ms": summarize(ms.iter().copied())}),
            )).collect::<BTreeMap<_, _>>(),
        }),
    );
}

/// Run steps across roots: how long each node's step took in its Run, and
/// for observed invocations how much of that the invocation itself was.
fn steps(query: &Query, invocations: &[Invocation], report: &mut Report) {
    let lifetimes: BTreeMap<&Key, u64> = invocations
        .iter()
        .filter_map(|i| {
            let ms = i
                .lifecycles()
                .iter()
                .map(|l| l.ended_at_ms.saturating_sub(l.started_at_ms))
                .sum();
            i.has_model_observations().then_some((&i.result?.key, ms))
        })
        .collect();
    let (mut unobserved, mut outside) = (BTreeMap::<String, Vec<u64>>::new(), Vec::new());
    for task in query.tasks() {
        for (row, since) in task.steps() {
            if row.verdict.effect != Effect::Accepted {
                continue;
            }
            let (Some(since), Some((_, node, _))) = (since, node_result(row)) else {
                continue;
            };
            match lifetimes.get(&row.key) {
                Some(&ms) => outside.push(since.saturating_sub(ms)),
                None => unobserved.entry(node.to_owned()).or_default().push(since),
            }
        }
    }
    report.summary.insert(
        "steps",
        json!({
            "observed_overhead_ms": summarize(outside),
            "unobserved_step_ms": unobserved.iter().map(|(node, ms)| (node.clone(), summarize(ms.iter().copied()))).collect::<BTreeMap<_, _>>(),
        }),
    );
}

/// Per configuration: the same node under different configurations is what
/// a later comparison would contrast.
fn configurations(query: &Query, invocations: &[Invocation], report: &mut Report) {
    let observed = invocations.iter().filter(|i| i.has_model_observations());
    let by_config = group_by(observed, |i| i.config().unwrap_or("").to_owned());
    let mut summary = BTreeMap::new();
    for (digest, invocations) in &by_config {
        let manifest = query.configuration(digest);
        // Startup: from the invocation's start to its first observed
        // activity, the harness's fixed cost before the model is asked.
        let startup: Vec<(u64, u64)> = invocations
            .iter()
            .filter(|i| i.timeline_complete())
            .flat_map(|i| {
                let gaps = i.gaps();
                i.lifecycles().into_iter().filter_map(move |life| {
                    let gap = gaps.iter().find(|g| {
                        g.place() == GapPlace::Startup && g.start == life.started_at_ms
                    })?;
                    Some((
                        gap.ms(),
                        life.ended_at_ms.saturating_sub(life.started_at_ms),
                    ))
                })
            })
            .collect();
        let startup_share = share(
            startup.iter().map(|(gap, _)| *gap).sum::<u64>() as usize,
            startup.iter().map(|(_, total)| *total).sum::<u64>() as usize,
        );
        summary.insert(
            digest.clone(),
            json!({
                "node": manifest.map(|m| m.node_id.as_str()),
                "model": manifest.map(|m| m.model.as_str()),
                "plugins": manifest.map(|m| m.plugins.len()),
                "known": manifest.is_some(),
                "invocations": invocations.len(),
                "turns": summarize(invocations.iter().map(|i| i.usage().totals.messages)),
                "tool_calls": summarize(invocations.iter().map(|i| i.calls().len() as u64)),
                "tokens": summarize(invocations.iter().filter_map(|i| i.usage().total_tokens())),
                "usage": usage(invocations.iter().copied()),
                "duration_ms": summarize(invocations.iter().flat_map(|i| i.lifecycles()).map(|l| l.ended_at_ms.saturating_sub(l.started_at_ms))),
                "startup_ms": summarize(startup.iter().map(|(gap, _)| *gap)),
                "startup_share": startup_share,
            }),
        );
        if let Some(part) = startup_share.as_f64()
            && part >= STARTUP_SHARE
        {
            report.findings.push(Finding {
                code: "startup_heavy",
                subject: format!(
                    "config {digest} (node {})",
                    manifest.map_or("?", |m| m.node_id.as_str())
                ),
                detail: format!(
                    "{:.0}% of invocation time passes before the first observed activity (median {} ms)",
                    part * 100.0,
                    summarize(startup.iter().map(|(gap, _)| *gap)).map_or(0, |s| s.median)
                ),
                evidence: cite(invocations.iter().flat_map(|i| i.evidence())),
            });
        }
        if manifest.is_none() {
            report.findings.push(Finding {
                code: "configuration_unknown",
                subject: format!("config {digest}"),
                detail: format!(
                    "{} invocation(s) recorded under a configuration with no readable manifest",
                    invocations.len()
                ),
                evidence: cite(invocations.iter().flat_map(|i| i.evidence())),
            });
        }
    }
    report.summary.insert("configurations", json!(summary));
}

/// Task quality from grades, beside what the graded Runs cost: the signal a
/// comparison of configurations weighs efficiency against.
fn quality(query: &Query, invocations: &[Invocation], report: &mut Report) {
    let grades = query.grades();
    let by_run = group_by(invocations, |i| i.run_id.as_str());
    let by_task = group_by(grades.values(), |g| {
        (g.grade.task.clone(), g.grade.task_version)
    });
    let mut summary = BTreeMap::new();
    for ((task, version), graded) in by_task {
        let runs: BTreeSet<&str> = graded.iter().map(|g| g.grade.run_id.as_str()).collect();
        let of_run = |run: &str| {
            by_run
                .get(run)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .copied()
        };
        let run_tokens = |run: &str| usage(of_run(run)).total_tokens();
        let tokens: Vec<_> = runs.iter().filter_map(|run| run_tokens(run)).collect();
        let agent_ms: Vec<_> = runs
            .iter()
            .filter_map(|run| crate::query::agent_ms(of_run(run)))
            .collect();
        let tool_calls = runs
            .iter()
            .map(|run| of_run(run).map(|i| i.calls().len() as u64).sum::<u64>());
        let configs: BTreeSet<&str> = runs
            .iter()
            .flat_map(|run| of_run(run).filter_map(|i| i.config()))
            .collect();
        let mut checks: BTreeMap<&str, (usize, Vec<Key>)> = BTreeMap::new();
        for g in &graded {
            for check in &g.grade.checks {
                let entry = checks.entry(check.name.as_str()).or_default();
                if check.passed {
                    entry.0 += 1;
                } else {
                    entry.1.push(g.row.key.clone());
                }
            }
        }
        let scores: Vec<f64> = graded.iter().map(|g| g.grade.score()).collect();
        summary.insert(
            format!("{task} v{version}"),
            json!({
                "runs": graded.len(),
                "mean_score": (scores.iter().sum::<f64>() / scores.len() as f64 * 1000.0).round() / 1000.0,
                "pass_rate": share(graded.iter().filter(|g| g.grade.passed()).count(), graded.len()),
                "checks": checks.iter().map(|(name, (passed, _))| (*name, share(*passed, graded.len()))).collect::<BTreeMap<_, _>>(),
                "configurations": configs.len(),
                "tokens": summarize(tokens.iter().copied()),
                "runs_with_incomplete_tokens": runs.len() - tokens.len(),
                "agent_ms": summarize(agent_ms.iter().copied()),
                "runs_with_incomplete_agent_time": runs.len() - agent_ms.len(),
                "tool_calls": summarize(tool_calls),
            }),
        );
        for (name, (passed, failed)) in checks {
            if graded.len() < FAILING_RUNS || passed == graded.len() {
                continue;
            }
            // Where the failing Runs sit in cost among the task's Runs: a
            // check failing only in the cheapest Runs reads differently from
            // one failing in the dearest.
            let failing_runs: BTreeSet<&str> = graded
                .iter()
                .filter(|g| g.grade.checks.iter().any(|c| c.name == name && !c.passed))
                .map(|g| g.grade.run_id.as_str())
                .collect();
            let mean_of = |pick: bool| {
                let picked: Vec<u64> = runs
                    .iter()
                    .filter(|run| failing_runs.contains(*run) == pick)
                    .filter_map(|run| run_tokens(run))
                    .collect();
                (!picked.is_empty()).then(|| picked.iter().sum::<u64>() / picked.len() as u64)
            };
            report.findings.push(Finding {
                code: "check_failing",
                subject: format!("task {task} v{version}"),
                detail: format!(
                    "check {name} failed in {} of {} graded Run(s); mean tokens {} where it failed, {} where it passed",
                    failed.len(),
                    graded.len(),
                    mean_of(true).map_or("?".into(), |t| t.to_string()),
                    mean_of(false).map_or("?".into(), |t| t.to_string()),
                ),
                evidence: cite(failed),
            });
        }
    }
    report.summary.insert("quality", json!(summary));
}
