use super::*;

const FILE_PREVIEW_CAP: usize = 1024 * 1024;

fn delivery_error(error: crate::application::run_access::ArtifactAccessError) -> HttpResponse {
    use crate::application::run_access::ArtifactAccessError;
    use crate::artifacts::ArtifactAvailability;
    match error {
        ArtifactAccessError::Application(crate::application::ApplicationError::Invalid(
            message,
        )) => super::error(StatusCode::BAD_REQUEST, message),
        ArtifactAccessError::Application(error) => application_error(error),
        ArtifactAccessError::Unavailable(ArtifactAvailability::Expired) => {
            super::error(StatusCode::GONE, "交付文件已过期")
        }
        ArtifactAccessError::Unavailable(_) => {
            super::error(StatusCode::NOT_FOUND, "交付文件不可用")
        }
    }
}

pub(super) async fn list_files(
    State(state): State<ApiState>,
    AxumPath((run, node)): AxumPath<(String, String)>,
) -> Result<Json<Value>, HttpResponse> {
    let commit = state
        .application
        .node_delivery(&run, &node)
        .map_err(application_error)?;
    let mut listing = state
        .application
        .delivery_listing(&commit.id, &state.workspace_root)
        .map_err(application_error)?;
    listing["node"] = json!(node);
    Ok(Json(listing))
}

pub(super) async fn list_artifact_files(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .delivery_listing(&id, &state.workspace_root)
        .map(Json)
        .map_err(application_error)
}

pub(super) async fn retain_artifact(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, HttpResponse> {
    let retained = body
        .get("retained")
        .and_then(Value::as_bool)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "retained must be a boolean"))?;
    state
        .application
        .retain_delivery(&id, retained, &state.workspace_root)
        .map(Json)
        .map_err(delivery_error)
}

pub(super) async fn read_file(
    State(state): State<ApiState>,
    AxumPath((run, node, path)): AxumPath<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<HttpResponse, HttpResponse> {
    let commit = state
        .application
        .node_delivery(&run, &node)
        .map_err(application_error)?;
    delivery_response(&state, &commit.id, &path, &query).await
}

pub(super) async fn read_artifact_file(
    State(state): State<ApiState>,
    AxumPath((id, path)): AxumPath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<HttpResponse, HttpResponse> {
    delivery_response(&state, &id, &path, &query).await
}

async fn delivery_response(
    state: &ApiState,
    id: &str,
    path: &str,
    query: &HashMap<String, String>,
) -> Result<HttpResponse, HttpResponse> {
    let opened = state
        .application
        .open_delivery_file(id, path, &state.workspace_root)
        .map_err(delivery_error)?;
    let size = opened.bytes;
    let (file, lease) = opened.into_parts();
    if query.get("download").map(String::as_str) == Some("1") {
        use futures_util::StreamExt;
        let stream =
            tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file)).map(move |chunk| {
                let _guard = &lease;
                chunk
            });
        return Ok((
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CONTENT_DISPOSITION, "attachment"),
            ],
            axum::body::Body::from_stream(stream),
        )
            .into_response());
    }
    let _lease = lease;
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(FILE_PREVIEW_CAP as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let truncated = size > bytes.len() as u64;
    if truncated
        && let Err(error) = std::str::from_utf8(&bytes)
        && error.error_len().is_none()
    {
        bytes.truncate(error.valid_up_to());
    }
    let binary = std::str::from_utf8(&bytes).is_err() || bytes.contains(&0);
    let text = if binary {
        String::new()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok(
        Json(json!({"path":path,"size":size,"binary":binary,"text":text,"truncated":truncated}))
            .into_response(),
    )
}

/// Read the durable rich channel reply prepared by a Rust AgentNode.  It is
/// kept out of the normal Run JSON projection because an image can approach
/// the WeCom 10 MiB limit.
pub(super) async fn read_channel_reply(
    State(state): State<ApiState>,
    AxumPath(run): AxumPath<String>,
) -> Result<HttpResponse, HttpResponse> {
    if run.is_empty()
        || !run
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid Run id"));
    }
    let metadata = state
        .application
        .metadata(&run)
        .map_err(application_error)?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "no such run"))?;
    if metadata.conversation.is_none() {
        return Err(error(StatusCode::NOT_FOUND, "Run has no channel reply"));
    }
    let path = state
        .data_root
        .join("channel-replies")
        .join(format!("{run}.json"));
    let completed = state
        .application
        .channel_reply_is_completed(&run)
        .map_err(|message| error(StatusCode::INTERNAL_SERVER_ERROR, message))?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "no such run"))?;
    if !completed || state.application.active_runs(None).await.contains(&run) {
        return Err(error(StatusCode::CONFLICT, "channel Run has not completed"));
    }
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(&path).await.map_err(|e| {
        error(
            if e.kind() == std::io::ErrorKind::NotFound {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            },
            "channel reply is unavailable",
        )
    })?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "channel reply exceeds its size limit",
        ));
    }
    let items: Vec<Value> = serde_json::from_slice(&bytes)
        .map_err(|_| error(StatusCode::UNPROCESSABLE_ENTITY, "invalid channel reply"))?;
    if items.is_empty() {
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "empty channel reply",
        ));
    }
    Ok((
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        bytes,
    )
        .into_response())
}
