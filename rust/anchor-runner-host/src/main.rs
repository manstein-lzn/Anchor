mod api;
mod application;
mod artifacts;
mod channel_inputs;
mod channel_tools;
mod execution;
mod goose_acp;
mod goose_tool_context;
mod local_inputs;
#[cfg(feature = "legacy-regression")]
mod model_registry;
mod pilot_host;
mod pilot_tools;
mod resource_read;
use artifacts::HostArtifacts;
mod node_host;
mod node_tools;
mod op;
mod run_data;
mod tool_environment;
mod tool_host;
use execution::PreparedExecution;
use node_host::HostNodes;
#[cfg(test)]
use node_host::load_host_completion_fact;

use anchor_runtime_rig::Cancellation;
#[cfg(test)]
use anchor_runtime_rig::graph::{CompletionFact, InvocationKey};
use anchor_runtime_rig::graph::{GraphRunRecord, GraphSnapshot, RunControl, RunStatus, RunStore};
use anchor_sandbox_bwrap::{BubblewrapPolicy, BubblewrapSandbox};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, env, io, path::PathBuf, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_FRAME: usize = 1024 * 1024;

fn create_durable_directory(path: &std::path::Path) -> io::Result<()> {
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

fn write_durable(path: &std::path::Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    std::fs::write(path, bytes)?;
    std::fs::File::open(path)?.sync_all()?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
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
        status: RunStatus,
        error: Option<String>,
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
    pause: Arc<std::sync::atomic::AtomicBool>,
}
impl RunControl for HostControl {
    fn pause_requested(&self) -> bool {
        self.pause.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn stop_requested(&self) -> bool {
        self.cancellation.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
}

fn reject_snapshot(snapshot: &GraphSnapshot) -> Result<(), String> {
    snapshot.validate().map_err(|error| error.to_string())?;
    // The parallel coordinator currently dispatches branch nodes through the
    // plain NodeExecutionPort. Graph calls require the host-owned GraphCallPort
    // and their parent/child admission facts, so allowing one inside a
    // fanout/join branch would turn a valid-looking graph into a runtime
    // failure (or, worse, a partially admitted child). Reject the combination
    // before a Run is created until the coordinator has an explicit call port.
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

fn env_path(name: &str) -> Result<PathBuf, String> {
    env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} is required"))
}

fn acquire_deployment_writer() -> Result<Box<dyn anchor_runtime_rig::graph::RunLease>, String> {
    let root = env_path("ANCHOR_RUNNER_STATE_ROOT")?;
    anchor_runtime_rig::graph::FileRunStore::new(root.join("deployment-locks"))
        .acquire_lease("deployment-writer")
        .map_err(|error| format!("state root already has a writing host: {error}"))
}
fn make_host_with_control(
    run_id: &str,
    plugin_bindings: BTreeMap<String, anchor_runtime_rig::graph::PluginBinding>,
    control: HostControl,
) -> Result<
    (
        anchor_runtime_rig::graph::FileRunStore,
        HostArtifacts,
        HostNodes,
        HostControl,
    ),
    String,
> {
    let state = env_path("ANCHOR_RUNNER_STATE_ROOT")?;
    let work_root = env_path("ANCHOR_RUNNER_WORKSPACE_ROOT")?;
    let _ = run_id;
    create_durable_directory(&state.join("artifacts")).map_err(|e| e.to_string())?;
    create_durable_directory(&work_root).map_err(|e| e.to_string())?;
    let commands = env::var("ANCHOR_RUNNER_ALLOWED_COMMANDS")
        .map_err(|_| "ANCHOR_RUNNER_ALLOWED_COMMANDS is required".to_owned())?;
    let policy = BubblewrapPolicy::new(
        env::var_os("ANCHOR_BWRAP")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("bwrap")),
        commands
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    )
    .authorize_workspace_root(&work_root)
    .authorize_readonly_input_root(state.join("artifacts"))
    .authorize_readonly_destination_root("/in")
    .authorize_readonly_destination_root("/plugins")
    .allow_network();
    // Plugin bundles are immutable host inputs.  The standalone runner and
    // the service API both expose their accepted bundle/catalog roots through
    // these existing deployment settings; no new Plugin protocol is added.
    let mut policy = policy;
    for root in [
        env::var_os("ANCHOR_RUNNER_BUNDLE_ROOT").map(PathBuf::from),
        env::var_os("ANCHOR_RUNNER_CATALOG_ROOT").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    .filter(|path| path.is_dir())
    {
        policy = policy.authorize_readonly_input_root(root);
    }
    // Library Plugins may be top-level symlinks to operator-managed source
    // directories. Authorize each resolved, admitted Plugin root so the
    // sandbox can mount the same canonical path the catalog resolved.
    for root in [
        env::var_os("ANCHOR_RUNNER_BUNDLE_ROOT").map(PathBuf::from),
        env::var_os("ANCHOR_RUNNER_CATALOG_ROOT").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    {
        let catalog = anchor_graph_host::FilePluginCatalog::new(&root);
        for binding in plugin_bindings.values() {
            if let Ok(directory) = catalog.plugin_directory(&binding.id) {
                policy = policy.authorize_readonly_input_root(directory);
            }
        }
    }
    let tool_environment = tool_environment::ToolEnvironment::from_env()?;
    let sandbox = Arc::new(
        BubblewrapSandbox::new(tool_environment.authorize(policy)).map_err(|e| e.to_string())?,
    );
    #[cfg(feature = "legacy-regression")]
    let models = model_registry::from_env()?;
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
    let io_resolver = Arc::new(node_host::HostIoResolver::new(
        artifacts.clone(),
        Arc::clone(&sandbox),
        Arc::new(tools.clone()),
        mcp.clone(),
        plugin_bindings.clone(),
        anchor_runtime_rig::graph::FileRunStore::new(state.join("runs")),
        local_inputs::LocalInputs::from_env(state.clone())?,
    ));
    let goose_nodes = goose_acp::GooseNodePort::from_env(
        &state,
        &work_root,
        Arc::clone(&io_resolver),
        Arc::clone(&sandbox),
    )?;
    #[cfg(feature = "legacy-regression")]
    let io_nodes = models.as_ref().filter(|_| goose_nodes.is_none()).map(|models| {
        anchor_io_harness_runtime::node_port::IoHarnessNodePort::new_with_default_policy_and_registry(
            state.join("io-harness/facts"),
            state.join("io-harness/store"),
            models.clone(),
            Arc::clone(&io_resolver),
        )
    });
    Ok((
        anchor_runtime_rig::graph::FileRunStore::new(state.join("runs")),
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
            mcp,
            #[cfg(feature = "legacy-regression")]
            io_nodes,
            goose_nodes,
        },
        control,
    ))
}

async fn handle(request: Request) -> Response {
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
            let bundle =
                match anchor_graph_host::FileGraphBundleLoader::new(bundle_root.clone()).load() {
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
                bundle.snapshot,
                input,
                run_id,
                request_id,
                bundle
                    .plugins
                    .into_iter()
                    .map(|binding| (binding.id.clone(), binding))
                    .collect(),
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
            match env_path("ANCHOR_RUNNER_STATE_ROOT") {
                Ok(root) => match anchor_runtime_rig::graph::FileRunStore::new(root.join("runs"))
                    .load(&run_id)
                {
                    Ok(Some(record)) => Response::Run {
                        version: 1,
                        request_id,
                        run_id,
                        status: record.status,
                        error: record.error,
                    },
                    Ok(None) => Response::Missing {
                        version: 1,
                        request_id,
                        run_id,
                    },
                    Err(e) => Response::Rejected {
                        version: 1,
                        request_id,
                        reason: e.to_string(),
                    },
                },
                Err(reason) => Response::Rejected {
                    version: 1,
                    request_id,
                    reason,
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
            execute_snapshot(*snapshot, input, run_id, request_id, BTreeMap::new(), None).await
        }
    }
}

async fn execute_snapshot(
    snapshot: GraphSnapshot,
    input: Value,
    run_id: String,
    request_id: String,
    plugin_bindings: BTreeMap<String, anchor_runtime_rig::graph::PluginBinding>,
    bundle_root: Option<PathBuf>,
) -> Response {
    execute_snapshot_with_control(
        snapshot,
        input,
        run_id,
        request_id,
        None,
        plugin_bindings,
        bundle_root,
    )
    .await
}

async fn execute_snapshot_with_control(
    snapshot: GraphSnapshot,
    input: Value,
    run_id: String,
    request_id: String,
    supplied_control: Option<HostControl>,
    plugin_bindings: BTreeMap<String, anchor_runtime_rig::graph::PluginBinding>,
    bundle_root: Option<PathBuf>,
) -> Response {
    let _writer = match acquire_deployment_writer() {
        Ok(lease) => lease,
        Err(reason) => {
            return Response::Rejected {
                version: 1,
                request_id,
                reason,
            };
        }
    };
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
        pause: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    let execution = match PreparedExecution::prepare_with_bindings(
        &expected,
        control,
        plugin_bindings,
        None,
        None,
    ) {
        Ok(execution) => execution,
        Err(reason) => {
            return Response::Rejected {
                version: 1,
                request_id,
                reason,
            };
        }
    };
    let record = match execution.store.load(&run_id) {
        Ok(Some(record))
            if record.snapshot == expected.snapshot && record.input == expected.input =>
        {
            record
        }
        Ok(Some(_)) => {
            return Response::Rejected {
                version: 1,
                request_id,
                reason: "run identity is already bound to a different snapshot or input".into(),
            };
        }
        Ok(None) => expected,
        Err(error) => {
            return Response::Rejected {
                version: 1,
                request_id,
                reason: error.to_string(),
            };
        }
    };
    if let Err(reason) = ensure_standalone_run_metadata(&record, bundle_root.as_deref()) {
        return Response::Rejected {
            version: 1,
            request_id,
            reason,
        };
    }
    match execution.run(record).await {
        Ok(record) => Response::Run {
            version: 1,
            request_id,
            run_id,
            status: record.status,
            error: record.error,
        },
        Err(error) => Response::Rejected {
            version: 1,
            request_id,
            reason: error.to_string(),
        },
    }
}

/// Persist the manual Run identity metadata a framed/standalone top-level Run
/// needs before GraphRunner can admit an `Op.call` child. `RunnerGraphCatalog`
/// fails closed when the parent Run has no durable source metadata, so a
/// `start_bundle`/`start` Run records the same manual identity the HTTP
/// RunApplication writes. This is idempotent, verifies graph/source/digest on
/// replay, and never fabricates metadata for a Graph-call child.
fn ensure_standalone_run_metadata(
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
    let state = env_path("ANCHOR_RUNNER_STATE_ROOT")?;
    let expected = crate::application::RunMetadata::new(
        record.run_id.clone(),
        graph,
        record.graph_digest.clone(),
        &source,
    )
    .map_err(|error| format!("{error:?}"))?;
    if let Some(existing) =
        crate::application::metadata::load(&state, &record.run_id).map_err(|e| format!("{e:?}"))?
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
    crate::application::metadata::save(&state, &expected).map_err(|error| format!("{error:?}"))
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
    if env::args().nth(1).as_deref() == Some("serve") {
        return api::serve().await;
    }
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = stdin;
    let mut writer = stdout;
    while let Some(frame) = read_frame(&mut reader).await? {
        let response = match serde_json::from_slice::<Request>(&frame) {
            Ok(request) => handle(request).await,
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
        graph.edges.push(anchor_runtime_rig::graph::GraphEdge {
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
        graph.nodes.push(anchor_runtime_rig::graph::GraphNode {
            id: "good".into(),
            agent: None,
            op: Some("run".into()),
            input: None,
            plugins: Vec::new(),
            max_rounds: None,
        });
        graph.nodes.push(anchor_runtime_rig::graph::GraphNode {
            id: "bad".into(),
            agent: None,
            op: Some("run".into()),
            input: None,
            plugins: Vec::new(),
            max_rounds: None,
        });
        graph.edges.extend([
            anchor_runtime_rig::graph::GraphEdge {
                from_node: "command".into(),
                to_node: "good".into(),
            },
            anchor_runtime_rig::graph::GraphEdge {
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
        let response = handle(Request::Status {
            version: 2,
            request_id: "req".into(),
            run_id: "run".into(),
        })
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
