use super::*;
use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use anchor_runtime_rig::graph::{GraphError, GraphSnapshot, PluginBinding};

pub(super) async fn validate_graph(
    State(state): State<ApiState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let definition = match body {
        Ok(Json(body)) => match body.get("definition").filter(|value| value.is_object()) {
            Some(definition) => definition.clone(),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"valid":false,"error":"definition must be a Graph object"})),
                );
            }
        },
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({"valid":false,"error":"body must be JSON containing a definition object"}),
                ),
            );
        }
    };
    let validation =
        resolve_graph_definition(&definition, &state.catalog_root).and_then(|(snapshot, _, _)| {
            crate::reject_snapshot(&snapshot).map_err(GraphError::Unsupported)?;
            Ok(snapshot)
        });
    match validation {
        Ok(snapshot) => (
            StatusCode::OK,
            Json(json!({
                "valid":true,
                "nodes":snapshot.nodes.iter().map(|node| &node.id).collect::<Vec<_>>(),
                "entry":snapshot.entry,
            })),
        ),
        Err(failure) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"valid":false,"error":failure.to_string()})),
        ),
    }
}

fn resolve_graph_definition(
    definition: &Value,
    catalog_root: &std::path::Path,
) -> Result<(GraphSnapshot, FilePluginCatalog, Vec<PluginBinding>), GraphError> {
    let snapshot = GraphSnapshot::from_authoring(definition.clone())?;
    let ids = snapshot
        .nodes
        .iter()
        .flat_map(|node| node.plugins.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    // Resolve only installed resource identities; transport/env expansion belongs to execution.
    let catalog = FilePluginCatalog::new(library_root(catalog_root));
    let bindings = catalog.resolve(&ids)?;
    Ok((snapshot, catalog, bindings))
}

pub(super) fn graph_name(state: &ApiState) -> String {
    state.graph_name.clone()
}

#[allow(clippy::result_large_err)]
pub(super) fn graph_path(state: &ApiState, name: &str) -> Result<PathBuf, HttpResponse> {
    if !valid_graph_name(name) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid graph name"));
    }
    // The configured Graph name resolves to its deployment bundle root; every
    // other name resolves inside the catalog root. Aliasing the same directory
    // under two names must still yield one identity for locks and deletion.
    Ok(state.application.graph_bundle_path(name))
}

fn valid_graph_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

#[allow(clippy::result_large_err)]
pub(super) fn load_graph_definition(
    state: &ApiState,
    name: &str,
) -> Result<(PathBuf, anchor_graph_host::LoadedGraphBundle), HttpResponse> {
    let path = graph_path(state, name)?;
    if !path.exists() {
        return Err(error(StatusCode::NOT_FOUND, "no such graph"));
    }
    let bundle = FileGraphBundleLoader::new(&path)
        .load()
        .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    Ok((path, bundle))
}

pub(super) async fn graphs(State(state): State<ApiState>) -> Result<Json<Value>, HttpResponse> {
    let mut names = vec![graph_name(&state)];
    if let Ok(entries) = std::fs::read_dir(&state.catalog_root) {
        for entry in entries.flatten() {
            if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                if entry.path() == state.bundle_root {
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
        let active_runs = state.application.active_runs(Some(&name)).await;
        let missing = !state
            .application
            .graph_bundle_path(&name)
            .join("graph.json")
            .is_file();
        graphs.push(json!({"graph":name,"running":active_runs.first(),"active_runs":active_runs,"missing":missing}));
    }
    Ok(Json(json!({"graphs":graphs})))
}

pub(super) async fn graph(
    State(state): State<ApiState>,
    AxumPath(requested): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    let (_path, bundle) = load_graph_definition(&state, &requested)?;
    let node_plugins = bundle
        .snapshot
        .nodes
        .iter()
        .map(|node| (node.id.clone(), json!(node.plugins)))
        .collect::<serde_json::Map<_, _>>();
    Ok(Json(
        json!({"graph":requested,"definition":bundle.authoring_definition,"node_plugins":node_plugins}),
    ))
}

pub(super) async fn create_graph(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "name is required"))?;
    let _catalog_guard = state.application.graph_catalog_mutation_guard().await;
    let path = graph_path(&state, name)?;
    let _graph_lease = state
        .application
        .graph_admission_lease(&path)
        .map_err(application_error)?;
    if path.exists() {
        return Err(error(StatusCode::CONFLICT, "graph already exists"));
    }
    let definition = body.get("definition").cloned().unwrap_or_else(|| {
        json!({
            "objective": name,
            "entry": "start",
            "agents": {},
            "ops": {"start": {"run": "true"}},
            "nodes": [{"id": "start", "op": "start", "plugins": []}],
            "edges": []
        })
    });
    create_graph_bundle_from_catalog(&path, &definition, &state.catalog_root)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"graph":name,"definition":definition})),
    ))
}

#[allow(clippy::result_large_err)]
fn create_graph_bundle_from_catalog(
    path: &std::path::Path,
    definition: &Value,
    catalog_root: &std::path::Path,
) -> Result<(), HttpResponse> {
    let parent = path
        .parent()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "graph path has no parent"))?;
    crate::create_durable_directory(parent)
        .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))?;
    // Staging names cannot be addressed or listed as a catalog Graph.
    let staged = parent.join(format!(
        ".graph-create-{}-{}~",
        std::process::id(),
        crate::application::metadata::now_nanos(),
    ));
    std::fs::create_dir(&staged)
        .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))?;
    let prepared = (|| {
        write_graph_bundle_from_catalog(&staged, definition, catalog_root)?;
        FileGraphBundleLoader::new(&staged)
            .load()
            .map_err(|failure| error(StatusCode::UNPROCESSABLE_ENTITY, failure.to_string()))?;
        sync_bundle_directory(&staged)
            .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))?;
        if path.exists() {
            return Err(error(StatusCode::CONFLICT, "graph already exists"));
        }
        std::fs::rename(&staged, path)
            .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))?;
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|failure| error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()))
    })();
    if staged.exists() {
        let _ = std::fs::remove_dir_all(staged);
    }
    prepared
}

