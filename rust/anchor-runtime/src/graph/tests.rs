use super::*;
use serde::{Deserialize, Serialize};
use std::{
    io::BufRead,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct MemStore {
    record: Mutex<Option<GraphRunRecord>>,
    saves: Mutex<usize>,
    fail_on: Mutex<Option<usize>>,
}
struct MemLease;
impl RunLease for MemLease {}
impl RunStore for MemStore {
    fn load(&self, id: &str) -> Result<Option<GraphRunRecord>, GraphError> {
        Ok(self
            .record
            .lock()
            .unwrap()
            .clone()
            .filter(|r| r.run_id == id))
    }
    fn save(&self, r: &GraphRunRecord) -> Result<(), GraphError> {
        let mut n = self.saves.lock().unwrap();
        *n += 1;
        if *self.fail_on.lock().unwrap() == Some(*n) {
            return Err(GraphError::CorruptRun("injected save failure".into()));
        }
        *self.record.lock().unwrap() = Some(r.clone());
        Ok(())
    }
    fn acquire_lease(&self, _: &str) -> Result<Box<dyn RunLease>, GraphError> {
        Ok(Box::new(MemLease))
    }
}

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
        node_id: node.into(),
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

#[tokio::test]
async fn waiting_recovery_preserves_cursor_and_direct_graph_resume_cannot_bypass_it() {
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let control = Control::default();
    let nodes = FakeNodes {
        recovery_once: Mutex::new(true),
        ..Default::default()
    };
    let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    let first = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(initial)
        .await
        .unwrap();
    assert_eq!(first.status, RunStatus::WaitingRecovery);
    assert_eq!(first.recovery.len(), 1);
    assert_eq!(first.recovery[0].key, first.cursor.as_ref().unwrap().key);
    assert_eq!(first.cursor.as_ref().unwrap().key.invocation, 1);
    assert_eq!(nodes.calls.lock().unwrap().len(), 1);

    let again = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(first.clone())
        .await
        .unwrap();
    assert_eq!(again, first);
    assert_eq!(nodes.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn graph_call_wait_resumes_same_identity_and_detach_records_acceptance() {
    fn call_snapshot(mode: &str) -> GraphSnapshot {
        let mut snapshot = graph(&["call", "end"], &[("call", "end")], "call");
        snapshot.nodes[0].agent = None;
        snapshot.nodes[0].op = Some("child".into());
        let mut call = serde_json::json!({
            "graph": "child-graph",
            "mode": mode,
            "input": {"x": 1},
            "input_map": {"topic": "/request/topic"},
            "files": [{"node": "produce", "path": "report.md", "as": "input/report.md"}],
            "session": "research"
        });
        if mode == "wait" {
            call["result"] = serde_json::json!({"node": "answer", "files": ["answer.md"]});
        }
        snapshot
            .ops
            .insert("child".into(), serde_json::json!({"call": call}));
        snapshot
    }

    let port = Arc::new(FakeGraphCalls {
        outcomes: Mutex::new(vec![
            GraphCallOutcome::Waiting {
                child_run_id: "child-run-1".into(),
            },
            GraphCallOutcome::Completed {
                child_run_id: "child-run-1".into(),
                output: serde_json::json!({"answer": 42}),
            },
        ]),
        ..Default::default()
    });
    let nodes = FakeNodes {
        graph_call_port: Some(port.clone()),
        ..Default::default()
    };
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let control = Control::default();
    let initial = GraphRunRecord::create(call_snapshot("wait"), Value::Null).unwrap();
    let first = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(initial)
        .await
        .unwrap();
    assert_eq!(first.status, RunStatus::WaitingCall);
    assert!(
        first
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.node_id == "call")
    );
    let resumed = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(first)
        .await
        .unwrap();
    assert_eq!(resumed.status, RunStatus::Completed);
    {
        let identities = port.identities.lock().unwrap();
        assert_eq!(identities.len(), 2);
        assert_eq!(identities[0], identities[1]);
    }
    assert_eq!(
        resumed.graph_calls.values().next().unwrap().status,
        GraphCallStatus::Completed
    );
    assert!(
        nodes
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.key.node_id != "call")
    );

    for outcome in [
        GraphCallOutcome::Detached {
            child_run_id: "child-run-2".into(),
        },
        // If the child finishes before the parent observes it, detach still
        // returns only the accepted child reference, never its business result.
        GraphCallOutcome::Completed {
            child_run_id: "child-run-3".into(),
            output: serde_json::json!({"secret_result": "not forwarded"}),
        },
    ] {
        let port = Arc::new(FakeGraphCalls {
            outcomes: Mutex::new(vec![outcome]),
            ..Default::default()
        });
        let nodes = FakeNodes {
            graph_call_port: Some(port),
            ..Default::default()
        };
        let store = MemStore::default();
        let artifacts = MemoryArtifacts::default();
        let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(GraphRunRecord::create(call_snapshot("detach"), Value::Null).unwrap())
            .await
            .unwrap();
        let call_result = result.results.get("call").unwrap().last().unwrap();
        assert_eq!(call_result.completion.output["status"], "accepted");
        assert!(call_result.completion.output.get("secret_result").is_none());
    }
}

#[tokio::test]
async fn graph_call_identity_must_keep_one_child_run_across_reload() {
    let mut snapshot = graph(&["call"], &[], "call");
    snapshot.nodes[0].agent = None;
    snapshot.nodes[0].op = Some("child".into());
    snapshot.ops.insert(
        "child".into(),
        serde_json::json!({"call": {"graph": "child-graph", "mode": "wait"}}),
    );

    let port = Arc::new(FakeGraphCalls {
        outcomes: Mutex::new(vec![
            GraphCallOutcome::Waiting {
                child_run_id: "child-run-1".into(),
            },
            // A durable identity must keep resolving to its original child Run.
            // Switching to a different child on reload would silently fork or
            // replay work, so the mismatch must fail closed.
            GraphCallOutcome::Completed {
                child_run_id: "child-run-2".into(),
                output: serde_json::json!({"answer": "unexpected"}),
            },
        ]),
        ..Default::default()
    });
    let nodes = FakeNodes {
        graph_call_port: Some(port.clone()),
        ..Default::default()
    };
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let control = Control::default();
    let waiting = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
        .await
        .unwrap();
    assert_eq!(waiting.status, RunStatus::WaitingCall);
    assert_eq!(
        waiting
            .graph_calls
            .values()
            .next()
            .unwrap()
            .child_run_id
            .as_deref(),
        Some("child-run-1")
    );

    let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(waiting)
        .await;
    assert!(
        matches!(result, Err(GraphError::CorruptRun(message)) if message.contains("changed child Run"))
    );
}

#[tokio::test]
async fn deleted_graph_call_marker_resumes_as_completed_without_replay() {
    use sha2::Digest;

    let mut snapshot = graph(&["call"], &[], "call");
    snapshot.nodes[0].agent = None;
    snapshot.nodes[0].op = Some("child".into());
    let spec = serde_json::json!({"graph": "child-graph", "mode": "wait", "input": {}});
    snapshot
        .ops
        .insert("child".into(), serde_json::json!({"call": spec.clone()}));

    let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "call".into(),
        invocation: 1,
    };
    let identity = CallIdentity {
        parent_run_id: record.run_id.clone(),
        parent_graph_digest: record.graph_digest.clone(),
        node_id: "call".into(),
        invocation: 1,
        call_spec_digest: format!(
            "{:x}",
            sha2::Sha256::digest(serde_json::to_vec(&spec).unwrap())
        ),
    };
    record.graph_calls.insert(
        identity.durable_key(),
        GraphCallRecord {
            identity,
            graph: "child-graph".into(),
            child_run_id: Some("child-run-deleted".into()),
            mode: "wait".into(),
            status: GraphCallStatus::Deleted,
            output: None,
            error: None,
        },
    );
    record.cursor = Some(RunCursor {
        node_id: "call".into(),
        key: key.clone(),
        input_commits: vec![],
        prepared_input: serde_json::json!({"input": Value::Null, "committed_inputs": []}),
    });
    record.invocations.insert("call".into(), 1);
    record.passes.insert("call".into(), 1);
    record.status = RunStatus::Stopped;

    let port = Arc::new(FakeGraphCalls::default());
    let nodes = FakeNodes {
        graph_call_port: Some(port.clone()),
        ..Default::default()
    };
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let control = Control::default();
    let finished = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(record)
        .await
        .unwrap();

    assert_eq!(finished.status, RunStatus::Completed);
    assert!(
        port.identities.lock().unwrap().is_empty(),
        "a deleted child must never be re-admitted"
    );
    let result = finished.results.get("call").unwrap().last().unwrap();
    assert_eq!(result.completion.output["status"], "deleted");
    assert_eq!(result.completion.output["run_id"], "child-run-deleted");
    assert_eq!(
        finished.graph_calls.values().next().unwrap().status,
        GraphCallStatus::Deleted
    );
}

#[test]
fn stopped_conversation_run_can_retain_unknown_recovery_attempts() {
    let mut record = GraphRunRecord::create(graph(&["work"], &[], "work"), Value::Null).unwrap();
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    };
    record.cursor = Some(RunCursor {
        node_id: "work".into(),
        key: key.clone(),
        input_commits: vec![],
        prepared_input: serde_json::json!({"input": Value::Null, "committed_inputs": []}),
    });
    record.invocations.insert("work".into(), 1);
    record.passes.insert("work".into(), 1);
    record.recovery.push(PendingRecovery {
        key,
        attempt: RecoveryAttempt {
            attempt_id: 77,
            step: 2,
            tool: "external_effect".into(),
            started_at: "2026-10-05T00:00:00Z".into(),
        },
    });
    record.status = RunStatus::Stopped;
    assert!(record.validate().is_ok());
}

