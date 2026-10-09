mod assistant_recovery;
#[allow(clippy::result_large_err)]
mod channel_sessions;
mod files;
mod graphs;
#[allow(clippy::result_large_err)]
pub(crate) mod oauth;
mod plugins;
#[allow(clippy::result_large_err)]
mod responses;
#[allow(clippy::result_large_err)]
mod runs;
mod schedules;
#[allow(clippy::result_large_err)]
mod sessions;
#[cfg(test)]
mod tests;
mod timeline;
#[allow(clippy::result_large_err)]
mod turns;
mod webhooks;
#[allow(clippy::result_large_err)]
mod wecom;
mod wecom_progress;
use super::*;
use crate::application::{ApplicationError, RunApplication};
use anchor_graph_host::FileGraphBundleLoader;
use anchor_runtime::graph::{FileRunStore, RunStatus};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response as HttpResponse},
    routing::{get, post},
};
use channel_sessions::*;
use files::*;
use graphs::*;
use oauth::*;
use plugins::*;
use responses::*;
use runs::*;
use schedules::*;
use serde_json::{Value, json};
use sessions::*;
use std::{
    collections::{HashMap, HashSet},
    env, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use timeline::*;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::ServeDir;
use turns::*;
use webhooks::*;

#[derive(Clone)]
struct ApiState {
    #[cfg(test)]
    bundle_root: PathBuf,
    catalog_root: PathBuf,
    data_root: PathBuf,
    workspace_root: PathBuf,
    graph_name: String,
    application: RunApplication,
    loopback: bool,
    api_keys: Vec<String>,
    schedules: ScheduleStoreHandle,
    pilots: crate::pilot_host::PilotService,
    wecom: WecomSettings,
    channel_descriptors: std::collections::BTreeMap<String, PathBuf>,
    channel_event_locks:
        std::sync::Arc<tokio::sync::Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>>,
    #[cfg(test)]
    plugin_checkout: Option<std::sync::Arc<dyn anchor_library::Checkout>>,
    #[cfg(test)]
    response_fixture: Option<String>,
}

#[derive(Clone, Default)]
struct WecomSettings {
    graph: String,
    reply_node: String,
    account: Option<String>,
    users: HashSet<String>,
}

impl WecomSettings {
    fn from_env() -> Self {
        Self {
            graph: env::var("ANCHOR_WECOM_GRAPH")
                .unwrap_or_default()
                .trim()
                .to_owned(),
            reply_node: env::var("ANCHOR_WECOM_REPLY_NODE")
                .unwrap_or_else(|_| "assistant".into())
                .trim()
                .to_owned(),
            account: env::var("WECOM_BOT_ID")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            users: env::var("ANCHOR_WECOM_USERS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    fn allows(&self, user: &str) -> bool {
        self.users.contains("*") || self.users.contains(user)
    }
}

fn error(status: StatusCode, message: impl Into<String>) -> HttpResponse {
    (status, Json(json!({"error": message.into()}))).into_response()
}

async fn auth(
    State(state): State<ApiState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> HttpResponse {
    let oauth_request = is_oauth_path(request.uri().path());
    if state.api_keys.is_empty() && state.loopback {
        return next.run(request).await;
    }
    let key = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !state
        .api_keys
        .iter()
        .any(|expected| constant_time_eq(expected, key))
    {
        let response = error(StatusCode::UNAUTHORIZED, "invalid API key");
        return if oauth_request {
            oauth::secure_oauth_response(response)
        } else {
            response
        };
    }
    next.run(request).await
}

async fn secure_oauth_responses(request: axum::extract::Request, next: Next) -> HttpResponse {
    let oauth_request = is_oauth_path(request.uri().path());
    let response = next.run(request).await;
    if oauth_request {
        oauth::secure_oauth_response(response)
    } else {
        response
    }
}

fn is_oauth_path(path: &str) -> bool {
    if path == "/oauth/callback" {
        return true;
    }
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    segments.len() == 4 && segments[0] == "plugins" && matches!(segments[2], "oauth" | "authorize")
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let (a, b) = (left.as_bytes(), right.as_bytes());
    let mut difference = a.len() ^ b.len();
    for index in 0..a.len().max(b.len()) {
        difference |=
            usize::from(a.get(index).copied().unwrap_or(0) ^ b.get(index).copied().unwrap_or(0));
    }
    difference == 0
}

fn status(value: RunStatus) -> &'static str {
    match value {
        RunStatus::Ready => "ready",
        RunStatus::Running => "running",
        RunStatus::Paused => "paused",
        RunStatus::BudgetStopped => "budget_stopped",
        RunStatus::WaitingCall => "waiting_call",
        RunStatus::WaitingRecovery => "waiting_recovery",
        RunStatus::Completed => "completed",
        RunStatus::Aborted => "aborted",
        RunStatus::Stopped => "stopped",
        RunStatus::Failed => "failed",
    }
}

pub async fn serve() -> io::Result<()> {
    let bundle_root = env::var_os("ANCHOR_RUNNER_BUNDLE_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("ANCHOR_RUNNER_BUNDLE_ROOT is required"))?;
    let data_root = env::var_os("ANCHOR_RUNNER_STATE_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("ANCHOR_RUNNER_STATE_ROOT is required"))?;
    let workspace_root = env::var_os("ANCHOR_RUNNER_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("ANCHOR_RUNNER_WORKSPACE_ROOT is required"))?;
    let graph_name = env::var("ANCHOR_RUNNER_GRAPH_NAME").unwrap_or_else(|_| {
        bundle_root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    let listen = env::var("ANCHOR_RUNNER_LISTEN").unwrap_or_else(|_| "127.0.0.1:8077".into());
    let loopback = listen
        .parse::<std::net::SocketAddr>()
        .is_ok_and(|addr| addr.ip().is_loopback())
        || listen.starts_with("localhost:");
    let api_keys = parse_api_keys(&env::var("ANCHOR_API_KEYS").unwrap_or_default())?;
    if !loopback && api_keys.is_empty() {
        return Err(io::Error::other(
            "ANCHOR_API_KEYS required on non-loopback bind",
        ));
    }
    let catalog_root = env::var_os("ANCHOR_RUNNER_CATALOG_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            bundle_root
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf()
        });
    let configured_graph_ready =
        validate_service_configuration(&bundle_root, &data_root, &workspace_root, &catalog_root)?;
    let _writer = crate::acquire_deployment_writer().map_err(io::Error::other)?;
    let schedules_path = env::var_os("ANCHOR_RUNNER_SCHEDULES_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_root.join("state/schedules.json"));
    let mut state = ApiState {
        #[cfg(test)]
        bundle_root: bundle_root.clone(),
        catalog_root: catalog_root.clone(),
        application: RunApplication::new(data_root.clone(), catalog_root.clone())
            .with_configured_graph(graph_name.clone(), bundle_root.clone()),
        data_root,
        workspace_root,
        graph_name,
        loopback,
        api_keys,
        schedules: ScheduleStore::open(schedules_path).map_err(io::Error::other)?,
        pilots: crate::pilot_host::PilotService::default(),
        wecom: WecomSettings::from_env(),
        channel_descriptors: Default::default(),
        channel_event_locks: Default::default(),
        #[cfg(test)]
        plugin_checkout: None,
        #[cfg(test)]
        response_fixture: None,
    };
    recover_pilot_turns(&state)
        .await
        .map_err(io::Error::other)?;
    state
        .application
        .recover_detached_at_startup()
        .await
        .map_err(|_| io::Error::other("startup Run recovery failed"))?;
    // A restart gives every Session back the assistant instance its binding
    // still owns; a failure is reported per Session and never stops the Host.
    if let Err(error) = assistant_recovery::resume_channel_assistants(&state).await {
        eprintln!("anchor-runner-host: assistant recovery is incomplete: {error}");
    }
    if let Err(error) = skip_missed_schedules(&state, chrono::Local::now().naive_local()) {
        return Err(io::Error::other(error));
    }
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    let callback_listen = listener.local_addr()?.to_string();
    let mut channel_supervisor = crate::channel_supervisor::ChannelSupervisor::start(
        &bundle_root,
        &state.data_root,
        &callback_listen,
        &state.api_keys,
    )
    .await?;
    if let Some(supervisor) = channel_supervisor.as_ref()
        && let Some(descriptor) = supervisor.descriptor("wecom")
        && let Some(existing) = env::var_os("ANCHOR_CHANNEL_CONTROL_DESCRIPTOR")
        && Path::new(&existing) != descriptor
    {
        if let Some(supervisor) = channel_supervisor.as_mut() {
            supervisor.shutdown().await;
        }
        return Err(io::Error::other(
            "ANCHOR_CHANNEL_CONTROL_DESCRIPTOR conflicts with the discovered WeCom channel",
        ));
    }
    if channel_supervisor.is_some()
        && (env::var_os("ANCHOR_CHANNEL_CONTROL_SOCKET").is_some()
            || env::var_os("ANCHOR_CHANNEL_CONTROL_TOKEN").is_some())
    {
        if let Some(supervisor) = channel_supervisor.as_mut() {
            supervisor.shutdown().await;
        }
        return Err(io::Error::other(
            "automatic channel supervision does not accept inherited control socket or token",
        ));
    }
    if let Some(supervisor) = channel_supervisor.as_ref() {
        state.channel_descriptors = supervisor.descriptors();
    }
    let ready = Arc::new(AtomicBool::new(configured_graph_ready));
    start_schedule_ticker(state.clone());
    let app = router_with_readiness(state, ready.clone());
    let shutdown_ready = ready.clone();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown_ready.store(false, Ordering::Release);
        })
        .await
        .map_err(io::Error::other);
    if let Some(supervisor) = channel_supervisor.as_mut() {
        supervisor.shutdown().await;
    }
    result
}

fn validate_service_configuration(
    bundle_root: &Path,
    state_root: &Path,
    workspace_root: &Path,
    catalog_root: &Path,
) -> io::Result<bool> {
    let roots = [
        (bundle_root, "bundle"),
        (state_root, "state"),
        (workspace_root, "workspace"),
        (catalog_root, "catalog"),
    ];
    for (path, label) in roots {
        if !path.is_absolute() {
            return Err(io::Error::other(format!(
                "configured {label} path must be absolute"
            )));
        }
        if path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            return Err(io::Error::other(format!(
                "configured {label} path must not contain parent components"
            )));
        }
        reject_symlink_components(path, label)?;
    }
    if paths_overlap(state_root, workspace_root) {
        return Err(io::Error::other(
            "configured state and workspace directories must not overlap",
        ));
    }

    let configured_graph_ready = match std::fs::symlink_metadata(bundle_root) {
        Err(failure) if failure.kind() == io::ErrorKind::NotFound => false,
        Err(_) => return Err(io::Error::other("configured Graph bundle is unavailable")),
        Ok(metadata) if !metadata.is_dir() => {
            return Err(io::Error::other(
                "configured Graph bundle is missing or invalid",
            ));
        }
        Ok(_) if !bundle_root.join("graph.json").is_file() => false,
        Ok(_) => {
            FileGraphBundleLoader::new(bundle_root)
                .load()
                .map_err(|_| io::Error::other("configured Graph bundle is missing or invalid"))?;
            true
        }
    };

    for (path, label) in [
        (state_root, "state"),
        (workspace_root, "workspace"),
        (catalog_root, "catalog"),
    ] {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| {
            io::Error::other(format!("configured {label} directory is unavailable"))
        })?;
        if !metadata.is_dir() || std::fs::File::open(path).is_err() {
            return Err(io::Error::other(format!(
                "configured {label} directory is unavailable"
            )));
        }
        let probe = path.join(format!(
            ".anchor-host-readiness-{}-{}.tmp",
            std::process::id(),
            READINESS_PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .map_err(|_| {
                io::Error::other(format!("configured {label} directory is unavailable"))
            })?;
        std::fs::remove_file(probe).map_err(|_| {
            io::Error::other(format!("configured {label} directory is unavailable"))
        })?;
    }
    reject_symlink_components(bundle_root, "bundle")?;

    let state_canonical = std::fs::canonicalize(state_root)
        .map_err(|_| io::Error::other("configured state directory is unavailable"))?;
    let workspace_canonical = std::fs::canonicalize(workspace_root)
        .map_err(|_| io::Error::other("configured workspace directory is unavailable"))?;
    if path_prefixes_overlap(&state_canonical, &workspace_canonical) {
        return Err(io::Error::other(
            "configured state and workspace directories must not overlap",
        ));
    }
    Ok(configured_graph_ready)
}

fn reject_symlink_components(path: &Path, label: &str) -> io::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::other(format!(
                    "configured {label} path contains a symlink"
                )));
            }
            Ok(_) => {}
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {
                return Err(io::Error::other(format!(
                    "configured {label} path is unavailable"
                )));
            }
        }
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left = normalize_absolute_path(left);
    let right = normalize_absolute_path(right);
    path_prefixes_overlap(&left, &right)
}

fn path_prefixes_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

static READINESS_PROBE_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(terminate) => terminate,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn graph_projection_error(
    failure: crate::application::graphs::GraphManagementError,
) -> HttpResponse {
    use crate::application::graphs::GraphManagementError;

    match failure {
        GraphManagementError::BadRequest(message) => error(StatusCode::BAD_REQUEST, message),
        GraphManagementError::Missing(message) => error(StatusCode::NOT_FOUND, message),
        GraphManagementError::Conflict(message) => error(StatusCode::CONFLICT, message),
        GraphManagementError::Invalid(message) => error(StatusCode::UNPROCESSABLE_ENTITY, message),
        GraphManagementError::Storage(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Graph catalog unavailable",
        ),
    }
}

async fn graph_relations(State(state): State<ApiState>) -> Result<Json<Value>, HttpResponse> {
    let catalog = state
        .application
        .list_graphs()
        .await
        .map_err(graph_projection_error)?;
    let names = catalog["graphs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|graph| graph["graph"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let mut graphs = Vec::with_capacity(names.len());
    let mut calls = Vec::new();
    for name in names {
        match state.application.load_graph(&name) {
            Ok((_, bundle)) => {
                for node in &bundle.snapshot.nodes {
                    let Some(op_name) = node.op.as_deref() else {
                        continue;
                    };
                    let Some(call) = bundle
                        .snapshot
                        .ops
                        .get(op_name)
                        .and_then(|op| op.get("call"))
                        .and_then(Value::as_object)
                    else {
                        continue;
                    };
                    let (Some(target), Some(mode)) = (
                        call.get("graph").and_then(Value::as_str),
                        call.get("mode").and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    calls.push(json!({
                        "graph": name,
                        "node": node.id,
                        "op": op_name,
                        "target": target,
                        "mode": mode,
                    }));
                }
            }
            Err(crate::application::graphs::GraphManagementError::Missing(_)) => {}
            Err(failure) => return Err(graph_projection_error(failure)),
        }
        graphs.push(name);
    }
    let schedules = state.schedules.lock().map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "schedule store unavailable",
        )
    })?;
    let graphs = graphs
        .into_iter()
        .map(|graph| {
            let count = schedules
                .items
                .iter()
                .filter(|item| item.graph == graph)
                .count();
            json!({"graph":graph,"schedules":count})
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"graphs":graphs,"calls":calls})))
}

