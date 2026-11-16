//! Raw-sample UTC summaries. Run quality and invocation efficiency never share a denominator.
use crate::query::Query;
use anchor_facts::window::WindowManifest;
use anchor_facts::{ProvenanceCategory, longitudinal::*};
use std::collections::{BTreeMap, BTreeSet};

type Identity = (
    ProvenanceCategory,
    String,
    Option<u32>,
    Option<String>,
    Option<String>,
);
#[derive(Default)]
struct Samples {
    summary: IntervalSamples,
    quality: Vec<f64>,
    tokens: Vec<f64>,
    costs: Vec<f64>,
    times: Vec<f64>,
}
impl Samples {
    fn finish(mut self, limit: usize) -> IntervalSamples {
        self.summary.tool_error_families_total = self.summary.tool_error_families.len();
        self.summary.command_error_families_total = self.summary.command_error_families.len();
        self.summary.tool_error_families = self
            .summary
            .tool_error_families
            .into_iter()
            .take(limit)
            .collect();
        self.summary.command_error_families = self
            .summary
            .command_error_families
            .into_iter()
            .take(limit)
            .collect();
        self.summary.quality = distribution(self.quality);
        self.summary.tokens = distribution(self.tokens);
        self.summary.cost_usd = distribution(self.costs);
        self.summary.agent_ms = distribution(self.times);
        self.summary
    }
}
fn distribution(mut values: Vec<f64>) -> Option<Distribution> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    Some(Distribution {
        samples: n,
        min: values[0],
        median: if n.is_multiple_of(2) {
            (values[n / 2 - 1] + values[n / 2]) / 2.0
        } else {
            values[n / 2]
        },
        p95: values[(n * 95).div_ceil(100) - 1],
        mean: values.iter().sum::<f64>() / n as f64,
    })
}
fn delta(manifest: &WindowManifest, key: &crate::query::Key) -> bool {
    manifest.sources.iter().any(|s| {
        s.source_instance == key.root
            && s.resources.iter().any(|p| {
                p.resource == key.resource && key.place > p.after && key.place <= p.through
            })
    })
}

