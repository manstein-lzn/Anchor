//! The Pilot tool surface's regression suite.
//!
//! It lives beside the tools it exercises; the table itself stays the single
//! source of authority, which one of these cases asserts directly.

use super::*;

use crate::application::run_access::fixtures;
use crate::plugin_library::Catalog;
use anchor_platform_session::CreateSession;

#[tokio::test]
async fn plugin_management_metadata_and_reads_share_the_library_publication_guard() {
    let (root, tools) = fixture();
    let plugin = root.path().join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills/check")).unwrap();
    std::fs::write(plugin.join("plugin.json"), r#"{"name":"Demo"}"#).unwrap();
    std::fs::write(
        plugin.join("skills/check/SKILL.md"),
        "# Public Skill\nPurpose",
    )
    .unwrap();
    let lease = crate::plugin_library::PluginLibrary::new(root.path())
        .try_install_lease()
        .unwrap();
    for (name, args) in [
        ("plugin_list", json!({})),
        ("plugin_read", json!({"plugin":"demo","section":"skills"})),
    ] {
        assert!(
            tools
                .execute(name, args)
                .await
                .unwrap_err()
                .to_string()
                .contains("Library is changing")
        );
    }
    drop(lease);
    assert_eq!(
        tools.execute("plugin_list", json!({})).await.unwrap()["total"],
        1
    );
    assert_eq!(
        tools
            .execute("plugin_read", json!({"plugin":"demo","section":"skills"}))
            .await
            .unwrap()["skills"][0]["name"],
        "Public Skill"
    );
}

fn fixture() -> (tempfile::TempDir, PilotTools) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("fixture")).unwrap();
    std::fs::write(root.path().join("fixture/graph.json"), r#"{"entry":"idle","ops":{"idle":{"run":"true"}},"nodes":[{"id":"idle","op":"idle"}],"edges":[]}"#).unwrap();
    std::fs::write(
        root.path().join("fixture/manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let sessions = SessionStore::open(root.path().join("sessions.sqlite")).unwrap();
    sessions
        .create(
            "local",
            CreateSession {
                id: Some("pilot".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let tools = PilotTools {
        application: RunApplication::new(root.path().join("state"), root.path().to_path_buf()),
        graph_name: "fixture".into(),
        library_root: root.path().to_path_buf(),
        data_root: root.path().join("state"),
        workspace_root: root.path().join("work"),
        sessions,
        owner: "local".into(),
        session: "pilot".into(),
        turn: String::new(),
        context: None,
    };
    (root, tools)
}

#[tokio::test]
async fn run_pause_requests_the_node_boundary_without_cancelling_the_worker() {
    let (_root, mut tools) = fixture();
    let run = "pilot-pause";
    fixtures::create(&tools.application, &fixtures::work_record(run));
    let drive = fixtures::gated_drive(&tools.application, run).await;
    assert!(
        tools
            .execute("run_pause", json!({"run":run}))
            .await
            .is_err()
    );
    assert!(!drive.cancelled());

    let (turn, _) = tools
        .sessions
        .create_turn("local", "pilot", "pause-run", None)
        .unwrap();
    tools = tools.for_turn(&turn.id);
    let receipt = tools
        .execute("run_pause", json!({"run":run}))
        .await
        .unwrap();
    assert_eq!(
        receipt,
        json!({"run":run,"operation":"pause","requested":true})
    );
    assert!(matches!(
        fixtures::view(&tools.application, run).phase(),
        crate::application::run_access::RunPhase::Executing
    ));
    assert!(!drive.cancelled());
    assert!(tools.application.run_is_active(run).await);

    tools
        .sessions
        .finish_turn(
            "local",
            "pilot",
            &turn.id,
            anchor_platform_session::TurnStatus::Completed,
            None,
        )
        .unwrap();
    assert!(tools.execute("run_stop", json!({"run":run})).await.is_err());
    assert!(!drive.cancelled());
    drive.drain(&tools.application, run).await;
}

#[tokio::test]
async fn run_stop_cancels_the_worker_without_requesting_a_boundary_pause() {
    let (_root, mut tools) = fixture();
    let run = "pilot-stop";
    fixtures::create(&tools.application, &fixtures::work_record(run));
    let drive = fixtures::gated_drive(&tools.application, run).await;
    assert!(tools.execute("run_stop", json!({"run":run})).await.is_err());
    assert!(!drive.cancelled());

    let (turn, _) = tools
        .sessions
        .create_turn("local", "pilot", "stop-run", None)
        .unwrap();
    tools = tools.for_turn(&turn.id);
    let receipt = tools.execute("run_stop", json!({"run":run})).await.unwrap();
    assert_eq!(
        receipt,
        json!({"run":run,"operation":"stop","requested":true})
    );
    assert!(drive.cancelled());
    assert!(fixtures::view(&tools.application, run).has_stopped_execution());
    assert!(tools.application.run_is_active(run).await);
    drive.drain(&tools.application, run).await;
}

#[tokio::test]
async fn platform_status_is_advertised_as_a_read_only_pilot_tool_and_fails_closed() {
    let (_root, tools) = fixture();
    let definition = tools
        .definitions()
        .into_iter()
        .find(|definition| definition.name == "platform_status")
        .expect("platform status is part of the common Pilot tool table");
    assert_eq!(
        definition.parameters,
        json!({
            "type":"object",
            "properties":{},
            "additionalProperties":false
        })
    );
    assert!(tools.is_read_only("platform_status"));

    let status = tools.execute("platform_status", json!({})).await.unwrap();
    assert_eq!(status["python"]["shared_interpreter_configured"], false);
    assert_eq!(status["python"]["shared_interpreter_available"], false);
    assert_eq!(status["python"]["authoring_granted"], false);
    assert_eq!(
        status["python"]["dependency_preparation"]["configured"],
        false
    );
    assert_eq!(status["rust"]["configured"], false);
    assert_eq!(status["rust"]["installed_toolchains"], json!([]));
    assert_eq!(status["network"]["allowed_this_turn"], false);
    assert_eq!(status["mutations_performed"], false);
}

#[tokio::test]
async fn assistant_reviews_a_workspace_candidate_without_installing_it() {
    let (root, tools) = fixture();
    let candidate = tools.task_workspace().join("candidate");
    std::fs::create_dir_all(candidate.join("skills/check")).unwrap();
    std::fs::write(
            candidate.join("plugin.json"),
            r#"{"name":"Candidate","description":"from the workspace","skills":"skills","mcpServers":{"probe":{"command":"bin/run","args":["serve"],"env_vars":["CANDIDATE_TOKEN"]}}}"#,
        )
        .unwrap();
    std::fs::write(candidate.join("skills/check/SKILL.md"), "# check\n").unwrap();

    let review = tools
        .execute("plugin_review", json!({"directory":"candidate"}))
        .await
        .unwrap();
    assert_eq!(review["id"], "candidate");
    assert_eq!(review["name"], "Candidate");
    assert_eq!(
        review["resources"],
        json!(["plugin.json", "skills/check/SKILL.md"])
    );
    assert_eq!(review["servers"][0]["name"], "probe");
    assert_eq!(
        review["servers"][0]["environment"],
        json!(["CANDIDATE_TOKEN"])
    );
    assert!(
        !root.path().join("plugins").exists(),
        "a review must not publish anything"
    );
}

#[tokio::test]
async fn assistant_review_refuses_paths_outside_its_workspace() {
    let (root, tools) = fixture();
    let workspace = tools.task_workspace();
    std::fs::create_dir_all(workspace.join("candidate")).unwrap();
    std::fs::write(
        workspace.join("candidate/plugin.json"),
        r#"{"name":"Candidate"}"#,
    )
    .unwrap();
    std::fs::write(workspace.join("plain.txt"), "not a directory").unwrap();
    std::os::unix::fs::symlink(root.path().join("fixture"), workspace.join("escape")).unwrap();

    for directory in [
        "/etc",
        "../fixture",
        "candidate/../candidate",
        "escape",
        "plain.txt",
        "missing",
        "",
    ] {
        let error = tools
            .execute("plugin_review", json!({"directory":directory}))
            .await
            .unwrap_err();
        assert!(
            matches!(error, ToolError::Failed(_)),
            "{directory:?} was accepted"
        );
    }

    // The same candidate is accepted once addressed inside the workspace.
    assert!(
        tools
            .execute("plugin_review", json!({"directory":"candidate"}))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn assistant_refuses_to_rewrite_or_delete_its_own_graph() {
    let (_root, tools) = fixture();
    let definition = json!({"entry":"idle","ops":{"idle":{"run":"true"}},"nodes":[{"id":"idle","op":"idle"}],"edges":[]});

    for error in [
        tools
            .execute(
                "graph_update",
                json!({"graph":"fixture","definition":definition}),
            )
            .await
            .unwrap_err(),
        tools
            .execute(
                "graph_create",
                json!({"name":"fixture","definition":definition}),
            )
            .await
            .unwrap_err(),
    ] {
        match error {
            ToolError::Failed(reason) => {
                assert!(reason.contains("defines this assistant"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    // Reading the assistant's own Graph stays available, and another Graph
    // is not refused by this guard.
    assert!(
        tools
            .execute("graph_read", json!({"graph":"fixture"}))
            .await
            .is_ok()
    );
    let other = tools
        .execute(
            "graph_update",
            json!({"graph":"other","definition":definition}),
        )
        .await
        .unwrap_err();
    match other {
        ToolError::Failed(reason) => assert!(!reason.contains("defines this assistant")),
        other => panic!("expected a Turn-state failure, got {other:?}"),
    }
}

#[tokio::test]
async fn read_tools_do_not_create_runs_or_modify_definitions() {
    let (root, tools) = fixture();
    let original = std::fs::read(root.path().join("fixture/graph.json")).unwrap();
    for name in [
        "platform_status",
        "graph_read",
        "plugin_list",
        "run_list",
        "schedule_list",
    ] {
        assert!(tools.is_read_only(name), "{name} must stay read-only");
    }
    for name in [
        "runtime_prepare",
        "development_prepare",
        "credential_request",
        "schedule_create",
        "schedule_delete",
    ] {
        assert!(!tools.is_read_only(name), "{name} changes Host facts");
    }
    assert!(!tools.is_read_only("graph_run"));
    assert_eq!(
        tools
            .execute("graph_read", json!({"graph":"fixture"}))
            .await
            .unwrap()["definition"]["entry"],
        "idle"
    );
    assert_eq!(
        tools
            .execute(
                "graph_validate",
                json!({"definition":serde_json::from_slice::<Value>(&original).unwrap()})
            )
            .await
            .unwrap()["valid"],
        true
    );
    assert_eq!(
        tools.execute("plugin_list", json!({})).await.unwrap()["plugins"],
        json!([])
    );
    assert_eq!(
        tools.execute("run_list", json!({})).await.unwrap(),
        json!({"runs":[]})
    );
    assert_eq!(
        tools.execute("session_wait", json!({})).await.unwrap()["session"]["id"],
        "pilot"
    );
    assert!(
        tools
            .execute("graph_run", json!({"graph":"fixture"}))
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(root.path().join("fixture/graph.json")).unwrap(),
        original
    );
    assert!(!root.path().join("state").exists());
}

#[tokio::test]
async fn graph_authoring_tools_share_a_usable_example_on_an_empty_catalog() {
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, mut tools) = fixture();
    let catalog = root.path().join("empty-catalog");
    std::fs::create_dir(&catalog).unwrap();
    tools.application = RunApplication::new(root.path().join("state"), catalog.clone());
    tools.graph_name.clear();
    assert_eq!(
        tools.execute("graph_list", json!({})).await.unwrap(),
        json!({"graphs":[]})
    );
    let definitions = tools.definitions();
    let parameter = &definitions
        .iter()
        .find(|tool| tool.name == "graph_validate")
        .unwrap()
        .parameters["properties"]["definition"];
    for name in ["graph_create", "graph_update"] {
        assert_eq!(
            &definitions
                .iter()
                .find(|tool| tool.name == name)
                .unwrap()
                .parameters["properties"]["definition"],
            parameter
        );
    }
    let definition = parameter["examples"][0].clone();
    let description = parameter["description"].as_str().unwrap();
    assert!(description.contains(&definition.to_string()));
    for fact in [
        crate::model_configuration::MODEL_REFERENCES,
        "final_result",
        "files",
        "own stable /workspace",
        "/in/<source-node>",
        "read-only",
        "writes",
        "reads",
    ] {
        assert!(
            description.contains(fact),
            "missing authoring contract: {fact}"
        );
    }
    let snapshot = GraphSnapshot::from_authoring(definition.clone()).unwrap();
    assert_eq!(snapshot.ops["hello"]["writes"], json!(["hello.txt"]));
    assert_eq!(
        tools
            .execute("graph_validate", json!({"definition":definition}))
            .await
            .unwrap(),
        json!({"valid":true,"entry":"hello","nodes":["hello"]})
    );
    let (turn, _) = tools
        .sessions
        .create_turn("local", "pilot", "author", Some("create a Graph"))
        .unwrap();
    let tools = tools.for_turn(&turn.id);
    tools
        .execute(
            "graph_create",
            json!({"name":"hello","definition":definition}),
        )
        .await
        .unwrap();
    let mut updated = definition;
    updated["objective"] = json!("updated without running");
    tools
        .execute(
            "graph_update",
            json!({"graph":"hello","definition":updated}),
        )
        .await
        .unwrap();
    assert_eq!(
        tools
            .execute("graph_read", json!({"graph":"hello"}))
            .await
            .unwrap()["definition"],
        GraphSnapshot::normalize_network_authoring(updated, false)
    );
    assert!(tools.application.records().unwrap().is_empty());
}

#[tokio::test]
async fn graph_validate_preserves_kernel_authoring_diagnostics() {
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (_root, tools) = fixture();
    let example = graphs::definition_parameter()["examples"][0].clone();
    let mut argv = example.clone();
    argv["ops"]["hello"]["run"] = json!(["printf", "hello"]);
    let mut inline_op = example.clone();
    inline_op["nodes"][0]["op"] = json!({"run":"true"});
    let mut misplaced_writes = example.clone();
    misplaced_writes["nodes"][0]["writes"] = json!(["hello.txt"]);
    let mut unknown_op = example;
    unknown_op["nodes"][0]["op"] = json!("missing");
    for (definition, diagnostic) in [
        (argv, "op \"hello\".run must be a non-empty command string"),
        (inline_op, ".op must be a non-empty string"),
        (misplaced_writes, "unknown field `writes`"),
        (unknown_op, "references undeclared op \"missing\""),
    ] {
        let expected = GraphSnapshot::from_authoring(definition.clone())
            .unwrap_err()
            .to_string();
        assert!(expected.contains(diagnostic), "{expected}");
        let report = tools
            .execute("graph_validate", json!({"definition":definition}))
            .await
            .unwrap();
        assert_eq!(report, json!({"valid":false,"error":expected}));
    }
    let error = tools
        .execute("graph_validate", json!({"definition":[]}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("definition must be an object"));
}

#[tokio::test]
async fn graph_validate_hides_private_plugin_paths_and_host_configuration_failures() {
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, tools) = fixture();
    let plugin = root.path().join("plugins/private");
    std::fs::create_dir_all(&plugin).unwrap();
    let private = root.path().join("host-private-config.json");
    std::fs::write(&private, r#"{"name":"Private","secret":"private-key"}"#).unwrap();
    std::os::unix::fs::symlink(&private, plugin.join("plugin.json")).unwrap();
    let raw = Catalog::new(root.path())
        .resolve(&["private".into()])
        .unwrap_err()
        .to_string();
    assert!(raw.contains(root.path().to_str().unwrap()));
    let definition = json!({
        "entry":"worker",
        "agents":{"worker":{"model":"fixture"}},
        "nodes":[{"id":"worker","agent":"worker","plugins":["private"]}],
        "edges":[]
    });
    GraphSnapshot::from_authoring(definition.clone()).unwrap();
    let plugin_report = tools
        .execute("graph_validate", json!({"definition":definition}))
        .await
        .unwrap();
    assert_eq!(
        plugin_report,
        json!({"valid":false,"error":"invalid Graph or unavailable Plugin"})
    );
    assert!(
        !plugin_report
            .to_string()
            .contains(root.path().to_str().unwrap())
    );
    assert!(!plugin_report.to_string().contains("private-key"));
    let mut unsupported = definition;
    unsupported["nodes"][0]["plugins"] = json!([]);
    unsupported["agents"]["worker"]["max_steps"] = json!(1);
    let snapshot = GraphSnapshot::from_authoring(unsupported.clone()).unwrap();
    assert!(crate::reject_snapshot(&snapshot).is_err());
    assert_eq!(
        tools
            .execute("graph_validate", json!({"definition":unsupported}))
            .await
            .unwrap(),
        plugin_report
    );
}

#[tokio::test]
async fn graph_mutations_require_an_owned_running_turn_and_do_not_start_runs() {
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, tools) = fixture();
    let definition = json!({"entry":"idle","ops":{"idle":{"run":"true"}},
            "nodes":[{"id":"idle","op":"idle"}],"edges":[]});
    assert!(
        tools
            .execute(
                "graph_create",
                json!({"name":"new","definition":definition})
            )
            .await
            .is_err()
    );
    let (turn, _) = tools
        .sessions
        .create_turn("local", "pilot", "write-fixture", Some("create a Graph"))
        .unwrap();
    let mut tools = tools.for_turn(&turn.id);
    tools.owner = "responses-other".into();
    assert!(
        tools
            .execute(
                "graph_create",
                json!({"name":"new","definition":definition})
            )
            .await
            .is_err()
    );
    assert!(!root.path().join("new").exists());
    tools.owner = "local".into();
    let created = tools
        .execute(
            "graph_create",
            json!({"name":"new","definition":definition}),
        )
        .await
        .unwrap();
    assert_eq!(created["graph"], "new");
    assert!(tools.application.records().unwrap().is_empty());
    let mut updated = definition;
    updated["objective"] = json!("updated only, not executed");
    tools
        .execute("graph_update", json!({"graph":"new","definition":updated}))
        .await
        .unwrap();
    assert_eq!(
        tools
            .execute("graph_read", json!({"graph":"new"}))
            .await
            .unwrap()["definition"],
        GraphSnapshot::normalize_network_authoring(updated.clone(), false)
    );
    assert!(tools.application.records().unwrap().is_empty());
    assert!(
        tools
            .execute("graph_delete", json!({"graph":"new"}))
            .await
            .is_err()
    );
    tools
        .sessions
        .finish_turn(
            "local",
            "pilot",
            &turn.id,
            anchor_platform_session::TurnStatus::Completed,
            None,
        )
        .unwrap();
    assert!(
        tools
            .execute("graph_update", json!({"graph":"new","definition":updated}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn graph_paths_symlinks_and_plugin_credentials_fail_closed() {
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, tools) = fixture();
    assert!(
        tools
            .execute("graph_read", json!({"graph":"../secret"}))
            .await
            .is_err()
    );
    std::fs::write(root.path().join("secret.json"), "host-secret").unwrap();
    std::fs::remove_file(root.path().join("fixture/graph.json")).unwrap();
    std::os::unix::fs::symlink(
        root.path().join("secret.json"),
        root.path().join("fixture/graph.json"),
    )
    .unwrap();
    assert!(
        tools
            .execute("graph_read", json!({"graph":"fixture"}))
            .await
            .is_err()
    );
    let plugin = root.path().join("plugins/demo");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.json"), r#"{"name":"Demo","mcpServers":{"remote":{"url":"https://private.invalid/?key=do-not-expose","headers":{"Authorization":"private-key"}}}}"#).unwrap();
    let public = tools
        .execute("plugin_list", json!({}))
        .await
        .unwrap()
        .to_string();
    assert!(
        !public.contains("private-key")
            && !public.contains("private.invalid")
            && !public.contains("do-not-expose"),
        "{public}"
    );
}

/// The retired environment tools are gone, not merely disabled: an unknown name
/// is refused by the dispatcher and no definition advertises them.
#[tokio::test]
async fn the_retired_environment_tools_no_longer_exist() {
    let (_root, tools) = fixture();
    for name in [
        "environment_list",
        "environment_request",
        "environment_references",
        "environment_show",
        "environment_publish",
        "dependency_provision",
    ] {
        let error = tools.execute(name, json!({})).await.unwrap_err();
        assert!(
            error.to_string().contains("is not registered")
                || error.to_string().contains("unknown tool"),
            "`{name}` must not be a tool any more, got {error}"
        );
    }
}
