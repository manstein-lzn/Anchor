use super::{ApplicationError, RunApplication};
use crate::plugin_library::Catalog;
use anchor_graph_host::{FileGraphBundleLoader, LoadedGraphBundle, PluginCatalog};
#[cfg(test)]
use anchor_runtime::graph::PluginBinding;
use anchor_runtime::graph::{GraphError, GraphSnapshot};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

mod deletion;
mod references;
mod updates;

#[cfg(test)]
mod tests;

pub(crate) use references::{ImpactCoverage, ImpactScan, coverage_of};

#[derive(Debug)]
pub(crate) enum GraphManagementError {
    BadRequest(String),
    Missing(String),
    Conflict(String),
    Invalid(String),
    Storage(String),
}

/// Authoring diagnostics are safe to return to the author. Library and Host
/// failures can include private paths, so protocol adapters redact those.
#[derive(Debug)]
pub(crate) enum GraphValidationFailure {
    Authoring(GraphError),
    Unavailable(GraphError),
}

impl std::fmt::Display for GraphValidationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authoring(failure) | Self::Unavailable(failure) => failure.fmt(formatter),
        }
    }
}

impl std::fmt::Display for GraphManagementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadRequest(message)
            | Self::Missing(message)
            | Self::Conflict(message)
            | Self::Invalid(message)
            | Self::Storage(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for GraphManagementError {}

impl From<ApplicationError> for GraphManagementError {
    fn from(failure: ApplicationError) -> Self {
        match failure {
            ApplicationError::Missing => Self::Missing("no such run".into()),
            ApplicationError::Conflict(message) => Self::Conflict(message),
            ApplicationError::Invalid(message) => Self::Invalid(message),
            ApplicationError::Storage(message) => Self::Storage(message),
        }
    }
}

impl RunApplication {
    pub(crate) fn graph_path_checked(&self, name: &str) -> Result<PathBuf, GraphManagementError> {
        if !anchor_runtime::identifier::is_name(name) {
            return Err(GraphManagementError::BadRequest(
                "invalid graph name".into(),
            ));
        }
        Ok(self.graph_bundle_path(name))
    }

    pub(crate) fn load_graph(
        &self,
        name: &str,
    ) -> Result<(PathBuf, LoadedGraphBundle), GraphManagementError> {
        let path = self.graph_path_checked(name)?;
        if !path.exists() {
            return Err(GraphManagementError::Missing("no such graph".into()));
        }
        let bundle = FileGraphBundleLoader::new(&path)
            .with_library_root(self.library_root().to_path_buf())
            .load()
            .map_err(invalid)?;
        Ok((path, bundle))
    }

    pub(crate) fn validate_graph(
        &self,
        definition: &Value,
    ) -> Result<GraphSnapshot, GraphManagementError> {
        self.inspect_graph_definition(definition).map_err(invalid)
    }

    pub(crate) fn inspect_graph_definition(
        &self,
        definition: &Value,
    ) -> Result<GraphSnapshot, GraphValidationFailure> {
        let snapshot = GraphSnapshot::from_authoring(GraphSnapshot::normalize_network_authoring(
            definition.clone(),
            false,
        ))
        .map_err(GraphValidationFailure::Authoring)?;
        for (name, agent) in &snapshot.agents {
            let reference = (!agent.model.is_empty()).then_some(agent.model.as_str());
            let result = if let Some(models) = &self.fixture_models {
                models.resolve(reference).map(|_| ())
            } else if let Some(configuration) = &self.model_configuration {
                configuration.validate_reference(reference)
            } else {
                Ok(())
            };
            result.map_err(|reason| {
                GraphValidationFailure::Authoring(GraphError::InvalidSnapshot(format!(
                    "agent {name:?}: {reason}"
                )))
            })?;
        }
        crate::reject_snapshot(&snapshot)
            .map_err(GraphError::Unsupported)
            .map_err(GraphValidationFailure::Unavailable)?;
        Catalog::new(self.library_root().to_path_buf())
            .resolve_snapshot(&snapshot)
            .map_err(GraphValidationFailure::Unavailable)?;
        // R2: one channel Plugin per persistent assistant.
        super::channel_management::channel_plan(&snapshot, self.library_root()).map_err(|_| {
            GraphValidationFailure::Authoring(GraphError::InvalidSnapshot(
                super::channel_management::TWO_CHANNELS.into(),
            ))
        })?;
        Ok(snapshot)
    }

    pub(crate) async fn list_graphs(&self) -> Result<Value, GraphManagementError> {
        let mut names = self
            .configured_graph
            .as_ref()
            .map(|(name, _)| vec![name.clone()])
            .unwrap_or_default();
        if let Ok(entries) = std::fs::read_dir(&self.catalog_root) {
            for entry in entries.flatten() {
                if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                    if self
                        .configured_graph
                        .as_ref()
                        .is_some_and(|(_, path)| entry.path() == *path)
                    {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if anchor_runtime::identifier::is_name(&name)
                        && !names.contains(&name)
                        && FileGraphBundleLoader::new(entry.path())
                            .with_library_root(self.library_root().to_path_buf())
                            .load()
                            .is_ok()
                    {
                        names.push(name);
                    }
                }
            }
        }
        names.sort();
        let completed_tasks = self.completed_task_graphs(&names);
        let mut graphs = Vec::new();
        for name in names {
            let active_runs = self.active_runs(Some(&name)).await;
            let mut active_nodes = std::collections::BTreeSet::new();
            for run_id in &active_runs {
                if let Some(view) = self.read_run(run_id)? {
                    active_nodes.extend(view.with_active(true).visible_active_nodes());
                }
            }
            let missing = !self.graph_bundle_path(&name).join("graph.json").is_file();
            let mut graph = json!({"graph":name,"running":active_runs.first(),"active_runs":active_runs,"active_nodes":active_nodes,"missing":missing});
            if active_runs.is_empty() && completed_tasks.contains(&name) {
                graph["completed_task"] = json!(true);
            }
            graphs.push(graph);
        }
        Ok(json!({"graphs":graphs}))
    }

    pub(crate) fn read_graph(&self, name: &str) -> Result<Value, GraphManagementError> {
        let (_path, bundle) = self.load_graph(name)?;
        let definition =
            GraphSnapshot::normalize_network_authoring(bundle.authoring_definition, true);
        let projected = GraphSnapshot::from_authoring(definition.clone()).map_err(invalid)?;
        let node_plugins = projected
            .nodes
            .iter()
            .map(|node| (node.id.clone(), json!(node.plugins)))
            .collect::<serde_json::Map<_, _>>();
        Ok(
            json!({"graph":name,"definition":definition,"node_plugins":node_plugins,
            "plugin_activation":updates::activation(&bundle.snapshot)?}),
        )
    }

    pub(crate) async fn create_graph(
        &self,
        name: &str,
        definition: Option<Value>,
    ) -> Result<Value, GraphManagementError> {
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        let path = self.graph_path_checked(name)?;
        let _graph_lease = self.graph_admission_lease(&path)?;
        if path.exists() {
            return Err(GraphManagementError::Conflict(
                "graph already exists".into(),
            ));
        }
        let definition = definition.unwrap_or_else(|| {
            json!({
                "objective": name,
                "entry": "start",
                "agents": {},
                "ops": {"start": {"run": "true"}},
                "nodes": [{"id": "start", "op": "start", "plugins": []}],
                "edges": []
            })
        });
        let definition = GraphSnapshot::normalize_network_authoring(definition, false);
        self.validate_graph(&definition)?;
        if super::graph_tasks::task_graph_ownership_path(self, name)?.exists() {
            return Err(GraphManagementError::Conflict(
                "this Graph name is reserved by a retained task creation".into(),
            ));
        }
        create_graph_bundle_from_catalog(&path, &definition, self.library_root())?;
        Ok(json!({"graph":name,"definition":definition}))
    }

    pub(crate) async fn remove_graph(
        &self,
        name: &str,
        workspace_root: &Path,
    ) -> Result<usize, GraphManagementError> {
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        let path = self.graph_path_checked(name)?;
        self.load_graph(name)?;
        let removed_runs = self.delete_graph(name, &path, workspace_root).await?;
        Ok(removed_runs)
    }
}

fn invalid(failure: impl std::fmt::Display) -> GraphManagementError {
    GraphManagementError::Invalid(failure.to_string())
}

fn storage(failure: impl std::fmt::Display) -> GraphManagementError {
    GraphManagementError::Storage(failure.to_string())
}

/// The current bindings of the Plugins one Graph bundle references.
///
/// A bundle carries ids only, so every consumer that needs the code a Run will
/// actually use resolves it here — from the Library.
#[cfg(test)]
pub(crate) fn referenced_plugin_bindings(
    library_root: &Path,
    ids: &[String],
) -> Result<std::collections::BTreeMap<String, PluginBinding>, GraphError> {
    Ok(Catalog::new(library_root)
        .resolve(ids)?
        .into_iter()
        .map(|binding| (binding.id.clone(), binding))
        .collect())
}

fn resolve_graph_definition(
    definition: &Value,
    catalog_root: &Path,
) -> Result<GraphSnapshot, GraphError> {
    let snapshot = GraphSnapshot::from_authoring(definition.clone())?;
    let catalog = Catalog::new(catalog_root.to_path_buf());
    catalog.resolve_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn snapshot_plugin_ids(snapshot: &GraphSnapshot) -> Vec<String> {
    snapshot
        .nodes
        .iter()
        .flat_map(|node| node.plugins.iter().map(|reference| reference.id.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(super) fn create_graph_bundle_from_catalog(
    path: &Path,
    definition: &Value,
    catalog_root: &Path,
) -> Result<(), GraphManagementError> {
    let parent = path
        .parent()
        .ok_or_else(|| GraphManagementError::BadRequest("graph path has no parent".into()))?;
    crate::create_durable_directory(parent).map_err(storage)?;
    let staged = parent.join(format!(
        ".graph-create-{}-{}~",
        std::process::id(),
        crate::run_metadata::now_nanos(),
    ));
    std::fs::create_dir(&staged).map_err(storage)?;
    let prepared = (|| {
        write_graph_bundle_from_catalog(&staged, definition, catalog_root)?;
        FileGraphBundleLoader::new(&staged)
            .load()
            .map_err(invalid)?;
        sync_bundle_directory(&staged).map_err(storage)?;
        if path.exists() {
            return Err(GraphManagementError::Conflict(
                "graph already exists".into(),
            ));
        }
        std::fs::rename(&staged, path).map_err(storage)?;
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(storage)
    })();
    if staged.exists() {
        let _ = std::fs::remove_dir_all(staged);
    }
    prepared
}

fn sync_bundle_directory(path: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_bundle_directory(&entry.path())?;
        } else {
            std::fs::File::open(entry.path())?.sync_all()?;
        }
    }
    std::fs::File::open(path)?.sync_all()
}

#[cfg(test)]
pub(crate) fn write_graph_bundle(
    path: &Path,
    definition: &Value,
) -> Result<(), GraphManagementError> {
    anchor_runtime::graph::GraphSnapshot::from_authoring(definition.clone()).map_err(invalid)?;
    std::fs::create_dir_all(path).map_err(storage)?;
    let graph_path = path.join("graph.json");
    let manifest_path = path.join("manifest.json");
    let graph_tmp = path.join(".graph.json.tmp");
    let manifest_tmp = path.join(".manifest.json.tmp");
    std::fs::write(
        &graph_tmp,
        serde_json::to_vec_pretty(definition).map_err(storage)?,
    )
    .map_err(storage)?;
    std::fs::write(
        &manifest_tmp,
        br#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .map_err(storage)?;
    std::fs::rename(graph_tmp, graph_path).map_err(storage)?;
    std::fs::rename(manifest_tmp, manifest_path).map_err(storage)?;
    Ok(())
}

pub(super) fn write_graph_bundle_from_catalog(
    path: &Path,
    definition: &Value,
    catalog_root: &Path,
) -> Result<(), GraphManagementError> {
    let _library_guard = crate::plugin_library::PluginLibrary::new(catalog_root.to_path_buf())
        .catalog_read_guard()
        .map_err(|failure| match failure {
            anchor_library::InstallError::CatalogBusy => {
                GraphManagementError::Conflict(failure.to_string())
            }
            _ => GraphManagementError::Storage(failure.to_string()),
        })?;
    // Resolving here is a validation of the export, not a copy: the Plugin stays
    // in the Library, which is the only place it exists.
    let snapshot = resolve_graph_definition(definition, catalog_root).map_err(invalid)?;
    std::fs::create_dir_all(path).map_err(storage)?;
    // Do not mutate Plugin resources here. In a supported single-root deployment
    // path/plugins is the canonical Library, not a stale bundle copy. Other
    // embedded Plugin copies are rejected by load_graph before an update.
    let graph_tmp = path.join(".graph.json.tmp");
    let manifest_tmp = path.join(".manifest.json.tmp");
    let references = snapshot_plugin_ids(&snapshot)
        .into_iter()
        .map(|id| json!({"id": id}))
        .collect::<Vec<_>>();
    let write_result = (|| -> Result<(), String> {
        std::fs::write(
            &graph_tmp,
            serde_json::to_vec_pretty(definition).map_err(|failure| failure.to_string())?,
        )
        .map_err(|failure| failure.to_string())?;
        std::fs::write(
            &manifest_tmp,
            serde_json::to_vec_pretty(&json!({
                "format": 1,
                "graph": "graph.json",
                "plugins": references
            }))
            .map_err(|failure| failure.to_string())?,
        )
        .map_err(|failure| failure.to_string())?;
        std::fs::File::open(&graph_tmp)
            .and_then(|file| file.sync_all())
            .map_err(|failure| failure.to_string())?;
        std::fs::File::open(&manifest_tmp)
            .and_then(|file| file.sync_all())
            .map_err(|failure| failure.to_string())?;
        Ok(())
    })();
    if let Err(message) = write_result {
        let _ = std::fs::remove_file(&graph_tmp);
        let _ = std::fs::remove_file(&manifest_tmp);
        return Err(GraphManagementError::Invalid(message));
    }
    std::fs::rename(&graph_tmp, path.join("graph.json")).map_err(storage)?;
    std::fs::rename(&manifest_tmp, path.join("manifest.json")).map_err(storage)?;
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(storage)?;
    Ok(())
}
