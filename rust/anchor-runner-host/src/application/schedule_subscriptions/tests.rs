use super::*;
use crate::application::{
    channel_management::fake::FakeChannels,
    graph_tasks::assignment_tests::prepared,
    run_access::fixtures,
    scheduling::{ScheduleAssignmentTarget, ScheduleManagementPort},
};
use crate::artifacts::ArtifactAvailability;
use crate::run_metadata::{self as metadata, AssistantSource, RunTrigger};
use anchor_graph_host::PluginCatalog;
use anchor_platform_session::{
    AssistantInput, ChannelIdentity, ChannelInboundRequest, CreateSession, SessionStore, TurnStatus,
};
use anchor_runtime::graph::{ArtifactPort, CommitRef, GraphRunRecord, InvocationKey};
use std::{path::Path, sync::Arc};

const PLAN: &str = "shared-plan";
const INSTRUCTION: &str = "Send the published report to this conversation";

#[tokio::test]
async fn graph_relations_preserve_subscription_state_without_exposing_private_delivery_data() {
    let fixture = Fixture::new().await;
    fixture
        .share(std::slice::from_ref(&fixture.alice), true)
        .await;
    let subscription = fixture.subscribe(&fixture.alice, Some("09:05")).await;
    let app = &fixture.application;
    let mut expected = json!([{
        "graph":"assistant","target":"producer","schedule":PLAN,
        "active":true,"authorized":true,"not_before":"09:05"
    }]);
    let projection = app.graph_relations("local").await.unwrap();
    assert_eq!(projection["subscriptions"], expected);
    assert_eq!(
        app.graph_relations("another-owner").await.unwrap()["subscriptions"],
        json!([])
    );
    for private in [
        fixture.alice.as_str(),
        fixture.bob.as_str(),
        INSTRUCTION,
        "producer-input-marker",
        "producer-origin-marker",
        "authorization_digest",
    ] {
        assert!(!projection.to_string().contains(private), "{private}");
    }
    fixture
        .share(std::slice::from_ref(&fixture.alice), false)
        .await;
    expected[0]["authorized"] = json!(false);
    assert_eq!(
        app.graph_relations("local").await.unwrap()["subscriptions"],
        expected
    );
    app.operator_unsubscribe_schedule("local", &subscription.id)
        .await
        .unwrap();
    expected[0]["active"] = json!(false);
    assert_eq!(
        app.graph_relations("local").await.unwrap()["subscriptions"],
        expected
    );
    app.put_subscription_fact("recipients/corrupt", json!({"unknown":true}))
        .unwrap();
    assert!(matches!(
        app.graph_relations("local").await,
        Err(crate::application::graphs::GraphManagementError::Storage(_))
    ));
}

struct Schedules(ScheduleAssignmentTarget);

impl ScheduleManagementPort for Schedules {
    fn list(&self) -> Result<Value, ApplicationError> {
        Ok(json!({"schedules":[{
            "id":PLAN,"graph":self.0.graph,"input":self.0.input,
            "delegation":{"private":"producer-origin-marker"},
            "rule":{"type":"daily","time":"07:30"},
            "next_at":"2099-01-01T07:30:00","enabled":true
        },{"id":"private-plan","input":{"private":"unshared-input-marker"}}]}))
    }

