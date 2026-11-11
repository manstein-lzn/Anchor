use super::*;
use crate::application::scheduling::{GraphScheduleReference, ScheduleManagementPort};
use anchor_platform_session::{ChannelIdentity, ChannelInboundRequest, TaskResult};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Plans(Mutex<Vec<GraphScheduleReference>>);

impl ScheduleManagementPort for Plans {
    fn list(&self) -> Result<Value, ApplicationError> {
        unreachable!()
    }
    fn create(&self, _: &str, _: &Value, _: &Value) -> Result<Value, ApplicationError> {
        unreachable!()
    }
    fn update(
        &self,
        _: &str,
        _: &str,
        _: &Value,
        _: &Value,
        _: bool,
    ) -> Result<Value, ApplicationError> {
        unreachable!()
    }
    fn delete(&self, _: &str) -> Result<Value, ApplicationError> {
        unreachable!()
    }
    fn graph_references(&self) -> Result<Vec<GraphScheduleReference>, ApplicationError> {
        Ok(self.0.lock().unwrap().clone())
    }
}

struct Task {
    _root: tempfile::TempDir,
    app: RunApplication,
    plans: Arc<Plans>,
    sessions: SessionStore,
    session: String,
    origin: TaskOrigin,
    graph: String,
}

impl Task {
    async fn new() -> Self {
        let (root, app) = fixture();
        let plans = Arc::new(Plans::default());
        let app = app.with_schedule_management(plans.clone());
        crate::host_fs::directory(&app.data_root.join("platform")).unwrap();
        let sessions = crate::assistant::sessions(&app.data_root).unwrap();
        let admitted = sessions
            .admit_channel_inbound(
                "owner",
                ChannelInboundRequest {
                    inbound_id: "user-request".into(),
                    identity: ChannelIdentity {
                        source: "feishu".into(),
                        account: Some("bot".into()),
                        conversation_kind: Default::default(),
                        conversation_id: "chat".into(),
                        sender_id: "alice".into(),
                    },
                    graph: "source".into(),
                    reply_node: "work".into(),
                    text: Some("remind me".into()),
                    attachments: Default::default(),
                    run_id: None,
                    replace_running: false,
                },
            )
            .unwrap();
        let session = admitted.session.id;
        sessions
            .bind_channel_assistant("owner", &session, "assistant", "wait", "work", "reply")
            .unwrap();
        let user = sessions
            .claim_assistant_input("owner", &session, "assistant", "user")
            .unwrap()
            .unwrap();
        sessions
            .finish_turn(
                "owner",
                &session,
                &user.turn().id,
                TurnStatus::Completed,
                None,
            )
            .unwrap();
        let origin = approved(
            &app,
            GraphTaskPolicy {
                create_graphs: true,
                commands: true,
                ..Default::default()
            },
            Some(&session),
        )
        .await;
        let created = app
            .create_task_graph(&origin, command(false), "create-reminder".into())
            .await
            .unwrap();
        let graph = created["graph"].as_str().unwrap().to_owned();
        plans.0.lock().unwrap().push(GraphScheduleReference {
            graph: graph.clone(),
            request_id: "occurrence".into(),
            once: true,
            enabled: false,
        });
        Self {
            _root: root,
            app,
            plans,
            sessions,
            session,
            origin,
            graph,
        }
    }

    fn admit(&self, id: &str, request: &str) {
        let (_, bundle) = self.app.load_graph(&self.graph).unwrap();
        let record = GraphRunRecord::create_with_id(bundle.snapshot, json!({}), id).unwrap();
        fixtures::create(&self.app, &record);
        let mut metadata = RunMetadata::new(
            id.into(),
            self.graph.clone(),
            record.graph_digest,
            &self.app.graph_bundle_path(&self.graph),
        )
        .unwrap();
        metadata.task = Some(self.origin.clone());
        metadata.request = Some(crate::application::run_access::admission_request(
            request.into(),
            &self.graph,
            &json!({}),
            &None,
        ));
        metadata::save(&self.app.data_root, &metadata).unwrap();
    }

    fn queue(&self, run: &str) {
        self.sessions
            .record_task_result(
                "owner",
                &self.session,
                &TaskResult {
                    id: format!("task-{:x}", Sha256::digest(run.as_bytes())),
                    source_run: Some(run.into()),
                    outcome: "done".into(),
                    content: json!({"event":"run.completed"}),
                    files: vec![],
                },
            )
            .unwrap();
    }

    fn handle(&self, key: &str) {
        self.sessions
            .claim_assistant_input("owner", &self.session, "assistant", key)
            .unwrap()
            .unwrap();
        self.sessions
            .finish_task_result("owner", &self.session, "assistant", key)
            .unwrap();
    }

