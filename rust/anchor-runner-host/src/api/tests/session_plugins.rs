use super::*;
use anchor_platform_session::SessionStore;
use std::path::Path;

use crate::test_support::EnvVar;

/// One installed Plugin, exactly as the Library stores it.
fn write_plugin(state: &ApiState, id: &str, manifest: Value) {
    if let Some(system) = anchor_graph_host::SystemPlugin::from_id(id) {
        crate::test_system_plugin(&state.catalog_root, system);
        return;
    }
    let plugin = state.catalog_root.join("plugins").join(id);
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.json"), manifest.to_string()).unwrap();
}

/// Declare Plugins in the Graph *file*, which the Session view must not answer
/// from: the mount filter reads the admitted Run's frozen snapshot instead.
fn declare_plugins_in_the_file(state: &ApiState, references: &[(&str, bool, Option<bool>)]) {
    let declared = references
        .iter()
        .map(|(id, required, default)| {
            let mut reference = json!({"id": id, "required": required});
            if let Some(default) = default {
                reference["default"] = json!(default);
            }
            reference
        })
        .collect::<Vec<_>>();
    let graph = json!({
        "objective":"fixture","entry":"work","agents":{"worker":{"model":"fixture"}},"ops":{},
        "nodes":[{"id":"work","agent":"worker","plugins":declared}],
        "edges":[]
    });
    std::fs::write(state.bundle_root.join("graph.json"), graph.to_string()).unwrap();
    let manifest = json!({
        "format":1,"graph":"graph.json",
        "plugins":references.iter().map(|(id, _, _)| json!({"id": id})).collect::<Vec<_>>()
    });
    std::fs::write(
        state.bundle_root.join("manifest.json"),
        manifest.to_string(),
    )
    .unwrap();
}

/// One admitted Run for the Session whose frozen snapshot declares `references`,
/// with the Graph name its metadata records. This is the snapshot the page and
/// the mount filter must agree on.
fn freeze_run(
    state: &ApiState,
    session: &str,
    graph: &str,
    references: &[(&str, bool, Option<bool>)],
) {
    static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let declared = references
        .iter()
        .map(|(id, required, default)| {
            let mut reference = json!({"id": id, "required": required});
            if let Some(default) = default {
                reference["default"] = json!(default);
            }
            reference
        })
        .collect::<Vec<_>>();
    let snapshot = GraphSnapshot::admit(json!({
        "objective":"session plugins","entry":"work",
        "agents":{"worker":{"model":"fixture"}},"ops":{},
        "nodes":[{"id":"work","agent":"worker","plugins":declared}],
        "edges":[]
    }))
    .unwrap();
    let run = format!(
        "run-{session}-{}",
        RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let record = GraphRunRecord::create_with_id(snapshot, json!({}), run).unwrap();
    fixtures::create(&state.application, &record);
    let metadata = RunMetadata::new(
        record.run_id.clone(),
        graph.into(),
        record.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    metadata::save(&state.data_root, &metadata).unwrap();
    session_store(state)
        .attach_run("local", session, &record.run_id)
        .unwrap();
}

/// Every byte the Library owns: Plugin directories, dependency views and the
/// shared store. Anything changing under here would mean a refusal installed,
/// built a view or fetched a package after all.
fn library_bytes(state: &ApiState) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, directory: &Path, files: &mut Vec<(String, Vec<u8>)>) {
        let mut entries = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }
    let root = state.application.library_root().to_path_buf();
    let mut files = Vec::new();
    for subtree in ["plugins", "dependencies"] {
        let path = root.join(subtree);
        if path.is_dir() {
            walk(&root, &path, &mut files);
        }
    }
    files
}

fn session_store(state: &ApiState) -> SessionStore {
    SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap()
}

