use super::*;
use crate::application::{RunMetadata, metadata};
use anchor_runtime::graph::{FileRunStore, GraphRunRecord, RunStatus, RunStore};
use std::{collections::BTreeMap, time::Duration};

mod deletion;

fn definition(objective: &str) -> Value {
    json!({
        "objective": objective,
        "entry": "work",
        "agents": {},
        "ops": {"work": {"run": "true"}},
        "nodes": [{"id": "work", "op": "work", "plugins": []}],
        "edges": []
    })
}

fn caller_definition(target: &str) -> Value {
    json!({
        "objective": "caller",
        "entry": "call",
        "agents": {},
        "ops": {"call": {"call": {"graph": target, "mode": "wait", "input": {}}}},
        "nodes": [{"id": "call", "op": "call", "plugins": []}],
        "edges": []
    })
}

fn fixture() -> (tempfile::TempDir, RunApplication) {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    let bundle = catalog.join("bundle");
    write_graph_bundle(&bundle, &definition("fixture")).unwrap();
    let application = RunApplication::new(root.path().join("state"), catalog)
        .with_configured_graph("fixture".into(), bundle);
    (root, application)
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            result.insert(path.clone(), Vec::new());
            result.extend(files(&path));
        } else {
            result.insert(path.clone(), std::fs::read(path).unwrap());
        }
    }
    result
}

#[test]
fn names_and_configured_paths_are_checked_without_writing() {
    let (root, application) = fixture();
    let before = files(root.path());
    for name in [
        "",
        ".",
        "..",
        "../fixture",
        "a/b",
        "a\\b",
        "a b",
        "中文",
        "hidden~",
    ] {
        let failure = application.graph_path_checked(name).unwrap_err();
        assert!(matches!(failure, GraphManagementError::BadRequest(_)));
        assert_eq!(failure.to_string(), "invalid graph name");
    }
    for name in ["graph", "Graph-2_1.json", ".hidden"] {
        assert_eq!(
            application.graph_path_checked(name).unwrap(),
            application.catalog_root.join(name)
        );
    }
    assert_eq!(
        application.graph_path_checked("fixture").unwrap(),
        application.catalog_root.join("bundle")
    );
    assert_eq!(files(root.path()), before);
}

#[tokio::test]
async fn list_and_read_preserve_configured_alias_and_missing_projection() {
    let (root, application) = fixture();
    write_graph_bundle(
        &application.catalog_root.join("another"),
        &definition("other"),
    )
    .unwrap();
    write_graph_bundle(
        &application.catalog_root.join(".graph-create-1-2~"),
        &definition("staged"),
    )
    .unwrap();
    std::fs::create_dir(application.catalog_root.join("invalid")).unwrap();
    let listed = application.list_graphs().await.unwrap();
    assert_eq!(
        listed,
        json!({"graphs": [
            {"graph":"another","running":null,"active_runs":[],"active_nodes":[],"missing":false},
            {"graph":"fixture","running":null,"active_runs":[],"active_nodes":[],"missing":false}
        ]})
    );
    let configured = application.read_graph("fixture").unwrap();
    let aliased = application.read_graph("bundle").unwrap();
    assert_eq!(configured["graph"], "fixture");
    assert_eq!(aliased["graph"], "bundle");
    assert_eq!(configured["definition"], aliased["definition"]);
    assert_eq!(configured["node_plugins"], json!({"work":[]}));
    assert_eq!(
        application.load_graph("fixture").unwrap().0,
        application.load_graph("bundle").unwrap().0
    );
    let absent = RunApplication::new(root.path().join("other-state"), root.path().join("absent"))
        .with_configured_graph("missing".into(), root.path().join("deployment"));
    assert_eq!(
        absent.list_graphs().await.unwrap(),
        json!({"graphs":[{"graph":"missing","running":null,"active_runs":[],"active_nodes":[],"missing":true}]})
    );
    let catalog_only =
        RunApplication::new(root.path().join("other-state"), root.path().join("absent"));
    assert_eq!(
        catalog_only.list_graphs().await.unwrap(),
        json!({"graphs":[]})
    );
}

#[test]
fn load_distinguishes_missing_and_invalid_bundles() {
    let (_root, application) = fixture();
    assert!(matches!(
        application.load_graph("missing"),
        Err(GraphManagementError::Missing(message)) if message == "no such graph"
    ));
    std::fs::create_dir(application.catalog_root.join("invalid")).unwrap();
    assert!(matches!(
        application.load_graph("invalid"),
        Err(GraphManagementError::Invalid(_))
    ));
}

