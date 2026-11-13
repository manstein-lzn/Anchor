use crate::application::{AdmissionOptions, ApplicationError, RunApplication, new_control};
use crate::execution::PreparedExecution;
use crate::run_metadata::{self as metadata, RunMetadata};
use anchor_graph_host::LoadedGraphBundle;
use anchor_runtime::graph::{GraphRunRecord, RunLease};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

mod experiment;
pub(in crate::application) use experiment::ValidatedExperimentBundle;

enum AdmissionReplay {
    Absent,
    Incomplete(Box<RunMetadata>),
    Accepted(String),
}

enum AdmissionOrigin {
    Ordinary(Option<crate::task_delegation::TaskOrigin>),
    Dreaming(metadata::DreamingRunContext),
    Experiment {
        context: metadata::ExperimentRunContext,
        bundle_digest: String,
        authority: Option<Box<crate::application::dreaming::experiments::ExperimentAuthority>>,
    },
}

impl AdmissionOrigin {
    fn task(&self) -> Option<&crate::task_delegation::TaskOrigin> {
        match self {
            Self::Ordinary(task) => task.as_ref(),
            _ => None,
        }
    }

    fn dreaming(&self) -> Option<&metadata::DreamingRunContext> {
        match self {
            Self::Dreaming(context) => Some(context),
            _ => None,
        }
    }

    fn experiment(&self) -> Option<&metadata::ExperimentRunContext> {
        match self {
            Self::Experiment { context, .. } => Some(context),
            _ => None,
        }
    }
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
            AdmissionOrigin::Ordinary(None),
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
        let origin = AdmissionOrigin::Ordinary(task);
        if let Some(request) = &request {
            let _active = self.admission.lock().await;
            if let AdmissionReplay::Accepted(run) = self.admission_replay(
                &admission_run_id(Some(request)),
                graph,
                &path,
                &options,
                Some(request),
                &origin,
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
            origin,
        )
        .await
    }

