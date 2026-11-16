use super::*;
use anchor_kernel::{FileStore, Outcome, Reason, StoreError, Waiter};
use serde::{Deserialize, Serialize};
use std::{
    io::BufRead,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

include!("tests/fixtures.rs");
include!("tests/harness.rs");
mod host_operations;
mod invocation_authority;
mod plugin_revisions;
include!("tests/migrated_behavior.rs");
include!("tests/migrated_lifecycle.rs");
include!("tests/migrated_crash.rs");
include!("tests/migrated_storage.rs");
include!("tests/migrated_leases.rs");
include!("tests/migrated_plugins.rs");
include!("tests/migrated_reload.rs");
include!("tests/migrated_validation.rs");
include!("tests/migrated_scenarios.rs");

#[test]
fn host_assigned_run_identity_is_preserved_and_validated() {
    let snapshot = graph(&["one"], &[], "one");
    let record =
        GraphRunRecord::create_with_id(snapshot.clone(), Value::Null, "platform-run-42").unwrap();
    assert_eq!(record.run_id, "platform-run-42");
    assert_eq!(record.snapshot, snapshot);
    assert!(matches!(
        GraphRunRecord::create_with_id(snapshot, Value::Null, "../escape"),
        Err(GraphError::InvalidRunId(_))
    ));
}

#[test]
fn executed_nodes_follow_first_durable_completion_order() {
    let mut record = GraphRunRecord::create(
        graph(&["alpha", "beta", "gamma"], &[], "alpha"),
        Value::Null,
    )
    .unwrap();
    let make_result = |node: &str, sequence: u64| RunResult {
        plugin_views: Default::default(),
        plugin_digests: Default::default(),
        node_id: node.into(),
        interruption: None,
        key: InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: node.into(),
            invocation: sequence,
        },
        completion: NodeCompletion {
            submission: String::new(),
            route: None,
            model_requests: 0,
            output: Value::Null,
        },
        commit: CommitRef {
            id: format!("commit-{node}-{sequence}"),
            node_id: node.into(),
            invocation: sequence,
        },
        sequence,
    };
    record.results.insert(
        "alpha".into(),
        vec![make_result("alpha", 2), make_result("alpha", 4)],
    );
    record
        .results
        .insert("beta".into(), vec![make_result("beta", 1)]);
    record
        .results
        .insert("gamma".into(), vec![make_result("gamma", 3)]);
    assert_eq!(record.executed_nodes(), ["beta", "alpha", "gamma"]);
}

#[test]
fn inactive_scc_waits_for_external_incoming_decision() {
    let snapshot = graph(
        &["entry", "source", "cycle_a", "cycle_b"],
        &[
            ("entry", "source"),
            ("source", "cycle_a"),
            ("cycle_a", "cycle_b"),
            ("cycle_b", "cycle_a"),
        ],
        "entry",
    );
    let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    record.invocations.insert("entry".into(), 1);
    record.invocations.insert("source".into(), 1);
    record.passes.insert("entry".into(), 1);
    record.passes.insert("source".into(), 1);
    record.decided.insert(
        "entry|source".into(),
        EdgeDecision {
            selected: true,
            sequence: 1,
            source_invocation: 1,
            result_sequence: 1,
        },
    );

    assert!(!propagate_inactive_edges(&mut record).unwrap());
    assert_eq!(record.decided.len(), 1);
    assert!(!record.decided.contains_key("source|cycle_a"));
    assert!(!record.decided.contains_key("cycle_a|cycle_b"));
    assert!(!record.decided.contains_key("cycle_b|cycle_a"));
}