#[tokio::test]
async fn plugin_manifest_is_pinned_to_run_and_drift_fails_before_dispatch() {
    let mut snapshot = graph(&["work"], &[], "work");
    snapshot.nodes[0].plugins = vec!["review".into()];
    let binding = PluginBinding {
        id: "review".into(),
        digest: "manifest-v1".into(),
        resources: vec!["instructions.md".into()],
        mcp_servers: vec!["search".into()],
    };
    let nodes = FakeNodes {
        resolved_plugins: Mutex::new(vec![binding.clone()]),
        budget_once: Mutex::new(true),
        ..Default::default()
    };
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let control = Control::default();
    let first = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
        .await
        .unwrap();
    assert_eq!(first.status, RunStatus::BudgetStopped);
    assert_eq!(first.plugin_bindings.get("review"), Some(&binding));
    assert!(first.plugin_bindings_initialized);
    assert_eq!(nodes.calls.lock().unwrap()[0].plugins, vec![binding]);

    {
        let mut changed = nodes.resolved_plugins.lock().unwrap();
        changed[0].digest = "manifest-v2".into();
    }
    let resumed = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(first)
        .await
        .unwrap();
    assert_eq!(resumed.status, RunStatus::Failed);
    assert!(
        resumed
            .error
            .as_deref()
            .unwrap()
            .contains("manifest changed")
    );
    assert_eq!(nodes.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn legacy_in_progress_plugin_run_without_binding_fails_closed() {
    let mut snapshot = graph(&["work"], &[], "work");
    snapshot.nodes[0].plugins = vec!["review".into()];
    let nodes = FakeNodes {
        resolved_plugins: Mutex::new(vec![PluginBinding {
            id: "review".into(),
            digest: "manifest-v1".into(),
            resources: vec![],
            mcp_servers: vec![],
        }]),
        ..Default::default()
    };
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let control = Control::default();
    let mut legacy = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    legacy.status = RunStatus::Paused;
    store.save(&legacy).unwrap();
    let resumed = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(legacy)
        .await
        .unwrap();
    assert_eq!(resumed.status, RunStatus::Failed);
    assert!(
        resumed
            .error
            .as_deref()
            .unwrap()
            .contains("in-progress legacy Run")
    );
    assert!(nodes.calls.lock().unwrap().is_empty());
}

struct PersistThenFailStore {
    inner: FileRunStore,
    fail_after_result_commit: Mutex<bool>,
}
impl RunStore for PersistThenFailStore {
    fn load(&self, id: &str) -> Result<Option<GraphRunRecord>, GraphError> {
        self.inner.load(id)
    }
    fn save(&self, record: &GraphRunRecord) -> Result<(), GraphError> {
        self.inner.save(record)?;
        let committed_result = record.cursor.is_none()
            && record.status == RunStatus::Running
            && record.results.values().any(|results| !results.is_empty());
        if committed_result {
            let mut fail = self.fail_after_result_commit.lock().unwrap();
            if *fail {
                *fail = false;
                return Err(GraphError::CorruptRun(
                    "injected feedback failure after durable Run save".into(),
                ));
            }
        }
        Ok(())
    }
    fn acquire_lease(&self, id: &str) -> Result<Box<dyn RunLease>, GraphError> {
        self.inner.acquire_lease(id)
    }
}
#[derive(Default)]
struct MemoryArtifacts {
    values: Mutex<BTreeMap<String, Value>>,
    freezes: Mutex<usize>,
}
impl ArtifactPort for MemoryArtifacts {
    fn freeze<'a>(
        &'a self,
        key: &'a InvocationKey,
        c: &'a NodeCompletion,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let id = key.durable_key();
            let mut values = self.values.lock().unwrap();
            if let Some(existing) = values.get(&id) {
                if existing != &c.output {
                    return Err(GraphError::CorruptRun("conflicting freeze retry".into()));
                }
            } else {
                values.insert(id.clone(), c.output.clone());
                *self.freezes.lock().unwrap() += 1;
            }
            Ok(CommitRef {
                id,
                node_id: key.node_id.clone(),
                invocation: key.invocation,
            })
        })
    }
    fn resolve<'a>(
        &'a self,
        c: &'a CommitRef,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            self.values
                .lock()
                .unwrap()
                .get(&c.id)
                .cloned()
                .ok_or_else(|| GraphError::CorruptRun("missing commit".into()))
        })
    }
}
#[derive(Default)]
struct FakeNodes {
    facts: Mutex<BTreeMap<String, CompletionFact>>,
    calls: Mutex<Vec<NodeExecutionRequest>>,
    budget_once: Mutex<bool>,
    uncertain: Mutex<bool>,
    caps_budget_off: bool,
    cancel_once: Mutex<bool>,
    op_run: bool,
    fail_once: Mutex<Option<String>>,
    interrupt_once: Mutex<BTreeMap<String, String>>,
    invalid_route_once: Mutex<bool>,
    recovery_once: Mutex<bool>,
    delays_ms: Mutex<BTreeMap<String, u64>>,
    completed_order: Mutex<Vec<String>>,
    active_calls: std::sync::atomic::AtomicUsize,
    max_active_calls: std::sync::atomic::AtomicUsize,
    graph_call_port: Option<Arc<FakeGraphCalls>>,
    resolved_plugins: Mutex<Vec<PluginBinding>>,
}
impl NodeExecutionPort for FakeNodes {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: true,
            op_run: self.op_run,
            exact_provider_request_budget: !self.caps_budget_off,
        }
    }
    fn graph_call_port(&self) -> Option<&dyn GraphCallPort> {
        self.graph_call_port
            .as_deref()
            .map(|port| port as &dyn GraphCallPort)
    }
    fn resolve_plugins(&self, ids: &[String]) -> Result<Vec<PluginBinding>, GraphError> {
        let bindings = self.resolved_plugins.lock().unwrap();
        ids.iter()
            .map(|id| {
                bindings
                    .iter()
                    .find(|binding| &binding.id == id)
                    .cloned()
                    .ok_or_else(|| GraphError::Unsupported(format!("unknown Plugin `{id}`")))
            })
            .collect()
    }
    fn completion_fact<'a>(
        &'a self,
        key: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            if *self.uncertain.lock().unwrap() {
                return Ok(CompletionFact::Uncertain("fake unknown".into()));
            }
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
            if self
                .facts
                .lock()
                .unwrap()
                .get(&request.key.durable_key())
                .is_some_and(|fact| !matches!(fact, CompletionFact::Resumable))
            {
                return Err(GraphError::CorruptRun(
                    "duplicate dispatch after completion fact".into(),
                ));
            }
            self.calls.lock().unwrap().push(request.clone());
            let active = self
                .active_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.max_active_calls
                .fetch_max(active, std::sync::atomic::Ordering::SeqCst);
            let delay = self
                .delays_ms
                .lock()
                .unwrap()
                .get(&request.key.node_id)
                .copied()
                .unwrap_or_default();
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            {
                let mut recovery = self.recovery_once.lock().unwrap();
                if *recovery {
                    *recovery = false;
                    self.active_calls
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    return Ok(NodeExecutionOutcome::WaitingRecovery {
                        attempts: vec![RecoveryAttempt {
                            attempt_id: 17,
                            step: 4,
                            tool: "publish".into(),
                            started_at: "2026-10-03T00:00:00Z".into(),
                        }],
                    });
                }
            }
            if let Some(reason) = self.fail_once.lock().unwrap().take() {
                self.facts.lock().unwrap().insert(
                    request.key.durable_key(),
                    CompletionFact::Failed(reason.clone()),
                );
                self.active_calls
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(NodeExecutionOutcome::Failed { reason });
            }
            if let Some(reason) = self
                .interrupt_once
                .lock()
                .unwrap()
                .remove(&request.key.node_id)
            {
                self.facts
                    .lock()
                    .unwrap()
                    .insert(request.key.durable_key(), CompletionFact::Resumable);
                self.active_calls
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(NodeExecutionOutcome::Interrupted { reason });
            }
            {
                let mut budget = self.budget_once.lock().unwrap();
                if *budget {
                    *budget = false;
                    self.active_calls
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    return Ok(NodeExecutionOutcome::BudgetExhausted { model_requests: 2 });
                }
            }
            {
                let mut cancel = self.cancel_once.lock().unwrap();
                if *cancel {
                    *cancel = false;
                    request.cancellation.store(true, Ordering::Relaxed);
                    self.active_calls
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    return Ok(NodeExecutionOutcome::Cancelled);
                }
            }
            let route = {
                let mut invalid = self.invalid_route_once.lock().unwrap();
                if *invalid {
                    *invalid = false;
                    Some("unlisted-route".into())
                } else {
                    request.routes.first().cloned()
                }
            };
            let completion = NodeCompletion {
                submission: format!("done:{}", request.key.node_id),
                route,
                model_requests: 1,
                output: serde_json::json!({"node":request.key.node_id,"input":request.input}),
            };
            self.facts.lock().unwrap().insert(
                request.key.durable_key(),
                CompletionFact::Completed(completion.clone()),
            );
            self.completed_order
                .lock()
                .unwrap()
                .push(request.key.node_id.clone());
            self.active_calls
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            Ok(NodeExecutionOutcome::Completed(completion))
        })
    }
}

#[derive(Default)]
struct FakeGraphCalls {
    outcomes: Mutex<Vec<GraphCallOutcome>>,
    identities: Mutex<Vec<String>>,
}
impl GraphCallPort for FakeGraphCalls {
    fn call<'a>(
        &'a self,
        identity: &'a CallIdentity,
        _spec: &'a Value,
        _input: &'a Value,
        _input_commits: &'a [CommitRef],
        _cancellation: crate::Cancellation,
    ) -> Pin<Box<dyn Future<Output = Result<GraphCallOutcome, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            self.identities.lock().unwrap().push(identity.durable_key());
            if self.outcomes.lock().unwrap().is_empty() {
                return Err(GraphError::CorruptRun(
                    "fake GraphCallPort exhausted".into(),
                ));
            }
            Ok(self.outcomes.lock().unwrap().remove(0))
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CrashPoint {
    NotStartedFactCheck,
    NodeFact,
    FailedFact,
    ArtifactCommit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct DurableTestArtifact {
    commit: CommitRef,
    output: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum DurableTestNodeFact {
    Completed(NodeCompletion),
    Failed(String),
}

struct DurableTestPorts {
    root: PathBuf,
    crash_point: Option<CrashPoint>,
}

impl DurableTestPorts {
    fn node_fact_path(&self, key: &InvocationKey) -> PathBuf {
        self.root.join(format!("node-{}.json", key.durable_key()))
    }

    fn artifact_path(&self, key: &InvocationKey) -> PathBuf {
        self.root
            .join(format!("artifact-{}.json", key.durable_key()))
    }

    fn execute_count_path(&self) -> PathBuf {
        self.root.join("execute-count")
    }

    fn freeze_count_path(&self) -> PathBuf {
        self.root.join("freeze-count")
    }
}

impl NodeExecutionPort for DurableTestPorts {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: true,
            op_run: false,
            exact_provider_request_budget: true,
        }
    }

    fn completion_fact<'a>(
        &'a self,
        key: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let path = self.node_fact_path(key);
            match fs::read(path) {
                Ok(bytes) => {
                    let fact: DurableTestNodeFact =
                        serde_json::from_slice(&bytes).map_err(GraphError::RunDecode)?;
                    match fact {
                        DurableTestNodeFact::Completed(completion) => {
                            Ok(CompletionFact::Completed(completion))
                        }
                        DurableTestNodeFact::Failed(reason) => Ok(CompletionFact::Failed(reason)),
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if self.crash_point == Some(CrashPoint::NotStartedFactCheck) {
                        std::process::exit(70);
                    }
                    Ok(CompletionFact::NotStarted)
                }
                Err(error) => Err(error.into()),
            }
        })
    }

    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            if self.node_fact_path(&request.key).exists() {
                return Err(GraphError::CorruptRun(
                    "durable test Node port received a duplicate dispatch".into(),
                ));
            }
            write_durable_bytes(&self.execute_count_path(), b"1")?;
            if self.crash_point == Some(CrashPoint::FailedFact) {
                let reason = "durable test failure".to_owned();
                write_durable_json(
                    &self.node_fact_path(&request.key),
                    &DurableTestNodeFact::Failed(reason),
                )?;
                std::process::exit(73);
            }
            let completion = NodeCompletion {
                submission: format!("completed:{}", request.key.node_id),
                route: request.routes.first().cloned(),
                model_requests: 1,
                output: serde_json::json!({"node":request.key.node_id,"input":request.input}),
            };
            write_durable_json(
                &self.node_fact_path(&request.key),
                &DurableTestNodeFact::Completed(completion.clone()),
            )?;
            if self.crash_point == Some(CrashPoint::NodeFact) {
                std::process::exit(71);
            }
            Ok(NodeExecutionOutcome::Completed(completion))
        })
    }
}

impl ArtifactPort for DurableTestPorts {
    fn freeze<'a>(
        &'a self,
        key: &'a InvocationKey,
        completion: &'a NodeCompletion,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let path = self.artifact_path(key);
            let artifact = match fs::read(&path) {
                Ok(bytes) => {
                    let existing: DurableTestArtifact =
                        serde_json::from_slice(&bytes).map_err(GraphError::RunDecode)?;
                    if existing.output != completion.output {
                        return Err(GraphError::CorruptRun(
                            "artifact retry changed the frozen output".into(),
                        ));
                    }
                    existing
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let artifact = DurableTestArtifact {
                        commit: CommitRef {
                            id: key.durable_key(),
                            node_id: key.node_id.clone(),
                            invocation: key.invocation,
                        },
                        output: completion.output.clone(),
                    };
                    write_durable_json(&path, &artifact)?;
                    write_durable_bytes(&self.freeze_count_path(), b"1")?;
                    artifact
                }
                Err(error) => return Err(error.into()),
            };
            if self.crash_point == Some(CrashPoint::ArtifactCommit)
                && !completion.submission.starts_with("fanout activation")
            {
                std::process::exit(72);
            }
            Ok(artifact.commit)
        })
    }

    fn resolve<'a>(
        &'a self,
        commit: &'a CommitRef,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let path = self.root.join(format!("artifact-{}.json", commit.id));
            let bytes = fs::read(path)?;
            let artifact: DurableTestArtifact =
                serde_json::from_slice(&bytes).map_err(GraphError::RunDecode)?;
            if artifact.commit != *commit {
                return Err(GraphError::CorruptRun(
                    "test artifact commit identity mismatch".into(),
                ));
            }
            Ok(artifact.output)
        })
    }
}

fn write_durable_json(path: &std::path::Path, value: &impl Serialize) -> Result<(), GraphError> {
    let bytes = serde_json::to_vec(value).map_err(GraphError::RunDecode)?;
    write_durable_bytes(path, &bytes)
}

fn write_durable_bytes(path: &std::path::Path, bytes: &[u8]) -> Result<(), GraphError> {
    let parent = path
        .parent()
        .ok_or_else(|| GraphError::CorruptRun("test durable path has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| GraphError::CorruptRun("test durable path is invalid".into()))?;
    let temporary = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), stamp));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

struct Control {
    pause: bool,
    stop: bool,
    token: crate::Cancellation,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            pause: false,
            stop: false,
            token: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}
