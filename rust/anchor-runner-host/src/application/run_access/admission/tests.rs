use super::*;
use crate::application::{graphs, run_access::fixtures};
use crate::run_metadata::RunTrigger;
use crate::task_delegation::TaskOrigin;
use anchor_runtime::graph::{GraphTaskPolicy, InvocationKey};
use serde_json::json;

fn trigger() -> RunTrigger {
    serde_json::from_value(json!({
        "source":"schedule", "schedule":"plan", "scheduled_at":"2026-11-05T09:00:00"
    }))
    .unwrap()
}

fn task_origin() -> TaskOrigin {
    TaskOrigin {
        owner: "local".into(),
        session: Some("conversation".into()),
        graph: "source".into(),
        source: crate::task_delegation::TaskOriginSource::AgentDelegation {
            key: InvocationKey {
                run_id: "source-run".into(),
                graph_digest: "source-digest".into(),
                node_id: "agent".into(),
                invocation: 1,
            },
        },
        authorization_digest: "source-digest".into(),
        policy: GraphTaskPolicy::default(),
        bindings: Default::default(),
        targets: Default::default(),
    }
}

fn fixture(
    admitted: bool,
    task: Option<TaskOrigin>,
) -> (
    tempfile::TempDir,
    RunApplication,
    RunMetadata,
    GraphRunRecord,
) {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    let definition = json!({"objective":"original", "entry":"work", "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work"}],"edges":[]});
    let source = application.graph_bundle_path("task");
    graphs::write_graph_bundle_from_catalog(&source, &definition, application.library_root())
        .unwrap();
    let (_, bundle) = application.load_graph("task").unwrap();
    let input = json!({"topic":"briefing"});
    let request = admission_request("retained-request".into(), "task", &input, &None);
    let record =
        GraphRunRecord::create_with_id(bundle.snapshot, input, admission_run_id(Some(&request)))
            .unwrap();
    let mut metadata = RunMetadata::new(
        record.run_id.clone(),
        "task".into(),
        record.graph_digest.clone(),
        &source,
    )
    .unwrap();
    metadata.oauth_owner = Some("local".into());
    metadata.request = Some(request);
    metadata.task = task;
    trigger().apply(&mut metadata);
    metadata::save(&application.data_root, &metadata).unwrap();
    if admitted {
        fixtures::create(&application, &record);
    }
    (root, application, metadata, record)
}

#[tokio::test]
async fn accepted_request_replay_survives_graph_changes_removal_and_an_orphaned_worker() {
    let (_root, application, metadata, record) = fixture(true, None);
    let request = metadata.request.as_ref().unwrap();
    let original = serde_json::to_value(&metadata).unwrap();
    let source = application.graph_bundle_path("task");
    std::fs::write(source.join("graph.json"), "not JSON").unwrap();
    for remove in [false, true] {
        if remove {
            std::fs::remove_dir_all(&source).unwrap();
        }
        let replayed = application
            .admit_graph_request(
                "task",
                record.input.clone(),
                None,
                trigger(),
                Some("local".into()),
                Some(request.id.clone()),
            )
            .await
            .unwrap();
        assert_eq!(replayed, record.run_id);
        assert!(application.active_runs(None).await.is_empty());
        let orphan = application.read_run(&replayed).unwrap().unwrap();
        assert!(!orphan.active());
        assert_eq!(orphan.record.input, record.input);
        assert_eq!(
            serde_json::to_value(application.metadata(&replayed).unwrap().unwrap()).unwrap(),
            original
        );
    }
}

