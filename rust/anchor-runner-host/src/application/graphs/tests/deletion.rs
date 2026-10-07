use super::*;

fn save_run(application: &RunApplication, name: &str, run_id: &str, status: RunStatus) {
    let (path, bundle) = application.load_graph(name).unwrap();
    let mut record = GraphRunRecord::create_with_id(bundle.snapshot, json!({}), run_id).unwrap();
    record.status = status;
    record.plugin_bindings_initialized = true;
    FileRunStore::new(application.data_root.join("runs"))
        .save(&record)
        .unwrap();
    metadata::save(
        &application.data_root,
        &RunMetadata::new(
            run_id.into(),
            name.into(),
            record.graph_digest.clone(),
            &path,
        )
        .unwrap(),
    )
    .unwrap();
}

fn plugin_bundle(application: &RunApplication) -> PathBuf {
    let path = application.graph_path_checked("fixture").unwrap();
    let plugin = path.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills")).unwrap();
    std::fs::write(plugin.join("plugin.json"), r#"{"name":"Demo"}"#).unwrap();
    std::fs::write(plugin.join("skills/SKILL.md"), "safe resource").unwrap();
    let mut authoring = definition("fixture");
    authoring["agents"] = json!({"worker": {"model": "fixture", "instructions": "work"}});
    authoring["ops"] = json!({});
    authoring["nodes"] = json!([{"id": "work", "agent": "worker", "plugins": ["demo"]}]);
    std::fs::write(path.join("graph.json"), authoring.to_string()).unwrap();
    seal_plugin_bundle(&path);
    application.load_graph("fixture").unwrap();
    path
}

fn seal_plugin_bundle(path: &Path) {
    let binding = FilePluginCatalog::new(path)
        .resolve(&["demo".into()])
        .unwrap()
        .remove(0);
    std::fs::write(
        path.join("manifest.json"),
        json!({
            "format": 1,
            "graph": "graph.json",
            "plugins": [{
                "id": binding.id,
                "digest": binding.digest,
                "resources": binding.resources,
                "mcp_servers": binding.mcp_servers
            }]
        })
        .to_string(),
    )
    .unwrap();
}

async fn assert_conflict_unchanged(
    application: &RunApplication,
    name: &str,
    workspace_root: &Path,
    expected: &str,
) {
    let path = application.graph_path_checked(name).unwrap();
    let before = files(&path);
    assert!(matches!(
        application
            .remove_graph_if_unchanged(name, workspace_root, expected)
            .await,
        Err(GraphManagementError::Conflict(_))
    ));
    assert_eq!(files(&path), before);
}

#[tokio::test]
async fn unchanged_snapshot_survives_reopen_and_reuses_terminal_run_cleanup() {
    let (root, application) = fixture();
    application.create_graph("other", None).await.unwrap();
    save_run(&application, "fixture", "target-run", RunStatus::Completed);
    save_run(&application, "other", "other-run", RunStatus::Completed);
    let workspace_root = root.path().join("workspaces");
    let target_workspace = workspace_root.join("target-run");
    std::fs::create_dir_all(&target_workspace).unwrap();
    std::fs::write(target_workspace.join("result.txt"), "result").unwrap();
    let target_path = application.graph_path_checked("fixture").unwrap();
    let before = files(&target_path);
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    assert_eq!(files(&target_path), before);
    assert_eq!(
        application
            .graph_delete_precondition("fixture")
            .await
            .unwrap(),
        expected
    );
    let reopened = RunApplication::new(
        application.data_root.clone(),
        application.catalog_root.clone(),
    )
    .with_configured_graph("fixture".into(), target_path.clone());
    assert_eq!(
        reopened.graph_delete_precondition("fixture").await.unwrap(),
        expected
    );
    assert_eq!(
        reopened
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await
            .unwrap(),
        1
    );
    assert!(!target_path.exists());
    assert!(!target_workspace.exists());
    assert!(!application.data_root.join("runs/target-run.json").exists());
    assert!(application.data_root.join("runs/other-run.json").exists());
    assert!(application.read_graph("other").is_ok());
    assert!(matches!(
        application
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await,
        Err(GraphManagementError::Missing(_))
    ));
}

#[tokio::test]
async fn definition_and_layout_changes_refuse_the_old_confirmation() {
    let (root, application) = fixture();
    for replacement in [definition("changed"), {
        let mut replacement = definition("changed");
        replacement["layout"] = json!({"work": {"x": 12, "y": 34}});
        replacement
    }] {
        let expected = application
            .graph_delete_precondition("fixture")
            .await
            .unwrap();
        application
            .update_graph("fixture", replacement)
            .await
            .unwrap();
        assert_conflict_unchanged(
            &application,
            "fixture",
            &root.path().join("workspaces"),
            &expected,
        )
        .await;
    }
}

#[tokio::test]
async fn definition_changed_then_restored_still_refuses_the_old_confirmation() {
    let (root, application) = fixture();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    application
        .update_graph("fixture", definition("changed"))
        .await
        .unwrap();
    application
        .update_graph("fixture", definition("fixture"))
        .await
        .unwrap();
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
}

#[tokio::test]
async fn deletion_and_identical_recreation_refuse_the_old_confirmation() {
    let (root, application) = fixture();
    application.create_graph("target", None).await.unwrap();
    let expected = application
        .graph_delete_precondition("target")
        .await
        .unwrap();
    let workspace_root = root.path().join("workspaces");
    application
        .remove_graph("target", &workspace_root)
        .await
        .unwrap();
    application.create_graph("target", None).await.unwrap();
    assert_conflict_unchanged(&application, "target", &workspace_root, &expected).await;
    let fresh = application
        .graph_delete_precondition("target")
        .await
        .unwrap();
    assert_ne!(fresh, expected);
    assert_eq!(
        application
            .remove_graph_if_unchanged("target", &workspace_root, &fresh)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn identical_directory_replacement_refuses_the_old_confirmation() {
    let (root, application) = fixture();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let path = application.graph_path_checked("fixture").unwrap();
    let before = files(&path);
    std::fs::rename(&path, root.path().join("old-bundle")).unwrap();
    write_graph_bundle(&path, &definition("fixture")).unwrap();
    assert_eq!(files(&path), before);
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
}

#[tokio::test]
async fn identical_resource_replacement_refuses_the_old_confirmation() {
    let (root, application) = fixture();
    let path = plugin_bundle(&application);
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let resource = path.join("plugins/demo/skills/SKILL.md");
    let replacement = root.path().join("replacement");
    std::fs::write(&replacement, std::fs::read(&resource).unwrap()).unwrap();
    std::fs::rename(replacement, &resource).unwrap();
    application.load_graph("fixture").unwrap();
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
}

#[tokio::test]
async fn valid_resource_updates_refuse_the_old_confirmation() {
    let (root, application) = fixture();
    let path = plugin_bundle(&application);
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    std::fs::write(
        path.join("plugins/demo/skills/SKILL.md"),
        "updated resource",
    )
    .unwrap();
    seal_plugin_bundle(&path);
    application.load_graph("fixture").unwrap();
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
}

#[tokio::test]
async fn invalid_and_changed_resources_fail_closed_without_cleanup() {
    let (root, application) = fixture();
    let path = plugin_bundle(&application);
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    save_run(
        &application,
        "fixture",
        "retained-run",
        RunStatus::Completed,
    );
    let resource = path.join("plugins/demo/skills/SKILL.md");
    std::fs::write(&resource, "changed resource").unwrap();
    let before = files(root.path());
    assert!(matches!(
        application
            .remove_graph_if_unchanged("fixture", &root.path().join("workspaces"), &expected)
            .await,
        Err(GraphManagementError::Invalid(_))
    ));
    assert_eq!(files(root.path()), before);
    assert!(matches!(
        application.graph_delete_precondition("fixture").await,
        Err(GraphManagementError::Invalid(_))
    ));
    std::fs::remove_file(resource).unwrap();
    assert!(
        application
            .graph_delete_precondition("fixture")
            .await
            .is_err()
    );
    assert!(
        application
            .data_root
            .join("runs/retained-run.json")
            .exists()
    );
}

#[tokio::test]
async fn malformed_tokens_and_names_are_rejected_without_deleting() {
    let (root, application) = fixture();
    let valid = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let before = files(root.path());
    for token in [
        String::new(),
        "{}".into(),
        "graph-delete-v0:".into(),
        format!("graph-delete-v1:{}", "g".repeat(64)),
        format!("graph-delete-v1:{}", "A".repeat(64)),
        format!("graph-delete-v1:{}", "a".repeat(63)),
        format!("graph-delete-v1:{}", "a".repeat(65)),
        format!("{valid}\n"),
    ] {
        assert!(matches!(
            application
                .remove_graph_if_unchanged("fixture", &root.path().join("workspaces"), &token)
                .await,
            Err(GraphManagementError::BadRequest(_))
        ));
        assert_eq!(files(root.path()), before);
    }
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &format!("graph-delete-v1:{}", "0".repeat(64)),
    )
    .await;
    for name in ["", "../fixture", "a/b"] {
        assert!(matches!(
            application.graph_delete_precondition(name).await,
            Err(GraphManagementError::BadRequest(_))
        ));
        assert!(matches!(
            application
                .remove_graph_if_unchanged(name, &root.path().join("workspaces"), &valid)
                .await,
            Err(GraphManagementError::BadRequest(_))
        ));
    }
}

#[tokio::test]
async fn tokens_are_bound_to_target_name_path_and_configured_alias() {
    let (root, application) = fixture();
    application
        .create_graph("other", Some(definition("fixture")))
        .await
        .unwrap();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    for name in ["other", "bundle"] {
        assert_conflict_unchanged(
            &application,
            name,
            &root.path().join("workspaces"),
            &expected,
        )
        .await;
    }
    let (_other_root, other_application) = fixture();
    assert_conflict_unchanged(
        &other_application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
}

#[tokio::test]
async fn missing_invalid_and_symlink_targets_cannot_produce_or_consume_a_snapshot() {
    use std::os::unix::fs::symlink;

    let (root, application) = fixture();
    assert!(matches!(
        application.graph_delete_precondition("missing").await,
        Err(GraphManagementError::Missing(_))
    ));
    let path = application.graph_path_checked("fixture").unwrap();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let moved = root.path().join("moved");
    std::fs::rename(&path, &moved).unwrap();
    symlink(&moved, &path).unwrap();
    for outcome in [
        application
            .graph_delete_precondition("fixture")
            .await
            .map(|_| 0),
        application
            .remove_graph_if_unchanged("fixture", &root.path().join("workspaces"), &expected)
            .await,
    ] {
        assert!(matches!(outcome, Err(GraphManagementError::Invalid(_))));
    }
    assert!(moved.join("graph.json").exists());
    std::fs::remove_file(&path).unwrap();
    symlink(root.path().join("missing"), &path).unwrap();
    assert!(matches!(
        application.graph_delete_precondition("fixture").await,
        Err(GraphManagementError::Invalid(_))
    ));
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("graph.json"), "not JSON").unwrap();
    assert!(
        application
            .graph_delete_precondition("fixture")
            .await
            .is_err()
    );
    assert!(
        application
            .remove_graph_if_unchanged("fixture", &root.path().join("workspaces"), &expected)
            .await
            .is_err()
    );
    assert!(path.join("graph.json").exists());
}

#[tokio::test]
async fn resource_symlinks_and_special_files_are_rejected_without_following_them() {
    use std::os::unix::fs::symlink;

    let (root, application) = fixture();
    let path = plugin_bundle(&application);
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let resource = path.join("plugins/demo/skills/SKILL.md");
    let outside = root.path().join("outside");
    std::fs::write(&outside, "safe resource").unwrap();
    std::fs::remove_file(&resource).unwrap();
    symlink(&outside, &resource).unwrap();
    assert!(matches!(
        application
            .remove_graph_if_unchanged("fixture", &root.path().join("workspaces"), &expected)
            .await,
        Err(GraphManagementError::Invalid(_))
    ));
    assert!(matches!(
        application.graph_delete_precondition("fixture").await,
        Err(GraphManagementError::Invalid(_))
    ));
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "safe resource");
    std::fs::remove_file(&resource).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &resource,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(1),
            application.graph_delete_precondition("fixture")
        )
        .await
        .unwrap(),
        Err(GraphManagementError::Invalid(_))
    ));
}

