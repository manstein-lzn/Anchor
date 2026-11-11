use super::*;
use crate::PluginCatalog;
use crate::{FilePluginCatalog, LoadedGraphBundle};
use anchor_kernel::{Outcome, Phase};
use anchor_runtime::graph::GraphSnapshot;
use anchor_runtime::graph::{CompletionFact, NodeCompletion};
mod fixture {
    use super::{CallRun, CallRunAccess, ChildExecutionPort, PluginCatalog};
    include!("../tests/support/kernel.rs");
}
use fixture::{Executor, Runs, parent_call};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Artifacts {
    exports: Mutex<Vec<(InvocationKey, CommitRef, Vec<String>)>>,
}

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

    fn export_call_result_files<'a>(
        &'a self,
        key: &'a InvocationKey,
        commit: &'a CommitRef,
        files: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            self.exports
                .lock()
                .unwrap()
                .push((key.clone(), commit.clone(), files.to_vec()));
            Ok(files.iter().map(|file| format!("result/{file}")).collect())
        })
    }
}

#[derive(Default)]
struct Nodes(AtomicUsize);

impl NodeExecutionPort for Nodes {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: false,
            op_run: true,
            host_operations: false,
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
                submission: "session reply".into(),
                route: None,
                model_requests: 0,
                output: request.input,
            }))
        })
    }
}

fn child_snapshot() -> GraphSnapshot {
    GraphSnapshot::from_authoring(json!({
        "objective":"child", "input":{"default":1}, "entry":"work",
        "agents":{}, "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work"}], "edges":[]
    }))
    .unwrap()
}

struct OrdinaryCatalog;

impl GraphCatalog for OrdinaryCatalog {
    fn snapshot(&self, _: &str) -> Result<Option<GraphSnapshot>, GraphError> {
        Ok(Some(child_snapshot()))
    }

    fn graph_ancestry(&self, _: &str) -> Result<Vec<String>, GraphError> {
        Ok(vec!["parent".into()])
    }
}

#[tokio::test]
async fn child_admission_resolves_network_policy_without_requiring_an_offline_provider() {
    struct Catalog(GraphSnapshot);
    impl GraphCatalog for Catalog {
        fn snapshot(&self, _: &str) -> Result<Option<GraphSnapshot>, GraphError> {
            Ok(Some(self.0.clone()))
        }

        fn bundle(&self, _: &str) -> Result<Option<LoadedGraphBundle>, GraphError> {
            Ok(Some(LoadedGraphBundle {
                authoring_definition: serde_json::to_value(&self.0).unwrap(),
                snapshot: self.0.clone(),
                plugins: vec!["network".into()],
            }))
        }

        fn graph_ancestry(&self, _: &str) -> Result<Vec<String>, GraphError> {
            Ok(vec!["parent".into()])
        }
    }
    for mode in ["denied", "auto", "enabled"] {
        let root = tempfile::tempdir().unwrap();
        let runs = Runs::new(root.path());
        let artifacts = Artifacts::default();
        let nodes = Nodes::default();
        let plugins = FilePluginCatalog::new(root.path());
        let snapshot = GraphSnapshot::from_authoring(json!({
            "objective":"offline child", "ops":{"work":{"run":"true"}},
            "nodes":[{"id":"work","op":"work","plugins":[{"id":"network","network":mode}]}], "edges":[]
        })).unwrap();
        let digest = snapshot.digest().unwrap();
        let catalog = Catalog(snapshot);
        let executor = Executor {
            runs: &runs,
            artifacts: &artifacts,
            nodes: &nodes,
            plugins: Some(&plugins),
        };
        let host = InProcessGraphHost::new(&catalog, &runs, &artifacts, &nodes)
            .with_plugin_catalog(&plugins)
            .with_child_executor(&executor, false);
        let spec = json!({"graph":"child","mode":"wait"});
        let (identity, input) = parent_call(&runs, &spec);
        let result = host
            .call(&identity, &spec, &input, &[], Cancellation::default())
            .await;
        if mode == "enabled" {
            assert!(result.unwrap_err().to_string().contains("network"));
            assert!(runs.load(&child_run_id(&identity)).unwrap().is_none());
        } else {
            assert!(matches!(
                result.unwrap(),
                GraphCallOutcome::Completed { .. }
            ));
            let child = runs.load(&child_run_id(&identity)).unwrap().unwrap();
            assert_eq!(child.record.snapshot.digest().unwrap(), digest);
            assert!(child.record.plugin_bindings.is_empty());
        }
    }
}