#[allow(clippy::result_large_err)]
async fn channel_sessions_projection(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    blocking(move || {
        let sessions = store(&state)?
            .list(&owner)
            .map_err(session_error)?
            .into_iter()
            .filter(|session| {
                !session.graph.is_empty()
                    && !session.channel.is_empty()
                    && session.status != anchor_platform_session::SessionStatus::Archived
            })
            .map(|session| {
                json!({
                    "id": session.id,
                    "title": session.title,
                    "graph": session.graph,
                    "platform": session.channel.get("source")
                        .or_else(|| session.channel.get("platform"))
                        .cloned()
                        .unwrap_or_default(),
                })
            })
            .collect::<Vec<_>>();
        Ok(Json(json!({"sessions":sessions})))
    })
    .await
}

fn parse_api_keys(raw: &str) -> io::Result<Vec<String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let keys = serde_json::from_str::<Vec<String>>(raw)
        .map_err(|_| io::Error::other("ANCHOR_API_KEYS must be a JSON array of strings"))?;
    let unique = keys.iter().collect::<HashSet<_>>().len() == keys.len();
    if keys.is_empty() || keys.iter().any(|key| key.is_empty() || key.len() < 32) || !unique {
        return Err(io::Error::other(
            "ANCHOR_API_KEYS must contain unique secrets of at least 32 bytes",
        ));
    }
    Ok(keys)
}

