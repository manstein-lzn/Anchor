use super::*;
use crate::plugin_test_support::{catalog_with_dependency_plugin, catalog_with_plugin};
use crate::views_root;
use std::fs;

fn network_snapshot(mode: &str, plugins: &[&str]) -> GraphSnapshot {
    let mut plugins = plugins
        .iter()
        .map(|id| serde_json::json!(id))
        .collect::<Vec<_>>();
    plugins.push(serde_json::json!({"id":"network","network":mode,"required":true,"default":true}));
    GraphSnapshot::from_authoring(serde_json::json!({
        "objective":"network policy", "agents":{"role":{}},
        "nodes":[{"id":"work","agent":"role","plugins":plugins}], "edges":[]
    }))
    .unwrap()
}

#[test]
fn snapshot_resolution_distinguishes_policy_from_a_required_provider() {
    let root = tempfile::tempdir().unwrap();
    let catalog = FilePluginCatalog::new(root.path());
    for (id, manifest) in [
        ("offline", serde_json::json!({"name":"Offline"})),
        (
            "online",
            serde_json::json!({"name":"Online","network":true}),
        ),
        (
            "remote",
            serde_json::json!({"name":"Remote","mcpServers":{"api":{"url":"https://example.test/mcp"}}}),
        ),
    ] {
        let directory = root.path().join("plugins").join(id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("plugin.json"), manifest.to_string()).unwrap();
        assert_eq!(
            catalog.metadata(id).unwrap().requires_network(),
            id != "offline"
        );
        assert_eq!(
            catalog.definition(id).unwrap().requires_network(),
            id != "offline"
        );
    }

    for (mode, plugin, expected) in [
        ("denied", None, Vec::new()),
        ("auto", None, Vec::new()),
        ("auto", Some("offline"), vec!["offline"]),
        ("denied", Some("online"), vec!["online"]),
        ("denied", Some("remote"), vec!["remote"]),
    ] {
        let ids = plugin.into_iter().collect::<Vec<_>>();
        let snapshot = network_snapshot(mode, &ids);
        assert!(!catalog.snapshot_requires_network(&snapshot).unwrap());
        let bindings = catalog.resolve_snapshot(&snapshot).unwrap();
        assert_eq!(
            bindings
                .iter()
                .map(|binding| binding.id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
    }
    for (mode, ids) in [
        ("enabled", vec![]),
        ("auto", vec!["online"]),
        ("auto", vec!["remote"]),
    ] {
        let snapshot = network_snapshot(mode, &ids);
        assert!(catalog.snapshot_requires_network(&snapshot).unwrap());
        assert!(
            catalog
                .resolve_snapshot(&snapshot)
                .unwrap_err()
                .to_string()
                .contains("network")
        );
    }

    crate::plugin_test_support::system_plugin(root.path(), SystemPlugin::Network);
    for (mode, ids, expected) in [
        ("enabled", vec![], vec!["network"]),
        ("auto", vec!["offline"], vec!["offline"]),
        ("auto", vec!["online"], vec!["network", "online"]),
        ("auto", vec!["remote"], vec!["network", "remote"]),
        ("denied", vec!["remote"], vec!["remote"]),
    ] {
        let snapshot = network_snapshot(mode, &ids);
        let digest = snapshot.digest().unwrap();
        let bindings = catalog.resolve_snapshot(&snapshot).unwrap();
        assert_eq!(
            bindings
                .iter()
                .map(|binding| binding.id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(snapshot.digest().unwrap(), digest);
    }
    fs::remove_file(root.path().join("plugins/online/plugin.json")).unwrap();
    assert!(
        catalog
            .snapshot_requires_network(&network_snapshot("auto", &["online"]))
            .is_err()
    );
}

#[test]
fn catalogs_without_metadata_resolve_only_explicit_network_requirements() {
    #[derive(Default)]
    struct Catalog(std::sync::Mutex<Vec<String>>);
    impl PluginCatalog for Catalog {
        fn resolve(&self, ids: &[String]) -> Result<Vec<PluginBinding>, GraphError> {
            *self.0.lock().unwrap() = ids.to_vec();
            Ok(Vec::new())
        }
    }
    let catalog = Catalog::default();
    for (mode, expected) in [
        ("denied", vec!["offline"]),
        ("auto", vec!["offline"]),
        ("enabled", vec!["network", "offline"]),
    ] {
        catalog
            .resolve_snapshot(&network_snapshot(mode, &["offline"]))
            .unwrap();
        assert_eq!(*catalog.0.lock().unwrap(), expected);
    }
}

#[test]
fn system_identity_requires_the_releases_public_resources_and_inventory() {
    let root = tempfile::tempdir().unwrap();
    let catalog = FilePluginCatalog::new(root.path());
    for plugin in crate::all_system_plugins() {
        crate::plugin_test_support::system_plugin(root.path(), *plugin);
        let definition = catalog.definition(plugin.id()).unwrap();
        assert_eq!(definition.system, Some(*plugin));
        assert_eq!(catalog.metadata(plugin.id()).unwrap().system, Some(*plugin));
        assert_eq!(definition.metadata().system, Some(*plugin));
        assert_eq!(SystemPlugin::from_id(plugin.id()), Some(*plugin));

        let skill = plugin
            .public_files()
            .iter()
            .find(|(path, _)| path.ends_with("SKILL.md"))
            .unwrap()
            .0;
        fs::write(definition.directory.join(skill), "modified public content").unwrap();
        assert!(catalog.definition(plugin.id()).is_err());
        assert!(catalog.metadata(plugin.id()).is_err());
        assert!(catalog.channels(plugin.id()).is_err());
        assert!(catalog.mcp_servers(plugin.id(), false).is_err());
        crate::plugin_test_support::system_plugin(root.path(), *plugin);

        let extra = definition.directory.join("unexpected.txt");
        fs::write(&extra, "not a platform resource").unwrap();
        assert!(catalog.definition(plugin.id()).is_err());
        fs::remove_file(extra).unwrap();
        assert!(catalog.definition(plugin.id()).is_ok());
    }
    assert_eq!(SystemPlugin::from_id("git"), None);
    assert_eq!(SystemPlugin::from_id("anchor-steward"), None);
}

#[test]
fn extensions_cannot_assert_system_identity_or_claim_host_channel_platforms() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("plugins/extension");
    fs::create_dir_all(&directory).unwrap();
    let manifest = directory.join("plugin.json");
    fs::write(&manifest, r#"{"name":"Extension","system":true}"#).unwrap();
    let catalog = FilePluginCatalog::new(root.path());
    assert!(catalog.definition("extension").is_err());
    fs::write(&manifest, r#"{"name":"Extension"}"#).unwrap();
    assert_eq!(catalog.definition("extension").unwrap().system, None);
    for plugin in [SystemPlugin::Feishu, SystemPlugin::Wecom] {
        let channel = plugin
            .public_files()
            .iter()
            .find(|(path, _)| *path == "channel.json")
            .unwrap()
            .1;
        fs::write(directory.join("channel.json"), channel).unwrap();
        assert!(catalog.definition("extension").is_err());
        assert!(catalog.channels("extension").is_err());
    }
    fs::remove_file(directory.join("channel.json")).unwrap();
    fs::rename(directory, root.path().join("plugins/graph-tasks")).unwrap();
    assert!(catalog.definition("graph-tasks").is_err());
}

#[cfg(unix)]
#[test]
fn system_executables_must_remain_executable() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    crate::plugin_test_support::system_plugin(root.path(), SystemPlugin::Network);
    let path = root.path().join("plugins/network/bin/anchor-network-tools");
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        FilePluginCatalog::new(root.path())
            .metadata("network")
            .is_err()
    );
}

#[test]
fn a_network_requirement_is_strict_and_part_of_the_approved_plugin_code() {
    let (tmp, catalog) = catalog_with_dependency_plugin();
    let path = tmp.path().join("plugins/probe/plugin.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let original = catalog.definition("probe").unwrap();
    assert!(!original.network);
    manifest["network"] = Value::Bool(true);
    fs::write(&path, manifest.to_string()).unwrap();
    let networked = catalog.definition("probe").unwrap();
    assert!(networked.network);
    assert_ne!(original.digest, networked.digest);
    manifest["network"] = "true".into();
    fs::write(path, manifest.to_string()).unwrap();
    assert!(
        catalog
            .definition("probe")
            .unwrap_err()
            .to_string()
            .contains("network must be a boolean")
    );
    assert!(
        catalog
            .mcp_servers("probe", false)
            .unwrap_err()
            .to_string()
            .contains("network must be a boolean")
    );
}

#[test]
fn a_view_path_is_derived_from_the_declaration_both_halves_agree_on() {
    let (tmp, catalog) = catalog_with_dependency_plugin();
    let definition = catalog.definition("probe").unwrap();
    assert_eq!(definition.dependencies.python, vec!["idna==3.6"]);
    let view = catalog.view_path("probe").unwrap().expect("a view path");
    let root = tmp.path().canonicalize().unwrap();
    assert_eq!(
        view,
        plugin_view_path(
            &root,
            "probe",
            &definition.dependencies.declaration_digest()
        )
    );
    assert!(view.starts_with(views_root(&root)));
    // The same declaration in a different order is the same view; a changed
    // one is not.
    let dir = tmp.path().join("plugins/other");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("plugin.json"),
        r#"{"name":"Other","dependencies":{"python":["idna==3.6"]}}"#,
    )
    .unwrap();
    // The same declaration is the same store content, but a view belongs to
    // one Plugin: `other` gets its own directory under the same digest.
    assert_eq!(
        view.file_name(),
        catalog.view_path("other").unwrap().unwrap().file_name()
    );
    assert_ne!(catalog.view_path("other").unwrap(), Some(view));

    // Declaring nothing means no view at all.
    let plain = tmp.path().join("plugins/plain");
    fs::create_dir_all(&plain).unwrap();
    fs::write(plain.join("plugin.json"), r#"{"name":"Plain"}"#).unwrap();
    assert_eq!(catalog.view_path("plain").unwrap(), None);
}

/// The digest covers code only: a declared mutable path is the Plugin's own
/// state, and what it contains is never part of "which code is this".
#[test]
fn writing_into_a_declared_mutable_path_does_not_change_the_digest() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/probe");
    fs::create_dir_all(dir.join("skills")).unwrap();
    fs::write(
        dir.join("plugin.json"),
        r#"{"name":"Probe","mutable":["data/","cache.db"],"skills":"skills/"}"#,
    )
    .unwrap();
    fs::write(dir.join("skills/SKILL.md"), "code").unwrap();
    let catalog = FilePluginCatalog::new(tmp.path());
    let before = catalog.definition("probe").unwrap();
    assert_eq!(before.mutable.paths(), ["data/", "cache.db"]);
    // The mutable paths are not resources either.
    assert_eq!(before.digest.len(), 64);
    assert!(before.mutable.covers("data/state.json"));
    assert!(before.mutable.covers("cache.db"));
    assert!(!before.mutable.covers("cache.db.bak"));
    assert!(!before.mutable.covers("skills/SKILL.md"));
    // The Plugin writes its state, and the code digest does not move. Its own
    // state may even contain what the platform refuses in *code*: a symlink and
    // a credentials file are the Plugin's business inside a declared mutable
    // path (invariant 9).
    fs::create_dir_all(dir.join("data")).unwrap();
    fs::write(dir.join("data/state.json"), "state").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", dir.join("data/link")).unwrap();
    fs::write(dir.join("data/.env"), "TOKEN=plugin-own").unwrap();
    fs::write(dir.join("cache.db"), "cache").unwrap();
    let after = catalog.definition("probe").unwrap();
    assert_eq!(after.digest, before.digest);
    assert_eq!(after.resources, before.resources);
    let binding = catalog.resolve(&["probe".into()]).unwrap().remove(0);
    assert!(
        !binding
            .resources
            .iter()
            .any(|path| path.starts_with("data/"))
    );
    assert!(!binding.resources.contains(&"cache.db".to_owned()));
    // A code change does move it.
    fs::write(dir.join("skills/SKILL.md"), "changed").unwrap();
    assert_ne!(catalog.definition("probe").unwrap().digest, before.digest);
}

/// A mutable declaration that could leave the Plugin directory is refused.
#[test]
fn an_escaping_or_mistyped_mutable_declaration_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    for (field, declared) in [
        ("absolute", "/etc/passwd"),
        ("traversal", "../outside"),
        ("the manifest itself", "plugin.json"),
        ("empty", ""),
    ] {
        let dir = tmp.path().join("plugins/escaping");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("plugin.json"),
            serde_json::json!({"name":"Probe","mutable":[declared]}).to_string(),
        )
        .unwrap();
        assert!(
            FilePluginCatalog::new(tmp.path())
                .definition("escaping")
                .is_err(),
            "{field} was accepted"
        );
    }
}

#[test]
fn a_mistyped_dependencies_field_is_refused_by_the_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/typo");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("plugin.json"),
        r#"{"name":"Typo","dependencie":{"python":["idna==3.6"]}}"#,
    )
    .unwrap();
    let error = FilePluginCatalog::new(tmp.path())
        .definition("typo")
        .unwrap_err();
    assert!(
        error.to_string().contains("unsupported manifest field"),
        "{error}"
    );
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
fn metadata_is_live_and_separate_from_the_execution_content_identity() {
    let (tmp, catalog) = catalog_with_plugin();
    let resource = tmp.path().join("plugins/demo/notes.txt");
    fs::write(&resource, "first revision").unwrap();
    let first = catalog.definition("demo").unwrap();
    assert_eq!(catalog.metadata("demo").unwrap(), first.metadata());
    fs::write(&resource, "second revision").unwrap();
    assert_eq!(catalog.metadata("demo").unwrap(), first.metadata());
    let next = catalog.definition("demo").unwrap();
    assert_ne!(first.binding().digest, next.binding().digest);
    assert_eq!(catalog.metadata("demo").unwrap(), next.metadata());
    fs::write(
        tmp.path().join("plugins/demo/plugin.json"),
        r#"{"name":"Renamed","description":"current facts","skills":"skills/"}"#,
    )
    .unwrap();
    let metadata = catalog.metadata("demo").unwrap();
    assert_eq!(metadata.name, "Renamed");
    assert_eq!(metadata.description, "current facts");
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

#[cfg(unix)]
#[test]
fn plugin_root_symlink_is_accepted() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source/demo");
    fs::create_dir_all(source.join("skills/example")).unwrap();
    fs::write(
        source.join("plugin.json"),
        r#"{"name":"Demo","mcpServers":{}}"#,
    )
    .unwrap();
    fs::write(source.join("skills/example/SKILL.md"), "hello").unwrap();
    fs::create_dir_all(tmp.path().join("plugins")).unwrap();
    symlink(&source, tmp.path().join("plugins/demo")).unwrap();

    let binding = FilePluginCatalog::new(tmp.path())
        .resolve(&["demo".into()])
        .unwrap()
        .remove(0);
    assert_eq!(
        binding.resources,
        vec!["plugin.json", "skills/example/SKILL.md"]
    );
}

