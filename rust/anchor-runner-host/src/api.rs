mod assistant_recovery;
mod channel_management;
#[allow(clippy::result_large_err)]
mod channel_sessions;
mod credential_requests;
mod files;
mod graphs;
#[allow(clippy::result_large_err)]
pub(crate) mod oauth;
mod plugin_management;
mod plugins;
#[allow(clippy::result_large_err)]
mod responses;
#[allow(clippy::result_large_err)]
mod runs;
mod schedules;
#[allow(clippy::result_large_err)]
mod sessions;
mod settings;
#[cfg(test)]
mod tests;
mod timeline;
#[allow(clippy::result_large_err)]
mod turns;
mod webhooks;
#[allow(clippy::result_large_err)]
mod wecom;
mod wecom_progress;
mod workspaces;
#[cfg(test)]
use super::*;
use crate::application::{ApplicationError, RunApplication};
use crate::deployment::configured_pilot_graph;
use crate::plugin_library::Catalog;
use anchor_graph_host::FileGraphBundleLoader;
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
    collections::{BTreeMap, HashMap, HashSet},
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
    model_configuration: Arc<crate::model_configuration::ModelConfiguration>,
    runtime_management: Arc<crate::runtime_management::RuntimeManagement>,
    /// The one identity every Web/API request of this installation acts as.
    owner: String,
    schedules: ScheduleStoreHandle,
    pilots: crate::pilot_host::PilotService,
    channel_event_locks:
        std::sync::Arc<tokio::sync::Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>>,
    #[cfg(test)]
    response_fixture: Option<String>,
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
    let key = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    // A gateway's callback key reaches only its own bot Graph's two
    // callback routes; a key of the earlier single-bot shape reaches nothing.
    // Every other request is this installation's own: the Host listens on
    // loopback only and has no login.
    let callback = state.application.channel_callback(key);
    if key.starts_with("anchor-channel-api-") || callback.is_some() {
        let allowed = callback.is_some_and(|callback| {
            channel_callback_path(request.method(), request.uri().path(), &callback.graph)
        });
        if !allowed {
            return error(
                StatusCode::UNAUTHORIZED,
                "invalid channel callback authorization",
            );
        }
        return next.run(request).await;
    }
    next.run(request).await
}

fn channel_callback_path(method: &axum::http::Method, path: &str, graph: &str) -> bool {
    let Some(rest) = path
        .strip_prefix("/channels/")
        .and_then(|rest| rest.strip_prefix(graph))
    else {
        return false;
    };
    (method == axum::http::Method::POST && rest == "/events")
        || (method == axum::http::Method::GET
            && rest
                .strip_prefix("/progress/")
                .is_some_and(|event| !event.is_empty() && !event.contains('/')))
}

