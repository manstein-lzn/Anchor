mod api;
mod application;
mod artifacts;
mod assistant;
mod assistant_plugins;
mod attachments;
mod channel_inputs;
mod channel_notifications;
mod channel_replies;
mod channel_supervisor;
mod channel_tools;
mod dependency_provision;
mod deployment;
mod durable_write;
mod execution;
mod fact_recorder;
mod goose_acp;
mod goose_tool_context;
mod host_fs;
mod local_inputs;
mod pilot_attachments;

mod conversation_resources;
mod credential_store;
mod model_configuration;
mod model_fields;
mod oauth_links;
mod pilot_host;
mod pilot_resources;
mod pilot_tools;
mod plugin_configuration;
mod plugin_configuration_test;
mod plugin_dependencies;
mod plugin_disclosure;
mod plugin_library;
mod plugin_mounts;
mod plugin_network;
mod plugin_view;
mod resource_read;
mod runtime_configuration;
mod runtime_management;
use artifacts::HostArtifacts;
mod node_host;
mod node_tools;
mod oauth_http;
mod op;
mod retention;
mod run_data;
mod run_deletions;
mod run_metadata;
#[cfg(test)]
mod test_support;
mod tool_disclosure;
mod tool_environment;
mod tool_host;
mod tool_names;
mod workspaces;
use durable_write::write_durable;
use node_host::HostNodes;
#[cfg(test)]
use node_host::load_host_completion_fact;

use anchor_runtime::Cancellation;
#[cfg(test)]
use anchor_runtime::graph::{CompletionFact, InvocationKey};
use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot};
use anchor_sandbox_bwrap::{BubblewrapPolicy, BubblewrapSandbox};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, env, io, path::PathBuf, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_FRAME: usize = 1024 * 1024;

