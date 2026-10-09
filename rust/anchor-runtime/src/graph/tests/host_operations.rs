use super::*;
use std::collections::VecDeque;

struct ScriptedNodes {
    capabilities: NodeExecutionCapabilities,
    outcomes: Mutex<VecDeque<NodeExecutionOutcome>>,
    facts: Mutex<BTreeMap<String, CompletionFact>>,
    calls: Mutex<Vec<NodeExecutionRequest>>,
}

impl ScriptedNodes {
    fn new(outcomes: Vec<NodeExecutionOutcome>) -> Self {
        Self {
            capabilities: NodeExecutionCapabilities {
                agent: true,
                op_run: true,
                host_operations: true,
                exact_provider_request_budget: true,
            },
            outcomes: Mutex::new(outcomes.into()),
            facts: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl NodeExecutionPort for ScriptedNodes {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        self.capabilities
    }

    fn completion_fact<'a>(
        &'a self,
        key: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(self
                .facts
                .lock()
                .unwrap()
                .get(&key.durable_key())
                .cloned()
                .unwrap_or(CompletionFact::NotStarted))
        })
    }

    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let identity = request.key.durable_key();
            if self
                .facts
                .lock()
                .unwrap()
                .get(&identity)
                .is_some_and(|fact| {
                    !matches!(fact, CompletionFact::NotStarted | CompletionFact::Resumable)
                })
            {
                return Err(GraphError::CorruptRun("duplicate scripted dispatch".into()));
            }
            self.calls.lock().unwrap().push(request);
            let outcome = self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| GraphError::CorruptRun("unexpected scripted dispatch".into()))?;
            let fact = match &outcome {
                NodeExecutionOutcome::Completed(completion) => {
                    CompletionFact::Completed(completion.clone())
                }
                NodeExecutionOutcome::Yielded { reason, route } => CompletionFact::Yielded {
                    reason: reason.clone(),
                    route: route.clone(),
                },
                NodeExecutionOutcome::Failed { reason } => CompletionFact::Failed(reason.clone()),
                _ => CompletionFact::Resumable,
            };
            self.facts.lock().unwrap().insert(identity, fact);
            Ok(outcome)
        })
    }
}

#[derive(Default)]
struct ControlArtifacts {
    inner: MemoryArtifacts,
    contexts: Mutex<Vec<(InvocationKey, ArtifactFreezeContext)>>,
    workspace_freezes: Mutex<usize>,
    fail_before_interruption: Mutex<bool>,
    fail_after_interruption: Mutex<bool>,
}

impl ArtifactPort for ControlArtifacts {
    fn freeze<'a>(
        &'a self,
        key: &'a InvocationKey,
        completion: &'a NodeCompletion,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
        *self.workspace_freezes.lock().unwrap() += 1;
        self.inner.freeze(key, completion)
    }

    fn freeze_with_context<'a>(
        &'a self,
        key: &'a InvocationKey,
        completion: &'a NodeCompletion,
        context: &'a ArtifactFreezeContext,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            self.contexts
                .lock()
                .unwrap()
                .push((key.clone(), context.clone()));
            if context.kind != ArtifactKind::Interruption {
                return self.freeze(key, completion).await;
            }
            if std::mem::take(&mut *self.fail_before_interruption.lock().unwrap()) {
                return Err(GraphError::CorruptRun(
                    "before interruption artifact".into(),
                ));
            }
            let commit = self.inner.freeze(key, completion).await?;
            if std::mem::take(&mut *self.fail_after_interruption.lock().unwrap()) {
                return Err(GraphError::CorruptRun("after interruption artifact".into()));
            }
            Ok(commit)
        })
    }

    fn resolve<'a>(
        &'a self,
        commit: &'a CommitRef,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
        self.inner.resolve(commit)
    }
}

fn host_authoring(host: Value) -> Value {
    serde_json::json!({
        "objective": "host fixture",
        "entry": "wait",
        "ops": {"wait": {"host": host}},
        "nodes": [{"id": "wait", "op": "wait"}]
    })
}

