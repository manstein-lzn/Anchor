#[tokio::test]
async fn serial_routing_skips_unselected_and_passes_fixed_commit_input() {
    let mut g = graph(
        &["start", "left", "right"],
        &[("start", "left"), ("start", "right")],
        "start",
    );
    let worker = g.agents.get_mut("worker").unwrap();
    worker.network = true;
    worker.wall_time_limit_seconds = Some(45.0);
    g.nodes[1].input = Some(Value::String("review left".into()));
    let a = MemoryArtifacts::default();
    let n = FakeNodes::default();
    let record =
        GraphRunRecord::create(g, serde_json::json!({"defaults":{"b":2},"request":"x"})).unwrap();
    let harness = Harness::memory(record);
    harness.run(&a, &n).await.unwrap();
    let done = harness.record();
    assert_eq!(
        harness.phase(),
        Phase::Settled {
            result: Outcome::Done
        }
    );
    assert_eq!(
        n.calls
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.key.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["start", "left"]
    );
    let next = &n.calls.lock().unwrap()[1];
    assert_eq!(next.input_commits.len(), 1);
    assert_eq!(next.input["input"]["defaults"]["a"], 1);
    assert_eq!(next.input["input"]["defaults"]["b"], 2);
    assert_eq!(next.input["input"]["request"], "x");
    assert!(next.task.contains("review left"));
    assert!(!next.task.contains("Upstream artifacts"));
    assert_eq!(next.model.as_deref(), Some("m"));
    assert!(next.network);
    assert_eq!(next.wall_time_limit_seconds, Some(45.0));
    assert_eq!(done.graph_digest, done.snapshot.digest().unwrap());
}

#[tokio::test]
async fn skipped_diamond_branch_propagates_false_edge_and_releases_join() {
    let snapshot = graph(
        &["start", "left", "right", "merge"],
        &[
            ("start", "left"),
            ("start", "right"),
            ("left", "merge"),
            ("right", "merge"),
        ],
        "start",
    );
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes::default();
    let harness = Harness::memory(GraphRunRecord::create(snapshot, Value::Null).unwrap());
    harness.run(&artifacts, &nodes).await.unwrap();
    let result = harness.record();

    assert_eq!(
        harness.phase(),
        Phase::Settled {
            result: Outcome::Done
        }
    );
    let executed: Vec<String> = nodes
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.key.node_id.clone())
        .collect();
    assert_eq!(executed, vec!["start", "left", "merge"]);
    assert!(!result.decided["start|right"].selected);
    let propagated = &result.decided["right|merge"];
    assert!(!propagated.selected);
    assert_eq!(propagated.source_invocation, 0);
    assert_eq!(propagated.result_sequence, 0);
    assert!(!result.invocations.contains_key("right"));
}

#[tokio::test]
async fn skipped_multi_level_chain_propagates_to_join_without_running_nodes() {
    let snapshot = graph(
        &["start", "live", "skip-a", "skip-b", "join"],
        &[
            ("start", "live"),
            ("start", "skip-a"),
            ("live", "join"),
            ("skip-a", "skip-b"),
            ("skip-b", "join"),
        ],
        "start",
    );
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes::default();
    let harness = Harness::memory(GraphRunRecord::create(snapshot, Value::Null).unwrap());
    harness.run(&artifacts, &nodes).await.unwrap();
    let result = harness.record();

    assert_eq!(
        harness.phase(),
        Phase::Settled {
            result: Outcome::Done
        }
    );
    let executed: Vec<String> = nodes
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.key.node_id.clone())
        .collect();
    assert_eq!(executed, vec!["start", "live", "join"]);
    assert_eq!(result.decided["start|skip-a"].source_invocation, 1);
    for edge in ["skip-a|skip-b", "skip-b|join"] {
        let decision = &result.decided[edge];
        assert!(!decision.selected, "{edge}");
        assert_eq!(decision.source_invocation, 0, "{edge}");
        assert_eq!(decision.result_sequence, 0, "{edge}");
    }
    for node in ["skip-a", "skip-b"] {
        assert!(!result.invocations.contains_key(node));
        assert!(!result.passes.contains_key(node));
    }
}

