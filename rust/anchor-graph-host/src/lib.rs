//! Standalone Graph bundle loading and minimal in-process host adapter for
//! recursive Graph calls.
//!
//! The host supplies a graph catalog, the shared FileRunStore, artifact port,
//! node executor, and run control. Child Runs are always executed by the same
//! `GraphRunner` implementation. Bundle loading is read-only; result
//! materialization, session handoff, and platform APIs are outside this adapter.

use anchor_runtime_rig::Cancellation;
use anchor_runtime_rig::graph::{
    ArtifactPort, CallIdentity, GraphCallOutcome, GraphCallPort, GraphError, GraphRunRecord,
    GraphRunner, GraphSnapshot, NodeExecutionCapabilities, NodeExecutionOutcome, NodeExecutionPort,
    NodeExecutionRequest, RunControl, RunLease, RunStatus, RunStore,
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    path::{Component, Path, PathBuf},
    pin::Pin,
};

/// Strict metadata for a distributable, secret-free Graph directory.
///
/// Format 1 contains one `graph.json`, this `manifest.json`, and explicitly
/// declared Plugin resource directories at `plugins/<id>/`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphBundleManifest {
    pub format: u32,
    pub graph: String,
    pub plugins: Vec<BundlePluginSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePluginSummary {
    pub id: String,
    pub digest: String,
    pub resources: Vec<String>,
    pub mcp_servers: Vec<String>,
}

/// Admitted expanded Graph snapshot and secret-free Plugin identities.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedGraphBundle {
    pub snapshot: GraphSnapshot,
    pub plugins: Vec<anchor_runtime_rig::graph::PluginBinding>,
}

/// Read-only loader for a single standalone Graph bundle directory.
pub struct FileGraphBundleLoader {
    root: PathBuf,
}

impl FileGraphBundleLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Load and admit `manifest.json`, `graph.json`, and explicitly declared
    /// `plugins/<id>/` assets. This function never dispatches Graph work.
    pub fn load(&self) -> Result<LoadedGraphBundle, GraphError> {
        let root = self.root.canonicalize()?;
        if !root.is_dir() {
            return Err(invalid("Graph bundle root must be a directory".into()));
        }
        let manifest_path = root.join("manifest.json");
        reject_symlink_components(&root, &manifest_path)?;
        let manifest: GraphBundleManifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)
            .map_err(GraphError::SnapshotDecode)?;
        if manifest.format != 1 || manifest.graph != "graph.json" {
            return Err(invalid(
                "unsupported Graph bundle format or graph path".into(),
            ));
        }
        let graph_path = root.join("graph.json");
        reject_symlink_components(&root, &graph_path)?;
        let value: Value = serde_json::from_slice(&std::fs::read(&graph_path)?)
            .map_err(GraphError::SnapshotDecode)?;
        let snapshot = GraphSnapshot::admit(value)?;

        let expected = snapshot
            .nodes
            .iter()
            .flat_map(|node| node.plugins.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        let ids = manifest
            .plugins
            .iter()
            .map(|plugin| plugin.id.clone())
            .collect::<Vec<_>>();
        let declared = ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if expected != declared || declared.len() != ids.len() {
            return Err(invalid(
                "manifest Plugin set must exactly match Graph references".into(),
            ));
        }

        let catalog = FilePluginCatalog::new(&root);
        let bindings = catalog.resolve(&ids)?;
        for (summary, binding) in manifest.plugins.iter().zip(&bindings) {
            if summary.digest != binding.digest
                || summary.resources != binding.resources
                || summary.mcp_servers != binding.mcp_servers
            {
                return Err(invalid(format!(
                    "Plugin `{}` summary does not match bundled resources",
                    summary.id
                )));
            }
        }

        // A bundle is deliberately closed: no undeclared payload, plugin, or
        // extra top-level config can be silently carried into deployment.
        let mut allowed = vec!["graph.json", "manifest.json"];
        if !ids.is_empty() {
            allowed.push("plugins");
        }
        allowed.sort_unstable();
        let mut actual = std::fs::read_dir(&root)?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        actual.sort_unstable();
        if actual != allowed {
            return Err(invalid(
                "Graph bundle contains undeclared top-level resources".into(),
            ));
        }
        if !ids.is_empty() {
            let plugin_root = root.join("plugins");
            reject_symlink_components(&root, &plugin_root)?;
            let mut actual = std::fs::read_dir(plugin_root)?
                .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
                .collect::<Result<Vec<_>, _>>()?;
            actual.sort_unstable();
            let mut declared = ids;
            declared.sort_unstable();
            if actual != declared {
                return Err(invalid(
                    "bundle contains undeclared Plugin directories".into(),
                ));
            }
        }
        Ok(LoadedGraphBundle {
            snapshot,
            plugins: bindings,
        })
    }
}