fn wait_snapshot() -> GraphSnapshot {
    GraphSnapshot::from_authoring(host_authoring(
        serde_json::json!({"operation": "session.wait_input"}),
    ))
    .unwrap()
}

fn authoring_value(snapshot: &GraphSnapshot) -> Value {
    let mut value = serde_json::to_value(snapshot).unwrap();
    for agent in value["agents"].as_object_mut().unwrap().values_mut() {
        agent
            .as_object_mut()
            .unwrap()
            .retain(|_, field| !field.is_null());
    }
    for node in value["nodes"].as_array_mut().unwrap() {
        node.as_object_mut()
            .unwrap()
            .retain(|_, field| !field.is_null());
    }
    value
}

fn assistant_loop() -> GraphSnapshot {
    let mut snapshot = graph(
        &["wait", "work", "reply"],
        &[
            ("wait", "work"),
            ("work", "reply"),
            ("work", "wait"),
            ("reply", "wait"),
        ],
        "wait",
    );
    for (node_index, operation) in [(0, "session.wait_input"), (2, "session.reply")] {
        let node = &mut snapshot.nodes[node_index];
        node.agent = None;
        node.op = Some(node.id.clone());
        snapshot.ops.insert(
            node.id.clone(),
            serde_json::json!({"host": {"operation": operation}}),
        );
    }
    snapshot
}

fn completed(output: Value, route: Option<&str>, model_requests: u64) -> NodeExecutionOutcome {
    NodeExecutionOutcome::Completed(NodeCompletion {
        submission: output.to_string(),
        route: route.map(str::to_owned),
        model_requests,
        output,
    })
}

#[test]
fn host_op_compilation_and_admission_preserve_opaque_json_parameters() {
    for parameters in [
        Value::Null,
        serde_json::json!(7),
        serde_json::json!([true, {"nested": "value"}]),
    ] {
        let host = serde_json::json!({"operation": "custom.operation", "params": parameters});
        let snapshot = GraphSnapshot::from_authoring(host_authoring(host.clone())).unwrap();
        assert_eq!(snapshot.ops["wait"]["host"], host);
        assert!(
            snapshot.ops["wait"]
                .get("wall_time_limit_seconds")
                .is_none()
        );
        assert_eq!(
            GraphSnapshot::admit(serde_json::to_value(&snapshot).unwrap()).unwrap(),
            snapshot
        );
    }
}

#[test]
fn host_op_compilation_and_admission_reject_invalid_forms() {
    for host in [
        Value::Null,
        serde_json::json!([]),
        serde_json::json!(true),
        serde_json::json!({}),
        serde_json::json!({"operation": null}),
        serde_json::json!({"operation": 4}),
        serde_json::json!({"operation": ""}),
        serde_json::json!({"operation": " \n\t"}),
    ] {
        assert!(matches!(
            GraphSnapshot::from_authoring(host_authoring(host.clone())),
            Err(GraphError::InvalidSnapshot(_))
        ));
        let mut snapshot = wait_snapshot();
        snapshot.ops.get_mut("wait").unwrap()["host"] = host;
        assert!(matches!(
            GraphSnapshot::admit(serde_json::to_value(snapshot).unwrap()),
            Err(GraphError::InvalidSnapshot(_))
        ));
    }
    let mut unused = host_authoring(serde_json::json!({"operation": "custom.operation"}));
    unused["ops"]["unused"] = serde_json::json!({"host": {"operation": ""}});
    assert!(GraphSnapshot::from_authoring(unused).is_err());
    for (operation, spec) in [
        ("run", serde_json::json!("true")),
        (
            "call",
            serde_json::json!({"graph": "other", "mode": "wait"}),
        ),
        ("fanout", serde_json::json!({"join": "collect"})),
        ("join", serde_json::json!({})),
    ] {
        let mut authoring = host_authoring(serde_json::json!({"operation": "custom.operation"}));
        authoring["ops"]["wait"][operation] = spec.clone();
        assert!(GraphSnapshot::from_authoring(authoring).is_err());
        let mut snapshot = wait_snapshot();
        snapshot.ops.get_mut("wait").unwrap()[operation] = spec;
        assert!(GraphSnapshot::admit(serde_json::to_value(snapshot).unwrap()).is_err());
    }
}

