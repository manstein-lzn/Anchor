use super::*;
use crate::application::graphs::GraphManagementError;

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
    match state.application.validate_graph(&definition) {
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

fn graph_management_error(failure: GraphManagementError) -> HttpResponse {
    match failure {
        GraphManagementError::BadRequest(message) => error(StatusCode::BAD_REQUEST, message),
        GraphManagementError::Missing(message) => error(StatusCode::NOT_FOUND, message),
        GraphManagementError::Conflict(message) => error(StatusCode::CONFLICT, message),
        GraphManagementError::Invalid(message) => error(StatusCode::UNPROCESSABLE_ENTITY, message),
        GraphManagementError::Storage(message) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

#[allow(clippy::result_large_err)]
pub(super) fn graph_path(state: &ApiState, name: &str) -> Result<PathBuf, HttpResponse> {
    state
        .application
        .graph_path_checked(name)
        .map_err(graph_management_error)
}

#[allow(clippy::result_large_err)]
pub(super) fn load_graph_definition(
    state: &ApiState,
    name: &str,
) -> Result<(PathBuf, anchor_graph_host::LoadedGraphBundle), HttpResponse> {
    state
        .application
        .load_graph(name)
        .map_err(graph_management_error)
}

pub(super) async fn graphs(State(state): State<ApiState>) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .list_graphs()
        .await
        .map(Json)
        .map_err(graph_management_error)
}

pub(super) async fn graph(
    State(state): State<ApiState>,
    AxumPath(requested): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .read_graph(&requested)
        .map(Json)
        .map_err(graph_management_error)
}

pub(super) async fn create_graph(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "name is required"))?;
    let created = state
        .application
        .create_graph(name, body.get("definition").cloned())
        .await
        .map_err(graph_management_error)?;
    Ok((StatusCode::CREATED, Json(created)))
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
    state
        .application
        .update_graph(&name, definition)
        .await
        .map(Json)
        .map_err(graph_management_error)
}

pub(super) async fn graph_delete_precondition(
    State(state): State<ApiState>,
    AxumPath(name): AxumPath<String>,
) -> Result<(HeaderMap, Json<Value>), HttpResponse> {
    let precondition = state
        .application
        .graph_delete_precondition(&name)
        .await
        .map_err(graph_management_error)?;
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::ETAG, format!("\"{precondition}\"").parse().unwrap());
    Ok((
        headers,
        Json(json!({"graph":name,"precondition":precondition})),
    ))
}

pub(super) async fn delete_graph(
    State(state): State<ApiState>,
    AxumPath(name): AxumPath<String>,
    headers: HeaderMap,
) -> Result<StatusCode, HttpResponse> {
    let conditions = headers.get_all(header::IF_MATCH).iter().collect::<Vec<_>>();
    let removed_runs = match conditions.as_slice() {
        [] => {
            state
                .application
                .remove_graph(&name, &state.workspace_root)
                .await
        }
        [condition] => {
            let expected = condition
                .to_str()
                .ok()
                .and_then(|condition| condition.strip_prefix('"'))
                .and_then(|condition| condition.strip_suffix('"'))
                .filter(|condition| !condition.contains('"'))
                .ok_or_else(|| {
                    error(
                        StatusCode::BAD_REQUEST,
                        "If-Match must be one quoted Graph delete precondition",
                    )
                })?;
            state
                .application
                .remove_graph_if_unchanged(&name, &state.workspace_root, expected)
                .await
        }
        _ => {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "If-Match must contain exactly one Graph delete precondition",
            ));
        }
    }
    .map_err(graph_management_error)?;
    eprintln!("deleted Graph `{name}` and {removed_runs} terminal Run(s)");
    Ok(StatusCode::NO_CONTENT)
}

#[allow(clippy::result_large_err)]
#[cfg(test)]
pub(super) fn write_graph_bundle(
    path: &std::path::Path,
    definition: &Value,
) -> Result<(), HttpResponse> {
    crate::application::graphs::write_graph_bundle(path, definition).map_err(graph_management_error)
}
