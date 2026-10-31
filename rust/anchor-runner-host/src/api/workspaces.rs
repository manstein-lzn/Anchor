use super::*;
use crate::application::run_access::workspaces::WorkspaceBindRequest;

pub(super) async fn bind_workspace(
    State(state): State<ApiState>,
    Json(request): Json<WorkspaceBindRequest>,
) -> Result<Json<Value>, HttpResponse> {
    let workspaces = crate::workspaces::Workspaces::new(state.workspace_root);
    state
        .application
        .bind_workspace_resource(request, &workspaces)
        .await
        .map(Json)
        .map_err(application_error)
}

pub(super) async fn workspace_binding(
    State(state): State<ApiState>,
    AxumPath((run, node)): AxumPath<(String, String)>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .node_workspace_binding(&run, &node)
        .map(Json)
        .map_err(application_error)
}

pub(super) async fn release_workspace(
    State(state): State<ApiState>,
    AxumPath(resource): AxumPath<String>,
) -> Result<StatusCode, HttpResponse> {
    state
        .application
        .release_workspace_resource(
            &resource,
            &crate::workspaces::Workspaces::new(state.workspace_root),
        )
        .map_err(application_error)?;
    Ok(StatusCode::NO_CONTENT)
}