#[derive(Clone, Copy)]
enum SessionAction {
    Waiting,
    Detached,
    Complete,
    Unproven,
}

struct SessionCatalog<'a> {
    store: Arc<Runs>,
    artifacts: &'a Artifacts,
    context: Value,
    authorized: bool,
    action: Mutex<SessionAction>,
    admissions: AtomicUsize,
    contexts: Mutex<Vec<Value>>,
    nodes: Nodes,
}

impl<'a> SessionCatalog<'a> {
    fn new(store: &Arc<Runs>, artifacts: &'a Artifacts, action: SessionAction) -> Self {
        Self {
            store: store.clone(),
            artifacts,
            context: json!({
                "session":"trusted-session", "reply_node":"work",
                "conversation_id":"trusted-user",
                "channel":{"source":"wecom", "sender_id":"trusted-user"}
            }),
            authorized: true,
            action: Mutex::new(action),
            admissions: AtomicUsize::new(0),
            contexts: Mutex::new(Vec::new()),
            nodes: Nodes::default(),
        }
    }
}

impl GraphCatalog for SessionCatalog<'_> {
    fn snapshot(&self, _: &str) -> Result<Option<GraphSnapshot>, GraphError> {
        Ok(Some(child_snapshot()))
    }

    fn graph_ancestry(&self, _: &str) -> Result<Vec<String>, GraphError> {
        Ok(vec!["parent".into()])
    }

    fn record_child_admission(
        &self,
        run_id: &str,
        _: &str,
        _: &GraphSnapshot,
        _: &[anchor_runtime::graph::PluginBinding],
        _: &CallIdentity,
        _: &str,
    ) -> Result<(), GraphError> {
        assert!(self.store.load(run_id)?.is_none());
        self.admissions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn prepare_session_call<'a>(
        &'a self,
        _: &'a CallIdentity,
        session: &'a str,
        graph: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(session, "trusted-session");
            assert_eq!(graph, "child");
            if self.authorized {
                Ok(self.context.clone())
            } else {
                Err(GraphError::Unsupported("Session is not authorized".into()))
            }
        })
    }

    fn record_session_call(&self, run_id: &str, context: &Value) -> Result<(), GraphError> {
        assert_eq!(self.admissions.load(Ordering::SeqCst), 1);
        assert!(self.store.load(run_id)?.is_none());
        self.contexts.lock().unwrap().push(context.clone());
        Ok(())
    }

    fn session_call_outcome<'a>(
        &'a self,
        run_id: &'a str,
        mode: &'a str,
        _: Cancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Option<GraphCallOutcome>, GraphError>> + Send + 'a>>
    {
        Box::pin(async move {
            drop(self.store.acquire_lease(run_id)?);
            assert_eq!(self.contexts.lock().unwrap().len(), 1);
            let action = *self.action.lock().unwrap();
            match action {
                SessionAction::Waiting => Ok(Some(GraphCallOutcome::Waiting {
                    child_run_id: run_id.into(),
                })),
                SessionAction::Detached => {
                    assert_eq!(mode, "detach");
                    Ok(Some(GraphCallOutcome::Detached {
                        child_run_id: run_id.into(),
                    }))
                }
                SessionAction::Complete => {
                    let child = self.store.load(run_id)?.unwrap();
                    let completed = Executor {
                        runs: &self.store,
                        artifacts: self.artifacts,
                        nodes: &self.nodes,
                        plugins: None,
                    }
                    .execute(child, Cancellation::default())
                    .await?;
                    assert_eq!(
                        completed.phase,
                        Phase::Settled {
                            result: Outcome::Done
                        }
                    );
                    Ok(None)
                }
                SessionAction::Unproven => Ok(None),
            }
        })
    }

    fn dispatch_detached<'a>(
        &'a self,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
        Box::pin(async { panic!("Session child bypassed its owning host") })
    }
}