async fn secure_private_responses(request: axum::extract::Request, next: Next) -> HttpResponse {
    let oauth_request = is_oauth_path(request.uri().path());
    let credential_request = credential_requests::is_private_path(request.uri().path())
        || plugin_management::is_private_path(request.uri().path())
        || settings::is_private_path(request.uri().path());
    let response = next.run(request).await;
    if oauth_request {
        oauth::secure_oauth_response(response)
    } else if credential_request {
        credential_requests::secure_private_response(response)
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

pub async fn serve(configuration: Arc<crate::HostConfiguration>) -> io::Result<()> {
    let configured_bundle = env::var_os("ANCHOR_RUNNER_BUNDLE_ROOT")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let data_root = configuration
        .state_root()
        .map_err(io::Error::other)?
        .to_path_buf();
    let workspace_root = env::var_os("ANCHOR_RUNNER_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("ANCHOR_RUNNER_WORKSPACE_ROOT is required"))?;
    let bundle_root = configured_bundle
        .clone()
        .unwrap_or_else(|| data_root.join("unconfigured-graph"));
    // The same single authority the node call and the graph routes read.
    let graph_name = configured_pilot_graph().unwrap_or_default();
    let listen = env::var("ANCHOR_RUNNER_LISTEN").unwrap_or_else(|_| "127.0.0.1:8077".into());
    let loopback = listen
        .parse::<std::net::SocketAddr>()
        .is_ok_and(|addr| addr.ip().is_loopback())
        || listen.starts_with("localhost:");
    // There is no login: the Host serves only this machine. Remote use goes
    // through an SSH tunnel or another authenticated front of the operator's.
    if !loopback {
        return Err(io::Error::other(
            "ANCHOR_RUNNER_LISTEN must be a loopback address; Anchor has no login, use an SSH tunnel for remote access",
        ));
    }
    if env::var("ANCHOR_API_KEYS").is_ok_and(|value| !matches!(value.trim(), "" | "[]")) {
        return Err(io::Error::other(
            "ANCHOR_API_KEYS was removed: Anchor no longer has API key login; unset it",
        ));
    }
    let owner = sessions::installation_owner(&data_root)?;
    let catalog_root = configuration.catalog_root.clone().unwrap_or_else(|| {
        if configured_bundle.is_none() {
            return data_root.join("catalog");
        }
        bundle_root
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf()
    });
    for root in [&data_root, &workspace_root, &catalog_root] {
        crate::host_fs::directory(root).map_err(io::Error::other)?;
    }
    let configured_graph_ready =
        validate_service_configuration(&bundle_root, &data_root, &workspace_root, &catalog_root)?;
    let _writer = crate::acquire_deployment_writer(&data_root).map_err(io::Error::other)?;
    let retention = crate::retention::Policy::from_env().map_err(io::Error::other)?;
    crate::durable_write::cleanup_at_startup(&data_root, &workspace_root, || {
        crate::application::RunApplication::new(data_root.clone(), data_root.clone())
            .durable_input_workspaces(&workspace_root)
            .ok()
    });
    let swept = plugin_management::sweep_staging(&data_root);
    if swept > 0 {
        eprintln!(
            "anchor-runner-host: removed {swept} Plugin staging directories left by an earlier process"
        );
    }
    let library_root = configuration.library_root(&catalog_root);
    crate::runtime_configuration::install_release_network(library_root)
        .map_err(io::Error::other)?;
    // Start-up reclamation is best effort: a Library that cannot be swept (a busy
    // writer, an unreadable manifest) must never keep the host from starting, and
    // the sweep only deletes what it can prove is unreferenced.
    {
        match anchor_library::reclaim(library_root, true) {
            // Anything that happened, or was refused, is worth one line; a Library
            // with nothing to reclaim stays quiet.
            Ok(report)
                if report.removed_views.is_empty()
                    && report.removed_package_versions.is_empty()
                    && report.deferred.is_none()
                    && report.store_phase_abandoned.is_none() => {}
            Ok(report) => eprintln!(
                "anchor-runner-host: dependency reclamation: {}",
                serde_json::to_string(&anchor_library::summarize(&report)).unwrap_or_default()
            ),
            Err(error) => {
                eprintln!("anchor-runner-host: dependency reclamation did not run: {error}")
            }
        }
    }
    let schedules_path = env::var_os("ANCHOR_RUNNER_SCHEDULES_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_root.join("state/schedules.json"));
    // Environment preparation is the one capability that reaches the network.
    // A deployment that did not authorize sources and a pinned toolchain keeps
    // exactly the behaviour it had: the steward prepares packages, and
    // provisioning is refused with a reason.
    // The root dependency preparation writes its wheelhouse into: the
    // explicit Library root, otherwise the Graph catalog the Plugin API also
    // falls back to. The build root must stay disjoint from it.
    let runtime_configuration = Arc::new(
        crate::runtime_configuration::RuntimeConfiguration::from_env(
            &data_root,
            library_root,
            configuration.bwrap.clone(),
        )
        .map_err(io::Error::other)?,
    );
    let model_configuration = Arc::new(
        crate::model_configuration::ModelConfiguration::from_env(&data_root)
            .map_err(io::Error::other)?,
    );
    let runtime_management = Arc::new(
        crate::runtime_management::RuntimeManagement::new(
            runtime_configuration.clone(),
            &data_root,
        )
        .map_err(io::Error::other)?,
    );
    let mut application = RunApplication::new(data_root.clone(), catalog_root.clone())
        .with_host_configuration(configuration)
        .with_runtime_configuration(runtime_configuration);
    let configured_graph_entry = configured_bundle
        .is_some()
        .then(|| (graph_name.clone(), bundle_root.clone()));
    if let Some((name, path)) = configured_graph_entry.clone() {
        application = application.with_configured_graph(name, path);
    }
    let schedules = ScheduleStore::open(schedules_path).map_err(io::Error::other)?;
    application = application.with_schedule_management(Arc::new(
        schedules::ScheduleManagementAdapter::new(schedules.clone()),
    ));
    let mut state = ApiState {
        #[cfg(test)]
        bundle_root: bundle_root.clone(),
        catalog_root: catalog_root.clone(),
        application,
        model_configuration,
        runtime_management,
        data_root,
        workspace_root,
        graph_name,
        owner,
        schedules,
        pilots: crate::pilot_host::PilotService::default(),
        channel_event_locks: Default::default(),
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
    if let Err(error) = skip_missed_schedules(&state, chrono::Local::now().naive_local()) {
        return Err(io::Error::other(error));
    }
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    let callback_listen = listener.local_addr()?.to_string();
    state.application = state.application.clone().with_oauth_management(Arc::new(
        oauth::OAuthManagementAdapter::new(
            state.data_root.clone(),
            state.application.library_root().to_path_buf(),
            &callback_listen,
        ),
    ));
    let channel_manager = Arc::new(
        crate::channel_supervisor::management::ChannelManagementAdapter::open(
            crate::channel_supervisor::management::ChannelHostConfiguration {
                data_root: state.data_root.clone(),
                catalog_root: state.catalog_root.clone(),
                configured_graph: configured_graph_entry.clone(),
                library_root: state.application.library_root().to_path_buf(),
                listen: callback_listen,
            },
        )
        .map_err(io::Error::other)?,
    );
    channel_manager
        .resume()
        .await
        .map_err(|error| io::Error::other(format!("channel startup unavailable: {error:?}")))?;
    state.application = state
        .application
        .clone()
        .with_channel_management(channel_manager.clone());
    if let Err(error) = assistant_recovery::resume_channel_assistants(&state).await {
        eprintln!("anchor-runner-host: assistant recovery is incomplete: {error}");
    }
    let ready = Arc::new(AtomicBool::new(
        configured_bundle.is_none() || configured_graph_ready,
    ));
    start_schedule_ticker(state.clone());
    schedules::start_retention(state.clone(), retention);
    let app = router_with_readiness(state, ready.clone());
    let shutdown_ready = ready.clone();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown_ready.store(false, Ordering::Release);
        })
        .await
        .map_err(io::Error::other);
    channel_manager.shutdown().await;
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

/// What one Session's current Run froze about one Plugin.
///
/// Several nodes of the same snapshot may declare the same Plugin, and the
/// Session view cannot know which node the next Turn starts at, so both fields
/// aggregate over the whole frozen snapshot: **any** node declaring `required`
/// makes it required, and it counts as mounted by default when **some** node
/// would mount it with no Session switch (it is required, or its `default` is
/// not explicitly false). For a single declaring node this is exactly what the
/// mount filter does for that node.
use crate::plugin_library::DeclarationProjection as PluginDeclaration;

/// The declarations the Session's **attached Runs froze**, by Plugin id.
///
/// Deliberately not the Graph on disk: the mount filter reads the admitted Run's
/// frozen snapshot (`node_host::mounted_plugins`), so this page has to answer
/// from the same facts or it would promise a mount that Run never makes.
///
/// The answer is the **union over every Run this Session attached**, not the most
/// recent one. An assistant's `graph_run` attaches a Run of a Graph it made
/// itself, so "the last Run" is something the assistant can choose: reading only
/// that Run would let it hide a Plugin the operator's pilot Graph declares
/// required, the page would offer the switch, and a human click would write a
/// `session.plugin_mount.changed` event authorizing a switch-off that the filter
/// then ignores. A Plugin is therefore only "not required" when **no** attached
/// Run requires it, and a Run that cannot be read (a deleted Run still listed)
/// contributes nothing rather than an unlock.
///
/// `required: true` is an authorization, and the filter recognizes exactly one
/// source for it: `configured_pilot_graph()`, the deployment's own pilot Graph. A
/// Run whose metadata names any other Graph — one an assistant created or edited,
/// or a Session's own choice — has its `required` downgraded to optional here
/// too, by the same comparison. The two sides must never disagree about who may
/// hand out a lock.
///
/// Aggregation is per Plugin across the whole frozen snapshots: **any** declaring
/// node/run makes it required, and it counts as mounted by default when **some**
/// declaration would mount it with no Session switch (it is required, or its
/// `default` is not explicitly false). For a single declaring node this is
/// exactly what the mount filter does for that node.
///
/// A Session with no attached Run froze no declarations. Then nothing is reported
/// as required, and `on` follows only the Session's own switches: unknown is
/// reported as "not mounted by any Run's declaration", never as a promise.
///
/// Note for readers of the audit trail: mounting has two distinct sources. A
/// Plugin the Session switched on is authorized by the human's click plus the
/// `session.plugin_mount.changed` event; a Plugin mounted because a declaration
/// says so (required, or `default`) was never clicked and writes no such event.
fn session_plugin_declarations(
    state: &ApiState,
    session: &anchor_platform_session::Session,
) -> BTreeMap<String, PluginDeclaration> {
    let configured_pilot_graph = configured_pilot_graph();
    let mut declarations: BTreeMap<String, PluginDeclaration> = BTreeMap::new();
    for run in &session.run_ids {
        let Ok(Some(view)) = crate::application::run_access::load(&state.data_root, run) else {
            continue;
        };
        let record = view.progress();
        // A Run that declares no Plugin cannot contribute a declaration, so its
        // Graph name does not matter either.
        if record
            .snapshot
            .nodes
            .iter()
            .all(|node| node.plugins.is_empty())
        {
            continue;
        }
        // The same comparison, from the same two facts, as `mounted_plugins`: the
        // Run's own recorded Graph name, and the deployment's configured pilot
        // Graph.
        let declaring_graph = crate::run_metadata::load(&state.data_root, run)
            .ok()
            .flatten()
            .map(|metadata| metadata.graph);
        for node in &record.snapshot.nodes {
            crate::plugin_library::merge_declarations(
                &mut declarations,
                &crate::plugin_library::mount_declarations(
                    &node.plugins,
                    declaring_graph.as_deref(),
                    configured_pilot_graph.as_deref(),
                ),
            );
        }
    }
    declarations
}

/// One installed Plugin's row for the Session view.
///
/// `available` is a report, not an action: this reads whether the view the
/// Plugin's own declaration maps to is usable right now, and never builds,
/// repairs, downloads or installs anything.
fn session_plugin_row(
    installed: &crate::application::plugins::catalog::InstalledPlugin,
    declaration: Option<PluginDeclaration>,
    mounts: &BTreeMap<String, bool>,
) -> Value {
    let declaration = declaration.unwrap_or_default();
    let required = declaration.required;
    // The Session's own switch wins; otherwise the frozen declaration decides,
    // exactly as the mount filter does for the node that declaration belongs to.
    let on = required
        || mounts
            .get(&installed.id)
            .copied()
            .unwrap_or(declaration.mounts_by_default);
    let definition = installed.metadata.as_ref();
    let mut row = json!({
        "id": installed.id,
        "name": definition.map(|definition| definition.name.as_str()).unwrap_or(&installed.id),
        "description": definition.map(|definition| definition.description.as_str()).unwrap_or_default(),
        "skills": definition.map(|definition| definition.skills.clone()).unwrap_or_default(),
        // Named apart from the `/plugins` catalog's `mcpServers` object: this one
        // is the plain list of server names the Session row shows.
        "mcpServerNames": definition.into_iter().flat_map(|definition| &definition.mcp_servers)
            .map(|server| server.name.clone())
            .collect::<Vec<_>>(),
        "required": required,
        "on": on,
        "requiresNetwork": definition.is_some_and(crate::plugin_network::metadata_requires_network),
        "networkProvider": installed.id == crate::plugin_network::NETWORK_PLUGIN,
    });
    installed.project_health(&mut row);
    row
}

/// Library facts plus this Session's policy. Broken assets and stale enabled
/// references remain visible so the owner can revoke them.
#[allow(clippy::result_large_err)]
fn session_plugin_rows(
    state: &ApiState,
    session: &anchor_platform_session::Session,
) -> Result<Vec<Value>, HttpResponse> {
    let root = state.application.library_root().to_path_buf();
    let mut catalog = crate::application::plugins::catalog::InstalledPluginCatalog::read(&root)
        .map_err(application_error)?;
    let declarations = session_plugin_declarations(state, session);
    for id in declarations.keys().chain(
        session
            .plugin_mounts
            .iter()
            .filter(|(_, on)| **on)
            .map(|(id, _)| id),
    ) {
        if !catalog.entries.iter().any(|entry| &entry.id == id) {
            catalog.entries.push(
                crate::application::plugins::catalog::InstalledPlugin::unavailable(id.clone()),
            );
        }
    }
    let mut rows = Vec::new();
    let mut definitions = BTreeMap::new();
    for entry in &catalog.entries {
        rows.push(session_plugin_row(
            entry,
            declarations.get(&entry.id).copied(),
            &session.plugin_mounts,
        ));
        if let Some(definition) = &entry.metadata {
            definitions.insert(entry.id.clone(), definition.clone());
        }
    }
    let mounted = rows
        .iter()
        .filter(|row| row["on"] == true && row["available"] == true)
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let network = crate::plugin_network::resolve_metadata(
        &root,
        &definitions,
        &mounted,
        session.network_allowed,
    );
    for row in &mut rows {
        let id = row["id"].as_str().unwrap_or_default().to_owned();
        row["explicitOn"] = row["on"].clone();
        row["networkAllowed"] = json!(network.allowed);
        row["networkPermission"] = json!(session.network_allowed);
        if id == crate::plugin_network::NETWORK_PLUGIN {
            row["on"] = json!(network.enabled);
            row["enabledBy"] = json!(network.enabled_by);
        }
        if let Some(problem) = network.errors.get(&id) {
            row["available"] = json!(false);
            row["contextAuthorized"] = json!(false);
            row["error"] = json!(problem);
        }
    }
    rows.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    Ok(rows)
}

/// Which installed Plugins this Session can mount, and which it already has on.
#[allow(clippy::result_large_err)]
async fn session_plugins(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let (_, _, session) = owned_session(&state, &headers, &identifier)?;
        let plugins = session_plugin_rows(&state, &session)?;
        Ok(Json(json!({"plugins": plugins})))
    })
    .await
}

/// One Plugin's mount switch for a Session.
///
/// The authorization is this click plus the audit event the store writes with
/// it. Mounting never installs: the Plugin must already be installed with a
/// usable view, and a Plugin the Graph declares `required` cannot be switched
/// off at all.
#[allow(clippy::result_large_err)]
async fn set_session_plugin(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((identifier, plugin)): AxumPath<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, HttpResponse> {
    let fields = body
        .as_object()
        .filter(|fields| fields.len() == 1 && fields.contains_key("on"))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "provide on only"))?;
    let on = fields
        .get("on")
        .and_then(Value::as_bool)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "on must be a boolean"))?;
    if !anchor_runtime::identifier::is_plugin_id(&plugin) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            format!("插件 id `{plugin}` 不合法"),
        ));
    }
    let pilot_guard = state.pilots.plugin_mount_guard().await;
    let catalog_guard = state.application.graph_catalog_mutation_guard().await;
    // Known refusals must not create a Library or its publication lock. This
    // read-only preflight is repeated after taking the lease because a writer
    // from another host may replace/uninstall the Plugin while we wait.
    blocking({
        let (state, headers, identifier, plugin) = (
            state.clone(),
            headers.clone(),
            identifier.clone(),
            plugin.clone(),
        );
        move || {
            session_plugin_mount_target(&state, &headers, &identifier, &plugin, on)?;
            Ok(())
        }
    })
    .await?;
    let lease = state
        .application
        .plugin_reference_lease()
        .await
        .map_err(application_error)?;
    blocking(move || {
        // Retain both guards even if validation or the transaction fails.
        let (pilot_guard, catalog_guard, lease) = (pilot_guard, catalog_guard, lease);
        let (sessions, owner) =
            session_plugin_mount_target(&state, &headers, &identifier, &plugin, on)?;
        let before = sessions.get(&owner, &identifier).map_err(session_error)?;
        let changed = before.plugin_mounts.get(&plugin) != Some(&on);
        let updated = sessions
            .set_plugin_mount(
                &owner,
                &identifier,
                &plugin,
                on,
                &owner,
                "the Session view switched this Plugin",
            )
            .map_err(session_error)?;
        let (effect, continuation) = pilot_guard
            .effect(&sessions, &owner, &updated, &state.data_root, changed)
            .map_err(session_error)?;
        drop(lease);
        drop(catalog_guard);
        drop(pilot_guard);
        let rows = session_plugin_rows(&state, &updated)?;
        let row = rows
            .iter()
            .find(|row| row["id"] == plugin)
            .cloned()
            .unwrap_or_else(|| {
                session_plugin_row(
                    &crate::application::plugins::catalog::InstalledPlugin::unavailable(
                        plugin.clone(),
                    ),
                    None,
                    &updated.plugin_mounts,
                )
            });
        Ok(Json(
            json!({"plugin": row, "plugins": rows, "effect":effect, "continuation":continuation}),
        ))
    })
    .await
}