#[tokio::test]
async fn host_capabilities_fail_closed_even_when_op_run_is_enabled() {
    assert!(!NodeExecutionCapabilities::default().host_operations);
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes {
        op_run: true,
        ..Default::default()
    };
    let control = Control::default();
    let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(wait_snapshot(), Value::Null).unwrap())
        .await;
    assert!(
        matches!(result, Err(GraphError::Unsupported(message)) if message.contains("host operation"))
    );
    assert!(nodes.calls.lock().unwrap().is_empty());
    assert_eq!(*store.saves.lock().unwrap(), 0);
}

#[tokio::test]
async fn host_operations_dispatch_opaque_spec_and_preserve_timeout_defaults() {
    let mut snapshot = graph(
        &["wait", "command", "work"],
        &[("wait", "command"), ("command", "work")],
        "wait",
    );
    let host = serde_json::json!({"operation": "custom.operation", "params": {"nested": [1, 2]}});
    snapshot.nodes[0].agent = None;
    snapshot.nodes[0].op = Some("wait".into());
    snapshot.nodes[1].agent = None;
    snapshot.nodes[1].op = Some("command".into());
    snapshot
        .ops
        .insert("wait".into(), serde_json::json!({"host": host}));
    snapshot
        .ops
        .insert("command".into(), serde_json::json!({"run": "true"}));
    let snapshot = GraphSnapshot::from_authoring(authoring_value(&snapshot)).unwrap();
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let nodes = ScriptedNodes::new(vec![
        completed(Value::Null, None, 0),
        completed(Value::Null, None, 0),
        completed(Value::Null, None, 1),
    ]);
    let control = Control::default();
    let done = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
        .await
        .unwrap();
    assert_eq!(done.status, RunStatus::Completed);
    let calls = nodes.calls.lock().unwrap();
    assert_eq!(calls[0].kind, NodeKind::OpHost);
    assert_eq!(calls[0].operation, Some(host));
    assert_eq!(calls[0].wall_time_limit_seconds, None);
    assert_eq!(calls[0].model, None);
    assert_eq!(calls[0].max_provider_requests, None);
    assert_eq!(calls[1].kind, NodeKind::OpRun);
    assert_eq!(calls[1].wall_time_limit_seconds, Some(3600.0));
    assert_eq!(calls[2].kind, NodeKind::Agent);
    assert_eq!(calls[2].wall_time_limit_seconds, Some(3600.0));
}

#[tokio::test]
async fn host_wait_explicit_timeout_is_preserved() {
    let mut authoring = host_authoring(serde_json::json!({"operation": "session.wait_input"}));
    authoring["ops"]["wait"]["wall_time_limit_seconds"] = serde_json::json!(12.5);
    let snapshot = GraphSnapshot::from_authoring(authoring).unwrap();
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let mut nodes = ScriptedNodes::new(vec![completed(Value::Null, None, 0)]);
    nodes.capabilities.op_run = false;
    let control = Control::default();
    GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
        .await
        .unwrap();
    assert_eq!(
        nodes.calls.lock().unwrap()[0].wall_time_limit_seconds,
        Some(12.5)
    );
}

#[tokio::test]
async fn host_wait_suspends_without_artifact_and_resumes_same_invocation() {
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let nodes = ScriptedNodes::new(vec![
        NodeExecutionOutcome::Suspended,
        completed(serde_json::json!({"turn": 1}), None, 0),
    ]);
    let control = Control::default();
    let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
    let paused = runner
        .run(GraphRunRecord::create(wait_snapshot(), Value::Null).unwrap())
        .await
        .unwrap();
    assert_eq!(paused.status, RunStatus::Paused);
    let key = paused.cursor.as_ref().unwrap().key.clone();
    assert!(paused.results.is_empty());
    assert!(paused.decided.is_empty());
    assert_eq!(*artifacts.freezes.lock().unwrap(), 0);
    assert_eq!(paused.invocations["wait"], 1);
    assert_eq!(paused.passes["wait"], 1);
    paused.validate().unwrap();
    let resumed = runner.run(paused).await.unwrap();
    assert_eq!(resumed.status, RunStatus::Completed);
    let calls = nodes.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|request| request.key == key && request.wall_time_limit_seconds.is_none())
    );
    assert_eq!(resumed.results["wait"][0].key, key);
    assert_eq!(resumed.invocations["wait"], 1);
    assert_eq!(*artifacts.freezes.lock().unwrap(), 1);
}