fn session_spec(mode: &str) -> Value {
    json!({
        "graph":"child", "mode":mode, "session":"trusted-session",
        "input":{"session":"forged", "channel":{"sender_id":"forged"}},
        "input_map":{"session":"/session", "channel":"/channel"}
    })
}

#[tokio::test]
async fn an_existing_child_resolves_current_library_code_and_records_its_observations() {
    struct Catalog;
    impl GraphCatalog for Catalog {
        fn snapshot(&self, _: &str) -> Result<Option<GraphSnapshot>, GraphError> {
            GraphSnapshot::from_authoring(json!({"objective":"child", "entry":"work",
                "agents":{"worker":{"model":"fixture"}}, "nodes":[{"id":"work","agent":"worker","plugins":["review"]}], "edges":[]})).map(Some)
        }
        fn graph_ancestry(&self, _: &str) -> Result<Vec<String>, GraphError> {
            Ok(vec!["parent".into()])
        }
        fn bundle(&self, name: &str) -> Result<Option<LoadedGraphBundle>, GraphError> {
            let snapshot = self.snapshot(name)?.unwrap();
            Ok(Some(LoadedGraphBundle {
                authoring_definition: serde_json::to_value(&snapshot)
                    .map_err(GraphError::SnapshotDecode)?,
                snapshot,
                plugins: vec!["review".into()],
            }))
        }
    }
    #[derive(Default)]
    struct HotNodes(Mutex<Vec<NodeExecutionRequest>>);
    impl NodeExecutionPort for HotNodes {
        fn capabilities(&self) -> NodeExecutionCapabilities {
            NodeExecutionCapabilities {
                agent: true,
                op_run: true,
                host_operations: false,
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
        ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>>
        {
            Box::pin(async move {
                let mut calls = self.0.lock().unwrap();
                calls.push(request);
                Ok(if calls.len() == 1 {
                    NodeExecutionOutcome::BudgetExhausted { model_requests: 0 }
                } else {
                    NodeExecutionOutcome::Completed(NodeCompletion {
                        submission: "done".into(),
                        route: None,
                        model_requests: 0,
                        output: json!({}),
                    })
                })
            })
        }
        fn observed_plugin_digests(&self, key: &InvocationKey) -> BTreeMap<String, String> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|call| &call.key == key)
                .map(|call| {
                    call.plugins
                        .iter()
                        .map(|plugin| (plugin.id.clone(), plugin.digest.clone()))
                        .collect()
                })
                .unwrap_or_default()
        }
        fn observed_plugin_views(&self, _: &InvocationKey) -> BTreeMap<String, String> {
            BTreeMap::from([("review".into(), "observed-view".into())])
        }
    }
    let root = tempfile::tempdir().unwrap();
    let plugin = root.path().join("plugins/review");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.json"), r#"{"name":"Review"}"#).unwrap();
    std::fs::write(plugin.join("instructions.md"), "original").unwrap();
    let plugins = FilePluginCatalog::new(root.path());
    let original = plugins.resolve(&["review".into()]).unwrap().remove(0);
    let store = Arc::new(Runs::new(&root.path().join("runs")));
    let artifacts = Artifacts::default();
    let nodes = HotNodes::default();
    let executor = Executor {
        runs: &store,
        artifacts: &artifacts,
        nodes: &nodes,
        plugins: Some(&plugins),
    };
    let host = InProcessGraphHost::new(&Catalog, &*store, &artifacts, &nodes)
        .with_plugin_catalog(&plugins)
        .with_child_executor(&executor, false);
    let spec = json!({"graph":"child", "mode":"wait"});
    let (identity, input) = parent_call(&store, &spec);
    let first = host
        .call(&identity, &spec, &input, &[], Cancellation::default())
        .await
        .unwrap();
    assert!(matches!(first, GraphCallOutcome::Waiting { .. }));
    let parked = host
        .call(&identity, &spec, &input, &[], Cancellation::default())
        .await
        .unwrap();
    assert!(matches!(parked, GraphCallOutcome::Waiting { .. }));
    assert_eq!(nodes.0.lock().unwrap().len(), 1);
    std::fs::create_dir_all(plugin.join("skills/new")).unwrap();
    std::fs::write(plugin.join("skills/new/SKILL.md"), "current").unwrap();
    let current = plugins.resolve(&["review".into()]).unwrap().remove(0);
    store.control(&child_run_id(&identity), "resume");
    let next = host
        .call(&identity, &spec, &input, &[], Cancellation::default())
        .await
        .unwrap();
    assert!(matches!(next, GraphCallOutcome::Completed { .. }));
    assert_eq!(nodes.0.lock().unwrap()[1].plugins, vec![current.clone()]);
    let child = store.load(&child_run_id(&identity)).unwrap().unwrap();
    assert_eq!(child.record.plugin_bindings["review"], original);
    assert_eq!(
        child.record.results["work"][0].plugin_digests["review"],
        current.digest
    );
    assert_eq!(
        child.record.results["work"][0].plugin_views["review"],
        "observed-view"
    );
}

