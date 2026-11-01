use super::*;

mod calls;
mod differential;
use crate::graph::test_support::{Artifacts, commit_of, parallel_fixture_graph};
use anchor_kernel::{Authority, Evidence, MemoryStore, Outcome, Waiter};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::{Semaphore, mpsc};

struct AuthorityFixture;
impl Authority for AuthorityFixture {
    fn permits(&self, p: &Principal, action: Action, _: &str) -> bool {
        match p.kind {
            PrincipalKind::Agent => action == Action::Write,
            PrincipalKind::Code => {
                matches!(action, Action::Write | Action::Settle | Action::Request)
            }
            PrincipalKind::Human => action == Action::Request,
        }
    }
}
fn kernel(snapshot: GraphSnapshot) -> (Kernel<MemoryStore>, String) {
    let record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    let resource = validator::run_resource(&record, 100).unwrap();
    let id = resource.id.clone();
    let mut kernel = Kernel::new(MemoryStore::default(), Box::new(AuthorityFixture)).unwrap();
    kernel
        .install(validator::GRAPH_VALIDATOR, Arc::new(GraphValidator))
        .unwrap();
    kernel.create(resource).unwrap();
    (kernel, id)
}

#[derive(Clone, Copy)]
enum ResultKind {
    Complete,
    Fail,
    Error,
    Panic,
}
struct Nodes {
    gates: BTreeMap<String, Semaphore>,
    results: BTreeMap<String, ResultKind>,
    events: mpsc::UnboundedSender<String>,
    requests: Mutex<Vec<NodeExecutionRequest>>,
    facts: Mutex<BTreeMap<String, NodeCompletion>>,
    active_calls: AtomicUsize,
    max_active_calls: AtomicUsize,
}
impl Nodes {
    fn new(
        gated: &[&str],
        results: &[(&str, ResultKind)],
    ) -> (Self, mpsc::UnboundedReceiver<String>) {
        let (events, receiver) = mpsc::unbounded_channel();
        (
            Self {
                gates: gated
                    .iter()
                    .map(|node| (node.to_string(), Semaphore::new(0)))
                    .collect(),
                results: results
                    .iter()
                    .map(|(node, result)| (node.to_string(), *result))
                    .collect(),
                events,
                requests: Mutex::default(),
                facts: Mutex::default(),
                active_calls: AtomicUsize::new(0),
                max_active_calls: AtomicUsize::new(0),
            },
            receiver,
        )
    }
    fn release(&self, node: &str) {
        self.gates[node].add_permits(1);
    }
}
impl NodeExecutionPort for Nodes {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: true,
            exact_provider_request_budget: true,
            ..Default::default()
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
                .map(CompletionFact::Completed)
                .unwrap_or(CompletionFact::NotStarted))
        })
    }
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let key = request.key.clone();
            self.requests.lock().unwrap().push(request);
            let active = self.active_calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active_calls.fetch_max(active, Ordering::SeqCst);
            self.events.send(key.node_id.clone()).unwrap();
            if let Some(gate) = self.gates.get(&key.node_id) {
                gate.acquire().await.unwrap().forget();
            }
            // The rest of execution is synchronous, so no other call can
            // overlap after the gate has been acquired in this test fixture.
            self.active_calls.fetch_sub(1, Ordering::SeqCst);
            match self
                .results
                .get(&key.node_id)
                .unwrap_or(&ResultKind::Complete)
            {
                ResultKind::Fail => Ok(NodeExecutionOutcome::Failed {
                    reason: format!("{} failure", key.node_id),
                }),
                ResultKind::Error => Err(GraphError::Io(io::Error::other("fixture error"))),
                ResultKind::Panic => panic!("fixture executor panic"),
                ResultKind::Complete => {
                    let completion = NodeCompletion {
                        submission: format!("{} complete", key.node_id),
                        route: None,
                        output: serde_json::json!({"node":key.node_id}),
                        model_requests: 3,
                    };
                    self.facts
                        .lock()
                        .unwrap()
                        .insert(key.durable_key(), completion.clone());
                    Ok(NodeExecutionOutcome::Completed(completion))
                }
            }
        })
    }
}
#[derive(Default)]
struct TrackingArtifacts(Mutex<Vec<(InvocationKey, ArtifactFreezeContext)>>);
impl ArtifactPort for TrackingArtifacts {
    fn freeze<'a>(
        &'a self,
        key: &'a InvocationKey,
        c: &'a NodeCompletion,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
        Artifacts.freeze(key, c)
    }
    fn freeze_with_context<'a>(
        &'a self,
        key: &'a InvocationKey,
        _: &'a NodeCompletion,
        context: &'a ArtifactFreezeContext,
    ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            self.0.lock().unwrap().push((key.clone(), context.clone()));
            Ok(commit_of(key))
        })
    }
    fn resolve<'a>(
        &'a self,
        commit: &'a CommitRef,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
        Artifacts.resolve(commit)
    }
}
fn scheduler<'a>(
    kernel: &'a Kernel<MemoryStore>,
    artifacts: &'a TrackingArtifacts,
    nodes: &'a Nodes,
) -> KernelScheduler<'a, MemoryStore, TrackingArtifacts, Nodes> {
    KernelScheduler {
        kernel,
        observer: None,
        boundary: None,
        artifacts,
        nodes,
        cancellation: Arc::new(AtomicBool::new(false)),
    }
}
async fn started(events: &mut mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(3), events.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn parallel_branches_complete_out_of_order_and_join_receives_all_branch_facts() {
    for right_first in [false, true] {
        // Copied legacy behavior: overlapping branches, exact immutable provenance,
        // one coordinator-owned join. Add a successor to prove no wave barrier.
        let mut snapshot = parallel_fixture_graph();
        let mut tail = snapshot.nodes[1].clone();
        tail.id = "tail".into();
        snapshot.nodes.push(tail);
        snapshot
            .edges
            .iter_mut()
            .find(|e| e.from_node == "left")
            .unwrap()
            .to_node = "tail".into();
        snapshot.edges.push(GraphEdge {
            from_node: "tail".into(),
            to_node: "collect".into(),
        });
        let (kernel, id) = kernel(snapshot);
        let (nodes, mut events) = Nodes::new(&["left", "right", "tail"], &[]);
        let artifacts = TrackingArtifacts::default();
        let scheduler = scheduler(&kernel, &artifacts, &nodes);
        let execute = async {
            let initial = BTreeSet::from([started(&mut events).await, started(&mut events).await]);
            assert_eq!(initial, BTreeSet::from(["left".into(), "right".into()]));
            if right_first {
                nodes.release("right");
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    while !record_of(&kernel.load(&id).unwrap().unwrap())
                        .unwrap()
                        .results
                        .contains_key("right")
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            nodes.release("left");
            assert_eq!(started(&mut events).await, "tail");
            let resource = kernel.load(&id).unwrap().unwrap();
            let record = record_of(&resource).unwrap();
            assert_eq!(record.results["left"].len(), 1);
            assert_eq!(record.results.contains_key("right"), right_first);
            nodes.release("tail");
            if !right_first {
                nodes.release("right");
            }
        };
        let (result, ()) = tokio::join!(scheduler.run(&id), execute);
        let resource = result.unwrap();
        assert_eq!(
            resource.phase(),
            Some(&Phase::Settled {
                result: Outcome::Done
            })
        );
        assert_eq!(resource.task.as_ref().unwrap().budget.spent, 12);
        let record = record_of(&resource).unwrap();
        assert_eq!(nodes.max_active_calls.load(Ordering::SeqCst), 2);
        if right_first {
            assert!(record.results["right"][0].sequence < record.results["left"][0].sequence);
        }
        assert_eq!(record.results["collect"].len(), 1);
        assert_eq!(
            record.results["collect"][0].completion.output["branches"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        for branch in record.results["collect"][0].completion.output["branches"]
            .as_array()
            .unwrap()
        {
            for node in branch["nodes"].as_array().unwrap() {
                let id = node["node"].as_str().unwrap();
                assert_eq!(
                    node["commit"],
                    serde_json::to_value(&record.results[id][0].commit).unwrap()
                );
                assert_eq!(node["output"], record.results[id][0].completion.output);
            }
        }
        let requests = nodes.requests.lock().unwrap();
        assert!(
            !requests
                .iter()
                .any(|r| ["start", "collect"].contains(&r.key.node_id.as_str()))
        );
        for request in requests.iter().filter(|r| r.key.node_id != "after") {
            assert!(request.input["parallel_activation"].is_string());
            assert!(request.input["branch_id"].is_string());
            assert_eq!(request.input_commits.len(), 1);
        }
        let freezes = artifacts.0.lock().unwrap();
        assert_eq!(
            freezes
                .iter()
                .find(|(k, _)| k.node_id == "start")
                .unwrap()
                .1
                .kind,
            ArtifactKind::Fanout
        );
        let join = &freezes
            .iter()
            .find(|(k, _)| k.node_id == "collect")
            .unwrap()
            .1;
        assert_eq!(join.kind, ArtifactKind::Join);
        assert_eq!(join.input_commits.len(), 3);
        let history = kernel.history(&id).unwrap();
        assert!(
            history
                .iter()
                .any(|r| r.verdict.reason == Reason::StaleVersion)
        );
        assert!(
            history
                .iter()
                .filter(|r| r.verdict.is_accepted()
                    && r.submission
                        .slot
                        .as_deref()
                        .is_some_and(|s| s.starts_with("start#") || s.starts_with("collect#")))
                .all(|r| r.stamp.principal().kind == PrincipalKind::Code)
        );
        assert_eq!(history.iter().map(|r| r.stamp.cost()).sum::<u64>(), 12);
    }
}

#[tokio::test]
async fn parallel_branch_failure_is_terminal_and_never_dispatches_join() {
    // Copied legacy behavior, with S17: the later sibling fact is recorded
    // under TaskSettled instead of changing an already terminal Run.
    for late_failure in [false, true] {
        let (kernel, id) = kernel(parallel_fixture_graph());
        let (nodes, mut events) = Nodes::new(
            &["left", "right"],
            &[
                ("left", ResultKind::Fail),
                (
                    "right",
                    if late_failure {
                        ResultKind::Fail
                    } else {
                        ResultKind::Complete
                    },
                ),
            ],
        );
        let artifacts = TrackingArtifacts::default();
        let scheduler = scheduler(&kernel, &artifacts, &nodes);
        let release = async {
            started(&mut events).await;
            started(&mut events).await;
            nodes.release("left");
            // Wait for the durable terminal observation, without a timing race.
            for _ in 0..100 {
                if kernel.load(&id).unwrap().unwrap().is_settled() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(kernel.load(&id).unwrap().unwrap().is_settled());
            nodes.release("right");
        };
        let (result, ()) = tokio::join!(scheduler.run(&id), release);
        let resource = result.unwrap();
        assert!(
            matches!(resource.phase(),Some(Phase::Settled { result:Outcome::Failed { detail,.. } }) if detail=="left failed: left failure")
        );
        let record = record_of(&resource).unwrap();
        assert!(record.parallel.is_some());
        assert!(!record.results.contains_key("collect"));
        assert!(!record.results.contains_key("right"));
        assert!(
            !nodes
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.key.node_id == "collect")
        );
        assert_eq!(
            artifacts.0.lock().unwrap().len(),
            if late_failure { 1 } else { 2 }
        );
        let history = kernel.history(&id).unwrap();
        let late = history
            .iter()
            .filter(|r| {
                r.submission.slot.as_deref() == Some("right#1")
                    && r.verdict.reason != Reason::StaleVersion
            })
            .collect::<Vec<_>>();
        assert_eq!(late.len(), 1);
        assert_eq!(late[0].verdict.reason, Reason::TaskSettled);
        assert_eq!(
            late[0].submission.action,
            if late_failure {
                Action::Settle
            } else {
                Action::Write
            }
        );
        if !late_failure {
            assert!(matches!(
                late[0].submission.evidence.as_slice(),
                [Evidence::Resource { .. }]
            ));
        }
        assert_eq!(
            resource.task.as_ref().unwrap().budget.spent,
            if late_failure { 0 } else { 3 }
        );
    }
}

#[tokio::test]
async fn parallel_error_or_panic_drains_siblings_and_restart_reuses_their_completion() {
    for failure in [ResultKind::Error, ResultKind::Panic] {
        let (kernel, id) = kernel(parallel_fixture_graph());
        let (nodes, mut events) = Nodes::new(&["left", "right"], &[("left", failure)]);
        let artifacts = TrackingArtifacts::default();
        let scheduler = scheduler(&kernel, &artifacts, &nodes);
        let release = async {
            started(&mut events).await;
            started(&mut events).await;
            nodes.release("left");
            tokio::task::yield_now().await;
            nodes.release("right");
        };
        let (result, ()) = tokio::join!(scheduler.run(&id), release);
        assert!(result.is_err());
        let resource = kernel.load(&id).unwrap().unwrap();
        assert_eq!(resource.phase(), Some(&Phase::Executing));
        let record = record_of(&resource).unwrap();
        assert_eq!(record.results["right"].len(), 1);
        assert!(!record.results.contains_key("left"));
        let (fresh, _events) = Nodes::new(&[], &[]);
        let done = super::KernelScheduler {
            nodes: &fresh,
            ..scheduler
        }
        .run(&id)
        .await
        .unwrap();
        assert_eq!(
            done.phase(),
            Some(&Phase::Settled {
                result: Outcome::Done
            })
        );
        assert!(
            fresh
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| r.key.node_id != "right")
        );
    }
}

#[tokio::test]
async fn parallel_stop_keeps_late_costs_and_reuses_durable_facts_after_resume() {
    let (kernel, id) = kernel(parallel_fixture_graph());
    let (nodes, mut events) = Nodes::new(&["left", "right"], &[]);
    let artifacts = TrackingArtifacts::default();
    let scheduler = scheduler(&kernel, &artifacts, &nodes);
    let stop = async {
        started(&mut events).await;
        started(&mut events).await;
        let resource = kernel.load(&id).unwrap().unwrap();
        assert!(
            kernel
                .submit(
                    &Principal::new(OPERATOR_ID, PrincipalKind::Human),
                    0,
                    Submission::new(&id, resource.version, Action::Request)
                        .content(serde_json::json!({"control":"stop","reason":"test"}))
                )
                .unwrap()
                .is_accepted()
        );
        nodes.release("left");
        nodes.release("right");
    };
    let (result, ()) = tokio::join!(scheduler.run(&id), stop);
    let waiting = result.unwrap();
    assert_eq!(
        waiting.phase(),
        Some(&Phase::Waiting {
            waiter: Waiter::Human {
                reason: "test".into()
            }
        })
    );
    assert_eq!(waiting.task.as_ref().unwrap().budget.spent, 6);
    let history = kernel.history(&id).unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|r| r.verdict.reason == Reason::TaskWaiting)
            .count(),
        2
    );
    assert!(
        kernel
            .submit(
                &Principal::new(OPERATOR_ID, PrincipalKind::Human),
                0,
                Submission::new(&id, waiting.version, Action::Request)
                    .content(serde_json::json!({"control":"resume"}))
            )
            .unwrap()
            .is_accepted()
    );
    let resource = scheduler.run(&id).await.unwrap();
    assert!(resource.is_settled());
    let requests = nodes.requests.lock().unwrap();
    for node in ["left", "right"] {
        assert_eq!(requests.iter().filter(|r| r.key.node_id == node).count(), 1);
    }
}

#[tokio::test]
async fn immediate_sibling_completions_are_dispatched_once_and_late_facts_are_retained() {
    let (kernel, id) = kernel(parallel_fixture_graph());
    let (nodes, _events) = Nodes::new(&[], &[("left", ResultKind::Fail)]);
    let artifacts = TrackingArtifacts::default();
    let result = scheduler(&kernel, &artifacts, &nodes)
        .run(&id)
        .await
        .unwrap();
    assert!(matches!(
        result.phase(),
        Some(Phase::Settled {
            result: Outcome::Failed { .. }
        })
    ));
    let requests = nodes.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|r| r.key.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["left", "right"]
    );
    let history = kernel.history(&id).unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|r| r.submission.slot.as_deref() == Some("right#1")
                && r.verdict.reason == Reason::TaskSettled)
            .count(),
        1
    );
    assert_eq!(result.task.as_ref().unwrap().budget.spent, 3);
}