#[cfg(test)]
fn router(state: ApiState) -> Router {
    let web_root = env::var_os("ANCHOR_RUNNER_WEB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/web/dist"));
    router_with_readiness_and_web_root(state, Arc::new(AtomicBool::new(true)), web_root)
}

#[cfg(test)]
fn router_with_web_root(state: ApiState, web_root: PathBuf) -> Router {
    router_with_readiness_and_web_root(state, Arc::new(AtomicBool::new(true)), web_root)
}

fn router_with_readiness(state: ApiState, ready: Arc<AtomicBool>) -> Router {
    let web_root = env::var_os("ANCHOR_RUNNER_WEB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/web/dist"));
    router_with_readiness_and_web_root(state, ready, web_root)
}

fn router_with_readiness_and_web_root(
    state: ApiState,
    ready: Arc<AtomicBool>,
    web_root: PathBuf,
) -> Router {
    let conversations = Router::new()
        .route("/conversation-runs", post(conversation_run))
        .layer(DefaultBodyLimit::max(crate::channel_inputs::MAX_BODY_BYTES))
        .layer(RequestBodyLimitLayer::new(
            crate::channel_inputs::MAX_BODY_BYTES,
        ));
    let wecom_events = Router::new()
        .route("/channels/wecom/events", post(wecom::receive_event))
        .route(
            "/channels/wecom/progress/{event_id}",
            get(wecom_progress::wecom_progress),
        )
        .layer(DefaultBodyLimit::max(crate::channel_inputs::MAX_BODY_BYTES))
        .layer(RequestBodyLimitLayer::new(
            crate::channel_inputs::MAX_BODY_BYTES,
        ));
    let api = Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route(
            "/ready",
            get(move || {
                let ready = ready.clone();
                async move {
                    if ready.load(Ordering::Acquire) {
                        (StatusCode::OK, Json(json!({"status":"ready"})))
                    } else {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"status":"not_ready"})),
                        )
                    }
                }
            }),
        )
        .route("/plugins", get(plugin_catalog))
        .route("/graph-relations", get(graph_relations))
        .route("/plugins/install", post(install_plugin))
        .route("/plugins/{plugin}", get(plugin_detail))
        .route("/plugins/{plugin}/files/{*path}", get(plugin_file))
        .route(
            "/plugins/{plugin}/oauth/{server}",
            get(oauth_status).post(authorize_oauth).delete(revoke_oauth),
        )
        .route(
            "/plugins/{plugin}/authorize/{server}",
            post(authorize_oauth),
        )
        .route("/graphs", get(graphs))
        .route("/graphs", post(create_graph))
        .route("/graph-validation", post(validate_graph))
        .route(
            "/graphs/{graph}",
            get(graph).put(update_graph).delete(delete_graph),
        )
        .route(
            "/graphs/{graph}/delete-precondition",
            get(graph_delete_precondition),
        )
        .route("/trigger", post(trigger))
        .route("/v1/responses", post(create_response))
        .route("/v1/webhooks/graphs/{graph}", post(graph_webhook))
        .route("/sessions", get(list_sessions).post(create_session))
        .route(
            "/sessions/{session}",
            get(get_session).put(rename_session).delete(delete_session),
        )
        .route("/sessions/{session}/status", post(set_session_status))
        .route("/sessions/{session}/events", get(session_events))
        .route(
            "/sessions/{session}/messages",
            get(pilot_messages).post(session_execution_unavailable),
        )
        .route(
            "/sessions/{session}/turns",
            get(list_pilot_turns).post(create_pilot_turn),
        )
        .route("/sessions/{session}/stop", post(stop_pilot))
        .route("/sessions/{session}/turns/{turn}", get(get_pilot_turn))
        .route(
            "/sessions/{session}/turns/{turn}/questions",
            get(list_pilot_questions),
        )
        .route(
            "/sessions/{session}/turns/{turn}/questions/{question}/answer",
            post(answer_pilot_question),
        )
        .route(
            "/sessions/{session}/turns/{turn}/events",
            get(pilot_turn_events),
        )
        .route("/channel-sessions/inbound", post(admit_channel_inbound))
        .route("/channel-sessions", get(channel_sessions_projection))
        .route(
            "/channel-sessions/{session}/assistant",
            get(get_channel_assistant),
        )
        .route(
            "/channel-sessions/{session}/assistant/retire",
            post(retire_channel_assistant),
        )
        .route("/channels/wecom/settlements", post(wecom::settle_delivery))
        .route(
            "/channel-sessions/{session}/inbounds/{inbound}",
            get(get_channel_relation),
        )
        .route(
            "/channel-sessions/{session}/inbounds/{inbound}/run",
            post(associate_channel_run),
        )
        .route(
            "/channel-sessions/{session}/turns/{turn}/deliveries",
            post(admit_channel_delivery),
        )
        .route(
            "/channel-sessions/{session}/turns/{turn}/deliveries/{key}/begin",
            post(begin_channel_delivery),
        )
        .route(
            "/channel-sessions/{session}/deliveries/{key}/settle",
            post(settle_channel_delivery),
        )
        .route(
            "/channel-sessions/{session}/deliveries/unresolved",
            get(list_channel_deliveries),
        )
        .route(
            "/channel-sessions/{session}",
            axum::routing::delete(delete_channel_session),
        )
        .route("/schedules", get(list_schedules).post(create_schedule))
        .route(
            "/schedules/{schedule}",
            axum::routing::delete(delete_schedule),
        )
        .route("/runs", get(list_runs))
        .route("/runs/{run}", get(get_run).delete(delete_run))
        .route("/runs/{run}/channel-reply", get(read_channel_reply))
        .route("/runs/{run}/session-execution", post(execute_session_call))
        .route("/runs/{run}/session-yield", post(yield_session_call))
        .route("/runs/{run}/session-settlement", post(settle_session_call))
        .route("/runs/{run}/recovery", post(recover_run))
        .route("/runs/{run}/abandon", post(abandon_run))
        .route("/runs/{run}/{operation}", post(control))
        .route("/runs/{run}/files/{node}", get(list_files))
        .route("/runs/{run}/files/{node}/{*path}", get(read_file))
        .route("/timeline", get(timeline))
        .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))
        .merge(conversations)
        .merge(wecom_events)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state.clone(), auth));
    let oauth_callback = Router::new()
        .route("/oauth/callback", get(oauth_callback))
        .with_state(state.clone());
    Router::new()
        .merge(api)
        .merge(oauth_callback)
        .fallback_service(ServeDir::new(web_root).append_index_html_on_directories(true))
        .layer(middleware::from_fn(secure_oauth_responses))
}

fn application_error(failure: ApplicationError) -> HttpResponse {
    match failure {
        ApplicationError::Missing => error(StatusCode::NOT_FOUND, "no such run"),
        ApplicationError::Conflict(message) => error(StatusCode::CONFLICT, message),
        ApplicationError::Invalid(message) => error(StatusCode::UNPROCESSABLE_ENTITY, message),
        ApplicationError::Storage(message) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}
