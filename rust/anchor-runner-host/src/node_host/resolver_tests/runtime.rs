use super::*;

#[tokio::test]
async fn managed_python_is_shared_by_agent_commands_and_op_without_dependency_plugin() {
    let _environment = crate::PROCESS_ENV_LOCK.lock().await;
    struct Clear(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Clear {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..).rev() {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }
    let names = [
        "ANCHOR_DEPENDENCIES_PYTHON",
        "ANCHOR_DEPENDENCIES_UV",
        "ANCHOR_DEPENDENCIES_UV_SHA256",
        "ANCHOR_DEPENDENCIES_SOURCES",
        "ANCHOR_DEPENDENCIES_BUILD_ROOT",
        "ANCHOR_PILOT_PYTHON",
        "ANCHOR_PILOT_PYTHON_ENABLED",
        "ANCHOR_PILOT_RUSTUP",
        "ANCHOR_PILOT_RUSTUP_HOME",
        "ANCHOR_PILOT_CARGO_HOME",
    ];
    let _cleared = Clear(
        names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect(),
    );
    for name in names {
        unsafe { std::env::remove_var(name) }
    }
    let (_root, resolver, _mounts, mut request) = session_fixture(&[], &[], &[]);
    let state = resolver.local_inputs.state_root();
    let managed = state.join("managed");
    let python = managed.join("python/runtime/bin/python3");
    let source = [
        "/usr/bin/python3.12",
        "/usr/bin/python3.11",
        "/usr/bin/python3.10",
        "/usr/bin/python3.9",
        "/usr/bin/python3.8",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).is_file())
    .expect("managed runtime fixture needs supported system Python");
    crate::host_fs::directory(python.parent().unwrap()).unwrap();
    std::fs::copy(source, &python).unwrap();
    let config = managed.join("runtime-config.json");
    std::fs::write(
        &config,
        json!({"format":1,"python":python,"python_enabled":true,"sources":null,"rust":null})
            .to_string(),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let configuration = crate::runtime_configuration::RuntimeConfiguration::from_env(
        state,
        resolver.plugin_library_root().unwrap(),
        "bwrap".into(),
    )
    .unwrap();
    let resolver = resolver.with_runtime_configuration(Some(std::sync::Arc::new(configuration)));
    let tools = resolver.tools(&request).await.unwrap();
    let result=tools.tools.call("anchor_run",json!({"command":["python","-c","import sys; print(sys.executable); print('managed-agent-marker')"]})).await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("managed-agent-marker"), "{text}");
    assert!(text.contains(python.to_str().unwrap()), "{text}");
    drop(tools);
    let nodes = HostNodes {
        sandbox: resolver.sandbox.clone(),
        allowed_commands: vec!["sh".into()],
        artifacts: resolver.artifacts.clone(),
        facts_root: resolver.local_inputs.state_root().join("node-facts"),
        io_resolver: std::sync::Arc::new(resolver),
        goose_nodes: None,
    };
    request.kind = NodeKind::OpRun;
    request.operation = Some(json!(
        "python -c 'import sys; print(sys.executable); print(\"managed-op-marker\")'"
    ));
    request.input = json!({"input":{}});
    request.key.invocation = 2;
    let result = nodes.execute(request).await.unwrap();
    assert!(
        matches!(result,NodeExecutionOutcome::Completed(ref completion) if completion.output["stdout"].as_str().is_some_and(|stdout|stdout.contains("managed-op-marker") && stdout.contains(python.to_str().unwrap()))),
        "{result:?}"
    );
}

#[tokio::test]
async fn an_installed_plugin_code_and_resource_update_is_used_by_the_next_call() {
    let (_root, resolver, _mounts, mut request) =
        session_fixture(&["review"], &[("review", true, None)], &[]);
    let library = resolver.plugin_library_root().unwrap();
    let catalog = crate::plugin_library::Catalog::new(library);
    let admitted = catalog.resolve(&["review".into()]).unwrap().remove(0);
    seed_admitted_binding(&resolver, &mut request, admitted.clone());

    let plugin = library.join("plugins/review");
    std::fs::create_dir_all(plugin.join("skills/new")).unwrap();
    std::fs::write(plugin.join("skills/new/SKILL.md"), "current Skill").unwrap();
    let current = catalog.resolve(&["review".into()]).unwrap().remove(0);
    assert_ne!(current.digest, admitted.digest);
    assert_ne!(current.resources, admitted.resources);
    assert_eq!(current.authorization_digest, admitted.authorization_digest);

    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert_eq!(mounted.bindings, vec![current.clone()]);
    let resolved = resolver.tools(&request).await.unwrap();
    assert_eq!(resolved.plugins, vec![current.clone()]);
    assert_eq!(
        resolver.observed_plugin_digests(&request.key),
        BTreeMap::from([("review".to_owned(), current.digest)])
    );
}

