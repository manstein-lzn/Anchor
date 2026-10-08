use super::*;
use anchor_runtime::graph::PluginBinding;

#[tokio::test]
async fn run_plugins_retain_frozen_identity_order_and_node_scope_after_library_drift() {
    let (root, state) = fixture();
    let snapshot = GraphSnapshot::admit(json!({
        "objective":"frozen Plugin facts",
        "entry":"module.first",
        "agents":{"worker":{}},
        "ops":{"verify":{"run":"true"}},
        "nodes":[
            {"id":"module.first","agent":"worker","plugins":["beta","alpha"]},
            {"id":"module.second","agent":"worker","plugins":["alpha"]},
            {"id":"verify","op":"verify"}
        ],
        "edges":[
            {"from":"module.first","to":"module.second"},
            {"from":"module.second","to":"verify"}
        ]
    }))
    .unwrap();
    let mut record = GraphRunRecord::create_with_id(snapshot, json!({}), "plugin-history").unwrap();
    record.status = RunStatus::Completed;
    record.plugin_bindings_initialized = true;
    for (id, digest) in [("alpha", "a".repeat(64)), ("beta", "b".repeat(64))] {
        record.plugin_bindings.insert(
            id.into(),
            PluginBinding {
                id: id.into(),
                digest,
                resources: vec!["resources/input.txt".into()],
                mcp_servers: Vec::new(),
            },
        );
    }
    let store = FileRunStore::new(state.data_root.join("runs"));
    store.save(&record).unwrap();
    metadata::save(
        &state.data_root,
        &RunMetadata::new(
            record.run_id.clone(),
            "fixture".into(),
            record.graph_digest.clone(),
            &state.bundle_root,
        )
        .unwrap(),
    )
    .unwrap();
    let expected = json!({
        "module.first":[
            {"id":"beta","name":"beta","description":"","digest":"b".repeat(64)},
            {"id":"alpha","name":"alpha","description":"","digest":"a".repeat(64)}
        ],
        "module.second":[
            {"id":"alpha","name":"alpha","description":"","digest":"a".repeat(64)}
        ],
        "verify":[]
    });
    let app = router(state.clone());
    let (status, initial) = call(app, "GET", "/runs/plugin-history", None).await;
    assert_eq!(status, StatusCode::OK, "{initial}");
    assert_eq!(initial["plugins"], expected);
    let library = root.path().join("library/plugins/alpha/resources");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(library.join("input.txt"), "changed after Run").unwrap();
    std::fs::write(state.bundle_root.join("graph.json"), "{}").unwrap();
    let app = router(state);
    let (status, reopened) = call(app, "GET", "/runs/plugin-history", None).await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert_eq!(reopened["plugins"], expected);
    assert_eq!(store.load(&record.run_id).unwrap().unwrap(), record);
}