#[test]
fn false_ingress_closes_cascaded_sccs_deterministically() {
    // Deliberately list downstream components first. Closure must converge
    // independently of graph storage order and produce stable sequences.
    let snapshot = graph(
        &[
            "entry", "join", "second/a", "second/b", "first/a", "first/b", "source",
        ],
        &[
            ("entry", "source"),
            ("source", "first/a"),
            ("first/a", "first/b"),
            ("first/b", "first/a"),
            ("first/b", "second/a"),
            ("second/a", "second/b"),
            ("second/b", "second/a"),
            ("second/b", "join"),
        ],
        "entry",
    );
    let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    let entry_key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "entry".into(),
        invocation: 1,
    };
    let source_key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "source".into(),
        invocation: 1,
    };
    record.invocations.insert("entry".into(), 1);
    record.invocations.insert("source".into(), 1);
    record.passes.insert("entry".into(), 1);
    record.passes.insert("source".into(), 1);
    record.results.insert(
        "entry".into(),
        vec![RunResult {
            plugin_views: Default::default(),
            plugin_digests: Default::default(),
            node_id: "entry".into(),
            interruption: None,
            key: entry_key,
            completion: NodeCompletion {
                submission: "entry complete".into(),
                route: None,
                model_requests: 1,
                output: Value::Null,
            },
            commit: CommitRef {
                id: "entry-result".into(),
                node_id: "entry".into(),
                invocation: 1,
            },
            sequence: 1,
        }],
    );
    record.results.insert(
        "source".into(),
        vec![RunResult {
            plugin_views: Default::default(),
            plugin_digests: Default::default(),
            node_id: "source".into(),
            interruption: None,
            key: source_key,
            completion: NodeCompletion {
                submission: "source chose another route".into(),
                route: None,
                model_requests: 1,
                output: Value::Null,
            },
            commit: CommitRef {
                id: "source-result".into(),
                node_id: "source".into(),
                invocation: 1,
            },
            sequence: 2,
        }],
    );
    record.decided.insert(
        "entry|source".into(),
        EdgeDecision {
            selected: true,
            sequence: 1,
            source_invocation: 1,
            result_sequence: 1,
        },
    );
    record.decided.insert(
        "source|first/a".into(),
        EdgeDecision {
            selected: false,
            sequence: 2,
            source_invocation: 1,
            result_sequence: 2,
        },
    );
    record.sequence = 2;

    assert!(propagate_inactive_edges(&mut record).unwrap());
    let first_sequences = record
        .decided
        .iter()
        .map(|(edge, decision)| (edge.clone(), decision.sequence))
        .collect::<BTreeMap<_, _>>();
    assert!(
        record.decided["first/a|first/b"].sequence < record.decided["second/a|second/b"].sequence
    );
    assert!(
        record.decided["second/a|second/b"].sequence < record.decided["second/b|join"].sequence
    );

    assert!(!propagate_inactive_edges(&mut record).unwrap());
    let second_sequences = record
        .decided
        .iter()
        .map(|(edge, decision)| (edge.clone(), decision.sequence))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(first_sequences, second_sequences);
    for edge in [
        "source|first/a",
        "first/a|first/b",
        "first/b|first/a",
        "first/b|second/a",
        "second/a|second/b",
        "second/b|second/a",
        "second/b|join",
    ] {
        assert!(!record.decided[edge].selected, "{edge}");
    }
}

#[test]
fn skipped_propagation_does_not_invent_decisions_inside_unentered_cycle() {
    let snapshot = graph(
        &["entry", "loop/a", "loop/b"],
        &[
            ("entry", "loop/a"),
            ("loop/a", "loop/b"),
            ("loop/b", "loop/a"),
        ],
        "entry",
    );
    let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    record.decided.insert(
        edge_key("entry", "loop/a"),
        EdgeDecision {
            selected: false,
            sequence: 1,
            source_invocation: 0,
            result_sequence: 0,
        },
    );
    record.sequence = 1;

    assert!(propagate_inactive_edges(&mut record).unwrap());
    for edge in ["entry|loop/a", "loop/a|loop/b", "loop/b|loop/a"] {
        let decision = &record.decided[edge];
        assert!(!decision.selected, "{edge}");
        assert_eq!(decision.source_invocation, 0, "{edge}");
        assert_eq!(decision.result_sequence, 0, "{edge}");
    }
}