#[tokio::test]
async fn node_tool_selection_waits_for_real_plugin_publication_and_returns_current_skills() {
    let (root, resolver, _mounts, request) =
        session_fixture(&["review"], &[("review", true, None)], &[]);
    let library_root = resolver.plugin_library_root().unwrap();
    let source = root.path().join("updated-plugin");
    std::fs::create_dir_all(source.join("skills/current")).unwrap();
    std::fs::write(
        source.join("plugin.json"),
        r#"{"name":"review","skills":"skills/"}"#,
    )
    .unwrap();
    std::fs::write(source.join("skills/current/SKILL.md"), "current generation").unwrap();
    let library = crate::plugin_library::PluginLibrary::new(library_root);
    let lease = library.install_lease().unwrap();
    let mut resolving = resolver.tools(&request);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), &mut resolving)
            .await
            .is_err()
    );
    library
        .install_directory_locked("review", &source, true, &lease)
        .unwrap();
    let current = crate::plugin_library::Catalog::new(library_root)
        .definition("review")
        .unwrap()
        .binding();
    drop(lease);
    let resolved = tokio::time::timeout(Duration::from_secs(5), resolving)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.plugins.as_slice(), std::slice::from_ref(&current));
    assert!(
        resolved.plugins[0]
            .resources
            .contains(&"skills/current/SKILL.md".to_owned())
    );
    assert_eq!(
        resolver.observed_plugin_digests(&request.key),
        BTreeMap::from([("review".to_owned(), current.digest)])
    );
}

#[tokio::test]
async fn a_graph_pin_keeps_skill_only_execution_leased_and_rechecks_current_grants() {
    let preview = tempfile::tempdir().unwrap();
    let baseline = crate::plugin_library::Catalog::new(library_with(preview.path(), &["review"]))
        .definition("review")
        .unwrap();
    let snapshot = GraphSnapshot::admit(json!({
        "objective":"exact source", "entry":"agent", "agents":{"worker":{"model":"fixture"}},
        "nodes":[{"id":"agent","agent":"worker","plugins":[{"id":"review","revision":baseline.digest}]}],
        "edges":[]
    })).unwrap();
    let (root, resolver, _mounts, request) = session_fixture_with_snapshot(
        &["review"],
        &[("review", true, None)],
        &[],
        Some(snapshot.clone()),
    );
    let library_root = resolver.plugin_library_root().unwrap();
    let library = anchor_library::Library::new(library_root);
    let lease = library.install_lease().unwrap();
    let revision = library
        .retain_revision_locked(
            "review",
            library_root.join("plugins/review"),
            &baseline.digest,
            None,
            &lease,
        )
        .unwrap();
    let catalog = crate::plugin_library::Catalog::new(library_root);
    assert_eq!(
        catalog.resolve_snapshot_locked(&snapshot, &lease).unwrap(),
        [baseline.binding()]
    );
    drop(lease);
    assert_eq!(
        catalog.resolve_snapshot(&snapshot).unwrap(),
        [baseline.binding()]
    );
    let candidate = root.path().join("candidate");
    std::fs::create_dir_all(&candidate).unwrap();
    std::fs::write(candidate.join("plugin.json"), r#"{"name":"review"}"#).unwrap();
    std::fs::write(
        candidate.join("instructions.md"),
        "new default instructions",
    )
    .unwrap();
    let installed = library
        .install_directory("review", &candidate, true)
        .unwrap();
    assert_ne!(installed.digest, baseline.digest);
    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert_eq!(mounted.bindings, [baseline.binding()]);
    let resolved = resolver.tools(&request).await.unwrap();
    assert_eq!(resolved.plugins, [baseline.binding()]);
    assert_eq!(
        resolver.observed_plugin_digests(&request.key),
        BTreeMap::from([("review".to_owned(), baseline.digest.clone())])
    );
    let detail = resolved
        .tools
        .call(
            "anchor_plugin_read",
            json!({
                "plugin":"review", "section":"overview"
            }),
        )
        .await
        .unwrap();
    assert!(
        !serde_json::to_string(&detail)
            .unwrap()
            .contains("new default instructions")
    );
    let lease = library
        .try_install_lease()
        .expect("running pins must not hold the global lease");
    assert!(matches!(
        library.release_revision_locked(&revision, &std::collections::BTreeSet::new(), &lease),
        Err(anchor_library::RevisionError::InUse)
    ));
    std::fs::write(
        candidate.join("plugin.json"),
        r#"{"name":"review","network":true}"#,
    )
    .unwrap();
    library
        .install_directory_locked("review", &candidate, true, &lease)
        .unwrap();
    drop(lease);
    assert!(
        resolver.tools(&request).await.is_err(),
        "old code cannot retain changed grants"
    );
    drop(resolved);
    let lease = library.install_lease().unwrap();
    assert!(
        library
            .release_revision_locked(&revision, &std::collections::BTreeSet::new(), &lease,)
            .unwrap()
    );
}