#[cfg(test)]
pub(crate) static PROCESS_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn create_durable_directory(path: &std::path::Path) -> io::Result<()> {
    // This general helper follows symlinks; host_fs::directory deliberately does not.
    std::fs::create_dir_all(path)?;
    // Sync directory entries too: syncing a file in a newly created directory
    // does not make that directory's link from its parent durable.
    for ancestor in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::File::open(ancestor)?.sync_all()?;
    }
    if path.is_relative() {
        std::fs::File::open(".")?.sync_all()?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
enum Request {
    #[serde(rename = "start_or_resume")]
    Start {
        version: u32,
        request_id: String,
        run_id: String,
        snapshot: Box<GraphSnapshot>,
        input: Value,
    },
    #[serde(rename = "start_bundle")]
    StartBundle {
        version: u32,
        request_id: String,
        run_id: String,
        input: Value,
    },
    #[serde(rename = "status")]
    Status {
        version: u32,
        request_id: String,
        run_id: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Response {
    #[serde(rename = "run")]
    Run {
        version: u32,
        request_id: String,
        run_id: String,
        projection: Value,
    },
    #[serde(rename = "missing")]
    Missing {
        version: u32,
        request_id: String,
        run_id: String,
    },
    #[serde(rename = "rejected")]
    Rejected {
        version: u32,
        request_id: String,
        reason: String,
    },
}

#[derive(Clone)]
struct HostControl {
    cancellation: Cancellation,
}

fn reject_snapshot(snapshot: &GraphSnapshot) -> Result<(), String> {
    snapshot.validate().map_err(|error| error.to_string())?;
    // Product admission boundary shared by both execution paths (§10.12):
    // Graph calls are supported outside fanout/join branches. Refuse this
    // combination before creating a Run or admitting any child.
    let parallel_regions = snapshot
        .parallel_regions()
        .map_err(|error| error.to_string())?;
    let parallel_nodes = parallel_regions
        .values()
        .flat_map(|region| region.branches.iter().flatten())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for node in &snapshot.nodes {
        if let Some(agent_id) = &node.agent {
            let agent = &snapshot.agents[agent_id];
            if agent.max_steps.is_some() {
                return Err(
                    "exact cumulative provider budgets are not yet supported by this host".into(),
                );
            }
            continue;
        }
        let op = node
            .op
            .as_ref()
            .and_then(|name| snapshot.ops.get(name))
            .ok_or_else(|| "node needs an Agent or Op definition".to_owned())?;
        if parallel_nodes.contains(&node.id) && op.get("call").is_some() {
            return Err(format!(
                "Op.call node `{}` is unsupported inside a fanout/join region",
                node.id
            ));
        }
        if let Some(call) = op.get("call") {
            let fields = call.as_object().ok_or("Op.call must be an object")?;
            // `snapshot.validate()` above already rejects unknown fields and
            // structurally validates input/input_map/files/result. Session
            // handoff is validated by the selected host entrypoint below.
            if fields.get("graph").and_then(Value::as_str).is_none()
                || !matches!(
                    fields.get("mode").and_then(Value::as_str),
                    Some("wait" | "detach")
                )
                || fields.get("input").is_some_and(|input| !input.is_object())
                || op.get("run").is_some()
                || op.get("fanout").is_some()
                || op.get("join").is_some()
                || !node.plugins.is_empty()
            {
                return Err("unsupported Op.call configuration".into());
            }
            continue;
        }
        if op.get("fanout").is_some() || op.get("join").is_some() {
            if !node.plugins.is_empty() {
                return Err(
                    "fanout/join are Coordinator operations and cannot execute Plugins".into(),
                );
            }
            continue;
        }
        if let Some(host) = op.get("host") {
            if !matches!(
                host.get("operation").and_then(Value::as_str),
                Some(crate::assistant::WAIT_INPUT | crate::assistant::REPLY)
            ) || !node.plugins.is_empty()
            {
                return Err("unsupported host operation configuration".into());
            }
            continue;
        }
        let command = op
            .get("run")
            .and_then(Value::as_str)
            .ok_or_else(|| "Op.call requires further host integration".to_owned())?;
        if !node.plugins.is_empty() {
            return Err("Op.run cannot execute Plugins".into());
        }
        if command.trim().is_empty() || command.contains('\0') {
            return Err("Op.run must be a non-empty shell command without NUL bytes".into());
        }
    }
    Ok(())
}

fn reject_standalone_snapshot(snapshot: &GraphSnapshot) -> Result<(), String> {
    reject_snapshot(snapshot)?;
    if snapshot.ops.values().any(|op| op.get("host").is_some()) {
        return Err("Session host operations require a platform assistant instance".into());
    }
    for node in &snapshot.nodes {
        let Some(call) = node
            .op
            .as_ref()
            .and_then(|op_id| snapshot.ops.get(op_id))
            .and_then(|op| op.get("call"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        if call.get("session").is_some() {
            return Err("Op.call session handoff requires a platform application".into());
        }
        if call.get("mode").and_then(Value::as_str) == Some("detach") {
            return Err("Op.call detach requires a persistent background host".into());
        }
    }
    Ok(())
}

/// Deployment paths are read once at the process entry and passed to consumers.
#[derive(Clone)]
pub(crate) struct HostConfiguration {
    pub(crate) state_root: Option<PathBuf>,
    pub(crate) catalog_root: Option<PathBuf>,
    pub(crate) library_root: Option<PathBuf>,
    pub(crate) bwrap: PathBuf,
}

impl Default for HostConfiguration {
    fn default() -> Self {
        Self {
            state_root: None,
            catalog_root: None,
            library_root: None,
            bwrap: "bwrap".into(),
        }
    }
}

impl HostConfiguration {
    fn from_env() -> Self {
        Self {
            state_root: env::var_os("ANCHOR_RUNNER_STATE_ROOT").map(PathBuf::from),
            catalog_root: env::var_os("ANCHOR_RUNNER_CATALOG_ROOT").map(PathBuf::from),
            library_root: env::var_os("ANCHOR_RUNNER_LIBRARY_ROOT").map(PathBuf::from),
            bwrap: env::var_os("ANCHOR_BWRAP")
                .map(PathBuf::from)
                .unwrap_or_else(|| "bwrap".into()),
        }
    }

    fn state_root(&self) -> Result<&std::path::Path, String> {
        self.state_root
            .as_deref()
            .ok_or_else(|| "ANCHOR_RUNNER_STATE_ROOT is required".into())
    }

    fn library_root<'a>(&'a self, catalog_root: &'a std::path::Path) -> &'a std::path::Path {
        self.library_root.as_deref().unwrap_or(catalog_root)
    }

    fn standalone_library_root(&self) -> Option<PathBuf> {
        self.library_root
            .clone()
            .or_else(|| self.catalog_root.clone())
    }
}

fn env_path(name: &str) -> Result<PathBuf, String> {
    env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} is required"))
}

fn acquire_deployment_writer(
    root: &std::path::Path,
) -> Result<Box<dyn anchor_runtime::graph::RunLease>, String> {
    anchor_runtime::graph::FileRunLeaseProvider::new(root.join("deployment-locks"))
        .acquire_lease("deployment-writer")
        .map_err(|error| format!("state root already has a writing host: {error}"))
}
fn make_host_with_control(
    application: &application::RunApplication,
    run_id: &str,
    _control: HostControl,
    library_root: Option<PathBuf>,
    runtime_configuration: Option<Arc<runtime_configuration::RuntimeConfiguration>>,
) -> Result<(HostArtifacts, HostNodes), String> {
    let state = &application.data_root;
    let work_root = env_path("ANCHOR_RUNNER_WORKSPACE_ROOT")?;
    let _ = run_id;
    create_durable_directory(&state.join("artifacts")).map_err(|e| e.to_string())?;
    // Authorized roots are canonicalized when the sandbox is built, so the
    // retention root must exist before then.
    create_durable_directory(&state.join("spill")).map_err(|e| e.to_string())?;
    create_durable_directory(&work_root).map_err(|e| e.to_string())?;
    let commands = env::var("ANCHOR_RUNNER_ALLOWED_COMMANDS")
        .map_err(|_| "ANCHOR_RUNNER_ALLOWED_COMMANDS is required".to_owned())?;
    let policy = BubblewrapPolicy::new(
        application.bwrap.clone(),
        commands
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    )
    .authorize_workspace_root(&work_root)
    .authorize_readonly_input_root(state.join("artifacts"))
    .authorize_readonly_destination_root("/in")
    .authorize_readonly_destination_root("/plugins")
    // Node command output beyond the stream previews is retained here and
    // mounted read-only at /spill.
    .authorize_readonly_destination_root("/spill")
    .authorize_spill_root(state.join("spill"))
    .allow_network();
    // Each invocation authorizes its current Plugin views and mutable paths;
    // the deployment policy does not grant the entire Library.
    let tool_environment = tool_environment::ToolEnvironment::for_library(
        application.library_root_override.as_deref(),
    )?;
    let sandbox = Arc::new(
        BubblewrapSandbox::new(tool_environment.authorize(policy)).map_err(|e| e.to_string())?,
    );
    let fake_plugin_ids = env::var("ANCHOR_RUNNER_FAKE_PLUGINS")
        .unwrap_or_default()
        .split(',')
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_owned())
        .collect::<Vec<_>>();
    let mut mcp = tool_host::McpToolConfig::default();
    mcp.environment = tool_environment;
    let artifacts = HostArtifacts::new(state.join("artifacts"), work_root.clone());
    let tools = tool_host::PluginToolHost::new(fake_plugin_ids);
    // The Session store is the platform's; this host only asks it what the
    // Session mounted. The resolver never opens that database itself.
    let session_mounts = Arc::new(plugin_mounts::SessionMounts::open(state)?);
    let io_resolver = Arc::new(
        node_host::HostIoResolver::new(
            artifacts.clone(),
            Arc::clone(&sandbox),
            Arc::new(tools.clone()),
            mcp.clone(),
            local_inputs::LocalInputs::from_env(state.clone())?,
            session_mounts,
            deployment::configured_pilot_graph(),
        )
        .with_library_root(library_root)
        .with_runtime_configuration(runtime_configuration),
    );
    let goose_nodes = goose_acp::GooseNodePort::from_env(
        state,
        &work_root,
        Arc::clone(&io_resolver),
        Arc::clone(&sandbox),
    )?;
    Ok((
        artifacts.clone(),
        HostNodes {
            sandbox,
            allowed_commands: commands
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            artifacts,
            facts_root: state.join("facts"),
            io_resolver,
            goose_nodes,
        },
    ))
}

async fn standalone_response(
    application: &application::RunApplication,
    view: application::run_access::RunView<'static>,
    run_id: String,
    request_id: String,
) -> Response {
    let projected = async {
        let metadata = application.metadata(&run_id)?;
        let projection = application.project_run(&view, metadata.as_ref()).await?;
        serde_json::to_value(projection)
            .map_err(|error| application::ApplicationError::Storage(error.to_string()))
    }
    .await;
    match projected {
        Ok(projection) => Response::Run {
            version: 1,
            request_id,
            run_id,
            projection,
        },
        Err(error) => Response::Rejected {
            version: 1,
            request_id,
            reason: format!("{error:?}"),
        },
    }
}

fn standalone_application(
    configuration: &Arc<HostConfiguration>,
) -> Result<application::RunApplication, String> {
    let state = configuration.state_root()?.to_path_buf();
    let catalog = configuration
        .catalog_root
        .clone()
        .unwrap_or_else(|| state.clone());
    // The same runtime configuration `serve` uses: nodes run the Python and
    // Rust the deployment configured, not whatever the sandbox's system
    // directories happen to resolve.
    // Without a configured Library there is none to share; dependency
    // preparation then gets the same disjoint default `serve` uses.
    let dependency_library = configuration
        .standalone_library_root()
        .unwrap_or_else(|| state.join("catalog"));
    let runtime_configuration = runtime_configuration::RuntimeConfiguration::from_env(
        &state,
        &dependency_library,
        configuration.bwrap.clone(),
    )?;
    Ok(application::RunApplication::new(state, catalog)
        .with_host_configuration(configuration.clone())
        .with_runtime_configuration(Arc::new(runtime_configuration)))
}

async fn handle(request: Request, configuration: &Arc<HostConfiguration>) -> Response {
    match request {
        Request::StartBundle {
            version,
            request_id,
            run_id,
            input,
        } => {
            if version != 1 {
                return Response::Rejected {
                    version: 1,
                    request_id,
                    reason: "unsupported protocol version".into(),
                };
            }
            let bundle_root = match env_path("ANCHOR_RUNNER_BUNDLE_ROOT") {
                Ok(root) => root,
                Err(reason) => {
                    return Response::Rejected {
                        version: 1,
                        request_id,
                        reason,
                    };
                }
            };
            let mut loader = anchor_graph_host::FileGraphBundleLoader::new(bundle_root.clone());
            if let Some(root) = configuration.standalone_library_root() {
                loader = loader.with_library_root(root);
            }
            let bundle = match loader.load() {
                Ok(bundle) => bundle,
                Err(error) => {
                    return Response::Rejected {
                        version: 1,
                        request_id,
                        reason: error.to_string(),
                    };
                }
            };
            if let Err(reason) = reject_standalone_snapshot(&bundle.snapshot) {
                return Response::Rejected {
                    version: 1,
                    request_id,
                    reason,
                };
            }
            execute_snapshot(
                configuration,
                bundle.snapshot,
                input,
                run_id,
                request_id,
                Some(bundle_root),
            )
            .await
        }
        Request::Status {
            version,
            request_id,
            run_id,
        } => {
            if version != 1 {
                return Response::Rejected {
                    version: 1,
                    request_id,
                    reason: "unsupported protocol version".into(),
                };
            }
            let data_root = match configuration.state_root() {
                Ok(root) => root,
                Err(reason) => {
                    return Response::Rejected {
                        version: 1,
                        request_id,
                        reason,
                    };
                }
            };
            match application::run_access::status_projection(data_root, &run_id) {
                Ok(Some(projection)) => Response::Run {
                    version: 1,
                    request_id,
                    run_id,
                    projection,
                },
                Ok(None) => Response::Missing {
                    version: 1,
                    request_id,
                    run_id,
                },
                Err(error) => Response::Rejected {
                    version: 1,
                    request_id,
                    reason: format!("{error:?}"),
                },
            }
        }
        Request::Start {
            version,
            request_id,
            run_id,
            snapshot,
            input,
        } => {
            if version != 1 {
                return Response::Rejected {
                    version: 1,
                    request_id,
                    reason: "unsupported protocol version".into(),
                };
            }
            if let Err(reason) = reject_standalone_snapshot(&snapshot) {
                return Response::Rejected {
                    version: 1,
                    request_id,
                    reason,
                };
            }
            execute_snapshot(configuration, *snapshot, input, run_id, request_id, None).await
        }
    }
}

async fn execute_snapshot(
    configuration: &Arc<HostConfiguration>,
    snapshot: GraphSnapshot,
    input: Value,
    run_id: String,
    request_id: String,
    bundle_root: Option<PathBuf>,
) -> Response {
    execute_snapshot_with_control(
        configuration,
        snapshot,
        input,
        run_id,
        request_id,
        None,
        bundle_root,
    )
    .await
}

async fn execute_snapshot_with_control(
    configuration: &Arc<HostConfiguration>,
    snapshot: GraphSnapshot,
    input: Value,
    run_id: String,
    request_id: String,
    supplied_control: Option<HostControl>,
    bundle_root: Option<PathBuf>,
) -> Response {
    let _writer = match acquire_deployment_writer(match configuration.state_root() {
        Ok(root) => root,
        Err(reason) => {
            return Response::Rejected {
                version: 1,
                request_id,
                reason,
            };
        }
    }) {
        Ok(lease) => lease,
        Err(reason) => {
            return Response::Rejected {
                version: 1,
                request_id,
                reason,
            };
        }
    };
    if let (Ok(data_root), Ok(workspace_root)) = (
        configuration.state_root().map(std::path::Path::to_path_buf),
        env_path("ANCHOR_RUNNER_WORKSPACE_ROOT"),
    ) {
        durable_write::cleanup_at_startup(&data_root, &workspace_root, || {
            crate::application::RunApplication::new(data_root.clone(), data_root.clone())
                .durable_input_workspaces(&workspace_root)
                .ok()
        });
    }
    let expected = match GraphRunRecord::create_with_id(snapshot, input, run_id.clone()) {
        Ok(record) => record,
        Err(error) => {
            return Response::Rejected {
                version: 1,
                request_id,
                reason: error.to_string(),
            };
        }
    };
    let control = supplied_control.unwrap_or_else(|| HostControl {
        cancellation: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    let library_root = configuration.standalone_library_root();
    let plugin_ids = expected
        .snapshot
        .nodes
        .iter()
        .flat_map(|node| node.plugins.iter().map(|plugin| plugin.id.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let reference_lease = if plugin_ids.is_empty() {
        None
    } else {
        let Some(root) = library_root.as_ref() else {
            return Response::Rejected {
                version: 1,
                request_id,
                reason: "Plugin admission requires the Library root".into(),
            };
        };
        let access = crate::dependency_provision::DependencyAccess {
            network: false,
            cancellation: control.cancellation.clone(),
        };
        match crate::plugin_library::publication_lease(
            root,
            &access,
            "standalone reference publication",
        )
        .await
        {
            Ok(lease) => Some(lease),
            Err(error) => {
                return Response::Rejected {
                    version: 1,
                    request_id,
                    reason: format!("Plugin admission failed: {error:?}"),
                };
            }
        }
    };
    // Resolve under the publication lease and hold it until the durable Run
    // reference is saved, so uninstall cannot pass between those two facts.
    let plugin_bindings = match library_root.as_ref() {
        Some(root) => {
            match crate::application::graphs::referenced_plugin_bindings(root, &plugin_ids) {
                Ok(bindings) => bindings,
                Err(error) => {
                    return Response::Rejected {
                        version: 1,
                        request_id,
                        reason: error.to_string(),
                    };
                }
            }
        }
        None => BTreeMap::new(),
    };
    let expected = match GraphRunRecord::create_with_plugins(
        expected.snapshot,
        expected.input,
        run_id.clone(),
        plugin_bindings.values().cloned().collect(),
    ) {
        Ok(record) => record,
        Err(error) => {
            return Response::Rejected {
                version: 1,
                request_id,
                reason: error.to_string(),
            };
        }
    };
    let application = match standalone_application(configuration) {
        Ok(application) => application,
        Err(reason) => {
            return Response::Rejected {
                version: 1,
                request_id,
                reason,
            };
        }
    };
    // Reject retired storage and conflicting identities before publishing any
    // metadata or frozen input authorization for this request.
    match application.execution_record(&run_id) {
        Ok(Some(record))
            if record.snapshot != expected.snapshot || record.input != expected.input =>
        {
            return Response::Rejected {
                version: 1,
                request_id,
                reason: "run identity is already bound to a different snapshot or input".into(),
            };
        }
        Ok(_) => {}
        Err(error) => {
            return Response::Rejected {
                version: 1,
                request_id,
                reason: format!("{error:?}"),
            };
        }
    }
    if expected
        .snapshot
        .ops
        .values()
        .any(|op| op.get("call").is_some())
        && configuration.catalog_root.is_none()
    {
        return Response::Rejected {
            version: 1,
            request_id,
            reason: "ANCHOR_RUNNER_CATALOG_ROOT is required for Op.call".into(),
        };
    }
    if let Err(reason) =
        ensure_standalone_run_metadata(configuration, &expected, bundle_root.as_deref())
    {
        return Response::Rejected {
            version: 1,
            request_id,
            reason,
        };
    }
    match application
        .execute_standalone(expected, control.cancellation, reference_lease)
        .await
    {
        Ok(view) => standalone_response(&application, view, run_id, request_id).await,
        Err(error) => Response::Rejected {
            version: 1,
            request_id,
            reason: format!("{error:?}"),
        },
    }
}

/// Persist the manual Run identity metadata a framed/standalone top-level Run
/// needs before the shared Kernel can admit an `Op.call` child. `HostGraphCatalog`
/// fails closed when the parent Run has no durable source metadata, so a
/// `start_bundle`/`start` Run records the same manual identity the HTTP
/// RunApplication writes. This is idempotent, verifies graph/source/digest on
/// replay, and never fabricates metadata for a Graph-call child.
fn ensure_standalone_run_metadata(
    configuration: &HostConfiguration,
    record: &GraphRunRecord,
    bundle_root: Option<&std::path::Path>,
) -> Result<(), String> {
    if env::var_os("ANCHOR_RUNNER_LOCAL_INPUTS_ROOT").is_none()
        && !record
            .snapshot
            .ops
            .values()
            .any(|op| op.get("call").is_some())
    {
        return Ok(());
    }
    let bundle_root = match bundle_root {
        Some(root) => root.to_path_buf(),
        None => match env::var_os("ANCHOR_RUNNER_BUNDLE_ROOT") {
            Some(root) => PathBuf::from(root),
            None => {
                return Err(
                    "op.call requires ANCHOR_RUNNER_BUNDLE_ROOT to record the parent Run source"
                        .into(),
                );
            }
        },
    };
    let graph_name = env::var("ANCHOR_RUNNER_GRAPH_NAME")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    if env::var_os("ANCHOR_RUNNER_LOCAL_INPUTS_ROOT").is_some() && graph_name.is_none() {
        return Err(
            "ANCHOR_RUNNER_GRAPH_NAME is required with ANCHOR_RUNNER_LOCAL_INPUTS_ROOT".into(),
        );
    }
    let graph = graph_name
        .or_else(|| {
            bundle_root
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .ok_or_else(|| "cannot derive the standalone parent Graph name".to_owned())?;
    let source = bundle_root
        .canonicalize()
        .map_err(|error| format!("standalone bundle source unavailable: {error}"))?;
    let state = configuration.state_root()?.to_path_buf();
    let expected = crate::run_metadata::RunMetadata::new(
        record.run_id.clone(),
        graph,
        record.graph_digest.clone(),
        &source,
    )
    .map_err(|error| format!("{error:?}"))?;
    if let Some(existing) =
        crate::run_metadata::load(&state, &record.run_id).map_err(|e| format!("{e:?}"))?
    {
        if existing.trigger_source == "graph_call" {
            return Err("standalone Run id collides with a Graph-call child identity".into());
        }
        if existing.graph != expected.graph
            || existing.graph_digest != expected.graph_digest
            || existing.bundle_source != expected.bundle_source
        {
            return Err("standalone Run identity metadata conflicts with the accepted Run".into());
        }
        return Ok(());
    }
    crate::run_metadata::save(&state, &expected).map_err(|error| format!("{error:?}"))
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let size = u32::from_be_bytes(len) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds limit",
        ));
    }
    let mut data = vec![0; size];
    reader.read_exact(&mut data).await?;
    Ok(Some(data))
}
async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, value: &Response) -> io::Result<()> {
    let data = serde_json::to_vec(value).map_err(io::Error::other)?;
    if data.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "response exceeds limit",
        ));
    }
    writer.write_all(&(data.len() as u32).to_be_bytes()).await?;
    writer.write_all(&data).await?;
    writer.flush().await
}

#[tokio::main]
async fn main() -> io::Result<()> {
    if let Some(args) = op::route_cli_args() {
        std::process::exit(op::run_route_cli(&args));
    }
    let configuration = Arc::new(HostConfiguration::from_env());
    if env::args().nth(1).as_deref() == Some("serve") {
        return api::serve(configuration).await;
    }
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = stdin;
    let mut writer = stdout;
    while let Some(frame) = read_frame(&mut reader).await? {
        let response = match serde_json::from_slice::<Request>(&frame) {
            Ok(request) => handle(request, &configuration).await,
            Err(error) => Response::Rejected {
                version: 1,
                request_id: "unknown".into(),
                reason: format!("invalid request: {error}"),
            },
        };
        write_frame(&mut writer, &response).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn configured_library(
        catalog: &std::path::Path,
        library_root: Option<PathBuf>,
        configured_catalog: Option<PathBuf>,
    ) -> PathBuf {
        let catalog = configured_catalog.unwrap_or_else(|| catalog.to_path_buf());
        application::RunApplication::new(PathBuf::new(), catalog)
            .with_host_configuration(Arc::new(HostConfiguration {
                library_root,
                ..Default::default()
            }))
            .library_root()
            .to_path_buf()
    }

    #[test]
    fn oauth_token_store_follows_the_deployment_library_root() {
        let catalog = std::path::Path::new("/srv/anchor/catalog");
        // The deployment's Library root, when it configured one.
        assert_eq!(
            configured_library(
                catalog,
                Some(PathBuf::from("/srv/anchor/library")),
                Some(PathBuf::from("/srv/anchor/catalog")),
            ),
            PathBuf::from("/srv/anchor/library")
        );
        // Otherwise the Graph catalog it configured — never a path inferred from the
        // bundle, which is where the Graph is, not where a Plugin is.
        assert_eq!(configured_library(catalog, None, None), catalog);
        assert_eq!(
            configured_library(
                catalog,
                None,
                Some(PathBuf::from("/srv/anchor/catalog/bundles/main")),
            ),
            PathBuf::from("/srv/anchor/catalog/bundles/main")
        );
    }

    #[test]
    fn invocation_filenames_preserve_the_durable_key_hash_bytes() {
        let key = InvocationKey {
            run_id: "run-1".into(),
            graph_digest: "digest".into(),
            node_id: "branch/work".into(),
            invocation: 7,
        };
        assert_eq!(
            host_fs::invocation_stem(&key),
            "542df885ca2f936d298143ede4b5c911630784e4b567b799524ed60302b9d540"
        );
    }

    /// A standalone Run carries the deployment's runtime configuration, so
    /// its nodes get the configured Python rather than the sandbox's system
    /// one. Before, `start_bundle` built its application without it and an
    /// AgentNode's `python3` resolved to whatever `/usr/bin` held.
    #[test]
    fn a_standalone_run_carries_the_configured_python() {
        let Some(python) = [
            "/usr/bin/python3.12",
            "/usr/bin/python3.11",
            "/usr/bin/python3.10",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file()) else {
            eprintln!("SKIPPED (no verification): no system Python 3.8+ is available");
            return;
        };
        let _env = PROCESS_ENV_LOCK.blocking_lock();
        let root = tempdir().unwrap();
        let saved: Vec<_> = ["ANCHOR_DEPENDENCIES_PYTHON", "ANCHOR_PILOT_PYTHON_ENABLED"]
            .into_iter()
            .map(|name| (name, env::var_os(name)))
            .collect();
        let set = |name: &str, value: &std::ffi::OsStr| unsafe { env::set_var(name, value) };
        set("ANCHOR_DEPENDENCIES_PYTHON", python.as_os_str());
        set("ANCHOR_PILOT_PYTHON_ENABLED", "1".as_ref());
        let snapshot = standalone_application(&Arc::new(HostConfiguration {
            state_root: Some(root.path().join("state")),
            catalog_root: Some(root.path().join("catalog")),
            library_root: Some(root.path().join("library")),
            ..Default::default()
        }))
        .and_then(|application| {
            application
                .runtime_snapshot()
                .map_err(|error| format!("{error:?}"))
        });
        for (name, value) in saved {
            unsafe {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
        let snapshot = snapshot.unwrap().expect("a runtime snapshot");
        assert_eq!(snapshot.python.as_deref(), Some(python.as_path()));
        assert!(snapshot.python_enabled);
    }

    #[test]
    fn write_durable_preserves_creation_and_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().unwrap();
        let ordinary = directory.path().join("ordinary");
        let atomic = directory.path().join("atomic");
        std::fs::write(&ordinary, b"ordinary").unwrap();
        write_durable(&atomic, b"original").unwrap();
        assert_eq!(
            std::fs::metadata(&ordinary).unwrap().permissions().mode(),
            std::fs::metadata(&atomic).unwrap().permissions().mode()
        );
        std::fs::set_permissions(&atomic, std::fs::Permissions::from_mode(0o640)).unwrap();
        write_durable(&atomic, b"replacement").unwrap();
        assert_eq!(std::fs::read(&atomic).unwrap(), b"replacement");
        assert_eq!(
            std::fs::metadata(&atomic).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn write_durable_failed_publication_cleans_temporary_file() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("occupied");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("retained"), b"original").unwrap();
        assert!(write_durable(&target, b"replacement").is_err());
        assert_eq!(std::fs::read(target.join("retained")).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    fn snapshot() -> GraphSnapshot {
        serde_json::from_value(serde_json::json!({
            "objective": "fixture",
            "entry": "command",
            "ops": {"run": {"run": "printf fixture"}},
            "nodes": [{"id": "command", "op": "run"}],
            "edges": []
        }))
        .expect("fixture snapshot")
    }

    #[test]
    fn host_admission_rejects_invalid_agent_and_accepts_serial_edges() {
        let mut graph = snapshot();
        graph.nodes[0].agent = Some("worker".into());
        assert!(reject_snapshot(&graph).is_err());
        let mut graph = snapshot();
        graph.edges.push(anchor_runtime::graph::GraphEdge {
            from_node: "command".into(),
            to_node: "command".into(),
        });
        assert!(reject_snapshot(&graph).is_ok());
    }

    #[test]
    fn host_admission_accepts_declared_agent_file_interfaces() {
        let graph: GraphSnapshot = serde_json::from_value(serde_json::json!({
            "objective": "academic fixture",
            "entry": "plan",
            "agents": {
                "planner": {
                    "model": "models.academic",
                    "instructions": "write a plan",
                    "reads": [],
                    "writes": ["plan.md"]
                },
                "writer": {
                    "model": "models.academic",
                    "instructions": "write the paper",
                    "reads": ["plan.md"],
                    "writes": ["paper.md"]
                }
            },
            "nodes": [
                {"id": "plan", "agent": "planner"},
                {"id": "write", "agent": "writer"}
            ],
            "edges": [{"from": "plan", "to": "write"}]
        }))
        .expect("fixture snapshot");

        assert!(reject_snapshot(&graph).is_ok());
    }

    #[test]
    fn host_admission_accepts_op_routes_with_multiple_exits() {
        let mut graph = snapshot();
        graph.nodes.push(anchor_runtime::graph::GraphNode {
            id: "good".into(),
            agent: None,
            op: Some("run".into()),
            input: None,
            plugins: Vec::new(),
            max_rounds: None,
        });
        graph.nodes.push(anchor_runtime::graph::GraphNode {
            id: "bad".into(),
            agent: None,
            op: Some("run".into()),
            input: None,
            plugins: Vec::new(),
            max_rounds: None,
        });
        graph.edges.extend([
            anchor_runtime::graph::GraphEdge {
                from_node: "command".into(),
                to_node: "good".into(),
            },
            anchor_runtime::graph::GraphEdge {
                from_node: "command".into(),
                to_node: "bad".into(),
            },
        ]);
        assert!(reject_snapshot(&graph).is_ok());
    }

    #[test]
    fn host_admission_accepts_frozen_op_call_fields_and_session() {
        let graph = GraphSnapshot::admit(serde_json::json!({
            "objective":"parent", "entry":"call", "agents":{},
            "ops":{"call":{"call":{
                "graph":"child","mode":"wait","input":{"a":1},
                "input_map":{"b":"/b"},
                "files":[{"node":"up","path":"r.md","as":"input/r.md"}],
                "result":{"node":"answer","files":["answer.md"]}
            }}},
            "nodes":[{"id":"call","op":"call","plugins":[]}], "edges":[]
        }))
        .unwrap();
        assert!(reject_snapshot(&graph).is_ok());
        let session = GraphSnapshot::admit(serde_json::json!({
            "objective":"parent", "entry":"call", "agents":{},
            "ops":{"call":{"call":{"graph":"child","mode":"wait","session":"ops"}}},
            "nodes":[{"id":"call","op":"call","plugins":[]}], "edges":[]
        }))
        .unwrap();
        assert!(reject_snapshot(&session).is_ok());
    }

    #[test]
    fn standalone_admission_rejects_detached_graph_calls_but_host_admits_them() {
        let graph = GraphSnapshot::admit(serde_json::json!({
            "objective":"detached child", "entry":"call", "agents":{},
            "ops":{"call":{"call":{"graph":"child","mode":"detach"}}},
            "nodes":[{"id":"call","op":"call","plugins":[]}], "edges":[]
        }))
        .unwrap();

        assert!(
            reject_snapshot(&graph).is_ok(),
            "persistent HTTP host supports detach"
        );
        let error = reject_standalone_snapshot(&graph)
            .expect_err("standalone host cannot own a persistent detached child");
        assert!(error.contains("persistent background host"), "{error}");
    }

    #[test]
    fn standalone_admission_rejects_session_calls_before_run_creation() {
        let graph = GraphSnapshot::admit(serde_json::json!({
            "objective":"session child", "entry":"call", "agents":{},
            "ops":{"call":{"call":{"graph":"child","mode":"wait","session":"ops"}}},
            "nodes":[{"id":"call","op":"call","plugins":[]}], "edges":[]
        }))
        .unwrap();

        assert!(
            reject_snapshot(&graph).is_ok(),
            "platform host owns Session calls"
        );
        let error = reject_standalone_snapshot(&graph)
            .expect_err("standalone entrypoint has no Session delivery authority");
        assert!(error.contains("platform application"), "{error}");
    }

    #[test]
    fn host_admission_rejects_graph_call_inside_parallel_branch() {
        let graph = GraphSnapshot::admit(serde_json::json!({
            "objective": "parallel call",
            "entry": "fork",
            "ops": {
                "fork": {"fanout": {"join": "join"}},
                "call": {"call": {"graph": "child", "mode": "wait"}},
                "work": {"run": "true"},
                "join": {"join": {}}
            },
            "nodes": [
                {"id": "fork", "op": "fork"},
                {"id": "call", "op": "call"},
                {"id": "work", "op": "work"},
                {"id": "join", "op": "join"}
            ],
            "edges": [
                {"from": "fork", "to": "call"},
                {"from": "fork", "to": "work"},
                {"from": "call", "to": "join"},
                {"from": "work", "to": "join"}
            ]
        }))
        .expect("parallel graph shape");

        let error = reject_snapshot(&graph).expect_err("unsupported combination must fail closed");
        assert!(error.contains("inside a fanout/join region"), "{error}");
    }

    #[tokio::test]
    async fn protocol_version_is_checked_before_host_setup() {
        let response = handle(
            Request::Status {
                version: 2,
                request_id: "req".into(),
                run_id: "run".into(),
            },
            &Arc::new(HostConfiguration::default()),
        )
        .await;
        assert!(
            matches!(response, Response::Rejected { reason, .. } if reason.contains("version"))
        );
    }

    #[test]
    fn started_op_without_terminal_fact_is_uncertain_and_never_not_started() {
        let tmp = tempdir().unwrap();
        let key = InvocationKey {
            run_id: "run-1".into(),
            graph_digest: "digest".into(),
            node_id: "command".into(),
            invocation: 1,
        };
        assert_eq!(
            load_host_completion_fact(tmp.path(), &key).unwrap(),
            CompletionFact::NotStarted
        );
        let stem = key.durable_key().replace(':', "_");
        std::fs::write(tmp.path().join(format!("{stem}.started")), b"started\n").unwrap();
        assert!(matches!(
            load_host_completion_fact(tmp.path(), &key).unwrap(),
            CompletionFact::Uncertain(reason) if reason.contains("durably started")
        ));
        std::fs::write(tmp.path().join(format!("{stem}.failed")), "failed").unwrap();
        assert_eq!(
            load_host_completion_fact(tmp.path(), &key).unwrap(),
            CompletionFact::Failed("failed".into())
        );
    }
}
