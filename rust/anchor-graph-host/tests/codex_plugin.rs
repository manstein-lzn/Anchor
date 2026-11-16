use anchor_graph_host::{FilePluginCatalog, PluginCatalog, PluginConfigurationFieldType};
use anchor_runtime::graph::{GraphSnapshot, GraphTaskPolicy, NetworkMode};
use serde_json::Value;
use std::{fs, path::PathBuf};

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn example(name: &str) -> GraphSnapshot {
    let definition: Value = serde_json::from_slice(
        &fs::read(repository().join(format!("examples/graphs/{name}.json"))).unwrap(),
    )
    .unwrap();
    let snapshot = GraphSnapshot::from_authoring(definition).unwrap();
    snapshot.validate().unwrap();
    snapshot
}

#[test]
fn codex_package_is_an_ordinary_http_plugin_with_owner_bound_configuration() {
    let catalog = FilePluginCatalog::new(repository());
    let definition = catalog.definition("codex").unwrap();
    assert!(definition.system.is_none());
    assert!(definition.channels.is_empty());
    assert!(definition.dependencies.is_empty());
    assert!(definition.unsupported.is_empty());
    assert!(definition.requires_network());
    assert_eq!(definition.skills, ["skills/codex/SKILL.md"]);
    assert_eq!(definition.mcp_servers.len(), 1);
    let server = &definition.mcp_servers[0];
    assert_eq!(server.name, "codex");
    assert_eq!(server.config["type"], "http");
    assert_eq!(server.config["url"], "${ANCHOR_CODEX_BRIDGE_URL}/mcp");
    assert_eq!(
        server.config["bearer_token_env_var"],
        "ANCHOR_CODEX_BRIDGE_TOKEN"
    );
    assert_eq!(
        server.config["headers"]["X-Codex-Project-Scope"],
        "${ANCHOR_CODEX_PROJECT_SCOPE:-}"
    );
    assert!(server.config.get("command").is_none());
    assert!(server.config["headers"].get("Authorization").is_none());

    let fields = &definition.configuration.fields;
    assert_eq!(fields.len(), 3);
    assert_eq!(fields["bridge_url"].kind, PluginConfigurationFieldType::Url);
    assert_eq!(
        fields["bridge_url"].default.as_deref(),
        Some("http://127.0.0.1:8091")
    );
    assert!(fields["api_key"].is_secret());
    assert!(fields["api_key"].required);
    assert!(fields["api_key"].default.is_none());
    assert_eq!(
        fields["project_scope"].kind,
        PluginConfigurationFieldType::String
    );
    assert!(!fields["project_scope"].required);
    fields["project_scope"].validate_value("").unwrap();
    let environment = definition.configuration.environment_for("codex").unwrap();
    assert_eq!(environment.len(), 3);
    assert_eq!(environment["ANCHOR_CODEX_BRIDGE_URL"], "bridge_url");
    assert_eq!(environment["ANCHOR_CODEX_BRIDGE_TOKEN"], "api_key");
    assert_eq!(environment["ANCHOR_CODEX_PROJECT_SCOPE"], "project_scope");

    let bindings = catalog.resolve(&["codex".to_owned()]).unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].id, "codex");
    assert_eq!(bindings[0].mcp_servers, ["codex"]);
}

