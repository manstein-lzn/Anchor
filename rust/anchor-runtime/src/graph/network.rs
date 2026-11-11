use super::*;
use serde_json::json;

impl GraphSnapshot {
    /// Canonical authoring projection, never applied to a retained Run snapshot.
    /// The legacy mode is used only when opening an existing Graph for editing;
    /// it can be removed after the local catalog has been migrated and verified.
    pub fn normalize_network_authoring(mut definition: Value, legacy: bool) -> Value {
        // Projection cannot repair invalid author input by dropping its fields.
        // Leave diagnostics to the same compiler the write path already uses.
        if Self::from_authoring(definition.clone()).is_err() {
            return definition;
        }
        let agents = definition.get("agents").cloned().unwrap_or_default();
        let ops = definition.get("ops").cloned().unwrap_or_default();
        fn body(value: &mut Value, agents: &Value, ops: &Value, legacy: bool) {
            if let Some(nodes) = value.get_mut("nodes").and_then(Value::as_array_mut) {
                for node in nodes {
                    let source = if let Some(agent) = node.get("agent").and_then(Value::as_str) {
                        &agents[agent]
                    } else if let Some(op) = node.get("op").and_then(Value::as_str) {
                        if ops[op].get("run").is_none() {
                            continue;
                        }
                        &ops[op]
                    } else {
                        continue;
                    };
                    let prior = source.get("network").and_then(Value::as_bool);
                    let Some(node) = node.as_object_mut() else {
                        continue;
                    };
                    let mut plugins = node
                        .get("plugins")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let reference = plugins.iter().position(|plugin| {
                        plugin.as_str() == Some("network")
                            || plugin.get("id").and_then(Value::as_str) == Some("network")
                    });
                    let explicit = reference
                        .and_then(|index| plugins[index].get("network"))
                        .is_some();
                    if explicit {
                        continue;
                    }
                    let mode = match prior {
                        Some(true) => NetworkMode::Enabled,
                        Some(false) => NetworkMode::Denied,
                        None if legacy => NetworkMode::Denied,
                        None if reference.is_some() => NetworkMode::Enabled,
                        None => NetworkMode::Auto,
                    };
                    if let Some(index) = reference {
                        if plugins[index].is_string() {
                            plugins[index] = json!({"id":"network"});
                        }
                        plugins[index]["network"] = json!(mode);
                    } else {
                        plugins.push(json!({"id":"network","required":false,"network":mode}));
                    }
                    node.insert("plugins".into(), json!(plugins));
                }
            }
            if let Some(graphs) = value.get_mut("graphs").and_then(Value::as_object_mut) {
                for nested in graphs.values_mut() {
                    body(nested, agents, ops, legacy);
                }
            }
        }
        body(&mut definition, &agents, &ops, legacy);
        for field in ["agents", "ops"] {
            if let Some(entries) = definition.get_mut(field).and_then(Value::as_object_mut) {
                for entry in entries.values_mut() {
                    if let Some(entry) = entry.as_object_mut() {
                        entry.remove("network");
                    }
                }
            }
        }
        definition
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_preserves_denial_shared_roles_and_snapshot_digest() {
        let definition = json!({"objective":"network policy","agents":{"role":{"network":false}},"ops":{"cmd":{"run":"true","network":true}},
            "nodes":[{"id":"a","agent":"role","plugins":["network","search"]},{"id":"b","agent":"role","plugins":["search"]},{"id":"c","op":"cmd"}],"edges":[{"from":"a","to":"b"},{"from":"b","to":"c"}]});
        let original = GraphSnapshot::from_authoring(definition.clone()).unwrap();
        let digest = original.digest().unwrap();
        let normalized = GraphSnapshot::normalize_network_authoring(definition, true);
        assert!(normalized["agents"]["role"].get("network").is_none());
        assert!(normalized["ops"]["cmd"].get("network").is_none());
        let current = GraphSnapshot::from_authoring(normalized).unwrap();
        let modes = current
            .nodes
            .iter()
            .map(|node| current.node_network_mode(node))
            .collect::<Vec<_>>();
        assert_eq!(
            modes,
            [
                NetworkMode::Denied,
                NetworkMode::Denied,
                NetworkMode::Enabled
            ]
        );
        assert_eq!(original.digest().unwrap(), digest);
        let loaded = GraphSnapshot::admit(serde_json::to_value(&original).unwrap()).unwrap();
        assert_eq!(loaded.digest().unwrap(), digest);
    }

    #[test]
    fn policies_are_node_scoped_and_ops_do_not_gain_arbitrary_plugins() {
        let definition = json!({"objective":"network","agents":{"role":{}},"ops":{"cmd":{"run":"true"}},
            "nodes":[{"id":"a","agent":"role","plugins":[{"id":"network","network":"denied"}]},{"id":"b","agent":"role","plugins":["search"]},{"id":"c","op":"cmd","plugins":[{"id":"network","network":"enabled"}]}],"edges":[{"from":"a","to":"b"},{"from":"b","to":"c"}]});
        let mut normalized = GraphSnapshot::normalize_network_authoring(definition, false);
        let snapshot = GraphSnapshot::from_authoring(normalized.clone()).unwrap();
        assert_eq!(
            snapshot.node_network_mode(&snapshot.nodes[0]),
            NetworkMode::Denied
        );
        assert_eq!(
            snapshot.node_network_mode(&snapshot.nodes[1]),
            NetworkMode::Auto
        );
        assert_eq!(
            snapshot.node_network_mode(&snapshot.nodes[2]),
            NetworkMode::Enabled
        );
        normalized["nodes"][2]["plugins"] = json!(["search"]);
        assert!(
            GraphSnapshot::from_authoring(normalized)
                .unwrap_err()
                .to_string()
                .contains("only the network")
        );
    }

    #[test]
    fn pluginless_drafts_keep_their_policy_across_editing_and_invalid_input_is_not_repaired() {
        let definition =
            json!({"agents":{"role":{}},"nodes":[{"id":"work","agent":"role"}],"edges":[]});
        for (legacy, mode) in [(true, NetworkMode::Denied), (false, NetworkMode::Auto)] {
            let normalized = GraphSnapshot::normalize_network_authoring(definition.clone(), legacy);
            let snapshot = GraphSnapshot::from_authoring(normalized.clone()).unwrap();
            assert_eq!(snapshot.node_network_mode(&snapshot.nodes[0]), mode);
            assert_eq!(
                GraphSnapshot::normalize_network_authoring(normalized.clone(), true),
                normalized
            );
        }
        let mut invalid = definition.clone();
        invalid["agents"]["role"]["network"] = json!("yes");
        assert_eq!(
            GraphSnapshot::normalize_network_authoring(invalid.clone(), false),
            invalid
        );
        assert!(GraphSnapshot::from_authoring(invalid).is_err());
        let nullable = json!({"agents":{"role":{"network":true}},"nodes":[{"id":"work","agent":"role","plugins":[{"id":"network","network":null}]}],"edges":[]});
        assert_eq!(
            GraphSnapshot::normalize_network_authoring(nullable.clone(), true),
            nullable
        );
        assert!(GraphSnapshot::from_authoring(nullable).is_err());
        let mut conflicting = definition;
        conflicting["nodes"][0]["plugins"] = json!([
            {"id":"network","network":"enabled"}, {"id":"network","network":"denied"}
        ]);
        assert!(GraphSnapshot::from_authoring(conflicting).is_err());
    }

    #[test]
    fn admission_binds_executable_resources_not_offline_policy_markers() {
        let definition =
            json!({"agents":{"role":{}},"nodes":[{"id":"work","agent":"role"}],"edges":[]});
        let mut snapshot = GraphSnapshot::from_authoring(
            GraphSnapshot::normalize_network_authoring(definition, false),
        )
        .unwrap();
        let provider = PluginBinding {
            id: "network".into(),
            digest: "code".into(),
            resources: vec![],
            mcp_servers: vec![],
            authorization_digest: "authority".try_into().unwrap(),
        };
        assert!(
            GraphRunRecord::create_with_plugins(
                snapshot.clone(),
                Value::Null,
                "auto-offline",
                vec![]
            )
            .is_ok()
        );
        assert!(
            GraphRunRecord::create_with_plugins(
                snapshot.clone(),
                Value::Null,
                "auto-online",
                vec![provider.clone()]
            )
            .is_ok()
        );
        snapshot.nodes[0].plugins[0].network = Some(NetworkMode::Denied);
        assert!(
            GraphRunRecord::create_with_plugins(snapshot.clone(), Value::Null, "denied", vec![])
                .is_ok()
        );
        assert!(
            GraphRunRecord::create_with_plugins(
                snapshot.clone(),
                Value::Null,
                "denied-extra",
                vec![provider.clone()]
            )
            .is_err()
        );
        snapshot.nodes[0].plugins[0].network = Some(NetworkMode::Enabled);
        assert!(
            GraphRunRecord::create_with_plugins(
                snapshot.clone(),
                Value::Null,
                "enabled-missing",
                vec![]
            )
            .is_err()
        );
        assert!(
            GraphRunRecord::create_with_plugins(snapshot, Value::Null, "enabled", vec![provider])
                .is_ok()
        );
    }
}
