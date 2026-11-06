use super::*;
use crate::{
    application::{RunApplication, run_access::fixtures},
    node_host::HostIoResolver,
};
use anchor_platform_session::{ChannelIdentity, ChannelInboundRequest, SessionStore, TaskResult};
use anchor_runtime::graph::{CompletionFact, GraphRunRecord, GraphSnapshot, NodeExecutionPort};
use anchor_sandbox_bwrap::{BubblewrapPolicy, BubblewrapSandbox};

fn inbound(id: &str) -> ChannelInboundRequest {
    ChannelInboundRequest {
        inbound_id: id.into(),
        identity: ChannelIdentity {
            source: "wecom".into(),
            account: Some("bot".into()),
            conversation_kind: Default::default(),
            conversation_id: "alice".into(),
            sender_id: "alice".into(),
        },
        graph: "assistant".into(),
        reply_node: "agent".into(),
        text: Some(id.into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
    }
}

fn fixture() -> (
    tempfile::TempDir,
    HostNodes,
    SessionStore,
    String,
    InvocationKey,
) {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let workspace = root.path().join("work");
    let library = root.path().join("library");
    let bundle = root.path().join("assistant");
    for directory in [
        &workspace,
        &library.join("plugins"),
        &bundle,
        &state.join("platform"),
    ] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let definition = json!({
        "entry":"wait", "agents":{"worker":{"model":"fixture","instructions":"fixture"}},
        "ops":{"wait":{"host":{"operation":"session.wait_input"}},"reply":{"host":{"operation":"session.reply"}}},
        "nodes":[{"id":"wait","op":"wait"},{"id":"agent","agent":"worker"},{"id":"reply","op":"reply"}],
        "edges":[{"from":"wait","to":"agent"},{"from":"agent","to":"reply"},{"from":"reply","to":"wait"}],
    });
    std::fs::write(bundle.join("graph.json"), definition.to_string()).unwrap();
    std::fs::write(
        bundle.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[]}).to_string(),
    )
    .unwrap();
    let store = SessionStore::open(state.join("platform/sessions.sqlite")).unwrap();
    let admitted = store
        .admit_channel_inbound("owner", inbound("initial"))
        .unwrap();
    let session = admitted.session.id;
    let mut record = GraphRunRecord::create_with_id(
        GraphSnapshot::from_authoring(definition).unwrap(),
        json!({}),
        "assistant-run",
    )
    .unwrap();
    record.plugin_bindings_initialized = true;
    let app = RunApplication::new(state.clone(), library.clone());
    fixtures::create(&app, &record);
    store
        .bind_channel_assistant("owner", &session, &record.run_id, "wait", "agent", "reply")
        .unwrap();
    store
        .claim_channel_assistant_input("owner", &session, &record.run_id, "initial")
        .unwrap()
        .unwrap();
    store
        .finish_turn(
            "owner",
            &session,
            &admitted.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    let mut metadata = crate::run_metadata::RunMetadata::new(
        record.run_id.clone(),
        "assistant".into(),
        record.graph_digest.clone(),
        &bundle,
    )
    .unwrap();
    metadata.assistant = Some(crate::run_metadata::AssistantSource {
        owner: "owner".into(),
        session: session.clone(),
        wait_node: "wait".into(),
        work_node: "agent".into(),
        reply_node: "reply".into(),
    });
    metadata.conversation = Some(crate::run_metadata::ConversationSource {
        session: session.clone(),
        reply_node: "agent".into(),
        previous_run: None,
    });
    crate::run_metadata::save(&state, &metadata).unwrap();
    let artifacts = crate::HostArtifacts::new(state.join("artifacts"), workspace.clone());
    let sandbox = Arc::new(
        BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&workspace),
        )
        .unwrap(),
    );
    let resolver = HostIoResolver::new(
        artifacts.clone(),
        sandbox.clone(),
        Arc::new(crate::tool_host::PluginToolHost::new(Vec::<String>::new())),
        crate::tool_host::McpToolConfig::default(),
        crate::local_inputs::LocalInputs::new(state.clone(), None).unwrap(),
        Arc::new(crate::plugin_mounts::SessionMounts::open(&state).unwrap()),
        None,
        crate::execution::tool_assembly::new(app.clone()),
    )
    .with_library_root(Some(library));
    let _ = resolver.run_application.set(app);
    let nodes = HostNodes {
        sandbox,
        artifacts,
        allowed_commands: vec!["sh".into()],
        facts_root: state.join("facts"),
        io_resolver: Arc::new(resolver),
        goose_nodes: None,
    };
    let key = InvocationKey {
        run_id: record.run_id,
        graph_digest: record.graph_digest,
        node_id: "wait".into(),
        invocation: 2,
    };
    (root, nodes, store, session, key)
}