fn sync_bundle_directory(path: &std::path::Path) -> std::io::Result<()> {
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

pub(super) async fn update_graph(
    State(state): State<ApiState>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, HttpResponse> {
    let definition = body
        .get("definition")
        .cloned()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "definition is required"))?;
    let _catalog_guard = state.application.graph_catalog_mutation_guard().await;
    let path = graph_path(&state, &name)?;
    let _graph_lease = state
        .application
        .graph_admission_lease(&path)
        .map_err(application_error)?;
    let (path, current) = load_graph_definition(&state, &name)?;
    for (run_id, _) in state
        .application
        .runs_for_graph(&path)
        .map_err(application_error)?
    {
        state
            .application
            .reject_pending_session_delivery(&run_id)
            .map_err(application_error)?;
    }
    let next = anchor_runtime_rig::graph::GraphSnapshot::from_authoring(definition.clone())
        .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    if (!current.plugins.is_empty() || next.nodes.iter().any(|node| !node.plugins.is_empty()))
        && state
            .application
            .has_unfinished_plugin_run(&path)
            .await
            .map_err(application_error)?
    {
        return Err(error(
            StatusCode::CONFLICT,
            "cannot replace Plugin resources while this Graph has an unfinished Run",
        ));
    }
    write_graph_bundle_from_catalog(&path, &definition, &state.catalog_root)?;
    Ok(Json(json!({"graph":name,"definition":definition})))
}

pub(super) async fn delete_graph(
    State(state): State<ApiState>,
    AxumPath(name): AxumPath<String>,
) -> Result<StatusCode, HttpResponse> {
    let _catalog_guard = state.application.graph_catalog_mutation_guard().await;
    let path = graph_path(&state, &name)?;
    load_graph_definition(&state, &name)?;
    let removed_runs = state
        .application
        .delete_graph(&path, &state.workspace_root)
        .await
        .map_err(application_error)?;
    eprintln!("deleted Graph `{name}` and {removed_runs} terminal Run(s)");
    Ok(StatusCode::NO_CONTENT)
}

#[allow(clippy::result_large_err)]
#[cfg(test)]
pub(super) fn write_graph_bundle(
    path: &std::path::Path,
    definition: &Value,
) -> Result<(), HttpResponse> {
    anchor_runtime_rig::graph::GraphSnapshot::from_authoring(definition.clone())
        .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    std::fs::create_dir_all(path)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let graph_path = path.join("graph.json");
    let manifest_path = path.join("manifest.json");
    let graph_tmp = path.join(".graph.json.tmp");
    let manifest_tmp = path.join(".manifest.json.tmp");
    std::fs::write(
        &graph_tmp,
        serde_json::to_vec_pretty(definition)
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    )
    .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    std::fs::write(
        &manifest_tmp,
        br#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    std::fs::rename(graph_tmp, graph_path)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    std::fs::rename(manifest_tmp, manifest_path)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(())
}

#[allow(clippy::result_large_err)]
fn write_graph_bundle_from_catalog(
    path: &std::path::Path,
    definition: &Value,
    catalog_root: &std::path::Path,
) -> Result<(), HttpResponse> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static STAGING_ID: AtomicU64 = AtomicU64::new(0);
    let (_, catalog, bindings) = resolve_graph_definition(definition, catalog_root)
        .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    std::fs::create_dir_all(path)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
            std::fs::remove_dir_all(temporary)
                .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        } else if temporary.exists() {
            std::fs::remove_file(temporary)
                .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
    }

    let write_result = (|| -> Result<(), String> {
        for binding in &bindings {
            let source = catalog
                .plugin_directory(&binding.id)
                .map_err(|e| e.to_string())?;
            let plugin_stage = staged_plugins.join(&binding.id);
            for resource in &binding.resources {
                let relative = std::path::Path::new(resource);
                if relative.is_absolute()
                    || relative
                        .components()
                        .any(|component| !matches!(component, std::path::Component::Normal(_)))
                {
                    return Err(format!("Plugin {} has an unsafe resource path", binding.id));
                }
                let source_file = source.join(relative);
                let metadata =
                    std::fs::symlink_metadata(&source_file).map_err(|e| e.to_string())?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(format!(
                        "Plugin {} resource is not a regular file",
                        binding.id
                    ));
                }
                let destination = plugin_stage.join(relative);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::copy(source_file, destination).map_err(|e| e.to_string())?;
            }
        }
        std::fs::write(
            &graph_tmp,
            serde_json::to_vec_pretty(definition).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
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
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })();

    if let Err(message) = write_result {
        let _ = std::fs::remove_dir_all(&staged_plugins);
        let _ = std::fs::remove_file(&graph_tmp);
        let _ = std::fs::remove_file(&manifest_tmp);
        return Err(error(StatusCode::UNPROCESSABLE_ENTITY, message));
    }

    let plugin_root = path.join("plugins");
    let had_plugins = plugin_root.exists();
    if had_plugins {
        std::fs::rename(&plugin_root, &old_plugins)
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        return Err(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            failure.to_string(),
        ));
    }
    if had_plugins {
        std::fs::remove_dir_all(&old_plugins)
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(())
}