#[test]
fn later_false_loop_input_retires_stale_selected_output() {
    let snapshot = graph(
        &["source", "branch", "merge"],
        &[
            ("source", "branch"),
            ("branch", "source"),
            ("branch", "merge"),
        ],
        "source",
    );
    let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    record.invocations.insert("source".into(), 2);
    record.invocations.insert("branch".into(), 1);
    record.passes.insert("source".into(), 2);
    record.passes.insert("branch".into(), 1);
    let key = |node: &str, invocation| InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: node.into(),
        invocation,
    };
    record.results.insert(
        "branch".into(),
        vec![RunResult {
            plugin_views: Default::default(),
            plugin_digests: Default::default(),
            node_id: "branch".into(),
            interruption: None,
            key: key("branch", 1),
            completion: NodeCompletion {
                submission: "branch completed in round one".into(),
                route: Some("merge".into()),
                model_requests: 1,
                output: serde_json::json!({"stale":true}),
            },
            commit: CommitRef {
                id: "branch-round-one".into(),
                node_id: "branch".into(),
                invocation: 1,
            },
            sequence: 10,
        }],
    );
    record.decided.insert(
        "source|branch".into(),
        EdgeDecision {
            selected: false,
            sequence: 11,
            source_invocation: 2,
            result_sequence: 0,
        },
    );
    record.decided.insert(
        "branch|merge".into(),
        EdgeDecision {
            selected: true,
            sequence: 12,
            source_invocation: 1,
            result_sequence: 10,
        },
    );
    record.sequence = 12;

    assert!(propagate_inactive_edges(&mut record).unwrap());
    let stale_output = &record.decided["branch|merge"];
    assert!(!stale_output.selected);
    assert_eq!(stale_output.source_invocation, 0);
    assert_eq!(stale_output.result_sequence, 0);
    assert!(
        record.results["branch"]
            .iter()
            .any(|result| { result.commit.id == "branch-round-one" })
    ); // Preserve the old artifact as history, but no longer route it.
}

#[test]
fn parallel_admission_rejects_orphan_reused_and_malformed_pairs() {
    let mut orphan = parallel_graph();
    orphan
        .ops
        .insert("lonely".into(), serde_json::json!({"join":{}}));
    orphan.nodes.push(GraphNode {
        id: "orphan".into(),
        agent: None,
        op: Some("lonely".into()),
        input: None,
        plugins: vec![],
        max_rounds: None,
    });
    assert!(matches!(
        orphan.validate(),
        Err(GraphError::InvalidSnapshot(_))
    ));

    let mut wrong_pair = parallel_graph();
    wrong_pair
        .ops
        .insert("other".into(), serde_json::json!({"join":{}}));
    wrong_pair.nodes.push(GraphNode {
        id: "other-join".into(),
        agent: None,
        op: Some("other".into()),
        input: None,
        plugins: vec![],
        max_rounds: None,
    });
    wrong_pair.ops.insert(
        "fanout2".into(),
        serde_json::json!({"fanout":{"join":"collect"}}),
    );
    wrong_pair.nodes.push(GraphNode {
        id: "split2".into(),
        agent: None,
        op: Some("fanout2".into()),
        input: None,
        plugins: vec![],
        max_rounds: None,
    });
    wrong_pair.edges.extend([
        GraphEdge {
            from_node: "split2".into(),
            to_node: "left".into(),
        },
        GraphEdge {
            from_node: "split2".into(),
            to_node: "right".into(),
        },
    ]);
    assert!(matches!(
        wrong_pair.validate(),
        Err(GraphError::InvalidSnapshot(_))
    ));

    let mut both = parallel_graph();
    both.ops.get_mut("fanout").unwrap()["run"] = Value::String("echo".into());
    assert!(matches!(
        both.validate(),
        Err(GraphError::InvalidSnapshot(_))
    ));
}

#[test]
fn parallel_admission_rejects_branch_forks_merges_and_cycles() {
    let mut fork = parallel_graph();
    fork.edges.push(GraphEdge {
        from_node: "left".into(),
        to_node: "after".into(),
    });
    assert!(matches!(
        fork.validate(),
        Err(GraphError::InvalidSnapshot(_))
    ));

    let mut merge = parallel_graph();
    merge.edges.push(GraphEdge {
        from_node: "after".into(),
        to_node: "left".into(),
    });
    assert!(matches!(
        merge.validate(),
        Err(GraphError::InvalidSnapshot(_))
    ));

    let mut cycle = parallel_graph();
    cycle.edges.push(GraphEdge {
        from_node: "left".into(),
        to_node: "start".into(),
    });
    assert!(matches!(
        cycle.validate(),
        Err(GraphError::InvalidSnapshot(_))
    ));
}