/// Resolves installed filesystem Plugins to secret-free immutable identities.
pub trait PluginCatalog: Send + Sync {
    fn resolve(
        &self,
        ids: &[String],
    ) -> Result<Vec<anchor_runtime_rig::graph::PluginBinding>, GraphError>;
}

/// Filesystem catalog rooted at the library directory containing `plugins/`.
pub struct FilePluginCatalog {
    root: PathBuf,
}

impl FilePluginCatalog {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn plugin(&self, id: &str) -> Result<anchor_runtime_rig::graph::PluginBinding, GraphError> {
        if !valid_reference(id) {
            return Err(invalid(format!("invalid Plugin reference `{id}`")));
        }
        let root = self.root.canonicalize()?;
        let plugins = root.join("plugins");
        let dir = plugins.join(id);
        reject_symlink_components(&root, &dir)?;
        if !dir.is_dir() {
            return Err(invalid(format!("unknown Plugin `{id}`")));
        }
        let manifest_path = dir.join("plugin.json");
        reject_symlink_components(&root, &manifest_path)?;
        let manifest = read_object(&manifest_path)?;
        let mut resources = Vec::new();
        collect_files(&dir, &dir, &mut resources)?;
        resources.sort();
        let mut hash = Sha256::new();
        for rel in &resources {
            let content = std::fs::read(dir.join(rel))?;
            hash.update(rel.as_bytes());
            hash.update(Sha256::digest(content));
        }
        let digest = format!("{:x}", hash.finalize());

        let mut servers = std::collections::BTreeSet::new();
        let dot_mcp = dir.join(".mcp.json");
        if dot_mcp.exists() {
            reject_symlink_components(&root, &dot_mcp)?;
            collect_server_names(read_object(&dot_mcp)?.get("mcpServers"), &mut servers, id)?;
        }
        match manifest.get("mcpServers") {
            None | Some(Value::Null) => {}
            Some(Value::String(relative)) => {
                let rel = Path::new(relative);
                if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
                    return Err(invalid(format!("Plugin {id}: unsafe MCP config path")));
                }
                let config_path = dir.join(rel);
                reject_symlink_components(&root, &config_path)?;
                collect_server_names(
                    read_object(&config_path)?.get("mcpServers"),
                    &mut servers,
                    id,
                )?;
            }
            Some(value) => collect_server_names(Some(value), &mut servers, id)?,
        }
        Ok(anchor_runtime_rig::graph::PluginBinding {
            id: id.into(),
            digest,
            resources,
            mcp_servers: servers.into_iter().collect(),
        })
    }
}

impl PluginCatalog for FilePluginCatalog {
    fn resolve(
        &self,
        ids: &[String],
    ) -> Result<Vec<anchor_runtime_rig::graph::PluginBinding>, GraphError> {
        ids.iter().map(|id| self.plugin(id)).collect()
    }
}

