use super::*;

const FILE_LIST_CAP: usize = 1000;
const FILE_PREVIEW_CAP: usize = 1024 * 1024;

#[allow(clippy::result_large_err)]
pub(super) fn workspace_path(
    state: &ApiState,
    run: &str,
    node: &str,
    relative: Option<&str>,
) -> Result<PathBuf, HttpResponse> {
    if run.is_empty()
        || node.is_empty()
        || !run
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        || !node
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid Run or node id"));
    }
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(run)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "no such run"))?;
    let result = record
        .results
        .get(node)
        .and_then(|results| results.last())
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "node has no committed file snapshot"))?;
    let artifacts = HostArtifacts::new(
        state.data_root.join("artifacts"),
        state.workspace_root.clone(),
    );
    let root = artifacts
        .files_path(&result.commit)
        .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let path = if let Some(relative) = relative {
        let relative_path = std::path::Path::new(relative);
        if relative_path.is_absolute()
            || relative_path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(error(StatusCode::BAD_REQUEST, "invalid file path"));
        }
        root.join(relative_path)
    } else {
        root.clone()
    };
    let canonical_root = root
        .canonicalize()
        .map_err(|_| error(StatusCode::NOT_FOUND, "no such node"))?;
    let canonical = path
        .canonicalize()
        .map_err(|_| error(StatusCode::NOT_FOUND, "no such file"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "file path escapes node workspace",
        ));
    }
    Ok(canonical)
}

pub(super) async fn list_files(
    State(state): State<ApiState>,
    AxumPath((run, node)): AxumPath<(String, String)>,
) -> Result<Json<Value>, HttpResponse> {
    let workspace = workspace_path(&state, &run, &node, None)?;
    let mut files = Vec::new();
    let mut stack = vec![workspace.clone()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory)
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        {
            let entry =
                entry.map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            if file_type.is_symlink() {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !path.is_file() {
                continue;
            }
            files.push(json!({"path":path.strip_prefix(&workspace).unwrap_or(&path).to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"),"size":entry.metadata().map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?.len()}));
            if files.len() >= FILE_LIST_CAP {
                break;
            }
        }
        if files.len() >= FILE_LIST_CAP {
            break;
        }
    }
    files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    Ok(Json(
        json!({"node":node,"files":files,"truncated":files.len() >= FILE_LIST_CAP}),
    ))
}

pub(super) async fn read_file(
    State(state): State<ApiState>,
    AxumPath((run, node, path)): AxumPath<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<HttpResponse, HttpResponse> {
    let target = workspace_path(&state, &run, &node, Some(&path))?;
    let metadata =
        std::fs::metadata(&target).map_err(|_| error(StatusCode::NOT_FOUND, "no such file"))?;
    if !metadata.is_file() {
        return Err(error(StatusCode::NOT_FOUND, "no such file"));
    }
    if query.get("download").map(String::as_str) == Some("1") {
        let file = tokio::fs::File::open(&target)
            .await
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let stream = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
        return Ok((
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CONTENT_DISPOSITION, "attachment"),
            ],
            stream,
        )
            .into_response());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(&target)
        .and_then(|file| file.take(FILE_PREVIEW_CAP as u64).read_to_end(&mut bytes))
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let truncated = metadata.len() > bytes.len() as u64;
    // A multibyte character split at the preview boundary is not binary data.
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
        String::from_utf8_lossy(&bytes[..bytes.len().min(FILE_PREVIEW_CAP)]).into_owned()
    };
    Ok(Json(json!({"path":path,"size":metadata.len(),"binary":binary,"text":text,"truncated":truncated})).into_response())
}
