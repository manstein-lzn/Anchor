use super::*;

#[tokio::test]
async fn resolver_returns_owned_anchor_tools_under_admitted_plugin_authority() {
    let root = tempfile::tempdir().unwrap();
    let workspace_root = root.path().join("workspaces");
    let artifacts_root = root.path().join("artifacts");
    std::fs::create_dir_all(&workspace_root).unwrap();
    std::fs::create_dir_all(&artifacts_root).unwrap();
    let sandbox = std::sync::Arc::new(
        BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["sh"])
                .authorize_workspace_root(&workspace_root)
                .authorize_readonly_input_root(&artifacts_root)
                .authorize_readonly_destination_root("/in"),
        )
        .unwrap(),
    );
    let tools = tool_host::PluginToolHost::new(["fake-tools".to_owned()]);
    let bindings = tools
        .resolve_fixture_plugins(&["fake-tools".into()])
        .unwrap();
    let snapshot = anchor_runtime::graph::GraphSnapshot::admit(json!({
        "objective":"resolver test","entry":"agent","agents":{"worker":{"model":"fixture"}},
        "ops":{},"nodes":[{"id":"agent","agent":"worker","plugins":["fake-tools"]}],"edges":[]
    }))
    .unwrap();
    let mut admitted = GraphRunRecord::create(snapshot, serde_json::Value::Null).unwrap();
    admitted.plugin_bindings = bindings
        .iter()
        .cloned()
        .map(|b| (b.id.clone(), b))
        .collect();
    admitted.plugin_bindings_initialized = true;
    test_runs::seed(root.path(), &admitted);
    let mut resolver = HostIoResolver::new(
        HostArtifacts::new(artifacts_root, workspace_root),
        sandbox,
        std::sync::Arc::new(tools),
        tool_host::McpToolConfig::default(),
        LocalInputs::new(root.path().to_path_buf(), None).unwrap(),
        fixed(&[]),
        None,
        crate::execution::tool_assembly::new(crate::application::RunApplication::new(
            root.path().into(),
            root.path().into(),
        )),
    )
    .with_library_root(Some(root.path().to_path_buf()));

    assert_eq!(
        resolver.resolve_plugins(&["fake-tools".into()]).unwrap(),
        bindings
    );
    assert!(resolver.resolve_plugins(&["missing".into()]).is_err());

    let request = NodeExecutionRequest {
        key: InvocationKey {
            run_id: admitted.run_id,
            graph_digest: admitted.graph_digest,
            node_id: "agent".into(),
            invocation: 1,
        },
        model: Some("fixture".into()),
        task: "inspect inputs".into(),
        instructions: "use Anchor tools".into(),
        routes: Vec::new(),
        input: serde_json::Value::Null,
        input_commits: Vec::new(),
        plugins: bindings,
        max_provider_requests: None,
        wall_time_limit_seconds: Some(30.0),
        network: false,
        kind: NodeKind::Agent,
        operation: None,
        writes: Vec::new(),
        cancellation: std::sync::Arc::new(AtomicBool::new(false)),
    };
    let workspace = resolver.workspace(&request).unwrap();
    let tools = resolver.tools(&request).await.unwrap().tools;
    let names = tools
        .definitions()
        .into_iter()
        .map(|definition| definition.name.to_string())
        .collect::<Vec<_>>();
    assert!(names.iter().any(|name| name == tool_host::FAKE_ECHO_TOOL));
    assert!(
        names
            .iter()
            .any(|name| name == crate::tool_names::RUN_TOOL_NAME)
    );
    assert!(workspace.is_dir());

    let replacement = root.path().join("plugins/fake-tools");
    std::fs::create_dir_all(&replacement).unwrap();
    std::fs::write(
        replacement.join("plugin.json"),
        json!({"name":"replacement","network":true}).to_string(),
    )
    .unwrap();
    resolver.tools = std::sync::Arc::new(tool_host::PluginToolHost::new(Vec::new()));
    let error = resolver
        .tools(&request)
        .await
        .err()
        .expect("changed Host authority was accepted");
    assert!(
        error.to_string().contains("Host authorization changed"),
        "{error}"
    );
}

