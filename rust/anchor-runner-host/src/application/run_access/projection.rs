use super::run_view::RunView as LifecycleView;
use super::{RunView, validate_control_identity};
use crate::application::{ApplicationError, RunApplication};
use crate::run_metadata::RunMetadata;
use anchor_kernel::{Outcome, Phase, Waiter};
use serde::Serialize;
use serde_json::{Value, json};
pub(crate) fn projected_run_is_unfinished(run: &Value) -> bool {
    run.get("projection")
        .and_then(|p| p.get("phase"))
        .and_then(Value::as_str)
        .is_some_and(|phase| matches!(phase, "executing" | "waiting"))
}
pub(crate) use super::run_view::AvailableActions as RunActions;
#[derive(Debug, Serialize)]
pub(crate) struct RunProjection {
    pub(crate) phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) waiting_for: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) outcome: Option<&'static str>,
    pub(crate) reason: Option<String>,
    pub(crate) active: bool,
    /// A later conversation Run took this one over; it can never execute again.
    /// Omitted otherwise, so existing projections keep their bytes.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) superseded: bool,
    pub(crate) actions: RunActions,
    pub(crate) updated: Option<String>,
}
impl RunProjection {
    pub(crate) fn from_view(view: &LifecycleView) -> Self {
        let (phase, waiting_for, outcome, reason) = match &view.phase {
            Phase::Executing => ("executing", None, None, None),
            Phase::Waiting {
                waiter: Waiter::Human { reason },
            } => ("waiting", Some("human"), None, Some(reason.clone())),
            Phase::Waiting {
                waiter: Waiter::Child { .. },
            } => ("waiting", Some("child"), None, view.reason.clone()),
            Phase::Waiting {
                waiter: Waiter::Effect { .. },
            } => ("waiting", Some("effect"), None, view.reason.clone()),
            Phase::Settled {
                result: Outcome::Done,
            } => ("settled", None, Some("done"), None),
            Phase::Settled {
                result: Outcome::Failed { detail, .. },
            } => ("settled", None, Some("failed"), Some(detail.clone())),
            Phase::Settled {
                result: Outcome::Escalated { detail },
            } => ("settled", None, Some("escalated"), Some(detail.clone())),
            Phase::Settled {
                result: Outcome::Unknown { detail },
            } => ("settled", None, Some("unknown"), Some(detail.clone())),
            Phase::Settled {
                result: Outcome::Aborted { detail },
            } => ("settled", None, Some("aborted"), Some(detail.clone())),
        };
        Self {
            phase,
            waiting_for,
            outcome,
            reason,
            active: view.worker,
            superseded: false,
            actions: view.actions(),
            updated: view.updated.clone(),
        }
    }
}
impl RunApplication {
    pub(crate) async fn project_run<'record>(
        &self,
        record: impl Into<RunView<'record>>,
        metadata: Option<&RunMetadata>,
    ) -> Result<RunProjection, ApplicationError> {
        let supplied = record.into();
        let observed = self
            .read_run(&supplied.record.run_id)?
            .ok_or(ApplicationError::Missing)?;
        self.project_observed_run(&observed, metadata)
    }

    fn project_observed_run(
        &self,
        observed: &RunView<'_>,
        metadata: Option<&RunMetadata>,
    ) -> Result<RunProjection, ApplicationError> {
        let record = observed.record.as_ref();
        let view = &observed.lifecycle;
        let mut projection = RunProjection::from_view(view);
        projection.actions.abandon = observed.can_abandon();
        projection.actions.pause = observed.can_pause();
        projection.actions.stop = observed.can_stop();
        projection.actions.resume = observed.can_resume();
        // Scope and identity narrow advisory actions; writes still revalidate.
        let identity = metadata.filter(|m| validate_control_identity(record, m).is_ok());
        let coordinated = metadata.is_some_and(|m| m.session_call.is_some());
        if !view.worker && !coordinated && identity.is_none() {
            projection.actions.pause = false;
            projection.actions.stop = false;
            projection.actions.resume = false;
            projection.actions.abandon = false;
        }
        projection.actions.pause &= !coordinated;
        // A lineage that cannot be read refuses resume, as before, without
        // claiming a takeover it did not see.
        let superseded = self.superseded_conversation_ancestor(&record.run_id);
        projection.superseded = matches!(superseded, Ok(Some(_)));
        projection.actions.resume = projection.actions.resume
            && !coordinated
            && identity.is_some_and(|m| self.ensure_current_assistant(m).is_ok())
            && self.abandon_intent(&record.run_id)?.is_none()
            && matches!(superseded, Ok(None));
        if projection.superseded {
            // A taken-over Run only keeps the control that ends a live worker.
            projection.actions.pause = false;
            projection.actions.abandon = false;
            projection.actions.stop &= view.worker;
        }
        Ok(projection)
    }
    pub(crate) fn pilot_session_runs(
        &self,
        run_ids: &[String],
    ) -> Result<Vec<Value>, ApplicationError> {
        self.raw_records()?
            .into_iter()
            .filter(|(id, _)| run_ids.contains(id))
            .map(|(id, record)| {
                let mut value = serde_json::to_value(record)
                    .map_err(|error| ApplicationError::Storage(error.to_string()))?;
                value
                    .as_object_mut()
                    .expect("record")
                    .shift_remove("status");
                Ok(json!({"run":id,"record":value}))
            })
            .collect()
    }
    pub(crate) async fn projected_runs(&self) -> Result<Vec<Value>, ApplicationError> {
        let mut runs = Vec::new();
        for (id, view) in self.run_views()? {
            let metadata = self.metadata(&id)?;
            let (graph, created, mut trigger) = metadata
                .as_ref()
                .map(|metadata| (
                    metadata.graph.clone(),
                    metadata.created.clone(),
                    json!({"source":metadata.trigger_source,"schedule":metadata.schedule,"scheduled_at":metadata.scheduled_at}),
                ))
                .unwrap_or_else(|| (
                    "unknown".into(),
                    String::new(),
                    json!({"source":"unknown"}),
                ));
            if let Some(source) = metadata
                .as_ref()
                .and_then(|metadata| metadata.graph_call.as_ref())
            {
                trigger["graph"] = json!(source.parent_graph);
                trigger["run"] = json!(source.parent_run);
                trigger["node"] = json!(source.node);
                trigger["invocation"] = json!(source.invocation);
                trigger["mode"] = json!(source.mode);
                trigger["root_run"] = json!(source.root_run);
            }
            if let Some(source) = metadata
                .as_ref()
                .and_then(|metadata| metadata.conversation.as_ref())
            {
                trigger["session"] = json!(source.session);
                trigger["reply_node"] = json!(source.reply_node);
                trigger["previous_run"] = json!(source.previous_run);
            }
            let progress = view.progress();
            let executed = progress.executed_nodes();
            let projection = self.project_observed_run(&view, metadata.as_ref())?;
            runs.push(json!({"session_call":metadata.as_ref().and_then(|m| m.session_call.as_ref()),"run":id,"graph":graph,"running":projection.active,"projection":projection,"started":created,"updated":projection.updated,"executed":executed,"objective":progress.snapshot.objective,"trigger":trigger}));
        }
        Ok(runs)
    }

    pub(crate) async fn pilot_run_list(&self) -> Result<Value, ApplicationError> {
        let mut runs = Vec::new();
        for (id, view) in self.run_views()? {
            let metadata = self.metadata(&id)?;
            let projection = self.project_observed_run(&view, metadata.as_ref())?;
            runs.push(json!({"run":id,"projection":projection}));
        }
        Ok(json!({"runs":runs}))
    }

    pub(crate) async fn pilot_run_status(&self, run: &str) -> Result<Value, ApplicationError> {
        let view = self.read_run(run)?.ok_or(ApplicationError::Missing)?;
        let metadata = self.metadata(run)?;
        let projection = self.project_observed_run(&view, metadata.as_ref())?;
        let mut progress = serde_json::to_value(view.record.as_ref())
            .map_err(|error| ApplicationError::Storage(error.to_string()))?;
        progress
            .as_object_mut()
            .expect("record")
            .shift_remove("status");
        Ok(json!({"run":run,"record":progress,"projection":projection}))
    }
}
