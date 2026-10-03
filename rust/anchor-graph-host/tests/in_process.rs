use anchor_graph_host::{GraphCatalog, InProcessGraphHost};
use anchor_runtime_rig::{Cancellation, graph::*};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Catalog {
    snapshot: std::sync::Mutex<Option<GraphSnapshot>>,
    child_metadata: std::sync::Mutex<BTreeMap<String, (String, String, CallIdentity, String)>>,
}

impl Catalog {
    fn new(snapshot: GraphSnapshot) -> Self {
        Self {
            snapshot: std::sync::Mutex::new(Some(snapshot)),
            child_metadata: std::sync::Mutex::new(BTreeMap::new()),
        }
    }
}

impl GraphCatalog for Catalog {
    fn snapshot(&self, name: &str) -> Result<Option<GraphSnapshot>, GraphError> {
        Ok(if name == "child" {
            self.snapshot.lock().unwrap().clone()
        } else {
            None
        })
    }
    fn record_child_admission(
        &self,
        run_id: &str,
        graph: &str,
        snapshot: &GraphSnapshot,
        _: &[PluginBinding],
        identity: &CallIdentity,
        mode: &str,
    ) -> Result<(), GraphError> {
        self.child_metadata.lock().unwrap().insert(
            run_id.to_owned(),
            (
                graph.to_owned(),
                snapshot.digest()?,
                identity.clone(),
                mode.to_owned(),
            ),
        );
        Ok(())
    }
    fn verify_child_identity(
        &self,
        run_id: &str,
        graph: &str,
        snapshot: &GraphSnapshot,
        identity: &CallIdentity,
        mode: &str,
    ) -> Result<(), GraphError> {
        let metadata = self.child_metadata.lock().unwrap();
        if metadata.get(run_id).is_some_and(
            |(recorded_graph, digest, recorded_identity, recorded_mode)| {
                recorded_graph == graph
                    && digest == &snapshot.digest().unwrap_or_default()
                    && recorded_identity == identity
                    && recorded_mode == mode
            },
        ) {
            Ok(())
        } else {
            Err(GraphError::CorruptRun(
                "child metadata does not match frozen snapshot".into(),
            ))
        }
    }
    fn has_child_admission(&self, run_id: &str) -> Result<bool, GraphError> {
        Ok(self.child_metadata.lock().unwrap().contains_key(run_id))
    }
}

#[derive(Default)]
struct Artifacts;
impl ArtifactPort for Artifacts {
    fn freeze<'a>(
        &'a self,
        key: &'a InvocationKey,
        _: &'a NodeCompletion,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(CommitRef {
                id: format!("commit-{}", key.durable_key()),
                node_id: key.node_id.clone(),
                invocation: key.invocation,
            })
        })
    }
    fn resolve<'a>(
        &'a self,
        _: &'a CommitRef,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
        Box::pin(async { Ok(json!({})) })
    }
}

struct Nodes(Arc<AtomicUsize>);
impl NodeExecutionPort for Nodes {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: false,
            op_run: true,
            exact_provider_request_budget: true,
        }
    }
    fn completion_fact<'a>(
        &'a self,
        _: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
        Box::pin(async { Ok(CompletionFact::NotStarted) })
    }
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(NodeExecutionOutcome::Completed(NodeCompletion {
                submission: "ok".into(),
                route: None,
                model_requests: 0,
                output: json!({"child": request.input}),
            }))
        })
    }
}

struct BudgetOnceNodes(Arc<AtomicUsize>);
impl NodeExecutionPort for BudgetOnceNodes {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: false,
            op_run: true,
            exact_provider_request_budget: true,
        }
    }
    fn completion_fact<'a>(
        &'a self,
        _: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
        Box::pin(async { Ok(CompletionFact::NotStarted) })
    }
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>> {
        let count = self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if count == 0 {
                Ok(NodeExecutionOutcome::BudgetExhausted { model_requests: 1 })
            } else {
                Ok(NodeExecutionOutcome::Completed(NodeCompletion {
                    submission: "ok".into(),
                    route: None,
                    model_requests: 1,
                    output: json!({"child": request.input}),
                }))
            }
        })
    }
}
#[derive(Default)]
struct Control;
impl RunControl for Control {
    fn pause_requested(&self) -> bool {
        false
    }
    fn stop_requested(&self) -> bool {
        false
    }
    fn cancellation(&self) -> Cancellation {
        Arc::new(std::sync::atomic::AtomicBool::new(false))
    }
}

fn child_graph() -> GraphSnapshot {
    GraphSnapshot {
        objective: "child".into(),
        input: json!({"default": 1}),
        entry: "work".into(),
        agents: BTreeMap::new(),
        ops: BTreeMap::from([("work".into(), json!({"run":"true"}))]),
        nodes: vec![GraphNode {
            id: "work".into(),
            agent: None,
            op: Some("work".into()),
            input: None,
            plugins: vec![],
            max_rounds: None,
        }],
        edges: vec![],
        module_rounds: BTreeMap::new(),
    }
}
fn spec(mode: &str) -> Value {
    json!({"graph":"child", "mode":mode, "input":{"provided":2}})
}

