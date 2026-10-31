use super::*;

mod parallel;
mod recovery;
use anchor_kernel::{Evidence, Outcome, Reason, Waiter};
use anchor_runtime::graph::test_support::snapshots::{FrozenStatus, Population, Snapshots};
use anchor_runtime::graph::test_support::{
    Artifacts, Scripted, ScriptedCalls, facts, random_graph,
};
use anchor_runtime::graph::*;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::atomic::AtomicBool};
use tokio::sync::{Notify, Semaphore};

#[derive(Clone, Copy)]
enum Mode {
    Complete,
    Fail,
    Gate,
    PauseGate,
    PauseLastGate,
    Crash(bool),
    Recovery,
}
struct Nodes {
    root: std::path::PathBuf,
    mode: Mode,
    once: AtomicBool,
    trace: Mutex<Vec<InvocationKey>>,
    started: Notify,
    release: Semaphore,
}
impl Nodes {
    fn new(root: &Path, mode: Mode) -> Self {
        std::fs::create_dir_all(root).unwrap();
        Self {
            root: root.into(),
            mode,
            once: AtomicBool::new(true),
            trace: Mutex::default(),
            started: Notify::new(),
            release: Semaphore::new(0),
        }
    }
    fn fact_path(&self, key: &InvocationKey) -> std::path::PathBuf {
        self.root.join(key.durable_key())
    }
    fn save(&self, key: &InvocationKey) {
        std::fs::write(
            self.fact_path(key),
            serde_json::to_vec(&completion()).unwrap(),
        )
        .unwrap();
    }
}
fn completion() -> NodeCompletion {
    NodeCompletion {
        submission: "fixture result".into(),
        route: None,
        model_requests: 3,
        output: json!({"proof":"host-port","files":[]}),
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
            match std::fs::read(self.fact_path(key)) {
                Ok(bytes) if bytes == b"unknown" => {
                    Ok(CompletionFact::Uncertain("verify external effect".into()))
                }
                Ok(bytes) => Ok(CompletionFact::Completed(
                    serde_json::from_slice(&bytes).unwrap(),
                )),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Ok(CompletionFact::NotStarted)
                }
                Err(e) => Err(e.into()),
            }
        })
    }
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(request.key.clone());
            let first = self.once.swap(false, Ordering::AcqRel);
            match self.mode {
                Mode::Fail => {
                    return Ok(NodeExecutionOutcome::Failed {
                        reason: "known failure".into(),
                    });
                }
                Mode::Gate if first => {
                    self.started.notify_one();
                    self.release.acquire().await.unwrap().forget();
                    assert!(request.cancellation.load(Ordering::Acquire));
                }
                Mode::PauseGate if first => {
                    self.started.notify_one();
                    self.release.acquire().await.unwrap().forget();
                    assert!(!request.cancellation.load(Ordering::Acquire));
                }
                Mode::PauseLastGate if request.key.node_id == "c" => {
                    self.started.notify_one();
                    self.release.acquire().await.unwrap().forget();
                    assert!(!request.cancellation.load(Ordering::Acquire));
                }
                Mode::Crash(completed) if request.key.node_id == "b" => {
                    if completed {
                        self.save(&request.key);
                    }
                    return Err(std::io::Error::other("fixture process interruption").into());
                }
                Mode::Recovery if first => {
                    return Ok(NodeExecutionOutcome::WaitingRecovery { attempts: vec![] });
                }
                _ => {}
            }
            self.save(&request.key);
            Ok(NodeExecutionOutcome::Completed(completion()))
        })
    }
}
fn record(id: &str) -> GraphRunRecord {
    GraphRunRecord::create_with_id(serde_json::from_value(json!({
        "objective":"probe", "entry":"a", "agents":{"worker":{"model":"fixture","instructions":"work"}},
        "nodes":[{"id":"a","agent":"worker"},{"id":"b","agent":"worker"},{"id":"c","agent":"worker"}],
        "edges":[{"from":"a","to":"b"},{"from":"b","to":"c"}]
    })).unwrap(), json!({}), id).unwrap()
}
fn app(root: &Path) -> RunApplication {
    RunApplication::new(root.into(), root.join("catalog"))
}
fn cancel() -> anchor_runtime::Cancellation {
    Arc::new(AtomicBool::new(false))
}

