use super::*;
use anchor_platform_session::{
    ChannelDeliveryRequest, ChannelDeliveryStatus, ChannelIdentity, ChannelInboundRequest,
    SessionStore, TurnStatus,
};
use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot};

mod identity;
mod plugin_updates;

fn kernel_resource(state: &ApiState, id: &str) -> anchor_kernel::Resource {
    use anchor_kernel::Store;
    anchor_kernel::FileStore::new(state.data_root.join("kernel-runs"))
        .unwrap()
        .load(&anchor_runtime::graph::validator::run_resource_id(id))
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn claimed_input_survives_restart_before_artifact_even_after_new_input_arrives() {
    use anchor_runtime::graph::{
        InvocationKey, NodeExecutionOutcome, NodeExecutionPort, NodeExecutionRequest, NodeKind,
    };
    let (root, state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let (sessions, session, _) = saved_assistant(&state, "local");
    let inbound = |id: &str| ChannelInboundRequest {
        inbound_id: id.into(),
        identity: ChannelIdentity {
            conversation_kind: Default::default(),
            source: "wecom".into(),
            account: Some("corp-a".into()),
            conversation_id: "conversation-a".into(),
            sender_id: "user-a".into(),
        },
        graph: "fixture".into(),
        reply_node: "assistant".into(),
        text: Some(id.into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
        bot_origin: None,
        platform_message_id: None,
    };
    let first = sessions
        .admit_channel_inbound("local", inbound("crash-before-artifact"))
        .unwrap();
    let record = fixtures::Reader::new(&state.application)
        .load("persistent-fixture-run")
        .unwrap()
        .unwrap();
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "wait_input".into(),
        invocation: 2,
    };
    sessions
        .claim_channel_assistant_input("local", &session, &record.run_id, &key.durable_key())
        .unwrap()
        .unwrap();
    let newer = sessions
        .admit_channel_inbound("local", inbound("input-arriving-after-crash"))
        .unwrap();
    assert_ne!(first.turn.id, newer.turn.id);
    let request = || NodeExecutionRequest {
        key: key.clone(),
        kind: NodeKind::OpHost,
        model: None,
        task: String::new(),
        instructions: String::new(),
        routes: vec!["assistant".into()],
        input: json!({"session":session}),
        input_commits: Vec::new(),
        plugins: Vec::new(),
        max_provider_requests: None,
        wall_time_limit_seconds: None,
        network: false,
        operation: Some(json!({"operation":"session.wait_input"})),
        writes: Vec::new(),
        cancellation: std::sync::Arc::new(AtomicBool::new(false)),
    };
    let mut completions = Vec::new();
    for _ in 0..2 {
        let control = HostControl {
            cancellation: std::sync::Arc::new(AtomicBool::new(false)),
        };
        let (_, nodes) = crate::make_host_with_control(
            &state.application,
            &record.run_id,
            control,
            Some(state.application.library_root().to_path_buf()),
            None,
        )
        .unwrap();
        let _ = nodes
            .io_resolver
            .run_application
            .set(state.application.clone());
        let NodeExecutionOutcome::Completed(completion) = nodes.execute(request()).await.unwrap()
        else {
            panic!("claimed input did not complete");
        };
        assert_eq!(completion.output["turn"], first.turn.id);
        assert_eq!(completion.output["message"], "crash-before-artifact");
        assert_eq!(completion.model_requests, 0);
        completions.push(completion);
    }
    assert_eq!(completions[0], completions[1]);
    assert_eq!(
        sessions
            .get_channel_assistant_input("local", &session, &record.run_id, &key.durable_key())
            .unwrap()
            .unwrap()
            .relation
            .superseded_by_turn_id,
        Some(newer.turn.id.clone())
    );
    let next_key = InvocationKey {
        invocation: 3,
        ..key
    };
    assert_eq!(
        sessions
            .claim_channel_assistant_input(
                "local",
                &session,
                &record.run_id,
                &next_key.durable_key()
            )
            .unwrap()
            .unwrap()
            .relation
            .turn_id,
        newer.turn.id
    );
}

fn definition() -> Value {
    let mut definition: Value = serde_json::from_str(include_str!(
        "../../../../../examples/graphs/wecom-persistent-assistant.json"
    ))
    .unwrap();
    definition["nodes"][1]["plugins"] = json!([]);
    definition
}

#[tokio::test]
async fn yielded_invocation_projection_never_claims_successful_submission() {
    use anchor_runtime::graph::{
        ArtifactFreezeContext, ArtifactKind, ArtifactPort, InvocationKey, NodeCompletion,
    };
    let (root, state) = fixture();
    let (_, _, _) = saved_assistant(&state, "local");
    let store = fixtures::Reader::new(&state.application);
    let record = store.load("persistent-fixture-run").unwrap().unwrap();
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "assistant".into(),
        invocation: 1,
    };
    let completion = NodeCompletion {
        submission: "superseded by a newer Turn".into(),
        route: Some("reply".into()),
        model_requests: 0,
        output: json!({"interrupted":true,"reason":"superseded by a newer Turn"}),
    };
    let artifacts = HostArtifacts::new(
        state.data_root.join("artifacts"),
        state.workspace_root.clone(),
    );
    let wait_key = InvocationKey {
        node_id: "wait_input".into(),
        ..key.clone()
    };
    std::fs::create_dir_all(artifacts.workspace_path(&wait_key).unwrap()).unwrap();
    let wait_completion = fixtures::completion(json!({}));
    let wait_commit = artifacts.freeze(&wait_key, &wait_completion).await.unwrap();
    let commit = artifacts
        .freeze_with_context(
            &key,
            &completion,
            &ArtifactFreezeContext {
                kind: ArtifactKind::Interruption,
                input_commits: vec![wait_commit.clone()],
            },
        )
        .await
        .unwrap();
    fixtures::control(&state.application, &record.run_id, "resume", "resumed");
    fixtures::complete(
        &state.application,
        &record.run_id,
        "wait_input",
        wait_completion,
        Some(wait_commit),
    );
    fixtures::interrupted(
        &state.application,
        &record.run_id,
        "assistant",
        completion,
        Some(commit),
        "superseded by a newer Turn",
    );
    let (status, projected) = call(
        router_with_web_root(state, root.path().join("web")),
        "GET",
        "/runs/persistent-fixture-run",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{projected}");
    assert_eq!(projected["state"]["nodes"]["assistant"]["submitted"], false);
    assert_eq!(
        projected["state"]["nodes"]["assistant"]["exit_status"],
        "interrupted"
    );
    assert_eq!(
        projected["state"]["nodes"]["assistant"]["interruption"],
        "superseded by a newer Turn"
    );
}

fn saved_assistant(state: &ApiState, owner: &str) -> (SessionStore, String, String) {
    saved_assistant_with_kind(state, owner, Default::default())
}

fn saved_assistant_with_kind(
    state: &ApiState,
    owner: &str,
    kind: anchor_platform_session::ChannelConversationKind,
) -> (SessionStore, String, String) {
    write_graph_bundle(&state.bundle_root, &definition()).unwrap();
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let admission = sessions
        .admit_channel_inbound(
            owner,
            ChannelInboundRequest {
                inbound_id: "persistent-fixture-input".into(),
                identity: ChannelIdentity {
                    conversation_kind: kind,
                    source: "wecom".into(),
                    account: Some("corp-a".into()),
                    conversation_id: "conversation-a".into(),
                    sender_id: "user-a".into(),
                },
                graph: "fixture".into(),
                reply_node: "assistant".into(),
                text: Some("hello".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: true,
                bot_origin: None,
                platform_message_id: None,
            },
        )
        .unwrap();
    let session = admission.session.id;
    let run = "persistent-fixture-run";
    sessions
        .bind_channel_assistant(owner, &session, run, "wait_input", "assistant", "reply")
        .unwrap();
    sessions
        .claim_channel_assistant_input(owner, &session, run, "fixture-wait-key")
        .unwrap()
        .unwrap();
    sessions
        .finish_turn(
            owner,
            &session,
            &admission.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    let mut record = GraphRunRecord::create_with_id(
        GraphSnapshot::from_authoring(definition()).unwrap(),
        json!({"session":session}),
        run,
    )
    .unwrap();
    record.plugin_bindings_initialized = true;
    fixtures::create(&state.application, &record);
    fixtures::control(&state.application, run, "stop", "stopped");
    let mut saved = RunMetadata::new(
        run.into(),
        "fixture".into(),
        record.graph_digest,
        &state.bundle_root,
    )
    .unwrap();
    saved.conversation = Some(metadata::ConversationSource {
        session: session.clone(),
        reply_node: "assistant".into(),
        previous_run: None,
    });
    saved.assistant = Some(metadata::AssistantSource {
        owner: owner.into(),
        session: session.clone(),
        wait_node: "wait_input".into(),
        work_node: "assistant".into(),
        reply_node: "reply".into(),
    });
    metadata::save(&state.data_root, &saved).unwrap();
    (sessions, session, admission.turn.id)
}

#[tokio::test]
async fn group_wait_input_preserves_current_and_interrupted_actors_and_turn_attachment_scope() {
    use anchor_platform_session::{AttachmentManifestEntry, ChannelConversationKind};
    use anchor_runtime::graph::{
        InvocationKey, NodeExecutionOutcome, NodeExecutionPort, NodeExecutionRequest, NodeKind,
    };
    let (root, state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let (sessions, session, _) =
        saved_assistant_with_kind(&state, "local", ChannelConversationKind::Group);
    let record = fixtures::Reader::new(&state.application)
        .load("persistent-fixture-run")
        .unwrap()
        .unwrap();
    let saved = metadata::load(&state.data_root, &record.run_id)
        .unwrap()
        .unwrap();
    let mut admissions = Vec::new();
    for (id, sender, name, bytes) in [
        ("alice-input", "alice", "alice.txt", "YWxpY2U="),
        ("bob-input", "bob", "bob.txt", "Ym9i"),
    ] {
        let uploaded = serde_json::from_value::<crate::channel_inputs::UploadedAttachment>(
            json!({"name":name,"data_base64":bytes,"media_type":"text/plain"}),
        )
        .unwrap();
        let prepared = crate::channel_inputs::prepare(&[uploaded]).unwrap();
        let files = prepared
            .manifest()
            .into_iter()
            .map(|file| AttachmentManifestEntry {
                path: format!("/in/channel/{}", file.name),
                name: file.name,
                sha256: file.sha256,
                size: file.size,
                media_type: file.media_type,
            })
            .collect();
        let admission = sessions
            .admit_channel_inbound(
                "local",
                ChannelInboundRequest {
                    inbound_id: id.into(),
                    identity: ChannelIdentity {
                        source: "wecom".into(),
                        account: Some("corp-a".into()),
                        conversation_kind: ChannelConversationKind::Group,
                        conversation_id: "conversation-a".into(),
                        sender_id: sender.into(),
                    },
                    graph: "fixture".into(),
                    reply_node: "assistant".into(),
                    text: Some(format!("from {sender}")),
                    attachments: anchor_platform_session::AttachmentManifest { format: 1, files },
                    run_id: None,
                    replace_running: true,
                    bot_origin: None,
                    platform_message_id: None,
                },
            )
            .unwrap();
        assert_eq!(admission.session.id, session);
        let input = sessions.get_channel_inbound("local", &session, id).unwrap();
        let selected = crate::assistant::attachment_metadata_for_input(&saved, &input);
        crate::channel_inputs::freeze(&state.data_root, &selected, &prepared).unwrap();
        admissions.push(admission);
    }
    assert_eq!(
        sessions
            .get_turn("local", &session, &admissions[0].turn.id)
            .unwrap()
            .status,
        TurnStatus::Interrupted
    );
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "wait_input".into(),
        invocation: 2,
    };
    let request = NodeExecutionRequest {
        key: key.clone(),
        kind: NodeKind::OpHost,
        model: None,
        task: String::new(),
        instructions: String::new(),
        routes: vec!["assistant".into()],
        input: json!({"session":session}),
        input_commits: Vec::new(),
        plugins: Vec::new(),
        max_provider_requests: None,
        wall_time_limit_seconds: None,
        network: false,
        operation: Some(json!({"operation":"session.wait_input"})),
        writes: Vec::new(),
        cancellation: std::sync::Arc::new(AtomicBool::new(false)),
    };
    let control = HostControl {
        cancellation: std::sync::Arc::new(AtomicBool::new(false)),
    };
    let (_, nodes) = crate::make_host_with_control(
        &state.application,
        &record.run_id,
        control,
        Some(state.application.library_root().to_path_buf()),
        None,
    )
    .unwrap();
    let _ = nodes
        .io_resolver
        .run_application
        .set(state.application.clone());
    let NodeExecutionOutcome::Completed(completion) = nodes.execute(request).await.unwrap() else {
        panic!("group wait did not complete")
    };
    assert_eq!(completion.output["channel"]["sender_id"], "bob");
    assert_eq!(completion.output["channel"]["conversation_kind"], "group");
    assert_eq!(
        completion.output["attachments"]["files"][0]["name"],
        "bob.txt"
    );
    assert_eq!(
        completion.output["interrupted_messages"][0]["channel"]["sender_id"],
        "alice"
    );
    assert_eq!(
        completion.output["interrupted_messages"][0]["channel"]["conversation_kind"],
        "group"
    );
    assert_eq!(
        completion.output["interrupted_messages"][0]["attachments"]["files"][0]["path"],
        format!("/in/channel-pending/{}/alice.txt", admissions[0].turn.id)
    );
    assert_eq!(
        sessions
            .get_channel_assistant("local", &session)
            .unwrap()
            .unwrap()
            .run_id,
        record.run_id
    );
    let selected = crate::assistant::attachment_metadata(&state.data_root, &saved, &key).unwrap();
    assert_eq!(selected.run_id, format!("turn-{}", admissions[1].turn.id));
    assert_eq!(selected.attachments.len(), 1);
    assert_eq!(selected.attachments[0].name, "bob.txt");
    let selected_key = InvocationKey {
        run_id: selected.run_id.clone(),
        ..key.clone()
    };
    let files = crate::channel_inputs::node_inputs(&state.data_root, &selected, &selected_key)
        .unwrap()
        .mount
        .unwrap()
        .source;
    assert_eq!(std::fs::read(files.join("bob.txt")).unwrap(), b"bob");
    assert!(!files.join("alice.txt").exists());
    let mut wrong = selected;
    wrong.attachments = crate::assistant::attachment_metadata_for_input(
        &saved,
        &sessions
            .get_channel_inbound("local", &session, "alice-input")
            .unwrap(),
    )
    .attachments;
    assert!(crate::channel_inputs::node_inputs(&state.data_root, &wrong, &selected_key).is_err());
}

#[tokio::test]
async fn stopped_assistant_rejects_inbound_without_leaving_a_running_turn_or_resuming() {
    let (root, mut state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    with_bot(&mut state, "assistant", &["*"]);
    let (sessions, session, _) = saved_assistant(&state, "local");
    let stopped_resource = kernel_resource(&state, "persistent-fixture-run");
    let binding = sessions.get_channel_assistant("local", &session).unwrap();
    let event = json!({"event":{
        "source":"wecom", "event_id":"stopped-assistant-input", "sender_id":"user-a",
        "conversation_id":"conversation-a", "text":"new input", "reply_target":"conversation-a",
        "message_type":"text", "metadata":{"chat_type":"single","request_id":"stopped-callback"},
        "attachments":[],
    }});
    let inbound_id = super::super::wecom::inbound_id_for("wecom", "stopped-assistant-input");
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    for _ in 0..2 {
        let (status, response) = call(
            app.clone(),
            "POST",
            "/channels/fixture/events",
            Some(&event.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{response}");
        assert_eq!(response["code"], "assistant_stopped");
        let inbound = sessions
            .get_channel_inbound("local", &session, &inbound_id)
            .unwrap();
        assert_eq!(inbound.turn.status, TurnStatus::Failed);
        assert_eq!(inbound.relation.run_id, None);
        assert!(inbound.turn.runs.is_empty());
    }
    assert_eq!(sessions.list_turns("local", &session).unwrap().len(), 2);
    assert_eq!(
        kernel_resource(&state, "persistent-fixture-run"),
        stopped_resource
    );
    assert_eq!(
        sessions.get_channel_assistant("local", &session).unwrap(),
        binding
    );
    assert!(
        !state
            .application
            .run_is_active("persistent-fixture-run")
            .await
    );
    assert!(
        sessions
            .events("local", &session, 0)
            .unwrap()
            .iter()
            .all(|event| event.kind != "channel.assistant_handover")
    );
}

#[test]
fn host_operations_require_platform_admission_and_known_capabilities() {
    let snapshot = GraphSnapshot::admit(definition()).unwrap();
    assert!(crate::reject_snapshot(&snapshot).is_ok());
    assert!(crate::reject_standalone_snapshot(&snapshot).is_err());
    let mut unknown = definition();
    unknown["ops"]["wait_input"]["host"]["operation"] = json!("session.arbitrary");
    let snapshot = GraphSnapshot::admit(unknown).unwrap();
    assert!(crate::reject_snapshot(&snapshot).is_err());
}

#[tokio::test]
async fn unbound_host_operation_is_rejected_before_a_run_is_saved() {
    let (root, state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let snapshot = GraphSnapshot::admit(json!({"objective":"unbound host capability fixture", "entry":"wait", "ops":{"wait":{"host":{"operation":"session.wait_input"}}}, "nodes":[{"id":"wait","op":"wait"}], "edges":[]})).unwrap();
    let record = GraphRunRecord::create_with_id(
        snapshot,
        json!({"session":"forged-session"}),
        "unbound-host-operation",
    )
    .unwrap();
    let control = HostControl {
        cancellation: std::sync::Arc::new(AtomicBool::new(false)),
    };
    let rejected = crate::execution::PreparedExecution::prepare_with_bindings(
        &record,
        control,
        Default::default(),
        state.application.clone(),
        None,
        None,
    );
    assert!(matches!(rejected, Err(message) if message.contains("explicitly admitted assistant")));
    assert!(
        fixtures::Reader::new(&state.application)
            .load(&record.run_id)
            .unwrap()
            .is_none()
    );
    assert!(!state.data_root.join("platform/sessions.sqlite").exists());
}

#[tokio::test]
async fn pending_input_scope_reports_truncation_and_authorizes_only_the_latest_eight() {
    use anchor_runtime::graph::{
        InvocationKey, NodeExecutionOutcome, NodeExecutionPort, NodeExecutionRequest, NodeKind,
    };
    let (root, state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let (sessions, session, _) = saved_assistant(&state, "local");
    let inbound = |id: &str| ChannelInboundRequest {
        inbound_id: id.into(),
        identity: ChannelIdentity {
            conversation_kind: Default::default(),
            source: "wecom".into(),
            account: Some("corp-a".into()),
            conversation_id: "conversation-a".into(),
            sender_id: "user-a".into(),
        },
        graph: "fixture".into(),
        reply_node: "assistant".into(),
        text: Some(id.into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
        bot_origin: None,
        platform_message_id: None,
    };
    for index in 0..9 {
        let admission = sessions
            .admit_channel_inbound("local", inbound(&format!("pending-{index}")))
            .unwrap();
        sessions
            .finish_turn(
                "local",
                &session,
                &admission.turn.id,
                TurnStatus::Interrupted,
                None,
            )
            .unwrap();
    }
    let current = sessions
        .admit_channel_inbound("local", inbound("current-input"))
        .unwrap();
    let record = fixtures::Reader::new(&state.application)
        .load("persistent-fixture-run")
        .unwrap()
        .unwrap();
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "wait_input".into(),
        invocation: 2,
    };
    let request = NodeExecutionRequest {
        key: key.clone(),
        kind: NodeKind::OpHost,
        model: None,
        task: String::new(),
        instructions: String::new(),
        routes: vec!["assistant".into()],
        input: json!({"session":session}),
        input_commits: Vec::new(),
        plugins: Vec::new(),
        max_provider_requests: None,
        wall_time_limit_seconds: None,
        network: false,
        operation: Some(json!({"operation":"session.wait_input"})),
        writes: Vec::new(),
        cancellation: std::sync::Arc::new(AtomicBool::new(false)),
    };
    let control = HostControl {
        cancellation: std::sync::Arc::new(AtomicBool::new(false)),
    };
    let (_, nodes) = crate::make_host_with_control(
        &state.application,
        &record.run_id,
        control,
        Some(state.application.library_root().to_path_buf()),
        None,
    )
    .unwrap();
    let _ = nodes
        .io_resolver
        .run_application
        .set(state.application.clone());
    let NodeExecutionOutcome::Completed(completion) = nodes.execute(request).await.unwrap() else {
        panic!("waiting input did not complete");
    };
    assert_eq!(completion.output["turn"], current.turn.id);
    assert_eq!(completion.output["interrupted_messages_truncated"], true);
    assert_eq!(completion.model_requests, 0);
    assert_eq!(
        completion.output["interrupted_messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["message"].as_str().unwrap())
            .collect::<Vec<_>>(),
        (1..9)
            .map(|index| format!("pending-{index}"))
            .collect::<Vec<_>>()
    );
    let binding = crate::assistant::binding(&state.data_root, &key)
        .unwrap()
        .unwrap();
    assert!(binding.pending_truncated);
    assert_eq!(
        binding.pending_inbounds,
        (1..9)
            .map(|index| format!("pending-{index}"))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn assistant_retirement_preserves_history_and_rejects_resume() {
    let (root, state) = fixture();
    let (sessions, session, _) = saved_assistant(&state, "local");
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let route = format!("/channel-sessions/{session}/assistant");
    let (status, projection) = call(app.clone(), "GET", &route, None).await;
    assert_eq!(status, StatusCode::OK, "{projection}");
    assert_eq!(projection["assistant"]["run_id"], "persistent-fixture-run");
    let body = json!({"run_id":"persistent-fixture-run"}).to_string();
    for _ in 0..2 {
        let (status, retired) =
            call(app.clone(), "POST", &format!("{route}/retire"), Some(&body)).await;
        assert_eq!(status, StatusCode::OK, "{retired}");
    }
    assert!(
        sessions
            .get_channel_assistant("local", &session)
            .unwrap()
            .is_none()
    );
    assert!(
        sessions
            .get_channel_assistant_input(
                "local",
                &session,
                "persistent-fixture-run",
                "fixture-wait-key"
            )
            .unwrap()
            .is_some()
    );
    let (status, rejected) = call(app, "POST", "/runs/persistent-fixture-run/resume", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert!(
        state
            .application
            .records()
            .unwrap()
            .iter()
            .any(|(run, _)| run == "persistent-fixture-run")
    );
}

#[tokio::test]
async fn current_assistant_instance_is_retained_until_it_is_retired() {
    let (root, state) = fixture();
    let (_, session, _) = saved_assistant(&state, "local");
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    // The Session still points at this instance, so its record stays even though
    // nothing newer reads it.
    let (status, rejected) =
        call(app.clone(), "DELETE", "/runs/persistent-fixture-run", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert!(
        rejected["error"]
            .as_str()
            .unwrap()
            .contains("current assistant instance")
    );
    let route = format!("/channel-sessions/{session}/assistant");
    let body = json!({"run_id":"persistent-fixture-run"}).to_string();
    let (status, retired) =
        call(app.clone(), "POST", &format!("{route}/retire"), Some(&body)).await;
    assert_eq!(status, StatusCode::OK, "{retired}");
    // A retired instance is the oldest surviving link of its chain and nothing
    // newer reads it, so its history can be cleaned up.
    let (status, deleted) = call(app.clone(), "DELETE", "/runs/persistent-fixture-run", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    assert!(
        crate::run_deletions::load(&state.data_root, "persistent-fixture-run")
            .unwrap()
            .is_some()
    );
    assert!(state.application.records().unwrap().is_empty());
}

#[tokio::test]
async fn assistant_retirement_is_owner_scoped_and_does_not_accept_forged_runs() {
    let (root, state) = fixture();
    let (sessions, session, _) = saved_assistant(&state, "another-owner");
    let app = router_with_web_root(state, root.path().join("web"));
    let (status, response) = call(
        app.clone(),
        "GET",
        &format!("/channel-sessions/{session}/assistant"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{response}");
    let (status, response) = call(
        app,
        "POST",
        &format!("/channel-sessions/{session}/assistant/retire"),
        Some(&json!({"run_id":"persistent-fixture-run"}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{response}");
    assert!(
        sessions
            .get_channel_assistant("another-owner", &session)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn unknown_delivery_blocks_retirement_and_graph_delete_until_settled() {
    let (root, state) = fixture();
    let (sessions, session, turn) = saved_assistant(&state, "local");
    let request = ChannelDeliveryRequest {
        key: "pending-reply".into(),
        kind: "wecom.reply".into(),
        content_sha256: "a".repeat(64),
    };
    sessions
        .admit_channel_delivery("local", &session, &turn, request.clone())
        .unwrap();
    sessions
        .begin_channel_delivery("local", &session, &turn, request.clone())
        .unwrap();
    sessions
        .settle_channel_delivery(
            "local",
            &session,
            request.clone(),
            ChannelDeliveryStatus::Unknown,
            Some("ACK missing"),
        )
        .unwrap();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let body = json!({"run_id":"persistent-fixture-run"}).to_string();
    for (method, uri, input) in [
        (
            "POST",
            format!("/channel-sessions/{session}/assistant/retire"),
            Some(body.as_str()),
        ),
        ("DELETE", "/graphs/fixture".into(), None),
    ] {
        let (status, response) = call(app.clone(), method, &uri, input).await;
        assert_eq!(status, StatusCode::CONFLICT, "{response}");
    }
    assert!(state.bundle_root.exists());
    assert!(
        sessions
            .get_channel_assistant("local", &session)
            .unwrap()
            .is_some()
    );
    sessions
        .settle_channel_delivery(
            "local",
            &session,
            request,
            ChannelDeliveryStatus::Confirmed,
            None,
        )
        .unwrap();
    let (status, response) = call(app, "DELETE", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{response}");
    assert!(!state.bundle_root.exists());
    assert!(
        sessions
            .get_channel_assistant("local", &session)
            .unwrap()
            .is_none()
    );
    assert!(state.application.records().unwrap().is_empty());
}

#[tokio::test]
async fn a_bot_saved_enabled_keeps_its_graph_history_even_when_not_running() {
    use crate::application::channel_management::fake::FakeChannels;
    let (root, mut state) = fixture();
    let (sessions, session, _) = saved_assistant(&state, "local");
    // Saved enabled, but not running: its credentials name another account.
    let mut bot = FakeChannels::bot("fixture", "local", "assistant", &["*"]);
    bot.enabled = false;
    state.application = state
        .application
        .clone()
        .with_channel_management(Arc::new(FakeChannels::new().with(bot)));
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let (status, response) = call(app, "DELETE", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert!(state.bundle_root.exists());
    assert!(
        sessions
            .get_channel_assistant("local", &session)
            .unwrap()
            .is_some(),
        "deletion must be refused before any history is touched"
    );
    assert!(!state.application.records().unwrap().is_empty());
}

/// `needs_recovery` is derived from liveness, so a handover whose admission has
/// not run yet — a bound instance with no Run record — still reads as needing
/// recovery, while an unbound Session does not.
#[tokio::test]
async fn assistant_projection_reports_derived_needs_recovery() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let owner = "local";
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let admission = sessions
        .admit_channel_inbound(
            owner,
            ChannelInboundRequest {
                inbound_id: "assistant-recovery-input".into(),
                identity: ChannelIdentity {
                    conversation_kind: Default::default(),
                    source: "wecom".into(),
                    account: None,
                    conversation_id: "conversation-a".into(),
                    sender_id: "user-a".into(),
                },
                graph: "fixture".into(),
                reply_node: "assistant".into(),
                text: Some("hello".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: true,
                bot_origin: None,
                platform_message_id: None,
            },
        )
        .unwrap();
    let session = admission.session.id.clone();

    let (status, body) = call(
        app.clone(),
        "GET",
        &format!("/channel-sessions/{session}/assistant"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["assistant"].is_null(), "{body}");
    assert_eq!(body["needs_recovery"], json!(false));

    sessions
        .bind_channel_assistant(
            owner,
            &session,
            "assistant-missing",
            "wait_input",
            "assistant",
            "reply",
        )
        .unwrap();
    let (status, body) = call(
        app,
        "GET",
        &format!("/channel-sessions/{session}/assistant"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["assistant"]["run_id"], "assistant-missing");
    assert_eq!(body["needs_recovery"], json!(true));
}

fn recovery_inbound(identity: &str) -> ChannelInboundRequest {
    ChannelInboundRequest {
        inbound_id: identity.into(),
        identity: ChannelIdentity {
            conversation_kind: Default::default(),
            source: "wecom".into(),
            account: Some("corp-a".into()),
            conversation_id: "conversation-a".into(),
            sender_id: "user-a".into(),
        },
        graph: "fixture".into(),
        reply_node: "assistant".into(),
        text: Some(identity.into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
        bot_origin: None,
        platform_message_id: None,
    }
}

fn run_store(state: &ApiState) -> fixtures::Reader<'_> {
    fixtures::Reader::new(&state.application)
}

fn wait_key(run: &str, digest: &str) -> String {
    anchor_runtime::graph::InvocationKey {
        run_id: run.into(),
        graph_digest: digest.into(),
        node_id: "wait_input".into(),
        invocation: 1,
    }
    .durable_key()
}

fn auto_resume_events(
    sessions: &SessionStore,
    owner: &str,
    session: &str,
) -> Vec<std::collections::BTreeMap<String, Value>> {
    sessions
        .events(owner, session, 0)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "channel.assistant_auto_resume")
        .map(|event| event.data)
        .collect()
}

/// A restart finds a bound instance whose Run is persisted as running with
/// nothing executing it, reconciles that stale Run, and gives the Session a new
/// instance of the same Graph — carrying the claimed but uncommitted Turn.
#[tokio::test]
async fn restart_hands_a_live_assistant_instance_over_and_carries_its_round() {
    let (root, state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let (sessions, session, _) = saved_assistant(&state, "local");
    fixtures::control(
        &state.application,
        "persistent-fixture-run",
        "resume",
        "resumed",
    );
    let carried = sessions
        .admit_channel_inbound("local", recovery_inbound("recovery-carried-input"))
        .unwrap();
    sessions
        .claim_channel_assistant_input(
            "local",
            &session,
            "persistent-fixture-run",
            "crashed-wait-key",
        )
        .unwrap()
        .unwrap();

    crate::api::assistant_recovery::resume_channel_assistants(&state)
        .await
        .unwrap();

    let binding = sessions
        .get_channel_assistant("local", &session)
        .unwrap()
        .unwrap();
    let target = binding.run_id.clone();
    assert!(target.starts_with("assistant-resume-"), "{binding:?}");
    assert_eq!(binding.wait_node, "wait_input");
    assert_eq!(binding.work_node, "assistant");
    assert_eq!(binding.reply_node, "reply");
    assert!(
        sessions
            .retired_channel_assistant("local", &session, "persistent-fixture-run")
            .unwrap()
    );
    // The stale Run was reconciled into a settled predecessor, not revived.
    assert!(
        run_store(&state)
            .load("persistent-fixture-run")
            .unwrap()
            .unwrap()
            .waits_reason("stopped")
    );
    // The claimed round travelled with the instance and answers the new Run's
    // wait key; the retired Run can no longer retrieve it. The new Run owns its
    // own Graph digest, which is the identity of the bundle this Host loaded.
    let target_digest = state
        .application
        .metadata(&target)
        .unwrap()
        .unwrap()
        .graph_digest;
    let moved = sessions
        .get_channel_assistant_input(
            "local",
            &session,
            &target,
            &wait_key(&target, &target_digest),
        )
        .unwrap()
        .unwrap();
    assert_eq!(moved.relation.turn_id, carried.turn.id);
    assert_eq!(moved.relation.inbound_id, "recovery-carried-input");
    assert_eq!(moved.relation.run_id.as_deref(), Some(target.as_str()));
    assert!(
        sessions
            .get_channel_assistant_input(
                "local",
                &session,
                "persistent-fixture-run",
                "crashed-wait-key"
            )
            .unwrap()
            .is_none()
    );
    // Admission cannot complete without a configured Agent runtime, so the
    // instance is not live yet; the automatic event only describes a completed
    // recovery and must not have been written.
    assert!(auto_resume_events(&sessions, "local", &session).is_empty());
    assert!(
        sessions
            .events("local", &session, 0)
            .unwrap()
            .iter()
            .any(|event| event.kind == "channel.assistant_handover"
                && event.data["from"] == json!("persistent-fixture-run")
                && event.data["to"] == json!(target))
    );

    // A second restart must not move the instance again: the recovery target is
    // the binding it already handed over to.
    crate::api::assistant_recovery::resume_channel_assistants(&state)
        .await
        .unwrap();
    assert_eq!(
        sessions
            .get_channel_assistant("local", &session)
            .unwrap()
            .unwrap()
            .run_id,
        target
    );
    assert_eq!(
        sessions
            .channel_assistant_predecessor("local", &session, &target)
            .unwrap()
            .unwrap()
            .run_id,
        "persistent-fixture-run"
    );
    assert_eq!(sessions.list_channel_assistants().unwrap().len(), 1);
}

/// The window a crash leaves between the handover and its admission: the target
/// is already current with no Run facts. Recovery must admit that same Run id.
#[tokio::test]
async fn restart_adopts_a_committed_handover_whose_admission_never_ran() {
    let (root, state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let (sessions, session, _) = saved_assistant(&state, "local");
    let stale = run_store(&state)
        .load("persistent-fixture-run")
        .unwrap()
        .unwrap();
    fixtures::control(
        &state.application,
        "persistent-fixture-run",
        "resume",
        "resumed",
    );
    let target = "assistant-resume-pending";
    sessions
        .handover_channel_assistant(
            "local",
            &session,
            "persistent-fixture-run",
            target,
            &wait_key(target, &stale.graph_digest),
        )
        .unwrap();
    assert!(state.application.metadata(target).unwrap().is_none());

    crate::api::assistant_recovery::resume_channel_assistants(&state)
        .await
        .unwrap();

    // The retry admits the Run the handover already named instead of handing the
    // instance over a second time.
    assert_eq!(
        sessions
            .get_channel_assistant("local", &session)
            .unwrap()
            .unwrap()
            .run_id,
        target
    );
    assert_eq!(
        sessions
            .channel_assistant_predecessor("local", &session, target)
            .unwrap()
            .unwrap()
            .run_id,
        "persistent-fixture-run"
    );
    assert!(sessions.list_channel_assistants().unwrap().len() == 1);
    assert!(
        run_store(&state)
            .load("persistent-fixture-run")
            .unwrap()
            .unwrap()
            .waits_reason("stopped")
    );
    // The interrupted admission left its immutable metadata behind; the next
    // recovery must keep the same target and report it instead of churning.
    let admitted = state.application.metadata(target).unwrap().unwrap();
    assert_eq!(
        admitted
            .conversation
            .as_ref()
            .unwrap()
            .previous_run
            .as_deref(),
        Some("persistent-fixture-run")
    );
    crate::api::assistant_recovery::resume_channel_assistants(&state)
        .await
        .unwrap();
    assert_eq!(
        sessions
            .get_channel_assistant("local", &session)
            .unwrap()
            .unwrap()
            .run_id,
        target
    );
    assert!(sessions.list_channel_assistants().unwrap().len() == 1);
}

/// Retired instances stay down and unbound Sessions stay unbound.
#[tokio::test]
async fn automatic_recovery_ignores_retired_and_unbound_sessions() {
    let (root, state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let (sessions, session, _) = saved_assistant(&state, "local");
    sessions
        .retire_channel_assistant("local", &session, "persistent-fixture-run")
        .unwrap();
    let unbound = sessions
        .admit_channel_inbound("local", recovery_inbound("recovery-unbound-input"))
        .unwrap();

    crate::api::assistant_recovery::resume_channel_assistants(&state)
        .await
        .unwrap();

    // A hand-retired instance is history, not a crash.
    assert!(
        sessions
            .get_channel_assistant("local", &session)
            .unwrap()
            .is_none()
    );
    assert!(
        sessions
            .get_channel_assistant("local", &unbound.session.id)
            .unwrap()
            .is_none()
    );
    assert!(sessions.list_channel_assistants().unwrap().is_empty());
    assert!(
        run_store(&state)
            .load("persistent-fixture-run")
            .unwrap()
            .unwrap()
            .waits_reason("stopped")
    );
    assert!(auto_resume_events(&sessions, "local", &session).is_empty());
    assert!(auto_resume_events(&sessions, "local", &unbound.session.id).is_empty());
}

/// The Host process can turn automatic recovery off; nothing then moves.
#[tokio::test]
async fn automatic_recovery_can_be_disabled_by_environment() {
    let (root, state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let (sessions, session, _) = saved_assistant(&state, "local");
    unsafe { env::set_var(crate::api::assistant_recovery::AUTO_RESUME_ENV, "0") };
    let resumed = crate::api::assistant_recovery::resume_channel_assistants(&state).await;
    unsafe { env::remove_var(crate::api::assistant_recovery::AUTO_RESUME_ENV) };
    resumed.unwrap();

    assert_eq!(
        sessions
            .get_channel_assistant("local", &session)
            .unwrap()
            .unwrap()
            .run_id,
        "persistent-fixture-run"
    );
    assert!(
        !sessions
            .retired_channel_assistant("local", &session, "persistent-fixture-run")
            .unwrap()
    );
    assert!(auto_resume_events(&sessions, "local", &session).is_empty());
}

/// A resident instance keeps one Run while its Session lives, so the board has to
/// place it by the Turns it actually worked instead of by its whole lifetime.
#[tokio::test]
async fn timeline_places_a_resident_run_by_its_turn_windows() {
    let (root, state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let (sessions, session, turn) = saved_assistant(&state, "local");
    let turn = sessions.get_turn("local", &session, &turn).unwrap();

    let run = "persistent-fixture-run";
    let resident = |timeline: &Value| {
        timeline["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["run"] == run)
            .cloned()
            .unwrap_or_else(|| panic!("Run {run} is missing from {timeline}"))
    };
    let app = router(state.clone());
    let (status, timeline) = call(app, "GET", "/timeline?days=30", None).await;
    assert_eq!(status, StatusCode::OK);
    let projected = resident(&timeline);
    let activity = projected["activity"].as_array().unwrap();
    assert_eq!(activity.len(), 1, "{projected}");
    assert_eq!(
        activity[0]["start"],
        turn.created_at
            .with_timezone(&chrono::Local)
            .naive_local()
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    );
    assert_eq!(activity[0]["running"], false);
    // The window is one Turn, not the Run's own lifetime: it ends where the Turn did.
    assert_eq!(
        activity[0]["end"],
        turn.updated_at
            .with_timezone(&chrono::Local)
            .naive_local()
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    );

    // A resident Run outlives the page that holds its `started`, so the board must
    // keep it while it still worked inside the page.
    let mut saved = state.application.metadata(run).unwrap().unwrap();
    saved.created = (chrono::Utc::now() - chrono::Duration::days(40)).to_rfc3339();
    crate::run_metadata::save(&state.data_root, &saved).unwrap();
    let app = router(state.clone());
    let (status, history) = call(app, "GET", "/timeline?days=30", None).await;
    assert_eq!(status, StatusCode::OK);
    let projected = resident(&history);
    assert_eq!(projected["activity"].as_array().unwrap().len(), 1);

    // The same Run on an older page keeps its windows but no longer appears.
    let app = router(state);
    let (status, older) = call(app, "GET", "/timeline?days=30&before=2026-01-01", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        older["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["run"] != run)
    );
}

/// An idle handover admits a fresh Run before any Turn is claimed. The board has
/// to know it is still a resident instance, otherwise it falls back to drawing
/// the Run from `started` to now — the very line this projection removed.
#[tokio::test]
async fn timeline_marks_an_idle_resident_run_without_turn_windows() {
    let (root, state) = fixture();
    let _environment = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let (sessions, _, _) = saved_assistant(&state, "local");
    let idle = "idle-fixture-run";
    let session = sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: "idle-fixture-input".into(),
                identity: ChannelIdentity {
                    conversation_kind: Default::default(),
                    source: "wecom".into(),
                    account: Some("corp-a".into()),
                    conversation_id: "conversation-b".into(),
                    sender_id: "user-b".into(),
                },
                graph: "fixture".into(),
                reply_node: "assistant".into(),
                text: Some("hello".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: true,
                bot_origin: None,
                platform_message_id: None,
            },
        )
        .unwrap()
        .session
        .id;
    // Bound and running, but no Turn has been claimed yet.
    sessions
        .bind_channel_assistant("local", &session, idle, "wait_input", "assistant", "reply")
        .unwrap();
    let mut record = GraphRunRecord::create_with_id(
        GraphSnapshot::admit(definition()).unwrap(),
        json!({"session":session}),
        idle,
    )
    .unwrap();
    record.plugin_bindings_initialized = true;
    fixtures::create(&state.application, &record);
    let mut saved = RunMetadata::new(
        idle.into(),
        "fixture".into(),
        record.graph_digest,
        &state.bundle_root,
    )
    .unwrap();
    saved.conversation = Some(metadata::ConversationSource {
        session: session.clone(),
        reply_node: "assistant".into(),
        previous_run: None,
    });
    saved.assistant = Some(metadata::AssistantSource {
        owner: "local".into(),
        session,
        wait_node: "wait_input".into(),
        work_node: "assistant".into(),
        reply_node: "reply".into(),
    });
    metadata::save(&state.data_root, &saved).unwrap();

    let app = router(state.clone());
    let (status, timeline) = call(app, "GET", "/timeline?days=30", None).await;
    assert_eq!(status, StatusCode::OK);
    let idle_run = timeline["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["run"] == idle)
        .unwrap_or_else(|| panic!("Run {idle} is missing from {timeline}"));
    assert_eq!(idle_run["activity"], json!([]), "{idle_run}");
    assert_eq!(idle_run["running"], false);
}
