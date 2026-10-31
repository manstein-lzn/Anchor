use super::*;
use crate::run_metadata::{
    self as metadata, ConversationSource, RunMetadata, SessionCall, SessionContext,
};
use anchor_kernel::Outcome;
use anchor_runtime::graph::{
    ArtifactPort, GraphCallStatus, GraphSnapshot, InvocationKey,
    test_support::{Artifacts, Scripted, ScriptedCalls},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::atomic::AtomicBool};

fn fixture() -> (tempfile::TempDir, RunApplication, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    let workspace = root.path().join("workspaces");
    (root, application, workspace)
}

fn definition(name: &str) -> serde_json::Value {
    json!({"objective":name,"entry":"work","agents":{},
        "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]})
}

fn create(application: &RunApplication, graph: &str, id: &str) -> GraphRunRecord {
    create_definition(application, graph, id, definition(graph))
}

fn create_definition(
    application: &RunApplication,
    graph: &str,
    id: &str,
    definition: serde_json::Value,
) -> GraphRunRecord {
    let path = application.catalog_root.join(graph);
    crate::application::graphs::write_graph_bundle(&path, &definition).unwrap();
    let record =
        GraphRunRecord::create_with_id(GraphSnapshot::admit(definition).unwrap(), json!({}), id)
            .unwrap();
    application
        .kernel_runs(true)
        .unwrap()
        .unwrap()
        .create(&record, 100)
        .unwrap();
    metadata::save(
        &application.data_root,
        &RunMetadata::child(id.into(), graph.into(), record.graph_digest.clone(), &path).unwrap(),
    )
    .unwrap();
    record
}

fn nodes() -> Scripted {
    Scripted {
        seed: 0,
        inject_failures: false,
        trace: Mutex::default(),
        calls: ScriptedCalls::default(),
    }
}

fn cancellation() -> anchor_runtime::Cancellation {
    Arc::new(AtomicBool::new(false))
}

async fn complete(application: &RunApplication, workspace: &Path, id: &str) -> GraphRunRecord {
    let runs = application.kernel_runs(false).unwrap().unwrap();
    let artifacts =
        crate::HostArtifacts::new(application.data_root.join("artifacts"), workspace.into());
    let record = application.load_run(id).unwrap().unwrap();
    let key = InvocationKey {
        run_id: id.into(),
        graph_digest: record.graph_digest,
        node_id: "work".into(),
        invocation: 1,
    };
    std::fs::create_dir_all(artifacts.workspace_path(&key).unwrap()).unwrap();
    let completion = crate::application::run_access::fixtures::completion(json!({}));
    let commit = artifacts.freeze(&key, &completion).await.unwrap();
    crate::application::run_access::fixtures::complete(
        application,
        id,
        "work",
        completion,
        Some(commit),
    );
    let resource = runs.load(id).unwrap().unwrap();
    assert_eq!(
        resource.phase(),
        Some(&Phase::Settled {
            result: Outcome::Done
        })
    );
    KernelRuns::record(&resource).unwrap()
}

fn conversation(application: &RunApplication, id: &str, previous: Option<&str>) {
    let mut metadata = application.metadata(id).unwrap().unwrap();
    metadata.conversation = Some(ConversationSource {
        session: "delete-session".into(),
        reply_node: "work".into(),
        previous_run: previous.map(str::to_owned),
    });
    metadata::save(&application.data_root, &metadata).unwrap();
}

// Copied from api::tests::graph_delete_removes_only_its_run_data_and_preserves_other_graph_runs,
// with the Kernel scheduler and authority as the Run path.
#[tokio::test]
async fn kernel_graph_delete_removes_only_its_run_data_and_preserves_other_graph_runs() {
    let (_root, application, workspace) = fixture();
    create(&application, "target", "target-run");
    let target = complete(&application, &workspace, "target-run").await;
    create(&application, "grandchild", "grandchild-run");
    let mut child_metadata = application.metadata("grandchild-run").unwrap().unwrap();
    child_metadata.graph_call = Some(metadata::GraphCallSource {
        parent_run: "target-run".into(),
        parent_graph: "target".into(),
        parent_graph_digest: target.graph_digest.clone(),
        node: "work".into(),
        invocation: 1,
        mode: "wait".into(),
        root_run: "target-run".into(),
    });
    metadata::save(&application.data_root, &child_metadata).unwrap();
    let target_workspace = workspace.join("target-run");
    std::fs::create_dir_all(&target_workspace).unwrap();
    std::fs::write(target_workspace.join("stale.txt"), b"x").unwrap();
    let key = &target.results["work"][0].key;
    let hash = format!("{:x}", Sha256::digest(key.durable_key().as_bytes()));
    let facts = application.data_root.join("io-harness/facts");
    let store = application.data_root.join("io-harness/store");
    std::fs::create_dir_all(&facts).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    for suffix in ["json", "model.json", "recovery-7.json"] {
        std::fs::write(facts.join(format!("np1-{hash}.{suffix}")), b"{}").unwrap();
    }
    std::fs::write(store.join(format!("np1-{hash}.sqlite3-wal")), b"x").unwrap();
    let recordings = store.join(format!("np1-{hash}.recordings"));
    std::fs::create_dir_all(recordings.join("00000000000000000001")).unwrap();
    std::fs::write(recordings.join("00000000000000000001/request.json"), b"{}").unwrap();
    let local = crate::local_inputs::fact_path(&application.data_root, "target-run");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::write(&local, b"{}").unwrap();
    let artifact = application
        .data_root
        .join("artifacts")
        .join(&target.results["work"][0].commit.id);
    assert!(artifact.exists());
    assert_eq!(
        application
            .delete_graph(
                "target",
                &application.catalog_root.join("target"),
                &workspace
            )
            .await
            .unwrap(),
        1
    );
    assert!(application.load_run("target-run").unwrap().is_none());
    assert!(application.metadata("target-run").unwrap().is_none());
    assert!(!target_workspace.exists());
    assert_eq!(
        application
            .delivery_listing(&target.results["work"][0].commit.id, &workspace)
            .unwrap()["artifact_availability"],
        "expired"
    );
    assert!(!recordings.exists());
    assert!(!local.exists());
    for suffix in ["json", "model.json", "recovery-7.json"] {
        assert!(!facts.join(format!("np1-{hash}.{suffix}")).exists());
    }
    assert!(!store.join(format!("np1-{hash}.sqlite3-wal")).exists());
    assert!(application.load_run("grandchild-run").unwrap().is_some());
    assert!(application.catalog_root.join("grandchild").exists());
    assert!(
        application
            .kernel_runs(false)
            .unwrap()
            .unwrap()
            .kernel
            .history(&run_resource_id("target-run"))
            .unwrap()
            .is_empty()
    );
}

// Copied from api::tests::graph_delete_is_rejected_while_its_own_run_is_unfinished.
#[tokio::test]
async fn kernel_graph_delete_is_rejected_while_its_own_run_is_unfinished() {
    let (_root, application, workspace) = fixture();
    create(&application, "target", "unfinished");
    let runs = application.kernel_runs(false).unwrap().unwrap();
    for reason in [None, Some("paused"), Some("budget"), Some("stopped")] {
        if let Some(reason) = reason {
            let resource = runs.load("unfinished").unwrap().unwrap();
            if resource.phase() != Some(&Phase::Executing) {
                runs.control("unfinished", "resume", None).unwrap();
            }
            runs.control("unfinished", "stop", Some(reason)).unwrap();
        }
        assert!(
            matches!(application.delete_graph("target", &application.catalog_root.join("target"), &workspace).await,
            Err(ApplicationError::Conflict(message)) if message.contains("unfinished Run"))
        );
        assert!(runs.load("unfinished").unwrap().is_some());
        assert!(application.metadata("unfinished").unwrap().is_some());
        assert!(application.catalog_root.join("target").exists());
    }
}

// Copied from graphs::tests::deletion::guarded_delete_rechecks_active_execution_even_with_a_terminal_record.
#[tokio::test]
async fn kernel_delete_rechecks_active_worker_even_with_a_settled_resource() {
    let (_root, application, workspace) = fixture();
    create(&application, "target", "draining");
    complete(&application, &workspace, "draining").await;
    let runs = application.kernel_runs(false).unwrap().unwrap();
    let port = nodes();
    let registered = runs.drive("draining", &Artifacts, &port, cancellation());
    assert!(runs.active("draining"));
    assert!(matches!(
        application.delete_run("draining", &workspace).await,
        Err(ApplicationError::Conflict(_))
    ));
    assert!(matches!(
        application
            .delete_graph(
                "target",
                &application.catalog_root.join("target"),
                &workspace
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    assert!(runs.load("draining").unwrap().is_some());
    drop(registered);
    application
        .delete_run("draining", &workspace)
        .await
        .unwrap();
    assert!(runs.load("draining").unwrap().is_none());
    assert!(application.metadata("draining").unwrap().is_none());
    assert!(
        !application
            .data_root
            .join("kernel-run-observations/draining.json")
            .exists()
    );
}

#[tokio::test]
async fn kernel_single_delete_checks_native_scope_before_releasing_workspace_or_facts() {
    let (_root, application, workspace) = fixture();
    let definition = json!({"objective":"Run-owned native conversation","entry":"work",
        "agents":{"worker":{"model":"fixture-goose"}},
        "nodes":[{"id":"work","agent":"worker"}],"edges":[]});
    let workspaces = crate::workspaces::Workspaces::new(workspace.clone());
    let process = workspace.join(".goose-process");
    let mut owned = Vec::new();
    for id in ["target-run", "other-run"] {
        let record = create_definition(&application, "target", id, definition.clone());
        let key = InvocationKey {
            run_id: id.into(),
            graph_digest: record.graph_digest,
            node_id: "work".into(),
            invocation: 1,
        };
        application
            .ensure_node_workspace_binding(&key, &workspaces)
            .unwrap();
        std::fs::write(workspaces.path(&key).unwrap().join("draft.txt"), id).unwrap();
        let metadata = application.metadata(id).unwrap().unwrap();
        assert!(metadata.conversation.is_none());
        let scope = format!("gc1-{}", metadata.node_conversation_key("work"));
        let directory = process.join("conversations").join(&scope);
        std::fs::create_dir_all(&directory).unwrap();
        let binding = json!({"version":1,"scope":scope,"binary_sha256":"fixture-binary",
            "model_binding":"fixture-model","session_id":"fixture-session","latest":key});
        std::fs::write(
            directory.join("scope.json"),
            serde_json::to_vec(&binding).unwrap(),
        )
        .unwrap();
        let fact = application
            .data_root
            .join("goose-acp")
            .join(format!("{}.json", crate::host_fs::invocation_stem(&key)));
        std::fs::create_dir_all(fact.parent().unwrap()).unwrap();
        std::fs::write(
            &fact,
            serde_json::to_vec(&json!({
                "version":2,"key":key,"binary_sha256":"fixture-binary",
                "model_binding":"fixture-model","session_id":"fixture-session",
                "conversation_scope":scope,"completion":null,"reason":null,
                "tool_observation":null
            }))
            .unwrap(),
        )
        .unwrap();
        owned.push((key, scope, directory, fact));
    }
    complete(&application, &workspace, "target-run").await;
    let (target, scope, target_scope, target_fact) = &owned[0];
    let (other, _, other_scope, other_fact) = &owned[1];
    let target_workspace = workspaces.path(target).unwrap();
    let other_workspace = workspaces.path(other).unwrap();
    let binding = application
        .node_workspace_binding("target-run", "work")
        .unwrap();
    let resource_id = binding["binding"]["resource_id"].as_str().unwrap();
    let resource_fact_id = format!("workspaces/{resource_id}");
    let resource = application.file_fact(&resource_fact_id).unwrap();
    let target_model = std::fs::read(target_fact).unwrap();
    let other_model = std::fs::read(other_fact).unwrap();
    let other_native = std::fs::read(other_scope.join("scope.json")).unwrap();
    let locks = process.join("conversation-locks");
    std::fs::create_dir_all(&locks).unwrap();
    let lease = anchor_file_lease::FileLease::acquire(
        std::fs::File::create(locks.join(format!("{scope}.lock"))).unwrap(),
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .unwrap();

    assert!(matches!(
        application.delete_run("target-run", &workspace).await,
        Err(ApplicationError::Storage(message)) if message.contains("already active")
    ));
    assert_eq!(
        application
            .node_workspace_binding("target-run", "work")
            .unwrap(),
        binding
    );
    assert_eq!(application.file_fact(&resource_fact_id).unwrap(), resource);
    assert!(workspaces.is_bound(target).unwrap());
    assert_eq!(workspaces.resource(target).unwrap(), resource_id);
    assert_eq!(
        std::fs::read(target_workspace.join("draft.txt")).unwrap(),
        b"target-run"
    );
    assert_eq!(std::fs::read(target_fact).unwrap(), target_model);
    assert!(target_scope.exists());
    assert!(application.load_run("target-run").unwrap().is_some());
    assert!(application.metadata("target-run").unwrap().is_some());

    drop(lease);
    application
        .delete_run("target-run", &workspace)
        .await
        .unwrap();
    assert!(!target_scope.exists());
    assert!(!target_fact.exists());
    assert!(!target_workspace.exists());
    assert!(application.load_run("target-run").unwrap().is_none());
    assert!(application.metadata("target-run").unwrap().is_none());
    assert_eq!(
        std::fs::read(other_scope.join("scope.json")).unwrap(),
        other_native
    );
    assert_eq!(std::fs::read(other_fact).unwrap(), other_model);
    assert_eq!(
        std::fs::read(other_workspace.join("draft.txt")).unwrap(),
        b"other-run"
    );
    assert!(workspaces.is_bound(other).unwrap());
    assert!(application.load_run("other-run").unwrap().is_some());
}

#[tokio::test]
async fn kernel_delete_reservation_blocks_control_and_queued_worker_and_releases_after_error() {
    let (_root, application, _workspace) = fixture();
    let record = create(&application, "target", "reserved");
    let runs = application.kernel_runs(false).unwrap().unwrap();
    let ids = vec!["reserved".into()];
    let deletion = runs.reserve_deletion(&ids).unwrap();
    let nodes = nodes();
    assert!(matches!(
        runs.control("reserved", "stop", None),
        Err(ApplicationError::Conflict(_))
    ));
    assert!(matches!(
        runs.create(&record, 100),
        Err(ApplicationError::Conflict(_))
    ));
    assert!(matches!(
        runs.drive("reserved", &Artifacts, &nodes, cancellation())
            .await,
        Err(GraphError::RunBusy(_))
    ));
    assert!(nodes.trace.lock().unwrap().is_empty());
    drop(deletion);
    runs.control("reserved", "stop", None).unwrap();
    let deletion = runs.reserve_deletion(&ids).unwrap();
    deletion.remove("reserved").unwrap();
    drop(deletion);
    assert!(matches!(
        runs.drive("reserved", &Artifacts, &nodes, cancellation())
            .await,
        Err(GraphError::CorruptRun(_))
    ));
    assert!(nodes.trace.lock().unwrap().is_empty());
    assert!(!runs.active("reserved"));
}

// Copied from graphs::tests::deletion::guarded_delete_preserves_pending_session_delivery.
#[tokio::test]
async fn kernel_delete_preserves_pending_session_delivery() {
    let (_root, application, workspace) = fixture();
    create(&application, "target", "pending");
    complete(&application, &workspace, "pending").await;
    let mut metadata = application.metadata("pending").unwrap().unwrap();
    metadata.session_call = Some(SessionCall {
        context: SessionContext {
            session: "session".into(),
            reply_node: "work".into(),
            conversation_id: "conversation".into(),
            channel: json!({}),
        },
        status: "pending".into(),
        error: String::new(),
    });
    metadata::save(&application.data_root, &metadata).unwrap();
    for result in [
        application.delete_run("pending", &workspace).await,
        application
            .delete_graph(
                "target",
                &application.catalog_root.join("target"),
                &workspace,
            )
            .await
            .map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(ApplicationError::Conflict(message)) if message.contains("pending Session"))
        );
    }
    assert!(application.load_run("pending").unwrap().is_some());
    metadata.session_call.as_mut().unwrap().status = "completed".into();
    metadata::save(&application.data_root, &metadata).unwrap();
    application.delete_run("pending", &workspace).await.unwrap();
    assert!(application.load_run("pending").unwrap().is_none());
}

#[tokio::test]
async fn kernel_single_delete_preserves_call_owner_and_child_references() {
    let (_root, application, workspace) = fixture();
    let parent_path = application.catalog_root.join("parent");
    let definition = json!({"objective":"parent call","entry":"work","agents":{},
        "ops":{"work":{"call":{"graph":"child","mode":"wait"}}},
        "nodes":[{"id":"work","op":"work"}],"edges":[]});
    crate::application::graphs::write_graph_bundle(&parent_path, &definition).unwrap();
    let parent = GraphRunRecord::create_with_id(
        GraphSnapshot::admit(definition).unwrap(),
        json!({}),
        "parent",
    )
    .unwrap();
    crate::application::run_access::fixtures::create(&application, &parent);
    metadata::save(
        &application.data_root,
        &RunMetadata::child(
            "parent".into(),
            "parent".into(),
            parent.graph_digest.clone(),
            &parent_path,
        )
        .unwrap(),
    )
    .unwrap();
    create(&application, "child", "child");
    crate::application::run_access::fixtures::bind_child(&application, "parent", "work", "child");
    let before_parent = application.load_run("parent").unwrap().unwrap();
    let before_child = application.load_run("child").unwrap().unwrap();
    let call = before_parent.graph_calls.values().next().unwrap();
    assert_eq!(call.status, GraphCallStatus::Waiting);
    assert_eq!(call.child_run_id.as_deref(), Some("child"));
    for id in ["parent", "child"] {
        assert!(matches!(application.delete_run(id, &workspace).await,
            Err(ApplicationError::Conflict(message)) if message.contains("Graph call")));
        assert!(application.load_run(id).unwrap().is_some());
        assert_eq!(
            application.load_run("parent").unwrap().unwrap(),
            before_parent
        );
        assert_eq!(
            application.load_run("child").unwrap().unwrap(),
            before_child
        );
    }
}

// Copied from api::tests::conversations::conversation_lineage_deletes_any_link_once_nothing_newer_reads_it.
#[tokio::test]
async fn kernel_conversation_delete_retains_reader_then_allows_a_settled_successor() {
    let (_root, application, workspace) = fixture();
    create(&application, "target", "first");
    complete(&application, &workspace, "first").await;
    conversation(&application, "first", None);
    create(&application, "target", "second");
    conversation(&application, "second", Some("first"));
    assert!(matches!(application.delete_run("first", &workspace).await,
        Err(ApplicationError::Conflict(message)) if message.contains("still inherits")));
    complete(&application, &workspace, "second").await;
    application.delete_run("first", &workspace).await.unwrap();
    assert!(application.load_run("first").unwrap().is_none());
    assert!(application.load_run("second").unwrap().is_some());
    assert!(
        crate::run_deletions::load(&application.data_root, "first")
            .unwrap()
            .is_some()
    );
    application.delete_run("second", &workspace).await.unwrap();
}

#[tokio::test]
async fn kernel_graph_conversation_delete_rechecks_wait_child_phase_and_worker() {
    let (_root, application, workspace) = fixture();
    create(&application, "target", "conversation");
    conversation(&application, "conversation", None);
    let runs = application.kernel_runs(false).unwrap().unwrap();
    runs.control("conversation", "stop", None).unwrap();
    create(&application, "child", "wait-child");
    let mut child = application.metadata("wait-child").unwrap().unwrap();
    child.graph_call = Some(metadata::GraphCallSource {
        parent_run: "conversation".into(),
        parent_graph: "target".into(),
        parent_graph_digest: application
            .load_run("conversation")
            .unwrap()
            .unwrap()
            .graph_digest,
        node: "work".into(),
        invocation: 1,
        mode: "wait".into(),
        root_run: "conversation".into(),
    });
    metadata::save(&application.data_root, &child).unwrap();
    assert!(
        matches!(application.delete_graph("target", &application.catalog_root.join("target"), &workspace).await,
        Err(ApplicationError::Conflict(message)) if message.contains("has not stopped"))
    );
    complete(&application, &workspace, "wait-child").await;
    let port = nodes();
    let registered = runs.drive("wait-child", &Artifacts, &port, cancellation());
    assert!(runs.active("wait-child"));
    assert!(
        matches!(application.delete_graph("target", &application.catalog_root.join("target"), &workspace).await,
            Err(ApplicationError::Conflict(message)) if message.contains("has not stopped"))
    );
    drop(registered);
    assert_eq!(
        application
            .delete_graph(
                "target",
                &application.catalog_root.join("target"),
                &workspace
            )
            .await
            .unwrap(),
        1
    );
    assert!(application.load_run("wait-child").unwrap().is_some());
}

#[tokio::test]
async fn kernel_single_delete_rechecks_frozen_artifact_and_recovers_after_reopen() {
    let (_root, application, workspace) = fixture();
    create(&application, "target", "artifact-run");
    let record = complete(&application, &workspace, "artifact-run").await;
    let artifact = application
        .data_root
        .join("artifacts")
        .join(&record.results["work"][0].commit.id);
    let manifest = artifact.join("manifest.json");
    let original = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"invalid artifact").unwrap();
    assert!(matches!(
        application.delete_run("artifact-run", &workspace).await,
        Err(ApplicationError::Invalid(_))
    ));
    assert!(application.load_run("artifact-run").unwrap().is_some());
    assert!(artifact.exists());
    {
        let runs = application.kernel_runs(false).unwrap().unwrap();
        let reservation = runs.reserve_deletion(&["artifact-run".into()]).unwrap();
        drop(reservation);
    }
    std::fs::write(&manifest, original).unwrap();
    let data = application.data_root.clone();
    let catalog = application.catalog_root.clone();
    drop(application);
    let reopened = RunApplication::new(data.clone(), catalog.clone());
    reopened
        .delete_run("artifact-run", &workspace)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .delivery_listing(&record.results["work"][0].commit.id, &workspace)
            .unwrap()["artifact_availability"],
        "expired"
    );
    assert!(reopened.load_run("artifact-run").unwrap().is_none());
    drop(reopened);
    let reopened = RunApplication::new(data, catalog);
    assert!(reopened.load_run("artifact-run").unwrap().is_none());
    assert!(reopened.metadata("artifact-run").unwrap().is_none());
    assert!(matches!(
        reopened.delete_run("artifact-run", &workspace).await,
        Err(ApplicationError::Missing)
    ));
}

#[tokio::test]
async fn kernel_graph_conversation_delete_preserves_pause_and_budget_waits() {
    let (_root, application, workspace) = fixture();
    create(&application, "target", "held-turn");
    conversation(&application, "held-turn", None);
    let runs = application.kernel_runs(false).unwrap().unwrap();
    for reason in ["paused", "budget"] {
        runs.control("held-turn", "stop", Some(reason)).unwrap();
        assert!(
            matches!(application.delete_graph("target", &application.catalog_root.join("target"), &workspace).await,
            Err(ApplicationError::Conflict(message)) if message.contains("has not stopped"))
        );
        assert!(runs.load("held-turn").unwrap().is_some());
        runs.control("held-turn", "resume", None).unwrap();
    }
    runs.control("held-turn", "stop", None).unwrap();
    assert_eq!(
        application
            .delete_graph(
                "target",
                &application.catalog_root.join("target"),
                &workspace
            )
            .await
            .unwrap(),
        1
    );
    assert!(runs.load("held-turn").unwrap().is_none());
}