#[tokio::test]
async fn host_input_reply_loop_keeps_one_run_for_two_turns() {
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let nodes = ScriptedNodes::new(vec![
        completed(serde_json::json!({"turn": 1}), None, 0),
        completed(serde_json::json!({"answer": 1}), Some("reply"), 1),
        completed(serde_json::json!({"sent": 1}), None, 0),
        completed(serde_json::json!({"turn": 2}), None, 0),
        completed(serde_json::json!({"answer": 2}), Some("reply"), 1),
        completed(serde_json::json!({"sent": 2}), None, 0),
        NodeExecutionOutcome::Suspended,
    ]);
    let control = Control::default();
    let initial = GraphRunRecord::create(assistant_loop(), Value::Null).unwrap();
    let run_id = initial.run_id.clone();
    let paused = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(initial)
        .await
        .unwrap();
    assert_eq!(paused.run_id, run_id);
    assert_eq!(paused.status, RunStatus::Paused);
    assert_eq!(paused.cursor.as_ref().unwrap().key.invocation, 3);
    for node_id in ["wait", "work", "reply"] {
        assert_eq!(paused.results[node_id].len(), 2);
        assert_eq!(paused.results[node_id][0].key.invocation, 1);
        assert_eq!(paused.results[node_id][1].key.invocation, 2);
        assert_ne!(
            paused.results[node_id][0].commit,
            paused.results[node_id][1].commit
        );
    }
    let calls = nodes.calls.lock().unwrap();
    assert!(calls.iter().all(|request| request.key.run_id == run_id));
    assert_eq!(calls[1].input["committed_inputs"][0]["turn"], 1);
    assert_eq!(calls[4].input["committed_inputs"][0]["turn"], 2);
    assert_eq!(
        calls[2].input_commits,
        vec![paused.results["work"][0].commit.clone()]
    );
    assert_eq!(
        calls[5].input_commits,
        vec![paused.results["work"][1].commit.clone()]
    );
    paused.validate().unwrap();
}

#[tokio::test]
async fn yielded_round_routes_back_to_wait_without_faking_model_success() {
    let store = MemStore::default();
    let artifacts = ControlArtifacts::default();
    let nodes = ScriptedNodes::new(vec![
        completed(serde_json::json!({"turn": 1}), None, 0),
        NodeExecutionOutcome::Yielded {
            reason: "superseded".into(),
            route: Some("wait".into()),
        },
        completed(serde_json::json!({"turn": 2}), None, 0),
        completed(serde_json::json!({"answer": 2}), Some("reply"), 1),
        completed(serde_json::json!({"sent": 2}), None, 0),
        NodeExecutionOutcome::Suspended,
    ]);
    let control = Control::default();
    let paused = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(assistant_loop(), Value::Null).unwrap())
        .await
        .unwrap();
    assert_eq!(paused.status, RunStatus::Paused);
    assert_eq!(paused.error, None);
    assert_eq!(paused.results["work"].len(), 2);
    assert_eq!(paused.results["reply"].len(), 1);
    let interruption = &paused.results["work"][0];
    assert_eq!(interruption.interruption.as_deref(), Some("superseded"));
    assert_eq!(
        interruption.completion.output,
        serde_json::json!({"interrupted": true, "reason": "superseded"})
    );
    assert_eq!(interruption.completion.model_requests, 0);
    assert_eq!(paused.results["work"][1].interruption, None);
    assert_eq!(paused.results["work"][1].completion.model_requests, 1);
    assert_ne!(interruption.key, paused.results["work"][1].key);
    let contexts = artifacts.contexts.lock().unwrap();
    let control_contexts = contexts
        .iter()
        .filter(|(_, context)| context.kind == ArtifactKind::Interruption)
        .collect::<Vec<_>>();
    assert_eq!(control_contexts.len(), 1);
    assert_eq!(control_contexts[0].0, interruption.key);
    assert_eq!(
        control_contexts[0].1.input_commits,
        vec![paused.results["wait"][0].commit.clone()]
    );
    assert_eq!(*artifacts.workspace_freezes.lock().unwrap(), 4);
    paused.validate().unwrap();
}