    fn assignment_target(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<ScheduleAssignmentTarget, ApplicationError> {
        if owner == "local" && id == PLAN {
            Ok(self.0.clone())
        } else {
            Err(ApplicationError::Missing)
        }
    }

    fn list_tasks(&self, _origin: &TaskOrigin) -> Result<Value, ApplicationError> {
        Ok(json!({"schedules":[]}))
    }

    fn create(
        &self,
        _graph: &str,
        _rule: &Value,
        _input: &Value,
    ) -> Result<Value, ApplicationError> {
        panic!("subscriptions must not create production schedules")
    }

    fn update(
        &self,
        _id: &str,
        _graph: &str,
        _rule: &Value,
        _input: &Value,
        _enabled: bool,
    ) -> Result<Value, ApplicationError> {
        panic!("subscriptions must not change production schedules")
    }

    fn delete(&self, _id: &str) -> Result<Value, ApplicationError> {
        panic!("subscriptions must not delete production schedules")
    }
}

struct Fixture {
    root: tempfile::TempDir,
    application: RunApplication,
    channels: Arc<FakeChannels>,
    schedules: Arc<Schedules>,
    sessions: SessionStore,
    alice: String,
    bob: String,
}

impl Fixture {
    async fn new() -> Self {
        let (root, application, channels, sessions, alice, mut target) = prepared().await;
        application
            .update_graph(
                "producer",
                json!({
                    "objective":"shared output fixture","entry":"research",
                    "ops":{"work":{"run":"true"}},
                    "nodes":[{"id":"research","op":"work"},{"id":"publish","op":"work"}],
                    "edges":[{"from":"research","to":"publish"}]
                }),
            )
            .await
            .unwrap();
        let mut definition = application
            .load_graph("assistant")
            .unwrap()
            .1
            .authoring_definition;
        definition["nodes"][1]["plugins"][1]["graph_tasks"] =
            json!({"schedules":[PLAN,"private-plan"]});
        application
            .update_graph("assistant", definition)
            .await
            .unwrap();
        channels.update("assistant", |bot| {
            bot.binding.users.push("bob".into());
            bot.binding.send_users.push("bob".into());
        });
        let admitted = sessions
            .admit_channel_inbound(
                "local",
                ChannelInboundRequest {
                    inbound_id: "initial-bob".into(),
                    identity: ChannelIdentity {
                        source: "wecom".into(),
                        account: Some("corp-a".into()),
                        conversation_kind: ChannelConversationKind::Single,
                        conversation_id: "bob-conversation".into(),
                        sender_id: "bob".into(),
                    },
                    graph: "assistant".into(),
                    reply_node: "work".into(),
                    text: Some("receive shared reports".into()),
                    attachments: Default::default(),
                    run_id: None,
                    replace_running: false,
                    bot_origin: None,
                    platform_message_id: None,
                },
            )
            .unwrap();
        let bob = admitted.session.id;
        sessions
            .finish_turn(
                "local",
                &bob,
                &admitted.turn.id,
                TurnStatus::Completed,
                None,
            )
            .unwrap();
        let (_, bundle) = application.load_graph("assistant").unwrap();
        let record =
            GraphRunRecord::create_with_id(bundle.snapshot, json!({}), "bob-assistant").unwrap();
        fixtures::create(&application, &record);
        fixtures::control(&application, &record.run_id, "stop", "fixture stopped");
        let mut metadata = RunMetadata::new(
            record.run_id.clone(),
            "assistant".into(),
            record.graph_digest,
            &application.graph_bundle_path("assistant"),
        )
        .unwrap();
        metadata.assistant = Some(AssistantSource {
            owner: "local".into(),
            session: bob.clone(),
            wait_node: "wait".into(),
            work_node: "work".into(),
            reply_node: "reply".into(),
        });
        metadata::save(&application.data_root, &metadata).unwrap();
        sessions
            .bind_channel_assistant("local", &bob, &record.run_id, "wait", "work", "reply")
            .unwrap();
        target.input["private"] = json!("producer-input-marker");
        let schedules = Arc::new(Schedules(target));
        let application = application.with_schedule_management(schedules.clone());
        Self {
            root,
            application,
            channels,
            schedules,
            sessions,
            alice,
            bob,
        }
    }

    fn origin(&self, session: &str) -> TaskOrigin {
        self.application
            .operator_subscription_origin("local", session, PLAN, INSTRUCTION)
            .unwrap()
    }

    async fn share(&self, sessions: &[String], enabled: bool) {
        self.application
            .share_schedule("local", PLAN, "Shared final report", sessions, enabled)
            .await
            .unwrap();
    }

    async fn subscribe(&self, session: &str, not_before: Option<&str>) -> ScheduleSubscription {
        let result = self
            .application
            .subscribe_task_schedule(&self.origin(session), PLAN, INSTRUCTION, not_before)
            .await
            .unwrap();
        self.application
            .subscription(result["subscription"]["id"].as_str().unwrap())
            .unwrap()
    }

    fn past_boundary(&self, subscription: &mut ScheduleSubscription) {
        // Freeze fixture time without sleeping or weakening production clock ownership.
        subscription.effective_at = "2020-01-01T00:00:00".into();
        self.application.store_subscription(subscription).unwrap();
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.root.path().join("workspaces")
    }

    fn producer(&self, id: &str, at: &str) -> GraphRunRecord {
        let (path, bundle) = self.application.load_graph("producer").unwrap();
        let mut record =
            GraphRunRecord::create_with_id(bundle.snapshot, self.schedules.0.input.clone(), id)
                .unwrap();
        record.plugin_bindings =
            crate::plugin_library::Catalog::new(self.application.library_root())
                .resolve_snapshot(&record.snapshot)
                .unwrap()
                .into_iter()
                .map(|binding| (binding.id.clone(), binding))
                .collect();
        record.plugin_bindings_initialized = true;
        let mut metadata = RunMetadata::new(
            id.into(),
            "producer".into(),
            record.graph_digest.clone(),
            &path,
        )
        .unwrap();
        metadata.oauth_owner = Some(crate::credential_store::binding_owner("local"));
        metadata.request = Some(super::super::run_access::admission_request(
            format!("request-{id}"),
            "producer",
            &record.input,
            &None,
        ));
        trigger(at).apply(&mut metadata);
        metadata.schedule_publication = self
            .application
            .schedule_publication_at_admission(&metadata)
            .unwrap();
        fixtures::create(&self.application, &record);
        metadata::save(&self.application.data_root, &metadata).unwrap();
        record
    }

