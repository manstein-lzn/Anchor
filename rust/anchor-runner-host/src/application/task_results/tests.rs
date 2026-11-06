use super::*;
use crate::application::run_access::fixtures;
use crate::artifacts::ArtifactAvailability;
use crate::run_metadata::GraphCallSource;
use crate::task_delegation::TaskOrigin;
use anchor_platform_session::{AssistantInput, ChannelIdentity, ChannelInboundRequest, TurnStatus};
use anchor_runtime::graph::{
    ArtifactPort, GraphRunRecord, GraphSnapshot, GraphTaskPolicy, InvocationKey, NodeCompletion,
    validator::CallSubmission,
};
use std::collections::BTreeMap;

fn sessions(application: &RunApplication) -> SessionStore {
    crate::host_fs::directory(&application.data_root.join("platform")).unwrap();
    crate::assistant::sessions(&application.data_root).unwrap()
}

fn session(sessions: &SessionStore, conversation: &str, assistant: &str) -> String {
    let admission = sessions
        .admit_channel_inbound(
            "owner",
            ChannelInboundRequest {
                inbound_id: format!("inbound-{conversation}"),
                identity: ChannelIdentity {
                    source: "wecom".into(),
                    account: Some("bot".into()),
                    conversation_kind: Default::default(),
                    conversation_id: conversation.into(),
                    sender_id: conversation.into(),
                },
                graph: "assistant".into(),
                reply_node: "agent".into(),
                text: Some("set up a task".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: false,
            },
        )
        .unwrap();
    let session = admission.session.id;
    sessions
        .bind_channel_assistant("owner", &session, assistant, "wait", "agent", "reply")
        .unwrap();
    let input = sessions
        .claim_assistant_input("owner", &session, assistant, "user")
        .unwrap()
        .unwrap();
    sessions
        .finish_turn(
            "owner",
            &session,
            &input.turn().id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    session
}

fn task_definition() -> serde_json::Value {
    json!({"objective":"task fixture","entry":"work","agents":{},
        "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]})
}

fn create(application: &RunApplication, id: &str, session: Option<&str>) -> GraphRunRecord {
    create_graph(application, id, session, task_definition())
}

fn create_graph(
    application: &RunApplication,
    id: &str,
    session: Option<&str>,
    definition: serde_json::Value,
) -> GraphRunRecord {
    let path = application.graph_bundle_path("task-graph");
    crate::application::graphs::write_graph_bundle(&path, &definition).unwrap();
    let record =
        GraphRunRecord::create_with_id(GraphSnapshot::admit(definition).unwrap(), json!({}), id)
            .unwrap();
    fixtures::create(application, &record);
    let mut metadata = RunMetadata::new(
        id.into(),
        "task-graph".into(),
        record.graph_digest.clone(),
        &path,
    )
    .unwrap();
    metadata.task = Some(TaskOrigin {
        owner: "owner".into(),
        session: session.map(str::to_owned),
        graph: "assistant".into(),
        source: crate::task_delegation::TaskOriginSource::AgentDelegation {
            key: InvocationKey {
                run_id: "assistant-run".into(),
                graph_digest: "a".repeat(64),
                node_id: "agent".into(),
                invocation: 1,
            },
        },
        authorization_digest: "a".repeat(64),
        policy: GraphTaskPolicy::default(),
        bindings: BTreeMap::new(),
        targets: BTreeMap::new(),
    });
    metadata::save(&application.data_root, &metadata).unwrap();
    record
}

async fn complete(
    application: &RunApplication,
    workspace: &Path,
    record: &GraphRunRecord,
) -> anchor_runtime::graph::CommitRef {
    complete_node(
        application,
        workspace,
        record,
        "work",
        fixtures::completion(json!({"files":["report.md"]})),
    )
    .await
}

async fn complete_node(
    application: &RunApplication,
    workspace: &Path,
    record: &GraphRunRecord,
    node: &str,
    completion: NodeCompletion,
) -> anchor_runtime::graph::CommitRef {
    let artifacts =
        crate::HostArtifacts::new(application.data_root.join("artifacts"), workspace.into());
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: node.into(),
        invocation: 1,
    };
    let directory = artifacts.workspace_path(&key).unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("report.md"), b"observed report").unwrap();
    std::fs::write(directory.join("not-selected.txt"), b"not delivered").unwrap();
    let commit = artifacts.freeze(&key, &completion).await.unwrap();
    fixtures::complete(
        application,
        &record.run_id,
        node,
        completion,
        Some(commit.clone()),
    );
    commit
}

fn detached_definition(target: &str) -> serde_json::Value {
    json!({"objective":"detached fixture","entry":"work","agents":{},
        "ops":{"work":{"call":{"graph":target,"mode":"detach"}}},
        "nodes":[{"id":"work","op":"work"}],"edges":[]})
}

fn called_child(
    application: &RunApplication,
    graph: &str,
    id: &str,
    parent: &GraphRunRecord,
    definition: serde_json::Value,
) -> GraphRunRecord {
    let path = application.graph_bundle_path(graph);
    crate::application::graphs::write_graph_bundle(&path, &definition).unwrap();
    let record =
        GraphRunRecord::create_with_id(GraphSnapshot::admit(definition).unwrap(), json!({}), id)
            .unwrap();
    let parent_metadata = application.metadata(&parent.run_id).unwrap().unwrap();
    let metadata = RunMetadata::graph_call_child(
        id.into(),
        graph.into(),
        record.graph_digest.clone(),
        &path,
        GraphCallSource {
            parent_run: parent.run_id.clone(),
            parent_graph: parent_metadata.graph,
            parent_graph_digest: parent.graph_digest.clone(),
            node: "work".into(),
            invocation: 1,
            mode: "detach".into(),
            root_run: parent_metadata
                .graph_call
                .map_or_else(|| parent.run_id.clone(), |source| source.root_run),
        },
    )
    .unwrap();
    metadata::save(&application.data_root, &metadata).unwrap();
    fixtures::create(application, &record);
    record
}

async fn detached_commit(
    application: &RunApplication,
    workspace: &Path,
    record: &GraphRunRecord,
) -> anchor_runtime::graph::CommitRef {
    let artifacts =
        crate::HostArtifacts::new(application.data_root.join("artifacts"), workspace.into());
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    };
    std::fs::create_dir_all(artifacts.workspace_path(&key).unwrap()).unwrap();
    artifacts
        .freeze(&key, &fixtures::completion(json!({})))
        .await
        .unwrap()
}

fn detach(
    application: &RunApplication,
    parent: &GraphRunRecord,
    child: &GraphRunRecord,
    commit: anchor_runtime::graph::CommitRef,
) {
    fixtures::call(
        application,
        &parent.run_id,
        "work",
        CallSubmission::Completed {
            child_run_id: child.run_id.clone(),
            output: json!({}),
            commit,
        },
    );
    assert!(
        application
            .read_run(&parent.run_id)
            .unwrap()
            .unwrap()
            .is_settled()
    );
}

#[tokio::test]
async fn bounded_results_select_only_final_outputs_and_preserve_all_run_facts() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    let workspace = root.path().join("workspaces");
    let sessions = sessions(&application);
    let session = session(&sessions, "alice", "assistant-run");
    let names = (0..6)
        .map(|index| format!("work{index}"))
        .collect::<Vec<_>>();
    let definition = json!({"objective":"large task fixture","agents":{},"entry":names[0],"ops":{"work":{"run":"true"}},
        "nodes":names.iter().map(|name|json!({"id":name,"op":"work"})).collect::<Vec<_>>(),
        "edges":names.windows(2).map(|pair|json!({"from":pair[0],"to":pair[1]})).collect::<Vec<_>>()});
    let record = create_graph(&application, "large", Some(&session), definition);
    let mut expected_files = Vec::new();
    let mut expected_submissions = Vec::new();
    for (index, node) in names.iter().enumerate() {
        let mut completion = fixtures::completion(json!({"files":["report.md"]}));
        completion.submission =
            json!({"report":"x".repeat(if index == 0 {400 * 1024} else {24 * 1024})}).to_string();
        expected_submissions.push(completion.submission.clone());
        let commit = complete_node(&application, &workspace, &record, node, completion).await;
        expected_files.push(ChannelNotificationFile {
            artifact_id: commit.id,
            path: "report.md".into(),
            name: format!("{}-report.md", index + 1),
        });
    }
    assert_eq!(
        application
            .reconcile_task_results(&workspace)
            .await
            .unwrap(),
        1
    );
    let results = sessions.pending_task_results("owner", &session).unwrap();
    let result = &results[0];
    assert!(serde_json::to_vec(result).unwrap().len() < 128 * 1024);
    let mut final_file = expected_files.pop().unwrap();
    final_file.name = "1-report.md".into();
    assert_eq!(result.files, vec![final_file]);
    assert_eq!(result.content["output_nodes"], json!(["work5"]));
    let nodes = result.content["nodes"].as_array().unwrap();
    let mut omitted = 0;
    for (node, expected) in nodes.iter().zip(&expected_submissions) {
        let submission = &node["submission"];
        if submission["omitted"] == true {
            omitted += 1;
            let encoded = serde_json::to_vec(expected).unwrap();
            assert_eq!(submission["bytes"], encoded.len());
            assert_eq!(
                submission["sha256"],
                format!("{:x}", Sha256::digest(encoded))
            );
            assert_eq!(submission["read_tool"], "graph_tasks_status");
        } else {
            assert_eq!(submission, &json!(expected));
        }
    }
    assert_eq!(
        omitted, 3,
        "one oversized value and two beyond the shared budget"
    );
    let view = application.read_run("large").unwrap().unwrap();
    for (name, expected) in names.iter().zip(expected_submissions) {
        assert_eq!(
            view.progress().results[name]
                .last()
                .unwrap()
                .completion
                .submission,
            expected
        );
    }
}

