//! Test construction through the same Kernel authority as production Runs.
//! Each helper records an actual admission, node result or control submission.
use super::{RunView, kernel_runs::KernelRuns};
use crate::application::{ApplicationError, RunApplication};
use anchor_kernel::{Action, Evidence, Phase, Principal, PrincipalKind, Submission};
use anchor_runtime::graph::{
    CommitRef, GraphRunRecord, GraphSnapshot, InvocationKey, NodeCompletion, RecoveryAttempt,
    scheduler::{EXECUTOR_ID, HOST_ID},
    validator::{CallSubmission, DispatchKind, GraphValidator, NodeSubmission, SettleSubmission},
};
use anchor_runtime::graph::{
    CompletionFact, GraphError, NodeExecutionCapabilities, NodeExecutionOutcome, NodeExecutionPort,
    NodeExecutionRequest,
};
use serde_json::json;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
};

struct Gate {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    plugins: std::collections::BTreeMap<String, anchor_runtime::graph::PluginBinding>,
}

impl NodeExecutionPort for Gate {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: true,
            op_run: true,
            exact_provider_request_budget: true,
            ..Default::default()
        }
    }
    fn resolve_plugins(
        &self,
        ids: &[String],
    ) -> Result<Vec<anchor_runtime::graph::PluginBinding>, GraphError> {
        ids.iter()
            .map(|id| {
                self.plugins.get(id).cloned().ok_or_else(|| {
                    GraphError::Unsupported(format!("unknown fixture Plugin `{id}`"))
                })
            })
            .collect()
    }
    fn completion_fact<'a>(
        &'a self,
        _: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
        Box::pin(async { Ok(CompletionFact::NotStarted) })
    }
    fn execute<'a>(
        &'a self,
        _: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            Ok(NodeExecutionOutcome::Suspended)
        })
    }
}

pub(crate) struct GatedDrive {
    gate: Arc<Gate>,
    cancellation: anchor_runtime::Cancellation,
    task: tokio::task::JoinHandle<Result<anchor_kernel::Resource, GraphError>>,
}

impl GatedDrive {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancellation.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(crate) async fn drain(self, app: &RunApplication, id: &str) {
        if !view(app, id).is_settled() {
            control(app, id, "stop", "stopped");
        }
        self.gate.release.add_permits(1);
        self.task.await.unwrap().unwrap();
        assert!(!view(app, id).active());
    }
}

