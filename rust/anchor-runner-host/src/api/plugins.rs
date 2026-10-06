use super::*;
use anchor_graph_host::{FilePluginCatalog, PluginDefinition};
use anchor_runtime_rig::graph::GraphError;
use std::io::Read;
use std::path::{Component, Path};

pub(super) fn library_root(catalog_root: &Path) -> PathBuf {
    std::env::var_os("ANCHOR_RUNNER_LIBRARY_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| catalog_root.to_path_buf())
}

fn plugin_record(definition: &PluginDefinition) -> Value {
    let mcp_servers = definition
        .mcp_servers
        .iter()
        .map(|server| {
            let config = &server.config;
            let transport = config.get("type").and_then(Value::as_str).unwrap_or(
                if config.get("command").is_some() {
                    "stdio"
                } else {
                    "http"
                },
            );
            let mut projection = serde_json::Map::new();
            projection.insert("transport".into(), Value::String(transport.into()));
            if config.get("auth").and_then(Value::as_str) == Some("oauth")
                || config.get("oauth_resource").is_some()
            {
                projection.insert("auth".into(), Value::String("oauth".into()));
            }
            (server.name.clone(), Value::Object(projection))
        })
        .collect::<serde_json::Map<_, _>>();
    json!({
        "id": definition.id,
        "name": definition.name,
        "description": definition.description,
        "skills": definition.skills,
        "unsupported": definition.unsupported,
        "mcpServers": mcp_servers,
        "channels": definition.channels.iter().map(|channel| json!({
            "plugin": channel.plugin,
            "platform": channel.platform,
            "transport": channel.transport,
            "entrypoint": channel.entrypoint,
            "required_environment": channel.required_environment,
            "description": channel.description,
            "sdk": channel.sdk,
        })).collect::<Vec<_>>(),
        "digest": definition.digest,
        "available": true,
    })
}

fn unavailable_record(id: String, error: impl ToString) -> Value {
    json!({
        "id": id,
        "name": id,
        "description": "",
        "skills": [],
        "mcpServers": {},
        "available": false,
        "error": error.to_string(),
    })
}

pub(super) async fn plugin_catalog(
    State(state): State<ApiState>,
) -> Result<Json<Value>, HttpResponse> {
    let root = library_root(&state.catalog_root);
    let catalog = FilePluginCatalog::new(&root);
    let entries = match std::fs::read_dir(root.join("plugins")) {
        Ok(entries) => entries,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Json(json!({"plugins": []})));
        }
        Err(_) => {
            return Err(error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot read Plugin catalog",
            ));
        }
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot read Plugin catalog",
            )
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let file_type = entry.file_type().map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot read Plugin catalog",
            )
        })?;
        if !file_type.is_dir() && !file_type.is_symlink() {
            continue;
        }
        records.push(if file_type.is_symlink() {
            unavailable_record(name, "Plugin is unavailable or malformed")
        } else {
            match catalog.definition(&name) {
                Ok(definition) => plugin_record(&definition),
                Err(failure) => unavailable_record(name, public_catalog_error(failure)),
            }
        });
    }
    records.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    Ok(Json(json!({"plugins": records})))
}

pub(super) async fn plugin_detail(
    State(state): State<ApiState>,
    AxumPath(plugin): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    let root = library_root(&state.catalog_root);
    plugin_entry(&root, &plugin).map_err(|message| error(StatusCode::BAD_REQUEST, message))?;
    let catalog = FilePluginCatalog::new(root);
    let definition = catalog
        .definition(&plugin)
        .map_err(|failure| error(StatusCode::BAD_REQUEST, public_catalog_error(failure)))?;
    let instructions = instructions(&definition)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "cannot read Plugin instructions"))?;
    let mut record = plugin_record(&definition);
    record["instructions"] = Value::String(instructions);
    Ok(Json(record))
}

