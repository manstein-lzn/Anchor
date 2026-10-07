use super::{ApplicationError, RunApplication};
use anchor_graph_host::{
    FileGraphBundleLoader, FilePluginCatalog, LoadedGraphBundle, PluginCatalog,
};
use anchor_runtime_rig::graph::{GraphError, GraphSnapshot, PluginBinding};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

mod deletion;

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(crate) enum GraphManagementError {
    BadRequest(String),
    Missing(String),
    Conflict(String),
    Invalid(String),
    Storage(String),
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
        if !valid_graph_name(name) {
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
        let bundle = FileGraphBundleLoader::new(&path).load().map_err(invalid)?;
        Ok((path, bundle))
    }

    pub(crate) fn validate_graph(
        &self,
        definition: &Value,
    ) -> Result<GraphSnapshot, GraphManagementError> {
        let (snapshot, _, _) =
            resolve_graph_definition(definition, &self.catalog_root).map_err(invalid)?;
        crate::reject_snapshot(&snapshot)
            .map_err(GraphError::Unsupported)
            .map_err(invalid)?;
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
                    if valid_graph_name(&name)
                        && !names.contains(&name)
                        && FileGraphBundleLoader::new(entry.path()).load().is_ok()
                    {
                        names.push(name);
                    }
                }
            }
        }
        names.sort();
        let mut graphs = Vec::new();
        for name in names {
            let active_runs = self.active_runs(Some(&name)).await;
            let missing = !self.graph_bundle_path(&name).join("graph.json").is_file();
            graphs.push(json!({"graph":name,"running":active_runs.first(),"active_runs":active_runs,"missing":missing}));
        }
        Ok(json!({"graphs":graphs}))
    }

    pub(crate) fn read_graph(&self, name: &str) -> Result<Value, GraphManagementError> {
        let (_path, bundle) = self.load_graph(name)?;
        let node_plugins = bundle
            .snapshot
            .nodes
            .iter()
            .map(|node| (node.id.clone(), json!(node.plugins)))
            .collect::<serde_json::Map<_, _>>();
        Ok(
            json!({"graph":name,"definition":bundle.authoring_definition,"node_plugins":node_plugins}),
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
        create_graph_bundle_from_catalog(&path, &definition, &self.catalog_root)?;
        Ok(json!({"graph":name,"definition":definition}))
    }

    pub(crate) async fn update_graph(
        &self,
        name: &str,
        definition: Value,
    ) -> Result<Value, GraphManagementError> {
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        let path = self.graph_path_checked(name)?;
        let _graph_lease = self.graph_admission_lease(&path)?;
        let (path, current) = self.load_graph(name)?;
        for (run_id, _) in self.runs_for_graph(&path)? {
            self.reject_pending_session_delivery(&run_id)?;
        }
        let next = anchor_runtime_rig::graph::GraphSnapshot::from_authoring(definition.clone())
            .map_err(invalid)?;
        if (!current.plugins.is_empty() || next.nodes.iter().any(|node| !node.plugins.is_empty()))
            && self.has_unfinished_plugin_run(&path).await?
        {
            return Err(GraphManagementError::Conflict(
                "cannot replace Plugin resources while this Graph has an unfinished Run".into(),
            ));
        }
        write_graph_bundle_from_catalog(&path, &definition, &self.catalog_root)?;
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
        let removed_runs = self.delete_graph(&path, workspace_root).await?;
        Ok(removed_runs)
    }
}

fn valid_graph_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
}

fn invalid(failure: impl std::fmt::Display) -> GraphManagementError {
    GraphManagementError::Invalid(failure.to_string())
}

fn storage(failure: impl std::fmt::Display) -> GraphManagementError {
    GraphManagementError::Storage(failure.to_string())
}