pub fn summarize(query: &Query, manifest: &WindowManifest) -> Longitudinal {
    let observation_window = UtcInterval {
        start_ms: manifest.baseline_at_ms.unwrap_or(0),
        end_ms: manifest.now_ms,
    };
    let baseline_window = manifest.baseline_at_ms.map(|end_ms| UtcInterval {
        start_ms: 0,
        end_ms,
    });
    let interval = |at| {
        if observation_window.contains(at) {
            Some(1)
        } else if baseline_window.as_ref().is_some_and(|b| b.contains(at)) {
            Some(0)
        } else {
            None
        }
    };
    let mut groups: BTreeMap<Identity, [Samples; 2]> = BTreeMap::new();
    let grades = query.grades();
    let invocations = query.invocations();
    let mut configs_by_run: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    for invocation in &invocations {
        if let Some(config) = invocation
            .config()
            .filter(|d| query.configuration(d).is_some_and(|c| c.is_stable()))
        {
            configs_by_run
                .entry(invocation.run_key())
                .or_default()
                .insert(config);
        }
    }
    let graphs: BTreeMap<_, _> = query
        .resources()
        .filter_map(|r| {
            Some((
                (r.namespace.as_str(), r.id.strip_prefix("run/")?),
                r.graph_digest.as_deref()?,
            ))
        })
        .collect();
    let mut missing_timestamp_runs = 0;
    let mut missing_timestamp_invocations = 0;
    let mut late_grades_joined = 0;
    let mut provenance_runs = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for task in query.tasks() {
        let Some(run) = task.entry.id.strip_prefix("run/") else {
            continue;
        };
        if !seen.insert((task.namespace, run)) {
            continue;
        }
        *provenance_runs.entry(task.provenance).or_default() += 1;
        let at = task
            .entry
            .settled_at
            .or_else(|| {
                if task.entry.status.is_some_and(|s| s.is_settled()) {
                    task.last_at()
                } else {
                    None
                }
            })
            .or(task.entry.created_at);
        let Some(at) = at else {
            missing_timestamp_runs += 1;
            continue;
        };
        let Some(bucket) = interval(at) else {
            continue;
        };
        let grade = grades.get(&(task.namespace, run));
        let config = configs_by_run
            .get(&(task.namespace, run))
            .filter(|c| c.len() == 1)
            .map(|c| (*c.first().expect("one")).to_owned());
        let identity = (
            task.provenance,
            grade
                .map(|g| format!("task:{}", g.grade.task))
                .unwrap_or_else(|| {
                    format!(
                        "graph:{}",
                        task.entry.graph_digest.as_deref().unwrap_or("unknown")
                    )
                }),
            grade.map(|g| g.grade.task_version),
            grade.and_then(|g| g.row.config.clone()),
            config,
        );
        let sample = &mut groups.entry(identity).or_default()[bucket];
        sample.summary.runs += 1;
        sample.summary.settled_runs +=
            usize::from(task.entry.status.is_some_and(|s| s.is_settled()));
        *sample
            .summary
            .run_outcomes
            .entry(serde_json::to_string(&task.entry.status).expect("status serializes"))
            .or_default() += 1;
        if let Some(grade) = grade {
            sample.summary.graded_runs += 1;
            sample.quality.push(grade.grade.score());
            if delta(manifest, &grade.row.key) && bucket == 0 {
                late_grades_joined += 1;
            }
        }
    }
    let mut exposures: BTreeMap<(ProvenanceCategory, String, String, String), [usize; 2]> =
        BTreeMap::new();
    let mut unknown_exposure = 0;
    for invocation in &invocations {
        let at = invocation
            .lifecycles()
            .iter()
            .map(|l| l.started_at_ms)
            .min()
            .or_else(|| invocation.traces.iter().filter_map(|t| t.row.at).min())
            .or_else(|| invocation.result.and_then(|r| r.at));
        let Some(at) = at else {
            missing_timestamp_invocations += 1;
            continue;
        };
        let Some(bucket) = interval(at) else {
            continue;
        };
        let grade = grades.get(&invocation.run_key());
        let config = invocation.config().and_then(|d| {
            query
                .configuration(d)
                .filter(|c| c.is_stable())
                .map(|c| (d, c))
        });
        if let Some((digest, c)) = config {
            exposures
                .entry((
                    invocation.provenance,
                    digest.into(),
                    c.graph_digest.clone(),
                    c.envelope_sha256.clone(),
                ))
                .or_default()[bucket] += 1;
        } else {
            unknown_exposure += 1;
        }
        let graph = graphs
            .get(&invocation.run_key())
            .copied()
            .or_else(|| invocation.graph_digest())
            .or_else(|| config.map(|(_, c)| c.graph_digest.as_str()))
            .unwrap_or("unknown");
        let identity = (
            invocation.provenance,
            grade
                .map(|g| format!("task:{}", g.grade.task))
                .unwrap_or_else(|| format!("graph:{graph}")),
            grade.map(|g| g.grade.task_version),
            grade.and_then(|g| g.row.config.clone()),
            config.map(|(d, _)| d.into()),
        );
        let sample = &mut groups.entry(identity).or_default()[bucket];
        sample.summary.invocations += 1;
        let usage = invocation.usage();
        if let Some(v) = usage.total_tokens() {
            sample.tokens.push(v as f64);
        }
        if let Some(v) = usage.cost_usd() {
            sample.costs.push(v);
        }
        if let Some(v) = invocation.agent_ms() {
            sample.times.push(v as f64);
        }
        for call in invocation.calls().into_iter().filter(|c| !c.done()) {
            if !call.ok {
                *sample
                    .summary
                    .tool_error_families
                    .entry(format!("{}:{:?}", call.tool, call.failure))
                    .or_default() += 1;
            }
            if let Some(end) = call.command.filter(|c| !c.clean()) {
                *sample
                    .summary
                    .command_error_families
                    .entry(format!(
                        "{}:{:?}",
                        call.program.unwrap_or("unknown"),
                        end.status
                    ))
                    .or_default() += 1;
            }
        }
    }
    let mix = |bucket: usize| {
        let mut mix: BTreeMap<_, [usize; 2]> = BTreeMap::new();
        for ((p, t, v, g, _), samples) in &groups {
            let n = &samples[bucket].summary;
            if n.runs + n.invocations > 0 {
                let counts = mix.entry((*p, t.clone(), *v, g.clone())).or_default();
                counts[0] += n.runs;
                counts[1] += n.invocations;
            }
        }
        mix
    };
    let (baseline_mix, observation_mix) = (mix(0), mix(1));
    let total =
        |mix: &BTreeMap<_, [usize; 2]>, i: usize| mix.values().map(|n| n[i] as u128).sum::<u128>();
    let composition_changed = baseline_window.is_some()
        && (baseline_mix.keys().ne(observation_mix.keys())
            || baseline_mix.iter().any(|(key, b)| {
                (0..2).any(|i| {
                    b[i] as u128 * total(&observation_mix, i)
                        != observation_mix.get(key).map_or(0, |n| n[i]) as u128
                            * total(&baseline_mix, i)
                })
            }));
    let strata_total = groups.len();
    let strata = groups
        .into_iter()
        .take(manifest.limit)
        .map(
            |((provenance, task_or_graph, task_version, grader_config, config), [b, o])| {
                let baseline = b.finish(manifest.limit);
                let observation = o.finish(manifest.limit);
                let cost_delta = baseline
                    .cost_usd
                    .as_ref()
                    .zip(observation.cost_usd.as_ref())
                    .filter(|(b, o)| {
                        b.samples == baseline.invocations && o.samples == observation.invocations
                    })
                    .map(|(b, o)| o.mean - b.mean);
                Stratum {
                    provenance,
                    task_or_graph,
                    task_version,
                    grader_config,
                    config,
                    baseline,
                    observation,
                    cost_delta,
                }
            },
        )
        .collect();
    let config_exposures_total = exposures.len();
    let config_exposures = exposures
        .into_iter()
        .take(manifest.limit)
        .map(
            |(
                (provenance, config, graph_digest, envelope_sha256),
                [baseline_invocations, observation_invocations],
            )| ConfigExposure {
                provenance,
                config,
                graph_digest,
                envelope_sha256,
                baseline_invocations,
                observation_invocations,
            },
        )
        .collect();
    Longitudinal {
        as_of_ms: manifest.now_ms,
        baseline_window,
        observation_window,
        bucket_rules: BucketRules {
            run_outcome_quality: "run_settled_or_created_at_ms".into(),
            invocation_efficiency: "invocation_started_at_ms".into(),
        },
        missing_timestamp_runs,
        missing_timestamp_invocations,
        late_grades_joined,
        revised_prior_interval: late_grades_joined > 0,
        composition_changed,
        provenance_runs,
        strata,
        strata_total,
        config_exposures,
        config_exposures_total,
        unknown_exposure,
        observational_only: true,
    }
}