#[tokio::test]
async fn session_wait_reuses_admission_and_exports_completed_result() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Runs::new(dir.path()));
    let artifacts = Artifacts::default();
    let catalog = SessionCatalog::new(&store, &artifacts, SessionAction::Waiting);
    let nodes = Nodes::default();
    let executor = Executor {
        runs: &store,
        artifacts: &artifacts,
        nodes: &nodes,
        plugins: None,
    };
    let host = InProcessGraphHost::new(&catalog, &*store, &artifacts, &nodes)
        .with_child_executor(&executor, false);
    let mut spec = session_spec("wait");
    spec["result"] = json!({"node":"work", "files":["report.txt"]});
    let (identity, input) = parent_call(&store, &spec);
    let child_id = child_run_id(&identity);
    let waiting = host
        .call(&identity, &spec, &input, &[], Cancellation::default())
        .await
        .unwrap();
    assert_eq!(
        waiting,
        GraphCallOutcome::Waiting {
            child_run_id: child_id.clone()
        }
    );
    let child = store.load(&child_id).unwrap().unwrap();
    assert_eq!(child.phase, Phase::Executing);
    assert_eq!(child.record.input["session"], catalog.context["session"]);
    assert_eq!(child.record.input["channel"], catalog.context["channel"]);
    assert_eq!(catalog.contexts.lock().unwrap()[0], catalog.context);
    assert_eq!(nodes.0.load(Ordering::SeqCst), 0);

    *catalog.action.lock().unwrap() = SessionAction::Complete;
    let completed = host
        .call(&identity, &spec, &input, &[], Cancellation::default())
        .await
        .unwrap();
    let GraphCallOutcome::Completed {
        child_run_id,
        output,
    } = completed
    else {
        panic!("expected completed Session call");
    };
    assert_eq!(child_run_id, child_id);
    assert_eq!(output["summary"], "session reply");
    assert_eq!(output["result"]["files"], json!(["result/report.txt"]));
    assert_eq!(catalog.admissions.load(Ordering::SeqCst), 1);
    assert_eq!(catalog.contexts.lock().unwrap().len(), 1);
    assert_eq!(catalog.nodes.0.load(Ordering::SeqCst), 1);
    assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
    let exports = artifacts.exports.lock().unwrap();
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].0.run_id, identity.parent_run_id);
    assert_eq!(exports[0].1.node_id, "work");
    assert_eq!(exports[0].2, vec!["report.txt"]);
}