pub(super) async fn plugin_file(
    State(state): State<ApiState>,
    AxumPath((plugin, path)): AxumPath<(String, String)>,
) -> Result<HttpResponse, HttpResponse> {
    let root = library_root(&state.catalog_root);
    plugin_entry(&root, &plugin).map_err(|message| error(StatusCode::BAD_REQUEST, message))?;
    let catalog = FilePluginCatalog::new(root);
    let definition = catalog
        .definition(&plugin)
        .map_err(|failure| error(StatusCode::BAD_REQUEST, public_catalog_error(failure)))?;
    if path == "instructions.md"
        && !definition
            .skills
            .iter()
            .any(|skill| skill == "instructions.md")
        && !instructions(&definition)
            .map_err(|_| error(StatusCode::BAD_REQUEST, "cannot read Plugin instructions"))?
            .is_empty()
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Plugin uses skills/<skill>/SKILL.md; read its content from the Plugin detail endpoint",
        ));
    }
    let file = open_resource(&definition.directory, &path)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "no such safe Plugin file"))?;
    let size = file
        .metadata()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "cannot read Plugin file"))?
        .len();
    let target = Path::new(&path);
    let filename = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(
        tokio::fs::File::from_std(file),
    ));
    Ok((
        [
            (header::CONTENT_TYPE, mime_for_path(target)),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{}\"", safe_filename(filename)),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CONTENT_LENGTH, &size.to_string()),
        ],
        body,
    )
        .into_response())
}

fn instructions(definition: &PluginDefinition) -> Result<String, std::io::Error> {
    let legacy = definition.directory.join("instructions.md");
    if legacy.is_file() {
        return read_resource(&definition.directory, "instructions.md");
    }
    let mut bodies = Vec::new();
    for relative in &definition.skills {
        let path = definition.directory.join(relative);
        if path.is_file() {
            let text = read_resource(&definition.directory, relative)?;
            bodies.push(skill_body(&text).to_owned());
        }
    }
    Ok(bodies.join("\n\n"))
}

fn skill_body(text: &str) -> &str {
    if let Some(rest) = text.strip_prefix("---\n") {
        return rest.split_once("\n---\n").map_or("", |(_, body)| body);
    }
    text
}

fn open_resource(base: &Path, relative: &str) -> Result<std::fs::File, std::io::Error> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let invalid =
        || std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid Plugin file path");
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid());
    }
    let target = base.join(path);
    let mut directory = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let components = target
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect::<Vec<_>>();
    // Descriptor-relative NOFOLLOW keeps a replaced internal symlink from
    // escaping the canonical Plugin directory between validation and reading.
    for (index, component) in components.iter().enumerate() {
        let last = index + 1 == components.len();
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let fd = openat(
            &directory,
            Path::new(component),
            if last {
                flags
            } else {
                flags | OFlags::DIRECTORY
            },
            Mode::empty(),
        )?;
        if last {
            let file = std::fs::File::from(fd);
            if !file.metadata()?.is_file() {
                return Err(invalid());
            }
            return Ok(file);
        }
        directory = fd;
    }
    Err(invalid())
}

fn read_resource(base: &Path, relative: &str) -> Result<String, std::io::Error> {
    let mut text = String::new();
    open_resource(base, relative)?.read_to_string(&mut text)?;
    Ok(text)
}

fn public_catalog_error(_error: GraphError) -> &'static str {
    // Parser errors can contain raw config keys or host paths; only expose
    // availability here, never the unexpanded MCP declaration.
    "Plugin is unavailable or malformed"
}

fn plugin_entry(root: &Path, id: &str) -> Result<PathBuf, &'static str> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        || !id.as_bytes()[0].is_ascii_alphanumeric()
    {
        return Err("invalid Plugin reference");
    }
    let entry = root.join("plugins").join(id);
    let metadata = std::fs::symlink_metadata(&entry).map_err(|_| "no such Plugin")?;
    if metadata.file_type().is_symlink() {
        return Err("Plugin symlinks are not supported");
    }
    if !metadata.is_dir() {
        return Err("no such Plugin");
    }
    Ok(entry)
}

fn safe_filename(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || ".-_".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "json" => "application/json",
        "csv" => "text/csv; charset=utf-8",
        "txt" | "yaml" | "yml" | "toml" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}