#[tokio::test]
async fn ordinary_remove_keeps_existing_symlink_compatibility() {
    use std::os::unix::fs::symlink;

    let (root, application) = fixture();
    let target = application.graph_path_checked("fixture").unwrap();
    let link = application.graph_path_checked("link").unwrap();
    symlink(&target, &link).unwrap();
    assert!(application.load_graph("link").is_ok());
    assert!(matches!(
        application.graph_delete_precondition("link").await,
        Err(GraphManagementError::Invalid(_))
    ));
    assert_eq!(
        application
            .remove_graph("link", &root.path().join("workspaces"))
            .await
            .unwrap(),
        0
    );
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert!(application.load_graph("fixture").is_ok());
}

#[tokio::test]
async fn guarded_delete_rechecks_unfinished_runs_created_after_the_snapshot() {
    let (root, application) = fixture();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    save_run(&application, "fixture", "unfinished", RunStatus::Stopped);
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    assert!(application.data_root.join("runs/unfinished.json").exists());
}

#[tokio::test]
async fn guarded_delete_rechecks_active_execution_even_with_a_terminal_record() {
    use crate::application::ActiveRun;
    use std::sync::{Arc, atomic::AtomicBool};

    let (root, application) = fixture();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    save_run(&application, "fixture", "active", RunStatus::Completed);
    application.active.lock().await.insert(
        "active".into(),
        ActiveRun {
            graph_path: application.graph_path_checked("fixture").unwrap(),
            control: crate::HostControl {
                cancellation: Arc::new(AtomicBool::new(false)),
                pause: Arc::new(AtomicBool::new(false)),
            },
        },
    );
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    assert!(application.data_root.join("runs/active.json").exists());
}