#[tokio::test]
async fn parallel_region_counts_module_activation_once_and_enforces_reentry_ceiling() {
    // Copy the legacy shared-module case to the real Kernel scheduler as well
    // as the validator tests. These nodes may pass once per module activation.
    for limit in [1, 2] {
        let mut snapshot = parallel_fixture_graph();
        for node in &mut snapshot.nodes {
            if ["left", "right"].contains(&node.id.as_str()) {
                node.id = format!("module/{}", node.id);
                node.max_rounds = Some(1);
            }
        }
        for edge in &mut snapshot.edges {
            if ["left", "right"].contains(&edge.from_node.as_str()) {
                edge.from_node = format!("module/{}", edge.from_node);
            }
            if ["left", "right"].contains(&edge.to_node.as_str()) {
                edge.to_node = format!("module/{}", edge.to_node);
            }
        }
        snapshot.edges.push(GraphEdge {
            from_node: "after".into(),
            to_node: "start".into(),
        });
        snapshot.module_rounds.insert("module".into(), limit);
        let (kernel, id) = kernel(snapshot);
        let (nodes, _events) = Nodes::new(&[], &[]);
        let artifacts = TrackingArtifacts::default();
        let resource = scheduler(&kernel, &artifacts, &nodes)
            .run(&id)
            .await
            .unwrap();
        assert!(
            matches!(resource.phase(),Some(Phase::Settled { result:Outcome::Failed {detail,..} }) if detail.contains("module activation ceiling"))
        );
        let record = record_of(&resource).unwrap();
        assert_eq!(record.module_activations["module"], u64::from(limit));
        assert!(record.ceased.contains(&format!("module@{limit}")));
        for node in ["module/left", "module/right"] {
            assert_eq!(record.results[node].len(), limit as usize);
            assert_eq!(record.invocations[node], u64::from(limit));
            assert_eq!(record.passes[node], 1);
            assert_eq!(
                nodes
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| r.key.node_id == node)
                    .count(),
                limit as usize
            );
        }
    }
}

#[tokio::test]
async fn omitted_agent_model_reaches_the_host_as_no_reference() {
    let snapshot = GraphSnapshot::from_authoring(serde_json::json!({
        "entry":"work", "agents":{"worker":{}},
        "nodes":[{"id":"work","agent":"worker"}], "edges":[]
    }))
    .unwrap();
    let (kernel, id) = kernel(snapshot);
    let (nodes, _events) = Nodes::new(&[], &[]);
    KernelScheduler {
        kernel: &kernel,
        observer: None,
        boundary: None,
        artifacts: &Artifacts,
        nodes: &nodes,
        cancellation: Arc::new(AtomicBool::new(false)),
    }
    .run(&id)
    .await
    .unwrap();
    assert!(nodes.requests.lock().unwrap()[0].model.is_none());
}
