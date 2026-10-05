use super::*;
use serde_json::json;

fn record(id: &str) -> GraphRunRecord {
    GraphRunRecord::create_with_id(
        GraphSnapshot::admit(json!({
            "objective":"local input authorization", "entry":"collect", "ops":{"run":{"run":"true"}},
            "nodes":[{"id":"collect","op":"run"},{"id":"other","op":"run"}],
            "edges":[{"from":"collect","to":"other"}]
        }))
        .unwrap(),
        json!({}),
        id,
    )
    .unwrap()
}

fn key(record: &GraphRunRecord, node: &str) -> InvocationKey {
    InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: node.into(),
        invocation: 1,
    }
}

fn grants(root: &Path, graph: &str, value: serde_json::Value) {
    fs::create_dir_all(root.join(graph)).unwrap();
    fs::write(
        root.join(graph).join("local-inputs.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
}

#[test]
fn named_grants_are_node_scoped_frozen_and_normalized() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("operator-workspaces");
    let input = root.path().join("input");
    fs::create_dir(&config).unwrap();
    fs::create_dir(&input).unwrap();
    let record = record("selected");
    grants(&config, "weekly", json!({"collect":{"history":input}}));
    let local = LocalInputs::new(root.path().join("state"), Some(config.clone())).unwrap();
    local
        .freeze(
            &record.run_id,
            &record.graph_digest,
            &record.snapshot,
            Some("weekly"),
            true,
        )
        .unwrap();
    assert_eq!(
        local.mounts(&record, &key(&record, "collect")).unwrap(),
        vec![ReadOnlyInput::new(&input, "/local-inputs/history")]
    );
    assert!(
        local
            .mounts(&record, &key(&record, "other"))
            .unwrap()
            .is_empty()
    );
    assert!(local.mounts(&record, &key(&record, "unknown")).is_err());
    let mut mismatched = key(&record, "collect");
    mismatched.graph_digest = "different".into();
    assert!(local.mounts(&record, &mismatched).is_err());
    let mut cross_run = key(&record, "collect");
    cross_run.run_id = "other-run".into();
    assert!(local.mounts(&record, &cross_run).is_err());

    fs::write(
        config.join("weekly/local-inputs.json"),
        format!(
            "{{ \"collect\": {{ \"history\": {:?} }} }}",
            input.join(".").to_str().unwrap()
        ),
    )
    .unwrap();
    local.verify(&record).unwrap();
    grants(&config, "weekly", json!({"other":{"history":input}}));
    assert!(local.verify(&record).unwrap_err().contains("grants"));
    assert!(local.mounts(&record, &key(&record, "collect")).is_err());
}

#[test]
fn empty_authority_is_frozen_and_source_changes_never_add_grants_on_resume() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let config = root.path().join("operator-workspaces");
    let input = root.path().join("input");
    fs::create_dir(&config).unwrap();
    fs::create_dir(&input).unwrap();
    let record = record("empty");
    let disabled = LocalInputs::new(state.clone(), None).unwrap();
    disabled
        .freeze(
            &record.run_id,
            &record.graph_digest,
            &record.snapshot,
            None,
            true,
        )
        .unwrap();
    assert!(fact_path(&state, &record.run_id).is_file());
    disabled.verify(&record).unwrap();
    grants(&config, "weekly", json!({"collect":{"history":input}}));
    let configured = LocalInputs::new(state.clone(), Some(config.clone())).unwrap();
    assert!(
        configured
            .freeze(
                &record.run_id,
                &record.graph_digest,
                &record.snapshot,
                Some("weekly"),
                false
            )
            .is_err()
    );

    let missing = super::tests::record("missing-file");
    configured
        .freeze(
            &missing.run_id,
            &missing.graph_digest,
            &missing.snapshot,
            Some("rsi"),
            true,
        )
        .unwrap();
    grants(&config, "rsi", json!({"collect":{"history":input}}));
    assert!(configured.verify(&missing).is_err());
    assert!(disabled.verify(&missing).is_err());
    let moved = root.path().join("other-workspaces");
    fs::create_dir(&moved).unwrap();
    grants(&moved, "weekly", json!({"collect":{"history":input}}));
    assert!(
        LocalInputs::new(state, Some(moved))
            .unwrap()
            .verify(&record)
            .is_err()
    );
}

#[test]
fn unknown_nodes_malformed_names_broad_paths_and_unfrozen_grants_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("operator-workspaces");
    let input = root.path().join("input");
    fs::create_dir(&config).unwrap();
    fs::create_dir(&input).unwrap();
    let local = LocalInputs::new(root.path().join("state"), Some(config.clone())).unwrap();
    let record = record("invalid");
    for value in [
        json!({"unknown":{"history":input}}),
        json!({"collect":{"../history":input}}),
        json!({"collect":{"history":"relative"}}),
        json!({"collect":{"history":"/root"}}),
        json!({"collect":{"history":"/proc/self"}}),
        json!({"collect":["history"]}),
        json!([]),
    ] {
        grants(&config, "weekly", value);
        assert!(
            local
                .freeze(
                    &record.run_id,
                    &record.graph_digest,
                    &record.snapshot,
                    Some("weekly"),
                    true
                )
                .is_err()
        );
        assert!(!fact_path(local.state_root(), &record.run_id).exists());
    }
    grants(&config, "weekly", json!({"collect":{"history":input}}));
    assert!(
        local
            .freeze(
                &record.run_id,
                &record.graph_digest,
                &record.snapshot,
                Some("weekly"),
                false
            )
            .unwrap_err()
            .contains("no frozen")
    );
}

#[test]
fn child_uses_its_own_graph_grants_even_with_the_same_node_id() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("operator-workspaces");
    let parent_input = root.path().join("parent-input");
    let child_input = root.path().join("child-input");
    fs::create_dir(&config).unwrap();
    fs::create_dir(&parent_input).unwrap();
    fs::create_dir(&child_input).unwrap();
    grants(
        &config,
        "parent",
        json!({"collect":{"source":parent_input}}),
    );
    grants(&config, "child", json!({"collect":{"source":child_input}}));
    let local = LocalInputs::new(root.path().join("state"), Some(config)).unwrap();
    let parent = record("parent-run");
    let child = record("child-run");
    for (record, graph) in [(&parent, "parent"), (&child, "child")] {
        local
            .freeze(
                &record.run_id,
                &record.graph_digest,
                &record.snapshot,
                Some(graph),
                true,
            )
            .unwrap();
    }
    assert_eq!(
        local.mounts(&parent, &key(&parent, "collect")).unwrap()[0].source,
        parent_input
    );
    assert_eq!(
        local.mounts(&child, &key(&child, "collect")).unwrap()[0].source,
        child_input
    );
    assert!(
        local
            .freeze(
                &child.run_id,
                &child.graph_digest,
                &child.snapshot,
                Some("parent"),
                false
            )
            .is_err()
    );
}