#[test]
fn route_selection_checks_any_explicit_route_and_requires_multiple_exit_choice() {
    let completion = |route: Option<&str>| NodeCompletion {
        submission: "done".into(),
        route: route.map(str::to_owned),
        model_requests: 1,
        output: Value::Null,
    };
    assert_eq!(
        select_route(&completion(None), &["only".into()]).unwrap(),
        Some("only".into())
    );
    assert!(matches!(
        select_route(&completion(Some("wrong")), &["only".into()]),
        Err(GraphError::InvalidRoute(_))
    ));
    assert!(matches!(
        select_route(&completion(None), &["left".into(), "right".into()]),
        Err(GraphError::InvalidRoute(_))
    ));
}

#[test]
fn snapshot_admission_preserves_module_rounds_and_rejects_unsafe_shapes() {
    let mut snapshot = graph(&["enter", "m/a", "m/b"], &[("enter", "m/a")], "enter");
    snapshot.module_rounds.insert("m".into(), 2);
    let value = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(value["_module_rounds"]["m"], 2);
    assert_eq!(GraphSnapshot::admit(value).unwrap().module_rounds["m"], 2);

    let mut bad = graph(&["a", "b"], &[("a", "b"), ("a", "b")], "a");
    assert!(bad.validate().is_err());
    bad = graph(&[".graph-calls/x"], &[], ".graph-calls/x");
    assert!(bad.validate().is_err());
    bad = graph(&["a"], &[], "a");
    bad.module_rounds.insert("missing".into(), 2);
    assert!(bad.validate().is_err());
    let mut raw = serde_json::to_value(graph(&["a"], &[], "a")).unwrap();
    raw["unexpected"] = Value::Bool(true);
    assert!(GraphSnapshot::admit(raw).is_err());
}

#[test]
fn expanded_graph_snapshot_fixture_preserves_node_policy() {
    let raw: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/one-search.snapshot.json"
    ))
    .unwrap();
    let snapshot = GraphSnapshot::admit(raw).unwrap();
    assert_eq!(snapshot.entry, "search");
    let agent = &snapshot.agents["searcher"];
    assert_eq!(agent.model, "models.academic");
    assert!(agent.network);
    assert_eq!(agent.max_steps, Some(25));
    assert_eq!(agent.wall_time_limit_seconds, Some(600.0));
    assert_eq!(snapshot.nodes[0].id, "search");
}

#[test]
fn created_records_carry_admission_timestamp_without_lifecycle_observations() {
    let record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    assert_eq!(record.format, RUN_RECORD_FORMAT);
    let text = record
        .started
        .as_deref()
        .expect("new records carry admission time");
    assert!(text.ends_with('Z'), "{text}");
    assert!(text.len() >= 20, "{text}");
    let value = serde_json::to_value(&record).unwrap();
    for field in ["updated", "error", "recovery", "recovery_submissions"] {
        assert!(
            value.get(field).is_none(),
            "{field} belongs outside Graph facts"
        );
    }
    record.validate().unwrap();
}