    async fn finish(&self, record: &GraphRunRecord) -> CommitRef {
        let artifacts = crate::HostArtifacts::new(
            self.application.data_root.join("artifacts"),
            self.workspace(),
        );
        let mut final_commit = None;
        for (node, name, text) in [
            ("research", "notes.md", "internal-research-marker"),
            ("publish", "report.md", "approved final report"),
        ] {
            let key = InvocationKey {
                run_id: record.run_id.clone(),
                graph_digest: record.graph_digest.clone(),
                node_id: node.into(),
                invocation: 1,
            };
            let directory = artifacts.workspace_path(&key).unwrap();
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(name), text).unwrap();
            std::fs::write(directory.join("unselected.txt"), "unselected-marker").unwrap();
            let mut completion = fixtures::completion(json!({"files":[name]}));
            completion.submission = text.into();
            let commit = artifacts.freeze(&key, &completion).await.unwrap();
            fixtures::complete(
                &self.application,
                &record.run_id,
                node,
                completion,
                Some(commit.clone()),
            );
            final_commit = Some(commit);
        }
        final_commit.unwrap()
    }
}

fn trigger(at: &str) -> RunTrigger {
    serde_json::from_value(json!({"source":"schedule","schedule":PLAN,"scheduled_at":at})).unwrap()
}

fn assert_available(application: &RunApplication, workspace: &Path, commit: &CommitRef) {
    let artifacts =
        crate::HostArtifacts::new(application.data_root.join("artifacts"), workspace.into());
    assert_eq!(
        artifacts.availability(commit).unwrap(),
        ArtifactAvailability::Available
    );
}