#[test]
fn adopts_manifest_skills_description_channels_and_mcp_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/contract");
    fs::create_dir_all(dir.join("custom/one")).unwrap();
    fs::write(dir.join("custom/one/SKILL.md"), "skill").unwrap();
    fs::write(dir.join("gateway.sh"), "# entrypoint").unwrap();
    fs::write(
        dir.join("plugin.json"),
        r#"{
            "name":"Contract",
            "description":"",
            "interface":{"longDescription":"from interface"},
            "skills":["custom"],
            "hooks":[],
            "mcpServers":"mcp-config.json"
        }"#,
    )
    .unwrap();
    fs::write(
        dir.join("mcp-config.json"),
        r#"{"mcpServers":{"gateway":{"url":"https://${MISSING:-example.test}/mcp","headers":{"X-Root":"${CODEX_PLUGIN_ROOT}","X-Literal":"${bad-name}"}}}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("channel.json"),
        r#"{"platform":"demo_platform","transport":"websocket","entrypoint":"gateway.sh","required_environment":["DEMO_TOKEN"],"description":"channel","sdk":"sdk"}"#,
    )
    .unwrap();

    let catalog = FilePluginCatalog::new(tmp.path());
    let definition = catalog.definition("contract").unwrap();
    assert_eq!(definition.description, "from interface");
    assert_eq!(definition.skills, vec!["custom/one/SKILL.md"]);
    assert!(definition.unsupported.is_empty());
    assert_eq!(definition.mcp_servers.len(), 1);
    assert_eq!(definition.channels[0].platform, "demo_platform");
    assert_eq!(
        definition.channels[0].required_environment,
        vec!["DEMO_TOKEN"]
    );

    let servers = catalog.mcp_servers("contract", true).unwrap();
    let config = servers[0].config.as_object().unwrap();
    assert_eq!(config["url"], "https://example.test/mcp");
    assert_eq!(config["headers"]["X-Root"], dir.to_string_lossy().as_ref());
    assert_eq!(config["headers"]["X-Literal"], "${bad-name}");
}