fn setup(
    id: &str,
    budget: u64,
) -> (
    tempfile::TempDir,
    RunApplication,
    Arc<KernelRuns>,
    GraphRunRecord,
) {
    let root = tempfile::tempdir().unwrap();
    let application = app(root.path());
    let runs = application.kernel_runs(true).unwrap().unwrap();
    let record = record(id);
    runs.create(&record, budget).unwrap();
    (root, application, runs, record)
}

fn spawn_drive(
    runs: &Arc<KernelRuns>,
    nodes: &Arc<Nodes>,
    id: &str,
) -> tokio::task::JoinHandle<Result<anchor_kernel::Resource, GraphError>> {
    let runs = runs.clone();
    let nodes = nodes.clone();
    let id = id.to_owned();
    tokio::spawn(async move { runs.drive(&id, &Artifacts, nodes.as_ref(), cancel()).await })
}

#[tokio::test]
async fn worker_registration_is_visible_before_poll_and_drops_with_its_future() {
    let (root, _application, runs, record) = setup("registered-before-poll", 30);
    let nodes = Nodes::new(&root.path().join("facts"), Mode::Complete);
    let worker = runs.drive(&record.run_id, &Artifacts, &nodes, cancel());
    assert!(runs.active(&record.run_id));
    assert!(matches!(
        runs.drive(&record.run_id, &Artifacts, &nodes, cancel())
            .await,
        Err(GraphError::RunBusy(_))
    ));
    assert!(runs.active(&record.run_id));
    drop(worker);
    assert!(!runs.active(&record.run_id));
    assert!(nodes.trace.lock().unwrap().is_empty());
    runs.drive(&record.run_id, &Artifacts, &nodes, cancel())
        .await
        .unwrap();
    assert!(!runs.active(&record.run_id));
    assert_eq!(nodes.trace.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn registered_worker_aborted_before_first_poll_releases_the_same_registry() {
    let (root, _application, runs, record) = setup("registered-aborted-before-poll", 30);
    let registration = runs.register(&record.run_id, cancel()).unwrap();
    let owner = Arc::clone(&runs);
    let nodes = Nodes::new(&root.path().join("facts"), Mode::Complete);
    let worker = tokio::spawn(async move {
        owner
            .drive_registered(registration, &Artifacts, &nodes)
            .await
    });
    assert!(runs.active(&record.run_id));
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    assert!(!runs.active(&record.run_id));
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runs.wait_idle(&record.run_id),
    )
    .await
    .unwrap();
    assert!(
        KernelRuns::record(&runs.load(&record.run_id).unwrap().unwrap())
            .unwrap()
            .results
            .is_empty()
    );
}

#[tokio::test]
async fn wait_idle_releases_all_waiters_when_an_unpolled_worker_is_dropped() {
    let (root, _application, runs, record) = setup("registered-waiters", 30);
    let nodes = Nodes::new(&root.path().join("facts"), Mode::Complete);
    let worker = runs.drive(&record.run_id, &Artifacts, &nodes, cancel());
    let first = runs.wait_idle(&record.run_id);
    let second = runs.wait_idle(&record.run_id);
    tokio::pin!(first, second);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(first.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(second.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    assert!(runs.active(&record.run_id));
    drop(worker);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(first, second);
    })
    .await
    .unwrap();
    assert!(!runs.active(&record.run_id));
    assert!(nodes.trace.lock().unwrap().is_empty());
}

#[tokio::test]
async fn serial_probe_commits_artifacts_cost_and_phase_through_application() {
    let (root, application, runs, record) = setup("p-serial", 30);
    let nodes = Nodes::new(&root.path().join("facts"), Mode::Complete);
    let resource = runs
        .drive(&record.run_id, &Artifacts, &nodes, cancel())
        .await
        .unwrap();
    assert_eq!(
        resource.phase(),
        Some(&Phase::Settled {
            result: Outcome::Done
        })
    );
    assert_eq!(resource.task.as_ref().unwrap().budget.spent, 9);
    let projected = application.load_run(&record.run_id).unwrap().unwrap();
    assert!(
        application
            .read_run(&record.run_id)
            .unwrap()
            .unwrap()
            .has_succeeded()
    );
    assert_eq!(projected.results.len(), 3);
    for report in runs
        .kernel
        .history(&run_resource_id(&record.run_id))
        .unwrap()
    {
        assert_eq!(report.stamp.principal().kind, PrincipalKind::Agent);
        assert_eq!(report.stamp.cost(), 3);
        assert!(matches!(
            report.submission.evidence.as_slice(),
            [Evidence::Resource { .. }]
        ));
    }
    assert!(
        application
            .project_run(
                &application.read_run(&record.run_id).unwrap().unwrap(),
                None
            )
            .await
            .unwrap()
            .phase
            == "settled"
    );
    assert!(!root.path().join("runs").exists());
    assert_eq!(application.records().unwrap().len(), 1);
}

#[tokio::test]
async fn pause_commits_current_result_before_human_wait_and_resume_does_not_replay() {
    let (root, application, runs, record) = setup("m1-pause", 50);
    assert!(application.control(&record.run_id, "pause").await.is_err());
    let nodes = Arc::new(Nodes::new(&root.path().join("facts"), Mode::PauseGate));
    let worker = spawn_drive(&runs, &nodes, &record.run_id);
    nodes.started.notified().await;
    application.control(&record.run_id, "pause").await.unwrap();
    assert_eq!(
        runs.load(&record.run_id).unwrap().unwrap().phase(),
        Some(&Phase::Executing)
    );
    assert!(runs.pause_pending(&record.run_id).unwrap());
    assert!(application.control(&record.run_id, "resume").await.is_err());
    nodes.release.add_permits(1);
    let resource = worker.await.unwrap().unwrap();
    assert_eq!(
        resource.phase(),
        Some(&Phase::Waiting {
            waiter: Waiter::Human {
                reason: "paused".into()
            }
        })
    );
    assert_eq!(resource.task.as_ref().unwrap().budget.spent, 3);
    let paused = KernelRuns::record(&resource).unwrap();
    assert_eq!(paused.results.len(), 1);
    assert!(paused.results.contains_key("a"));
    assert_eq!(nodes.trace.lock().unwrap().len(), 1);
    assert!(!runs.pause_pending(&record.run_id).unwrap());
    let history = runs
        .kernel
        .history(&run_resource_id(&record.run_id))
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].submission.slot.as_deref(), Some("a#1"));
    assert_eq!(history[1].stamp.principal().kind, PrincipalKind::Human);
    assert_eq!(history[1].submission.content["reason"], "paused");
    assert!(application.control(&record.run_id, "pause").await.is_err());
    runs.control(&record.run_id, "resume", None).unwrap();
    let finished = runs
        .drive(&record.run_id, &Artifacts, nodes.as_ref(), cancel())
        .await
        .unwrap();
    assert!(finished.is_settled());
    assert_eq!(KernelRuns::record(&finished).unwrap().results.len(), 3);
    assert_eq!(nodes.trace.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn pause_on_final_node_preserves_settlement_and_all_committed_results() {
    let (root, application, runs, record) = setup("m1-pause-final", 50);
    let nodes = Arc::new(Nodes::new(&root.path().join("facts"), Mode::PauseLastGate));
    let worker = spawn_drive(&runs, &nodes, &record.run_id);
    nodes.started.notified().await;
    application.control(&record.run_id, "pause").await.unwrap();
    nodes.release.add_permits(1);
    let resource = worker.await.unwrap().unwrap();
    assert_eq!(
        resource.phase(),
        Some(&Phase::Settled {
            result: Outcome::Done
        })
    );
    assert_eq!(resource.task.as_ref().unwrap().budget.spent, 9);
    assert_eq!(KernelRuns::record(&resource).unwrap().results.len(), 3);
    assert_eq!(nodes.trace.lock().unwrap().len(), 3);
    assert!(!runs.pause_pending(&record.run_id).unwrap());
}

#[tokio::test]
async fn stop_preempts_pending_pause_without_repausing_the_resumed_run() {
    let (root, application, runs, record) = setup("m1-pause-stop", 50);
    let nodes = Arc::new(Nodes::new(&root.path().join("facts"), Mode::Gate));
    let worker = spawn_drive(&runs, &nodes, &record.run_id);
    nodes.started.notified().await;
    application.control(&record.run_id, "pause").await.unwrap();
    application.control(&record.run_id, "stop").await.unwrap();
    assert!(!runs.pause_pending(&record.run_id).unwrap());
    nodes.release.add_permits(1);
    let resource = worker.await.unwrap().unwrap();
    assert_eq!(
        resource.phase(),
        Some(&Phase::Waiting {
            waiter: Waiter::Human {
                reason: "stopped".into()
            }
        })
    );
    assert!(KernelRuns::record(&resource).unwrap().results.is_empty());
    runs.control(&record.run_id, "resume", None).unwrap();
    let resource = runs
        .drive(&record.run_id, &Artifacts, nodes.as_ref(), cancel())
        .await
        .unwrap();
    assert_eq!(
        resource.phase(),
        Some(&Phase::Settled {
            result: Outcome::Done
        })
    );
    assert_eq!(KernelRuns::record(&resource).unwrap().results.len(), 3);
    assert_eq!(nodes.trace.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn pending_pause_survives_host_restart_without_dispatching_another_node() {
    pending_pause_restart(false).await;
}

#[tokio::test]
async fn pending_pause_restart_commits_durable_completion_before_waiting() {
    pending_pause_restart(true).await;
}

async fn pending_pause_restart(completed: bool) {
    let (root, application, runs, record) = setup("m1-pause-restart", 50);
    let nodes = Arc::new(Nodes::new(&root.path().join("facts"), Mode::PauseGate));
    let worker = spawn_drive(&runs, &nodes, &record.run_id);
    nodes.started.notified().await;
    runs.control(&record.run_id, "pause", None).unwrap();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    if completed {
        nodes.save(&nodes.trace.lock().unwrap()[0]);
    }
    drop(runs);
    drop(application);
    let restarted = app(root.path());
    let runs = restarted.kernel_runs(false).unwrap().unwrap();
    let resource = runs
        .drive(&record.run_id, &Artifacts, nodes.as_ref(), cancel())
        .await
        .unwrap();
    assert_eq!(
        resource.phase(),
        Some(&Phase::Waiting {
            waiter: Waiter::Human {
                reason: "paused".into()
            }
        })
    );
    assert_eq!(nodes.trace.lock().unwrap().len(), 1);
    assert_eq!(
        KernelRuns::record(&resource).unwrap().results.len(),
        usize::from(completed)
    );
    assert_eq!(
        resource.task.as_ref().unwrap().budget.spent,
        if completed { 3 } else { 0 }
    );
    assert!(!runs.pause_pending(&record.run_id).unwrap());
}

#[tokio::test]
async fn stop_refuses_late_result_then_resume_uses_durable_completion() {
    let (root, application, runs, record) = setup("p-stop", 50);
    let nodes = Arc::new(Nodes::new(&root.path().join("facts"), Mode::Gate));
    let worker = spawn_drive(&runs, &nodes, &record.run_id);
    nodes.started.notified().await;
    application.control("p-stop", "stop").await.unwrap();
    assert!(matches!(
        runs.load("p-stop").unwrap().unwrap().phase(),
        Some(Phase::Waiting { .. })
    ));
    nodes.release.add_permits(1);
    worker.await.unwrap().unwrap();
    let history = runs.kernel.history(&run_resource_id("p-stop")).unwrap();
    assert!(
        history
            .iter()
            .any(|r| r.verdict.reason == Reason::TaskWaiting
                && r.submission.slot.as_deref() == Some("a#1"))
    );
    assert_eq!(
        application
            .load_run("p-stop")
            .unwrap()
            .unwrap()
            .results
            .len(),
        0
    );
    runs.control("p-stop", "resume", None).unwrap();
    let result = runs
        .drive("p-stop", &Artifacts, nodes.as_ref(), cancel())
        .await
        .unwrap();
    assert!(result.is_settled());
    assert_eq!(
        nodes
            .trace
            .lock()
            .unwrap()
            .iter()
            .filter(|k| k.node_id == "a")
            .count(),
        1
    );
}

#[tokio::test]
async fn abandon_waiting_probe_is_terminal_and_refuses_late_submission() {
    let (_root, application, runs, _record) = setup("p-abandon", 50);
    application.control("p-abandon", "stop").await.unwrap();
    application.abandon_run("p-abandon", None).await.unwrap();
    let resource = runs.load("p-abandon").unwrap().unwrap();
    assert!(matches!(
        resource.phase(),
        Some(Phase::Settled {
            result: Outcome::Aborted { .. }
        })
    ));
    let verdict = runs
        .kernel
        .submit(
            &Principal::new(HOST_ID, PrincipalKind::Code),
            0,
            Submission::new(
                run_resource_id("p-abandon"),
                resource.version,
                Action::Settle,
            )
            .slot("a#1")
            .content(json!({"reason":"late"})),
        )
        .unwrap();
    assert_eq!(verdict.reason, Reason::TaskSettled);
    assert!(
        application
            .project_run(&application.read_run("p-abandon").unwrap().unwrap(), None)
            .await
            .unwrap()
            .phase
            == "settled"
    );
}

#[tokio::test]
async fn restart_recovers_same_invocation_from_completed_or_not_started_fact() {
    for completed in [true, false] {
        let (root, application, runs, _record) = setup("p-restart", 50);
        let crashing = Nodes::new(&root.path().join("facts"), Mode::Crash(completed));
        assert!(
            runs.drive("p-restart", &Artifacts, &crashing, cancel())
                .await
                .is_err()
        );
        assert_eq!(
            crashing
                .trace
                .lock()
                .unwrap()
                .iter()
                .map(|k| (&*k.node_id, k.invocation))
                .collect::<Vec<_>>(),
            vec![("a", 1), ("b", 1)]
        );
        drop(runs);
        drop(application);
        let fresh = app(root.path());
        let runs = fresh.kernel_runs(false).unwrap().unwrap();
        let resumed = Nodes::new(&root.path().join("facts"), Mode::Complete);
        assert!(
            runs.drive("p-restart", &Artifacts, &resumed, cancel())
                .await
                .unwrap()
                .is_settled()
        );
        let trace = resumed.trace.lock().unwrap();
        assert!(trace.iter().all(|k| k.node_id != "a" && k.invocation == 1));
        assert_eq!(
            trace.iter().filter(|k| k.node_id == "b").count(),
            usize::from(!completed)
        );
        assert_eq!(
            fresh.load_run("p-restart").unwrap().unwrap().invocations["b"],
            1
        );
    }
}

#[tokio::test]
async fn uncertain_effect_parks_until_human_verification_then_resume_or_abandon() {
    for abandon in [false, true] {
        let (root, application, runs, record) = setup("p-unknown", 50);
        let nodes = Nodes::new(&root.path().join("facts"), Mode::Complete);
        let key = InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: "a".into(),
            invocation: 1,
        };
        std::fs::write(nodes.fact_path(&key), b"unknown").unwrap();
        runs.drive("p-unknown", &Artifacts, &nodes, cancel())
            .await
            .unwrap();
        assert!(nodes.trace.lock().unwrap().is_empty());
        let history = runs.kernel.history(&run_resource_id("p-unknown")).unwrap();
        assert_eq!(history[0].stamp.principal().kind, PrincipalKind::Code);
        assert!(matches!(
            history[0].submission.evidence.as_slice(),
            [Evidence::Observation { .. }]
        ));
        if abandon {
            application.abandon_run("p-unknown", None).await.unwrap();
        } else {
            // The operator has verified the durable result, before resume.
            nodes.save(&key);
            runs.control("p-unknown", "resume", None).unwrap();
            runs.drive("p-unknown", &Artifacts, &nodes, cancel())
                .await
                .unwrap();
            assert!(nodes.trace.lock().unwrap().iter().all(|k| k.node_id != "a"));
        }
        assert!(runs.load("p-unknown").unwrap().unwrap().is_settled());
    }
}

#[tokio::test]
async fn waiting_recovery_outcome_parks_and_known_failure_settles() {
    for mode in [Mode::Recovery, Mode::Fail] {
        let (root, _application, runs, _record) = setup("p-outcome", 50);
        let nodes = Nodes::new(&root.path().join("facts"), mode);
        let resource = runs
            .drive("p-outcome", &Artifacts, &nodes, cancel())
            .await
            .unwrap();
        match mode {
            Mode::Recovery => {
                assert!(matches!(
                    resource.phase(),
                    Some(Phase::Waiting {
                        waiter: Waiter::Human { .. }
                    })
                ));
                assert_eq!(nodes.trace.lock().unwrap().len(), 1);
            }
            Mode::Fail => {
                assert!(
                    matches!(resource.phase(), Some(Phase::Settled { result: Outcome::Failed { detail, .. } }) if detail == "a failed: known failure")
                );
                let record = record_of(&resource).unwrap();
                assert!(record.results.is_empty());
                assert_eq!(record.invocations["a"], 1);
            }
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn application_matches_frozen_baseline_on_one_thousand_serial_seeds() {
    let root = tempfile::tempdir().unwrap();
    let application = app(root.path());
    let runs = application.kernel_runs(true).unwrap().unwrap();
    let mut snapshots = Snapshots::open(Population::Serial);
    for seed in 0..1000 {
        let record = GraphRunRecord::create_with_id(
            random_graph(seed),
            Value::Null,
            format!("scheduler-diff-{seed}"),
        )
        .unwrap();
        let case = snapshots.next(seed);
        let (old_trace, old) = case.scheduler.accepted();
        runs.create(&record, u64::MAX).unwrap();
        let nodes = Scripted {
            seed,
            inject_failures: true,
            trace: Mutex::default(),
            calls: ScriptedCalls {
                seed,
                ..Default::default()
            },
        };
        let resource = runs
            .drive(&record.run_id, &Artifacts, &nodes, cancel())
            .await
            .unwrap();
        assert_eq!(
            old_trace,
            &*nodes.trace.lock().unwrap(),
            "trace seed {seed}"
        );
        assert_eq!(
            facts(&old.graph_record()),
            facts(&record_of(&resource).unwrap()),
            "facts seed {seed}"
        );
        match old.status {
            FrozenStatus::Completed => assert_eq!(
                resource.phase(),
                Some(&Phase::Settled {
                    result: Outcome::Done
                })
            ),
            FrozenStatus::Stopped => assert!(matches!(
                resource.phase(),
                Some(Phase::Settled {
                    result: Outcome::Escalated { .. }
                })
            )),
            FrozenStatus::Failed => assert!(
                matches!(resource.phase(), Some(Phase::Settled { result:Outcome::Failed { detail, .. } }) if Some(detail) == old.error.as_ref())
            ),
            status => panic!("unexpected {status:?}"),
        }
    }
}

#[test]
fn probe_guards_keep_one_writer_and_charge_stale_work_once() {
    let (root, application, runs, record) = setup("p-stale", 50);
    assert!(matches!(
        app(root.path()).kernel_runs(false),
        Err(ApplicationError::Conflict(_))
    ));
    runs.control("p-stale", "stop", None).unwrap();
    runs.control("p-stale", "resume", None).unwrap();
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "a".into(),
        invocation: 1,
    };
    let commit = anchor_runtime::graph::test_support::commit_of(&key);
    let content = anchor_runtime::graph::validator::NodeSubmission {
        summary: "done".into(),
        route: None,
        output: json!({}),
        model_requests: 999999,
        commit: commit.clone(),
        plugin_views: Default::default(),
        plugin_digests: Default::default(),
        interruption: None,
    };
    let verdict = submit_with_retry(
        &runs.kernel,
        &Principal::new(EXECUTOR_ID, PrincipalKind::Agent),
        3,
        Submission::new(run_resource_id("p-stale"), 0, Action::Write)
            .slot("a#1")
            .content(serde_json::to_value(content).unwrap())
            .evidence(Evidence::Resource {
                id: commit.id,
                version: 0,
            }),
        None,
    )
    .unwrap();
    assert!(verdict.is_accepted());
    let history = runs.kernel.history(&run_resource_id("p-stale")).unwrap();
    assert_eq!(history[2].verdict.reason, Reason::StaleVersion);
    assert_eq!((history[2].stamp.cost(), history[3].stamp.cost()), (3, 0));
    let resource = runs.load("p-stale").unwrap().unwrap();
    assert_eq!(resource.task.as_ref().unwrap().budget.spent, 3);
    let refused = runs
        .kernel
        .submit(
            &Principal::new(EXECUTOR_ID, PrincipalKind::Agent),
            0,
            Submission::new(&resource.id, resource.version, Action::Settle)
                .slot("b#1")
                .content(json!({"reason":"model claim"})),
        )
        .unwrap();
    assert_eq!(refused.reason, Reason::Unauthorized);
    std::fs::create_dir_all(root.path().join("runs")).unwrap();
    std::fs::write(root.path().join("runs/p-stale.json"), b"{}").unwrap();
    assert!(matches!(
        application.load_run("p-stale"),
        Err(ApplicationError::Storage(message)) if message.contains("legacy_run_record")
    ));
}

#[test]
fn broken_host_observation_never_blocks_kernel_control_or_lifecycle_reads() {
    let (root, _application, runs, record) = setup("r-observation-failure", 50);
    let observations = root.path().join("kernel-run-observations");
    std::fs::remove_file(observations.join("r-observation-failure.json")).unwrap();
    std::fs::remove_dir(&observations).unwrap();
    std::fs::write(&observations, b"fixture unavailable").unwrap();
    runs.control(&record.run_id, "stop", None).unwrap();
    let resource = runs.load(&record.run_id).unwrap().unwrap();
    let view = runs.view(&resource).unwrap();
    assert!(matches!(
        view.phase,
        Phase::Waiting {
            waiter: Waiter::Human { .. }
        }
    ));
    assert!(view.updated.is_none());
    assert!(view.actions().resume);
    runs.control(&record.run_id, "abandon", None).unwrap();
    assert!(runs.load(&record.run_id).unwrap().unwrap().is_settled());
}