    pub(in crate::application) async fn admit_dreaming_graph_request(
        &self,
        request_id: &str,
        trigger: metadata::RunTrigger,
        context: metadata::DreamingRunContext,
    ) -> Result<String, ApplicationError> {
        let input = serde_json::json!({"cycle":context.cycle});
        let options = AdmissionOptions {
            objective: None,
            trigger,
            pilot: None,
            oauth_owner: None,
            workspace_bindings: Default::default(),
            workspace_root: None,
        };
        let request = admission_request(request_id.into(), "dreaming", &input, &None);
        let path = self.graph_path_checked("dreaming").map_err(graph_error)?;
        let origin = AdmissionOrigin::Dreaming(context);
        {
            let _active = self.admission.lock().await;
            if let AdmissionReplay::Accepted(run) = self.admission_replay(
                &admission_run_id(Some(&request)),
                "dreaming",
                &path,
                &options,
                Some(&request),
                &origin,
            )? {
                return Ok(run);
            }
        }
        let lease = self.graph_admission_lease(&path)?;
        let (path, bundle) = self.load_graph("dreaming").map_err(graph_error)?;
        self.admit_with_identity(
            "dreaming".into(),
            &path,
            bundle,
            input,
            options,
            lease,
            Some(request),
            origin,
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
        origin: AdmissionOrigin,
    ) -> Result<String, ApplicationError> {
        if let Some(objective) = options.objective.as_ref().filter(|value| !value.is_empty()) {
            bundle.snapshot.objective = objective.clone();
        }
        let graph_path = crate::run_metadata::graph_identity(source)?;
        if origin.experiment().is_some() {
            Self::validate_experiment_snapshot(&bundle.snapshot)?;
        }
        // Closed assignments have only a denied network marker, never a live
        // Plugin reference to publish. Its exclusive lease would also prevent
        // the independent target recheck from taking its ordinary read lease.
        let plugin_lease = if origin.experiment().is_some()
            || (bundle.plugins.is_empty() && origin.task().is_none())
        {
            None
        } else {
            Some(self.plugin_reference_lease().await?)
        };
        let active = self.admission.lock().await;
        if let AdmissionOrigin::Experiment {
            authority: Some(authority),
            ..
        } = &origin
        {
            authority.recheck(self, &input)?;
            self.frozen_experiment()
                .ok_or_else(|| {
                    ApplicationError::Invalid(
                        "experiment execution capability is unavailable".into(),
                    )
                })?
                .verify()
                .map_err(ApplicationError::Invalid)?;
        }
        let run_id = admission_run_id(request.as_ref());
        let existing = match self.admission_replay(
            &run_id,
            &graph,
            source,
            &options,
            request.as_ref(),
            &origin,
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
        if let Some(task) = origin.task() {
            self.authorize_delegated_snapshot(task, &graph, &bundle.snapshot)?;
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
        let plugin_bindings = match plugin_lease.as_ref() {
            Some(lease) => crate::plugin_library::Catalog::new(self.library_root())
                .resolve_snapshot_locked(&bundle.snapshot, lease)?
                .into_iter()
                .map(|binding| (binding.id.clone(), binding))
                .collect(),
            None => self.bundle_plugin_bindings(&bundle)?,
        };
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
        let execution = match origin.dreaming() {
            Some(context) => PreparedExecution::prepare_dreaming(
                &record,
                control.clone(),
                self.clone(),
                source,
                context,
            ),
            None => PreparedExecution::prepare_with_catalog(
                &record,
                control.clone(),
                record.plugin_bindings.clone(),
                self.clone(),
            ),
        }
        .map_err(ApplicationError::Invalid)?;
        if origin.experiment().is_some() {
            execution
                .validate_experiment_inputs()
                .map_err(ApplicationError::Invalid)?;
        }
        execution
            .bind_local_inputs(&record, &graph)
            .map_err(ApplicationError::Invalid)?;
        self.check_store()?;
        let first_admission = existing.is_none();
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
        // Incomplete admission replay preserves its original publication grant,
        // including absence; later sharing never grants historical authority.
        if first_admission {
            metadata.schedule_publication =
                self.schedule_publication_at_admission(&metadata)?;
        }
        match origin {
            AdmissionOrigin::Ordinary(task) => metadata.task = task,
            AdmissionOrigin::Dreaming(context) => metadata.dreaming = Some(context),
            AdmissionOrigin::Experiment { context, .. } => metadata.experiment = Some(context),
        }
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

    #[allow(clippy::too_many_arguments)]
    fn admission_replay(
        &self,
        run_id: &str,
        graph: &str,
        source: &Path,
        options: &AdmissionOptions,
        request: Option<&metadata::AdmissionRequest>,
        origin: &AdmissionOrigin,
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
            || existing.task.as_ref() != origin.task()
            || existing.dreaming.as_ref() != origin.dreaming()
            || existing.experiment.as_ref() != origin.experiment()
            || existing.oauth_owner != options.oauth_owner
            || existing.pilot != options.pilot
            || !options.trigger.matches_metadata(&existing)
            || (origin.experiment().is_some() && existing.bundle_source != source)
        {
            return Err(ApplicationError::Conflict(
                "admission request identity changed".into(),
            ));
        }
        if let Some(run) = self.read_run(run_id)? {
            let content_matches = request.is_some_and(|request| {
                *request
                    == match origin {
                        AdmissionOrigin::Experiment {
                            context,
                            bundle_digest,
                            ..
                        } => experiment::admission_request(
                            request.id.clone(),
                            graph,
                            &run.record.input,
                            context,
                            bundle_digest,
                        ),
                        _ => admission_request(
                            request.id.clone(),
                            graph,
                            &run.record.input,
                            &options.objective,
                        ),
                    }
            });
            if run.progress().graph_digest != existing.graph_digest
                || !content_matches
                || origin.experiment().is_some_and(|context| {
                    context.snapshot_digest != existing.graph_digest
                        || context.input_digest != experiment::input_digest(&run.record.input)
                })
            {
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
        |request| request_run_id(&request.id),
    )
}

pub(in crate::application) fn request_run_id(request_id: &str) -> String {
    anchor_facts::experiment::run_id(request_id)
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