fn library_root(catalog_root: &Path) -> PathBuf {
    std::env::var_os("ANCHOR_RUNNER_LIBRARY_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| catalog_root.to_path_buf())
}

fn resolve_graph_definition(
    definition: &Value,
    catalog_root: &Path,
) -> Result<(GraphSnapshot, FilePluginCatalog, Vec<PluginBinding>), GraphError> {
    let snapshot = GraphSnapshot::from_authoring(definition.clone())?;
    let ids = snapshot
        .nodes
        .iter()
        .flat_map(|node| node.plugins.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let catalog = FilePluginCatalog::new(library_root(catalog_root));
    let bindings = catalog.resolve(&ids)?;
    Ok((snapshot, catalog, bindings))
}

fn create_graph_bundle_from_catalog(
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
        crate::application::metadata::now_nanos(),
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
    anchor_runtime_rig::graph::GraphSnapshot::from_authoring(definition.clone())
        .map_err(invalid)?;
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

fn write_graph_bundle_from_catalog(
    path: &Path,
    definition: &Value,
    catalog_root: &Path,
) -> Result<(), GraphManagementError> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static STAGING_ID: AtomicU64 = AtomicU64::new(0);
    let _library_guard = anchor_library::Library::new(library_root(catalog_root))
        .catalog_read_guard()
        .map_err(|failure| match failure {
            anchor_library::InstallError::CatalogBusy => {
                GraphManagementError::Conflict(failure.to_string())
            }
            _ => GraphManagementError::Storage(failure.to_string()),
        })?;
    let (_, catalog, bindings) =
        resolve_graph_definition(definition, catalog_root).map_err(invalid)?;
    std::fs::create_dir_all(path).map_err(storage)?;

    let staging_id = STAGING_ID.fetch_add(1, Ordering::Relaxed);
    let staged_plugins = path.join(format!(
        ".plugins-stage-{}-{staging_id}",
        std::process::id()
    ));
    let old_plugins = path.join(format!(".plugins-old-{}-{staging_id}", std::process::id()));
    let graph_tmp = path.join(format!(".graph-stage-{}-{staging_id}", std::process::id()));
    let manifest_tmp = path.join(format!(
        ".manifest-stage-{}-{staging_id}",
        std::process::id()
    ));
    for temporary in [&staged_plugins, &old_plugins, &graph_tmp, &manifest_tmp] {
        if temporary.is_dir() {
            std::fs::remove_dir_all(temporary).map_err(storage)?;
        } else if temporary.exists() {
            std::fs::remove_file(temporary).map_err(storage)?;
        }
    }

    let write_result = (|| -> Result<(), String> {
        for binding in &bindings {
            let source = catalog
                .plugin_directory(&binding.id)
                .map_err(|failure| failure.to_string())?;
            let plugin_stage = staged_plugins.join(&binding.id);
            for resource in &binding.resources {
                let relative = Path::new(resource);
                if relative.is_absolute()
                    || relative
                        .components()
                        .any(|component| !matches!(component, std::path::Component::Normal(_)))
                {
                    return Err(format!("Plugin {} has an unsafe resource path", binding.id));
                }
                let source_file = source.join(relative);
                let metadata = std::fs::symlink_metadata(&source_file)
                    .map_err(|failure| failure.to_string())?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(format!(
                        "Plugin {} resource is not a regular file",
                        binding.id
                    ));
                }
                let destination = plugin_stage.join(relative);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent).map_err(|failure| failure.to_string())?;
                }
                std::fs::copy(source_file, destination).map_err(|failure| failure.to_string())?;
            }
        }
        std::fs::write(
            &graph_tmp,
            serde_json::to_vec_pretty(definition).map_err(|failure| failure.to_string())?,
        )
        .map_err(|failure| failure.to_string())?;
        let plugins = bindings
            .iter()
            .map(|binding| {
                json!({
                    "id":binding.id,
                    "digest":binding.digest,
                    "resources":binding.resources,
                    "mcp_servers":binding.mcp_servers
                })
            })
            .collect::<Vec<_>>();
        std::fs::write(
            &manifest_tmp,
            serde_json::to_vec_pretty(&json!({
                "format":1,
                "graph":"graph.json",
                "plugins":plugins
            }))
            .map_err(|failure| failure.to_string())?,
        )
        .map_err(|failure| failure.to_string())?;
        Ok(())
    })();

    if let Err(message) = write_result {
        let _ = std::fs::remove_dir_all(&staged_plugins);
        let _ = std::fs::remove_file(&graph_tmp);
        let _ = std::fs::remove_file(&manifest_tmp);
        return Err(GraphManagementError::Invalid(message));
    }

    let plugin_root = path.join("plugins");
    let had_plugins = plugin_root.exists();
    if had_plugins {
        std::fs::rename(&plugin_root, &old_plugins).map_err(storage)?;
    }
    let publish_result = (|| -> std::io::Result<()> {
        if !bindings.is_empty() {
            std::fs::rename(&staged_plugins, &plugin_root)?;
        }
        std::fs::rename(&graph_tmp, path.join("graph.json"))?;
        std::fs::rename(&manifest_tmp, path.join("manifest.json"))?;
        Ok(())
    })();
    if let Err(failure) = publish_result {
        if plugin_root.exists() {
            let _ = std::fs::remove_dir_all(&plugin_root);
        }
        if had_plugins && old_plugins.exists() {
            let _ = std::fs::rename(&old_plugins, &plugin_root);
        }
        let _ = std::fs::remove_dir_all(&staged_plugins);
        let _ = std::fs::remove_file(&graph_tmp);
        let _ = std::fs::remove_file(&manifest_tmp);
        return Err(storage(failure));
    }
    if had_plugins {
        std::fs::remove_dir_all(&old_plugins).map_err(storage)?;
    }
    Ok(())
}