#[tokio::test]
async fn retained_admission_identity_binds_input_trigger_owner_and_graph() {
    let (_root, application, metadata, record) = fixture(true, None);
    let request = metadata.request.as_ref().unwrap();
    let changed_trigger = serde_json::from_value(json!({
        "source":"schedule","schedule":"another-plan","scheduled_at":"2026-11-05T09:00:00"
    }))
    .unwrap();
    let changed_time = serde_json::from_value(json!({
        "source":"schedule","schedule":"plan","scheduled_at":"2026-11-06T09:00:00"
    }))
    .unwrap();
    for (graph, input, source, owner) in [
        ("task", json!({"topic":"changed"}), trigger(), "local"),
        ("task", record.input.clone(), RunTrigger::default(), "local"),
        ("task", record.input.clone(), changed_trigger, "local"),
        ("task", record.input.clone(), changed_time, "local"),
        ("task", record.input.clone(), trigger(), "another-owner"),
        ("another-graph", record.input.clone(), trigger(), "local"),
    ] {
        assert!(matches!(
            application
                .admit_graph_request(
                    graph,
                    input,
                    None,
                    source,
                    Some(owner.into()),
                    Some(request.id.clone()),
                )
                .await,
            Err(ApplicationError::Conflict(_))
        ));
    }
    assert_eq!(application.records().unwrap().len(), 1);
    assert!(application.active_runs(None).await.is_empty());
}

#[tokio::test]
async fn accepted_delegation_replays_only_with_the_original_frozen_origin() {
    let origin = task_origin();
    let (_root, application, metadata, record) = fixture(true, Some(origin.clone()));
    let request = metadata.request.as_ref().unwrap();
    assert!(
        application
            .authorize_delegated_graph(&origin, "task")
            .is_err()
    );
    std::fs::remove_dir_all(application.graph_bundle_path("task")).unwrap();
    assert_eq!(
        application
            .admit_delegated_graph(
                &origin,
                "task",
                record.input.clone(),
                request.id.clone(),
                trigger(),
            )
            .await
            .unwrap(),
        record.run_id
    );
    let mut changed_scope = origin.clone();
    changed_scope.session = Some("another-conversation".into());
    let mut changed_grant = origin;
    changed_grant.policy.create_graphs = true;
    for origin in [changed_scope, changed_grant] {
        assert!(matches!(
            application
                .admit_delegated_graph(
                    &origin,
                    "task",
                    record.input.clone(),
                    request.id.clone(),
                    trigger(),
                )
                .await,
            Err(ApplicationError::Conflict(_))
        ));
    }
    assert!(application.active_runs(None).await.is_empty());
}

#[tokio::test]
async fn incomplete_admission_is_not_a_receipt_and_requires_the_same_snapshot() {
    let (_root, application, metadata, record) = fixture(false, None);
    let definition = json!({"objective":"changed execution contract", "entry":"work",
        "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]});
    graphs::write_graph_bundle_from_catalog(
        &application.graph_bundle_path("task"),
        &definition,
        application.library_root(),
    )
    .unwrap();
    assert!(matches!(
        application
            .admit_graph_request(
                "task",
                record.input.clone(),
                None,
                trigger(),
                Some("local".into()),
                Some(metadata.request.unwrap().id),
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    assert!(application.records().unwrap().is_empty());
    assert!(application.active_runs(None).await.is_empty());
}

#[tokio::test]
async fn incomplete_delegation_still_requires_current_authorization() {
    let origin = task_origin();
    let (_root, application, metadata, record) = fixture(false, Some(origin.clone()));
    assert!(
        application
            .admit_delegated_graph(
                &origin,
                "task",
                record.input,
                metadata.request.unwrap().id,
                trigger(),
            )
            .await
            .is_err()
    );
    assert!(application.records().unwrap().is_empty());
    assert!(application.active_runs(None).await.is_empty());
}

#[tokio::test]
async fn an_admitted_run_without_request_metadata_is_quarantined_not_restarted() {
    let (_root, application, metadata, record) = fixture(true, None);
    std::fs::remove_file(
        application
            .data_root
            .join("run-metadata")
            .join(format!("{}.json", record.run_id)),
    )
    .unwrap();
    assert!(matches!(
        application
            .admit_graph_request(
                "task",
                record.input,
                None,
                trigger(),
                Some("local".into()),
                Some(metadata.request.unwrap().id),
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    assert!(application.active_runs(None).await.is_empty());
    assert!(application.metadata(&record.run_id).unwrap().is_none());
}