fn parent_graph(call_spec: Value) -> GraphSnapshot {
    GraphSnapshot {
        objective: "parent".into(),
        input: json!({}),
        entry: "invoke".into(),
        agents: BTreeMap::new(),
        ops: BTreeMap::from([("invoke".into(), json!({"call":call_spec}))]),
        nodes: vec![GraphNode {
            id: "invoke".into(),
            agent: None,
            op: Some("invoke".into()),
            input: None,
            plugins: vec![],
            max_rounds: None,
        }],
        edges: vec![],
        module_rounds: BTreeMap::new(),
    }
}

#[tokio::test]
async fn wait_runs_child_once_and_reload_reuses_file_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let catalog = Catalog::new(child_graph());
    let count = Arc::new(AtomicUsize::new(0));
    let nodes = Nodes(count.clone());
    let artifacts = Artifacts;
    let control = Control;
    let call_spec = spec("wait");
    let parent = GraphRunRecord::create(parent_graph(call_spec.clone()), json!({})).unwrap();
    let parent_id = parent.run_id.clone();
    let host = InProcessGraphHost::new(&catalog, &store, &artifacts, &nodes, &control);
    let completed = GraphRunner::new(&store, &artifacts, &host, &control)
        .run(parent)
        .await
        .unwrap();
    assert_eq!(
        completed.status,
        RunStatus::Completed,
        "{:?}",
        completed.error
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let call = completed.graph_calls.values().next().unwrap();
    let child_id = call.child_run_id.clone().unwrap();
    assert_eq!(
        completed.results["invoke"][0].completion.output["child"]["input"],
        json!({"default":1,"provided":2})
    );

    // Once the parent cursor is complete, replaying or transplanting its identity
    // cannot be used to admit a child call independently of GraphRunner.
    assert!(matches!(
        host.call(
            &call.identity,
            &call_spec,
            &completed.results["invoke"][0].completion.output,
            Cancellation::default()
        )
        .await,
        Err(GraphError::CorruptRun(_))
    ));
    let forged = CallIdentity {
        parent_run_id: child_id.clone(),
        ..call.identity.clone()
    };
    assert!(matches!(
        host.call(&forged, &call_spec, &json!({}), Cancellation::default())
            .await,
        Err(GraphError::CorruptRun(_))
    ));

    let reloaded_store = FileRunStore::new(dir.path());
    assert_eq!(
        reloaded_store.load(&child_id).unwrap().unwrap().status,
        RunStatus::Completed
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "completed child must not dispatch again after host restart"
    );
    assert!(reloaded_store.load(&parent_id).unwrap().is_some());
}

#[tokio::test]
async fn wait_retry_uses_frozen_child_after_catalog_change_or_removal() {
    for remove_catalog_entry in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = FileRunStore::new(dir.path());
        let catalog = Catalog::new(child_graph());
        let count = Arc::new(AtomicUsize::new(0));
        let nodes = BudgetOnceNodes(count.clone());
        let artifacts = Artifacts;
        let control = Control;
        let call_spec = spec("wait");
        let parent = GraphRunRecord::create(parent_graph(call_spec), json!({})).unwrap();
        let parent_id = parent.run_id.clone();
        let original_child_digest = child_graph().digest().unwrap();
        let host = InProcessGraphHost::new(&catalog, &store, &artifacts, &nodes, &control);

        let waiting = GraphRunner::new(&store, &artifacts, &host, &control)
            .run(parent)
            .await
            .unwrap();
        assert_eq!(waiting.status, RunStatus::WaitingCall);
        let child_id = waiting
            .graph_calls
            .values()
            .next()
            .unwrap()
            .child_run_id
            .clone()
            .unwrap();
        let admitted = store.load(&child_id).unwrap().unwrap();
        assert_eq!(admitted.status, RunStatus::BudgetStopped);
        assert_eq!(admitted.graph_digest, original_child_digest);
        assert!(
            catalog
                .child_metadata
                .lock()
                .unwrap()
                .get(&child_id)
                .is_some_and(|(graph, digest, source, mode)| graph == "child"
                    && digest == &original_child_digest
                    && source.parent_run_id == parent_id
                    && mode == "wait")
        );

        if remove_catalog_entry {
            *catalog.snapshot.lock().unwrap() = None;
        } else {
            let mut edited = child_graph();
            edited.objective = "edited current definition".into();
            edited.input = json!({"changed_default": true});
            *catalog.snapshot.lock().unwrap() = Some(edited);
        }

        let resumed_parent = store.load(&parent_id).unwrap().unwrap();
        let completed = GraphRunner::new(&store, &artifacts, &host, &control)
            .run(resumed_parent)
            .await
            .unwrap();
        assert_eq!(
            completed.status,
            RunStatus::Completed,
            "{:?}",
            completed.error
        );
        let resumed_child = store.load(&child_id).unwrap().unwrap();
        assert_eq!(resumed_child.status, RunStatus::Completed);
        assert_eq!(resumed_child.graph_digest, original_child_digest);
        assert_eq!(resumed_child.snapshot.objective, "child");
        assert_eq!(resumed_child.input, json!({"default":1,"provided":2}));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert!(
            catalog
                .child_metadata
                .lock()
                .unwrap()
                .get(&child_id)
                .is_some_and(|(graph, digest, source, mode)| graph == "child"
                    && digest == &original_child_digest
                    && source.parent_run_id == parent_id
                    && mode == "wait")
        );
    }
}