#[tokio::test]
async fn guarded_delete_preserves_pending_session_delivery() {
    use crate::application::session_calls::{SessionCall, SessionContext};

    let (root, application) = fixture();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    save_run(&application, "fixture", "pending", RunStatus::Completed);
    let mut run_metadata = application.metadata("pending").unwrap().unwrap();
    run_metadata.session_call = Some(SessionCall {
        context: SessionContext {
            session: "fixture-session".into(),
            reply_node: "work".into(),
            conversation_id: "fixture-conversation".into(),
            channel: json!({"source": "fixture"}),
        },
        status: "pending".into(),
        error: String::new(),
    });
    metadata::save(&application.data_root, &run_metadata).unwrap();
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    assert!(application.data_root.join("runs/pending.json").exists());
}

#[tokio::test]
async fn permission_changes_invalidate_the_snapshot_without_expanding_authority() {
    use std::os::unix::fs::PermissionsExt;

    let (root, application) = fixture();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let path = application.graph_path_checked("fixture").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn guarded_delete_rechecks_current_callers_created_after_the_snapshot() {
    let (root, application) = fixture();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    application
        .create_graph("caller", Some(caller_definition("fixture")))
        .await
        .unwrap();
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    assert!(application.read_graph("caller").is_ok());
}

#[tokio::test]
async fn guarded_delete_preserves_unfinished_callers_frozen_in_run_snapshots() {
    let (root, application) = fixture();
    application
        .create_graph("caller", Some(caller_definition("fixture")))
        .await
        .unwrap();
    save_run(&application, "caller", "caller-run", RunStatus::Stopped);
    application
        .update_graph("caller", definition("no longer calls"))
        .await
        .unwrap();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    assert!(application.data_root.join("runs/caller-run.json").exists());
}

#[tokio::test]
async fn snapshot_and_guarded_delete_wait_for_graph_admission_before_reading() {
    let (root, application) = fixture();
    let path = application.graph_path_checked("fixture").unwrap();
    let held = application.graph_admission_lease(&path).unwrap();
    let snapshot_application = application.clone();
    let mut snapshot = tokio::spawn(async move {
        snapshot_application
            .graph_delete_precondition("fixture")
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut snapshot)
            .await
            .is_err()
    );
    drop(held);
    let expected = snapshot.await.unwrap().unwrap();
    let held = application.graph_admission_lease(&path).unwrap();
    let delete_application = application.clone();
    let workspace_root = root.path().join("workspaces");
    let mut delete = tokio::spawn(async move {
        delete_application
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut delete)
            .await
            .is_err()
    );
    write_graph_bundle(&path, &definition("changed under admission lease")).unwrap();
    drop(held);
    assert!(matches!(
        delete.await.unwrap(),
        Err(GraphManagementError::Conflict(_))
    ));
    assert!(path.exists());
}

#[tokio::test]
async fn concurrent_admission_is_rechecked_after_the_guarded_delete_gets_its_lease() {
    let (root, application) = fixture();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let path = application.graph_path_checked("fixture").unwrap();
    let held = application.graph_admission_lease(&path).unwrap();
    let delete_application = application.clone();
    let workspace_root = root.path().join("workspaces");
    let mut delete = tokio::spawn(async move {
        delete_application
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut delete)
            .await
            .is_err()
    );
    save_run(&application, "fixture", "admitted", RunStatus::Running);
    drop(held);
    assert!(
        matches!(delete.await.unwrap(), Err(GraphManagementError::Conflict(message)) if message.contains("unfinished Run `admitted`"))
    );
    assert!(application.data_root.join("runs/admitted.json").exists());
    assert!(path.exists());
}

#[tokio::test]
async fn guarded_delete_holds_the_graph_lease_until_cleanup_finishes() {
    let (root, application) = fixture();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let path = application.graph_path_checked("fixture").unwrap();
    let active = application.active.lock().await;
    let delete_application = application.clone();
    let workspace_root = root.path().join("workspaces");
    let mut delete = tokio::spawn(async move {
        delete_application
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut delete)
            .await
            .is_err()
    );
    assert!(matches!(
        application.graph_admission_lease(&path),
        Err(ApplicationError::Conflict(_))
    ));
    assert!(path.exists());
    drop(active);
    assert_eq!(delete.await.unwrap().unwrap(), 0);
    assert!(!path.exists());
}

#[tokio::test]
async fn catalog_gate_serializes_snapshot_and_guarded_delete_with_reference_mutations() {
    let (root, application) = fixture();
    application.create_graph("caller", None).await.unwrap();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let guard = application.graph_catalog_mutation_guard().await;
    let update_application = application.clone();
    let mut update = tokio::spawn(async move {
        update_application
            .update_graph("caller", caller_definition("fixture"))
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut update)
            .await
            .is_err()
    );
    let delete_application = application.clone();
    let workspace_root = root.path().join("workspaces");
    let mut delete = tokio::spawn(async move {
        delete_application
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut delete)
            .await
            .is_err()
    );
    drop(guard);
    update.await.unwrap().unwrap();
    assert!(
        matches!(delete.await.unwrap(), Err(GraphManagementError::Conflict(message)) if message.contains("Graph `caller`"))
    );
    assert!(application.load_graph("fixture").is_ok());
}

#[tokio::test]
async fn concurrent_confirmed_deletes_have_only_one_success() {
    let (root, application) = fixture();
    let expected = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let workspace_root = root.path().join("workspaces");
    let (first, second) = tokio::join!(
        application.remove_graph_if_unchanged("fixture", &workspace_root, &expected),
        application.remove_graph_if_unchanged("fixture", &workspace_root, &expected),
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert!(
        matches!(first, Err(GraphManagementError::Missing(_)))
            || matches!(second, Err(GraphManagementError::Missing(_)))
    );
}

#[tokio::test]
async fn snapshots_wait_for_the_catalog_gate_and_capture_the_published_definition() {
    let (_root, application) = fixture();
    let previous = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap();
    let guard = application.graph_catalog_mutation_guard().await;
    let update_application = application.clone();
    let mut update = tokio::spawn(async move {
        update_application
            .update_graph("fixture", definition("published"))
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut update)
            .await
            .is_err()
    );
    let snapshot_application = application.clone();
    let mut snapshot = tokio::spawn(async move {
        snapshot_application
            .graph_delete_precondition("fixture")
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut snapshot)
            .await
            .is_err()
    );
    drop(guard);
    update.await.unwrap().unwrap();
    let expected = snapshot.await.unwrap().unwrap();
    assert_ne!(expected, previous);
    assert_eq!(
        expected,
        application
            .graph_delete_precondition("fixture")
            .await
            .unwrap()
    );
}