fn request(key: &InvocationKey, commits: Vec<CommitRef>) -> NodeExecutionRequest {
    NodeExecutionRequest {
        key: key.clone(),
        kind: NodeKind::OpHost,
        model: None,
        task: String::new(),
        instructions: String::new(),
        routes: vec![
            if key.node_id == "wait" {
                "agent"
            } else {
                "wait"
            }
            .into(),
        ],
        input: json!({}),
        input_commits: commits,
        plugins: vec![],
        max_provider_requests: None,
        wall_time_limit_seconds: None,
        network: false,
        operation: Some(
            json!({"operation":if key.node_id == "wait" { "session.wait_input" } else { "session.reply" }}),
        ),
        writes: vec![],
        cancellation: Arc::new(AtomicBool::new(false)),
    }
}

fn result(id: &str) -> TaskResult {
    TaskResult {
        id: id.into(),
        source_run: Some("child-run".into()),
        outcome: "completed".into(),
        content: json!({"summary":"morning report"}),
        files: vec![],
    }
}

async fn receive(nodes: &HostNodes, key: &InvocationKey) -> NodeCompletion {
    let NodeExecutionOutcome::Completed(completion) =
        nodes.execute(request(key, vec![])).await.unwrap()
    else {
        panic!("wait did not complete")
    };
    completion
}

#[tokio::test]
async fn task_input_mounts_only_selected_artifact_files_without_channel_identity() {
    let (_root, nodes, store, session, key) = fixture();
    let producer = InvocationKey {
        run_id: "child-run".into(),
        node_id: "produce".into(),
        invocation: 1,
        ..key.clone()
    };
    let workspace = nodes.artifacts.prepare_workspace(&producer, &[]).unwrap();
    std::fs::write(workspace.join("report.md"), "selected").unwrap();
    std::fs::write(workspace.join("unrelated.txt"), "not selected").unwrap();
    let produced = NodeCompletion {
        submission: "report".into(),
        route: None,
        model_requests: 0,
        output: json!({"files":["report.md","unrelated.txt"]}),
    };
    let commit = nodes.artifacts.freeze(&producer, &produced).await.unwrap();
    let mut task = result("morning");
    task.files
        .push(anchor_platform_session::ChannelNotificationFile {
            artifact_id: commit.id.clone(),
            path: "report.md".into(),
            name: "briefing.md".into(),
        });
    store.record_task_result("owner", &session, &task).unwrap();
    let completion = receive(&nodes, &key).await;
    assert_eq!(completion.output["source"], "task_result");
    assert_eq!(completion.output["task_result"]["id"], "morning");
    assert_eq!(
        completion.output["task_result"]["files"][0]["path"],
        "/in/tasks/morning/briefing.md"
    );
    assert!(completion.output.get("channel").is_none());
    assert!(completion.output.get("message").is_none());
    assert!(nodes.io_resolver.attested_channel(&key).unwrap().is_none());
    let mounts = nodes.io_resolver.local_input_mounts(&key).unwrap();
    assert_eq!(mounts.len(), 1);
    assert_eq!(
        mounts[0].destination,
        PathBuf::from("/in/tasks/morning/briefing.md")
    );
    assert!(mounts[0].source.is_file());
    assert_eq!(
        std::fs::read_to_string(&mounts[0].source).unwrap(),
        "selected"
    );
    assert!(!mounts.iter().any(|mount| mount.source.is_dir()));
    let saved = crate::run_metadata::load(nodes.io_resolver.state_root(), &key.run_id)
        .unwrap()
        .unwrap();
    let selected =
        assistant::attachment_metadata(nodes.io_resolver.state_root(), &saved, &key).unwrap();
    assert!(selected.attachments.is_empty());
}

