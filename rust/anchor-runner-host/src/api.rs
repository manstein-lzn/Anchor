mod files;
mod graphs;
mod plugins;
mod runs;
mod schedules;
#[cfg(test)]
mod tests;
mod timeline;
use super::*;
use crate::application::{ApplicationError, RunApplication};
use anchor_graph_host::FileGraphBundleLoader;
use anchor_runtime_rig::graph::{FileRunStore, RunStatus};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response as HttpResponse},
    routing::{get, post},
};
use files::*;
use graphs::*;
use plugins::*;
use runs::*;
use schedules::*;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    env, io,
    path::PathBuf,
};
use timeline::*;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::ServeDir;

#[derive(Clone)]
struct ApiState {
    bundle_root: PathBuf,
    catalog_root: PathBuf,
    data_root: PathBuf,
    workspace_root: PathBuf,
    graph_name: String,
    application: RunApplication,
    loopback: bool,
    api_keys: Vec<String>,
    schedules: ScheduleStoreHandle,
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
        return error(StatusCode::UNAUTHORIZED, "invalid API key");
    }
    next.run(request).await
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
    let _writer = crate::acquire_deployment_writer().map_err(io::Error::other)?;
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
    let schedules_path = env::var_os("ANCHOR_RUNNER_SCHEDULES_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_root.join("state/schedules.json"));
    let state = ApiState {
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
    };
    // A deleted configured bundle must not block service start: the Graph is
    // reported as missing and other Graphs keep serving. Detached recovery is
    // best-effort for the same reason.
    if let Err(error) = state.application.recover_detached_at_startup().await {
        eprintln!("detached Run recovery skipped: {error:?}");
    }
    if let Err(error) = skip_missed_schedules(&state, chrono::Local::now().naive_local()) {
        return Err(io::Error::other(error));
    }
    start_schedule_ticker(state.clone());
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    axum::serve(listener, app).await.map_err(io::Error::other)
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

fn router(state: ApiState) -> Router {
    let web_root = env::var_os("ANCHOR_RUNNER_WEB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/web/dist"));
    router_with_web_root(state, web_root)
}

fn router_with_web_root(state: ApiState, web_root: PathBuf) -> Router {
    let conversations = Router::new()
        .route("/conversation-runs", post(conversation_run))
        .layer(DefaultBodyLimit::max(crate::channel_inputs::MAX_BODY_BYTES))
        .layer(RequestBodyLimitLayer::new(
            crate::channel_inputs::MAX_BODY_BYTES,
        ));
    let api = Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/plugins", get(plugin_catalog))
        .route("/plugins/{plugin}", get(plugin_detail))
        .route("/plugins/{plugin}/files/{*path}", get(plugin_file))
        .route("/graphs", get(graphs))
        .route("/graphs", post(create_graph))
        .route("/graph-validation", post(validate_graph))
        .route(
            "/graphs/{graph}",
            get(graph).put(update_graph).delete(delete_graph),
        )
        .route("/trigger", post(trigger))
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
        .route("/runs/{run}/{operation}", post(control))
        .route("/runs/{run}/files/{node}", get(list_files))
        .route("/runs/{run}/files/{node}/{*path}", get(read_file))
        .route("/timeline", get(timeline))
        .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))
        .merge(conversations)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, auth));
    Router::new()
        .merge(api)
        .fallback_service(ServeDir::new(web_root).append_index_html_on_directories(true))
}

fn application_error(failure: ApplicationError) -> HttpResponse {
    match failure {
        ApplicationError::Missing => error(StatusCode::NOT_FOUND, "no such run"),
        ApplicationError::Conflict(message) => error(StatusCode::CONFLICT, message),
        ApplicationError::Invalid(message) => error(StatusCode::UNPROCESSABLE_ENTITY, message),
        ApplicationError::Storage(message) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}
