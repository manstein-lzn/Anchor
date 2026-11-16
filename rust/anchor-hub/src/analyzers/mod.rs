//! L3: analyzers. Each one is a plugin that reads facts through the
//! trajectory algebra only and returns findings that cite the records they
//! rest on. Analyzers do no IO; they can be added, replaced or removed freely.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::query::{Key, Query};

pub mod contrast;
pub mod gate;
pub mod health;
pub mod longitudinal;
pub mod profile;
pub mod signals;
pub mod stall;

/// A field of the record an analyzer cannot work without.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    /// Kernel stamps with time.
    Time,
    /// Run lifecycle source or authorized view.
    Runs,
    /// Observation records of invocations.
    Observations,
    /// Grades of Runs.
    Grades,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Over everything recorded so far.
    Batch,
    /// Repeatedly, as records are appended.
    Follow,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    /// A stable kind, such as `tool_failures`.
    pub code: &'static str,
    /// What the finding is about: a tool, a node, a Run.
    pub subject: String,
    pub detail: String,
    pub evidence: Vec<Key>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub analyzer: &'static str,
    pub version: u32,
    /// Needs the data did not meet, so the analyzer did not guess.
    pub not_applicable: Vec<Need>,
    /// Measurements the findings were judged against.
    pub summary: BTreeMap<&'static str, Value>,
    pub findings: Vec<Finding>,
}

pub trait Analyzer {
    fn id(&self) -> &'static str;
    fn version(&self) -> u32;
    fn needs(&self) -> &'static [Need];
    fn mode(&self) -> Mode {
        Mode::Batch
    }
    fn analyze(&self, query: &Query) -> Report;
}

/// Which needs the data meets: time if any record carries it, observations
/// if any invocation was observed.
pub fn unmet(query: &Query, needs: &[Need]) -> Vec<Need> {
    needs
        .iter()
        .copied()
        .filter(|need| match need {
            Need::Runs => !query.has_run_source(),
            Need::Time => !query.rows().iter().any(|row| row.at.is_some()),
            Need::Observations => !query.invocations().iter().any(|i| !i.traces.is_empty()),
            Need::Grades => query.grades().is_empty(),
        })
        .collect()
}

/// The built-in analyzers, in report order.
pub fn builtin() -> Vec<Box<dyn Analyzer>> {
    vec![Box::new(health::Health), Box::new(stall::Stall::default())]
}