#[tokio::test]
async fn durable_yielded_fact_recovers_all_pre_result_crash_windows_without_dispatch() {
    for failure in ["before_artifact", "after_artifact", "before_run_save"] {
        let store = MemStore::default();
        let artifacts = ControlArtifacts::default();
        *artifacts.fail_before_interruption.lock().unwrap() = failure == "before_artifact";
        *artifacts.fail_after_interruption.lock().unwrap() = failure == "after_artifact";
        if failure == "before_run_save" {
            *store.fail_on.lock().unwrap() = Some(3);
        }
        let nodes = ScriptedNodes::new(vec![NodeExecutionOutcome::Yielded {
            reason: "superseded".into(),
            route: None,
        }]);
        let control = Control::default();
        let record = GraphRunRecord::create(graph(&["work"], &[], "work"), Value::Null).unwrap();
        let run_id = record.run_id.clone();
        let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
        assert!(runner.run(record).await.is_err(), "{failure}");
        let saved = store.load(&run_id).unwrap().unwrap();
        assert!(saved.results.is_empty());
        let key = saved.cursor.as_ref().unwrap().key.clone();
        assert!(matches!(
            nodes.facts.lock().unwrap().get(&key.durable_key()),
            Some(CompletionFact::Yielded { .. })
        ));
        *store.fail_on.lock().unwrap() = None;
        let done = runner.run(saved).await.unwrap();
        assert_eq!(done.status, RunStatus::Completed, "{failure}");
        assert_eq!(done.results["work"].len(), 1);
        assert_eq!(done.results["work"][0].key, key);
        assert_eq!(
            done.results["work"][0].interruption.as_deref(),
            Some("superseded")
        );
        assert_eq!(nodes.calls.lock().unwrap().len(), 1, "{failure}");
        assert_eq!(*artifacts.inner.freezes.lock().unwrap(), 1, "{failure}");
        assert_eq!(*artifacts.workspace_freezes.lock().unwrap(), 0);
        done.validate().unwrap();
        assert_eq!(runner.run(done.clone()).await.unwrap(), done);
        assert_eq!(nodes.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn yielded_fact_recovery_preserves_loop_invocation_identity() {
    let store = MemStore::default();
    let artifacts = ControlArtifacts::default();
    *artifacts.fail_after_interruption.lock().unwrap() = true;
    let nodes = ScriptedNodes::new(vec![
        NodeExecutionOutcome::Yielded {
            reason: "superseded".into(),
            route: Some("work".into()),
        },
        completed(serde_json::json!({"answer": 2}), None, 1),
    ]);
    let control = Control::default();
    let mut snapshot = graph(&["work"], &[("work", "work")], "work");
    snapshot.nodes[0].max_rounds = Some(2);
    let record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    let run_id = record.run_id.clone();
    let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
    assert!(runner.run(record).await.is_err());
    let done = runner
        .run(store.load(&run_id).unwrap().unwrap())
        .await
        .unwrap();
    assert_eq!(done.results["work"].len(), 2);
    assert_eq!(done.results["work"][0].key.invocation, 1);
    assert_eq!(done.results["work"][1].key.invocation, 2);
    assert_eq!(
        done.results["work"][0].interruption.as_deref(),
        Some("superseded")
    );
    assert_eq!(done.results["work"][1].interruption, None);
    assert_eq!(nodes.calls.lock().unwrap().len(), 2);
    assert_eq!(*artifacts.inner.freezes.lock().unwrap(), 2);
    done.validate().unwrap();
}

#[tokio::test]
async fn interruption_artifacts_fail_closed_on_legacy_workspace_freezers() {
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let nodes = ScriptedNodes::new(vec![NodeExecutionOutcome::Yielded {
        reason: "superseded".into(),
        route: None,
    }]);
    let control = Control::default();
    let record = GraphRunRecord::create(graph(&["work"], &[], "work"), Value::Null).unwrap();
    let run_id = record.run_id.clone();
    let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
    assert!(matches!(
        runner.run(record).await,
        Err(GraphError::Unsupported(_))
    ));
    let saved = store.load(&run_id).unwrap().unwrap();
    assert!(saved.cursor.is_some());
    assert!(matches!(
        runner.run(saved).await,
        Err(GraphError::Unsupported(_))
    ));
    assert_eq!(*artifacts.freezes.lock().unwrap(), 0);
    assert_eq!(nodes.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_yielded_routes_fail_before_artifact_for_live_and_durable_facts() {
    for durable in [false, true] {
        for route in [Some("not-an-exit".to_owned()), None] {
            let store = MemStore::default();
            let artifacts = ControlArtifacts::default();
            let nodes = ScriptedNodes::new(vec![NodeExecutionOutcome::Yielded {
                reason: "superseded".into(),
                route: route.clone(),
            }]);
            let control = Control::default();
            let record = GraphRunRecord::create(
                graph(
                    &["work", "left", "right"],
                    &[("work", "left"), ("work", "right")],
                    "work",
                ),
                Value::Null,
            )
            .unwrap();
            if durable {
                let key = InvocationKey {
                    run_id: record.run_id.clone(),
                    graph_digest: record.graph_digest.clone(),
                    node_id: "work".into(),
                    invocation: 1,
                };
                nodes.facts.lock().unwrap().insert(
                    key.durable_key(),
                    CompletionFact::Yielded {
                        reason: "superseded".into(),
                        route,
                    },
                );
            }
            let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
            let failed = runner.run(record).await.unwrap();
            assert_eq!(failed.status, RunStatus::Failed);
            assert!(failed.error.as_deref().unwrap().contains("route"));
            assert!(failed.results.is_empty());
            assert!(failed.decided.is_empty());
            assert!(failed.cursor.is_none());
            assert!(artifacts.contexts.lock().unwrap().is_empty());
            assert_eq!(nodes.calls.lock().unwrap().len(), usize::from(!durable));
            assert_eq!(runner.run(failed.clone()).await.unwrap(), failed);
        }
    }
}

#[tokio::test]
async fn cancelled_and_interrupted_still_stop_without_advancing_graph() {
    for outcome in [
        NodeExecutionOutcome::Cancelled,
        NodeExecutionOutcome::Interrupted {
            reason: "executor interruption".into(),
        },
    ] {
        let store = MemStore::default();
        let artifacts = ControlArtifacts::default();
        let nodes = ScriptedNodes::new(vec![outcome]);
        let control = Control::default();
        let stopped = GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(
                GraphRunRecord::create(
                    graph(&["work", "next"], &[("work", "next")], "work"),
                    Value::Null,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stopped.status, RunStatus::Stopped);
        assert!(stopped.cursor.is_some());
        assert!(stopped.results.is_empty());
        assert!(stopped.decided.is_empty());
        assert!(artifacts.contexts.lock().unwrap().is_empty());
        assert_eq!(nodes.calls.lock().unwrap().len(), 1);
        stopped.validate().unwrap();
    }
}

#[test]
fn parallel_regions_reject_host_ops_at_compilation_and_admission() {
    for node_index in [1, 2] {
        let mut snapshot = parallel_graph();
        snapshot.nodes[node_index].agent = None;
        snapshot.nodes[node_index].op = Some("wait".into());
        snapshot.ops.insert(
            "wait".into(),
            serde_json::json!({"host": {"operation": "session.wait_input"}}),
        );
        assert!(
            matches!(GraphSnapshot::admit(serde_json::to_value(&snapshot).unwrap()), Err(GraphError::InvalidSnapshot(message)) if message.contains("host operation"))
        );
        assert!(
            matches!(GraphSnapshot::from_authoring(authoring_value(&snapshot)), Err(GraphError::InvalidSnapshot(message)) if message.contains("host operation"))
        );
    }
}

#[tokio::test]
async fn parallel_regions_reject_live_and_durable_yielded_results() {
    for durable in [false, true] {
        let store = MemStore::default();
        let artifacts = ControlArtifacts::default();
        let nodes = ScriptedNodes::new(vec![
            NodeExecutionOutcome::Yielded {
                reason: "superseded".into(),
                route: None,
            },
            completed(Value::Null, None, 1),
        ]);
        let control = Control::default();
        let record = GraphRunRecord::create(parallel_graph(), Value::Null).unwrap();
        if durable {
            let key = InvocationKey {
                run_id: record.run_id.clone(),
                graph_digest: record.graph_digest.clone(),
                node_id: "left".into(),
                invocation: 1,
            };
            nodes.facts.lock().unwrap().insert(
                key.durable_key(),
                CompletionFact::Yielded {
                    reason: "superseded".into(),
                    route: None,
                },
            );
        }
        let failed = GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(record)
            .await
            .unwrap();
        assert_eq!(failed.status, RunStatus::Failed);
        assert!(
            failed
                .error
                .as_deref()
                .unwrap()
                .contains("Yielded is not supported inside parallel region")
        );
        assert!(
            failed
                .results
                .values()
                .flatten()
                .all(|result| result.interruption.is_none())
        );
        assert!(
            artifacts
                .contexts
                .lock()
                .unwrap()
                .iter()
                .all(|(_, context)| context.kind != ArtifactKind::Interruption)
        );
        if durable {
            assert!(nodes.calls.lock().unwrap().is_empty());
        }
        failed.validate().unwrap();
    }
}

#[tokio::test]
async fn suspended_parallel_branch_uses_existing_stopped_policy() {
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let nodes = ScriptedNodes::new(vec![
        NodeExecutionOutcome::Suspended,
        completed(Value::Null, None, 1),
    ]);
    let control = Control::default();
    let stopped = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(parallel_graph(), Value::Null).unwrap())
        .await
        .unwrap();
    assert_eq!(stopped.status, RunStatus::Stopped);
    assert!(
        stopped
            .parallel
            .as_ref()
            .unwrap()
            .branches
            .iter()
            .any(|branch| branch.cursor.is_some())
    );
    stopped.validate().unwrap();
}

#[tokio::test]
async fn interruption_serialization_and_validation_keep_control_evidence_explicit() {
    let store = MemStore::default();
    let artifacts = ControlArtifacts::default();
    let nodes = ScriptedNodes::new(vec![NodeExecutionOutcome::Yielded {
        reason: "superseded".into(),
        route: None,
    }]);
    let control = Control::default();
    let done = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(graph(&["work"], &[], "work"), Value::Null).unwrap())
        .await
        .unwrap();
    let encoded = serde_json::to_value(&done).unwrap();
    assert_eq!(encoded["results"]["work"][0]["interruption"], "superseded");
    let decoded: GraphRunRecord = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, done);
    decoded.validate().unwrap();
    for change in ["reason", "output", "requests", "submission", "route"] {
        let mut corrupt = done.clone();
        let result = &mut corrupt.results.get_mut("work").unwrap()[0];
        match change {
            "reason" => result.interruption = Some("different".into()),
            "output" => result.completion.output["interrupted"] = serde_json::json!(false),
            "requests" => result.completion.model_requests = 1,
            "submission" => result.completion.submission = "ordinary success".into(),
            "route" => result.completion.route = Some("not-an-exit".into()),
            _ => unreachable!(),
        }
        assert!(
            matches!(corrupt.validate(), Err(GraphError::CorruptRun(_))),
            "{change}"
        );
    }
    let mut ordinary = done.results["work"][0].clone();
    ordinary.interruption = None;
    let legacy = serde_json::to_value(&ordinary).unwrap();
    assert!(legacy.get("interruption").is_none());
    assert_eq!(
        serde_json::from_value::<RunResult>(legacy)
            .unwrap()
            .interruption,
        None
    );
}
