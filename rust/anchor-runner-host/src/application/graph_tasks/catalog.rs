//! Completed task visibility is a projection of ownership, plans and Runs;
//! no archive marker can outlive a newly admitted task or an edited Graph.
use super::*;
use crate::application::{
    run_access::RunOutcome, run_access::RunPhase, scheduling::GraphScheduleReference,
};

impl RunApplication {
    pub(in crate::application) fn completed_task_graphs(
        &self,
        names: &[String],
    ) -> BTreeSet<String> {
        let Some(schedules) = self
            .schedules
            .as_ref()
            .and_then(|port| port.graph_references().ok())
        else {
            return BTreeSet::new();
        };
        if schedules.is_empty() {
            return BTreeSet::new();
        }
        let Ok(channels) = self.channel_snapshots() else {
            return BTreeSet::new();
        };
        let selected = match self.impact_scan() {
            graphs::ImpactScan::Read {
                bundles,
                considered,
                total,
                missing,
            } => {
                if graphs::coverage_of(considered, total, missing)
                    != graphs::ImpactCoverage::Complete
                {
                    return BTreeSet::new();
                }
                bundles
                    .into_iter()
                    .flat_map(|(_, bundle)| {
                        bundle
                            .snapshot
                            .nodes
                            .into_iter()
                            .flat_map(|node| node.plugins)
                            .filter_map(|plugin| plugin.graph_tasks)
                            .flat_map(|policy| policy.graphs)
                    })
                    .collect::<BTreeSet<_>>()
            }
            _ => return BTreeSet::new(),
        };
        names
            .iter()
            .filter(|name| {
                !selected.contains(*name)
                    && !channels.iter().any(|channel| channel.graph == **name)
                    && self.completed_once_task(name, &schedules).unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    fn completed_once_task(
        &self,
        graph: &str,
        schedules: &[GraphScheduleReference],
    ) -> Result<bool, ApplicationError> {
        let Some(owned) = self.task_graph_ownership_record(graph)? else {
            return Ok(false);
        };
        if owned.graph != graph
            || self
                .configured_graph
                .as_ref()
                .is_some_and(|(name, _)| name == graph)
        {
            return Ok(false);
        }
        let plans = schedules
            .iter()
            .filter(|plan| plan.graph == graph)
            .collect::<Vec<_>>();
        if plans.is_empty() || plans.iter().any(|plan| !plan.once || plan.enabled) {
            return Ok(false);
        }
        let (path, bundle) = self.load_graph(graph).map_err(invalid)?;
        let identity = crate::run_metadata::graph_identity(&path)?;
        if !self.graph_callers(&identity)?.is_empty() {
            return Ok(false);
        }
        let digest = bundle.snapshot.digest()?;
        let runs = self.runs_for_graph(&identity)?;
        let mut accepted = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for (id, view) in runs {
            if view.active()
                || !matches!(
                    view.phase(),
                    RunPhase::Settled {
                        outcome: RunOutcome::Done,
                        ..
                    }
                )
            {
                return Ok(false);
            }
            let metadata = self.metadata(&id)?.ok_or(ApplicationError::Missing)?;
            let Some(origin) = metadata.task.as_ref() else {
                return Ok(false);
            };
            if origin.scope_key() != owned.scope
                || crate::run_metadata::graph_identity(&self.graph_bundle_path(&origin.graph))?
                    != owned.source
                || metadata.graph_digest != digest
            {
                return Ok(false);
            }
            if let Some(request) = metadata.request {
                accepted.insert(request.id);
            }
            ids.insert(id);
        }
        // A disabled plan may have been paused before admission. Its exact
        // occurrence must have a real successful Run, not just an old deadline.
        if !plans.iter().all(|plan| accepted.contains(&plan.request_id)) {
            return Ok(false);
        }
        self.check_task_runs_deletion(&ids)?;
        Ok(true)
    }
}