#[tokio::test]
async fn active_nodes_require_a_live_executor_and_running_record() {
    let (_root, application) = fixture();
    let (path, bundle) = application.load_graph("fixture").unwrap();
    let mut record = GraphRunRecord::create(bundle.snapshot, json!({})).unwrap();
    record.status = RunStatus::Running;
    record.invocations.insert("work".into(), 1);
    record.passes.insert("work".into(), 1);
    record.cursor = Some(anchor_runtime::graph::RunCursor {
        node_id: "work".into(),
        key: anchor_runtime::graph::InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: "work".into(),
            invocation: 1,
        },
        input_commits: Vec::new(),
        prepared_input: json!({}),
    });
    application.store().save(&record).unwrap();
    assert_eq!(
        application.list_graphs().await.unwrap()["graphs"][0]["active_nodes"],
        json!([])
    );
    application.active.lock().await.insert(
        record.run_id.clone(),
        crate::application::ActiveRun {
            graph_path: RunApplication::graph_identity(&path).unwrap(),
            control: crate::application::new_control(),
        },
    );
    assert_eq!(
        application.list_graphs().await.unwrap()["graphs"][0]["active_nodes"],
        json!(["work"])
    );
    record.status = RunStatus::Paused;
    application.store().save(&record).unwrap();
    assert_eq!(
        application.list_graphs().await.unwrap()["graphs"][0]["active_nodes"],
        json!([])
    );
}

#[tokio::test]
async fn crud_round_trips_default_and_authoring_definition_without_http() {
    let (root, application) = fixture();
    let created = application.create_graph("new-graph", None).await.unwrap();
    assert_eq!(created["graph"], "new-graph");
    assert_eq!(created["definition"]["objective"], "new-graph");
    assert_eq!(created["definition"]["entry"], "start");
    let path = application.graph_path_checked("new-graph").unwrap();
    let (_, bundle) = application.load_graph("new-graph").unwrap();
    assert_eq!(bundle.authoring_definition, created["definition"]);
    let mut replacement = definition("edited");
    replacement["layout"] = json!({"work":{"x":12,"y":34}});
    let updated = application
        .update_graph("new-graph", replacement.clone())
        .await
        .unwrap();
    assert_eq!(
        updated,
        json!({"graph":"new-graph","definition":replacement})
    );
    assert_eq!(
        application.read_graph("new-graph").unwrap()["definition"],
        replacement
    );
    assert_eq!(
        application
            .remove_graph("new-graph", &root.path().join("workspaces"))
            .await
            .unwrap(),
        0
    );
    assert!(!path.exists());
    assert!(application.read_graph("fixture").is_ok());
    assert!(
        std::fs::read_dir(&application.catalog_root)
            .unwrap()
            .all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains("stage")
            })
    );
}

#[tokio::test]
async fn failed_creation_cleans_staging_and_preserves_conflict_precedence() {
    let (_root, application) = fixture();
    assert!(matches!(
        application.create_graph("fixture", Some(Value::Null)).await,
        Err(GraphManagementError::Conflict(message)) if message == "graph already exists"
    ));
    let before = files(&application.catalog_root);
    assert!(matches!(
        application.create_graph("invalid", Some(Value::Null)).await,
        Err(GraphManagementError::Invalid(_))
    ));
    assert!(!application.graph_path_checked("invalid").unwrap().exists());
    assert_eq!(files(&application.catalog_root), before);
}

#[tokio::test]
async fn filesystem_failures_are_storage_errors() {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog-file");
    std::fs::write(&catalog, "not a directory").unwrap();
    let application = RunApplication::new(root.path().join("state"), catalog);
    assert!(matches!(
        application.create_graph("new-graph", None).await,
        Err(GraphManagementError::Storage(_))
    ));
}

#[tokio::test]
async fn invalid_update_preserves_the_published_bundle() {
    let (_root, application) = fixture();
    let path = application.graph_path_checked("fixture").unwrap();
    let before = files(&path);
    assert!(matches!(
        application.update_graph("fixture", Value::Null).await,
        Err(GraphManagementError::Invalid(_))
    ));
    assert_eq!(files(&path), before);
    assert!(matches!(
        application.update_graph("missing", definition("new")).await,
        Err(GraphManagementError::Missing(message)) if message == "no such graph"
    ));
}

