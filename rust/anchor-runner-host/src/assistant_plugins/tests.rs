use super::*;
use serde_json::json;

fn graph() -> GraphSnapshot {
    GraphSnapshot::from_authoring(json!({
        "objective":"assistant", "entry":"wait",
        "agents":{"worker":{"model":"fixture","instructions":"keep history","network":false}},
        "ops":{"wait":{"host":{"operation":"session.wait_input"}},"reply":{"host":{"operation":"session.reply"}}},
        "nodes":[{"id":"wait","op":"wait"},{"id":"agent","agent":"worker","plugins":[]},{"id":"reply","op":"reply"}],
        "edges":[{"from":"wait","to":"agent"},{"from":"agent","to":"reply"},{"from":"reply","to":"wait"}]
    })).unwrap()
}

#[test]
fn only_work_plugins_and_network_are_hot() {
    let original = graph();
    let mut changed = original.clone();
    changed.nodes[1]
        .plugins
        .push(PluginRef::required("docmost"));
    changed.agents.get_mut("worker").unwrap().network = true;
    assert!(compatible(&original, &changed, "agent"));
    changed.nodes[0].plugins.push(PluginRef::required("other"));
    assert!(!compatible(&original, &changed, "agent"));
    let mut changed = original.clone();
    changed.agents.get_mut("worker").unwrap().instructions = "new".into();
    assert!(!compatible(&original, &changed, "agent"));
    let mut changed = original.clone();
    changed.edges.pop();
    assert!(!compatible(&original, &changed, "agent"));
}

#[test]
fn corrupt_or_foreign_turn_policy_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let path = turn_path(root.path(), "run", "session", "turn");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{bad json").unwrap();
    assert!(load(root.path(), "run", "session", "turn").is_err());
    std::fs::write(
        &path,
        serde_json::to_vec(&TurnPluginPolicy {
            format: 1,
            run: "other-run".into(),
            node: "agent".into(),
            graph_digest: "digest".into(),
            mounts: crate::plugin_mounts::MountSnapshot::new("session", "turn", &BTreeMap::new()),
            declarations: vec![],
            bindings: BTreeMap::new(),
            network: false,
            reasons: vec![],
            by_declaration: vec![],
            by_switch: vec![],
            task_grants: None,
        })
        .unwrap(),
    )
    .unwrap();
    assert!(load(root.path(), "run", "session", "turn").is_err());
}

#[test]
fn operator_approval_refreshes_authority_and_stays_scoped_to_bundle_and_graph_digest() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("graph");
    let library = root.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(library.join("plugins/demo")).unwrap();
    std::fs::write(
        library.join("plugins/demo/plugin.json"),
        r#"{"name":"demo"}"#,
    )
    .unwrap();
    let mut snapshot = graph();
    snapshot.nodes[1].plugins.push(PluginRef::required("demo"));
    publish(root.path(), &source, &snapshot, &library).unwrap();
    publish(root.path(), &source, &snapshot, &library).unwrap();
    let digest = snapshot.digest().unwrap();
    assert!(
        publication(root.path(), &source, &digest)
            .unwrap()
            .unwrap()
            .bindings
            .contains_key("demo")
    );
    assert!(
        publication(root.path(), &source, "another-digest")
            .unwrap()
            .is_none()
    );
    assert!(
        publication(root.path(), &root.path().join("another-graph"), &digest)
            .unwrap()
            .is_none()
    );
    // Ordinary code/description upgrades retain the approved Host authority.
    std::fs::write(
        library.join("plugins/demo/plugin.json"),
        r#"{"name":"demo","description":"new code"}"#,
    )
    .unwrap();
    publish(root.path(), &source, &snapshot, &library).unwrap();
    std::fs::write(
        library.join("plugins/demo/plugin.json"),
        r#"{"name":"demo","network":true}"#,
    )
    .unwrap();
    let previous = publication(root.path(), &source, &digest).unwrap().unwrap();
    publish(root.path(), &source, &snapshot, &library).unwrap();
    let refreshed = publication(root.path(), &source, &digest).unwrap().unwrap();
    assert_eq!(refreshed.graph_digest, previous.graph_digest);
    assert!(!previous.bindings["demo"].same_authorization(&refreshed.bindings["demo"]));
    publish(root.path(), &source, &snapshot, &library).unwrap();
    assert_eq!(
        publication(root.path(), &source, &digest).unwrap().unwrap(),
        refreshed
    );
}

