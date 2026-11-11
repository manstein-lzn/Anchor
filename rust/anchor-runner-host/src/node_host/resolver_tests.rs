use super::*;
use anchor_runtime::graph::GraphSnapshot;
use anchor_sandbox_bwrap::BubblewrapPolicy;
use std::sync::atomic::AtomicBool;

/// A Session mount source a test can fix, and then change under a Turn.
struct FixedMounts(std::sync::Mutex<BTreeMap<String, bool>>);

impl FixedMounts {
    fn new(entries: &[(&str, bool)]) -> Self {
        Self(std::sync::Mutex::new(
            entries
                .iter()
                .map(|(id, on)| ((*id).to_owned(), *on))
                .collect(),
        ))
    }

    fn set(&self, id: &str, on: bool) {
        self.0.lock().unwrap().insert(id.to_owned(), on);
    }
}

impl PluginMountSource for FixedMounts {
    fn session_mounts(&self, _session: &str) -> Result<BTreeMap<String, bool>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
}

fn fixed(entries: &[(&str, bool)]) -> std::sync::Arc<dyn PluginMountSource> {
    std::sync::Arc::new(FixedMounts::new(entries))
}

fn sandbox(workspace: &Path) -> std::sync::Arc<BubblewrapSandbox> {
    std::sync::Arc::new(
        BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(workspace),
        )
        .unwrap(),
    )
}

fn library_with(root: &Path, ids: &[&str]) -> PathBuf {
    let library = root.join("library");
    std::fs::create_dir_all(library.join("plugins")).unwrap();
    for id in ids {
        if let Some(system) = anchor_graph_host::SystemPlugin::from_id(id) {
            crate::test_system_plugin(&library, system);
            continue;
        }
        let directory = library.join("plugins").join(id);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("plugin.json"),
            json!({"name": id}).to_string(),
        )
        .unwrap();
    }
    library
}

/// A Run in one pilot Session/Turn whose Graph declares `declared`, with
/// `installed` present in the Library and `switches` set on the Session.
/// `declared` is `(id, required, default)`: what the *Graph* says, which is
/// the default the Session's switches are applied to.
fn session_fixture(
    installed: &[&str],
    declared: &[(&str, bool, Option<bool>)],
    switches: &[(&str, bool)],
) -> (
    tempfile::TempDir,
    HostIoResolver,
    std::sync::Arc<FixedMounts>,
    NodeExecutionRequest,
) {
    session_fixture_with_snapshot(installed, declared, switches, None)
}

fn session_fixture_with_snapshot(
    installed: &[&str],
    declared: &[(&str, bool, Option<bool>)],
    switches: &[(&str, bool)],
    snapshot: Option<GraphSnapshot>,
) -> (
    tempfile::TempDir,
    HostIoResolver,
    std::sync::Arc<FixedMounts>,
    NodeExecutionRequest,
) {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let workspace = root.path().join("workspaces");
    let artifacts = root.path().join("artifacts");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&artifacts).unwrap();
    let library = library_with(root.path(), installed);
    let declared_json = declared
        .iter()
        .map(|(id, required, default)| {
            let mut item = json!({"id": id, "required": required});
            if let Some(default) = default {
                item["default"] = json!(default);
            }
            item
        })
        .collect::<Vec<_>>();
    let snapshot = snapshot.unwrap_or_else(|| {
        GraphSnapshot::admit(json!({
            "objective":"session mounts","entry":"agent",
            "agents":{"worker":{"model":"fixture"}},
            "nodes":[{"id":"agent","agent":"worker","plugins":declared_json}],"edges":[]
        }))
        .unwrap()
    });
    let mut record = GraphRunRecord::create_with_id(snapshot, json!({}), "session-mounts").unwrap();
    record.plugin_bindings = crate::plugin_library::Catalog::new(&library)
        .resolve(
            &declared
                .iter()
                .map(|(id, _, _)| (*id).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap()
        .into_iter()
        .map(|binding| (binding.id.clone(), binding))
        .collect();
    record.plugin_bindings_initialized = true;
    test_runs::seed(&state, &record);
    let mut metadata = crate::run_metadata::RunMetadata::new(
        record.run_id.clone(),
        "session-mounts".into(),
        record.graph_digest.clone(),
        &library,
    )
    .unwrap();
    metadata.pilot = Some(crate::run_metadata::PilotRunSource {
        owner: "alice".into(),
        session: "session-one".into(),
        turn: "turn-one".into(),
    });
    crate::run_metadata::save(&state, &metadata).unwrap();
    let mounts = std::sync::Arc::new(FixedMounts::new(switches));
    let resolver = HostIoResolver::new(
        HostArtifacts::new(artifacts, workspace.clone()),
        sandbox(&workspace),
        std::sync::Arc::new(tool_host::PluginToolHost::new(Vec::<String>::new())),
        tool_host::McpToolConfig::default(),
        LocalInputs::new(state.clone(), None).unwrap(),
        mounts.clone(),
        None,
        crate::execution::tool_assembly::new(crate::application::RunApplication::new(
            state,
            library.clone(),
        )),
    )
    .with_library_root(Some(library));
    let request = NodeExecutionRequest {
        key: InvocationKey {
            run_id: record.run_id,
            graph_digest: record.graph_digest,
            node_id: "agent".into(),
            invocation: 1,
        },
        model: Some("fixture".into()),
        task: String::new(),
        instructions: String::new(),
        routes: Vec::new(),
        input: json!({}),
        input_commits: Vec::new(),
        plugins: record.plugin_bindings.values().cloned().collect(),
        max_provider_requests: None,
        wall_time_limit_seconds: Some(30.0),
        network: false,
        kind: NodeKind::Agent,
        operation: None,
        writes: Vec::new(),
        cancellation: std::sync::Arc::new(AtomicBool::new(false)),
    };
    (root, resolver, mounts, request)
}

fn mounted_ids(mounted: &MountedPlugins) -> Vec<String> {
    mounted
        .bindings
        .iter()
        .map(|binding| binding.id.clone())
        .collect()
}

fn seed_admitted_binding(
    resolver: &HostIoResolver,
    request: &mut NodeExecutionRequest,
    binding: PluginBinding,
) {
    let record = resolver.load_run(&request.key.run_id).unwrap().unwrap();
    assert_eq!(record.plugin_bindings.get(&binding.id), Some(&binding));
    request.plugins = vec![binding];
}

mod mounts;
mod runtime;
mod tool_grants;
mod turn_plugins;