#[tokio::test]
async fn validation_is_readonly_and_does_not_take_mutation_or_admission_locks() {
    let (root, application) = fixture();
    let path = application.graph_path_checked("fixture").unwrap();
    let _catalog_guard = application.graph_catalog_mutation_guard().await;
    let _lease = application.graph_admission_lease(&path).unwrap();
    let before = files(root.path());
    let snapshot = application.validate_graph(&definition("valid")).unwrap();
    assert_eq!(snapshot.entry, "work");
    assert_eq!(snapshot.nodes[0].id, "work");
    assert!(matches!(
        application.validate_graph(&Value::Null),
        Err(GraphManagementError::Invalid(_))
    ));
    assert_eq!(files(root.path()), before);
}

#[tokio::test]
async fn mutation_lease_protects_creation_update_and_aliased_delete() {
    let (root, application) = fixture();
    let new_path = application.graph_path_checked("new-graph").unwrap();
    let held = application.graph_admission_lease(&new_path).unwrap();
    assert!(matches!(
        application.create_graph("new-graph", None).await,
        Err(GraphManagementError::Conflict(_))
    ));
    drop(held);
    let path = application.graph_path_checked("fixture").unwrap();
    let held = application.graph_admission_lease(&path).unwrap();
    assert!(matches!(
        application
            .update_graph("bundle", definition("updated"))
            .await,
        Err(GraphManagementError::Conflict(_))
    ));
    let delete_application = application.clone();
    let workspace_root = root.path().join("workspaces");
    let mut delete = tokio::spawn(async move {
        delete_application
            .remove_graph("bundle", &workspace_root)
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut delete)
            .await
            .is_err()
    );
    drop(held);
    assert_eq!(delete.await.unwrap().unwrap(), 0);
    assert!(!path.exists());
}

#[tokio::test]
async fn catalog_gate_serializes_reference_update_and_delete() {
    let (root, application) = fixture();
    application.create_graph("target", None).await.unwrap();
    application.create_graph("caller", None).await.unwrap();
    let guard = application.graph_catalog_mutation_guard().await;
    let update_application = application.clone();
    let mut update = tokio::spawn(async move {
        update_application
            .update_graph("caller", caller_definition("target"))
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
            .remove_graph("target", &workspace_root)
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
    assert!(application.load_graph("target").is_ok());
}

#[tokio::test]
async fn update_preserves_run_snapshot_and_delete_reuses_unfinished_run_checks() {
    let (root, application) = fixture();
    let (path, bundle) = application.load_graph("fixture").unwrap();
    let mut record = GraphRunRecord::create_with_id(bundle.snapshot, json!({}), "frozen").unwrap();
    record.status = RunStatus::Stopped;
    record.plugin_bindings_initialized = true;
    let store = FileRunStore::new(application.data_root.join("runs"));
    store.save(&record).unwrap();
    let run_metadata = RunMetadata::new(
        "frozen".into(),
        "fixture".into(),
        record.graph_digest.clone(),
        &path,
    )
    .unwrap();
    metadata::save(&application.data_root, &run_metadata).unwrap();
    let before = std::fs::read(application.data_root.join("runs/frozen.json")).unwrap();
    application
        .update_graph("bundle", definition("edited"))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(application.data_root.join("runs/frozen.json")).unwrap(),
        before
    );
    assert_eq!(
        application.read_graph("fixture").unwrap()["definition"]["objective"],
        "edited"
    );
    assert!(matches!(
        application.remove_graph("bundle", &root.path().join("workspaces")).await,
        Err(GraphManagementError::Conflict(message)) if message.contains("unfinished Run `frozen`")
    ));
    record.status = RunStatus::Completed;
    store.save(&record).unwrap();
    assert_eq!(
        application
            .remove_graph("bundle", &root.path().join("workspaces"))
            .await
            .unwrap(),
        1
    );
    assert!(!path.exists());
    assert!(application.records().unwrap().is_empty());
}

#[test]
fn application_failures_keep_their_categories_and_messages() {
    let failures = [
        (ApplicationError::Missing, "no such run"),
        (ApplicationError::Conflict("busy".into()), "busy"),
        (ApplicationError::Invalid("invalid".into()), "invalid"),
        (ApplicationError::Storage("io".into()), "io"),
    ];
    for (failure, expected) in failures {
        let failure = GraphManagementError::from(failure);
        assert_eq!(failure.to_string(), expected);
        match expected {
            "no such run" => assert!(matches!(failure, GraphManagementError::Missing(_))),
            "busy" => assert!(matches!(failure, GraphManagementError::Conflict(_))),
            "invalid" => assert!(matches!(failure, GraphManagementError::Invalid(_))),
            "io" => assert!(matches!(failure, GraphManagementError::Storage(_))),
            _ => unreachable!(),
        }
    }
}