#[tokio::test]
async fn agent_tools_mount_plugins_without_a_run_policy_grant() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let workspace = root.path().join("workspaces");
    let artifacts = root.path().join("artifacts");
    for path in [&workspace, &artifacts] {
        std::fs::create_dir_all(path).unwrap();
    }
    // A Library Plugin with one real resource file.
    let library = root.path().join("library");
    let plugin = library.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("resources")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        json!({"name":"Demo"}).to_string(),
    )
    .unwrap();
    std::fs::write(plugin.join("resources/note.txt"), "plugin-resource").unwrap();
    let bindings = anchor_graph_host::PluginCatalog::resolve(
        &crate::plugin_library::Catalog::new(&library),
        &["demo".into()],
    )
    .unwrap();
    assert_eq!(
        bindings[0].resources,
        vec!["plugin.json", "resources/note.txt"]
    );

    let snapshot = GraphSnapshot::admit(json!({
        "objective":"plugin mount", "entry":"agent",
        "agents":{"worker":{"model":"fixture"}},
        "nodes":[{"id":"agent","agent":"worker","plugins":["demo"]}],
        "edges":[]
    }))
    .unwrap();
    let mut record =
        GraphRunRecord::create_with_id(snapshot, json!({}), "agent-plugin-mount").unwrap();
    record.plugin_bindings = bindings
        .iter()
        .cloned()
        .map(|binding| (binding.id.clone(), binding))
        .collect();
    record.plugin_bindings_initialized = true;
    test_runs::seed(&state, &record);
    crate::run_metadata::save(
        &state,
        &crate::run_metadata::RunMetadata::new(
            record.run_id.clone(),
            "mount".into(),
            record.graph_digest.clone(),
            &library,
        )
        .unwrap(),
    )
    .unwrap();

    // The Run policy deliberately does not authorize the Library at all: the
    // invocation grants the Plugin mount it actually made. A Run cannot know
    // its children's Plugins in advance.
    let sandbox = std::sync::Arc::new(
        BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&workspace),
        )
        .unwrap(),
    );
    let resolver = HostIoResolver::new(
        HostArtifacts::new(artifacts, workspace),
        sandbox,
        std::sync::Arc::new(tool_host::PluginToolHost::new(Vec::<String>::new())),
        tool_host::McpToolConfig::default(),
        LocalInputs::new(state.clone(), None).unwrap(),
        fixed(&[]),
        None,
        crate::execution::tool_assembly::new(crate::application::RunApplication::new(
            state.clone(),
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
        task: "read the plugin resource".into(),
        instructions: String::new(),
        routes: Vec::new(),
        input: json!({}),
        input_commits: Vec::new(),
        plugins: record.plugin_bindings.values().cloned().collect::<Vec<_>>(),
        max_provider_requests: None,
        wall_time_limit_seconds: Some(30.0),
        network: false,
        kind: NodeKind::Agent,
        operation: None,
        writes: Vec::new(),
        cancellation: std::sync::Arc::new(AtomicBool::new(false)),
    };
    let tools = resolver.tools(&request).await.unwrap().tools;
    let result = tools
        .call(
            crate::tool_names::RUN_TOOL_NAME,
            json!({"command":["sh","-c","cat /plugins/demo/resources/note.txt"]}),
        )
        .await
        .unwrap();
    assert_eq!(result[0].as_json().unwrap()["exit_code"], 0);
    assert_eq!(result[0].as_json().unwrap()["stdout"], "plugin-resource");
}

#[tokio::test]
async fn agent_command_tools_receive_only_the_nodes_frozen_local_inputs() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let config = root.path().join("operator-workspaces");
    let workspace = root.path().join("workspaces");
    let artifacts = root.path().join("artifacts");
    let private = root.path().join("private-inputs");
    for path in [&workspace, &artifacts, &private, &config.join("weekly")] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(private.join("history.txt"), "agent-local-evidence").unwrap();
    std::fs::write(
        config.join("weekly/local-inputs.json"),
        json!({"agent":{"history":private}}).to_string(),
    )
    .unwrap();
    let snapshot = GraphSnapshot::admit(json!({
        "objective":"agent local inputs", "entry":"agent", "agents":{"worker":{"model":"fixture"}},
        "nodes":[{"id":"agent","agent":"worker"},{"id":"other","agent":"worker"}],
        "edges":[{"from":"agent","to":"other"}]
    }))
    .unwrap();
    let mut record =
        GraphRunRecord::create_with_id(snapshot, json!({}), "agent-local-inputs").unwrap();
    record.plugin_bindings_initialized = true;
    let local = LocalInputs::new(state.clone(), Some(config.clone())).unwrap();
    local
        .freeze(
            &record.run_id,
            &record.graph_digest,
            &record.snapshot,
            Some("weekly"),
            true,
        )
        .unwrap();
    test_runs::seed(&state, &record);
    crate::run_metadata::save(
        &state,
        &crate::run_metadata::RunMetadata::new(
            record.run_id.clone(),
            "weekly".into(),
            record.graph_digest.clone(),
            root.path(),
        )
        .unwrap(),
    )
    .unwrap();
    let sandbox = std::sync::Arc::new(
        BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["sh"])
                .authorize_workspace_root(&workspace)
                .authorize_readonly_input_root(&artifacts)
                .authorize_readonly_destination_root("/in"),
        )
        .unwrap(),
    );
    let resolver = HostIoResolver::new(
        HostArtifacts::new(artifacts, workspace),
        sandbox,
        std::sync::Arc::new(tool_host::PluginToolHost::new(Vec::<String>::new())),
        tool_host::McpToolConfig::default(),
        local,
        fixed(&[]),
        None,
        crate::execution::tool_assembly::new(crate::application::RunApplication::new(
            state.clone(),
            root.path().into(),
        )),
    );
    let mut request = NodeExecutionRequest {
        key: InvocationKey {
            run_id: record.run_id,
            graph_digest: record.graph_digest,
            node_id: "agent".into(),
            invocation: 1,
        },
        model: Some("fixture".into()),
        task: "read evidence".into(),
        instructions: String::new(),
        routes: Vec::new(),
        input: json!({}),
        input_commits: Vec::new(),
        plugins: Vec::new(),
        max_provider_requests: None,
        wall_time_limit_seconds: Some(30.0),
        network: false,
        kind: NodeKind::Agent,
        operation: None,
        writes: Vec::new(),
        cancellation: std::sync::Arc::new(AtomicBool::new(false)),
    };
    let tools = resolver.tools(&request).await.unwrap().tools;
    let result = tools.call(crate::tool_names::RUN_TOOL_NAME, json!({"command":["sh","-c","cat /local-inputs/history/history.txt; ! touch /local-inputs/history/forbidden"]})).await.unwrap();
    assert_eq!(result[0].as_json().unwrap()["exit_code"], 0);
    assert_eq!(
        result[0].as_json().unwrap()["stdout"],
        "agent-local-evidence"
    );
    assert!(!private.join("forbidden").exists());
    request.key.node_id = "other".into();
    let tools = resolver.tools(&request).await.unwrap().tools;
    let result = tools
        .call(
            crate::tool_names::RUN_TOOL_NAME,
            json!({"command":["sh","-c","test ! -e /local-inputs/history/history.txt"]}),
        )
        .await
        .unwrap();
    assert_eq!(result[0].as_json().unwrap()["exit_code"], 0);
    std::fs::write(
        config.join("weekly/local-inputs.json"),
        json!({"other":{"history":private}}).to_string(),
    )
    .unwrap();
    assert!(resolver.tools(&request).await.is_err());
}
