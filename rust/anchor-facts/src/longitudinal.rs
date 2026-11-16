//! Versioned, observational interval summaries; never execution authority.
use crate::ProvenanceCategory;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UtcInterval {
    pub start_ms: u64,
    pub end_ms: u64,
}
impl UtcInterval {
    pub fn contains(&self, at: u64) -> bool {
        self.start_ms <= at && at < self.end_ms
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BucketRules {
    pub run_outcome_quality: String,
    pub invocation_efficiency: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Distribution {
    pub samples: usize,
    pub min: f64,
    pub median: f64,
    pub p95: f64,
    pub mean: f64,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntervalSamples {
    pub runs: usize,
    pub settled_runs: usize,
    pub graded_runs: usize,
    pub quality: Option<Distribution>,
    pub run_outcomes: BTreeMap<String, usize>,
    pub invocations: usize,
    pub tokens: Option<Distribution>,
    pub cost_usd: Option<Distribution>,
    pub agent_ms: Option<Distribution>,
    pub tool_error_families: BTreeMap<String, usize>,
    pub tool_error_families_total: usize,
    pub command_error_families: BTreeMap<String, usize>,
    pub command_error_families_total: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stratum {
    pub provenance: ProvenanceCategory,
    pub task_or_graph: String,
    pub task_version: Option<u32>,
    pub grader_config: Option<String>,
    pub config: Option<String>,
    pub baseline: IntervalSamples,
    pub observation: IntervalSamples,
    pub cost_delta: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigExposure {
    pub provenance: ProvenanceCategory,
    pub config: String,
    pub graph_digest: String,
    pub envelope_sha256: String,
    pub baseline_invocations: usize,
    pub observation_invocations: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Longitudinal {
    pub as_of_ms: u64,
    pub baseline_window: Option<UtcInterval>,
    pub observation_window: UtcInterval,
    pub bucket_rules: BucketRules,
    pub missing_timestamp_runs: usize,
    pub missing_timestamp_invocations: usize,
    pub late_grades_joined: usize,
    pub revised_prior_interval: bool,
    pub composition_changed: bool,
    pub provenance_runs: BTreeMap<ProvenanceCategory, usize>,
    pub strata: Vec<Stratum>,
    pub strata_total: usize,
    pub config_exposures: Vec<ConfigExposure>,
    pub config_exposures_total: usize,
    pub unknown_exposure: usize,
    pub observational_only: bool,
}
impl Longitudinal {
    pub fn validate(&self) -> Result<(), String> {
        if self.observation_window.start_ms > self.observation_window.end_ms
            || self.observation_window.end_ms != self.as_of_ms
            || self.baseline_window.as_ref().is_some_and(|b| {
                b.start_ms > b.end_ms || b.end_ms != self.observation_window.start_ms
            })
            || self.bucket_rules.run_outcome_quality != "run_settled_or_created_at_ms"
            || self.bucket_rules.invocation_efficiency != "invocation_started_at_ms"
            || !self.observational_only
            || self.strata.len() > self.strata_total
            || self.config_exposures.len() > self.config_exposures_total
        {
            return Err("invalid longitudinal intervals or sample units".into());
        }
        for s in &self.strata {
            for samples in [&s.baseline, &s.observation] {
                for d in [
                    &samples.quality,
                    &samples.tokens,
                    &samples.cost_usd,
                    &samples.agent_ms,
                ]
                .into_iter()
                .flatten()
                {
                    if d.samples == 0
                        || [d.min, d.median, d.p95, d.mean]
                            .into_iter()
                            .any(|v| !v.is_finite() || v < 0.0)
                        || d.min > d.median
                        || d.median > d.p95
                    {
                        return Err("invalid raw-sample distribution".into());
                    }
                }
                if samples.graded_runs > samples.runs
                    || samples
                        .quality
                        .as_ref()
                        .is_some_and(|d| d.samples != samples.graded_runs)
                {
                    return Err("invalid quality denominator".into());
                }
                if samples.settled_runs > samples.runs
                    || samples.run_outcomes.values().sum::<usize>() != samples.runs
                    || samples.tool_error_families.len() > samples.tool_error_families_total
                    || samples.command_error_families.len() > samples.command_error_families_total
                    || [&samples.tokens, &samples.cost_usd, &samples.agent_ms]
                        .into_iter()
                        .flatten()
                        .any(|d| d.samples > samples.invocations)
                {
                    return Err("invalid metric denominator".into());
                }
            }
            if s.cost_delta.is_some_and(|d| !d.is_finite())
                || (s.cost_delta.is_some()
                    && [&s.baseline, &s.observation].into_iter().any(|v| {
                        v.invocations == 0
                            || v.cost_usd
                                .as_ref()
                                .is_none_or(|d| d.samples != v.invocations)
                    }))
            {
                return Err("incomplete cost comparison".into());
            }
        }
        Ok(())
    }
}