impl RunControl for Control {
    fn pause_requested(&self) -> bool {
        self.pause
    }
    fn stop_requested(&self) -> bool {
        self.stop
    }
    fn cancellation(&self) -> crate::Cancellation {
        self.token.clone()
    }
}
fn graph(nodes: &[&str], edges: &[(&str, &str)], entry: &str) -> GraphSnapshot {
    let agents = BTreeMap::from([(
        "worker".to_string(),
        AgentDefinition {
            model: "m".into(),
            instructions: "base".into(),
            network: false,
            max_steps: None,
            wall_time_limit_seconds: None,
            reads: vec![],
            writes: vec![],
        },
    )]);
    GraphSnapshot {
        objective: "objective".into(),
        input: serde_json::json!({"defaults":{"a":1,"b":1}}),
        entry: entry.into(),
        agents,
        ops: BTreeMap::new(),
        nodes: nodes
            .iter()
            .map(|id| GraphNode {
                id: (*id).into(),
                agent: Some("worker".into()),
                op: None,
                input: None,
                plugins: vec![],
                max_rounds: None,
            })
            .collect(),
        edges: edges
            .iter()
            .map(|(a, b)| GraphEdge {
                from_node: (*a).into(),
                to_node: (*b).into(),
            })
            .collect(),
        module_rounds: BTreeMap::new(),
    }
}
fn parallel_graph() -> GraphSnapshot {
    let mut snapshot = graph(
        &["start", "left", "right", "collect", "after"],
        &[
            ("start", "left"),
            ("start", "right"),
            ("left", "collect"),
            ("right", "collect"),
            ("collect", "after"),
        ],
        "start",
    );
    snapshot.ops.insert(
        "fanout".into(),
        serde_json::json!({"fanout":{"join":"collect"}}),
    );
    snapshot
        .ops
        .insert("join".into(), serde_json::json!({"join":{}}));
    snapshot.nodes[0].agent = None;
    snapshot.nodes[0].op = Some("fanout".into());
    snapshot.nodes[3].agent = None;
    snapshot.nodes[3].op = Some("join".into());
    snapshot
}
fn setup() -> (MemStore, MemoryArtifacts, FakeNodes, Control) {
    (
        MemStore::default(),
        MemoryArtifacts::default(),
        FakeNodes::default(),
        Control::default(),
    )
}

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
    let (s, a, n, c) = setup();
    let record =
        GraphRunRecord::create(g, serde_json::json!({"defaults":{"b":2},"request":"x"})).unwrap();
    let runner = GraphRunner::new(&s, &a, &n, &c);
    let done = runner.run(record).await.unwrap();
    assert_eq!(done.status, RunStatus::Completed);
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
    assert!(
        next.task
            .contains("mounted read-only under `/in/<node-id>`")
    );
    assert!(next.task.contains("find /in"));
    assert!(next.task.contains("/workspace"));
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
    let (store, artifacts, nodes, control) = setup();
    let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
        .await
        .unwrap();

    assert_eq!(result.status, RunStatus::Completed);
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
    let (store, artifacts, nodes, control) = setup();
    let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
        .await
        .unwrap();

    assert_eq!(result.status, RunStatus::Completed);
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
    let (store, artifacts, nodes, control) = setup();
    let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
        .await
        .unwrap();

    assert_eq!(result.status, RunStatus::Completed);
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
            node_id: "entry".into(),
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
            node_id: "source".into(),
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
            node_id: "branch".into(),
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
    let (store, artifacts, mut nodes, control) = setup();
    nodes.op_run = true;
    let record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(record)
        .await
        .unwrap();
    assert_eq!(result.status, RunStatus::Completed);
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
async fn budget_stop_keeps_cursor_and_resume_uses_same_invocation() {
    let mut g = graph(&["one"], &[], "one");
    g.agents.get_mut("worker").unwrap().max_steps = Some(0);
    let (s, a, n, c) = setup();
    *n.budget_once.lock().unwrap() = true;
    let record = GraphRunRecord::create(g, Value::Null).unwrap();
    let run_id = record.run_id.clone();
    let runner = GraphRunner::new(&s, &a, &n, &c);
    let stopped = runner.run(record).await.unwrap();
    assert_eq!(stopped.status, RunStatus::BudgetStopped);
    assert!(stopped.cursor.is_some());
    assert_eq!(stopped.invocations["one"], 1);
    assert_eq!(stopped.passes["one"], 1);
    let durable = s.load(&run_id).unwrap().unwrap();
    assert_eq!(durable.invocations["one"], 1);
    assert_eq!(durable.passes["one"], 1);
    assert_eq!(
        n.facts
            .lock()
            .unwrap()
            .get(&stopped.cursor.as_ref().unwrap().key.durable_key()),
        None
    );
    assert_eq!(*a.freezes.lock().unwrap(), 0);
    let resumed = runner.run(stopped).await.unwrap();
    assert_eq!(resumed.status, RunStatus::Completed);
    let calls = n.calls.lock().unwrap();
    assert_eq!(calls[0].key, calls[1].key);
    assert_eq!(resumed.invocations["one"], 1);
    assert_eq!(resumed.passes["one"], 1);
}

