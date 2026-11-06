use crate::application::{AdmissionOptions, ApplicationError, RunApplication, new_control};
use crate::execution::PreparedExecution;
use crate::run_metadata::{self as metadata, RunMetadata};
use anchor_graph_host::LoadedGraphBundle;
use anchor_runtime::graph::{GraphRunRecord, RunLease};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

enum AdmissionReplay {
    Absent,
    Incomplete(Box<RunMetadata>),
    Accepted(String),
}

impl RunApplication {
    #[cfg(test)]
    pub(crate) async fn admit(
        &self,
        graph: String,
        source: &Path,
        bundle: LoadedGraphBundle,
        input: Value,
        options: AdmissionOptions,
        graph_lease: Box<dyn RunLease>,
    ) -> Result<String, ApplicationError> {
        self.admit_with_identity(
            graph,
            source,
            bundle,
            input,
            options,
            graph_lease,
            None,
            None,
        )
        .await
    }

    pub(crate) async fn admit_graph_request(
        &self,
        graph: &str,
        input: Value,
        objective: Option<String>,
        trigger: crate::run_metadata::RunTrigger,
        owner: Option<String>,
        request_id: Option<String>,
    ) -> Result<String, ApplicationError> {
        let request = request_id.map(|id| admission_request(id, graph, &input, &objective));
        self.admit_graph_with_options(
            graph,
            input,
            AdmissionOptions {
                objective,
                trigger,
                pilot: None,
                oauth_owner: owner,
                workspace_bindings: Default::default(),
                workspace_root: None,
            },
            request,
            None,
        )
        .await
    }

