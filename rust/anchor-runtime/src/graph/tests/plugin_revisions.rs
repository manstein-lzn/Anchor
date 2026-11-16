use super::*;
use serde_json::json;

fn snapshot(reference: Value) -> Value {
    json!({
        "objective":"revision fixture", "entry":"work", "agents":{"worker":{}},
        "nodes":[{"id":"work", "agent":"worker", "plugins":[reference]}], "edges":[]
    })
}

fn binding(digest: &str) -> PluginBinding {
    PluginBinding {
        id: "demo".into(),
        digest: digest.into(),
        resources: vec!["plugin.json".into()],
        mcp_servers: Vec::new(),
        authorization_digest: "fixture-authority".try_into().unwrap(),
    }
}

#[test]
fn absent_revision_preserves_all_old_reference_shapes_and_digests() {
    for reference in [
        json!("demo"),
        json!({"id":"demo", "required":true}),
        json!({"id":"demo", "revision":null}),
    ] {
        let parsed: PluginRef = serde_json::from_value(reference).unwrap();
        assert_eq!(parsed, PluginRef::required("demo"));
        assert_eq!(serde_json::to_value(parsed).unwrap(), json!("demo"));
    }
    let reference = json!({"id":"demo", "required":false, "default":true});
    let parsed: PluginRef = serde_json::from_value(reference.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), reference);
}

#[test]
fn exact_revisions_roundtrip_and_invalid_aliases_fail_in_both_admission_paths() {
    let revision = "ab".repeat(32);
    let reference = json!({"id":"demo", "revision":revision});
    let admitted = GraphSnapshot::admit(snapshot(reference.clone())).unwrap();
    let authored = GraphSnapshot::from_authoring(snapshot(reference)).unwrap();
    assert_eq!(admitted.nodes[0].plugins, authored.nodes[0].plugins);
    assert_eq!(admitted.plugin_revision("demo"), Some(revision.as_str()));
    let written = serde_json::to_value(&admitted).unwrap();
    assert_eq!(written["nodes"][0]["plugins"][0]["revision"], revision);
    assert_eq!(GraphSnapshot::admit(written).unwrap(), admitted);
    assert_ne!(
        admitted.digest().unwrap(),
        GraphSnapshot::admit(snapshot(json!("demo")))
            .unwrap()
            .digest()
            .unwrap()
    );
    for invalid in [
        json!(""),
        json!("latest"),
        json!("a".repeat(63)),
        json!("a".repeat(65)),
        json!("AB".repeat(32)),
        json!(format!("../{}", "a".repeat(64))),
        json!(1),
    ] {
        let value = snapshot(json!({"id":"demo", "revision":invalid}));
        assert!(GraphSnapshot::admit(value.clone()).is_err(), "{invalid}");
        assert!(GraphSnapshot::from_authoring(value).is_err(), "{invalid}");
    }
}

#[test]
fn one_graph_cannot_silently_mix_sources_for_one_plugin_id() {
    for other in [
        json!("demo"),
        json!({"id":"demo", "revision":"b".repeat(64)}),
    ] {
        let mut value = snapshot(json!({"id":"demo", "revision":"a".repeat(64)}));
        value["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"other", "agent":"worker", "plugins":[other]}));
        assert!(GraphSnapshot::admit(value.clone()).is_err());
        assert!(GraphSnapshot::from_authoring(value).is_err());
    }
}

#[test]
fn admission_and_saved_records_bind_the_exact_revision() {
    let revision = "a".repeat(64);
    let snapshot =
        GraphSnapshot::admit(snapshot(json!({"id":"demo", "revision":revision}))).unwrap();
    assert!(
        GraphRunRecord::create_with_plugins(
            snapshot.clone(),
            Value::Null,
            "wrong",
            vec![binding(&"b".repeat(64))]
        )
        .is_err()
    );
    let mut record = GraphRunRecord::create_with_plugins(
        snapshot,
        Value::Null,
        "matching",
        vec![binding(&revision)],
    )
    .unwrap();
    record.validate().unwrap();
    record.plugin_bindings.get_mut("demo").unwrap().digest = "b".repeat(64);
    assert!(record.validate().is_err());
}

#[tokio::test]
async fn default_dispatch_never_substitutes_current_code_for_a_pin() {
    let revision = "a".repeat(64);
    let snapshot =
        GraphSnapshot::admit(snapshot(json!({"id":"demo", "revision":revision}))).unwrap();
    let record = GraphRunRecord::create_with_plugins(
        snapshot,
        Value::Null,
        "pinned-dispatch",
        vec![binding(&revision)],
    )
    .unwrap();
    let nodes = FakeNodes {
        resolved_plugins: Mutex::new(vec![binding(&"b".repeat(64))]),
        ..Default::default()
    };
    let harness = Harness::memory(record);
    harness
        .run(&MemoryArtifacts::default(), &nodes)
        .await
        .unwrap();
    assert!(nodes.calls.lock().unwrap().is_empty());
    assert!(matches!(
        harness.phase(),
        Phase::Settled {
            result: Outcome::Failed { .. }
        }
    ));
}