#[tokio::test]
async fn serial_file_store_reload_resumes_same_invocation_after_interruption() {
    let root = std::env::temp_dir().join(format!(
        "anchor-serial-provider-retry-{}-{}",
        std::process::id(),
        RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let store = FileRunStore::new(&root);
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes {
        budget_once: Mutex::new(true),
        ..FakeNodes::default()
    };
    let control = Control::default();
    let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    let run_id = initial.run_id.clone();
    let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
    let paused = runner.run(initial).await.unwrap();
    assert_eq!(paused.status, RunStatus::BudgetStopped);
    let original_key = paused.cursor.as_ref().unwrap().key.clone();

    let loaded = store.load(&run_id).unwrap().unwrap();
    assert_eq!(loaded.cursor.as_ref().unwrap().key, original_key);
    let resumed = runner.run(loaded).await.unwrap();
    assert_eq!(resumed.status, RunStatus::Completed);
    assert_eq!(resumed.invocations["one"], 1);
    assert_eq!(resumed.passes["one"], 1);
    let calls = nodes.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].key, original_key);
    assert_eq!(calls[1].key, original_key);
    assert_eq!(resumed.results["one"].len(), 1);
    drop(calls);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn serial_interrupted_file_store_reload_resumes_same_invocation_and_clears_error() {
    let root = std::env::temp_dir().join(format!(
        "anchor-serial-interrupted-{}-{}",
        std::process::id(),
        RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let artifacts = MemoryArtifacts::default();
    let reason = "provider stream interrupted before final_result";
    let nodes = FakeNodes {
        interrupt_once: Mutex::new(BTreeMap::from([("work".into(), reason.into())])),
        ..FakeNodes::default()
    };
    let control = Control::default();
    let initial = GraphRunRecord::create(
        graph(
            &["before", "work", "after"],
            &[("before", "work"), ("work", "after")],
            "before",
        ),
        serde_json::json!({"request":"continue interrupted work"}),
    )
    .unwrap();
    let run_id = initial.run_id.clone();
    let stopped = {
        let store = FileRunStore::new(&root);
        GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(initial)
            .await
            .unwrap()
    };
    assert_eq!(stopped.status, RunStatus::Stopped);
    assert_eq!(stopped.error.as_deref(), Some(reason));
    assert!(stopped.recovery.is_empty());
    assert!(stopped.parallel.is_none());
    let cursor = stopped.cursor.as_ref().unwrap().clone();
    let request = nodes.calls.lock().unwrap()[1].clone();
    assert_eq!(cursor.node_id, "work");
    assert_eq!(cursor.key, request.key);
    assert_eq!(cursor.key.invocation, 1);
    assert_eq!(cursor.input_commits, request.input_commits);
    assert_eq!(
        cursor.input_commits,
        vec![stopped.results["before"][0].commit.clone()]
    );
    assert_eq!(cursor.prepared_input, request.input);
    assert_eq!(stopped.invocations["work"], 1);
    assert_eq!(stopped.passes["work"], 1);
    assert!(!stopped.results.contains_key("work"));
    assert!(!stopped.invocations.contains_key("after"));
    assert_eq!(
        nodes.facts.lock().unwrap().get(&cursor.key.durable_key()),
        Some(&CompletionFact::Resumable)
    );
    assert_eq!(*artifacts.freezes.lock().unwrap(), 1);

    let store = FileRunStore::new(&root);
    let loaded = store.load(&run_id).unwrap().unwrap();
    assert_eq!(loaded, stopped);
    loaded.validate().unwrap();
    let resumed = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(loaded)
        .await
        .unwrap();
    assert_eq!(resumed.status, RunStatus::Completed);
    assert!(resumed.cursor.is_none());
    assert!(resumed.recovery.is_empty());
    assert_eq!(resumed.results["before"], stopped.results["before"]);
    assert_eq!(resumed.results["work"].len(), 1);
    assert_eq!(resumed.results["work"][0].key, cursor.key);
    assert_eq!(resumed.results["after"].len(), 1);
    for node in ["before", "work", "after"] {
        assert_eq!(resumed.invocations[node], 1);
        assert_eq!(resumed.passes[node], 1);
    }
    let calls = nodes.calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[2].key, cursor.key);
    assert_eq!(calls[2].input_commits, cursor.input_commits);
    assert_eq!(calls[2].input, cursor.prepared_input);
    assert_eq!(calls[3].key.node_id, "after");
    drop(calls);
    assert_eq!(*artifacts.freezes.lock().unwrap(), 3);
    assert_eq!(store.load(&run_id).unwrap().unwrap(), resumed);
    fs::remove_dir_all(root).unwrap();
    assert!(
        resumed.error.is_none(),
        "completed Run retains interruption: {:?}",
        resumed.error
    );
}

#[tokio::test]
async fn graph_runner_dispatches_durable_resumable_node_with_same_invocation() {
    let nodes = FakeNodes::default();
    let store = MemStore::default();
    let artifacts = MemoryArtifacts::default();
    let control = Control::default();
    let snapshot = graph(&["one"], &[], "one");
    let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "one".into(),
        invocation: 1,
    };
    nodes
        .facts
        .lock()
        .unwrap()
        .insert(key.durable_key(), CompletionFact::Resumable);

    record = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(record)
        .await
        .unwrap();

    assert_eq!(record.status, RunStatus::Completed);
    assert_eq!(nodes.calls.lock().unwrap().len(), 1);
    assert_eq!(nodes.calls.lock().unwrap()[0].key, key);
    assert_eq!(record.invocations["one"], 1);
}

#[tokio::test]
async fn format_one_cursor_migrates_under_lease_and_persists_once() {
    let root = std::env::temp_dir().join(format!(
        "anchor-run-migration-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileRunStore::new(&root);
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes::default();
    let control = Control::default();
    let mut record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    let id = record.run_id.clone();
    record.cursor = Some(RunCursor {
        node_id: "one".into(),
        key: InvocationKey {
            run_id: id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: "one".into(),
            invocation: 1,
        },
        input_commits: vec![],
        prepared_input: Value::Null,
    });
    record.format = 1;
    record.status = RunStatus::BudgetStopped;
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join(format!("{id}.json")),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    // Loading migrates in memory, without writing outside the Runner lease.
    let loaded = store.load(&id).unwrap().unwrap();
    assert_eq!(loaded.format, 7);
    assert_eq!(loaded.invocations["one"], 1);
    assert_eq!(loaded.passes["one"], 1);
    let persisted: Value =
        serde_json::from_slice(&fs::read(root.join(format!("{id}.json"))).unwrap()).unwrap();
    assert_eq!(persisted["format"], 1);

    let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
    let result = runner.run(loaded).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    assert_eq!(result.invocations["one"], 1);
    assert_eq!(result.passes["one"], 1);
    let persisted = store.load(&id).unwrap().unwrap();
    assert_eq!(persisted.format, 7);
    assert_eq!(persisted.invocations["one"], 1);
    assert_eq!(persisted.passes["one"], 1);

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn format_one_terminal_run_migrates_without_changing_counters() {
    let root = std::env::temp_dir().join(format!(
        "anchor-run-terminal-migration-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileRunStore::new(&root);
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes::default();
    let control = Control::default();
    let mut record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    record.status = RunStatus::Completed;
    record.format = 1;
    record.passes.insert("one".into(), 3);
    record.invocations.insert("one".into(), 3);
    let id = record.run_id.clone();
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join(format!("{id}.json")),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let loaded = store.load(&id).unwrap().unwrap();
    assert_eq!(loaded.format, 7);
    assert_eq!(loaded.passes["one"], 3);
    assert_eq!(loaded.invocations["one"], 3);
    let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(loaded)
        .await
        .unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    let persisted = store.load(&id).unwrap().unwrap();
    assert_eq!(persisted.format, 7);
    assert_eq!(persisted.passes["one"], 3);
    assert_eq!(persisted.invocations["one"], 3);
    assert!(nodes.calls.lock().unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn known_node_failure_is_terminal_and_does_not_advance_edges() {
    let snapshot = graph(
        &["fails", "downstream"],
        &[("fails", "downstream")],
        "fails",
    );
    let (store, artifacts, nodes, control) = setup();
    *nodes.fail_once.lock().unwrap() = Some("provider rejected request".into());
    *store.fail_on.lock().unwrap() = Some(3);
    let record = GraphRunRecord::create(snapshot.clone(), Value::Null).unwrap();
    let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
    assert!(runner.run(record).await.is_err());
    let interrupted = store
        .load(&nodes.calls.lock().unwrap()[0].key.run_id)
        .unwrap()
        .unwrap();
    assert!(interrupted.cursor.is_some());
    let failed = runner.run(interrupted).await.unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert!(
        failed
            .error
            .as_deref()
            .unwrap()
            .contains("provider rejected request")
    );
    assert!(failed.error.as_deref().unwrap().contains("fails"));
    assert!(failed.cursor.is_none());
    assert_eq!(failed.invocations["fails"], 1);
    assert_eq!(failed.passes["fails"], 1);
    assert_eq!(nodes.calls.lock().unwrap().len(), 1);
    assert!(failed.decided.is_empty());
    assert!(!failed.invocations.contains_key("downstream"));
    assert!(!failed.passes.contains_key("downstream"));

    // Even an explicit second call with the durable terminal record cannot replay it.
    let terminal = runner.run(failed).await.unwrap();
    assert_eq!(terminal.status, RunStatus::Failed);
    assert_eq!(nodes.calls.lock().unwrap().len(), 1);
    assert_eq!(*artifacts.freezes.lock().unwrap(), 0);
}

#[tokio::test]
async fn invalid_durable_route_fails_run_without_settling_or_replaying() {
    let snapshot = graph(&["start", "next"], &[("start", "next")], "start");
    let (store, artifacts, nodes, control) = setup();
    *nodes.invalid_route_once.lock().unwrap() = true;
    *store.fail_on.lock().unwrap() = Some(3);
    let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
    let record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    let id = record.run_id.clone();
    assert!(runner.run(record).await.is_err());
    let interrupted = store.load(&id).unwrap().unwrap();
    assert!(interrupted.cursor.is_some());
    *store.fail_on.lock().unwrap() = None;
    let failed = runner.run(interrupted).await.unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert!(failed.error.as_deref().unwrap().contains("unlisted-route"));
    assert!(failed.cursor.is_none());
    assert_eq!(failed.invocations["start"], 1);
    assert_eq!(failed.passes["start"], 1);
    assert!(failed.decided.is_empty());
    assert!(failed.results.is_empty());
    assert_eq!(nodes.calls.lock().unwrap().len(), 1);
    let terminal = runner.run(failed).await.unwrap();
    assert_eq!(terminal.status, RunStatus::Failed);
    assert_eq!(nodes.calls.lock().unwrap().len(), 1);
    assert_eq!(*artifacts.freezes.lock().unwrap(), 0);
}

#[tokio::test]
async fn completion_fact_closes_run_store_crash_window_without_reexecution() {
    let (s, a, n, c) = setup();
    *s.fail_on.lock().unwrap() = Some(3);
    let record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    let id = record.run_id.clone();
    let runner = GraphRunner::new(&s, &a, &n, &c);
    assert!(runner.run(record).await.is_err());
    let recovered = s.load(&id).unwrap().unwrap();
    assert!(recovered.cursor.is_some());
    *s.fail_on.lock().unwrap() = None;
    let done = runner.run(recovered).await.unwrap();
    assert_eq!(done.status, RunStatus::Completed);
    assert_eq!(n.calls.lock().unwrap().len(), 1);
    assert_eq!(*a.freezes.lock().unwrap(), 1);
}

#[tokio::test]
async fn durable_run_save_feedback_error_requires_reloading_before_resume() {
    let root = std::env::temp_dir().join(format!(
        "anchor-run-save-feedback-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let store = PersistThenFailStore {
        inner: FileRunStore::new(&root),
        fail_after_result_commit: Mutex::new(true),
    };
    let ports = DurableTestPorts {
        root: root.clone(),
        crash_point: None,
    };
    let control = Control::default();
    let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    let run_id = initial.run_id.clone();

    let first = GraphRunner::new(&store, &ports, &ports, &control)
        .run(initial.clone())
        .await;
    assert!(first.is_err(), "wrapper must report its post-save error");

    // The atomic file replacement committed before the wrapper lost the
    // success feedback, so replaying the caller's old in-memory snapshot
    // must conflict with the durable Run facts.
    assert!(matches!(
        GraphRunner::new(&store, &ports, &ports, &control)
            .run(initial)
            .await,
        Err(GraphError::RunConflict)
    ));

    // Recovery starts from the durable latest record, using a fresh Runner
    // and the underlying FileRunStore adapter.
    let recovered_record = store.load(&run_id).unwrap().unwrap();
    assert_eq!(recovered_record.status, RunStatus::Running);
    assert!(recovered_record.cursor.is_none());
    assert_eq!(recovered_record.results["one"].len(), 1);
    let result = &recovered_record.results["one"][0];
    assert_eq!(result.commit.id, result.key.durable_key());
    let recovered = GraphRunner::new(&store.inner, &ports, &ports, &control)
        .run(recovered_record)
        .await
        .unwrap();

    assert_eq!(recovered.status, RunStatus::Completed);
    assert!(recovered.cursor.is_none());
    assert_eq!(recovered.results["one"].len(), 1);
    assert_eq!(recovered.invocations["one"], 1);
    assert_eq!(recovered.passes["one"], 1);
    assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
    assert_eq!(fs::read(root.join("freeze-count")).unwrap(), b"1");
    assert_eq!(store.inner.load(&run_id).unwrap().unwrap(), recovered);

    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn uncertain_and_pause_fail_closed_at_invocation_boundary() {
    let (s, a, n, mut c) = setup();
    *n.uncertain.lock().unwrap() = true;
    let record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    let runner = GraphRunner::new(&s, &a, &n, &c);
    let failed = runner.run(record).await.unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert!(n.calls.lock().unwrap().is_empty());
    assert!(failed.cursor.is_some());
    assert_eq!(failed.invocations["one"], 1);
    assert_eq!(failed.passes["one"], 1);
    assert!(failed.decided.is_empty());
    c.pause = true;
    let paused = GraphRunner::new(&s, &a, &n, &c)
        .run(GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap())
        .await
        .unwrap();
    assert_eq!(paused.status, RunStatus::Paused);
    assert!(paused.cursor.is_none());
}

#[tokio::test]
async fn stop_before_dispatch_and_executor_cancel_preserve_safe_cursor() {
    let (s, a, n, mut control) = setup();
    control.stop = true;
    let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    let stopped = GraphRunner::new(&s, &a, &n, &control)
        .run(initial)
        .await
        .unwrap();
    assert_eq!(stopped.status, RunStatus::Stopped);
    assert!(stopped.cursor.is_none());
    assert!(n.calls.lock().unwrap().is_empty());

    control.stop = false;
    *n.cancel_once.lock().unwrap() = true;
    let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    let stopped = GraphRunner::new(&s, &a, &n, &control)
        .run(initial)
        .await
        .unwrap();
    assert_eq!(stopped.status, RunStatus::Stopped);
    let cursor = stopped
        .cursor
        .as_ref()
        .expect("cancelled attempt stays resumable");
    assert_eq!(cursor.key.invocation, 1);
    assert_eq!(n.facts.lock().unwrap().get(&cursor.key.durable_key()), None);
}

#[tokio::test]
async fn run_rejects_cursor_identity_mismatch_before_dispatch() {
    let (store, artifacts, nodes, control) = setup();
    let mut record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    let key = InvocationKey {
        run_id: "another-run".into(),
        graph_digest: record.graph_digest.clone(),
        node_id: "one".into(),
        invocation: 1,
    };
    record.cursor = Some(RunCursor {
        node_id: "one".into(),
        key,
        input_commits: vec![],
        prepared_input: Value::Null,
    });
    assert!(matches!(
        GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(record)
            .await,
        Err(GraphError::CorruptRun(_))
    ));
    assert!(nodes.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn admission_accepts_paired_parallel_topology_and_runner_executes_control_ops() {
    let mut g = graph(&["one"], &[], "one");
    g.nodes[0].plugins = vec!["p".into()];
    let plugin_snapshot = GraphSnapshot::admit(serde_json::to_value(g).unwrap()).unwrap();
    let (plugin_store, plugin_artifacts, plugin_nodes, plugin_control) = setup();
    let rejected = GraphRunner::new(
        &plugin_store,
        &plugin_artifacts,
        &plugin_nodes,
        &plugin_control,
    )
    .run(GraphRunRecord::create(plugin_snapshot, Value::Null).unwrap())
    .await
    .unwrap();
    assert_eq!(rejected.status, RunStatus::Failed);
    assert!(
        rejected
            .error
            .as_deref()
            .unwrap()
            .contains("Plugin manifest resolution")
    );
    assert!(plugin_nodes.calls.lock().unwrap().is_empty());
    let g = parallel_graph();
    let admitted = GraphSnapshot::admit(serde_json::to_value(&g).unwrap()).unwrap();
    assert_eq!(
        admitted.parallel_regions().unwrap()["start"].branches,
        vec![vec!["left".to_string()], vec!["right".to_string()]]
    );
    let (s, a, n, c) = setup();
    let record = GraphRunRecord::create(admitted, Value::Null).unwrap();
    let result = GraphRunner::new(&s, &a, &n, &c).run(record).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    assert!(
        n.calls
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.key.node_id != "start" && request.key.node_id != "collect")
    );
    let mut g = graph(&["one"], &[], "one");
    g.agents.get_mut("worker").unwrap().max_steps = Some(2);
    let (s, a, mut n, c) = setup();
    n.caps_budget_off = true;
    let record = GraphRunRecord::create(g, Value::Null).unwrap();
    assert!(matches!(
        GraphRunner::new(&s, &a, &n, &c).run(record).await,
        Err(GraphError::Unsupported(_))
    ));
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
fn parallel_activation_facts_bind_pair_branch_paths_and_active_cursors() {
    let snapshot = parallel_graph();
    let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    record.status = RunStatus::Running;
    let fanout_key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "start".into(),
        invocation: 1,
    };
    record.invocations.insert("start".into(), 1);
    record.passes.insert("start".into(), 1);
    record.sequence = 1;
    record.results.insert(
        "start".into(),
        vec![RunResult {
            node_id: "start".into(),
            key: fanout_key,
            completion: NodeCompletion {
                submission: "fanout".into(),
                route: None,
                model_requests: 0,
                output: Value::Null,
            },
            commit: CommitRef {
                id: "start-1".into(),
                node_id: "start".into(),
                invocation: 1,
            },
            sequence: 1,
        }],
    );
    record.sequence = 3;
    for (sequence, target) in [(2, "left"), (3, "right")] {
        record.decided.insert(
            edge_key("start", target),
            EdgeDecision {
                selected: true,
                sequence,
                source_invocation: 1,
                result_sequence: 1,
            },
        );
    }
    let activation_id = format!("{}:start:1", record.run_id);
    record.parallel = Some(ParallelActivation {
        activation_id: activation_id.clone(),
        fanout_node: "start".into(),
        join_node: "collect".into(),
        fanout_invocation: 1,
        branches: ["left", "right"]
            .into_iter()
            .enumerate()
            .map(|(index, node)| ParallelBranchRecord {
                branch_id: format!("{activation_id}:branch:{index}"),
                entry: node.into(),
                nodes: vec![node.into()],
                next_index: 0,
                status: ParallelBranchStatus::Ready,
                cursor: None,
                completed: vec![],
                error: None,
            })
            .collect(),
    });
    record.validate().unwrap();

    let mut invalid_pair = record.clone();
    invalid_pair.parallel.as_mut().unwrap().join_node = "after".into();
    assert!(matches!(
        invalid_pair.validate(),
        Err(GraphError::CorruptRun(_))
    ));

    let mut invalid_cursor = record.clone();
    invalid_cursor.parallel.as_mut().unwrap().branches[0].cursor = Some(RunCursor {
        node_id: "left".into(),
        key: InvocationKey {
            run_id: invalid_cursor.run_id.clone(),
            graph_digest: invalid_cursor.graph_digest.clone(),
            node_id: "left".into(),
            invocation: 1,
        },
        input_commits: vec![],
        prepared_input: Value::Null,
    });
    assert!(matches!(
        invalid_cursor.validate(),
        Err(GraphError::CorruptRun(_))
    ));
}

#[test]
fn run_format_two_migrates_to_parallel_aware_format_three() {
    let mut record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    record.format = 2;
    let mut value = serde_json::to_value(&record).unwrap();
    value.as_object_mut().unwrap().remove("parallel");
    let mut loaded: GraphRunRecord = serde_json::from_value(value).unwrap();
    loaded.migrate_format().unwrap();
    assert_eq!(loaded.format, 7);
    assert!(loaded.parallel.is_none());
    loaded.validate().unwrap();
}

#[test]
fn run_format_three_migrates_graph_calls_and_plugin_bindings_to_current_format() {
    let mut value = serde_json::to_value(
        GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap(),
    )
    .unwrap();
    value["format"] = serde_json::json!(3);
    value.as_object_mut().unwrap().remove("graph_calls");
    value.as_object_mut().unwrap().remove("plugin_bindings");
    let mut record: GraphRunRecord = serde_json::from_value(value).unwrap();
    record.migrate_format().unwrap();
    assert_eq!(record.format, 7);
    assert!(record.graph_calls.is_empty());
    assert!(record.plugin_bindings.is_empty());
    record.validate().unwrap();
}

#[test]
fn run_format_six_migrates_recovery_submissions_to_current_format() {
    let mut value = serde_json::to_value(
        GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap(),
    )
    .unwrap();
    value["format"] = serde_json::json!(6);
    value
        .as_object_mut()
        .unwrap()
        .remove("recovery_submissions");
    let mut record: GraphRunRecord = serde_json::from_value(value).unwrap();
    record.migrate_format().unwrap();
    assert_eq!(record.format, 7);
    assert!(record.recovery_submissions.is_empty());
    record.validate().unwrap();
}

#[tokio::test]
async fn parallel_branches_complete_out_of_order_and_join_receives_all_branch_facts() {
    let (store, artifacts, mut nodes, control) = setup();
    nodes.op_run = true;
    nodes.delays_ms.lock().unwrap().insert("left".into(), 40);
    let initial = GraphRunRecord::create(parallel_graph(), Value::Null).unwrap();
    let run = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(initial)
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(
        &nodes.completed_order.lock().unwrap()[..2],
        &["right".to_string(), "left".to_string()]
    );
    assert_eq!(
        nodes
            .max_active_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "both branch NodeExecutionPort calls must overlap"
    );
    let calls = nodes.calls.lock().unwrap();
    assert!(!calls.iter().any(|request| request.key.node_id == "collect"));
    let join = run.results.get("collect").unwrap().first().unwrap();
    let manifest_branches = join.completion.output["branches"].as_array().unwrap();
    assert_eq!(manifest_branches.len(), 2);
    for branch in manifest_branches {
        for node in branch["nodes"].as_array().unwrap() {
            let node_id = node["node"].as_str().unwrap();
            let expected_commit = &run.results[node_id].first().unwrap().commit;
            assert_eq!(
                node["commit"],
                serde_json::to_value(expected_commit).unwrap()
            );
        }
    }
    assert_eq!(
        run.results.get("collect").unwrap().len(),
        1,
        "join executes exactly once after all branches settle"
    );
}

#[tokio::test]
async fn parallel_branch_failure_is_terminal_and_never_dispatches_join() {
    let (store, artifacts, mut nodes, control) = setup();
    nodes.op_run = true;
    nodes.delays_ms.lock().unwrap().insert("right".into(), 40);
    *nodes.fail_once.lock().unwrap() = Some("branch rejected".into());
    let initial = GraphRunRecord::create(parallel_graph(), Value::Null).unwrap();
    let failed = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(initial)
        .await
        .unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert!(failed.parallel.is_some());
    assert!(failed.error.as_deref().unwrap().contains("branch rejected"));
    assert!(
        !nodes
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.key.node_id == "collect")
    );
    assert!(!failed.results.contains_key("collect"));
    assert_eq!(
        *artifacts.freezes.lock().unwrap(),
        2,
        "fanout and successful sibling facts remain committed"
    );
}

#[tokio::test]
async fn parallel_resume_keeps_completed_branch_and_reuses_budget_cursor() {
    let (store, artifacts, mut nodes, control) = setup();
    nodes.op_run = true;
    *nodes.budget_once.lock().unwrap() = true;
    let initial = GraphRunRecord::create(parallel_graph(), Value::Null).unwrap();
    let run_id = initial.run_id.clone();
    let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
    let stopped = runner.run(initial).await.unwrap();
    assert_eq!(stopped.status, RunStatus::BudgetStopped);
    let durable = store.load(&run_id).unwrap().unwrap();
    let completed_before_resume = durable
        .parallel
        .as_ref()
        .unwrap()
        .branches
        .iter()
        .find(|branch| branch.status == ParallelBranchStatus::Completed)
        .unwrap()
        .entry
        .clone();
    let calls_before = nodes.calls.lock().unwrap().clone();
    let resumed = runner.run(durable).await.unwrap();
    assert_eq!(resumed.status, RunStatus::Completed);
    let calls_after = nodes.calls.lock().unwrap();
    assert_eq!(
        calls_after
            .iter()
            .filter(|request| request.key.node_id == completed_before_resume)
            .count(),
        1
    );
    let pending_before = calls_before
        .iter()
        .find(|request| request.key.node_id != completed_before_resume)
        .unwrap();
    let pending_after = calls_after
        .iter()
        .filter(|request| request.key.node_id == pending_before.key.node_id)
        .collect::<Vec<_>>();
    assert_eq!(pending_after.len(), 2);
    assert_eq!(pending_after[0].key, pending_after[1].key);
    assert_eq!(
        resumed.results.get("collect").unwrap().len(),
        1,
        "join runs only once after resumed branch completion"
    );
    assert!(*artifacts.freezes.lock().unwrap() >= 5);
}

#[tokio::test]
async fn parallel_file_store_roundtrip_preserves_activation_and_branch_cursors() {
    let root = std::env::temp_dir().join(format!(
        "anchor-parallel-roundtrip-{}-{}",
        std::process::id(),
        RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let store = FileRunStore::new(&root);
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes {
        op_run: true,
        ..FakeNodes::default()
    };
    *nodes.budget_once.lock().unwrap() = true;
    let control = Control::default();
    let initial = GraphRunRecord::create(parallel_graph(), Value::Null).unwrap();
    let run_id = initial.run_id.clone();
    let stopped = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(initial)
        .await
        .unwrap();
    assert_eq!(stopped.status, RunStatus::BudgetStopped);
    let loaded = store.load(&run_id).unwrap().unwrap();
    let activation = loaded.parallel.as_ref().unwrap();
    assert_eq!(activation.branches.len(), 2);
    assert!(
        activation
            .branches
            .iter()
            .any(|branch| branch.cursor.is_some())
    );
    assert_eq!(loaded.format, 7);
    loaded.validate().unwrap();
    let completed_before_resume = activation
        .branches
        .iter()
        .find(|branch| branch.status == ParallelBranchStatus::Completed)
        .unwrap()
        .entry
        .clone();
    let pending_key = activation
        .branches
        .iter()
        .find_map(|branch| branch.cursor.as_ref().map(|cursor| cursor.key.clone()))
        .unwrap();
    let calls_before_resume = nodes.calls.lock().unwrap().clone();
    let resumed = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(loaded)
        .await
        .unwrap();
    assert_eq!(resumed.status, RunStatus::Completed);
    assert!(resumed.parallel.is_none());
    let calls = nodes.calls.lock().unwrap();
    let resumed_pending = calls
        .iter()
        .filter(|request| request.key == pending_key)
        .collect::<Vec<_>>();
    assert_eq!(resumed_pending.len(), 2);
    assert!(
        calls_before_resume
            .iter()
            .any(|request| request.key == pending_key)
    );
    assert_eq!(
        calls
            .iter()
            .filter(|request| request.key.node_id == completed_before_resume)
            .count(),
        1,
        "completed sibling is not replayed after reload"
    );
    assert_eq!(resumed.results["collect"].len(), 1);
    drop(calls);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn parallel_cancelled_branch_can_reload_without_replaying_completed_sibling() {
    let root = std::env::temp_dir().join(format!(
        "anchor-parallel-cancel-{}-{}",
        std::process::id(),
        RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let store = FileRunStore::new(&root);
    let artifacts = MemoryArtifacts::default();
    let nodes = FakeNodes {
        op_run: true,
        ..FakeNodes::default()
    };
    *nodes.cancel_once.lock().unwrap() = true;
    nodes.delays_ms.lock().unwrap().insert("left".into(), 30);
    let control = Control::default();
    let initial = GraphRunRecord::create(parallel_graph(), Value::Null).unwrap();
    let run_id = initial.run_id.clone();
    let runner = GraphRunner::new(&store, &artifacts, &nodes, &control);
    let stopped = runner.run(initial).await.unwrap();
    assert_eq!(stopped.status, RunStatus::Stopped);
    let loaded = store.load(&run_id).unwrap().unwrap();
    let completed_before = loaded
        .parallel
        .as_ref()
        .unwrap()
        .branches
        .iter()
        .filter(|branch| branch.status == ParallelBranchStatus::Completed)
        .map(|branch| branch.entry.clone())
        .collect::<BTreeSet<_>>();
    let resumed = runner.run(loaded).await.unwrap();
    assert_eq!(resumed.status, RunStatus::Completed);
    let after = nodes.calls.lock().unwrap();
    for node in ["left", "right"] {
        let count = after
            .iter()
            .filter(|request| request.key.node_id == node)
            .count();
        if completed_before.contains(node) {
            assert_eq!(count, 1, "completed branch {node} must not replay");
        }
    }
    assert_eq!(resumed.results.get("collect").unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn parallel_interrupted_file_store_reload_preserves_completed_branch_and_clears_error() {
    let root = std::env::temp_dir().join(format!(
        "anchor-parallel-interrupted-{}-{}",
        std::process::id(),
        RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let artifacts = MemoryArtifacts::default();
    let reason = "left branch provider stream interrupted before final_result";
    let nodes = FakeNodes {
        op_run: true,
        interrupt_once: Mutex::new(BTreeMap::from([("left".into(), reason.into())])),
        ..FakeNodes::default()
    };
    let control = Control::default();
    let initial = GraphRunRecord::create(parallel_graph(), Value::Null).unwrap();
    let run_id = initial.run_id.clone();
    let stopped = {
        let store = FileRunStore::new(&root);
        GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(initial)
            .await
            .unwrap()
    };
    assert_eq!(stopped.status, RunStatus::Stopped);
    assert_eq!(stopped.error.as_deref(), Some(reason));
    assert!(stopped.cursor.is_none());
    assert!(stopped.recovery.is_empty());
    let activation = stopped.parallel.as_ref().unwrap();
    let pending = activation
        .branches
        .iter()
        .find(|branch| branch.entry == "left")
        .unwrap();
    let cursor = pending.cursor.as_ref().unwrap().clone();
    assert_eq!(pending.next_index, 0);
    assert!(pending.completed.is_empty());
    let completed = activation
        .branches
        .iter()
        .find(|branch| branch.entry == "right")
        .unwrap();
    assert_eq!(completed.status, ParallelBranchStatus::Completed);
    assert!(completed.cursor.is_none());
    assert_eq!(
        completed.completed,
        vec![stopped.results["right"][0].commit.clone()]
    );
    let calls = nodes.calls.lock().unwrap().clone();
    let request = calls
        .iter()
        .find(|request| request.key.node_id == "left")
        .unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(cursor.key, request.key);
    assert_eq!(cursor.key.invocation, 1);
    assert_eq!(cursor.input_commits, request.input_commits);
    assert_eq!(
        cursor.input_commits,
        vec![stopped.results["start"][0].commit.clone()]
    );
    assert_eq!(cursor.prepared_input, request.input);
    for node in ["start", "left", "right"] {
        assert_eq!(stopped.invocations[node], 1);
        assert_eq!(stopped.passes[node], 1);
    }
    assert!(!stopped.results.contains_key("left"));
    assert!(!stopped.invocations.contains_key("collect"));
    assert!(!stopped.invocations.contains_key("after"));
    assert_eq!(
        nodes.facts.lock().unwrap().get(&cursor.key.durable_key()),
        Some(&CompletionFact::Resumable)
    );
    assert_eq!(*artifacts.freezes.lock().unwrap(), 2);

    let store = FileRunStore::new(&root);
    let loaded = store.load(&run_id).unwrap().unwrap();
    assert_eq!(loaded, stopped);
    loaded.validate().unwrap();
    let resumed = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(loaded)
        .await
        .unwrap();
    assert_eq!(resumed.status, RunStatus::Completed);
    assert!(resumed.cursor.is_none());
    assert!(resumed.parallel.is_none());
    assert!(resumed.recovery.is_empty());
    assert_eq!(resumed.results["right"], stopped.results["right"]);
    assert_eq!(resumed.results["start"], stopped.results["start"]);
    assert_eq!(resumed.results["left"].len(), 1);
    assert_eq!(resumed.results["left"][0].key, cursor.key);
    assert_eq!(resumed.results["collect"].len(), 1);
    assert_eq!(resumed.results["after"].len(), 1);
    for node in ["start", "left", "right", "collect", "after"] {
        assert_eq!(resumed.invocations[node], 1);
        assert_eq!(resumed.passes[node], 1);
    }
    let calls = nodes.calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    let pending_calls = calls
        .iter()
        .filter(|request| request.key.node_id == "left")
        .collect::<Vec<_>>();
    assert_eq!(pending_calls.len(), 2);
    for request in pending_calls {
        assert_eq!(request.key, cursor.key);
        assert_eq!(request.input_commits, cursor.input_commits);
        assert_eq!(request.input, cursor.prepared_input);
    }
    for node in ["right", "after"] {
        assert_eq!(
            calls
                .iter()
                .filter(|request| request.key.node_id == node)
                .count(),
            1
        );
    }
    assert!(calls.iter().all(|request| request.key.node_id != "collect"));
    drop(calls);
    assert_eq!(*artifacts.freezes.lock().unwrap(), 5);
    assert_eq!(store.load(&run_id).unwrap().unwrap(), resumed);
    fs::remove_dir_all(root).unwrap();
    assert!(
        resumed.error.is_none(),
        "completed Run retains interruption: {:?}",
        resumed.error
    );
}

#[tokio::test]
async fn parallel_branch_reuses_node_round_ceiling_before_dispatch() {
    let (store, artifacts, mut nodes, control) = setup();
    nodes.op_run = true;
    let mut snapshot = parallel_graph();
    snapshot.nodes[1].max_rounds = Some(1);
    let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    record.passes.insert("left".into(), 1);
    let failed = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(record)
        .await
        .unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("left") && error.contains("ceiling"))
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
async fn parallel_region_counts_module_activation_once_and_enforces_reentry_ceiling() {
    let (store, artifacts, mut nodes, control) = setup();
    nodes.op_run = true;
    let mut snapshot = graph(
        &["start", "module/left", "module/right", "collect", "after"],
        &[
            ("start", "module/left"),
            ("start", "module/right"),
            ("module/left", "collect"),
            ("module/right", "collect"),
            ("collect", "after"),
            ("after", "start"),
        ],
        "start",
    );
    snapshot.module_rounds.insert("module".into(), 1);
    snapshot.ops.insert(
        "fanout".into(),
        serde_json::json!({"fanout":{"join":"collect"}}),
    );
    snapshot
        .ops
        .insert("join".into(), serde_json::json!({"join":{}}));
    snapshot.nodes[0].agent = None;
    snapshot.nodes[0].op = Some("fanout".into());
    snapshot.nodes[3].agent = None;
    snapshot.nodes[3].op = Some("join".into());
    let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(snapshot, Value::Null).unwrap())
        .await
        .unwrap();
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(result.module_activations.get("module"), Some(&1));
    assert!(result.ceased.contains("module@1"));
    assert_eq!(
        nodes
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.key.node_id == "module/left")
            .count(),
        1
    );
    assert_eq!(
        nodes
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.key.node_id == "module/right")
            .count(),
        1
    );
}

#[tokio::test]
async fn loop_reentry_uses_fresh_selected_back_edge_and_ceiling() {
    let mut g = graph(
        &["gather", "review"],
        &[("gather", "review"), ("review", "gather")],
        "gather",
    );
    g.nodes[0].max_rounds = Some(2);
    let (s, a, n, c) = setup();
    let run = GraphRunRecord::create(g, Value::Null).unwrap();
    let result = GraphRunner::new(&s, &a, &n, &c).run(run).await.unwrap();
    assert_eq!(
        n.calls
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.key.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["gather", "review", "gather", "review"]
    );
    assert_eq!(result.status, RunStatus::Stopped);
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
    let (store, artifacts, nodes, control) = setup();
    let run = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
        .run(run)
        .await
        .unwrap();
    assert_eq!(result.status, RunStatus::Stopped);
    assert_eq!(result.module_activations["module"], 2);
    assert_eq!(result.passes["module/a"], 1);
    assert_eq!(result.invocations["module/a"], 2);
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
fn file_run_store_roundtrips_rejects_corruption_and_releases_lease_after_process_crash() {
    let root = std::env::temp_dir().join(format!(
        "anchor-run-store-crash-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileRunStore::new(&root);
    let record = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    store.save(&record).unwrap();
    assert_eq!(store.load(&record.run_id).unwrap(), Some(record.clone()));
    let lease = store.acquire_lease(&record.run_id).unwrap();
    assert!(matches!(
        store.acquire_lease(&record.run_id),
        Err(GraphError::RunBusy(_))
    ));
    drop(lease);
    let reopened = store.acquire_lease(&record.run_id).unwrap();
    drop(reopened);

    // Hold the lease in another test process and synchronize using pipes:
    // the child announces only after acquiring the OS lock, and exits via
    // process::exit so Rust destructors cannot release it explicitly.
    let token = format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let executable = std::env::current_exe().unwrap();
    let mut child = Command::new(&executable)
        .args([
            "--exact",
            "graph::tests::file_run_store_lease_process_helper",
            "--nocapture",
        ])
        .env("ANCHOR_FILE_LEASE_TEST_TOKEN", &token)
        .env("ANCHOR_FILE_LEASE_TEST_MODE", "holder")
        .env("ANCHOR_FILE_LEASE_TEST_ROOT", &root)
        .env("ANCHOR_FILE_LEASE_TEST_RUN", &record.run_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let child_stdout = child.stdout.take().unwrap();
    let mut child_output = io::BufReader::new(child_stdout);
    let marker = format!("ANCHOR_FILE_LEASE_HELD:{token}");
    let mut line = String::new();
    let mut acquired = false;
    loop {
        line.clear();
        let bytes = child_output.read_line(&mut line).unwrap();
        if bytes == 0 {
            break;
        }
        if line.trim_end() == marker {
            acquired = true;
            break;
        }
    }
    if !acquired {
        let output = child.wait_with_output().unwrap();
        panic!(
            "lease holder exited before announcing acquisition; status={}, stdout={}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let contender = Command::new(&executable)
        .args([
            "--exact",
            "graph::tests::file_run_store_lease_process_helper",
            "--nocapture",
        ])
        .env("ANCHOR_FILE_LEASE_TEST_TOKEN", &token)
        .env("ANCHOR_FILE_LEASE_TEST_MODE", "contender")
        .env("ANCHOR_FILE_LEASE_TEST_ROOT", &root)
        .env("ANCHOR_FILE_LEASE_TEST_RUN", &record.run_id)
        .output();
    let release_result = writeln!(child.stdin.as_mut().unwrap(), "exit-crash:{token}");
    drop(child.stdin.take());
    let child_status = child.wait().unwrap();
    release_result.unwrap();
    assert!(
        child_status.success(),
        "lease holder exited unsuccessfully: {child_status}"
    );

    let contender = contender.unwrap();
    assert!(
        contender.status.success(),
        "lease contender failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&contender.stdout),
        String::from_utf8_lossy(&contender.stderr)
    );
    let contender_output = String::from_utf8_lossy(&contender.stdout);
    assert!(
        contender_output.contains(&format!("ANCHOR_FILE_LEASE_BUSY:{token}")),
        "contending process did not observe RunBusy: {contender_output}"
    );

    let after_crash = store.acquire_lease(&record.run_id).unwrap();
    drop(after_crash);

    fs::write(root.join(format!("{}.json", record.run_id)), b"not json").unwrap();
    assert!(matches!(
        store.load(&record.run_id),
        Err(GraphError::RunDecode(_))
    ));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn file_run_store_rejects_damaged_result_and_edge_facts() {
    let root = std::env::temp_dir().join(format!(
        "anchor-run-validation-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileRunStore::new(&root);
    let (memory_store, artifacts, nodes, control) = setup();
    let valid = GraphRunner::new(&memory_store, &artifacts, &nodes, &control)
        .run(
            GraphRunRecord::create(
                graph(&["one", "two"], &[("one", "two")], "one"),
                Value::Null,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(valid.status, RunStatus::Completed);
    let id = valid.run_id.clone();
    let path = root.join(format!("{id}.json"));

    let mut bad_records = Vec::new();
    let mut bad = valid.clone();
    bad.results.get_mut("one").unwrap()[0].node_id = "two".into();
    bad_records.push(bad);
    let mut bad = valid.clone();
    bad.results.get_mut("one").unwrap()[0].key.node_id = "two".into();
    bad_records.push(bad);
    let mut bad = valid.clone();
    bad.results.get_mut("one").unwrap()[0].commit.node_id = "two".into();
    bad_records.push(bad);
    let mut bad = valid.clone();
    bad.results.get_mut("one").unwrap()[0].sequence = valid.sequence + 1;
    bad_records.push(bad);
    let mut bad = valid.clone();
    let duplicate_sequence = bad.results["one"][0].sequence;
    bad.results.get_mut("two").unwrap()[0].sequence = duplicate_sequence;
    bad_records.push(bad);
    let mut bad = valid.clone();
    let selected = bad.decided.get_mut("one|two").unwrap();
    selected.result_sequence = valid.sequence + 1;
    bad_records.push(bad);
    let mut bad = valid.clone();
    bad.decided.insert(
        "one|ghost".into(),
        EdgeDecision {
            selected: false,
            sequence: valid.sequence,
            source_invocation: 0,
            result_sequence: 0,
        },
    );
    bad_records.push(bad);
    let mut bad = valid.clone();
    bad.decided.get_mut("one|two").unwrap().sequence = valid.sequence + 1;
    bad_records.push(bad);
    let cursor = RunCursor {
        node_id: "two".into(),
        key: InvocationKey {
            run_id: valid.run_id.clone(),
            graph_digest: valid.graph_digest.clone(),
            node_id: "two".into(),
            invocation: 1,
        },
        input_commits: vec![],
        prepared_input: Value::Null,
    };
    let mut bad = valid.clone();
    bad.cursor = Some(cursor.clone());
    bad_records.push(bad); // Completed + cursor.
    let mut bad = valid.clone();
    bad.status = RunStatus::BudgetStopped;
    bad.cursor = None;
    bad_records.push(bad); // BudgetStopped without resumable cursor.
    let mut bad = valid.clone();
    bad.status = RunStatus::Ready;
    bad.cursor = Some(cursor);
    bad_records.push(bad); // Ready + cursor.

    let mut valid_cursor = valid.clone();
    valid_cursor.status = RunStatus::BudgetStopped;
    valid_cursor.invocations.insert("two".into(), 2);
    valid_cursor.passes.insert("two".into(), 2);
    valid_cursor.cursor = Some(RunCursor {
        node_id: "two".into(),
        key: InvocationKey {
            run_id: valid.run_id.clone(),
            graph_digest: valid.graph_digest.clone(),
            node_id: "two".into(),
            invocation: 2,
        },
        input_commits: vec![valid.results["one"][0].commit.clone()],
        prepared_input: Value::Null,
    });
    valid_cursor.validate().unwrap();
    let mut bad = valid_cursor.clone();
    bad.cursor.as_mut().unwrap().input_commits[0].id = "foreign-commit".into();
    bad_records.push(bad);
    let mut bad = valid_cursor.clone();
    bad.cursor.as_mut().unwrap().input_commits.clear();
    bad_records.push(bad);
    let mut bad = valid_cursor.clone();
    bad.cursor
        .as_mut()
        .unwrap()
        .input_commits
        .push(valid.results["one"][0].commit.clone());
    bad_records.push(bad);

    for bad in bad_records {
        assert!(matches!(store.save(&bad), Err(GraphError::CorruptRun(_))));
        assert!(
            !path.exists(),
            "invalid save must not create final run file"
        );
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(matches!(store.load(&id), Err(GraphError::CorruptRun(_))));
        fs::remove_file(&path).unwrap();
    }

    let mut uncertain = valid.clone();
    uncertain.status = RunStatus::Failed;
    uncertain.error = Some("uncertain node result for two: outcome unavailable".into());
    uncertain.invocations.insert("two".into(), 2);
    uncertain.passes.insert("two".into(), 2);
    uncertain.cursor = Some(RunCursor {
        node_id: "two".into(),
        key: InvocationKey {
            run_id: valid.run_id.clone(),
            graph_digest: valid.graph_digest.clone(),
            node_id: "two".into(),
            invocation: 2,
        },
        input_commits: vec![valid.results["one"][0].commit.clone()],
        prepared_input: Value::Null,
    });
    // Failed + cursor is retained as a representable uncertain-result fact.
    store.save(&uncertain).unwrap();
    assert_eq!(store.load(&id).unwrap(), Some(uncertain));

    let mut loop_snapshot = graph(&["spin"], &[("spin", "spin")], "spin");
    loop_snapshot.nodes[0].max_rounds = Some(2);
    let looped = GraphRunner::new(&memory_store, &artifacts, &nodes, &control)
        .run(GraphRunRecord::create(loop_snapshot, Value::Null).unwrap())
        .await
        .unwrap();
    assert_eq!(looped.results["spin"].len(), 2);
    let loop_id = looped.run_id.clone();
    let loop_path = root.join(format!("{loop_id}.json"));
    let mut reversed = looped.clone();
    reversed.results.get_mut("spin").unwrap().reverse();
    assert!(matches!(
        store.save(&reversed),
        Err(GraphError::CorruptRun(_))
    ));
    assert!(!loop_path.exists());
    fs::create_dir_all(&root).unwrap();
    fs::write(&loop_path, serde_json::to_vec(&reversed).unwrap()).unwrap();
    assert!(matches!(
        store.load(&loop_id),
        Err(GraphError::CorruptRun(_))
    ));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn file_run_store_rejects_edge_decision_not_newer_than_its_result() {
    let root = std::env::temp_dir().join(format!(
        "anchor-self-loop-freshness-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileRunStore::new(&root);
    let mut record =
        GraphRunRecord::create(graph(&["spin"], &[("spin", "spin")], "spin"), Value::Null).unwrap();
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "spin".into(),
        invocation: 1,
    };
    let result_sequence = 1;
    record.results.insert(
        "spin".into(),
        vec![RunResult {
            node_id: "spin".into(),
            key: key.clone(),
            completion: NodeCompletion {
                submission: "spin completed".into(),
                route: Some("spin".into()),
                model_requests: 1,
                output: Value::Null,
            },
            commit: CommitRef {
                id: key.durable_key(),
                node_id: "spin".into(),
                invocation: 1,
            },
            sequence: result_sequence,
        }],
    );
    record.invocations.insert("spin".into(), 1);
    record.passes.insert("spin".into(), 1);
    record.sequence = 2;
    record.status = RunStatus::Stopped;
    record.decided.insert(
        "spin|spin".into(),
        EdgeDecision {
            selected: true,
            sequence: 2,
            source_invocation: 1,
            result_sequence,
        },
    );

    // This is a valid completed node result whose self-loop was selected;
    // the Run was then stopped before the next invocation was dispatched.
    store.save(&record).unwrap();
    let id = record.run_id.clone();
    assert_eq!(store.load(&id).unwrap(), Some(record.clone()));

    let mut stale = record.clone();
    stale.decided.get_mut("spin|spin").unwrap().sequence = result_sequence;
    assert!(matches!(store.save(&stale), Err(GraphError::CorruptRun(_))));

    let path = root.join(format!("{id}.json"));
    fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
    assert!(matches!(store.load(&id), Err(GraphError::CorruptRun(_))));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn file_run_store_lease_process_helper() {
    // This test is intentionally inert when run by the normal harness. The
    // parent test supplies a per-invocation token and a private temp root.
    let (Ok(token), Ok(mode), Ok(root), Ok(run_id)) = (
        std::env::var("ANCHOR_FILE_LEASE_TEST_TOKEN"),
        std::env::var("ANCHOR_FILE_LEASE_TEST_MODE"),
        std::env::var("ANCHOR_FILE_LEASE_TEST_ROOT"),
        std::env::var("ANCHOR_FILE_LEASE_TEST_RUN"),
    ) else {
        return;
    };
    if token.is_empty()
        || !["holder", "contender"].contains(&mode.as_str())
        || !PathBuf::from(&root)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("anchor-run-store-crash-"))
    {
        return;
    }

    let store = FileRunStore::new(root);
    if mode == "contender" {
        match store.acquire_lease(&run_id) {
            Err(GraphError::RunBusy(_)) => println!("ANCHOR_FILE_LEASE_BUSY:{token}"),
            Ok(lease) => {
                drop(lease);
                println!("ANCHOR_FILE_LEASE_ACQUIRED:{token}");
            }
            Err(error) => panic!("contender failed to check lease: {error}"),
        }
        return;
    }

    let _lease = store.acquire_lease(&run_id).unwrap();
    println!("ANCHOR_FILE_LEASE_HELD:{token}");
    io::stdout().flush().unwrap();
    let mut command = String::new();
    io::stdin().read_line(&mut command).unwrap();
    assert_eq!(command.trim_end(), format!("exit-crash:{token}"));
    // Deliberately bypass `drop(lease)`: process termination must release
    // the kernel lock while leaving its stable lock file/inode in place.
    std::process::exit(0);
    #[allow(unreachable_code)]
    drop(_lease);
}

#[tokio::test]
async fn file_run_store_recovers_completed_node_and_artifact_facts_after_process_crash() {
    let executable = std::env::current_exe().unwrap();
    for (crash_name, expected_exit) in [("after-node-fact", 71), ("after-artifact-commit", 72)] {
        let root = std::env::temp_dir().join(format!(
            "anchor-graph-recovery-{}-{}-{crash_name}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let store = FileRunStore::new(&root);
        let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
        let run_id = initial.run_id.clone();
        write_durable_json(&root.join("initial.json"), &initial).unwrap();

        let crashed = Command::new(&executable)
            .args([
                "--exact",
                "graph::tests::graph_runner_recovery_process_helper",
                "--nocapture",
            ])
            .env("ANCHOR_GRAPH_RECOVERY_TEST_ROOT", &root)
            .env("ANCHOR_GRAPH_RECOVERY_TEST_RUN", &run_id)
            .env("ANCHOR_GRAPH_RECOVERY_TEST_CRASH", crash_name)
            .output()
            .unwrap();
        assert_eq!(
            crashed.status.code(),
            Some(expected_exit),
            "child did not crash at {crash_name}; stdout={}, stderr={}",
            String::from_utf8_lossy(&crashed.stdout),
            String::from_utf8_lossy(&crashed.stderr)
        );

        let interrupted = store.load(&run_id).unwrap().unwrap();
        assert_eq!(interrupted.status, RunStatus::Running);
        let cursor = interrupted
            .cursor
            .as_ref()
            .expect("cursor persisted before dispatch");
        assert_eq!(cursor.node_id, "one");
        assert_eq!(cursor.key.invocation, 1);
        assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
        assert!(
            root.join(format!("node-{}.json", cursor.key.durable_key()))
                .exists(),
            "durable completion fact must survive {crash_name}"
        );

        let ports = DurableTestPorts {
            root: root.clone(),
            crash_point: None,
        };
        let control = Control::default();
        let recovered = GraphRunner::new(&store, &ports, &ports, &control)
            .run(interrupted)
            .await
            .unwrap();
        assert_eq!(recovered.status, RunStatus::Completed, "{crash_name}");
        assert!(recovered.cursor.is_none());
        assert_eq!(recovered.invocations["one"], 1);
        assert_eq!(recovered.passes["one"], 1);
        assert_eq!(recovered.results["one"].len(), 1);
        assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
        assert_eq!(fs::read(root.join("freeze-count")).unwrap(), b"1");

        let result = &recovered.results["one"][0];
        let artifact_path = ports.artifact_path(&result.key);
        let artifact: DurableTestArtifact =
            serde_json::from_slice(&fs::read(artifact_path).unwrap()).unwrap();
        assert_eq!(result.commit, artifact.commit);
        assert_eq!(result.commit.id, result.key.durable_key());
        assert_eq!(
            store.load(&run_id).unwrap().unwrap(),
            recovered,
            "the recovered result must be the durable Run fact"
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn file_run_store_recovers_not_started_invocation_after_process_crash() {
    let executable = std::env::current_exe().unwrap();
    let root = std::env::temp_dir().join(format!(
        "anchor-graph-recovery-{}-{}-before-dispatch",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let store = FileRunStore::new(&root);
    let initial = GraphRunRecord::create(graph(&["one"], &[], "one"), Value::Null).unwrap();
    let run_id = initial.run_id.clone();
    write_durable_json(&root.join("initial.json"), &initial).unwrap();

    let crashed = Command::new(&executable)
        .args([
            "--exact",
            "graph::tests::graph_runner_recovery_process_helper",
            "--nocapture",
        ])
        .env("ANCHOR_GRAPH_RECOVERY_TEST_ROOT", &root)
        .env("ANCHOR_GRAPH_RECOVERY_TEST_RUN", &run_id)
        .env("ANCHOR_GRAPH_RECOVERY_TEST_CRASH", "after-not-started")
        .output()
        .unwrap();
    assert_eq!(crashed.status.code(), Some(70));

    let interrupted = store.load(&run_id).unwrap().unwrap();
    assert_eq!(interrupted.status, RunStatus::Running);
    let cursor = interrupted
        .cursor
        .clone()
        .expect("cursor saved before fact lookup");
    assert_eq!(cursor.node_id, "one");
    assert_eq!(cursor.key.invocation, 1);
    assert!(
        !root
            .join(format!("node-{}.json", cursor.key.durable_key()))
            .exists()
    );
    assert!(!root.join("execute-count").exists());
    drop(store.acquire_lease(&run_id).unwrap());

    let ports = DurableTestPorts {
        root: root.clone(),
        crash_point: None,
    };
    let recovered = GraphRunner::new(&store, &ports, &ports, &Control::default())
        .run(interrupted)
        .await
        .unwrap();
    assert_eq!(recovered.status, RunStatus::Completed);
    assert!(recovered.cursor.is_none());
    assert_eq!(recovered.invocations["one"], 1);
    assert_eq!(recovered.passes["one"], 1);
    assert_eq!(recovered.results["one"].len(), 1);
    assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
    let result = &recovered.results["one"][0];
    assert_eq!(result.key, cursor.key);
    assert_eq!(result.commit.id, cursor.key.durable_key());
    assert_eq!(store.load(&run_id).unwrap().unwrap(), recovered);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn parallel_wave_recovers_each_branch_after_process_crash() {
    let executable = std::env::current_exe().unwrap();
    for (crash_name, expected_exit) in [
        ("after-not-started", 70),
        ("after-node-fact", 71),
        ("after-artifact-commit", 72),
    ] {
        let root = std::env::temp_dir().join(format!(
            "anchor-parallel-recovery-{}-{}-{crash_name}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let store = FileRunStore::new(&root);
        let initial = GraphRunRecord::create(parallel_graph(), Value::Null).unwrap();
        let run_id = initial.run_id.clone();
        write_durable_json(&root.join("initial.json"), &initial).unwrap();

        let crashed = Command::new(&executable)
            .args([
                "--exact",
                "graph::tests::graph_runner_recovery_process_helper",
                "--nocapture",
            ])
            .env("ANCHOR_GRAPH_RECOVERY_TEST_ROOT", &root)
            .env("ANCHOR_GRAPH_RECOVERY_TEST_RUN", &run_id)
            .env("ANCHOR_GRAPH_RECOVERY_TEST_CRASH", crash_name)
            .output()
            .unwrap();
        assert_eq!(
            crashed.status.code(),
            Some(expected_exit),
            "parallel child did not crash at {crash_name}; stdout={}, stderr={}",
            String::from_utf8_lossy(&crashed.stdout),
            String::from_utf8_lossy(&crashed.stderr)
        );

        let interrupted = store.load(&run_id).unwrap().unwrap();
        assert_eq!(interrupted.status, RunStatus::Running);
        let activation = interrupted.parallel.as_ref().unwrap();
        assert!(
            activation
                .branches
                .iter()
                .all(|branch| branch.cursor.is_some()
                    || branch.status == ParallelBranchStatus::Completed)
        );
        let ports = DurableTestPorts {
            root: root.clone(),
            crash_point: None,
        };
        let recovered = GraphRunner::new(&store, &ports, &ports, &Control::default())
            .run(interrupted)
            .await
            .unwrap();
        assert_eq!(recovered.status, RunStatus::Completed, "{crash_name}");
        assert!(recovered.parallel.is_none());
        for node in ["left", "right", "after"] {
            assert_eq!(recovered.results[node].len(), 1, "{node}, {crash_name}");
        }
        assert_eq!(recovered.results["collect"].len(), 1);
        assert_eq!(store.load(&run_id).unwrap().unwrap(), recovered);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn file_run_store_recovers_durable_failed_node_without_replay_or_freeze() {
    let executable = std::env::current_exe().unwrap();
    let root = std::env::temp_dir().join(format!(
        "anchor-graph-recovery-{}-{}-after-failed-fact",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let store = FileRunStore::new(&root);
    let initial = GraphRunRecord::create(
        graph(&["one", "two"], &[("one", "two")], "one"),
        Value::Null,
    )
    .unwrap();
    let run_id = initial.run_id.clone();
    write_durable_json(&root.join("initial.json"), &initial).unwrap();

    let crashed = Command::new(&executable)
        .args([
            "--exact",
            "graph::tests::graph_runner_recovery_process_helper",
            "--nocapture",
        ])
        .env("ANCHOR_GRAPH_RECOVERY_TEST_ROOT", &root)
        .env("ANCHOR_GRAPH_RECOVERY_TEST_RUN", &run_id)
        .env("ANCHOR_GRAPH_RECOVERY_TEST_CRASH", "after-failed-fact")
        .output()
        .unwrap();
    assert_eq!(crashed.status.code(), Some(73));

    let interrupted = store.load(&run_id).unwrap().unwrap();
    assert_eq!(interrupted.status, RunStatus::Running);
    let cursor = interrupted
        .cursor
        .clone()
        .expect("cursor saved before dispatch");
    assert_eq!(cursor.node_id, "one");
    assert_eq!(cursor.key.invocation, 1);
    assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
    let fact: DurableTestNodeFact = serde_json::from_slice(
        &fs::read(root.join(format!("node-{}.json", cursor.key.durable_key()))).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fact,
        DurableTestNodeFact::Failed("durable test failure".into())
    );
    drop(store.acquire_lease(&run_id).unwrap());

    let ports = DurableTestPorts {
        root: root.clone(),
        crash_point: None,
    };
    let recovered = GraphRunner::new(&store, &ports, &ports, &Control::default())
        .run(interrupted)
        .await
        .unwrap();
    assert_eq!(recovered.status, RunStatus::Failed);
    assert!(recovered.cursor.is_none());
    assert_eq!(recovered.invocations["one"], 1);
    assert_eq!(recovered.passes["one"], 1);
    assert!(recovered.results.is_empty());
    assert!(
        recovered.decided.is_empty(),
        "failure must not settle outgoing edges"
    );
    assert!(
        recovered
            .error
            .as_deref()
            .unwrap()
            .contains("durable test failure")
    );
    assert_eq!(fs::read(root.join("execute-count")).unwrap(), b"1");
    assert!(!root.join("freeze-count").exists());
    assert!(
        !root
            .join(format!("artifact-{}.json", cursor.key.durable_key()))
            .exists()
    );
    assert_eq!(store.load(&run_id).unwrap().unwrap(), recovered);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn graph_runner_recovery_process_helper() {
    let (Ok(root), Ok(run_id), Ok(crash_name)) = (
        std::env::var("ANCHOR_GRAPH_RECOVERY_TEST_ROOT"),
        std::env::var("ANCHOR_GRAPH_RECOVERY_TEST_RUN"),
        std::env::var("ANCHOR_GRAPH_RECOVERY_TEST_CRASH"),
    ) else {
        return;
    };
    if !PathBuf::from(&root)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with("anchor-graph-recovery-")
                || name.starts_with("anchor-parallel-recovery-")
        })
    {
        return;
    }
    let crash_point = match crash_name.as_str() {
        "after-not-started" => CrashPoint::NotStartedFactCheck,
        "after-node-fact" => CrashPoint::NodeFact,
        "after-failed-fact" => CrashPoint::FailedFact,
        "after-artifact-commit" => CrashPoint::ArtifactCommit,
        _ => panic!("unknown graph recovery crash point `{crash_name}`"),
    };
    let bytes = fs::read(PathBuf::from(&root).join("initial.json")).unwrap();
    let record: GraphRunRecord = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record.run_id, run_id);
    let store = FileRunStore::new(&root);
    let ports = DurableTestPorts {
        root: PathBuf::from(root),
        crash_point: Some(crash_point),
    };
    let control = Control::default();
    let result = GraphRunner::new(&store, &ports, &ports, &control)
        .run(record)
        .await
        .unwrap();
    assert_eq!(result.status, RunStatus::Completed);
}

#[derive(Default)]
struct ScenarioNodes {
    outcomes: Vec<Value>,
    calls: Mutex<Vec<(String, u64)>>,
    facts: Mutex<BTreeMap<String, CompletionFact>>,
}
impl NodeExecutionPort for ScenarioNodes {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: true,
            op_run: false,
            exact_provider_request_budget: true,
        }
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
            let index = self.calls.lock().unwrap().len();
            let outcome = self.outcomes.get(index).ok_or_else(|| {
                GraphError::CorruptRun("oracle has no outcome for node invocation".into())
            })?;
            if outcome["node"] != request.key.node_id
                || outcome["invocation"] != request.key.invocation
            {
                return Err(GraphError::CorruptRun(format!(
                    "oracle expected {} invocation {}, got {} invocation {}",
                    outcome["node"],
                    outcome["invocation"],
                    request.key.node_id,
                    request.key.invocation
                )));
            }
            self.calls
                .lock()
                .unwrap()
                .push((request.key.node_id.clone(), request.key.invocation));
            if outcome["exit_status"] == "budget_exhausted" {
                return Ok(NodeExecutionOutcome::BudgetExhausted { model_requests: 2 });
            }
            if outcome["exit_status"] != "Submitted" {
                return Ok(NodeExecutionOutcome::Failed {
                    reason: outcome["exit_status"]
                        .as_str()
                        .unwrap_or("fixture failure")
                        .into(),
                });
            }
            let completion = NodeCompletion {
                submission: format!("oracle:{}:{}", request.key.node_id, request.key.invocation),
                route: outcome["route"].as_str().map(str::to_owned),
                model_requests: 1,
                output: serde_json::json!({"node":request.key.node_id,"invocation":request.key.invocation}),
            };
            self.facts.lock().unwrap().insert(
                request.key.durable_key(),
                CompletionFact::Completed(completion.clone()),
            );
            Ok(NodeExecutionOutcome::Completed(completion))
        })
    }
}

#[tokio::test]
async fn graph_runner_preserves_recorded_runtime_scenarios() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/runtime-scenarios.json")).unwrap();
    assert_eq!(fixture["format"], 1);

    let scenarios = fixture["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 10);
    for scenario in scenarios {
        let snapshot = GraphSnapshot::admit(scenario["graph_snapshot"].clone()).unwrap();
        let override_input = scenario["run_override"]["input"].clone();
        let nodes = ScenarioNodes {
            outcomes: scenario["node_outcomes"].as_array().unwrap().clone(),
            ..ScenarioNodes::default()
        };
        let (store, artifacts, _, mut control) = setup();
        control.pause = scenario["control"]["pause_before_dispatch"] == true;
        let record = GraphRunRecord::create(snapshot, override_input).unwrap();
        assert_eq!(record.input, scenario["effective_input"]);
        let result = GraphRunner::new(&store, &artifacts, &nodes, &control)
            .run(record)
            .await
            .unwrap();
        let expected = &scenario["expected"];

        let rust_status = match result.status {
            RunStatus::Completed => "completed",
            RunStatus::Aborted => "aborted",
            RunStatus::BudgetStopped => "budget_stopped",
            RunStatus::Stopped => "stopped",
            RunStatus::Failed => "failed",
            RunStatus::Ready => "ready",
            RunStatus::Running => "running",
            RunStatus::Paused => "paused",
            RunStatus::WaitingCall => "waiting_call",
            RunStatus::WaitingRecovery => "waiting_recovery",
        };
        assert_eq!(
            rust_status, expected["status"],
            "scenario {}, reason {:?}, ceased {:?}",
            scenario["id"], result.error, result.ceased
        );

        let calls = nodes.calls.lock().unwrap().clone();
        let ordered_executed: Vec<&str> = calls.iter().map(|(node, _)| node.as_str()).collect();
        let expected_executed: Vec<&str> = expected["ordered_executed_nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node.as_str().unwrap())
            .collect();
        assert_eq!(
            ordered_executed, expected_executed,
            "scenario {}",
            scenario["id"]
        );

        let executed_set: BTreeSet<&str> = calls.iter().map(|(node, _)| node.as_str()).collect();
        let derived_skipped: Vec<&str> = result
            .snapshot
            .nodes
            .iter()
            .filter(|node| !executed_set.contains(node.id.as_str()))
            .map(|node| node.id.as_str())
            .collect();
        let expected_skipped: Vec<&str> = expected["skipped_nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node.as_str().unwrap())
            .collect();
        if scenario["id"] == "deterministic_failure_settles_single_exit" {
            assert_eq!(derived_skipped, vec!["downstream"]);
        } else if scenario["id"] == "pause_before_first_dispatch" {
            // Unstarted nodes are not considered skipped when the run pauses
            // before admission reaches the scheduler's propagation pass.
            assert!(expected_skipped.is_empty());
        } else {
            assert_eq!(
                derived_skipped, expected_skipped,
                "skipped nodes, scenario {}",
                scenario["id"]
            );
        }

        let expected_passes = expected["passes"].as_object().unwrap();
        assert_eq!(
            result.passes.len(),
            expected_passes.len(),
            "scenario {}",
            scenario["id"]
        );
        for (node, passes) in expected_passes {
            assert_eq!(
                result.passes.get(node),
                passes.as_u64().as_ref(),
                "passes for {node}, scenario {}",
                scenario["id"]
            );
        }
        if scenario["id"] == "budget_stop_preserves_cursor" {
            assert_eq!(
                expected["passes"]["budgeted"], 1,
                "A started budgeted invocation must be recorded"
            );
        }
        if scenario["id"] == "pause_before_first_dispatch" {
            assert_eq!(expected["reason"], "asked");
            assert_eq!(result.status, RunStatus::Paused);
            assert!(nodes.calls.lock().unwrap().is_empty());
            assert!(result.cursor.is_none());
            assert!(result.passes.is_empty());
            assert!(result.decided.is_empty());
            assert!(scenario["node_outcomes"].as_array().unwrap().is_empty());
        }

        let mut ceased: Vec<String> = result.ceased.iter().cloned().collect();
        ceased.sort();
        let mut expected_ceased: Vec<String> = expected["ceased"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_owned())
            .collect();
        expected_ceased.sort();
        assert_eq!(ceased, expected_ceased, "scenario {}", scenario["id"]);

        let expected_cursor = &expected["cursor"];
        let cursor_fact = result.cursor.as_ref().map_or_else(
            || serde_json::json!({"exists":false}),
            |cursor| {
                serde_json::json!({
                    "exists":true,
                    "node":cursor.node_id,
                    "pass":result.passes.get(&cursor.node_id).copied().unwrap_or(0),
                    "invocation":cursor.key.invocation
                })
            },
        );

        let mut decisions: Vec<Value> = result
            .decided
            .iter()
            .map(|(key, decision)| {
                let (from, to) = key.split_once('|').unwrap();
                serde_json::json!({"from":from,"to":to,"selected":decision.selected})
            })
            .collect();
        decisions.sort_by(|a, b| {
            (a["from"].as_str(), a["to"].as_str()).cmp(&(b["from"].as_str(), b["to"].as_str()))
        });
        let mut expected_decisions = expected["edge_decisions"].as_array().unwrap().clone();
        expected_decisions.sort_by(|a, b| {
            (a["from"].as_str(), a["to"].as_str()).cmp(&(b["from"].as_str(), b["to"].as_str()))
        });
        if scenario["id"] == "deterministic_failure_settles_single_exit" {
            // The historical scenario settles a single exit
            // even after node failure; Rust treats the failed fact as terminal.
            assert_eq!(
                result.status,
                RunStatus::Failed,
                "intentional semantic divergence: failure is terminal"
            );
            assert_eq!(result.passes.get("fail"), Some(&1));
            assert!(result.cursor.is_none());
            assert!(result.decided.is_empty());
            assert_eq!(expected_decisions.len(), 1);
            assert_eq!(expected_decisions[0]["to"], "downstream");
            assert_eq!(expected_decisions[0]["selected"], true);
            assert_eq!(cursor_fact, expected_cursor.clone());
        } else {
            assert_eq!(
                (decisions, cursor_fact),
                (expected_decisions, expected_cursor.clone()),
                "edge decisions and cursor, scenario {}",
                scenario["id"]
            );
        }
    }
}