pub(crate) async fn gated_drive(app: &RunApplication, id: &str) -> GatedDrive {
    let gate = Arc::new(Gate {
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        plugins: record(app, id).plugin_bindings,
    });
    let worker = gate.clone();
    let cancellation = Arc::new(AtomicBool::new(false));
    let worker_cancellation = cancellation.clone();
    let runs = app.kernel_owner(id).unwrap().unwrap();
    let worker_id = id.to_owned();
    let task = tokio::spawn(async move {
        runs.drive(
            &worker_id,
            &anchor_runtime::graph::test_support::Artifacts,
            worker.as_ref(),
            worker_cancellation,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.acquire())
        .await
        .unwrap_or_else(|_| {
            let finished = task.is_finished();
            task.abort();
            let observed = app
                .read_run(id)
                .map(|run| run.map(|view| (format!("{:?}", view.phase()), view.active())));
            panic!("fixture Kernel worker did not enter its port: finished={finished}, observed={observed:?}")
        })
        .unwrap()
        .forget();
    GatedDrive {
        gate,
        cancellation,
        task,
    }
}

pub(crate) fn work_record(id: &str) -> GraphRunRecord {
    GraphRunRecord::create_with_id(GraphSnapshot::from_authoring(json!({
        "objective":"test", "entry":"worker", "agents":{"role":{"model":"fixture","instructions":"work"}},
        "nodes":[{"id":"worker","agent":"role"}], "edges":[]
    })).unwrap(), json!({}), id).unwrap()
}

pub(crate) fn scenarios(app: &RunApplication) -> [&'static str; 9] {
    for id in [
        "executing",
        "paused",
        "stopped",
        "budget",
        "recovery",
        "done",
        "failed",
        "aborted",
    ] {
        create(app, &work_record(id));
    }
    for reason in ["paused", "stopped", "budget"] {
        control(app, reason, "stop", reason);
    }
    recovery(
        app,
        "recovery",
        "worker",
        vec![RecoveryAttempt {
            attempt_id: 1,
            step: 1,
            tool: "fixture-effect".into(),
            started_at: "2026-10-19T00:00:00Z".into(),
        }],
    );
    finish(app, "done");
    fail(app, "failed", "worker", "recorded reason");
    control(app, "aborted", "abandon", "recorded reason");
    let child = GraphRunRecord::create_with_id(GraphSnapshot::from_authoring(json!({
        "entry":"invoke", "agents":{}, "ops":{"invoke":{"call":{"graph":"child","mode":"wait"}}},
        "nodes":[{"id":"invoke","op":"invoke"}], "edges":[]
    })).unwrap(), json!({}), "child").unwrap();
    create(app, &child);
    bind_child(app, "child", "invoke", "child-target");
    [
        "executing",
        "paused",
        "stopped",
        "budget",
        "recovery",
        "done",
        "failed",
        "aborted",
        "child",
    ]
}

pub(crate) fn create(app: &RunApplication, record: &GraphRunRecord) {
    assert!(record.parallel.is_none());
    assert!(record.graph_calls.is_empty());
    assert!(record.invocations.is_empty());
    assert!(record.passes.is_empty());
    assert!(record.module_activations.is_empty());
    assert!(record.ceased.is_empty());
    assert!(record.results.is_empty());
    assert!(record.decided.is_empty());
    assert_eq!(record.sequence, 0);
    app.kernel_runs(true)
        .unwrap()
        .unwrap()
        .create(record, u64::MAX)
        .unwrap();
}

pub(crate) struct Reader<'a> {
    app: &'a RunApplication,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(app: &'a RunApplication) -> Self {
        Self { app }
    }
    pub(crate) fn load(&self, id: &str) -> Result<Option<Observation>, ApplicationError> {
        Ok(self.app.read_run(id)?.map(|view| Observation {
            record: view.record.clone().into_owned(),
            phase: view.lifecycle.phase.clone(),
            view,
        }))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Observation {
    pub(crate) record: GraphRunRecord,
    pub(crate) phase: Phase,
    pub(crate) view: RunView<'static>,
}

impl std::ops::Deref for Observation {
    type Target = GraphRunRecord;
    fn deref(&self) -> &Self::Target {
        &self.record
    }
}

impl PartialEq for Observation {
    fn eq(&self, other: &Self) -> bool {
        self.record == other.record
            && self.phase == other.phase
            && self.recovery() == other.recovery()
            && self.view.active() == other.view.active()
            && self.view.updated() == other.view.updated()
    }
}

impl PartialEq<GraphRunRecord> for Observation {
    fn eq(&self, other: &GraphRunRecord) -> bool {
        self.record == *other
    }
}

impl Observation {
    pub(crate) fn recovery(&self) -> &[anchor_runtime::graph::PendingRecovery] {
        &self.view.recovery
    }
    pub(crate) fn has_succeeded(&self) -> bool {
        self.view.has_succeeded()
    }
    pub(crate) fn is_settled(&self) -> bool {
        self.view.is_settled()
    }
    pub(crate) fn has_stopped_execution(&self) -> bool {
        self.view.has_stopped_execution()
    }
    pub(crate) fn waits_for_children(&self) -> bool {
        self.view.waits_for_children()
    }
    pub(crate) fn active(&self) -> bool {
        self.view.active()
    }
    pub(crate) fn waits_reason(&self, reason: &str) -> bool {
        matches!(self.view.phase(), super::RunPhase::Waiting { reason: Some(current), .. } if current == reason)
    }
}

pub(crate) fn record(app: &RunApplication, id: &str) -> GraphRunRecord {
    app.load_run(id).unwrap().unwrap()
}

pub(crate) fn view(app: &RunApplication, id: &str) -> RunView<'static> {
    app.read_run(id).unwrap().unwrap()
}

pub(crate) fn control(app: &RunApplication, id: &str, operation: &str, reason: &str) {
    app.kernel_owner(id)
        .unwrap()
        .unwrap()
        .control(id, operation, Some(reason))
        .unwrap();
}

fn submit(app: &RunApplication, id: &str, kind: PrincipalKind, cost: u64, submission: Submission) {
    let owner = app.kernel_owner(id).unwrap().unwrap();
    let principal = Principal::new(
        if kind == PrincipalKind::Agent {
            EXECUTOR_ID
        } else {
            HOST_ID
        },
        kind,
    );
    let verdict = owner.kernel.submit(&principal, cost, submission).unwrap();
    assert!(
        verdict.is_accepted(),
        "fixture submission refused: {verdict:?}"
    );
}

pub(crate) fn complete(
    app: &RunApplication,
    id: &str,
    node: &str,
    completion: NodeCompletion,
    commit: Option<CommitRef>,
) -> CommitRef {
    submit_completion(app, id, node, completion, commit, None)
}

pub(crate) fn interrupted(
    app: &RunApplication,
    id: &str,
    node: &str,
    completion: NodeCompletion,
    commit: Option<CommitRef>,
    reason: &str,
) -> CommitRef {
    submit_completion(app, id, node, completion, commit, Some(reason.into()))
}

fn submit_completion(
    app: &RunApplication,
    id: &str,
    node: &str,
    completion: NodeCompletion,
    commit: Option<CommitRef>,
    interruption: Option<String>,
) -> CommitRef {
    let owner = app.kernel_owner(id).unwrap().unwrap();
    let resource = owner.load(id).unwrap().unwrap();
    let dispatch = GraphValidator::frontier(&resource)
        .unwrap()
        .into_iter()
        .find(|dispatch| dispatch.node.id == node)
        .expect("fixture node must be open");
    assert_ne!(
        dispatch.kind,
        DispatchKind::Call,
        "use a Call submission for a Call slot"
    );
    let record = KernelRuns::record(&resource).unwrap();
    let key = InvocationKey {
        run_id: id.into(),
        graph_digest: record.graph_digest,
        node_id: node.into(),
        invocation: dispatch.invocation,
    };
    let commit = commit.unwrap_or_else(|| CommitRef {
        id: format!("fixture-{}", crate::host_fs::invocation_stem(&key)),
        node_id: node.into(),
        invocation: dispatch.invocation,
    });
    let host_interruption = interruption.is_some();
    let cost = completion.model_requests;
    let content = NodeSubmission {
        summary: completion.submission,
        route: completion.route,
        output: completion.output,
        model_requests: cost,
        commit: commit.clone(),
        interruption,
        plugin_views: Default::default(),
        plugin_digests: Default::default(),
    };
    submit(
        app,
        id,
        if dispatch.kind == DispatchKind::Work && !host_interruption {
            PrincipalKind::Agent
        } else {
            PrincipalKind::Code
        },
        cost,
        Submission::new(&resource.id, resource.version, Action::Write)
            .slot(dispatch.slot())
            .content(serde_json::to_value(content).unwrap())
            .evidence(Evidence::Resource {
                id: commit.id.clone(),
                version: 0,
            }),
    );
    commit
}

pub(crate) fn completion(mut output: serde_json::Value) -> NodeCompletion {
    output
        .as_object_mut()
        .unwrap()
        .entry("files")
        .or_insert_with(|| json!([]));
    NodeCompletion {
        submission: "fixture result".into(),
        route: None,
        model_requests: 0,
        output,
    }
}

pub(crate) fn finish(app: &RunApplication, id: &str) {
    for _ in 0..100 {
        let owner = app.kernel_owner(id).unwrap().unwrap();
        let resource = owner.load(id).unwrap().unwrap();
        if resource.is_settled() {
            return;
        }
        assert_eq!(
            resource.phase(),
            Some(&Phase::Executing),
            "finish cannot release a waiting Run"
        );
        let dispatch = GraphValidator::frontier(&resource)
            .unwrap()
            .into_iter()
            .next()
            .expect("open fixture node");
        complete(app, id, &dispatch.node.id, completion(json!({})), None);
    }
    panic!("fixture finish exceeded 100 node results");
}

pub(crate) fn fail(app: &RunApplication, id: &str, node: &str, reason: &str) {
    let owner = app.kernel_owner(id).unwrap().unwrap();
    let resource = owner.load(id).unwrap().unwrap();
    let dispatch = GraphValidator::frontier(&resource)
        .unwrap()
        .into_iter()
        .find(|dispatch| dispatch.node.id == node)
        .expect("fixture failure node must be open");
    submit(
        app,
        id,
        PrincipalKind::Code,
        0,
        Submission::new(&resource.id, resource.version, Action::Settle)
            .slot(dispatch.slot())
            .content(
                serde_json::to_value(SettleSubmission {
                    reason: reason.into(),
                })
                .unwrap(),
            ),
    );
}

pub(crate) fn recovery(
    app: &RunApplication,
    id: &str,
    node: &str,
    attempts: Vec<RecoveryAttempt>,
) -> InvocationKey {
    let owner = app.kernel_owner(id).unwrap().unwrap();
    let resource = owner.load(id).unwrap().unwrap();
    let record = KernelRuns::record(&resource).unwrap();
    let dispatch = GraphValidator::frontier(&resource)
        .unwrap()
        .into_iter()
        .find(|dispatch| dispatch.node.id == node)
        .expect("fixture recovery node must be open");
    let key = InvocationKey {
        run_id: id.into(),
        graph_digest: record.graph_digest,
        node_id: node.into(),
        invocation: dispatch.invocation,
    };
    submit(app, id, PrincipalKind::Code, 0,
        Submission::new(&resource.id, resource.version, Action::Request)
            .content(json!({"control":"stop","reason":"unknown external tool effects","invocation":key,"attempts":attempts}))
            .evidence(Evidence::Observation { id: format!("fixture/recovery/{}", key.durable_key()) }));
    key
}

pub(crate) fn call(app: &RunApplication, id: &str, node: &str, content: CallSubmission) {
    let owner = app.kernel_owner(id).unwrap().unwrap();
    let resource = owner.load(id).unwrap().unwrap();
    let dispatch = GraphValidator::frontier(&resource)
        .unwrap()
        .into_iter()
        .find(|dispatch| dispatch.node.id == node && dispatch.kind == DispatchKind::Call)
        .expect("fixture Call node must be open");
    submit(
        app,
        id,
        PrincipalKind::Code,
        0,
        Submission::new(&resource.id, resource.version, Action::Write)
            .slot(dispatch.slot())
            .content(serde_json::to_value(content).unwrap()),
    );
}

pub(crate) fn bind_child(app: &RunApplication, id: &str, node: &str, child_id: &str) {
    call(
        app,
        id,
        node,
        CallSubmission::Bound {
            child_run_id: child_id.into(),
        },
    );
}