#[tokio::test]
async fn session_detach_uses_session_host_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Runs::new(dir.path()));
    let artifacts = Artifacts::default();
    let catalog = SessionCatalog::new(&store, &artifacts, SessionAction::Detached);
    let nodes = Nodes::default();
    let executor = Executor {
        runs: &store,
        artifacts: &artifacts,
        nodes: &nodes,
        plugins: None,
    };
    let host = InProcessGraphHost::new(&catalog, &*store, &artifacts, &nodes)
        .with_child_executor(&executor, false);
    let spec = session_spec("detach");
    let (identity, input) = parent_call(&store, &spec);
    assert_eq!(
        host.call(&identity, &spec, &input, &[], Cancellation::default())
            .await
            .unwrap(),
        GraphCallOutcome::Detached {
            child_run_id: child_run_id(&identity)
        }
    );
    assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn session_rejection_or_invalid_context_never_admits_child() {
    for context in [
        None,
        Some(json!({"session":"other-session", "channel":{}})),
        Some(json!({"session":"trusted-session", "channel":false})),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Runs::new(dir.path()));
        let artifacts = Artifacts::default();
        let mut catalog = SessionCatalog::new(&store, &artifacts, SessionAction::Complete);
        if let Some(context) = context {
            catalog.context = context;
        } else {
            catalog.authorized = false;
        }
        let nodes = Nodes::default();
        let executor = Executor {
            runs: &store,
            artifacts: &artifacts,
            nodes: &nodes,
            plugins: None,
        };
        let host = InProcessGraphHost::new(&catalog, &*store, &artifacts, &nodes)
            .with_child_executor(&executor, false);
        let spec = session_spec("wait");
        let (identity, input) = parent_call(&store, &spec);
        let result = host
            .call(&identity, &spec, &input, &[], Cancellation::default())
            .await;
        assert!(matches!(
            result,
            Err(GraphError::Unsupported(_) | GraphError::CorruptRun(_))
        ));
        assert!(store.load(&child_run_id(&identity)).unwrap().is_none());
        assert_eq!(catalog.admissions.load(Ordering::SeqCst), 0);
        assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn session_delivery_confirmation_requires_completed_child() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Runs::new(dir.path()));
    let artifacts = Artifacts::default();
    let catalog = SessionCatalog::new(&store, &artifacts, SessionAction::Unproven);
    let nodes = Nodes::default();
    let executor = Executor {
        runs: &store,
        artifacts: &artifacts,
        nodes: &nodes,
        plugins: None,
    };
    let host = InProcessGraphHost::new(&catalog, &*store, &artifacts, &nodes)
        .with_child_executor(&executor, false);
    let spec = session_spec("wait");
    let (identity, input) = parent_call(&store, &spec);
    assert!(matches!(
        host.call(&identity, &spec, &input, &[], Cancellation::default()).await,
        Err(GraphError::CorruptRun(message)) if message.contains("without a completed child")
    ));
    assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
    assert!(artifacts.exports.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ordinary_host_rejects_session_but_keeps_ordinary_wait_behavior() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Runs::new(dir.path()));
    let artifacts = Artifacts::default();
    let nodes = Nodes::default();
    let executor = Executor {
        runs: &store,
        artifacts: &artifacts,
        nodes: &nodes,
        plugins: None,
    };
    let host = InProcessGraphHost::new(&OrdinaryCatalog, &*store, &artifacts, &nodes)
        .with_child_executor(&executor, false);
    let spec = session_spec("wait");
    let (identity, input) = parent_call(&store, &spec);
    assert!(matches!(
        host.call(&identity, &spec, &input, &[], Cancellation::default()).await,
        Err(GraphError::Unsupported(message)) if message.contains("session handoff")
    ));
    assert!(store.load(&child_run_id(&identity)).unwrap().is_none());
    assert_eq!(nodes.0.load(Ordering::SeqCst), 0);

    let spec = json!({"graph":"child", "mode":"wait"});
    let (identity, input) = parent_call(&store, &spec);
    assert!(matches!(
        host.call(&identity, &spec, &input, &[], Cancellation::default())
            .await,
        Ok(GraphCallOutcome::Completed { .. })
    ));
    assert_eq!(nodes.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_kernel_executor_refuses_wait_call_before_child_admission() {
    let dir = tempfile::tempdir().unwrap();
    let runs = Runs::new(dir.path());
    let artifacts = Artifacts::default();
    let nodes = Nodes::default();
    let host = InProcessGraphHost::new(&OrdinaryCatalog, &runs, &artifacts, &nodes);
    let spec = json!({"graph":"child","mode":"wait"});
    let (identity, input) = parent_call(&runs, &spec);
    assert!(matches!(
        host.call(&identity, &spec, &input, &[], Cancellation::default()).await,
        Err(GraphError::Unsupported(message)) if message.contains("shared Kernel child executor")
    ));
    assert!(runs.load(&child_run_id(&identity)).unwrap().is_none());
    assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
    assert!(artifacts.exports.lock().unwrap().is_empty());
}

#[tokio::test]
async fn parent_identity_input_and_provenance_must_match_the_kernel_frontier() {
    let dir = tempfile::tempdir().unwrap();
    let runs = Runs::new(dir.path());
    let artifacts = Artifacts::default();
    let nodes = Nodes::default();
    let executor = Executor {
        runs: &runs,
        artifacts: &artifacts,
        nodes: &nodes,
        plugins: None,
    };
    let host = InProcessGraphHost::new(&OrdinaryCatalog, &runs, &artifacts, &nodes)
        .with_child_executor(&executor, false);
    let spec = json!({"graph":"child","mode":"wait"});
    let (identity, input) = parent_call(&runs, &spec);
    for altered in ["invocation", "graph", "spec", "input", "provenance"] {
        let mut candidate = identity.clone();
        let mut candidate_input = input.clone();
        let mut commits = Vec::new();
        match altered {
            "invocation" => candidate.invocation += 1,
            "graph" => candidate.parent_graph_digest = "unproven-graph".into(),
            "spec" => candidate.call_spec_digest = "unproven-spec".into(),
            "input" => candidate_input["input"]["session"] = json!("another-session"),
            "provenance" => commits.push(CommitRef {
                id: "unproven-commit".into(),
                node_id: "source".into(),
                invocation: 1,
            }),
            _ => unreachable!(),
        }
        assert!(
            matches!(
                host.call(
                    &candidate,
                    &spec,
                    &candidate_input,
                    &commits,
                    Cancellation::default()
                )
                .await,
                Err(GraphError::CorruptRun(_))
            ),
            "{altered}"
        );
        assert!(
            runs.load(&child_run_id(&candidate)).unwrap().is_none(),
            "{altered}"
        );
    }
    assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
    assert!(artifacts.exports.lock().unwrap().is_empty());
}

#[tokio::test]
async fn deferred_wait_admits_once_before_the_shared_executor_runs_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let runs = Runs::new(dir.path());
    let artifacts = Artifacts::default();
    let nodes = Nodes::default();
    let executor = Executor {
        runs: &runs,
        artifacts: &artifacts,
        nodes: &nodes,
        plugins: None,
    };
    let spec = json!({"graph":"child","mode":"wait"});
    let (identity, input) = parent_call(&runs, &spec);
    let host = InProcessGraphHost::new(&OrdinaryCatalog, &runs, &artifacts, &nodes)
        .with_child_executor(&executor, true);
    let child_id = child_run_id(&identity);
    assert_eq!(
        host.call(&identity, &spec, &input, &[], Cancellation::default())
            .await
            .unwrap(),
        GraphCallOutcome::Waiting {
            child_run_id: child_id.clone()
        }
    );
    let admitted = runs.load(&child_id).unwrap().unwrap();
    assert_eq!(admitted.phase, Phase::Executing);
    assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
    let host = InProcessGraphHost::new(&OrdinaryCatalog, &runs, &artifacts, &nodes)
        .with_child_executor(&executor, false);
    assert!(
        matches!(host.call(&identity, &spec, &input, &[], Cancellation::default()).await.unwrap(),
        GraphCallOutcome::Completed { child_run_id, .. } if child_run_id == child_id)
    );
    let completed = runs.load(&child_id).unwrap().unwrap();
    assert_eq!(
        completed.phase,
        Phase::Settled {
            result: Outcome::Done
        }
    );
    assert_eq!(completed.record.graph_digest, admitted.record.graph_digest);
    assert_eq!(completed.record.input, admitted.record.input);
    assert_eq!(nodes.0.load(Ordering::SeqCst), 1);
}