fn owned_policy(root: &Path, run: &str) -> GraphRunRecord {
    let snapshot = graph();
    let record = GraphRunRecord::create_with_id(snapshot, serde_json::Value::Null, run).unwrap();
    let source = root.join("graph");
    std::fs::create_dir_all(&source).unwrap();
    let mut metadata = RunMetadata::new(
        run.into(),
        "assistant".into(),
        record.graph_digest.clone(),
        &source,
    )
    .unwrap();
    metadata.assistant = Some(crate::run_metadata::AssistantSource {
        owner: "owner".into(),
        session: "session".into(),
        wait_node: "wait".into(),
        work_node: "agent".into(),
        reply_node: "reply".into(),
    });
    crate::run_metadata::save(root, &metadata).unwrap();
    let policy = TurnPluginPolicy {
        format: 1,
        run: run.into(),
        node: "agent".into(),
        graph_digest: record.graph_digest.clone(),
        mounts: crate::plugin_mounts::MountSnapshot::new("session", "turn", &BTreeMap::new()),
        declarations: vec![],
        bindings: BTreeMap::new(),
        network: false,
        reasons: vec![],
        by_declaration: vec![],
        by_switch: vec![],
        task_grants: None,
    };
    crate::assistant::save_immutable(&turn_path(root, run, "session", "turn"), &policy).unwrap();
    mark_initialized(root, run).unwrap();
    record
}

#[test]
fn cleanup_only_removes_owned_policy_and_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let first = owned_policy(root.path(), "first");
    let second = owned_policy(root.path(), "second");
    cleanup(root.path(), &first).unwrap();
    cleanup(root.path(), &first).unwrap();
    assert!(
        load(root.path(), "first", "session", "turn")
            .unwrap()
            .is_none()
    );
    assert!(!initialized(root.path(), "first").unwrap());
    assert!(
        load(root.path(), "second", "session", "turn")
            .unwrap()
            .is_some()
    );
    assert!(initialized(root.path(), "second").unwrap());
    cleanup(root.path(), &second).unwrap();
}

#[test]
fn cleanup_refuses_symlinks_and_corrupt_facts_without_deleting_neighbors() {
    let root = tempfile::tempdir().unwrap();
    let record = owned_policy(root.path(), "first");
    let neighbor = owned_policy(root.path(), "neighbor");
    let path = turn_path(root.path(), "first", "session", "turn");
    let original = std::fs::read(&path).unwrap();
    let external = root.path().join("external.json");
    std::fs::write(&external, &original).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&external, &path).unwrap();
    assert!(cleanup(root.path(), &record).is_err());
    assert_eq!(std::fs::read(&external).unwrap(), original);
    assert!(initialized(root.path(), "first").unwrap());
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "{malformed").unwrap();
    assert!(cleanup(root.path(), &record).is_err());
    assert!(initialized(root.path(), "first").unwrap());
    assert!(
        load(root.path(), &neighbor.run_id, "session", "turn")
            .unwrap()
            .is_some()
    );
    std::fs::write(&path, &original).unwrap();
    cleanup(root.path(), &record).unwrap();
}

#[test]
fn simultaneous_switch_snapshots_all_observe_the_first_durable_writer() {
    use std::sync::{Arc, Barrier};
    struct Switches(BTreeMap<String, bool>);
    impl crate::plugin_mounts::PluginMountSource for Switches {
        fn session_mounts(&self, _: &str) -> Result<BTreeMap<String, bool>, String> {
            Ok(self.0.clone())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let writers = (0..8)
        .map(|index| {
            let root = root.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                crate::node_host::freeze_session_switches(
                    &root,
                    "session",
                    "turn",
                    &Switches(BTreeMap::from([(format!("candidate-{index}"), true)])),
                )
                .unwrap()
                .snapshot
            })
        })
        .collect::<Vec<_>>();
    let snapshots = writers
        .into_iter()
        .map(|writer| writer.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(snapshots.len(), 8);
    assert!(snapshots.iter().all(|snapshot| snapshot == &snapshots[0]));
}
