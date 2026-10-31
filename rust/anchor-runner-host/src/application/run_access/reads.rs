use super::RunView;
use crate::application::{ApplicationError, RunApplication, storage};
use anchor_runtime::graph::{GraphError, GraphRunRecord};
use serde_json::Value;
use std::path::Path;

fn reject_legacy(data_root: &Path, run_id: &str) -> Result<(), GraphError> {
    if !anchor_runtime::identifier::is_name(run_id) {
        return Err(GraphError::InvalidRunId(run_id.into()));
    }
    match std::fs::symlink_metadata(data_root.join("runs").join(format!("{run_id}.json"))) {
        Ok(_) => Err(GraphError::CorruptRun(format!(
            "legacy_run_record: Run {run_id} uses the retired record format; remove local development state before continuing"
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn load(data_root: &Path, run_id: &str) -> Result<Option<RunView<'static>>, GraphError> {
    use anchor_kernel::Store;
    reject_legacy(data_root, run_id)?;
    let root = data_root.join("kernel-runs");
    if !root.exists() {
        return Ok(None);
    }
    let store = anchor_kernel::FileStore::new(&root)
        .map_err(|error| GraphError::CorruptRun(error.to_string()))?;
    let Some(resource) = store
        .load(&anchor_runtime::graph::validator::run_resource_id(run_id))
        .map_err(|error| GraphError::CorruptRun(error.to_string()))?
    else {
        return Ok(None);
    };
    let record = anchor_runtime::graph::validator::record_of(&resource)?;
    record.validate()?;
    let updated =
        super::run_observations::RunObservations::new(&data_root.join("kernel-run-observations"))
            .load(run_id)
            .ok()
            .flatten();
    let lifecycle = super::kernel_runs::KernelRuns::project(&resource, false, updated)
        .map_err(|error| GraphError::CorruptRun(format!("Run view: {error:?}")))?;
    let history = store
        .history(&resource.id)
        .map_err(|error| GraphError::CorruptRun(error.to_string()))?;
    let (recovery, _) = super::kernel_runs::KernelRuns::project_recovery(&resource, history)
        .map_err(|error| GraphError::CorruptRun(format!("Run recovery view: {error:?}")))?;
    let mut view = RunView::from_kernel(record, lifecycle).with_kernel_frontier(&resource)?;
    view.recovery = recovery;
    Ok(Some(view))
}

pub(crate) fn status_projection(
    data_root: &Path,
    run_id: &str,
) -> Result<Option<Value>, GraphError> {
    load(data_root, run_id)?
        .map(|view| {
            serde_json::to_value(super::projection::RunProjection::from_view(&view.lifecycle))
                .map_err(|error| GraphError::CorruptRun(error.to_string()))
        })
        .transpose()
}

impl RunApplication {
    pub(crate) fn durable_input_workspaces(
        &self,
        workspace_root: &Path,
    ) -> Result<Vec<std::path::PathBuf>, ApplicationError> {
        let workspaces = crate::workspaces::Workspaces::new(workspace_root.into());
        let mut paths = std::collections::BTreeSet::new();
        for (_, record) in self.raw_records()? {
            for node in &record.snapshot.nodes {
                let operation = node
                    .op
                    .as_ref()
                    .and_then(|name| record.snapshot.ops.get(name))
                    .and_then(|op| op.get("host"))
                    .and_then(|host| host.get("operation"))
                    .and_then(serde_json::Value::as_str);
                if operation != Some(crate::assistant::WAIT_INPUT) {
                    continue;
                }
                let key = anchor_runtime::graph::InvocationKey {
                    run_id: record.run_id.clone(),
                    graph_digest: record.graph_digest.clone(),
                    node_id: node.id.clone(),
                    invocation: 1,
                };
                paths.insert(workspaces.path(&key)?);
            }
        }
        Ok(paths.into_iter().collect())
    }

    pub(in crate::application::run_access) fn reject_legacy_run(
        &self,
        run_id: &str,
    ) -> Result<(), ApplicationError> {
        Ok(reject_legacy(&self.data_root, run_id)?)
    }

    pub(crate) fn run_updated(&self, run_id: &str) -> Result<Option<String>, ApplicationError> {
        self.read_run(run_id)
            .map(|view| view.and_then(|view| view.lifecycle.updated))
    }

    fn run_ids(&self) -> Result<Vec<String>, ApplicationError> {
        let root = self.data_root.join("runs");
        match std::fs::read_dir(root) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(storage)?;
                    if let Some(id) = entry
                        .file_name()
                        .to_str()
                        .and_then(|name| name.strip_suffix(".json"))
                    {
                        self.reject_legacy_run(id)?;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
        self.kernel_run_ids()
    }

    pub(in crate::application::run_access) fn raw_records(
        &self,
    ) -> Result<Vec<(String, GraphRunRecord)>, ApplicationError> {
        let mut records = Vec::new();
        for id in self.run_ids()? {
            if let Some(record) = self.load_run_impl(&id)? {
                records.push((id, record));
            }
        }
        Ok(records)
    }

    pub(in crate::application::run_access) fn raw_runs_for_graph(
        &self,
        graph_path: &Path,
    ) -> Result<Vec<(String, GraphRunRecord)>, ApplicationError> {
        let mut runs = Vec::new();
        for (id, record) in self.raw_records()? {
            if let Some(metadata) = self.metadata(&id)?
                && crate::run_metadata::graph_identity(&metadata.bundle_source)? == graph_path
            {
                runs.push((id, record));
            }
        }
        Ok(runs)
    }

    pub(crate) fn read_run(
        &self,
        run_id: &str,
    ) -> Result<Option<RunView<'static>>, ApplicationError> {
        let Some((owner, resource)) = self.kernel_resource(run_id)? else {
            return Ok(None);
        };
        let lifecycle = owner.view(&resource)?;
        let record = super::kernel_runs::KernelRuns::record(&resource)?;
        let (recovery, _) = owner.resource_recovery(&resource)?;
        let mut view = RunView::from_kernel(record, lifecycle).with_kernel_frontier(&resource)?;
        view.recovery = recovery;
        view.session_coordinated = self
            .metadata(run_id)?
            .is_some_and(|metadata| metadata.session_call.is_some());
        Ok(Some(view))
    }

    pub(crate) fn run_views(&self) -> Result<Vec<(String, RunView<'static>)>, ApplicationError> {
        let mut views = Vec::new();
        for id in self.run_ids()? {
            if let Some(view) = self.read_run(&id)? {
                views.push((id, view));
            }
        }
        Ok(views)
    }

    pub(crate) fn runs_for_graph(
        &self,
        path: &Path,
    ) -> Result<Vec<(String, RunView<'static>)>, ApplicationError> {
        self.raw_runs_for_graph(path)?
            .into_iter()
            .map(|(id, _)| {
                self.read_run(&id)?
                    .map(|view| (id, view))
                    .ok_or(ApplicationError::Missing)
            })
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn records(&self) -> Result<Vec<(String, GraphRunRecord)>, ApplicationError> {
        self.raw_records()
    }
}

impl RunView<'_> {
    pub(crate) fn visible_active_nodes(&self) -> std::collections::BTreeSet<String> {
        if !self.active || !matches!(self.phase(), super::RunPhase::Executing) {
            return Default::default();
        }
        self.frontier
            .iter()
            .filter(|dispatch| {
                self.record
                    .snapshot
                    .nodes
                    .iter()
                    .find(|node| node.id == dispatch.node_id)
                    .and_then(|node| node.op.as_ref())
                    .and_then(|name| self.record.snapshot.ops.get(name))
                    .and_then(|op| op.get("host"))
                    .and_then(|host| host.get("operation"))
                    .and_then(serde_json::Value::as_str)
                    != Some(crate::assistant::WAIT_INPUT)
            })
            .map(|dispatch| dispatch.node_id.clone())
            .collect()
    }
}