#[tokio::test]
async fn silent_task_reply_ack_is_recovered_from_durable_completion_and_never_becomes_a_channel_reply()
 {
    let (_root, nodes, store, session, wait) = fixture();
    store
        .record_task_result("owner", &session, &result("first"))
        .unwrap();
    store
        .record_task_result("owner", &session, &result("second"))
        .unwrap();
    let completion = receive(&nodes, &wait).await;
    let turn = completion.output["turn"].as_str().unwrap();
    let wait_commit = nodes.artifacts.freeze(&wait, &completion).await.unwrap();
    let work = InvocationKey {
        node_id: "agent".into(),
        ..wait.clone()
    };
    assistant::bind_request(
        nodes.io_resolver.state_root(),
        &request(&work, vec![wait_commit.clone()]),
    )
    .unwrap();
    let workspace = nodes
        .artifacts
        .prepare_workspace(&work, &[wait_commit])
        .unwrap();
    assert!(workspace.is_dir());
    let work_completion = NodeCompletion {
        submission: "private processing note, not an outgoing message".into(),
        route: Some("reply".into()),
        model_requests: 0,
        output: json!({"files":[]}),
    };
    let work_commit = nodes
        .artifacts
        .freeze(&work, &work_completion)
        .await
        .unwrap();
    let reply = InvocationKey {
        node_id: "reply".into(),
        ..wait.clone()
    };
    // Simulate the crash window after the reply fact is saved but before Session ACK.
    assistant::bind_request(
        nodes.io_resolver.state_root(),
        &request(&reply, vec![work_commit.clone()]),
    )
    .unwrap();
    let persisted = NodeCompletion {
        submission: "silent result handling".into(),
        route: Some("wait".into()),
        model_requests: 0,
        output: json!({"files":[],"suppressed":true,"reply_for":turn,"task_result":"first","task_result_handled":true,"source_commit":work_commit,"items":[]}),
    };
    nodes.complete(&reply, persisted.clone()).unwrap();
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap().len(),
        2
    );
    for _ in 0..2 {
        assert_eq!(
            nodes.completion_fact(&reply).await.unwrap(),
            CompletionFact::Completed(persisted.clone())
        );
    }
    assert_eq!(
        store.get_turn("owner", &session, turn).unwrap().status,
        TurnStatus::Completed
    );
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![result("second")]
    );
    let next = InvocationKey {
        invocation: 3,
        ..wait
    };
    let second = receive(&nodes, &next).await;
    assert_eq!(second.output["task_result"]["id"], "second");
    let second_wait = nodes.artifacts.freeze(&next, &second).await.unwrap();
    let second_work = InvocationKey {
        node_id: "agent".into(),
        ..next.clone()
    };
    assistant::bind_request(
        nodes.io_resolver.state_root(),
        &request(&second_work, vec![second_wait.clone()]),
    )
    .unwrap();
    nodes
        .artifacts
        .prepare_workspace(&second_work, &[second_wait])
        .unwrap();
    let second_commit = nodes
        .artifacts
        .freeze(&second_work, &work_completion)
        .await
        .unwrap();
    let second_reply = InvocationKey {
        node_id: "reply".into(),
        ..next
    };
    let NodeExecutionOutcome::Completed(done) = nodes
        .execute(request(&second_reply, vec![second_commit]))
        .await
        .unwrap()
    else {
        panic!("reply did not complete")
    };
    assert_eq!(done.output["suppressed"], true);
    assert_eq!(done.output["task_result_handled"], true);
    assert!(!done.submission.contains("private processing note"));
    assert!(
        store
            .pending_task_results("owner", &session)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn user_interrupt_keeps_the_task_pending_and_the_next_wait_claims_the_user_first() {
    let (_root, nodes, store, session, wait) = fixture();
    store
        .record_task_result("owner", &session, &result("first"))
        .unwrap();
    let task = receive(&nodes, &wait).await;
    let commit = nodes.artifacts.freeze(&wait, &task).await.unwrap();
    let work = InvocationKey {
        node_id: "agent".into(),
        ..wait.clone()
    };
    let mut work_request = request(&work, vec![commit]);
    work_request.kind = NodeKind::Agent;
    work_request.operation = None;
    assistant::bind_request(nodes.io_resolver.state_root(), &work_request).unwrap();
    store
        .admit_channel_inbound("owner", inbound("urgent"))
        .unwrap();
    assert!(matches!(
        nodes.execute(work_request).await.unwrap(),
        NodeExecutionOutcome::Yielded { .. }
    ));
    let reply = InvocationKey {
        node_id: "reply".into(),
        ..wait.clone()
    };
    let input = store
        .get_assistant_input("owner", &session, &wait.run_id, &wait.durable_key())
        .unwrap()
        .unwrap();
    assistant::bind(
        nodes.io_resolver.state_root(),
        &reply,
        wait.durable_key(),
        &input,
        vec![],
        false,
        None,
    )
    .unwrap();
    let NodeExecutionOutcome::Completed(suppressed) =
        nodes.execute(request(&reply, vec![])).await.unwrap()
    else {
        panic!("interrupted reply did not complete")
    };
    assert_eq!(suppressed.output["suppressed"], true);
    assert!(suppressed.output.get("task_result_handled").is_none());
    assert_eq!(
        store
            .get_turn("owner", &session, task.output["turn"].as_str().unwrap())
            .unwrap()
            .status,
        TurnStatus::Interrupted
    );
    let next = InvocationKey {
        invocation: 3,
        ..wait
    };
    let user = receive(&nodes, &next).await;
    assert_eq!(user.output["source"], "user");
    assert_eq!(user.output["message"], "urgent");
    assert!(nodes.io_resolver.attested_channel(&next).unwrap().is_some());
    assert_eq!(
        store.pending_task_results("owner", &session).unwrap(),
        vec![result("first")]
    );
}