#[tokio::test]
async fn missing_child_record_with_existing_admission_metadata_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let catalog = Catalog::new(child_graph());
    let count = Arc::new(AtomicUsize::new(0));
    let nodes = BudgetOnceNodes(count.clone());
    let artifacts = Artifacts;
    let control = Control;
    let parent = GraphRunRecord::create(parent_graph(spec("wait")), json!({})).unwrap();
    let parent_id = parent.run_id.clone();
    let host = InProcessGraphHost::new(&catalog, &store, &artifacts, &nodes, &control);
    let waiting = GraphRunner::new(&store, &artifacts, &host, &control)
        .run(parent)
        .await
        .unwrap();
    let child_id = waiting
        .graph_calls
        .values()
        .next()
        .unwrap()
        .child_run_id
        .clone()
        .unwrap();
    std::fs::remove_file(dir.path().join(format!("{child_id}.json"))).unwrap();
    let resumed = store.load(&parent_id).unwrap().unwrap();
    let result = GraphRunner::new(&store, &artifacts, &host, &control)
        .run(resumed)
        .await;
    assert!(
        matches!(result, Err(GraphError::CorruptRun(message)) if message.contains("metadata exists"))
    );
    assert!(
        store.load(&child_id).unwrap().is_none(),
        "missing child facts must not be rebuilt from the current catalog"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn detach_only_admits_child_without_dispatching_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let catalog = Catalog::new(child_graph());
    let count = Arc::new(AtomicUsize::new(0));
    let nodes = Nodes(count.clone());
    let artifacts = Artifacts;
    let control = Control;
    let parent = GraphRunRecord::create(parent_graph(spec("detach")), json!({})).unwrap();
    let host = InProcessGraphHost::new(&catalog, &store, &artifacts, &nodes, &control);
    let parent = GraphRunner::new(&store, &artifacts, &host, &control)
        .run(parent)
        .await
        .unwrap();
    assert_eq!(parent.status, RunStatus::Completed, "{:?}", parent.error);
    let child_id = parent
        .graph_calls
        .values()
        .next()
        .unwrap()
        .child_run_id
        .clone()
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.load(&child_id).unwrap().unwrap().status,
        RunStatus::Ready
    );
}

#[tokio::test]
async fn nested_graph_call_targets_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let mut nested = child_graph();
    nested.ops.insert(
        "nested".into(),
        json!({"call":{"graph":"child","mode":"detach"}}),
    );
    let catalog = Catalog::new(nested);
    let count = Arc::new(AtomicUsize::new(0));
    let nodes = Nodes(count);
    let artifacts = Artifacts;
    let control = Control;
    let parent = GraphRunRecord::create(parent_graph(spec("wait")), json!({})).unwrap();
    let host = InProcessGraphHost::new(&catalog, &store, &artifacts, &nodes, &control);
    assert!(
        matches!(GraphRunner::new(&store, &artifacts, &host, &control).run(parent).await, Err(GraphError::Unsupported(message)) if message.contains("nested Graph calls"))
    );
}

#[tokio::test]
async fn unknown_child_graph_fails_closed_without_child_admission() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let catalog = Catalog::new(child_graph());
    let count = Arc::new(AtomicUsize::new(0));
    let nodes = Nodes(count.clone());
    let artifacts = Artifacts;
    let control = Control;
    let mut unknown = spec("wait");
    unknown["graph"] = json!("missing");
    let parent = GraphRunRecord::create(parent_graph(unknown), json!({})).unwrap();
    let host = InProcessGraphHost::new(&catalog, &store, &artifacts, &nodes, &control);
    let result = GraphRunner::new(&store, &artifacts, &host, &control)
        .run(parent)
        .await;
    assert!(
        matches!(result, Err(GraphError::InvalidSnapshot(message)) if message.contains("unknown child graph"))
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count(),
        1,
        "only the failed parent Run is persisted"
    );
}