#[test]
fn an_old_shape_snapshot_reproduces_its_stored_digest() {
    // A format-11-era record: bare ids, and the digest the old host computed.
    let stored = serde_json::json!({
        "objective": "old shape",
        "entry": "work",
        "agents": {"worker": {"model": "fixture"}},
        "ops": {},
        "nodes": [{"id": "work", "agent": "worker", "plugins": ["demo"]}],
        "edges": []
    });
    let snapshot = GraphSnapshot::admit(stored.clone()).unwrap();
    // The serialized shape is unchanged, which is what makes the digest match.
    let written = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(
        written["nodes"][0]["plugins"],
        serde_json::json!(["demo"]),
        "a required reference with no default writes the bare id again"
    );
    // The digest the old host stored is the digest of exactly these bytes.
    use sha2::Digest;
    let expected = format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_vec(&snapshot).unwrap())
    );
    assert_eq!(snapshot.digest().unwrap(), expected);
    assert_eq!(
        expected,
        "ff1b970d8f670fabec8431d9eb40a65c4e82b8c37b9b17f12491262d1fcaa66d"
    );
    // ...and a record carrying it validates, which is what `records()` needs.
    let mut record = GraphRunRecord::create_with_id(snapshot, Value::Null, "legacy-shape").unwrap();
    record.graph_digest = expected;
    assert!(record.validate().is_ok(), "{:?}", record.validate());

    // An optional or defaulted reference is a *new* declaration, and keeps a shape
    // of its own: it does not have to match the old bytes.
    let optional = GraphSnapshot::admit(serde_json::json!({
        "objective": "new shape",
        "entry": "work",
        "agents": {"worker": {"model": "fixture"}},
        "ops": {},
        "nodes": [{"id": "work", "agent": "worker",
                   "plugins": [{"id": "demo", "required": false, "default": false}]}],
        "edges": []
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&optional).unwrap()["nodes"][0]["plugins"],
        serde_json::json!([{"id": "demo", "required": false, "default": false}])
    );
}

#[test]
fn newer_run_formats_are_refused_instead_of_guessed() {
    let mut value = serde_json::to_value(
        GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap(),
    )
    .unwrap();
    value["format"] = serde_json::json!(RUN_RECORD_FORMAT + 1);
    let error = serde_json::from_value::<GraphRunRecord>(value)
        .unwrap_err()
        .to_string();
    assert!(error.contains("unsupported graph run format"), "{error}");
}

#[test]
fn run_record_decode_preserves_duplicate_field_rejection() {
    let mut record = GraphRunRecord::create(graph(&["work"], &[], "work"), Value::Null).unwrap();
    record.plugin_bindings.insert(
        "review".into(),
        PluginBinding {
            id: "review".into(),
            digest: "code".into(),
            resources: vec![],
            mcp_servers: vec![],
            authorization_digest: "authority".try_into().expect("non-empty fixture authority"),
        },
    );
    let json = serde_json::to_string(&record).unwrap();
    for (field, duplicate) in [
        ("\"format\":15", "\"format\":15,\"format\":15"),
        (
            "\"entry\":\"work\"",
            "\"entry\":\"other\",\"entry\":\"work\"",
        ),
        (
            "\"authorization_digest\":\"authority\"",
            "\"authorization_digest\":\"\",\"authorization_digest\":\"authority\"",
        ),
    ] {
        assert!(json.contains(field), "{field}");
        let error = serde_json::from_str::<GraphRunRecord>(&json.replace(field, duplicate))
            .unwrap_err()
            .to_string();
        assert!(error.contains("duplicate field"), "{error}");
    }
    assert_eq!(
        serde_json::from_str::<GraphRunRecord>(&json).unwrap(),
        record
    );
}

#[test]
fn non_object_run_record_decode_requires_offline_migration() {
    let record = GraphRunRecord::create(graph(&["work"], &[], "work"), Value::Null).unwrap();
    let object = serde_json::to_value(record).unwrap();
    let fields = [
        "format",
        "started",
        "updated",
        "run_id",
        "graph_digest",
        "snapshot",
        "input",
        "status",
        "cursor",
        "parallel",
        "recovery",
        "recovery_submissions",
        "graph_calls",
        "plugin_bindings",
        "plugin_bindings_initialized",
        "invocations",
        "passes",
        "module_activations",
        "ceased",
        "results",
        "decided",
        "sequence",
        "error",
    ];
    for format in [12, RUN_RECORD_FORMAT] {
        let mut values = fields
            .iter()
            .map(|field| object[*field].clone())
            .collect::<Vec<_>>();
        values[0] = serde_json::json!(format);
        let error = serde_json::from_value::<GraphRunRecord>(Value::Array(values))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("old non-object Graph Run format")
                && error.contains("offline migration required"),
            "{error}"
        );
    }
}