#[allow(clippy::result_large_err)]
fn session_plugin_mount_target(
    state: &ApiState,
    headers: &HeaderMap,
    identifier: &str,
    plugin: &str,
    on: bool,
) -> Result<(anchor_platform_session::SessionStore, String), HttpResponse> {
    let (sessions, owner, session) = owned_session(state, headers, identifier)?;
    if session.status == anchor_platform_session::SessionStatus::Archived {
        return Err(error(
            StatusCode::CONFLICT,
            "该会话已归档，不能再改变插件挂载",
        ));
    }
    let declaration = session_plugin_declarations(state, &session)
        .get(plugin)
        .copied();
    if declaration.is_some_and(|declaration| declaration.required) {
        return Err(error(
            StatusCode::CONFLICT,
            "该插件在图里被声明为必需（required: true）",
        ));
    }
    if on {
        let root = state.application.library_root().to_path_buf();
        Catalog::new(&root).definition(plugin).map_err(|_| {
            error(
                StatusCode::BAD_REQUEST,
                format!("插件 `{plugin}` 尚未安装，不能挂载"),
            )
        })?;
        if let Err(unusable) = crate::plugin_view::prepare_for_plugin(&root, plugin) {
            return Err(error(StatusCode::CONFLICT, unusable.to_string()));
        }
    } else if !session.plugin_mounts.contains_key(plugin) && declaration.is_none() {
        return Err(error(StatusCode::BAD_REQUEST, "该会话没有此插件的挂载配置"));
    }
    Ok((sessions, owner))
}