#[tokio::test]
async fn inactive_unstarted_cycle_closes_and_releases_selected_merge() {
    let snapshot = graph(
        &["start", "live", "cycle_a", "cycle_b", "merge"],
        &[
            ("start", "live"),
            ("start", "cycle_a"),
            ("cycle_a", "cycle_b"),
            ("cycle_b", "cycle_a"),
            ("cycle_b", "merge"),
            ("live", "merge"),
        ],
        "start",
    );
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes::default();
    let harness = Harness::memory(GraphRunRecord::create(snapshot, Value::Null).unwrap());
    harness.run(&artifacts, &nodes).await.unwrap();
    let result = harness.record();

    assert_eq!(
        harness.phase(),
        Phase::Settled {
            result: Outcome::Done
        }
    );
    let executed: Vec<String> = nodes
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.key.node_id.clone())
        .collect();
    assert_eq!(executed, vec!["start", "live", "merge"]);
    assert_eq!(result.decided["start|cycle_a"].source_invocation, 1);
    for edge in ["cycle_a|cycle_b", "cycle_b|cycle_a", "cycle_b|merge"] {
        let decision = &result.decided[edge];
        assert!(!decision.selected, "{edge}");
        assert_eq!(decision.source_invocation, 0, "{edge}");
        assert_eq!(decision.result_sequence, 0, "{edge}");
    }
    assert!(result.decided["live|merge"].selected);
}

#[tokio::test]
async fn op_run_policy_and_command_are_passed_to_the_host_port() {
    let mut snapshot = graph(&["command"], &[], "command");
    snapshot.ops.insert(
        "script".into(),
        serde_json::json!({
            "run": "printf fixture",
            "network": true,
            "wall_time_limit_seconds": 90
        }),
    );
    snapshot.nodes[0].agent = None;
    snapshot.nodes[0].op = Some("script".into());
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes {
        op_run: true,
        ..Default::default()
    };
    let record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    let harness = Harness::memory(record);
    harness.run(&artifacts, &nodes).await.unwrap();
    assert_eq!(
        harness.phase(),
        Phase::Settled {
            result: Outcome::Done
        }
    );
    let calls = nodes.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].kind, NodeKind::OpRun);
    assert_eq!(calls[0].model, None);
    assert_eq!(
        calls[0].operation,
        Some(Value::String("printf fixture".into()))
    );
    assert!(calls[0].network);
    assert_eq!(calls[0].wall_time_limit_seconds, Some(90.0));
}

#[tokio::test]
async fn parallel_branch_reuses_node_round_ceiling_before_dispatch() {
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes {
        op_run: true,
        ..Default::default()
    };
    let mut snapshot = parallel_graph();
    snapshot.nodes[1].max_rounds = Some(1);
    let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    record.passes.insert("left".into(), 1);
    let harness = Harness::memory(record);
    harness.run(&artifacts, &nodes).await.unwrap();
    let failed = harness.record();
    assert!(matches!(
        harness.phase(),
        Phase::Settled {
            result: Outcome::Failed { .. }
        }
    ));
    assert!(
        harness.failure_detail().contains("left") && harness.failure_detail().contains("ceiling")
    );
    assert!(
        !nodes
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.key.node_id == "left")
    );
    assert!(!failed.results.contains_key("collect"));
}

#[tokio::test]
async fn loop_reentry_uses_fresh_selected_back_edge_and_ceiling() {
    let mut g = graph(
        &["gather", "review"],
        &[("gather", "review"), ("review", "gather")],
        "gather",
    );
    g.nodes[0].max_rounds = Some(2);
    let a = MemoryArtifacts::default();
    let n = FakeNodes::default();
    let run = GraphRunRecord::create(g, Value::Null).unwrap();
    let harness = Harness::memory(run);
    harness.run(&a, &n).await.unwrap();
    assert_eq!(
        n.calls
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.key.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["gather", "review", "gather", "review"]
    );
    assert!(matches!(
        harness.phase(),
        Phase::Settled {
            result: Outcome::Escalated { .. }
        }
    ));
}

#[tokio::test]
async fn module_reentry_counts_activations_and_resets_node_pass_ceiling() {
    let mut snapshot = graph(
        &["entry", "module/a", "module/b", "again", "done"],
        &[
            ("entry", "module/a"),
            ("module/a", "module/b"),
            ("module/b", "again"),
            ("module/b", "done"),
            ("again", "module/a"),
        ],
        "entry",
    );
    snapshot.module_rounds.insert("module".into(), 2);
    snapshot.nodes[1].max_rounds = Some(1);
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes::default();
    let run = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    let harness = Harness::memory(run);
    harness.run(&artifacts, &nodes).await.unwrap();
    let result = harness.record();
    assert!(matches!(
        harness.phase(),
        Phase::Settled {
            result: Outcome::Escalated { .. }
        }
    ));
    assert_eq!(result.module_activations["module"], 2);
    assert_eq!(result.passes["module/a"], 1);
    assert_eq!(result.invocations["module/a"], 2);
}
