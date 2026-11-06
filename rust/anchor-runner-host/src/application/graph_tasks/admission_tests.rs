use super::*;
use crate::execution::HostGraphCatalog;
use crate::plugin_library::{Catalog, PluginLibrary};
use crate::run_metadata::{self as metadata, RunMetadata};
use anchor_graph_host::{GraphCatalog, PluginCatalog};
use anchor_runtime::graph::CallIdentity;
use std::{
    future::Future,
    sync::{Arc, atomic::AtomicBool},
    task::Poll,
};

async fn fixture() -> (tempfile::TempDir, RunApplication, TaskOrigin) {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    for (id, network) in [("offline", false), ("network", true)] {
        let directory = application.library_root().join("plugins").join(id);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("plugin.json"),
            json!({"name":id,"network":network}).to_string(),
        )
        .unwrap();
    }
    let official = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/graph-tasks");
    let installed = application.library_root().join("plugins/graph-tasks");
    std::fs::create_dir_all(installed.join("skills/graph-tasks")).unwrap();
    std::fs::copy(official.join("plugin.json"), installed.join("plugin.json")).unwrap();
    std::fs::copy(
        official.join("skills/graph-tasks/SKILL.md"),
        installed.join("skills/graph-tasks/SKILL.md"),
    )
    .unwrap();
    let selected = json!({
        "entry":"work","agents":{"role":{"model":"fixture"}},
        "nodes":[{"id":"work","agent":"role","plugins":[{"id":"offline"}]}],"edges":[]
    });
    graphs::write_graph_bundle_from_catalog(
        &application.graph_bundle_path("selected"),
        &selected,
        application.library_root(),
    )
    .unwrap();
    let policy = GraphTaskPolicy {
        graphs: vec!["selected".into()],
        ..Default::default()
    };
    let source = json!({
        "entry":"work","agents":{"role":{"model":"fixture"}},
        "nodes":[{"id":"work","agent":"role","plugins":[{"id":"graph-tasks","graph_tasks":policy}]}],
        "edges":[]
    });
    graphs::write_graph_bundle_from_catalog(
        &application.graph_bundle_path("source"),
        &source,
        application.library_root(),
    )
    .unwrap();
    application.update_graph("source", source).await.unwrap();
    let (_, bundle) = application.load_graph("source").unwrap();
    let digest = bundle.snapshot.digest().unwrap();
    let publication = assistant_plugins::publication(
        &application.data_root,
        &application.graph_bundle_path("source"),
        &digest,
    )
    .unwrap()
    .unwrap();
    let origin = TaskOrigin {
        owner: "local".into(),
        session: None,
        graph: "source".into(),
        source: TaskOriginSource::AgentDelegation {
            key: InvocationKey {
                run_id: "source-run".into(),
                graph_digest: digest.clone(),
                node_id: "work".into(),
                invocation: 1,
            },
        },
        authorization_digest: digest,
        policy,
        bindings: publication.bindings,
        targets: publication.task_graphs,
    };
    application
        .authorize_delegated_graph(&origin, "selected")
        .unwrap();
    (root, application, origin)
}

fn expand_plugin(root: &Path, library: &PluginLibrary, lease: &anchor_library::InstallLease) {
    let replacement = root.join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    std::fs::write(
        replacement.join("plugin.json"),
        json!({"name":"offline","network":true}).to_string(),
    )
    .unwrap();
    library
        .install_directory_locked("offline", &replacement, true, lease)
        .unwrap();
}

#[tokio::test]
async fn delegated_admission_rechecks_authority_after_library_publication_wait() {
    let (root, application, origin) = fixture().await;
    let library = PluginLibrary::new(application.library_root());
    let lease = library.install_lease().unwrap();
    let mut admission = std::pin::pin!(application.admit_delegated_graph(
        &origin,
        "selected",
        json!({}),
        "publication-race".into(),
        RunTrigger::default(),
    ));
    // A single poll completes every synchronous precheck and reaches the held
    // publication lease before the installed authorization changes.
    std::future::poll_fn(|cx| {
        assert!(admission.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    expand_plugin(root.path(), &library, &lease);
    drop(lease);

    let error = admission.await.unwrap_err();
    assert!(
        format!("{error:?}")
            .contains("Plugin `offline` no longer has the delegated Host authorization"),
        "{error:?}"
    );
    assert!(application.records().unwrap().is_empty());
    assert!(metadata::ids(&application.data_root).unwrap().is_empty());
}

#[tokio::test]
async fn delegated_static_child_rechecks_authority_at_locked_persistence_boundary() {
    let (root, application, origin) = fixture().await;
    let key = origin.agent_key().unwrap();
    let parent = RunMetadata::new(
        key.run_id.clone(),
        origin.graph.clone(),
        key.graph_digest.clone(),
        &application.graph_bundle_path("source"),
    )
    .unwrap();
    metadata::save(&application.data_root, &parent).unwrap();
    let catalog = HostGraphCatalog {
        root: application.library_root().to_path_buf(),
        state: application.data_root.clone(),
        application: Some(application.clone()),
        task: Some(origin.clone()),
    };
    let _target = catalog.lock_admission("selected").unwrap();
    let bundle = catalog.bundle("selected").unwrap().unwrap();
    let library = PluginLibrary::new(application.library_root());
    let lease = library.install_lease().unwrap();
    let mut publication =
        std::pin::pin!(catalog.lock_plugin_references(Arc::new(AtomicBool::new(false))));
    std::future::poll_fn(|cx| {
        assert!(publication.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    expand_plugin(root.path(), &library, &lease);
    drop(lease);
    let _publication = publication.await.unwrap();
    let bindings = Catalog::new(application.library_root())
        .resolve(&bundle.plugins)
        .unwrap();
    assert!(!origin.bindings["offline"].same_authorization(&bindings[0]));
    let identity = CallIdentity {
        parent_run_id: key.run_id.clone(),
        parent_graph_digest: key.graph_digest.clone(),
        node_id: "call".into(),
        invocation: 1,
        call_spec_digest: "fixture".into(),
    };

    let error = catalog
        .record_child_admission(
            "call-expanded",
            "selected",
            &bundle.snapshot,
            &bindings,
            &identity,
            "wait",
        )
        .unwrap_err();
    assert!(
        format!("{error:?}")
            .contains("Plugin `offline` no longer has the delegated Host authorization"),
        "{error:?}"
    );
    assert!(application.records().unwrap().is_empty());
    assert_eq!(
        metadata::ids(&application.data_root).unwrap(),
        vec![key.run_id.clone()]
    );
}