#[allow(clippy::result_large_err)]
async fn set_session_network(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, HttpResponse> {
    blocking(move || {
        let fields = body
            .as_object()
            .filter(|fields| fields.len() == 1 && fields.contains_key("allowed"))
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "provide allowed only"))?;
        let allowed = fields["allowed"]
            .as_bool()
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "allowed must be a boolean"))?;
        let (sessions, owner, session) = owned_session(&state, &headers, &identifier)?;
        if session.status == anchor_platform_session::SessionStatus::Archived {
            return Err(error(
                StatusCode::CONFLICT,
                "该会话已归档，不能再改变联网权限",
            ));
        }
        let updated = sessions
            .set_network_allowed(&owner, &identifier, allowed, &owner)
            .map_err(session_error)?;
        Ok(Json(
            json!({"plugins": session_plugin_rows(&state, &updated)?}),
        ))
    })
    .await
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
    let wecom_events = Router::new()
        .route("/channels/{graph}/events", post(wecom::receive_event))
        .route(
            "/channels/{graph}/progress/{event_id}",
            get(wecom_progress::wecom_progress),
        )
        .layer(DefaultBodyLimit::max(crate::channel_inputs::MAX_BODY_BYTES))
        .layer(RequestBodyLimitLayer::new(
            crate::channel_inputs::MAX_BODY_BYTES,
        ));
    let pilot_turns = Router::new()
        .route(
            "/sessions/{session}/turns",
            get(list_pilot_turns).post(create_pilot_turn),
        )
        .layer(DefaultBodyLimit::max(crate::attachments::MAX_BODY_BYTES))
        .layer(RequestBodyLimitLayer::new(
            crate::attachments::MAX_BODY_BYTES,
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
        .route("/graph-relations", get(graph_relations))
        .route("/plugins", get(plugin_catalog))
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
        .merge(credential_requests::router())
        .merge(settings::router())
        .merge(channel_management::router())
        .route(
            "/sessions/{session}",
            get(get_session).put(rename_session).delete(delete_session),
        )
        .route("/sessions/{session}/status", post(set_session_status))
        .route("/sessions/{session}/events", get(session_events))
        .route("/sessions/{session}/plugins", get(session_plugins))
        .route("/sessions/{session}/network", post(set_session_network))
        .route(
            "/sessions/{session}/plugins/{plugin}",
            post(set_session_plugin),
        )
        .route(
            "/sessions/{session}/messages",
            get(pilot_messages).post(session_execution_unavailable),
        )
        .route("/sessions/{session}/stop", post(stop_pilot))
        .route("/sessions/{session}/turns/{turn}", get(get_pilot_turn))
        .route(
            "/sessions/{session}/turns/{turn}/attachments/{attachment}",
            get(pilot_attachment),
        )
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
        .route(
            "/channel-sessions/{session}/inbounds/{inbound}",
            get(get_channel_relation),
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
            get(get_channel_session).delete(delete_channel_session),
        )
        .route("/schedules", get(list_schedules).post(create_schedule))
        .route(
            "/schedules/{schedule}",
            axum::routing::delete(delete_schedule).put(update_schedule),
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
        .route("/api/artifacts/{id}/files", get(list_artifact_files))
        .route("/api/artifacts/{id}/files/{*path}", get(read_artifact_file))
        .route("/api/artifacts/{id}/retention", post(retain_artifact))
        .route("/workspaces/bind", post(workspaces::bind_workspace))
        .route(
            "/runs/{id}/workspaces/{*node}",
            get(workspaces::workspace_binding),
        )
        .route(
            "/workspaces/{resource}",
            axum::routing::delete(workspaces::release_workspace),
        )
        .route("/timeline", get(timeline))
        .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))
        .merge(plugin_management::router())
        .merge(pilot_turns)
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
        .layer(middleware::from_fn(secure_private_responses))
}

fn application_error(failure: ApplicationError) -> HttpResponse {
    match failure {
        ApplicationError::Missing => error(StatusCode::NOT_FOUND, "no such run"),
        ApplicationError::Conflict(message) => error(StatusCode::CONFLICT, message),
        ApplicationError::Invalid(message) => error(StatusCode::UNPROCESSABLE_ENTITY, message),
        ApplicationError::Storage(message) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
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