    pub(crate) async fn admit_graph_with_options(
        &self,
        graph: &str,
        input: Value,
        options: AdmissionOptions,
        request: Option<metadata::AdmissionRequest>,
        task: Option<crate::task_delegation::TaskOrigin>,
    ) -> Result<String, ApplicationError> {
        if request.as_ref().is_some_and(|request| {
            *request != admission_request(request.id.clone(), graph, &input, &options.objective)
        }) {
            return Err(ApplicationError::Conflict(
                "admission request content does not match its input".into(),
            ));
        }
        let path = self.graph_path_checked(graph).map_err(graph_error)?;
        if let Some(request) = &request {
            let _active = self.admission.lock().await;
            if let AdmissionReplay::Accepted(run) = self.admission_replay(
                &admission_run_id(Some(request)),
                graph,
                &path,
                &options,
                Some(request),
                task.as_ref(),
            )? {
                return Ok(run);
            }
        }
        let lease = self.graph_admission_lease(&path)?;
        let (path, bundle) = self.load_graph(graph).map_err(graph_error)?;
        self.admit_with_identity(
            graph.into(),
            &path,
            bundle,
            input,
            options,
            lease,
            request,
            task,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn admit_with_identity(
        &self,
        graph: String,
        source: &Path,
        mut bundle: LoadedGraphBundle,
        input: Value,
        options: AdmissionOptions,
        graph_lease: Box<dyn RunLease>,
        request: Option<metadata::AdmissionRequest>,
        task: Option<crate::task_delegation::TaskOrigin>,
    ) -> Result<String, ApplicationError> {
        if let Some(objective) = options.objective.as_ref().filter(|value| !value.is_empty()) {
            bundle.snapshot.objective = objective.clone();
        }
        let graph_path = crate::run_metadata::graph_identity(source)?;
        let plugin_lease = if bundle.plugins.is_empty() && task.is_none() {
            None
        } else {
            Some(self.plugin_reference_lease().await?)
        };
        let active = self.admission.lock().await;
        let run_id = admission_run_id(request.as_ref());
        let existing = match self.admission_replay(
            &run_id,
            &graph,
            source,
            &options,
            request.as_ref(),
            task.as_ref(),
        )? {
            AdmissionReplay::Accepted(run) => return Ok(run),
            AdmissionReplay::Incomplete(existing) => {
                if existing.graph_digest != bundle.snapshot.digest()? {
                    return Err(ApplicationError::Conflict(
                    "Graph changed during an incomplete admission; inspect the retained request".into(),
                ));
                }
                Some(*existing)
            }
            AdmissionReplay::Absent => None,
        };
        if let Some(origin) = &task {
            self.authorize_delegated_snapshot(origin, &graph, &bundle.snapshot)?;
        }
        // A service restart does not silently launch another Run over an orphan.
        for (id, record) in self.run_views()? {
            if !record.is_settled() {
                match self.metadata(&id)? {
                    Some(meta)
                        if crate::run_metadata::graph_identity(&meta.bundle_source)? == graph_path =>
                    {
                        return Err(ApplicationError::Conflict(
                            "this graph has an unfinished Run; resume it, or stop and delete its record".into(),
                        ));
                    }
                    None => return Err(ApplicationError::Conflict(
                        "an unfinished Run has no immutable Graph identity; admission is quarantined".into(),
                    )),
                    Some(_) => {}
                }
            }
        }
        let plugin_bindings = self.bundle_plugin_bindings(&bundle)?;
        let record = GraphRunRecord::create_with_plugins(
            bundle.snapshot,
            input,
            run_id.clone(),
            plugin_bindings.into_values().collect(),
        )?;
        // The admission observation and the durable reference are published
        // under one Library lease. Node dispatch resolves the current Plugin
        // again so an update can take effect at the next invocation.
        let control = new_control();
        let execution = PreparedExecution::prepare_with_catalog(
            &record,
            control.clone(),
            record.plugin_bindings.clone(),
            self.clone(),
        )
        .map_err(ApplicationError::Invalid)?;
        execution
            .bind_local_inputs(&record, &graph)
            .map_err(ApplicationError::Invalid)?;
        self.check_store()?;
        let mut metadata = existing.unwrap_or(RunMetadata::new(
            run_id.clone(),
            graph.clone(),
            record.graph_digest.clone(),
            source,
        )?);
        options.trigger.apply(&mut metadata);
        metadata.pilot = options.pilot;
        metadata.oauth_owner = options.oauth_owner;
        metadata.request = request;
        metadata.task = task;
        if metadata.task_grants.is_none()
            && record
                .snapshot
                .nodes
                .iter()
                .flat_map(|node| &node.plugins)
                .any(|plugin| plugin.graph_tasks.is_some())
        {
            metadata.task_grants = crate::assistant_plugins::publication(
                &self.data_root,
                source,
                &record.graph_digest,
            )
            .map_err(ApplicationError::Invalid)?
            .map(|receipt| receipt.task_grants());
        }
        let runs = self.kernel_runs(true)?.expect("Run store initialized");
        metadata::save(&self.data_root, &metadata)?;
        runs.create(&record, u64::MAX)?;
        if !options.workspace_bindings.is_empty() {
            let workspaces = crate::workspaces::Workspaces::new(
                options
                    .workspace_root
                    .expect("workspace bindings have a Host workspace root"),
            );
            self.bind_admission_workspaces(&record, &options.workspace_bindings, &workspaces)?;
        }
        drop(active);
        drop(plugin_lease);
        drop(graph_lease);
        self.spawn_kernel(record, execution, control)?;
        Ok(run_id)
    }

    fn admission_replay(
        &self,
        run_id: &str,
        graph: &str,
        source: &Path,
        options: &AdmissionOptions,
        request: Option<&metadata::AdmissionRequest>,
        task: Option<&crate::task_delegation::TaskOrigin>,
    ) -> Result<AdmissionReplay, ApplicationError> {
        if !options.workspace_bindings.is_empty() && options.workspace_root.is_none() {
            return Err(ApplicationError::Invalid(
                "workspace bindings require a configured Host workspace root".into(),
            ));
        }
        options
            .trigger
            .validate()
            .map_err(ApplicationError::Invalid)?;
        let Some(existing) = self.metadata(run_id)? else {
            if request.is_some() && self.read_run(run_id)?.is_some() {
                return Err(ApplicationError::Conflict(
                    "an admitted request has no immutable identity metadata".into(),
                ));
            }
            return Ok(AdmissionReplay::Absent);
        };
        if request.is_none()
            || existing.request.as_ref() != request
            || existing.graph != graph
            || metadata::graph_identity(&existing.bundle_source)?
                != metadata::graph_identity(source)?
            || existing.task.as_ref() != task
            || existing.oauth_owner != options.oauth_owner
            || existing.pilot != options.pilot
            || !options.trigger.matches_metadata(&existing)
        {
            return Err(ApplicationError::Conflict(
                "admission request identity changed".into(),
            ));
        }
        if let Some(run) = self.read_run(run_id)? {
            let content_matches = request.is_some_and(|request| {
                *request
                    == admission_request(
                        request.id.clone(),
                        graph,
                        &run.record.input,
                        &options.objective,
                    )
            });
            if run.progress().graph_digest != existing.graph_digest || !content_matches {
                return Err(ApplicationError::Conflict(
                    "admitted Run and its immutable identity metadata disagree".into(),
                ));
            }
            // Observing an accepted fact never starts or resumes its worker.
            return Ok(AdmissionReplay::Accepted(existing.run_id));
        }
        Ok(AdmissionReplay::Incomplete(Box::new(existing)))
    }
}

fn admission_run_id(request: Option<&metadata::AdmissionRequest>) -> String {
    request.map_or_else(
        || format!("rust-{:x}", metadata::now_nanos()),
        |request| format!("request-{:x}", Sha256::digest(request.id.as_bytes())),
    )
}

pub(crate) fn admission_request(
    id: String,
    graph: &str,
    input: &Value,
    objective: &Option<String>,
) -> metadata::AdmissionRequest {
    let bytes =
        serde_json::to_vec(&(graph, input, objective)).expect("admission input is serializable");
    metadata::AdmissionRequest {
        id,
        content_digest: format!("{:x}", Sha256::digest(bytes)),
    }
}

fn graph_error(error: crate::application::graphs::GraphManagementError) -> ApplicationError {
    use crate::application::graphs::GraphManagementError;
    match error {
        GraphManagementError::Missing(_) => ApplicationError::Missing,
        GraphManagementError::Conflict(message) => ApplicationError::Conflict(message),
        GraphManagementError::Storage(message) => ApplicationError::Storage(message),
        GraphManagementError::BadRequest(message) | GraphManagementError::Invalid(message) => {
            ApplicationError::Invalid(message)
        }
    }
}

#[cfg(test)]
mod tests;