fn invalid(message: String) -> GraphError {
    GraphError::InvalidSnapshot(message)
}
fn valid_reference(value: &str) -> bool {
    !value.is_empty()
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
fn read_object(path: &Path) -> Result<serde_json::Map<String, Value>, GraphError> {
    let value: Value =
        serde_json::from_slice(&std::fs::read(path)?).map_err(GraphError::SnapshotDecode)?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid(format!("{} must contain a JSON object", path.display())))
}
fn collect_server_names(
    value: Option<&Value>,
    names: &mut std::collections::BTreeSet<String>,
    id: &str,
) -> Result<(), GraphError> {
    let Some(value) = value else { return Ok(()) };
    let servers = value
        .as_object()
        .ok_or_else(|| invalid(format!("Plugin {id}: mcpServers must be an object")))?;
    for (name, server) in servers {
        if !valid_reference(name) || !server.is_object() {
            return Err(invalid(format!(
                "Plugin {id}: invalid MCP server declaration"
            )));
        }
        if server.get("enabled") != Some(&Value::Bool(false)) {
            names.insert(name.clone());
        }
    }
    Ok(())
}
fn reject_symlink_components(root: &Path, path: &Path) -> Result<(), GraphError> {
    let root = root.canonicalize()?;
    if !path.starts_with(&root) {
        return Err(invalid("Plugin path escapes catalog root".into()));
    }
    let mut current = root.clone();
    for component in path.strip_prefix(&root).unwrap().components() {
        if let Component::Normal(part) = component {
            current.push(part);
            if std::fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(invalid(format!(
                    "Plugin symlinks are not supported: {}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}
fn collect_files(root: &Path, current: &Path, out: &mut Vec<String>) -> Result<(), GraphError> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_symlink() {
            return Err(invalid(format!(
                "Plugin symlinks are not supported: {}",
                path.display()
            )));
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".env" || name.starts_with(".env.") || name == ".mcp-auth" {
            return Err(invalid(format!(
                "Plugin-local credentials are not supported: {}",
                path.display()
            )));
        }
        if ty.is_dir() {
            collect_files(root, &path, out)?;
        } else if ty.is_file() {
            out.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

pub trait GraphCatalog: Send + Sync {
    fn snapshot(&self, name: &str) -> Result<Option<GraphSnapshot>, GraphError>;

    /// Short admission lock shared with target-Graph mutation. This lock is
    /// released before child execution; child execution uses only its Run lease.
    fn lock_admission(&self, _name: &str) -> Result<Box<dyn RunLease>, GraphError> {
        Ok(Box::new(NoopLease))
    }

    /// Resolve one immutable child bundle. Catalogs that only provide snapshots
    /// keep the provider-free adapter behavior; production catalogs should
    /// return the bundle's Plugin pins as well.
    fn bundle(&self, name: &str) -> Result<Option<LoadedGraphBundle>, GraphError> {
        Ok(self.snapshot(name)?.map(|snapshot| LoadedGraphBundle {
            snapshot,
            plugins: Vec::new(),
        }))
    }

    /// Persist API-visible identity metadata alongside a durable child Run.
    fn record_child_admission(
        &self,
        _run_id: &str,
        _graph: &str,
        _snapshot: &GraphSnapshot,
        _plugins: &[anchor_runtime_rig::graph::PluginBinding],
        _identity: &CallIdentity,
        _mode: &str,
    ) -> Result<(), GraphError> {
        Ok(())
    }

    /// Register a wait child with the host's per-Run control registry. The
    /// returned token belongs to the child; the host may additionally link
    /// parent cancellation for wait-mode calls.
    fn begin_child_execution<'a>(
        &'a self,
        _run_id: &'a str,
        _graph: &'a str,
        parent_cancellation: Cancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ChildRunControl, GraphError>> + Send + 'a>> {
        Box::pin(async move { Ok(ChildRunControl::linked_to_parent(parent_cancellation)) })
    }

    fn end_child_execution<'a>(
        &'a self,
        _run_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn child_execution_finished<'a>(
        &'a self,
        _run_id: &'a str,
        _graph: &'a str,
        _status: RunStatus,
    ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    /// Admit a detached child to host-owned background execution. The default
    /// standalone adapter keeps admission-only behavior.
    fn dispatch_detached<'a>(
        &'a self,
        _run_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    /// Verify that an existing durable child still has its API-visible source
    /// identity. This must not require the mutable Graph definition to exist.
    fn verify_child_identity(
        &self,
        _run_id: &str,
        _graph: &str,
        _snapshot: &GraphSnapshot,
        _identity: &CallIdentity,
        _mode: &str,
    ) -> Result<(), GraphError> {
        Ok(())
    }

    fn has_child_admission(&self, _run_id: &str) -> Result<bool, GraphError> {
        Ok(false)
    }

    /// Plugin-bearing children need their pinned resources rechecked before
    /// resuming. Implementations should inspect Plugin resources directly and
    /// must not use the current Graph definition as execution truth.
    fn verify_child_plugins(
        &self,
        graph: &str,
        bindings: &[anchor_runtime_rig::graph::PluginBinding],
    ) -> Result<(), GraphError> {
        let current = self
            .bundle(graph)?
            .ok_or_else(|| GraphError::InvalidSnapshot(format!("unknown child graph `{graph}`")))?;
        if current.plugins != bindings {
            return Err(GraphError::RunConflict);
        }
        Ok(())
    }
}

struct NoopLease;
impl RunLease for NoopLease {}

#[derive(Clone)]
pub struct ChildRunControl {
    pub cancellation: Cancellation,
    pub pause: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ChildRunControl {
    pub fn linked_to_parent(cancellation: Cancellation) -> Self {
        Self {
            cancellation,
            pause: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

/// Adapts GraphCallPort on top of shared kernel ports. Instantiate one host
/// around the same store/artifact/node/control ports used by the outer Runner.
pub struct InProcessGraphHost<'a, S, A, N, C> {
    catalog: &'a dyn GraphCatalog,
    store: &'a S,
    artifacts: &'a A,
    nodes: &'a N,
    plugin_catalog: Option<&'a dyn PluginCatalog>,
    _control: std::marker::PhantomData<fn() -> C>,
}

struct BoundNodes<'a, N> {
    nodes: &'a N,
    plugins: std::collections::BTreeMap<String, anchor_runtime_rig::graph::PluginBinding>,
}

impl<N: NodeExecutionPort> NodeExecutionPort for BoundNodes<'_, N> {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        self.nodes.capabilities()
    }
    fn resolve_plugins(
        &self,
        ids: &[String],
    ) -> Result<Vec<anchor_runtime_rig::graph::PluginBinding>, GraphError> {
        ids.iter()
            .map(|id| {
                self.plugins.get(id).cloned().ok_or_else(|| {
                    GraphError::InvalidSnapshot(format!("child Plugin `{id}` is not bundle-bound"))
                })
            })
            .collect()
    }
    fn completion_fact<'b>(
        &'b self,
        key: &'b anchor_runtime_rig::graph::InvocationKey,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<anchor_runtime_rig::graph::CompletionFact, GraphError>>
                + Send
                + 'b,
        >,
    > {
        self.nodes.completion_fact(key)
    }
    fn execute<'b>(
        &'b self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'b>> {
        self.nodes.execute(request)
    }
}

struct ChildControl {
    control: ChildRunControl,
}

impl ChildControl {
    fn new(control: ChildRunControl) -> Self {
        Self { control }
    }
}

impl RunControl for ChildControl {
    fn pause_requested(&self) -> bool {
        self.control
            .pause
            .load(std::sync::atomic::Ordering::Acquire)
    }
    fn stop_requested(&self) -> bool {
        self.control
            .cancellation
            .load(std::sync::atomic::Ordering::Acquire)
    }
    fn cancellation(&self) -> Cancellation {
        self.control.cancellation.clone()
    }
}

impl<'a, S, A, N, C> InProcessGraphHost<'a, S, A, N, C> {
    pub fn new(
        catalog: &'a dyn GraphCatalog,
        store: &'a S,
        artifacts: &'a A,
        nodes: &'a N,
        _control: &'a C,
    ) -> Self {
        Self {
            catalog,
            store,
            artifacts,
            nodes,
            plugin_catalog: None,
            _control: std::marker::PhantomData,
        }
    }

    /// Configure the filesystem (or other) catalog used to pin Plugin identities.
    pub fn with_plugin_catalog(mut self, catalog: &'a dyn PluginCatalog) -> Self {
        self.plugin_catalog = Some(catalog);
        self
    }
}

impl<S: RunStore, A: ArtifactPort, N: NodeExecutionPort, C: RunControl> NodeExecutionPort
    for InProcessGraphHost<'_, S, A, N, C>
{
    fn capabilities(&self) -> NodeExecutionCapabilities {
        self.nodes.capabilities()
    }
    fn resolve_plugins(
        &self,
        ids: &[String],
    ) -> Result<Vec<anchor_runtime_rig::graph::PluginBinding>, GraphError> {
        match self.plugin_catalog {
            Some(catalog) => catalog.resolve(ids),
            None => self.nodes.resolve_plugins(ids),
        }
    }
    fn completion_fact<'b>(
        &'b self,
        key: &'b anchor_runtime_rig::graph::InvocationKey,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<anchor_runtime_rig::graph::CompletionFact, GraphError>>
                + Send
                + 'b,
        >,
    > {
        self.nodes.completion_fact(key)
    }
    fn execute<'b>(
        &'b self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'b>> {
        self.nodes.execute(request)
    }
    fn graph_call_port(&self) -> Option<&dyn GraphCallPort> {
        Some(self)
    }
}

impl<S: RunStore, A: ArtifactPort, N: NodeExecutionPort, C: RunControl> GraphCallPort
    for InProcessGraphHost<'_, S, A, N, C>
{
    fn call<'b>(
        &'b self,
        identity: &'b CallIdentity,
        spec: &'b Value,
        input: &'b Value,
        cancellation: Cancellation,
    ) -> Pin<Box<dyn Future<Output = Result<GraphCallOutcome, GraphError>> + Send + 'b>> {
        Box::pin(async move {
            let parent = self
                .store
                .load(&identity.parent_run_id)
                .map_err(|e| GraphError::Unsupported(format!("load parent Run for call: {e}")))?
                .ok_or_else(|| GraphError::CorruptRun("Graph call parent Run is missing".into()))?;
            let cursor = parent.cursor.as_ref().ok_or_else(|| {
                GraphError::CorruptRun("Graph call parent has no active cursor".into())
            })?;
            if parent.graph_digest != identity.parent_graph_digest
                || cursor.node_id != identity.node_id
                || cursor.key.invocation != identity.invocation
                || cursor.prepared_input != *input
            {
                return Err(GraphError::CorruptRun(
                    "Graph call identity does not match durable parent cursor".into(),
                ));
            }
            let parent_node = parent
                .snapshot
                .nodes
                .iter()
                .find(|node| node.id == identity.node_id)
                .ok_or_else(|| {
                    GraphError::CorruptRun("Graph call node is missing from parent snapshot".into())
                })?;
            let frozen_spec = parent_node
                .op
                .as_ref()
                .and_then(|op_name| parent.snapshot.ops.get(op_name))
                .and_then(|op| op.get("call"))
                .ok_or_else(|| {
                    GraphError::CorruptRun("parent cursor is not an Op.call node".into())
                })?;
            let spec_digest = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(frozen_spec).map_err(GraphError::SnapshotDecode)?
                )
            );
            if frozen_spec != spec || identity.call_spec_digest != spec_digest {
                return Err(GraphError::CorruptRun(
                    "Graph call spec does not match frozen parent snapshot".into(),
                ));
            }
            let fields = spec
                .as_object()
                .ok_or_else(|| GraphError::InvalidSnapshot("op.call must be an object".into()))?;
            for unsupported in ["input_map", "files", "result", "session"] {
                if fields.contains_key(unsupported) {
                    return Err(GraphError::Unsupported(format!(
                        "standalone GraphCallPort does not support `{unsupported}` yet"
                    )));
                }
            }
            let graph_name = fields.get("graph").and_then(Value::as_str).ok_or_else(|| {
                GraphError::InvalidSnapshot("op.call.graph must be a string".into())
            })?;
            let mode = fields.get("mode").and_then(Value::as_str).ok_or_else(|| {
                GraphError::InvalidSnapshot("op.call.mode must be a string".into())
            })?;
            if !matches!(mode, "wait" | "detach") {
                return Err(GraphError::InvalidSnapshot(
                    "op.call.mode must be wait or detach".into(),
                ));
            }
            let child_input = fields
                .get("input")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            if !child_input.is_object() {
                return Err(GraphError::InvalidSnapshot(
                    "op.call input must be an object".into(),
                ));
            }
            let run_id = child_run_id(identity);
            let child_lease = self
                .store
                .acquire_lease(&run_id)
                .map_err(|e| GraphError::Unsupported(format!("acquire child lease: {e}")))?;
            let existing = self
                .store
                .load(&run_id)
                .map_err(|e| GraphError::Unsupported(format!("load child Run: {e}")))?;
            let mut child = if let Some(existing) = existing {
                if existing.input != expected_input(&existing.snapshot, &child_input) {
                    return Err(GraphError::RunConflict);
                }
                self.catalog.verify_child_identity(
                    &run_id,
                    graph_name,
                    &existing.snapshot,
                    identity,
                    mode,
                )?;
                if !existing.plugin_bindings.is_empty() {
                    let pins = existing
                        .plugin_bindings
                        .values()
                        .cloned()
                        .collect::<Vec<_>>();
                    self.catalog.verify_child_plugins(graph_name, &pins)?;
                }
                existing
            } else {
                // Serialize only the short child admission against mutations to
                // the target Graph. Existing children never consult this catalog
                // definition unless their pinned Plugin resources need checking.
                let _admission_lease = self.catalog.lock_admission(graph_name)?;
                // Recheck after taking the target lock in case another call won
                // the same identity between the first read and admission.
                if let Some(existing) = self.store.load(&run_id)? {
                    if existing.input != expected_input(&existing.snapshot, &child_input) {
                        return Err(GraphError::RunConflict);
                    }
                    self.catalog.verify_child_identity(
                        &run_id,
                        graph_name,
                        &existing.snapshot,
                        identity,
                        mode,
                    )?;
                    if !existing.plugin_bindings.is_empty() {
                        let pins = existing
                            .plugin_bindings
                            .values()
                            .cloned()
                            .collect::<Vec<_>>();
                        self.catalog.verify_child_plugins(graph_name, &pins)?;
                    }
                    existing
                } else {
                    if self.catalog.has_child_admission(&run_id)? {
                        return Err(GraphError::CorruptRun(
                            "child admission metadata exists but its durable Run record is missing"
                                .into(),
                        ));
                    }
                    let bundle = self
                        .catalog
                        .bundle(graph_name)
                        .map_err(|e| GraphError::Unsupported(format!("load child bundle: {e}")))?
                        .ok_or_else(|| {
                            GraphError::InvalidSnapshot(format!(
                                "unknown child graph `{graph_name}`"
                            ))
                        })?;
                    let snapshot = bundle.snapshot;
                    if snapshot.ops.values().any(|op| op.get("call").is_some()) {
                        return Err(GraphError::Unsupported(
                            "nested Graph calls are not supported by this standalone host yet"
                                .into(),
                        ));
                    }
                    let mut record = GraphRunRecord::create(snapshot.clone(), child_input)?;
                    record.run_id = run_id.clone();
                    record.plugin_bindings = bundle
                        .plugins
                        .iter()
                        .cloned()
                        .map(|plugin| (plugin.id.clone(), plugin))
                        .collect();
                    record.plugin_bindings_initialized = true;
                    self.catalog
                        .record_child_admission(
                            &run_id,
                            graph_name,
                            &record.snapshot,
                            &record.plugin_bindings.values().cloned().collect::<Vec<_>>(),
                            identity,
                            mode,
                        )
                        .map_err(|e| {
                            GraphError::Unsupported(format!("persist child identity metadata: {e}"))
                        })?;
                    self.store
                        .save(&record)
                        .map_err(|e| GraphError::Unsupported(format!("persist child Run: {e}")))?;
                    record
                }
            };
            drop(child_lease);
            // FileRunStore's record is the durable identity → child admission:
            // deterministic ID plus frozen snapshot/input are checked on retry.
            if mode == "detach" {
                self.catalog.dispatch_detached(&run_id).await?;
                return Ok(GraphCallOutcome::Detached {
                    child_run_id: run_id,
                });
            }
            if cancellation.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(GraphCallOutcome::Waiting {
                    child_run_id: run_id,
                });
            }
            let child_control = ChildControl::new(
                self.catalog
                    .begin_child_execution(&run_id, graph_name, cancellation.clone())
                    .await?,
            );
            let child_nodes = BoundNodes {
                nodes: self.nodes,
                plugins: child.plugin_bindings.clone(),
            };
            child = GraphRunner::new(self.store, self.artifacts, &child_nodes, &child_control)
                .run(child)
                .await
                .map_err(|e| GraphError::Unsupported(format!("execute child Run: {e}")))?;
            self.catalog.end_child_execution(&run_id).await;
            self.catalog
                .child_execution_finished(&run_id, graph_name, child.status)
                .await?;
            match child.status {
                RunStatus::Completed => {
                    let output = child
                        .results
                        .values()
                        .flatten()
                        .max_by_key(|result| result.sequence)
                        .map(|result| result.completion.output.clone())
                        .ok_or_else(|| {
                            GraphError::CorruptRun("completed child has no result".into())
                        })?;
                    Ok(GraphCallOutcome::Completed {
                        child_run_id: run_id,
                        output,
                    })
                }
                RunStatus::WaitingCall
                | RunStatus::WaitingRecovery
                | RunStatus::BudgetStopped
                | RunStatus::Paused
                | RunStatus::Stopped => Ok(GraphCallOutcome::Waiting {
                    child_run_id: run_id,
                }),
                RunStatus::Failed | RunStatus::Aborted => Ok(GraphCallOutcome::Failed {
                    child_run_id: Some(run_id),
                    reason: child.error.unwrap_or_else(|| "child Graph failed".into()),
                }),
                RunStatus::Ready | RunStatus::Running => Ok(GraphCallOutcome::Uncertain {
                    child_run_id: Some(run_id),
                    reason: format!("child Run returned nonterminal status {:?}", child.status),
                }),
            }
        })
    }
}

fn child_run_id(identity: &CallIdentity) -> String {
    format!("call-{}", identity.durable_key())
}

fn expected_input(snapshot: &GraphSnapshot, provided: &Value) -> Value {
    // GraphRunRecord::create applies the kernel's input merge rules. Keep this
    // helper aligned with the currently supported object merge behavior.
    fn merge(base: &Value, over: &Value) -> Value {
        match (base, over) {
            (Value::Object(a), Value::Object(b)) => {
                let mut result = a.clone();
                for (key, value) in b {
                    let merged = result
                        .get(key)
                        .filter(|old| old.is_object() && value.is_object())
                        .map(|old| merge(old, value))
                        .unwrap_or_else(|| value.clone());
                    result.insert(key.clone(), merged);
                }
                Value::Object(result)
            }
            (_, Value::Null) => base.clone(),
            (_, value) => value.clone(),
        }
    }
    merge(&snapshot.input, provided)
}

#[cfg(test)]
mod plugin_catalog_tests {
    use super::*;
    use std::fs;

    fn catalog_with_plugin() -> (tempfile::TempDir, FilePluginCatalog) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plugins/demo");
        fs::create_dir_all(dir.join("skills/example")).unwrap();
        fs::write(dir.join("plugin.json"), r#"{"name":"Demo","mcpServers":{"inline":{"command":"run","env":{"TOKEN":"top-secret"}},"disabled":{"enabled":false}}}"#).unwrap();
        fs::write(
            dir.join(".mcp.json"),
            r#"{"mcpServers":{"config":{"headers":{"Authorization":"secret-value"}}}}"#,
        )
        .unwrap();
        fs::write(dir.join("skills/example/SKILL.md"), "hello").unwrap();
        let catalog = FilePluginCatalog::new(tmp.path());
        (tmp, catalog)
    }

    #[test]
    fn resolves_sorted_bundle_digest_resources_and_secret_free_mcp_names() {
        let (_tmp, catalog) = catalog_with_plugin();
        let binding = catalog.resolve(&["demo".into()]).unwrap().remove(0);
        assert_eq!(binding.id, "demo");
        assert_eq!(
            binding.resources,
            vec![".mcp.json", "plugin.json", "skills/example/SKILL.md"]
        );
        assert_eq!(binding.mcp_servers, vec!["config", "inline"]);
        let serialized = serde_json::to_string(&binding).unwrap();
        assert!(!serialized.contains("top-secret"));
        assert!(!serialized.contains("secret-value"));
        assert!(!serialized.contains("TOKEN"));
    }

    #[test]
    fn bundle_mutation_changes_digest() {
        let (tmp, catalog) = catalog_with_plugin();
        let before = catalog.resolve(&["demo".into()]).unwrap().remove(0).digest;
        fs::write(
            tmp.path().join("plugins/demo/skills/example/SKILL.md"),
            "changed",
        )
        .unwrap();
        let after = catalog.resolve(&["demo".into()]).unwrap().remove(0).digest;
        assert_ne!(before, after);
    }

    #[test]
    fn unknown_plugin_is_rejected() {
        let (_tmp, catalog) = catalog_with_plugin();
        assert!(catalog.resolve(&["missing".into()]).is_err());
        assert!(catalog.resolve(&["../escape".into()]).is_err());
    }

    #[test]
    fn package_local_credentials_are_rejected() {
        let (tmp, catalog) = catalog_with_plugin();
        fs::write(tmp.path().join("plugins/demo/.env"), "TOKEN=secret").unwrap();
        assert!(catalog.resolve(&["demo".into()]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;
        let (tmp, catalog) = catalog_with_plugin();
        let outside = tmp.path().join("outside.json");
        fs::write(&outside, r#"{"mcpServers":{"escape":{}}}"#).unwrap();
        symlink(&outside, tmp.path().join("plugins/demo/escape.json")).unwrap();
        assert!(catalog.resolve(&["demo".into()]).is_err());
    }
}

#[cfg(test)]
mod bundle_loader_tests {
    use super::*;
    use std::fs;

    fn graph_json(plugins: &[&str]) -> Value {
        serde_json::json!({
            "objective":"demo",
            "entry":"work",
            "agents":{},
            "ops":{"work":{"run":"true"}},
            "nodes":[{"id":"work","op":"work","plugins":plugins}],
            "edges":[]
        })
    }

    fn bundle() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("plugins/demo/skills")).unwrap();
        fs::write(
            tmp.path().join("plugins/demo/plugin.json"),
            r#"{"name":"Demo"}"#,
        )
        .unwrap();
        fs::write(
            tmp.path().join("plugins/demo/skills/SKILL.md"),
            "safe resource",
        )
        .unwrap();
        fs::write(
            tmp.path().join("graph.json"),
            graph_json(&["demo"]).to_string(),
        )
        .unwrap();
        let binding = FilePluginCatalog::new(tmp.path())
            .resolve(&["demo".into()])
            .unwrap()
            .remove(0);
        let manifest = serde_json::json!({
            "format":1,
            "graph":"graph.json",
            "plugins":[{
                "id":binding.id,
                "digest":binding.digest,
                "resources":binding.resources,
                "mcp_servers":binding.mcp_servers
            }]
        });
        fs::write(tmp.path().join("manifest.json"), manifest.to_string()).unwrap();
        tmp
    }

    #[test]
    fn loads_graph_through_shared_admission_and_returns_only_plugin_summary() {
        let tmp = bundle();
        let loaded = FileGraphBundleLoader::new(tmp.path()).load().unwrap();
        assert_eq!(loaded.snapshot.objective, "demo");
        assert_eq!(loaded.plugins.len(), 1);
        assert_eq!(loaded.plugins[0].id, "demo");
        assert!(loaded.plugins[0].resources.contains(&"plugin.json".into()));
    }

    #[test]
    fn rejects_manifest_unknown_fields_graph_paths_and_plugin_set_drift() {
        let tmp = bundle();
        let path = tmp.path().join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["extra"] = Value::Bool(true);
        fs::write(&path, manifest.to_string()).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        let path = tmp.path().join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["graph"] = Value::String("../outside.json".into());
        fs::write(&path, manifest.to_string()).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        fs::write(tmp.path().join("graph.json"), graph_json(&[]).to_string()).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());
    }

    #[test]
    fn rejects_resource_digest_drift_extra_files_and_credentials() {
        let tmp = bundle();
        fs::write(tmp.path().join("plugins/demo/skills/SKILL.md"), "tampered").unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        fs::write(tmp.path().join("unexpected.txt"), "payload").unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        fs::write(tmp.path().join("plugins/demo/.env"), "TOKEN=secret").unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_graph_and_plugin_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = bundle();
        let outside = tmp.path().join("outside.json");
        fs::write(&outside, graph_json(&["demo"]).to_string()).unwrap();
        fs::remove_file(tmp.path().join("graph.json")).unwrap();
        symlink(&outside, tmp.path().join("graph.json")).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::remove_dir_all(tmp.path().join("plugins/demo/skills")).unwrap();
        symlink(&outside, tmp.path().join("plugins/demo/skills")).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());
    }
}
