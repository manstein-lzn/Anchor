use super::*;

async fn delete_snapshot(application: &RunApplication, name: &str) -> String {
    application.graph_delete_precondition(name).await.unwrap()
}

async fn assert_waiting<T>(task: &mut tokio::task::JoinHandle<T>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(30), task)
            .await
            .is_err()
    );
}

fn create_run(application: &RunApplication, name: &str, run_id: &str) {
    let (path, bundle) = application.load_graph(name).unwrap();
    let mut record = GraphRunRecord::create_with_id(bundle.snapshot, json!({}), run_id).unwrap();
    record.plugin_bindings_initialized = true;
    fixtures::create(application, &record);
    metadata::save(
        &application.data_root,
        &RunMetadata::new(run_id.into(), name.into(), record.graph_digest, &path).unwrap(),
    )
    .unwrap();
}

fn finished_run(application: &RunApplication, name: &str, run_id: &str) {
    create_run(application, name, run_id);
    fixtures::finish(application, run_id);
}

fn stopped_run(application: &RunApplication, name: &str, run_id: &str) {
    create_run(application, name, run_id);
    fixtures::control(application, run_id, "stop", "stopped");
}

fn plugin_bundle(application: &RunApplication) -> PathBuf {
    let path = application.graph_path_checked("fixture").unwrap();
    // The Plugin exists once, in the Library; the bundle references it by id.
    let plugin = application.catalog_root.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills")).unwrap();
    std::fs::write(plugin.join("plugin.json"), r#"{"name":"Demo"}"#).unwrap();
    std::fs::write(plugin.join("skills/SKILL.md"), "safe resource").unwrap();
    let mut authoring = definition("fixture");
    authoring["agents"] = json!({"worker": {"model": "fixture", "instructions": "work"}});
    authoring["ops"] = json!({});
    authoring["nodes"] = json!([{"id": "work", "agent": "worker", "plugins": ["demo"]}]);
    std::fs::write(path.join("graph.json"), authoring.to_string()).unwrap();
    seal_plugin_bundle(&application.catalog_root, &path);
    application.load_graph("fixture").unwrap();
    path
}

fn seal_plugin_bundle(library_root: &Path, path: &Path) {
    let binding = Catalog::new(library_root)
        .resolve(&["demo".into()])
        .unwrap()
        .remove(0);
    std::fs::write(
        path.join("manifest.json"),
        json!({
            "format": 1,
            "graph": "graph.json",
            "plugins": [{"id": binding.id}]
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
) -> String {
    let path = application.graph_path_checked(name).unwrap();
    let before = files(&path);
    let failure = application
        .remove_graph_if_unchanged(name, workspace_root, expected)
        .await;
    let reason = match failure {
        Err(GraphManagementError::Conflict(reason)) => reason,
        result => panic!("expected a deletion conflict, got {result:?}"),
    };
    assert_eq!(files(&path), before);
    reason
}

#[tokio::test]
async fn unchanged_snapshot_survives_reopen_and_reuses_terminal_run_cleanup() {
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, application) = fixture();
    application.create_graph("other", None).await.unwrap();
    finished_run(&application, "fixture", "target-run");
    finished_run(&application, "other", "other-run");
    let workspace_root = root.path().join("workspaces");
    let target_workspace = workspace_root.join("target-run");
    std::fs::create_dir_all(&target_workspace).unwrap();
    std::fs::write(target_workspace.join("result.txt"), "result").unwrap();
    let target_path = application.graph_path_checked("fixture").unwrap();
    let before = files(&target_path);
    let expected = delete_snapshot(&application, "fixture").await;
    assert_eq!(files(&target_path), before);
    assert_eq!(delete_snapshot(&application, "fixture").await, expected);
    let reopened = RunApplication::new(
        application.data_root.clone(),
        application.catalog_root.clone(),
    )
    .with_configured_graph("fixture".into(), target_path.clone());
    drop(application);
    assert_eq!(delete_snapshot(&reopened, "fixture").await, expected);
    assert_eq!(
        reopened
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await
            .unwrap(),
        1
    );
    assert!(!target_path.exists());
    assert!(!target_workspace.exists());
    assert!(reopened.read_run("target-run").unwrap().is_none());
    assert!(reopened.read_run("other-run").unwrap().is_some());
    assert!(reopened.read_graph("other").is_ok());
    assert!(matches!(
        reopened
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await,
        Err(GraphManagementError::Missing(_))
    ));
}

#[tokio::test]
async fn definition_and_layout_changes_refuse_the_old_confirmation() {
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, application) = fixture();
    for replacement in [definition("changed"), {
        let mut replacement = definition("changed");
        replacement["layout"] = json!({"work": {"x": 12, "y": 34}});
        replacement
    }] {
        let expected = delete_snapshot(&application, "fixture").await;
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
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, application) = fixture();
    let expected = delete_snapshot(&application, "fixture").await;
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
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, application) = fixture();
    application.create_graph("target", None).await.unwrap();
    let expected = delete_snapshot(&application, "target").await;
    let workspace_root = root.path().join("workspaces");
    application
        .remove_graph("target", &workspace_root)
        .await
        .unwrap();
    application.create_graph("target", None).await.unwrap();
    assert_conflict_unchanged(&application, "target", &workspace_root, &expected).await;
    let fresh = delete_snapshot(&application, "target").await;
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
    let expected = delete_snapshot(&application, "fixture").await;
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
    let expected = delete_snapshot(&application, "fixture").await;
    let resource = path.join("graph.json");
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
    let expected = delete_snapshot(&application, "fixture").await;
    // A changed but still valid Graph: the same Plugin reference, a new objective.
    let mut updated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.join("graph.json")).unwrap()).unwrap();
    updated["objective"] = json!("fixture updated");
    std::fs::write(path.join("graph.json"), updated.to_string()).unwrap();
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
    let expected = delete_snapshot(&application, "fixture").await;
    finished_run(&application, "fixture", "retained-run");
    let resource = path.join("graph.json");
    std::fs::write(&resource, "not a graph").unwrap();
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
    assert!(application.read_run("retained-run").unwrap().is_some());
}