async fn create_session(app: Router, id: &str) {
    let body = json!({"id": id}).to_string();
    let (status, created) = call(app, "POST", "/sessions", Some(&body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
}

async fn wait_for_plugin_writer(application: &RunApplication) {
    use std::time::Duration;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match tokio::time::timeout(
                Duration::from_millis(20),
                application.graph_catalog_mutation_guard(),
            )
            .await
            {
                Ok(guard) => drop(guard),
                Err(_) => return,
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the Plugin writer did not acquire the catalog guard");
}

#[tokio::test]
async fn concurrent_session_mount_and_uninstall_publish_one_consistent_result() {
    use crate::plugin_library::PluginLibrary;
    use std::time::Duration;

    let _env_guard = PROCESS_ENV.lock().await;
    for mount_first in [true, false] {
        let (root, state) = fixture();

        write_plugin(&state, "demo", json!({"name":"Demo"}));
        let app = router(state.clone());
        create_session(app.clone(), "sess").await;
        let sessions = session_store(&state);
        let precondition = state
            .application
            .plugin_uninstall_precondition(root.path(), "demo", &sessions)
            .await
            .unwrap();
        assert!(precondition.refusal().is_none());
        let lease = PluginLibrary::new(root.path()).install_lease().unwrap();
        let before = library_bytes(&state);
        let mount = async move {
            call(
                app,
                "POST",
                "/sessions/sess/plugins/demo",
                Some(r#"{"on":true}"#),
            )
            .await
        };
        let uninstall = {
            let (application, library, sessions) = (
                state.application.clone(),
                root.path().to_path_buf(),
                sessions.clone(),
            );
            async move {
                application
                    .uninstall_plugin_if_unchanged(&library, &precondition, &sessions)
                    .await
            }
        };
        let (mounting, deleting) = if mount_first {
            let mounting = tokio::spawn(mount);
            wait_for_plugin_writer(&state.application).await;
            (mounting, tokio::spawn(uninstall))
        } else {
            let deleting = tokio::spawn(uninstall);
            wait_for_plugin_writer(&state.application).await;
            (tokio::spawn(mount), deleting)
        };
        assert!(!mounting.is_finished());
        assert!(!deleting.is_finished());
        drop(lease);
        let ((status, response), deletion) = tokio::time::timeout(Duration::from_secs(5), async {
            (mounting.await.unwrap(), deleting.await.unwrap())
        })
        .await
        .expect("mount/uninstall did not release their publication guards");
        let session = sessions.get("local", "sess").unwrap();
        let changed = sessions
            .events("local", "sess", 0)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "session.plugin_mount.changed")
            .count();
        if mount_first {
            assert_eq!(status, StatusCode::OK, "{response}");
            let error = deletion.unwrap_err();
            assert!(
                matches!(error, ApplicationError::Conflict(ref reason) if reason.contains("sess")),
                "{error:?}"
            );
            assert_eq!(session.plugin_mounts.get("demo"), Some(&true));
            assert_eq!(changed, 1);
            assert_eq!(library_bytes(&state), before);
        } else {
            deletion.unwrap();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
            assert!(
                response["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("尚未安装")),
                "{response}"
            );
            assert_eq!(
                session.plugin_mounts,
                BTreeMap::from([("network".into(), true)])
            );
            assert_eq!(changed, 0);
            assert!(!root.path().join("plugins/demo").exists());
        }
    }
}

#[tokio::test]
async fn a_session_lists_installed_plugins_from_the_frozen_run_not_the_graph_file() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    let _pilot_graph = EnvVar::set("ANCHOR_RUNNER_GRAPH_NAME", "pilot-graph");
    write_plugin(
        &state,
        "pinned",
        json!({"name":"Pinned","description":"always on"}),
    );
    write_plugin(&state, "optional", json!({"name":"Optional"}));
    write_plugin(
        &state,
        "extra",
        json!({"name":"Extra","mcpServers":{"remote":{"url":"https://mcp.example"}}}),
    );
    write_plugin(
        &state,
        "needsview",
        json!({"name":"Needs view","dependencies":{"python":["idna==3.7"]}}),
    );
    // Broken installed assets stay visible with their base health facts.
    let broken = state.catalog_root.join("plugins/broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("plugin.json"), "{ not json").unwrap();
    let app = router(state.clone());
    create_session(app.clone(), "sess").await;
    freeze_run(
        &state,
        "sess",
        "pilot-graph",
        &[("pinned", true, None), ("optional", false, Some(false))],
    );
    // The Graph *file* disagrees, and declares one Plugin the Run never saw. The
    // page must still answer from the admitted Run the mount filter reads.
    declare_plugins_in_the_file(
        &state,
        &[("pinned", false, Some(true)), ("fileonly", true, None)],
    );
    write_plugin(&state, "fileonly", json!({"name":"File only"}));
    // A stale enabled switch stays visible so the owner can revoke it.
    session_store(&state)
        .set_plugin_mount("local", "sess", "ghost", true, "test", "stale")
        .unwrap();

    let (status, listed) = call(app, "GET", "/sessions/sess/plugins", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let rows = listed["plugins"].as_array().unwrap();
    let ids = rows
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        [
            "broken",
            "extra",
            "fileonly",
            "ghost",
            "needsview",
            "network",
            "optional",
            "pinned"
        ]
    );
    let row = |id: &str| {
        rows.iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("no row for {id}"))
            .clone()
    };
    assert_eq!(row("pinned")["required"], true, "the frozen snapshot wins");
    assert_eq!(row("pinned")["on"], true);
    assert_eq!(row("pinned")["description"], "always on");
    assert_eq!(row("pinned")["mcpServerNames"], json!([]));
    assert_eq!(row("pinned")["available"], true);
    assert_eq!(row("pinned")["error"], Value::Null);
    assert_eq!(row("broken")["definitionValid"], false);
    assert_eq!(row("broken")["dependenciesReady"], Value::Null);
    assert_eq!(row("ghost")["on"], true);
    assert_eq!(row("optional")["required"], false);
    assert_eq!(
        row("optional")["on"],
        false,
        "the frozen declaration's default:false stays off"
    );
    // The file-only declaration is not part of the Run that was admitted.
    assert_eq!(row("fileonly")["required"], false);
    assert_eq!(row("fileonly")["on"], false);
    assert_eq!(row("extra")["mcpServerNames"], json!(["remote"]));
    assert_eq!(row("extra")["on"], false, "undeclared is off until clicked");
    assert!(
        row("extra").get("mcpServers").is_none(),
        "the Session row names servers, it does not carry the catalog object"
    );
    assert_eq!(row("needsview")["available"], false);
    assert!(
        row("needsview")["error"]
            .as_str()
            .is_some_and(|error| error.contains("view")),
        "{}",
        row("needsview")
    );
    assert!(root.path().join("plugins/needsview").is_dir());
}

#[tokio::test]
async fn networking_is_enabled_by_dependencies_without_writing_a_provider_switch_and_denial_wins() {
    let (_root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    write_plugin(&state, "network", json!({"name":"联网","network":true}));
    write_plugin(
        &state,
        "research",
        json!({"name":"学术调研","network":true}),
    );
    write_plugin(
        &state,
        "remote",
        json!({"name":"Remote","mcpServers":{"api":{"type":"http","url":"http://127.0.0.1:1/mcp"}}}),
    );
    let app = router(state.clone());
    create_session(app.clone(), "sess").await;
    // Isolate dependency-derived enablement from the new Pilot's active default.
    session_store(&state)
        .set_plugin_mount(
            "local",
            "sess",
            "network",
            false,
            "operator",
            "test dependency enablement",
        )
        .unwrap();
    let (status, result) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/research",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let provider = |result: &Value| {
        result["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "network")
            .unwrap()
            .clone()
    };
    assert_eq!(provider(&result)["on"], true);
    assert_eq!(provider(&result)["explicitOn"], false);
    assert_eq!(provider(&result)["enabledBy"], json!(["research"]));
    assert_eq!(provider(&result)["networkAllowed"], true);
    assert_eq!(
        session_store(&state)
            .get("local", "sess")
            .unwrap()
            .plugin_mounts
            .get("network"),
        Some(&false)
    );
    let (_, result) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/remote",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(
        provider(&result)["enabledBy"],
        json!(["remote", "research"])
    );
    let (_, result) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/research",
        Some(r#"{"on":false}"#),
    )
    .await;
    assert_eq!(provider(&result)["on"], true);
    let (_, result) = call(
        app.clone(),
        "POST",
        "/sessions/sess/network",
        Some(r#"{"allowed":false}"#),
    )
    .await;
    assert_eq!(provider(&result)["networkAllowed"], false);
    let remote = result["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "remote")
        .unwrap();
    assert_eq!(remote["available"], false);
    assert!(remote["error"].as_str().unwrap().contains("禁止联网"));
    // Permission survives a mount change; a dependency cannot turn it back on.
    let (_, result) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/research",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(provider(&result)["networkPermission"], false);
    let (_, result) = call(
        app.clone(),
        "POST",
        "/sessions/sess/network",
        Some(r#"{"allowed":true}"#),
    )
    .await;
    assert_eq!(provider(&result)["networkAllowed"], true);
    for id in ["remote", "research"] {
        let (_, result) = call(
            app.clone(),
            "POST",
            &format!("/sessions/sess/plugins/{id}"),
            Some(r#"{"on":false}"#),
        )
        .await;
        if id == "research" {
            assert_eq!(provider(&result)["on"], false);
        }
    }
    let (_, result) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/network",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(provider(&result)["explicitOn"], true);
    assert_eq!(provider(&result)["networkAllowed"], true);
    let (status, _) = call(
        app,
        "POST",
        "/sessions/sess/network",
        Some(r#"{"allowed":true,"grant":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn switching_an_uninstalled_plugin_is_refused_and_changes_no_bytes() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    write_plugin(&state, "demo", json!({"name":"Demo"}));
    std::fs::write(
        state.catalog_root.join("plugins/demo/notes.txt"),
        "plugin bytes",
    )
    .unwrap();
    // A view and shared-store file that already exist: refusals must leave them
    // byte-identical, not merely avoid creating new ones.
    for (path, bytes) in [
        (
            "dependencies/views/demo/aaaa/pyvenv.cfg",
            "home = /usr/bin\n",
        ),
        ("dependencies/store/idna/3.7/MARKER", "package bytes"),
    ] {
        let file = state.catalog_root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }
    let app = router(state.clone());
    create_session(app.clone(), "sess").await;

    let before = library_bytes(&state);
    assert!(!before.is_empty(), "the fixture must have bytes to compare");
    // The view and the shared store are inside the comparison, not beside it.
    assert!(
        before
            .iter()
            .any(|(path, _)| path.ends_with("views/demo/aaaa/pyvenv.cfg")),
        "{before:?}"
    );
    assert!(
        before
            .iter()
            .any(|(path, _)| path.ends_with("store/idna/3.7/MARKER")),
        "{before:?}"
    );
    let (status, refused) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/ghost",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert_eq!(
        refused["error"], "插件 `ghost` 尚未安装，不能挂载",
        "a missing Plugin has its own reason"
    );
    // A malformed id is refused as an id, not as a missing Plugin.
    let (status, refused) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/-bad",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert_eq!(refused["error"], "插件 id `-bad` 不合法");

    assert_eq!(library_bytes(&state), before);
    assert!(!root.path().join("plugins/ghost").exists());
    assert_eq!(
        session_store(&state)
            .get("local", "sess")
            .unwrap()
            .plugin_mounts,
        BTreeMap::from([("network".into(), true)])
    );
}

#[tokio::test]
async fn catalog_and_session_share_base_health_and_allow_revoking_broken_mounts() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    for (id, manifest) in [
        ("healthy", json!({"name":"Healthy"})),
        (
            "needsview",
            json!({"name":"Needs view","dependencies":{"python":["idna==3.7"]}}),
        ),
        ("broken", json!({"name":"Initially healthy"})),
    ] {
        write_plugin(&state, id, manifest);
    }
    let app = router(state.clone());
    create_session(app.clone(), "sess").await;
    let sessions = session_store(&state);
    for id in ["needsview", "broken", "missing"] {
        sessions
            .set_plugin_mount("local", "sess", id, true, "test", "prior mount")
            .unwrap();
    }
    std::fs::write(
        state.catalog_root.join("plugins/broken/plugin.json"),
        "{invalid",
    )
    .unwrap();
    let (_, catalog) = call(app.clone(), "GET", "/plugins", None).await;
    let (_, listed) = call(app.clone(), "GET", "/sessions/sess/plugins", None).await;
    for row in catalog["plugins"].as_array().unwrap() {
        let session_row = listed["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == row["id"])
            .unwrap();
        for field in [
            "definitionValid",
            "dependenciesReady",
            "available",
            "connectionVerified",
            "contextAuthorized",
        ] {
            assert_eq!(
                row[field], session_row[field],
                "{field}: {row} / {session_row}"
            );
        }
    }
    for id in ["needsview", "broken", "missing"] {
        let (status, response) = call(
            app.clone(),
            "POST",
            &format!("/sessions/sess/plugins/{id}"),
            Some(r#"{"on":false}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{id}: {response}");
        assert_eq!(response["plugin"]["on"], false);
        assert_eq!(
            sessions.get("local", "sess").unwrap().plugin_mounts.get(id),
            Some(&false)
        );
        let (status, _) = call(
            app.clone(),
            "POST",
            &format!("/sessions/sess/plugins/{id}"),
            Some(r#"{"on":true}"#),
        )
        .await;
        assert!(
            status.is_client_error(),
            "a broken mount must not be re-enabled: {id}"
        );
    }
}

#[tokio::test]
async fn a_successful_switch_writes_the_session_mount_and_its_audit_event() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    let _pilot_graph = EnvVar::set("ANCHOR_RUNNER_GRAPH_NAME", "pilot-graph");
    write_plugin(&state, "demo", json!({"name":"Demo"}));
    write_plugin(&state, "pinned", json!({"name":"Pinned"}));
    write_plugin(
        &state,
        "needsview",
        json!({"name":"Needs view","dependencies":{"python":["idna==3.7"]}}),
    );
    let app = router(state.clone());
    create_session(app.clone(), "sess").await;
    freeze_run(&state, "sess", "pilot-graph", &[("pinned", true, None)]);

    let (status, mounted) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/demo",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{mounted}");
    assert_eq!(mounted["plugin"]["id"], "demo");
    assert_eq!(mounted["plugin"]["on"], true);
    assert_eq!(mounted["plugin"]["required"], false);
    assert_eq!(mounted["plugin"]["available"], true);
    assert_eq!(mounted["plugin"]["error"], Value::Null);
    assert_eq!(mounted["effect"]["status"], "next_turn");
    assert_eq!(mounted["continuation"]["status"], "none");

    let store = session_store(&state);
    assert_eq!(
        store.get("local", "sess").unwrap().plugin_mounts,
        BTreeMap::from([("network".to_owned(), true), ("demo".to_owned(), true)])
    );
    let events = store.events("local", "sess", 0).unwrap();
    assert!(
        events.iter().any(|event| {
            event.kind == "session.plugin_mount.changed"
                && event.data["plugin"] == "demo"
                && event.data["on"] == true
        }),
        "{events:?}"
    );
    let (status, unchanged) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/demo",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unchanged}");
    assert_eq!(unchanged["effect"]["status"], "unchanged");
    assert_eq!(unchanged["continuation"]["status"], "none");
    assert!(store.list_turns("local", "sess").unwrap().is_empty());
    assert_eq!(
        store
            .events("local", "sess", 0)
            .unwrap()
            .iter()
            .filter(|event| event.kind == "session.plugin_mount.changed")
            .count(),
        1
    );

    let (status, listed) = call(app.clone(), "GET", "/sessions/sess/plugins", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listed["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == "demo" && row["on"] == true),
        "{listed}"
    );

    // A Plugin the admitted Run froze as required cannot be switched off.
    let (status, refused) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/pinned",
        Some(r#"{"on":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(
        refused["error"],
        "该插件在图里被声明为必需（required: true）"
    );
    // A Plugin whose own declaration has no usable view cannot be mounted: that
    // would be building a view, which mounting never does.
    let (status, refused) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/needsview",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .is_some_and(|error| error.contains("view")),
        "{refused}"
    );
    // Neither refusal moved the switch or appended an event.
    assert_eq!(
        store.get("local", "sess").unwrap().plugin_mounts,
        BTreeMap::from([("network".to_owned(), true), ("demo".to_owned(), true)])
    );
    assert_eq!(
        store.events("local", "sess", 0).unwrap().len(),
        events.len()
    );

    let (status, off) = call(
        app.clone(),
        "POST",
        "/sessions/sess/plugins/demo",
        Some(r#"{"on":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{off}");
    assert_eq!(off["plugin"]["on"], false);
    assert_eq!(
        store.get("local", "sess").unwrap().plugin_mounts,
        BTreeMap::from([("network".to_owned(), true), ("demo".to_owned(), false)])
    );

    // An archived Session is a closed border.
    let (status, _) = call(
        app.clone(),
        "POST",
        "/sessions/sess/status",
        Some(r#"{"status":"archived","reason":"done"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, refused) = call(
        app,
        "POST",
        "/sessions/sess/plugins/demo",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(refused["error"].is_string(), "{refused}");
}

#[tokio::test]
async fn a_plugin_switch_on_a_terminal_turn_saves_policy_without_reviving_the_task() {
    use anchor_platform_session::TurnStatus;
    let _env_guard = PROCESS_ENV.lock().await;
    for terminal in [
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Interrupted,
        TurnStatus::Stopped,
    ] {
        let (_root, state) = fixture();

        write_plugin(&state, "demo", json!({"name":"Demo"}));
        let app = router(state.clone());
        create_session(app.clone(), "sess").await;
        let store = session_store(&state);
        let (source, _) = store
            .create_turn("local", "sess", "source", Some("previous task"))
            .unwrap();
        store
            .finish_turn("local", "sess", &source.id, terminal, None)
            .unwrap();
        let (status, mounted) = call(
            app,
            "POST",
            "/sessions/sess/plugins/demo",
            Some(r#"{"on":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{mounted}");
        assert_eq!(mounted["effect"]["status"], "next_turn");
        assert_eq!(mounted["continuation"]["status"], "none");
        assert_eq!(store.list_turns("local", "sess").unwrap().len(), 1);
        assert_eq!(
            store
                .get("local", "sess")
                .unwrap()
                .plugin_mounts
                .get("demo"),
            Some(&true)
        );
        assert!(
            store
                .events("local", "sess", 0)
                .unwrap()
                .iter()
                .all(|event| event.kind != "pilot.continuation.requested")
        );
    }
}

#[tokio::test]
async fn stopping_a_scheduled_completed_turn_preserves_history_and_cancels_once() {
    use anchor_platform_session::TurnStatus;
    let (_root, state) = fixture();
    let app = router(state.clone());
    create_session(app.clone(), "sess").await;
    let store = session_store(&state);
    let (source, _) = store
        .create_turn("local", "sess", "source", Some("previous task"))
        .unwrap();
    store
        .finish_turn("local", "sess", &source.id, TurnStatus::Completed, None)
        .unwrap();
    store
        .append_event(
            "local",
            "sess",
            "pilot.continuation.requested",
            BTreeMap::from([
                ("turn".into(), json!(source.id)),
                (
                    "request_id".into(),
                    json!(format!("continuation:{}", source.id)),
                ),
            ]),
        )
        .unwrap();
    for stopping in [true, false] {
        let (status, stopped) = call(app.clone(), "POST", "/sessions/sess/stop", None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{stopped}");
        assert_eq!(stopped["stopping"], stopping);
    }
    let (status, detail) = call(
        app,
        "GET",
        &format!("/sessions/sess/turns/{}", source.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["turn"]["status"], "completed");
    assert_eq!(detail["continuation"]["status"], "cancelled");
    assert_eq!(store.list_turns("local", "sess").unwrap().len(), 1);
    assert_eq!(
        store
            .events("local", "sess", 0)
            .unwrap()
            .iter()
            .filter(|event| event.kind == "pilot.continuation.cancelled")
            .count(),
        1
    );
}

#[tokio::test]
async fn only_the_configured_pilot_graph_may_declare_a_plugin_required() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    let _pilot_graph = EnvVar::set("ANCHOR_RUNNER_GRAPH_NAME", "pilot-graph");
    write_plugin(&state, "trusted", json!({"name":"Trusted"}));
    write_plugin(&state, "made-here", json!({"name":"Made here"}));
    let app = router(state.clone());
    create_session(app.clone(), "pilot-sess").await;
    create_session(app.clone(), "own-sess").await;
    // `required: true` from the deployment's own pilot Graph is an authorization.
    freeze_run(
        &state,
        "pilot-sess",
        "pilot-graph",
        &[("trusted", true, None)],
    );
    // A Graph the assistant created in this Session is not that Graph: its
    // `required` is downgraded to optional, exactly as the mount filter does, so
    // the human keeps the switch.
    freeze_run(
        &state,
        "own-sess",
        "graph-the-assistant-made",
        &[("made-here", true, None)],
    );

    let (status, listed) = call(app.clone(), "GET", "/sessions/pilot-sess/plugins", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let row = |listed: &Value, id: &str| {
        listed["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("no row for {id}"))
            .clone()
    };
    assert_eq!(row(&listed, "trusted")["required"], true);
    let (status, refused) = call(
        app.clone(),
        "POST",
        "/sessions/pilot-sess/plugins/trusted",
        Some(r#"{"on":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");

    let (status, listed) = call(app.clone(), "GET", "/sessions/own-sess/plugins", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(
        row(&listed, "made-here")["required"],
        false,
        "a Graph the assistant made may not lock a switch"
    );
    assert_eq!(
        row(&listed, "made-here")["on"],
        true,
        "downgraded to optional still mounts by default"
    );
    let (status, switched) = call(
        app,
        "POST",
        "/sessions/own-sess/plugins/made-here",
        Some(r#"{"on":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    assert_eq!(switched["plugin"]["required"], false);
    assert_eq!(switched["plugin"]["on"], false);
}

#[tokio::test]
async fn a_run_the_assistant_added_cannot_unlock_a_plugin_the_pilot_graph_requires() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    let _pilot_graph = EnvVar::set("ANCHOR_RUNNER_GRAPH_NAME", "pilot-graph");
    write_plugin(&state, "pinned", json!({"name":"Pinned"}));
    let app = router(state.clone());
    create_session(app.clone(), "sess").await;
    // The operator's pilot Run requires `pinned`.
    freeze_run(&state, "sess", "pilot-graph", &[("pinned", true, None)]);
    // The assistant then admits a Run of a Graph it made itself, which does not
    // require it. Reading only the newest Run would hide the lock and offer a
    // switch whose click would write an authorization the filter ignores.
    freeze_run(
        &state,
        "sess",
        "graph-the-assistant-made",
        &[("other", false, None)],
    );

    let (status, listed) = call(app.clone(), "GET", "/sessions/sess/plugins", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let row = listed["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "pinned")
        .unwrap()
        .clone();
    assert_eq!(
        row["required"], true,
        "any attached Run that requires it keeps the lock"
    );
    assert_eq!(row["on"], true);
    let (status, refused) = call(
        app,
        "POST",
        "/sessions/sess/plugins/pinned",
        Some(r#"{"on":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    // And no fake human authorization was recorded.
    assert!(
        session_store(&state)
            .events("local", "sess", 0)
            .unwrap()
            .iter()
            .all(|event| event.kind != "session.plugin_mount.changed"),
        "a refused switch must not write an authorization event"
    );
}

#[tokio::test]
async fn a_pilot_graph_named_by_the_bundle_root_keeps_its_lock() {
    let (_root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    // The deployment named no Graph explicitly: the configured bundle's own
    // directory name is the pilot Graph, and that is still the operator's lock.
    let _unnamed = EnvVar::unset("ANCHOR_RUNNER_GRAPH_NAME");
    let _bundle = EnvVar::set("ANCHOR_RUNNER_BUNDLE_ROOT", &state.bundle_root);
    write_plugin(&state, "pinned", json!({"name":"Pinned"}));
    let app = router(state.clone());
    create_session(app.clone(), "sess").await;
    let named = state
        .bundle_root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    freeze_run(&state, "sess", &named, &[("pinned", true, None)]);

    let (status, listed) = call(app.clone(), "GET", "/sessions/sess/plugins", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let row = listed["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "pinned")
        .unwrap()
        .clone();
    assert_eq!(
        row["required"], true,
        "the bundle's own name is the pilot Graph when no name is configured"
    );
    let (status, refused) = call(
        app,
        "POST",
        "/sessions/sess/plugins/pinned",
        Some(r#"{"on":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
}

/// The page's own URL, through the router `serve()` actually serves.
///
/// A handler test cannot catch this class of failure: the route has to exist in
/// the assembled router, behind the authentication layer, with the same
/// root-relative, unprefixed path the web app fetches (`api()` adds no prefix, and
/// the page and the API share one origin).
#[cfg(test)]
mod served_router_tests {
    use super::*;

    /// One request with a Bearer key, exactly as the page's `api()` sends it.
    async fn authorized(
        app: Router,
        method: &str,
        uri: &str,
        key: &str,
        body: Option<&str>,
    ) -> (StatusCode, Value) {
        authorized_call(
            app,
            method,
            uri,
            key,
            body.map(|body| serde_json::from_str::<Value>(body).unwrap()),
        )
        .await
    }

    #[tokio::test]
    async fn the_session_page_url_reaches_the_plugin_route_through_the_served_router() {
        let _env_guard = PROCESS_ENV.lock().await;
        let (root, state) = fixture();

        let key = "k".repeat(32);
        let app = router_with_readiness_and_web_root(
            state.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            root.path().join("web"),
        );
        // The Session id from the report.
        let session = "7d18d00b-0325-4e74-9707-5ffcce71cc25";
        let (status, created) = authorized(
            app.clone(),
            "POST",
            "/sessions",
            &key,
            Some(&json!({"id": session}).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let plugin = state.catalog_root.join("plugins/demo");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            json!({"name":"Demo"}).to_string(),
        )
        .unwrap();

        // The exact URL `SessionPlugins.tsx` builds, and its already-shipped
        // sibling for contrast: same router, same layer, same shape.
        let (status, listed) = authorized(
            app.clone(),
            "GET",
            &format!("/sessions/{session}/plugins"),
            &key,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        assert!(listed["plugins"].is_array(), "{listed}");
        let (status, events) = authorized(
            app.clone(),
            "GET",
            &format!("/sessions/{session}/events"),
            &key,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{events}");
        let (status, switched) = authorized(
            app.clone(),
            "POST",
            &format!("/sessions/{session}/plugins/demo"),
            &key,
            Some(r#"{"on":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{switched}");
        assert_eq!(switched["plugin"]["on"], true);

        // The two 404s are not the same 404, and the page shows which one it got: a
        // path the router does not know falls through to the static fallback, which
        // answers with no JSON body at all (the page shows `GET … → 404`), while a
        // Session the caller does not have answers a named reason.
        let (status, unknown) = authorized(
            app.clone(),
            "GET",
            &format!("/sessions/{session}/plugins-typo"),
            &key,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
        assert!(
            unknown.is_null(),
            "the fallback has no error body: {unknown}"
        );
        let (status, missing) = authorized(
            app.clone(),
            "GET",
            "/sessions/00000000-0000-0000-0000-000000000000/plugins",
            &key,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
        assert_eq!(missing["error"], "no such session");
    }
}