#[test]
fn codex_http_expansion_accepts_absent_empty_and_populated_scope() {
    let catalog = FilePluginCatalog::new(repository());
    let definition = catalog.definition("codex").unwrap();
    let declaration = definition.mcp_servers.clone();
    let default_url = definition.configuration.fields["bridge_url"]
        .default
        .as_deref()
        .unwrap();
    for scope in [None, Some(""), Some("Anchor"), Some("marketdata/simai")] {
        let resolved = catalog
            .resolve_mcp_servers_with(&definition, &|name| match name {
                "ANCHOR_CODEX_BRIDGE_URL" => Some(default_url.into()),
                "ANCHOR_CODEX_BRIDGE_TOKEN" => Some("fixture-owner-token".into()),
                "ANCHOR_CODEX_PROJECT_SCOPE" => scope.map(str::to_owned),
                _ => None,
            })
            .unwrap();
        assert_eq!(resolved.len(), 1);
        let server = &resolved[0];
        assert_eq!(server.name, "codex");
        assert_eq!(server.config["url"], "http://127.0.0.1:8091/mcp");
        assert_eq!(
            server.config["headers"]["Authorization"],
            "Bearer fixture-owner-token"
        );
        assert_eq!(
            server.config["headers"]["X-Codex-Project-Scope"],
            scope.unwrap_or("")
        );
        assert!(!format!("{resolved:?}").contains("fixture-owner-token"));
        assert_eq!(definition.mcp_servers, declaration);
    }
    assert!(
        catalog
            .resolve_mcp_servers_with(&definition, &|name| {
                (name == "ANCHOR_CODEX_BRIDGE_URL").then(|| default_url.to_owned())
            })
            .is_err(),
        "An optional scope must not make the bearer token optional"
    );
}

#[test]
fn codex_managers_validate_as_separate_persistent_channel_loops() {
    for channel in ["feishu", "wecom"] {
        let snapshot = example(&format!("{channel}-codex-project-manager"));
        assert_eq!(snapshot.entry, "wait_input");
        assert_eq!(snapshot.nodes.len(), 3);
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.ops.len(), 2);
        assert_eq!(
            snapshot.ops["wait_input"]["host"]["operation"],
            "session.wait_input"
        );
        assert_eq!(snapshot.ops["reply"]["host"]["operation"], "session.reply");
        let edges: Vec<_> = snapshot
            .edges
            .iter()
            .map(|edge| (edge.from_node.as_str(), edge.to_node.as_str()))
            .collect();
        assert_eq!(
            edges,
            [
                ("wait_input", "manager"),
                ("manager", "reply"),
                ("reply", "wait_input")
            ]
        );
        let manager = snapshot
            .nodes
            .iter()
            .find(|node| node.id == "manager")
            .unwrap();
        assert_eq!(manager.agent.as_deref(), Some("manager"));
        assert_eq!(snapshot.node_network_mode(manager), NetworkMode::Auto);
        let plugins: Vec<_> = manager
            .plugins
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect();
        assert_eq!(plugins, [channel, "codex", "network", "graph-tasks"]);
        let policy = manager
            .plugins
            .iter()
            .find(|plugin| plugin.id == "graph-tasks")
            .unwrap();
        assert_eq!(
            policy.graph_tasks,
            Some(GraphTaskPolicy {
                graphs: vec!["codex-watch-worker".into()],
                ..GraphTaskPolicy::default()
            })
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .filter(|node| node.op.is_some())
                .all(|node| node.plugins.is_empty())
        );
    }
}

#[test]
fn codex_watch_is_a_terminal_observer_without_channel_or_delegation() {
    let snapshot = example("codex-watch-worker");
    assert_eq!(snapshot.entry, "watch");
    assert_eq!(snapshot.nodes.len(), 1);
    assert!(snapshot.edges.is_empty());
    assert!(snapshot.ops.is_empty());
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents["watcher"].writes, ["codex-watch.json"]);
    let watcher = &snapshot.nodes[0];
    assert_eq!(watcher.agent.as_deref(), Some("watcher"));
    assert_eq!(snapshot.node_network_mode(watcher), NetworkMode::Auto);
    assert_eq!(
        watcher
            .plugins
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect::<Vec<_>>(),
        ["codex", "network"]
    );
    assert!(
        watcher
            .plugins
            .iter()
            .all(|plugin| plugin.graph_tasks.is_none())
    );
    assert!(
        FilePluginCatalog::new(repository())
            .snapshot_requires_network(&snapshot)
            .unwrap()
    );
}