    async fn collected(&self) -> bool {
        let listing = self.app.list_graphs().await.unwrap();
        listing["graphs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|graph| graph["graph"] == self.graph)
            .unwrap()["completed_task"]
            == true
    }

    async fn finish(&self) {
        self.admit("task-run", "occurrence");
        fixtures::finish(&self.app, "task-run");
        self.queue("task-run");
        self.handle("result");
        assert!(self.collected().await);
    }
}

#[tokio::test]
async fn completed_once_requires_admission_success_and_handled_result_without_deleting_history() {
    let task = Task::new().await;
    assert!(!task.collected().await, "paused before admission");
    task.admit("task-run", "occurrence");
    assert!(!task.collected().await, "still executing");
    fixtures::finish(&task.app, "task-run");
    assert!(!task.collected().await, "not queued");
    task.queue("task-run");
    assert!(!task.collected().await, "not handled");
    task.sessions
        .claim_assistant_input("owner", &task.session, "assistant", "result")
        .unwrap()
        .unwrap();
    assert!(!task.collected().await, "only claimed");
    let before = task.app.read_graph(&task.graph).unwrap();
    task.sessions
        .finish_task_result("owner", &task.session, "assistant", "result")
        .unwrap();
    assert!(task.collected().await);
    assert_eq!(task.app.read_graph(&task.graph).unwrap(), before);
    assert!(task.app.read_run("task-run").unwrap().unwrap().is_settled());
    assert!(task.app.metadata("task-run").unwrap().is_some());
    assert!(
        task.sessions
            .pending_task_results("owner", &task.session)
            .unwrap()
            .is_empty()
    );
    let expected = task.app.list_graphs().await.unwrap();
    let data_root = task.app.data_root.clone();
    let catalog_root = task.app.catalog_root.clone();
    drop(task.app);
    let reopened =
        RunApplication::new(data_root, catalog_root).with_schedule_management(task.plans.clone());
    assert_eq!(reopened.list_graphs().await.unwrap(), expected);
}

#[tokio::test]
async fn completed_once_reappears_for_new_plans_runs_recurrence_or_unconfirmed_occurrences() {
    let task = Task::new().await;
    task.finish().await;
    {
        task.plans.0.lock().unwrap()[0].enabled = true;
    }
    assert!(!task.collected().await);
    task.plans.0.lock().unwrap()[0].enabled = false;
    assert!(task.collected().await);
    task.plans.0.lock().unwrap()[0].once = false;
    assert!(
        !task.collected().await,
        "paused or exhausted recurrence is reusable"
    );
    task.plans.0.lock().unwrap()[0].once = true;
    task.plans.0.lock().unwrap()[0].request_id = "not-admitted".into();
    assert!(
        !task.collected().await,
        "old successful Run cannot stand in for another occurrence"
    );
    task.plans.0.lock().unwrap()[0].request_id = "occurrence".into();
    task.admit("new-run", "new-request");
    assert!(!task.collected().await);
    fixtures::finish(&task.app, "new-run");
    task.queue("new-run");
    assert!(!task.collected().await);
    task.handle("new-result");
    assert!(task.collected().await);
}

#[tokio::test]
async fn manual_shared_edited_or_unverifiable_tasks_remain_in_the_main_catalog() {
    let task = Task::new().await;
    task.finish().await;
    write(&task.app, "shared", &call(&task.graph));
    assert!(!task.collected().await, "static caller");
    std::fs::remove_dir_all(task.app.graph_bundle_path("shared")).unwrap();
    assert!(task.collected().await);
    let mut policy = task.origin.policy.clone();
    policy.graphs.push(task.graph.clone());
    task.app
        .update_graph("source", source(&policy))
        .await
        .unwrap();
    assert!(
        !task.collected().await,
        "explicitly selected reusable target"
    );
    task.app
        .update_graph("source", source(&task.origin.policy))
        .await
        .unwrap();
    assert!(task.collected().await);
    let owned_path = task_graph_ownership_path(&task.app, &task.graph).unwrap();
    let owned = std::fs::read(&owned_path).unwrap();
    std::fs::remove_file(&owned_path).unwrap();
    assert!(!task.collected().await, "a task-like name is not ownership");
    std::fs::write(&owned_path, b"invalid").unwrap();
    assert!(!task.collected().await, "unknown ownership stays visible");
    std::fs::write(&owned_path, owned).unwrap();
    assert!(task.collected().await);
    let mut changed = command(false);
    changed["objective"] = json!("new task draft");
    write(&task.app, &task.graph, &changed);
    assert!(!task.collected().await, "unexecuted edits");
}

#[tokio::test]
async fn failed_once_tasks_are_not_hidden_as_successful_completions() {
    let task = Task::new().await;
    task.admit("task-run", "occurrence");
    fixtures::fail(&task.app, "task-run", "work", "fixture failure");
    task.queue("task-run");
    task.handle("result");
    assert!(!task.collected().await);
}