#[tokio::test]
async fn terminal_results_reconcile_after_restart_without_reviving_stopped_assistants() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("state");
    let catalog = root.path().join("catalog");
    let workspace = root.path().join("workspaces");
    let application = RunApplication::new(data.clone(), catalog.clone());
    let sessions = sessions(&application);
    let alice = session(&sessions, "alice", "assistant-run");
    let bob = session(&sessions, "bob", "bob-assistant");
    let done = create(&application, "done", Some(&alice));
    let commit = complete(&application, &workspace, &done).await;
    create(&application, "failed", Some(&alice));
    fixtures::fail(&application, "failed", "work", "fixture failed");
    create(&application, "aborted", Some(&bob));
    fixtures::control(&application, "aborted", "abandon", "fixture abandoned");
    create(&application, "stopped", Some(&alice));
    fixtures::control(&application, "stopped", "stop", "stopped");
    create(&application, "executing", Some(&alice));
    create(&application, "unbound", None);
    fixtures::fail(&application, "unbound", "work", "no callback requested");
    sessions
        .retire_channel_assistant("owner", &alice, "assistant-run")
        .unwrap();
    drop(application);
    let restarted = RunApplication::new(data.clone(), catalog);
    assert_eq!(
        restarted.reconcile_task_results(&workspace).await.unwrap(),
        4
    );
    let alice_results = sessions.pending_task_results("owner", &alice).unwrap();
    assert_eq!(
        alice_results
            .iter()
            .map(|result| result.outcome.as_str())
            .collect::<Vec<_>>(),
        vec!["done", "failed", "blocked"]
    );
    let result = &alice_results[0];
    assert_eq!(result.source_run.as_deref(), Some("done"));
    assert_eq!(
        result.content["request"]["source"]["key"]["node_id"],
        "agent"
    );
    assert_eq!(alice_results[2].content["event"], "run.blocked");
    assert_eq!(alice_results[2].source_run.as_deref(), Some("stopped"));
    assert_eq!(alice_results[2].content["terminal"], false);
    assert_eq!(
        result.files,
        vec![ChannelNotificationFile {
            artifact_id: commit.id,
            path: "report.md".into(),
            name: "1-report.md".into()
        }]
    );
    assert!(!result.content.to_string().contains("bindings"));
    assert_eq!(
        sessions.pending_task_results("owner", &bob).unwrap()[0].outcome,
        "aborted"
    );
    assert!(
        sessions
            .get_channel_assistant("owner", &alice)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        restarted.reconcile_task_results(&workspace).await.unwrap(),
        0
    );
    assert_eq!(
        sessions.pending_task_results("owner", &alice).unwrap(),
        alice_results
    );
    assert_eq!(restarted.run_views().unwrap().len(), 6);
}