#[tokio::test]
async fn malformed_tokens_and_names_are_rejected_without_deleting() {
    let (root, application) = fixture();
    let valid = delete_snapshot(&application, "fixture").await;
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
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, application) = fixture();
    application
        .create_graph("other", Some(definition("fixture")))
        .await
        .unwrap();
    let expected = delete_snapshot(&application, "fixture").await;
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
    let expected = delete_snapshot(&application, "fixture").await;
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
    let expected = delete_snapshot(&application, "fixture").await;
    let resource = path.join("graph.json");
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
    let expected = delete_snapshot(&application, "fixture").await;
    stopped_run(&application, "fixture", "unfinished");
    // Refuse before showing a native form, and recheck after an earlier form.
    let preflight = application
        .graph_delete_precondition("fixture")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        preflight.contains("unfinished Run `unfinished`") && preflight.contains("stop or abandon")
    );
    let reason = assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    assert!(reason.contains("unfinished Run `unfinished`"), "{reason}");
    assert!(application.read_run("unfinished").unwrap().is_some());
}

#[tokio::test]
async fn guarded_delete_rechecks_active_execution_even_with_a_terminal_record() {
    let (root, application) = fixture();
    let expected = delete_snapshot(&application, "fixture").await;
    create_run(&application, "fixture", "active");
    let drive = fixtures::gated_drive(&application, "active").await;
    fixtures::finish(&application, "active");
    assert!(fixtures::view(&application, "active").has_succeeded());
    assert!(fixtures::view(&application, "active").active());
    let reason = assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    assert_eq!(reason, "that Run is still running");
    assert!(application.read_run("active").unwrap().is_some());
    drive.drain(&application, "active").await;
}

#[tokio::test]
async fn guarded_delete_preserves_pending_session_delivery() {
    use crate::run_metadata::{SessionCall, SessionContext};

    let (root, application) = fixture();
    let expected = delete_snapshot(&application, "fixture").await;
    finished_run(&application, "fixture", "pending");
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
    let reason = assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    assert!(
        reason.contains("Run has pending Session execution or delivery"),
        "{reason}"
    );
    assert!(application.read_run("pending").unwrap().is_some());
}

#[tokio::test]
async fn permission_changes_invalidate_the_snapshot_without_expanding_authority() {
    use std::os::unix::fs::PermissionsExt;

    let (root, application) = fixture();
    let expected = delete_snapshot(&application, "fixture").await;
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
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, application) = fixture();
    let expected = delete_snapshot(&application, "fixture").await;
    application
        .create_graph("caller", Some(caller_definition("fixture")))
        .await
        .unwrap();
    let reason = assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    assert!(reason.contains("Graph `caller`"), "{reason}");
    assert!(application.read_graph("caller").is_ok());
}