#[tokio::test]
async fn discovery_requires_explicit_audience_and_consumption_grant_without_exposing_producer_data()
{
    let fixture = Fixture::new().await;
    let app = &fixture.application;
    let alice = fixture.origin(&fixture.alice);
    assert_eq!(
        app.discover_task_schedules(&alice).unwrap()["schedules"],
        json!([])
    );
    assert!(
        app.share_schedule("local", PLAN, "Shared report", &[], true)
            .await
            .is_err()
    );
    let generic = fixture
        .sessions
        .create("local", CreateSession::default())
        .unwrap();
    assert!(
        app.share_schedule("local", PLAN, "Shared report", &[generic.id], true)
            .await
            .is_err()
    );
    assert!(
        app.share_schedule(
            "other-owner",
            PLAN,
            "Shared report",
            std::slice::from_ref(&fixture.alice),
            true
        )
        .await
        .is_err()
    );
    fixture
        .share(std::slice::from_ref(&fixture.alice), true)
        .await;
    let visible = app.discover_task_schedules(&alice).unwrap();
    assert_eq!(visible["schedules"].as_array().unwrap().len(), 1);
    assert_eq!(visible["schedules"][0]["id"], PLAN);
    for private in [
        "producer-input-marker",
        "producer-origin-marker",
        "unshared-input-marker",
        "delegation",
        "input",
    ] {
        assert!(!visible.to_string().contains(private), "{private}");
    }
    assert_eq!(
        app.list_task_schedules(&alice).unwrap()["schedules"],
        json!([])
    );
    assert_eq!(
        app.discover_task_schedules(&fixture.origin(&fixture.bob))
            .unwrap()["schedules"],
        json!([])
    );
    assert!(
        app.subscribe_task_schedule(&fixture.origin(&fixture.bob), PLAN, INSTRUCTION, None)
            .await
            .is_err()
    );
    assert!(alice.policy.graphs.is_empty());
    assert!(alice.targets.is_empty());
    assert!(app.authorize_delegated_graph(&alice, "producer").is_err());
    let mut execution_only = alice.clone();
    execution_only.policy.schedules.clear();
    execution_only.policy.graphs.push("producer".into());
    assert_eq!(
        app.discover_task_schedules(&execution_only).unwrap()["schedules"],
        json!([])
    );
    assert!(
        app.subscribe_task_schedule(&execution_only, PLAN, INSTRUCTION, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn subscription_retries_and_unsubscribe_never_create_or_change_production() {
    let fixture = Fixture::new().await;
    fixture
        .share(&[fixture.alice.clone(), fixture.bob.clone()], true)
        .await;
    let app = &fixture.application;
    let origin = fixture.origin(&fixture.alice);
    let before_runs = app.records().unwrap().len();
    let before_schedules = app.list_schedules().unwrap();
    let first = app
        .subscribe_task_schedule(&origin, PLAN, INSTRUCTION, Some("09:05"))
        .await
        .unwrap();
    assert_eq!(
        app.subscribe_task_schedule(&origin, PLAN, INSTRUCTION, Some("09:05"))
            .await
            .unwrap(),
        first
    );
    assert!(
        app.subscribe_task_schedule(&origin, PLAN, "different instruction", Some("09:05"))
            .await
            .is_err()
    );
    assert!(
        app.subscribe_task_schedule(&origin, PLAN, INSTRUCTION, Some("9:05"))
            .await
            .is_err()
    );
    assert_eq!(
        app.list_task_subscriptions(&fixture.origin(&fixture.bob))
            .unwrap()["subscriptions"],
        json!([])
    );
    let id = first["subscription"]["id"].as_str().unwrap();
    assert!(
        app.unsubscribe_task_schedule(&fixture.origin(&fixture.bob), id)
            .await
            .is_err()
    );
    assert!(matches!(
        app.delete_task_session("local", &fixture.alice).await,
        Err(ApplicationError::Conflict(reason)) if reason.contains("unsubscribe")
    ));
    let bot = fixture.channels.get("assistant").unwrap();
    fixture.channels.update("assistant", |state| {
        state.enabled = false;
        state.saved_enabled = false;
    });
    assert!(matches!(
        app.remove_graph("assistant", &fixture.workspace()).await,
        Err(crate::application::graphs::GraphManagementError::Conflict(reason))
            if reason.contains("unsubscribe")
    ));
    fixture.channels.put(bot);
    assert_eq!(
        app.unsubscribe_task_schedule(&origin, id).await.unwrap()["active"],
        false
    );
    assert_eq!(
        app.unsubscribe_task_schedule(&origin, id).await.unwrap()["active"],
        false
    );
    assert!(
        app.check_task_origin_deletion(&|candidate| Ok(candidate.same_scope(&origin)))
            .is_ok()
    );
    assert_eq!(app.list_schedules().unwrap(), before_schedules);
    assert_eq!(app.records().unwrap().len(), before_runs);
    assert!(app.active_runs(None).await.is_empty());
}

#[tokio::test]
async fn ordinary_schedule_admission_pins_publication_without_task_origin_or_duplicate_run() {
    let _environment_lock = crate::PROCESS_ENV_LOCK.lock().await;
    let fixture = Fixture::new().await;
    let mut environment = crate::test_support::idle_assistant_environment(fixture.root.path());
    environment.set("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh");
    fixture
        .share(std::slice::from_ref(&fixture.alice), true)
        .await;
    let app = &fixture.application;
    let revision = app.sharing(PLAN).unwrap().unwrap().revision;
    let before = app.records().unwrap().len();
    let input = fixture.schedules.0.input.clone();
    let run = app
        .admit_graph_request(
            "producer",
            input.clone(),
            None,
            trigger("2020-01-02T07:30:00"),
            Some(crate::credential_store::binding_owner("local")),
            Some("scheduled-occurrence".into()),
        )
        .await
        .unwrap();
    // Stop before any production work: this test exercises admission, not a Provider.
    if !app.read_run(&run).unwrap().unwrap().is_settled() {
        fixtures::control(app, &run, "abandon", "admission fixture only");
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while app.read_run(&run).unwrap().unwrap().active() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let admitted = app.metadata(&run).unwrap().unwrap();
    assert!(admitted.task.is_none());
    assert_eq!(admitted.schedule.as_deref(), Some(PLAN));
    assert_eq!(
        admitted.schedule_publication.as_deref(),
        Some(revision.as_str())
    );
    fixture
        .share(std::slice::from_ref(&fixture.alice), false)
        .await;
    let replay = app
        .admit_graph_request(
            "producer",
            input,
            None,
            trigger("2020-01-02T07:30:00"),
            Some(crate::credential_store::binding_owner("local")),
            Some("scheduled-occurrence".into()),
        )
        .await
        .unwrap();
    assert_eq!(replay, run);
    assert_eq!(app.records().unwrap().len(), before + 1);
    assert_eq!(
        app.metadata(&run).unwrap().unwrap().schedule_publication,
        admitted.schedule_publication
    );
}

#[tokio::test]
async fn graph_and_plugin_updates_preserve_subscriptions_and_report_impact() {
    let fixture = Fixture::new().await;
    let app = &fixture.application;
    let mut definition = app.load_graph("producer").unwrap().1.authoring_definition;
    definition["nodes"][0]["plugins"] = json!([{"id":"network","network":"enabled"}]);
    app.update_graph("producer", definition).await.unwrap();
    fixture
        .share(std::slice::from_ref(&fixture.alice), true)
        .await;
    let share = app.sharing(PLAN).unwrap().unwrap();
    let mut subscription = fixture.subscribe(&fixture.alice, None).await;
    fixture.past_boundary(&mut subscription);

    let mut updated = app.load_graph("producer").unwrap().1.authoring_definition;
    updated["objective"] = json!("updated shared producer objective");
    let saved = app.update_graph("producer", updated).await.unwrap();
    assert_eq!(saved["plugin_activation"]["shared_schedules"], 1);
    assert_eq!(saved["plugin_activation"]["active_subscriptions"], 1);

    let plugin = app.library_root().join("plugins/network");
    std::fs::write(
        plugin.join("bin/anchor-network-tools"),
        b"#!/bin/sh\n# next platform release\nexit 1\n",
    )
    .unwrap();
    let impact = app.plugin_impact("network").unwrap();
    assert_eq!(impact.shared_schedules, 1);
    assert_eq!(impact.active_subscriptions, 1);
    let upgraded = crate::plugin_library::Catalog::new(app.library_root())
        .resolve(&["network".into()])
        .unwrap()
        .remove(0);
    assert!(app.subscription_authorized(&subscription).unwrap());
    assert_eq!(app.sharing(PLAN).unwrap().unwrap(), share);

    let record = fixture.producer("upgraded-runtime", "2020-01-02T07:30:00");
    assert_eq!(
        app.metadata(&record.run_id)
            .unwrap()
            .unwrap()
            .schedule_publication,
        Some(share.revision)
    );
    assert_eq!(record.plugin_bindings["network"], upgraded);
    let commit = fixture.finish(&record).await;
    for expected in [1, 0] {
        assert_eq!(
            app.reconcile_task_results(&fixture.workspace())
                .await
                .unwrap(),
            expected
        );
    }
    let results = fixture
        .sessions
        .pending_task_results("local", &fixture.alice)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].files[0].artifact_id, commit.id);
    app.authorize_subscription_result("local", &fixture.alice, &results[0])
        .unwrap();
}

#[tokio::test]
async fn only_future_occurrences_with_the_admitted_sharing_revision_are_delivered() {
    let fixture = Fixture::new().await;
    fixture
        .share(std::slice::from_ref(&fixture.alice), true)
        .await;
    let subscription = fixture.subscribe(&fixture.alice, None).await;
    let effective =
        NaiveDateTime::parse_from_str(&subscription.effective_at, "%Y-%m-%dT%H:%M:%S%.f").unwrap();
    for (id, at, expected) in [
        ("past", effective - chrono::Duration::seconds(1), 0),
        ("current", effective, 0),
        ("future", effective + chrono::Duration::seconds(1), 1),
    ] {
        let record = fixture.producer(id, &at.format("%Y-%m-%dT%H:%M:%S%.f").to_string());
        fixture.finish(&record).await;
        assert_eq!(
            fixture
                .application
                .deliver_subscription_results(id, &fixture.workspace())
                .await
                .unwrap(),
            expected,
            "{id}"
        );
    }
    let record = fixture.producer(
        "before-sharing",
        &(effective + chrono::Duration::seconds(2))
            .format("%Y-%m-%dT%H:%M:%S%.f")
            .to_string(),
    );
    let mut old = fixture
        .application
        .metadata(&record.run_id)
        .unwrap()
        .unwrap();
    old.schedule_publication = None;
    metadata::save(&fixture.application.data_root, &old).unwrap();
    fixture.finish(&record).await;
    assert_eq!(
        fixture
            .application
            .deliver_subscription_results(&record.run_id, &fixture.workspace())
            .await
            .unwrap(),
        0
    );
    let pending = fixture
        .sessions
        .pending_task_results("local", &fixture.alice)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_run.as_deref(), Some("future"));
    assert!(
        fixture
            .application
            .task_run_status(&fixture.origin(&fixture.alice), "future")
            .is_err()
    );
}

#[tokio::test]
async fn partial_fanout_recovers_after_restart_and_sessions_handle_one_final_artifact_independently()
 {
    let fixture = Fixture::new().await;
    fixture
        .share(&[fixture.alice.clone(), fixture.bob.clone()], true)
        .await;
    for session in [&fixture.alice, &fixture.bob] {
        let mut subscription = fixture.subscribe(session, None).await;
        fixture.past_boundary(&mut subscription);
    }
    let record = fixture.producer("one-production", "2020-01-02T07:30:00");
    let commit = fixture.finish(&record).await;
    let workspace = fixture.workspace();
    assert!(
        fixture
            .application
            .reject_pending_task_result(&record.run_id)
            .is_err()
    );
    fixture
        .application
        .release_run_deliveries(&record.run_id, &workspace)
        .unwrap();
    assert_available(&fixture.application, &workspace, &commit);
    let database = rusqlite::Connection::open(
        fixture
            .application
            .data_root
            .join("platform/sessions.sqlite"),
    )
    .unwrap();
    // Exhaust only Bob's event sequence without altering the validated schema.
    database
        .execute(
            "INSERT INTO session_events(session_id, seq, at, kind, data) VALUES (?1, ?2, '2020-01-01T00:00:00Z', 'fixture.exhausted', '{}')",
            rusqlite::params![fixture.bob, i64::MAX],
        )
        .unwrap();
    let failed = fixture
        .application
        .reconcile_task_results(&workspace)
        .await
        .unwrap_err();
    assert!(
        format!("{failed:?}").contains("event sequence exhausted"),
        "{failed:?}"
    );
    let first = fixture
        .sessions
        .pending_task_results("local", &fixture.alice)
        .unwrap();
    assert_eq!(first.len(), 1);
    assert!(
        fixture
            .sessions
            .pending_task_results("local", &fixture.bob)
            .unwrap()
            .is_empty()
    );
    database
        .execute(
            "DELETE FROM session_events WHERE session_id = ?1 AND kind = 'fixture.exhausted'",
            [&fixture.bob],
        )
        .unwrap();
    drop(database);
    let data_root = fixture.application.data_root.clone();
    drop(fixture.application);
    let restarted = RunApplication::new(data_root, fixture.root.path().join("catalog"))
        .with_channel_management(fixture.channels.clone())
        .with_schedule_management(fixture.schedules.clone());
    assert_eq!(
        restarted.reconcile_task_results(&workspace).await.unwrap(),
        1
    );
    assert_eq!(
        restarted.reconcile_task_results(&workspace).await.unwrap(),
        0
    );
    let second = fixture
        .sessions
        .pending_task_results("local", &fixture.bob)
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].id, second[0].id);
    assert_eq!(first[0].source_run, second[0].source_run);
    assert_eq!(first[0].files, second[0].files);
    assert_eq!(first[0].files.len(), 1);
    assert_eq!(first[0].files[0].artifact_id, commit.id);
    assert_eq!(first[0].files[0].path, "report.md");
    assert_eq!(first[0].content["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(first[0].content["nodes"][0]["node"], "publish");
    for private in [
        "internal-research-marker",
        "producer-input-marker",
        "unselected-marker",
        "graph_tasks_status",
    ] {
        assert!(
            !serde_json::to_string(&first[0]).unwrap().contains(private),
            "{private}"
        );
    }
    assert!(
        restarted
            .authorize_subscription_result("local", &fixture.alice, &first[0])
            .is_ok()
    );
    assert!(
        restarted
            .authorize_subscription_result("local", &fixture.bob, &first[0])
            .is_err()
    );
    assert!(matches!(
        fixture
            .sessions
            .claim_assistant_input("local", &fixture.alice, "existing-assistant", "result")
            .unwrap(),
        Some(AssistantInput::Task(_))
    ));
    fixture
        .sessions
        .finish_task_result("local", &fixture.alice, "existing-assistant", "result")
        .unwrap();
    assert!(
        fixture
            .sessions
            .pending_task_results("local", &fixture.alice)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .sessions
            .pending_task_results("local", &fixture.bob)
            .unwrap(),
        second
    );
    assert!(
        restarted
            .reject_pending_task_result(&record.run_id)
            .is_err()
    );
    assert_eq!(restarted.records().unwrap().len(), 3);
    assert_eq!(
        restarted.reconcile_task_results(&workspace).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn not_before_preserves_undelivered_artifacts_without_changing_production() {
    let fixture = Fixture::new().await;
    fixture
        .share(std::slice::from_ref(&fixture.alice), true)
        .await;
    let mut subscription = fixture.subscribe(&fixture.alice, Some("09:05")).await;
    fixture.past_boundary(&mut subscription);
    let tomorrow = Local::now()
        .date_naive()
        .succ_opt()
        .unwrap()
        .and_hms_opt(7, 30, 0)
        .unwrap();
    let record = fixture.producer(
        "early-completion",
        &tomorrow.format("%Y-%m-%dT%H:%M:%S").to_string(),
    );
    let commit = fixture.finish(&record).await;
    let app = &fixture.application;
    let before = serde_json::to_value(app.metadata(&record.run_id).unwrap().unwrap()).unwrap();
    assert_eq!(
        app.reconcile_task_results(&fixture.workspace())
            .await
            .unwrap(),
        0
    );
    assert!(
        fixture
            .sessions
            .pending_task_results("local", &fixture.alice)
            .unwrap()
            .is_empty()
    );
    assert!(app.reject_pending_task_result(&record.run_id).is_err());
    app.release_run_deliveries(&record.run_id, &fixture.workspace())
        .unwrap();
    assert_available(app, &fixture.workspace(), &commit);
    app.unsubscribe_task_schedule(&fixture.origin(&fixture.alice), &subscription.id)
        .await
        .unwrap();
    assert!(app.reject_pending_task_result(&record.run_id).is_ok());
    assert_eq!(
        serde_json::to_value(app.metadata(&record.run_id).unwrap().unwrap()).unwrap(),
        before
    );
}

#[tokio::test]
async fn publication_window_waits_without_cancelling_an_authorized_result() {
    use std::{future::Future, task::Poll};

    let fixture = Fixture::new().await;
    fixture
        .share(std::slice::from_ref(&fixture.alice), true)
        .await;
    let mut subscription = fixture.subscribe(&fixture.alice, None).await;
    fixture.past_boundary(&mut subscription);
    let record = fixture.producer("publication-window", "2020-01-02T07:30:00");
    let commit = fixture.finish(&record).await;
    let app = &fixture.application;
    let workspace = fixture.workspace();
    assert_eq!(app.reconcile_task_results(&workspace).await.unwrap(), 1);
    let result = fixture
        .sessions
        .pending_task_results("local", &fixture.alice)
        .unwrap()
        .remove(0);
    let input = fixture
        .sessions
        .claim_assistant_input("local", &fixture.alice, "existing-assistant", "result")
        .unwrap()
        .unwrap();
    assert!(matches!(&input, AssistantInput::Task(task) if task.result == result));
    let source = app.metadata("existing-assistant").unwrap().unwrap();
    let key = InvocationKey {
        run_id: source.run_id,
        graph_digest: source.graph_digest,
        node_id: "work".into(),
        invocation: 1,
    };
    let assembly = crate::execution::tool_assembly::new(app.clone());
    let publishing = app.graph_catalog_mutation_guard().await;
    let path = app.graph_bundle_path("assistant");
    // Preserve authorization bytes while reproducing the publisher's temporary
    // directory state, which the normal bundle loader cannot yet admit.
    let temporary = path.join(".graph.json.tmp");
    std::fs::copy(path.join("graph.json"), &temporary).unwrap();
    assert!(app.load_graph("assistant").is_err());
    let mut reconcile = Box::pin(app.reconcile_task_results(&workspace));
    let mut authorize = assembly.authorize_assistant_input(&key, &input);
    std::future::poll_fn(|context| {
        assert!(reconcile.as_mut().poll(context).is_pending());
        assert!(authorize.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(
        !fixture
            .sessions
            .task_result_cancelled("local", &fixture.alice, &result.id)
            .unwrap(),
        "a temporary publication window must not permanently revoke the queued result"
    );
    std::fs::remove_file(temporary).unwrap();
    drop(publishing);
    let (reconciled, authorized) = tokio::join!(reconcile, authorize);
    assert_eq!(reconciled.unwrap(), 0);
    assert!(authorized.unwrap());
    assert!(
        app.authorize_subscription_result("local", &fixture.alice, &result)
            .is_ok()
    );
    assert!(
        !fixture
            .sessions
            .task_result_cancelled("local", &fixture.alice, &result.id)
            .unwrap()
    );
    assert_eq!(
        fixture
            .sessions
            .protected_task_results("local", &fixture.alice)
            .unwrap(),
        vec![result]
    );
    assert_available(app, &workspace, &commit);
}

#[tokio::test]
async fn revocation_cancels_unsent_results_but_preserves_running_claim_artifacts_until_completion()
{
    let fixture = Fixture::new().await;
    fixture
        .share(&[fixture.alice.clone(), fixture.bob.clone()], true)
        .await;
    for session in [&fixture.alice, &fixture.bob] {
        let mut subscription = fixture.subscribe(session, None).await;
        fixture.past_boundary(&mut subscription);
    }
    let record = fixture.producer("revoked-result", "2020-01-02T07:30:00");
    let commit = fixture.finish(&record).await;
    let app = &fixture.application;
    assert_eq!(
        app.reconcile_task_results(&fixture.workspace())
            .await
            .unwrap(),
        2
    );
    let alice = fixture
        .sessions
        .pending_task_results("local", &fixture.alice)
        .unwrap()
        .remove(0);
    let bob = fixture
        .sessions
        .pending_task_results("local", &fixture.bob)
        .unwrap()
        .remove(0);
    fixture
        .sessions
        .claim_assistant_input("local", &fixture.alice, "existing-assistant", "result")
        .unwrap()
        .unwrap();
    fixture
        .share(&[fixture.alice.clone(), fixture.bob.clone()], false)
        .await;
    for (session, result) in [(&fixture.alice, &alice), (&fixture.bob, &bob)] {
        assert!(
            fixture
                .sessions
                .pending_task_results("local", session)
                .unwrap()
                .is_empty()
        );
        assert!(
            fixture
                .sessions
                .task_result_cancelled("local", session, &result.id)
                .unwrap()
        );
        assert_eq!(
            fixture
                .sessions
                .task_result("local", session, &result.id)
                .unwrap()
                .as_ref(),
            Some(result)
        );
        assert!(
            app.authorize_subscription_result("local", session, result)
                .is_err()
        );
    }
    assert_eq!(
        fixture
            .sessions
            .protected_task_results("local", &fixture.alice)
            .unwrap(),
        vec![alice]
    );
    assert!(
        fixture
            .sessions
            .protected_task_results("local", &fixture.bob)
            .unwrap()
            .is_empty()
    );
    assert!(app.reject_pending_task_result(&record.run_id).is_err());
    app.release_run_deliveries(&record.run_id, &fixture.workspace())
        .unwrap();
    assert_available(app, &fixture.workspace(), &commit);
    fixture
        .sessions
        .finish_task_result("local", &fixture.alice, "existing-assistant", "result")
        .unwrap();
    assert!(app.reject_pending_task_result(&record.run_id).is_ok());
    assert_eq!(
        app.reconcile_task_results(&fixture.workspace())
            .await
            .unwrap(),
        0
    );
    let released = app
        .release_run_deliveries(&record.run_id, &fixture.workspace())
        .unwrap();
    assert!(released.expired.contains(&commit.id));
}

#[tokio::test]
async fn recipient_revocation_blocks_queued_results_without_cancelling_other_subscribers() {
    let fixture = Fixture::new().await;
    fixture
        .share(&[fixture.alice.clone(), fixture.bob.clone()], true)
        .await;
    for session in [&fixture.alice, &fixture.bob] {
        let mut subscription = fixture.subscribe(session, None).await;
        fixture.past_boundary(&mut subscription);
    }
    let origin = fixture.origin(&fixture.alice);
    let record = fixture.producer("recipient-revoked", "2020-01-02T07:30:00");
    fixture.finish(&record).await;
    let app = &fixture.application;
    assert_eq!(
        app.reconcile_task_results(&fixture.workspace())
            .await
            .unwrap(),
        2
    );
    let alice = fixture
        .sessions
        .pending_task_results("local", &fixture.alice)
        .unwrap()
        .remove(0);
    let bob = fixture
        .sessions
        .pending_task_results("local", &fixture.bob)
        .unwrap();
    fixture.channels.update("assistant", |bot| {
        bot.binding.send_users.retain(|user| user != "recipient");
    });
    assert!(
        app.authorize_subscription_result("local", &fixture.alice, &alice)
            .is_err()
    );
    assert!(
        app.authorize_subscription_result("local", &fixture.bob, &bob[0])
            .is_ok()
    );
    assert_eq!(
        app.discover_task_schedules(&origin).unwrap()["schedules"],
        json!([])
    );
    assert_eq!(
        app.list_task_subscriptions(&origin).unwrap()["subscriptions"][0]["authorized"],
        false
    );
    assert_eq!(
        app.reconcile_task_results(&fixture.workspace())
            .await
            .unwrap(),
        0
    );
    assert!(
        fixture
            .sessions
            .task_result_cancelled("local", &fixture.alice, &alice.id)
            .unwrap()
    );
    assert_eq!(
        fixture
            .sessions
            .pending_task_results("local", &fixture.bob)
            .unwrap(),
        bob
    );
    fixture.channels.update("assistant", |bot| {
        bot.binding.send_users.push("recipient".into());
    });
    assert!(
        app.authorize_subscription_result("local", &fixture.alice, &alice)
            .is_err()
    );
    assert_eq!(
        app.reconcile_task_results(&fixture.workspace())
            .await
            .unwrap(),
        0
    );
    assert!(
        fixture
            .sessions
            .pending_task_results("local", &fixture.alice)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn blocked_occurrences_queue_bounded_authorized_notices_without_duplicate_production() {
    let fixture = Fixture::new().await;
    fixture
        .share(std::slice::from_ref(&fixture.alice), true)
        .await;
    let mut subscription = fixture.subscribe(&fixture.alice, None).await;
    fixture.past_boundary(&mut subscription);
    let app = &fixture.application;
    let before = app.records().unwrap().len();
    for (at, expected) in [
        ("2020-01-02T07:30:00", 1),
        ("2020-01-02T07:30:00", 0),
        ("2020-01-03T07:30:00", 1),
    ] {
        assert_eq!(
            app.record_subscription_schedule_blocked(PLAN, at)
                .await
                .unwrap(),
            expected
        );
    }
    assert_eq!(app.records().unwrap().len(), before);
    let record = fixture.producer("stopped-publication", "2020-01-04T07:30:00");
    fixtures::control(app, &record.run_id, "stop", "producer-private-diagnostic");
    for expected in [1, 0] {
        assert_eq!(
            app.reconcile_task_results(&fixture.workspace())
                .await
                .unwrap(),
            expected
        );
    }
    let pending = fixture
        .sessions
        .pending_task_results("local", &fixture.alice)
        .unwrap();
    assert_eq!(pending.len(), 3);
    assert_ne!(pending[0].id, pending[1].id);
    for (index, result) in pending.iter().enumerate() {
        let event = if index < 2 {
            "schedule.blocked"
        } else {
            "run.blocked"
        };
        assert_eq!(result.outcome, "blocked");
        assert_eq!(result.content["event"], event);
        assert_eq!(result.content["admission_confirmed"], index == 2);
        assert_eq!(
            result.source_run.as_deref(),
            (index == 2).then_some(record.run_id.as_str())
        );
        assert!(result.files.is_empty());
        assert!(
            app.authorize_subscription_result("local", &fixture.alice, result)
                .is_ok()
        );
        assert!(
            app.authorize_subscription_result("local", &fixture.bob, result)
                .is_err()
        );
        let encoded = serde_json::to_string(result).unwrap();
        assert!(encoded.len() < 4096);
        for private in [
            "producer-private-diagnostic",
            "producer-input-marker",
            "producer-origin-marker",
            "graph_tasks_status",
        ] {
            assert!(!encoded.contains(private), "{private}");
        }
    }
    assert!(
        app.task_run_status(&fixture.origin(&fixture.alice), &record.run_id)
            .is_err()
    );
    assert!(
        fixture
            .sessions
            .pending_task_results("local", &fixture.bob)
            .unwrap()
            .is_empty()
    );
    assert_eq!(app.records().unwrap().len(), before + 1);
}