#[tokio::test]
async fn blocked_schedule_queues_one_notice_per_occurrence_without_admitting_a_run() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    let sessions = sessions(&application);
    let session = session(&sessions, "alice", "assistant-run");
    create(&application, "retained", Some(&session));
    let origin = application
        .metadata("retained")
        .unwrap()
        .unwrap()
        .task
        .unwrap();
    for detail in ["permission changed", "still unavailable"] {
        application
            .record_schedule_blocked(
                &origin,
                "plan",
                "2026-11-07T07:30:00",
                "task-graph",
                &ApplicationError::Conflict(detail.into()),
            )
            .unwrap();
    }
    let queued = sessions.pending_task_results("owner", &session).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].source_run, None);
    assert_eq!(queued[0].outcome, "blocked");
    assert_eq!(queued[0].content["event"], "schedule.blocked");
    assert_eq!(queued[0].content["admission_confirmed"], false);
    assert!(queued[0].files.is_empty());
    assert_eq!(application.run_views().unwrap().len(), 1);
    application
        .record_schedule_blocked(
            &origin,
            "plan",
            "2026-11-08T07:30:00",
            "task-graph",
            &ApplicationError::Missing,
        )
        .unwrap();
    assert_eq!(
        sessions
            .pending_task_results("owner", &session)
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn oversized_result_keeps_its_run_and_reports_the_queue_limit_to_its_assistant() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    let workspace = root.path().join("workspaces");
    let sessions = sessions(&application);
    let session = session(&sessions, "alice", "assistant-run");
    create(&application, "large-error", Some(&session));
    fixtures::fail(&application, "large-error", "work", &"x".repeat(300 * 1024));
    for _ in 0..2 {
        assert!(
            application
                .reconcile_task_results(&workspace)
                .await
                .is_err()
        );
    }
    let results = sessions.pending_task_results("owner", &session).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content["event"], "result.blocked");
    assert!(
        results[0].content["reason"]
            .as_str()
            .unwrap()
            .contains("metadata is too large")
    );
    assert!(
        application
            .reject_pending_task_result("large-error")
            .is_err()
    );
}