#[tokio::test]
async fn guarded_delete_preserves_unfinished_callers_frozen_in_run_snapshots() {
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, application) = fixture();
    application
        .create_graph("caller", Some(caller_definition("fixture")))
        .await
        .unwrap();
    stopped_run(&application, "caller", "caller-run");
    application
        .update_graph("caller", definition("no longer calls"))
        .await
        .unwrap();
    let expected = delete_snapshot(&application, "fixture").await;
    assert_conflict_unchanged(
        &application,
        "fixture",
        &root.path().join("workspaces"),
        &expected,
    )
    .await;
    assert!(application.read_run("caller-run").unwrap().is_some());
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
    assert_waiting(&mut snapshot).await;
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
    assert_waiting(&mut delete).await;
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
    let expected = delete_snapshot(&application, "fixture").await;
    let path = application.graph_path_checked("fixture").unwrap();
    let held = application.graph_admission_lease(&path).unwrap();
    let delete_application = application.clone();
    let workspace_root = root.path().join("workspaces");
    let mut delete = tokio::spawn(async move {
        delete_application
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await
    });
    assert_waiting(&mut delete).await;
    create_run(&application, "fixture", "admitted");
    drop(held);
    assert!(
        matches!(delete.await.unwrap(), Err(GraphManagementError::Conflict(message)) if message.contains("unfinished Run `admitted`"))
    );
    assert!(application.read_run("admitted").unwrap().is_some());
    assert!(path.exists());
}

#[tokio::test]
async fn guarded_delete_holds_the_graph_lease_until_cleanup_finishes() {
    let (root, application) = fixture();
    let expected = delete_snapshot(&application, "fixture").await;
    let path = application.graph_path_checked("fixture").unwrap();
    let active = application.admission.lock().await;
    let delete_application = application.clone();
    let workspace_root = root.path().join("workspaces");
    let mut delete = tokio::spawn(async move {
        delete_application
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await
    });
    assert_waiting(&mut delete).await;
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
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, application) = fixture();
    application.create_graph("caller", None).await.unwrap();
    let expected = delete_snapshot(&application, "fixture").await;
    let guard = application.graph_catalog_mutation_guard().await;
    let update_application = application.clone();
    let mut update = tokio::spawn(async move {
        update_application
            .update_graph("caller", caller_definition("fixture"))
            .await
    });
    assert_waiting(&mut update).await;
    let delete_application = application.clone();
    let workspace_root = root.path().join("workspaces");
    let mut delete = tokio::spawn(async move {
        delete_application
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await
    });
    assert_waiting(&mut delete).await;
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
    let expected = delete_snapshot(&application, "fixture").await;
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
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (_root, application) = fixture();
    let previous = delete_snapshot(&application, "fixture").await;
    let guard = application.graph_catalog_mutation_guard().await;
    let update_application = application.clone();
    let mut update = tokio::spawn(async move {
        update_application
            .update_graph("fixture", definition("published"))
            .await
    });
    assert_waiting(&mut update).await;
    let snapshot_application = application.clone();
    let mut snapshot = tokio::spawn(async move {
        snapshot_application
            .graph_delete_precondition("fixture")
            .await
    });
    assert_waiting(&mut snapshot).await;
    drop(guard);
    update.await.unwrap().unwrap();
    let expected = snapshot.await.unwrap().unwrap();
    assert_ne!(expected, previous);
    assert_eq!(expected, delete_snapshot(&application, "fixture").await);
}

/// A Graph the catalog lists but this host cannot read may call the target, so
/// the delete refuses and names it — and then succeeds once it is gone.
#[tokio::test]
async fn a_graph_that_cannot_be_read_refuses_the_delete_and_is_named() {
    let (root, application) = fixture();
    let workspace_root = root.path().join("workspaces");
    let path = application.graph_path_checked("fixture").unwrap();
    let expected = delete_snapshot(&application, "fixture").await;

    // `broken` has a definition but no manifest, so loading it fails. The report
    // may not turn that into "nobody calls this".
    let broken = application.catalog_root.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join("graph.json"),
        r#"{"objective":"broken","entry":"only","agents":{"worker":{"model":"test"}},"ops":{},"nodes":[{"id":"only","agent":"worker"}],"edges":[]}"#,
    )
    .unwrap();

    let error = application
        .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
        .await
        .unwrap_err();
    assert!(
        matches!(error, GraphManagementError::Conflict(ref message) if message.contains("broken")),
        "the unreadable Graph must be named: {error:?}"
    );
    assert!(path.exists(), "a refused delete removes nothing");
    assert!(broken.exists());

    // With the unreadable Graph gone the same confirmation deletes: the refusal
    // was about coverage, not about a caller.
    std::fs::remove_dir_all(&broken).unwrap();
    assert_eq!(
        application
            .remove_graph_if_unchanged("fixture", &workspace_root, &expected)
            .await
            .unwrap(),
        0
    );
    assert!(!path.exists());
}
