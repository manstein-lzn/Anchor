use crate::application::{AdmissionOptions, ApplicationError, RunApplication, new_control};
use crate::execution::PreparedExecution;
use crate::run_metadata::{self as metadata, RunMetadata};
use anchor_graph_host::LoadedGraphBundle;
use anchor_runtime::graph::{GraphRunRecord, RunLease};
use serde_json::Value;
use std::path::Path;

impl RunApplication {
    pub(crate) async fn admit(
        &self,
        graph: String,
        source: &Path,
        mut bundle: LoadedGraphBundle,
        input: Value,
        options: AdmissionOptions,
        graph_lease: Box<dyn RunLease>,
    ) -> Result<String, ApplicationError> {
        if !options.workspace_bindings.is_empty() && options.workspace_root.is_none() {
            return Err(ApplicationError::Invalid(
                "workspace bindings require a configured Host workspace root".into(),
            ));
        }
        options
            .trigger
            .validate()
            .map_err(ApplicationError::Invalid)?;
        if let Some(objective) = options.objective.filter(|value| !value.is_empty()) {
            bundle.snapshot.objective = objective;
        }
        let graph_path = crate::run_metadata::graph_identity(source)?;
        let plugin_lease = if bundle.plugins.is_empty() {
            None
        } else {
            Some(self.plugin_reference_lease().await?)
        };
        let active = self.admission.lock().await;
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
        let run_id = format!("rust-{:x}", metadata::now_nanos());
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
        let mut metadata = RunMetadata::new(
            run_id.clone(),
            graph.clone(),
            record.graph_digest.clone(),
            source,
        )?;
        options.trigger.apply(&mut metadata);
        metadata.pilot = options.pilot;
        metadata.oauth_owner = options.oauth_owner;
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
}