#[tokio::test]
async fn queue_retry_is_stable_and_one_unreadable_result_does_not_starve_other_sessions() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    let workspace = root.path().join("workspaces");
    let sessions = sessions(&application);
    let alice = session(&sessions, "alice", "assistant-run");
    let bob = session(&sessions, "bob", "bob-assistant");
    let broken = create(&application, "a-broken", Some(&alice));
    let commit = complete(&application, &workspace, &broken).await;
    create(&application, "z-other", Some(&bob));
    fixtures::fail(
        &application,
        "z-other",
        "work",
        "failure is a deliverable result",
    );
    let manifest = application
        .data_root
        .join("artifacts")
        .join(&commit.id)
        .join("manifest.json");
    let original = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"invalid").unwrap();
    assert!(
        application
            .reconcile_task_results(&workspace)
            .await
            .is_err()
    );
    let notice = sessions.pending_task_results("owner", &alice).unwrap();
    assert_eq!(notice.len(), 1);
    assert_eq!(notice[0].content["event"], "result.blocked");
    assert!(notice[0].files.is_empty());
    assert_eq!(
        sessions.pending_task_results("owner", &bob).unwrap().len(),
        1
    );
    assert!(application.reject_pending_task_result("a-broken").is_err());
    std::fs::write(&manifest, original).unwrap();
    assert_eq!(
        application
            .reconcile_task_results(&workspace)
            .await
            .unwrap(),
        1
    );
    let result = sessions.pending_task_results("owner", &alice).unwrap()[1].clone();
    sessions
        .record_task_result("owner", &alice, &result)
        .unwrap();
    assert_eq!(
        application
            .reconcile_task_results(&workspace)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sessions.pending_task_results("owner", &alice).unwrap(),
        vec![notice[0].clone(), result]
    );
}

