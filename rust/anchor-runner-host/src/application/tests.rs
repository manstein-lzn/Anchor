use super::*;
use crate::application::run_access::fixtures;
use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot};
use serde_json::json;

fn work_record(id: &str) -> GraphRunRecord {
    GraphRunRecord::create_with_id(
        GraphSnapshot::from_authoring(json!({
            "entry":"work", "agents":{}, "ops":{"work":{"run":"true"}},
            "nodes":[{"id":"work","op":"work"}], "edges":[]
        }))
        .unwrap(),
        json!({}),
        id,
    )
    .unwrap()
}

fn child_metadata(
    application: &RunApplication,
    root: &Path,
    child: &GraphRunRecord,
    parent: &GraphRunRecord,
    mode: &str,
) {
    metadata::save(
        &application.data_root,
        &RunMetadata::graph_call_child(
            child.run_id.clone(),
            "graph".into(),
            child.graph_digest.clone(),
            root,
            metadata::GraphCallSource {
                parent_run: parent.run_id.clone(),
                parent_graph: "graph".into(),
                parent_graph_digest: parent.graph_digest.clone(),
                node: "work".into(),
                invocation: 1,
                mode: mode.into(),
                root_run: parent.run_id.clone(),
            },
        )
        .unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn startup_finishes_kernel_initialization_before_background_work() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let application = RunApplication::new(state.clone(), root.path().into());
    assert!(!state.join("kernel-runs").exists());
    application.recover_detached_at_startup().await.unwrap();
    let reader = anchor_kernel::HistoryReader::open(state.join("kernel-runs")).unwrap();
    for id in [
        "validator/constitution",
        "validator/human-decision",
        "validator/effect",
        anchor_runtime::graph::validator::GRAPH_VALIDATOR,
    ] {
        assert!(reader.load(id).unwrap().is_some(), "{id}");
    }
    assert!(application.run_views().unwrap().is_empty());

    let damaged = root.path().join("damaged");
    let partial = damaged.join("kernel-runs/validator%2Feffect");
    std::fs::create_dir_all(&partial).unwrap();
    std::fs::write(partial.join("resource.tmp"), b"interrupted").unwrap();
    let application = RunApplication::new(damaged, root.path().into());
    assert!(application.recover_detached_at_startup().await.is_err());
    assert_eq!(
        std::fs::read(partial.join("resource.tmp")).unwrap(),
        b"interrupted"
    );
    assert!(!partial.join("resource.json").exists());
}

#[tokio::test]
async fn predecessor_handover_waits_for_child_terminal_status_and_executor_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().into());
    let parent = work_record("parent");
    let child = work_record("child");
    fixtures::create(&application, &parent);
    fixtures::control(&application, "parent", "stop", "stopped");
    fixtures::create(&application, &child);
    child_metadata(&application, root.path(), &child, &parent, "wait");
    let parent_view = fixtures::view(&application, "parent");
    assert!(matches!(
        application.ensure_predecessor_settled(&parent_view, &()),
        Err(ApplicationError::Conflict(_))
    ));
    let drive = fixtures::gated_drive(&application, "child").await;
    fixtures::finish(&application, "child");
    assert!(fixtures::view(&application, "child").has_succeeded());
    assert!(matches!(
        application.ensure_predecessor_settled(&parent_view, &()),
        Err(ApplicationError::Conflict(_))
    ));
    drive.drain(&application, "child").await;
    application
        .ensure_predecessor_settled(&parent_view, &())
        .unwrap();
    assert!(fixtures::view(&application, "child").has_succeeded());
}

#[tokio::test]
async fn detached_unconfigured_executor_preserves_admitted_record_without_registration() {
    let root = tempfile::tempdir().unwrap();
    let mut application = RunApplication::new(root.path().join("state"), root.path().into());
    application.executor = None;
    let record = work_record("child");
    fixtures::create(&application, &record);
    child_metadata(
        &application,
        root.path(),
        &record,
        &work_record("parent"),
        "detach",
    );
    let before = fixtures::record(&application, "child");
    assert!(matches!(application.dispatch_detached("child").await,
        Err(ApplicationError::Invalid(reason)) if reason.contains("executor")));
    assert!(!application.run_is_active("child").await);
    assert_eq!(fixtures::record(&application, "child"), before);
}

#[tokio::test]
async fn detached_invalid_metadata_leaves_admission_and_workers_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().into());
    fixtures::create(&application, &work_record("child"));
    std::fs::create_dir_all(application.data_root.join("run-metadata")).unwrap();
    std::fs::write(
        application.data_root.join("run-metadata/child.json"),
        "invalid-json",
    )
    .unwrap();
    let before = application.load_run("child").unwrap().unwrap();
    assert!(application.dispatch_detached("child").await.is_err());
    assert!(!application.run_is_active("child").await);
    assert_eq!(application.load_run("child").unwrap().unwrap(), before);
}

#[tokio::test]
async fn control_projection_uses_kernel_submission_while_a_worker_drains() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().into());
    let record = work_record("active-run");
    fixtures::create(&application, &record);
    metadata::save(
        &application.data_root,
        &RunMetadata::new(
            record.run_id.clone(),
            "graph".into(),
            record.graph_digest.clone(),
            root.path(),
        )
        .unwrap(),
    )
    .unwrap();
    let stale = fixtures::view(&application, "active-run");
    assert_eq!(
        application.project_run(&stale, None).await.unwrap().phase,
        "executing"
    );
    let drive = fixtures::gated_drive(&application, "active-run").await;
    let metadata = application.metadata("active-run").unwrap().unwrap();
    assert!(
        application
            .project_run(&stale, Some(&metadata))
            .await
            .unwrap()
            .active
    );
    fixtures::control(&application, "active-run", "stop", "stopped");
    let stopped = application
        .project_run(&stale, Some(&metadata))
        .await
        .unwrap();
    assert_eq!(stopped.phase, "waiting");
    assert_eq!(stopped.waiting_for, Some("human"));
    assert_eq!(stopped.reason.as_deref(), Some("stopped"));
    assert!(stopped.active);
    assert!(!stopped.actions.resume);
    drive.drain(&application, "active-run").await;
    let inactive = application
        .project_run(&stale, Some(&metadata))
        .await
        .unwrap();
    assert_eq!(inactive.phase, "waiting");
    assert_eq!(inactive.reason.as_deref(), Some("stopped"));
    assert!(!inactive.active);
    assert!(inactive.actions.resume);
}
