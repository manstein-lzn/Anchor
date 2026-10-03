use super::*;

pub(super) fn graph_name(state: &ApiState) -> String {
    state.graph_name.clone()
}

#[allow(clippy::result_large_err)]
pub(super) fn graph_path(state: &ApiState, name: &str) -> Result<PathBuf, HttpResponse> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid graph name"));
    }
    // The configured Graph name resolves to its deployment bundle root; every
    // other name resolves inside the catalog root. Aliasing the same directory
    // under two names must still yield one identity for locks and deletion.
    Ok(state.application.graph_bundle_path(name))
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
                if !names.contains(&name) && FileGraphBundleLoader::new(entry.path()).load().is_ok()
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
    let (path, _bundle) = load_graph_definition(&state, &requested)?;
    let definition: Value = serde_json::from_slice(
        &std::fs::read(path.join("graph.json"))
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    )
    .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    Ok(Json(json!({"graph":requested,"definition":definition})))
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
    let definition = json!({
        "objective": name,
        "entry": "start",
        "agents": {},
        "ops": {"start": {"run": "true"}},
        "nodes": [{"id": "start", "op": "start", "plugins": []}],
        "edges": []
    });
    write_graph_bundle(&path, &definition)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"graph":name,"definition":definition})),
    ))
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
    if current
        .snapshot
        .nodes
        .iter()
        .any(|node| !node.plugins.is_empty())
    {
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Plugin graph editing requires Rust Plugin binding",
        ));
    }
    let snapshot = anchor_runtime_rig::graph::GraphSnapshot::admit(definition.clone())
        .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    if snapshot.nodes.iter().any(|node| !node.plugins.is_empty()) {
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Plugin graph editing requires Rust Plugin binding",
        ));
    }
    write_graph_bundle(&path, &definition)?;
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
pub(super) fn write_graph_bundle(
    path: &std::path::Path,
    definition: &Value,
) -> Result<(), HttpResponse> {
    anchor_runtime_rig::graph::GraphSnapshot::admit(definition.clone())
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