#[tokio::test]
async fn task_delivery_protects_runs_and_exact_outputs_until_durable_handling() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    let workspace = root.path().join("workspaces");
    let sessions = sessions(&application);
    let session = session(&sessions, "alice", "assistant-run");
    let record = create(&application, "pending", Some(&session));
    let commit = complete(&application, &workspace, &record).await;
    let artifacts =
        crate::HostArtifacts::new(application.data_root.join("artifacts"), workspace.clone());
    let policy = crate::retention::Policy {
        max_age: Some(Duration::ZERO),
        max_bytes: Some(0),
    };
    assert!(matches!(
        application.delete_run("pending", &workspace).await,
        Err(ApplicationError::Conflict(_))
    ));
    assert!(matches!(
        application
            .delete_graph(
                "task-graph",
                &application.graph_bundle_path("task-graph"),
                &workspace
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    application
        .reclaim_deliveries(&policy, &workspace, i64::MAX)
        .unwrap();
    assert_eq!(
        artifacts.availability(&commit).unwrap(),
        ArtifactAvailability::Available
    );
    application
        .reconcile_task_results(&workspace)
        .await
        .unwrap();
    let AssistantInput::Task(input) = sessions
        .claim_assistant_input("owner", &session, "assistant-run", "task")
        .unwrap()
        .unwrap()
    else {
        panic!("expected completion input")
    };
    assert_eq!(input.result.source_run.as_deref(), Some("pending"));
    assert!(application.reject_pending_task_result("pending").is_err());
    application
        .reclaim_deliveries(&policy, &workspace, i64::MAX)
        .unwrap();
    assert_eq!(
        artifacts.availability(&commit).unwrap(),
        ArtifactAvailability::Available
    );
    sessions
        .finish_task_result("owner", &session, "assistant-run", "task")
        .unwrap();
    assert!(application.reject_pending_task_result("pending").is_ok());
    assert_eq!(
        application
            .reconcile_task_results(&workspace)
            .await
            .unwrap(),
        0
    );
    application
        .reclaim_deliveries(&policy, &workspace, i64::MAX)
        .unwrap();
    assert_eq!(
        artifacts.availability(&commit).unwrap(),
        ArtifactAvailability::Expired
    );
    application.delete_run("pending", &workspace).await.unwrap();
    assert!(application.read_run("pending").unwrap().is_none());
}

#[tokio::test]
async fn outgoing_task_keeps_its_source_graph_and_alias_until_durable_handling() {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    let source = catalog.join("source");
    let application = RunApplication::new(root.path().join("state"), catalog)
        .with_configured_graph("assistant".into(), source.clone());
    let workspace = root.path().join("workspaces");
    crate::application::graphs::write_graph_bundle(&source, &task_definition()).unwrap();
    let sessions = sessions(&application);
    let session = session(&sessions, "alice", "assistant-run");
    let record = create(&application, "outgoing", Some(&session));
    let unrelated = application.graph_bundle_path("unrelated");
    crate::application::graphs::write_graph_bundle(&unrelated, &task_definition()).unwrap();
    application
        .remove_graph("unrelated", &workspace)
        .await
        .unwrap();
    assert!(!unrelated.exists());

    for phase in ["unfinished", "terminal", "queued", "processing"] {
        match phase {
            "terminal" => {
                complete(&application, &workspace, &record).await;
            }
            "queued" => {
                assert_eq!(
                    application
                        .reconcile_task_results(&workspace)
                        .await
                        .unwrap(),
                    1
                );
            }
            "processing" => {
                assert!(matches!(
                    sessions
                        .claim_assistant_input("owner", &session, "assistant-run", "callback")
                        .unwrap(),
                    Some(AssistantInput::Task(_))
                ));
            }
            _ => {}
        }
        for name in ["assistant", "source"] {
            assert!(
                matches!(
                    application.remove_graph(name, &workspace).await,
                    Err(crate::application::graphs::GraphManagementError::Conflict(reason))
                        if reason.contains("delegated Graph task")
                ),
                "source Graph {name} must survive its outgoing task in phase {phase}"
            );
            assert!(source.join("graph.json").is_file());
        }
    }
    sessions
        .finish_task_result("owner", &session, "assistant-run", "callback")
        .unwrap();
    assert_eq!(
        application
            .remove_graph("source", &workspace)
            .await
            .unwrap(),
        0
    );
    assert!(!source.exists());
    assert!(
        application
            .read_run("outgoing")
            .unwrap()
            .unwrap()
            .is_settled()
    );
}

#[tokio::test]
async fn outgoing_task_does_not_retain_historical_source_run_or_settled_non_session_source() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    let workspace = root.path().join("workspaces");
    let source = application.graph_bundle_path("assistant");
    crate::application::graphs::write_graph_bundle(&source, &task_definition()).unwrap();
    let (_, loaded) = application.load_graph("assistant").unwrap();
    let historical =
        GraphRunRecord::create_with_id(loaded.snapshot, json!({}), "assistant-run").unwrap();
    fixtures::create(&application, &historical);
    fixtures::finish(&application, "assistant-run");
    let historical_metadata = RunMetadata::new(
        historical.run_id,
        "assistant".into(),
        historical.graph_digest,
        &source,
    )
    .unwrap();
    metadata::save(&application.data_root, &historical_metadata).unwrap();
    let task = create(&application, "non-session-task", None);

    application
        .delete_run("assistant-run", &workspace)
        .await
        .unwrap();
    assert!(application.read_run("assistant-run").unwrap().is_none());
    assert!(source.join("graph.json").is_file());
    assert!(matches!(
        application.remove_graph("assistant", &workspace).await,
        Err(crate::application::graphs::GraphManagementError::Conflict(
            _
        ))
    ));
    complete(&application, &workspace, &task).await;
    assert_eq!(
        application
            .remove_graph("assistant", &workspace)
            .await
            .unwrap(),
        0
    );
    assert!(!source.exists());
    assert!(application.read_run("non-session-task").unwrap().is_some());
}

#[tokio::test]
async fn handled_task_keeps_source_session_and_root_until_detached_descendants_settle() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    let workspace = root.path().join("workspaces");
    crate::application::graphs::write_graph_bundle(
        &application.graph_bundle_path("assistant"),
        &task_definition(),
    )
    .unwrap();
    let sessions = sessions(&application);
    let session = session(&sessions, "alice", "assistant-run");
    let task = create_graph(
        &application,
        "detached-root",
        Some(&session),
        detached_definition("middle"),
    );
    let middle = called_child(
        &application,
        "middle",
        "middle-run",
        &task,
        detached_definition("leaf"),
    );
    let leaf = called_child(&application, "leaf", "leaf-run", &middle, task_definition());
    detach(
        &application,
        &task,
        &middle,
        detached_commit(&application, &workspace, &task).await,
    );
    detach(
        &application,
        &middle,
        &leaf,
        detached_commit(&application, &workspace, &middle).await,
    );
    assert_eq!(
        application
            .reconcile_task_results(&workspace)
            .await
            .unwrap(),
        1
    );
    assert!(matches!(
        sessions
            .claim_assistant_input("owner", &session, "assistant-run", "callback")
            .unwrap(),
        Some(AssistantInput::Task(_))
    ));
    sessions
        .finish_task_result("owner", &session, "assistant-run", "callback")
        .unwrap();

    for phase in ["executing", "stopped"] {
        if phase == "stopped" {
            fixtures::control(&application, &leaf.run_id, "stop", "fixture stopped");
        }
        for graph in ["assistant", "task-graph"] {
            assert!(matches!(
                application.remove_graph(graph, &workspace).await,
                Err(crate::application::graphs::GraphManagementError::Conflict(reason))
                    if reason.contains("delegated Graph task")
            ));
        }
        assert!(matches!(
            application.delete_task_session("owner", &session).await,
            Err(ApplicationError::Conflict(reason)) if reason.contains("delegated Graph task")
        ));
    }
    fixtures::control(&application, &leaf.run_id, "abandon", "fixture abandoned");
    // Release completed descendant history through its ordinary Graph cascade.
    crate::application::graphs::write_graph_bundle(
        &application.graph_bundle_path("middle"),
        &task_definition(),
    )
    .unwrap();
    assert_eq!(
        application.remove_graph("leaf", &workspace).await.unwrap(),
        1
    );
    assert!(application.metadata(&leaf.run_id).unwrap().is_none());
    assert_eq!(
        application
            .remove_graph("assistant", &workspace)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        application
            .remove_graph("task-graph", &workspace)
            .await
            .unwrap(),
        1
    );
    assert!(application.read_run(&task.run_id).unwrap().is_none());
    assert!(
        application
            .read_run(&middle.run_id)
            .unwrap()
            .unwrap()
            .is_settled()
    );
}

#[tokio::test]
async fn deletion_rechecks_durable_children_published_after_its_metadata_inventory() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    let workspace = root.path().join("workspaces");
    let task = create_graph(
        &application,
        "racing-root",
        None,
        detached_definition("late-child"),
    );
    let commit = detached_commit(&application, &workspace, &task).await;
    let published = std::cell::Cell::new(false);
    let result = application.check_task_reference_deletion(&|metadata, _| {
        assert_eq!(metadata.run_id, task.run_id);
        assert!(!published.replace(true));
        let child = called_child(
            &application,
            "late-child",
            "late-child-run",
            &task,
            task_definition(),
        );
        detach(&application, &task, &child, commit.clone());
        Ok(true)
    });
    assert!(published.get());
    assert!(matches!(result, Err(ApplicationError::Conflict(_))));
    fixtures::finish(&application, "late-child-run");
    assert!(
        application
            .check_task_origin_deletion(&|_| Ok(true))
            .is_ok()
    );
}
