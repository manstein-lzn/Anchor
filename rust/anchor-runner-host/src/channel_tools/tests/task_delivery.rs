use super::*;

#[tokio::test]
async fn task_result_files_can_be_sent_without_granting_siblings_or_reply_images() {
    use crate::{
        application::channel_management::fake::FakeChannels,
        goose_tool_context::{GooseToolIdentity, scope},
        run_metadata::{AssistantSource, RunMetadata},
    };
    use anchor_platform_session::{
        ChannelIdentity, ChannelInboundRequest, SessionStore, TaskResult, TurnStatus,
    };
    use anchor_runtime::graph::{ArtifactPort, NodeCompletion};

    let root = tempfile::tempdir().unwrap();
    let mut host = tools(root.path());
    host.key.invocation = 2;
    fs::create_dir_all(root.path().join("platform")).unwrap();
    let sessions = SessionStore::open(root.path().join("platform/sessions.sqlite")).unwrap();
    let request = ChannelInboundRequest {
        inbound_id: "initial".into(),
        identity: ChannelIdentity {
            source: "wecom".into(),
            account: Some("corp-a".into()),
            conversation_kind: Default::default(),
            conversation_id: "alice".into(),
            sender_id: "alice".into(),
        },
        graph: "graph".into(),
        reply_node: "reply".into(),
        text: Some("initial".into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
    };
    let admitted = sessions
        .admit_channel_inbound("local", request.clone())
        .unwrap();
    let session = admitted.session.id;
    sessions
        .bind_channel_assistant(
            "local",
            &session,
            &host.key.run_id,
            "wait",
            "reply",
            "host-reply",
        )
        .unwrap();
    sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "initial")
        .unwrap();
    sessions
        .finish_turn(
            "local",
            &session,
            &admitted.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    let mut metadata = RunMetadata::new(
        host.key.run_id.clone(),
        "graph".into(),
        host.key.graph_digest.clone(),
        root.path(),
    )
    .unwrap();
    metadata.oauth_owner = Some("local".into());
    metadata.assistant = Some(AssistantSource {
        owner: "local".into(),
        session: session.clone(),
        wait_node: "wait".into(),
        work_node: "reply".into(),
        reply_node: "host-reply".into(),
    });
    crate::run_metadata::save(root.path(), &metadata).unwrap();

    let artifacts =
        crate::HostArtifacts::new(root.path().join("artifacts"), root.path().join("producing"));
    let producer = anchor_runtime::graph::InvocationKey {
        run_id: "background-run".into(),
        node_id: "produce".into(),
        ..host.key.clone()
    };
    let workspace = artifacts.prepare_workspace(&producer, &[]).unwrap();
    fs::write(workspace.join("report.md"), "selected-background-report").unwrap();
    fs::write(workspace.join("unrelated.txt"), "not-selected").unwrap();
    let commit = artifacts
        .freeze(
            &producer,
            &NodeCompletion {
                submission: "report".into(),
                route: None,
                model_requests: 0,
                output: json!({"files":["report.md","unrelated.txt"]}),
            },
        )
        .await
        .unwrap();
    let selected = anchor_platform_session::ChannelNotificationFile {
        artifact_id: commit.id.clone(),
        path: "report.md".into(),
        name: "briefing.md".into(),
    };
    sessions
        .record_task_result(
            "local",
            &session,
            &TaskResult {
                id: "background".into(),
                source_run: Some(producer.run_id),
                outcome: "done".into(),
                content: json!({"summary":"report ready"}),
                files: vec![selected.clone()],
            },
        )
        .unwrap();
    let input = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "task-wait")
        .unwrap()
        .unwrap();
    crate::assistant::bind(
        root.path(),
        &host.key,
        "task-wait".into(),
        &input,
        vec![],
        false,
        None,
    )
    .unwrap();
    let view = artifacts.files_path(&commit).unwrap();
    let path = "/in/tasks/background/briefing.md";
    host.readonly_inputs = vec![ReadOnlyInput::new(view.join("report.md"), path)];
    let (file, bytes) = host.notification_file(path).unwrap();
    assert_eq!(file, selected);
    assert_eq!(bytes, b"selected-background-report");
    for hidden in [
        "/in/tasks/background/unrelated.txt",
        "/in/tasks/background/briefing.md/../unrelated.txt",
        "/in/tasks/another/briefing.md",
    ] {
        assert!(host.notification_file(hidden).is_err(), "{hidden}");
    }
    let mounted = host.readonly_inputs.clone();
    host.readonly_inputs = vec![ReadOnlyInput::new(&view, "/in/tasks/background")];
    assert!(host.notification_file(path).is_err());
    assert!(
        host.notification_file("/in/tasks/background/unrelated.txt")
            .is_err()
    );
    let uploaded = root.path().join("uploaded.md");
    fs::write(&uploaded, &bytes).unwrap();
    host.readonly_inputs = vec![ReadOnlyInput::new(uploaded, path)];
    assert!(host.notification_file(path).is_err());
    host.readonly_inputs = mounted;
    assert!(
        !host
            .definitions()
            .iter()
            .any(|definition| definition.name == IMAGE_TOOL)
    );
    assert!(matches!(
        host.call(IMAGE_TOOL, json!({"path":"image.png"})).await,
        Err(ToolError::Unknown(_))
    ));
    assert!(host.reply_path().is_err());

    let gateway = task_gateway::TaskGateway::start(root.path()).await;
    let mut bot = FakeChannels::bot("graph", "local", "reply", &["alice", "bob"]);
    bot.descriptor = Some(gateway.descriptor.clone());
    host.application = host
        .application
        .with_channel_management(Arc::new(FakeChannels::new().with(bot)));
    let before_send = gateway.frames();
    let args = json!({"path":path,"content":"Your background report is ready"});
    assert_eq!(
        host.definitions()[0].parameters["required"],
        json!(["content"])
    );
    assert_eq!(
        host.definitions()[1].parameters["required"],
        json!(["path", "content"])
    );
    for invalid in [
        json!({"userid":"bob","path":path,"content":"private report"}),
        json!({"bot_graph":"another-bot","path":path,"content":"private report"}),
    ] {
        assert!(host.call(SEND_FILE_TOOL, invalid).await.is_err());
    }
    assert!(
        host.call(
            SEND_TOOL,
            json!({"userid":"bob","content":"private result"})
        )
        .await
        .is_err()
    );
    assert_eq!(gateway.frames(), before_send);
    assert!(host.call(SEND_FILE_TOOL, args.clone()).await.is_err()); // Native identity is still required.
    let sent = scope(
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "native-fixture".into(),
            tool_call: "task-report-send".into(),
        }),
        host.call(SEND_FILE_TOOL, args.clone()),
    )
    .await
    .unwrap()[0]
        .as_json()
        .unwrap()
        .clone();
    assert_eq!(sent["accepted"], true);
    assert_eq!(sent["status"], "confirmed");
    assert_eq!(
        sent["file"]["request_id"],
        format!("{}-file", sent["request_id"].as_str().unwrap())
    );
    assert_eq!(
        sent["introduction"]["request_id"],
        format!("{}-intro", sent["request_id"].as_str().unwrap())
    );
    let frames = gateway.frames();
    assert_eq!(frames[0]["cmd"], "aibot_upload_media_init");
    assert_eq!(frames[0]["body"]["filename"], "report.md");
    assert_eq!(
        STANDARD
            .decode(frames[1]["body"]["base64_data"].as_str().unwrap())
            .unwrap(),
        bytes
    );
    assert_eq!(gateway.gateway.delivery_facts().unwrap().len(), 2);
    let notification = sessions
        .latest_channel_notification("local", &session)
        .unwrap()
        .unwrap();
    assert_eq!(notification.files, vec![selected.clone()]);
    assert_eq!(notification.source_run, "background-run");

    // A user interrupts this result after its effects completed. A new Turn
    // and native call must read both ACKs back instead of sending it again.
    sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: "urgent".into(),
                ..request.clone()
            },
        )
        .unwrap();
    assert!(host.call(SEND_FILE_TOOL, args.clone()).await.is_err());
    let urgent = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "urgent-wait")
        .unwrap()
        .unwrap();
    sessions
        .finish_turn(
            "local",
            &session,
            &urgent.turn().id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    host.key.invocation = 3;
    let input = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "task-retry")
        .unwrap()
        .unwrap();
    crate::assistant::bind(
        root.path(),
        &host.key,
        "task-retry".into(),
        &input,
        vec![],
        false,
        None,
    )
    .unwrap();
    let replayed = scope(
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "new-native-session".into(),
            tool_call: "new-tool-call".into(),
        }),
        host.call(SEND_FILE_TOOL, args.clone()),
    )
    .await
    .unwrap()[0]
        .as_json()
        .unwrap()
        .clone();
    assert_eq!(replayed["request_id"], sent["request_id"]);
    assert_eq!(replayed["file"]["duplicate"], true);
    assert_eq!(replayed["introduction"]["duplicate"], true);
    assert_eq!(gateway.frames(), frames);
    let changed = scope(
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "new-native-session".into(),
            tool_call: "changed-content".into(),
        }),
        host.call(
            SEND_FILE_TOOL,
            json!({"path":path,"content":"rewritten introduction"}),
        ),
    )
    .await;
    assert!(
        changed
            .unwrap_err()
            .to_string()
            .contains("notification identity changed")
    );
    assert_eq!(gateway.frames(), frames);
    sessions
        .finish_task_result("local", &session, &host.key.run_id, "task-retry")
        .unwrap();

    sessions
        .record_task_result(
            "local",
            &session,
            &TaskResult {
                id: "unknown-introduction".into(),
                source_run: Some("background-run".into()),
                outcome: "done".into(),
                content: json!({}),
                files: vec![selected.clone()],
            },
        )
        .unwrap();
    host.key.invocation = 4;
    let input = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "unknown-wait")
        .unwrap()
        .unwrap();
    crate::assistant::bind(
        root.path(),
        &host.key,
        "unknown-wait".into(),
        &input,
        vec![],
        false,
        None,
    )
    .unwrap();
    let unknown_path = "/in/tasks/unknown-introduction/briefing.md";
    host.readonly_inputs = vec![ReadOnlyInput::new(view.join("report.md"), unknown_path)];
    let unknown_args = json!({"path":unknown_path,"content":"This introduction's ACK is withheld"});
    gateway.withhold_introductions(true);
    let partial = scope(
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "native-partial".into(),
            tool_call: "first-attempt".into(),
        }),
        host.call(SEND_FILE_TOOL, unknown_args.clone()),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        partial.contains("confirmed and saved for the next Turn; introduction"),
        "{partial}"
    );
    let partial_frames = gateway.frames();
    let facts = gateway.gateway.delivery_facts().unwrap();
    assert_eq!(facts.len(), 4);
    assert_eq!(
        facts
            .iter()
            .filter(|fact| fact.status == anchor_wecom_gateway::DeliveryStatus::Unknown)
            .count(),
        1
    );
    let notification = sessions
        .latest_channel_notification("local", &session)
        .unwrap()
        .unwrap();
    assert_eq!(notification.files, vec![selected.clone()]);

    sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: "interrupt-partial".into(),
                ..request.clone()
            },
        )
        .unwrap();
    let urgent = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "partial-user")
        .unwrap()
        .unwrap();
    sessions
        .finish_turn(
            "local",
            &session,
            &urgent.turn().id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    host.key.invocation = 5;
    let input = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "partial-retry")
        .unwrap()
        .unwrap();
    crate::assistant::bind(
        root.path(),
        &host.key,
        "partial-retry".into(),
        &input,
        vec![],
        false,
        None,
    )
    .unwrap();
    let repeated = scope(
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "another-native-session".into(),
            tool_call: "another-attempt".into(),
        }),
        host.call(SEND_FILE_TOOL, unknown_args),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        repeated.contains("previous delivery is unconfirmed"),
        "{repeated}"
    );
    assert_eq!(gateway.frames(), partial_frames);
    assert_eq!(gateway.gateway.delivery_facts().unwrap(), facts);
    sessions
        .finish_task_result("local", &session, &host.key.run_id, "partial-retry")
        .unwrap();

    // Admission failures have no producing Run or files. They can still notify
    // this private conversation without inventing a source Run or recipient.
    sessions
        .record_task_result(
            "local",
            &session,
            &TaskResult {
                id: "admission-blocked".into(),
                source_run: None,
                outcome: "blocked".into(),
                content: json!({"event":"schedule.blocked"}),
                files: vec![],
            },
        )
        .unwrap();
    host.key.invocation = 6;
    let input = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "blocked-wait")
        .unwrap()
        .unwrap();
    crate::assistant::bind(
        root.path(),
        &host.key,
        "blocked-wait".into(),
        &input,
        vec![],
        false,
        None,
    )
    .unwrap();
    assert!(
        host.call(SEND_FILE_TOOL, args.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("without a source Run")
    );
    gateway.withhold_introductions(false);
    let blocked_args = json!({"content":"The scheduled task could not start"});
    let notified = scope(
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "blocked-native".into(),
            tool_call: "notify".into(),
        }),
        host.call(SEND_TOOL, blocked_args.clone()),
    )
    .await
    .unwrap()[0]
        .as_json()
        .unwrap()
        .clone();
    let once = gateway.frames();
    let repeated = scope(
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "blocked-native-2".into(),
            tool_call: "notify-again".into(),
        }),
        host.call(SEND_TOOL, blocked_args),
    )
    .await
    .unwrap()[0]
        .as_json()
        .unwrap()
        .clone();
    assert_eq!(notified["request_id"], repeated["request_id"]);
    assert_eq!(repeated["duplicate"], true);
    assert_eq!(gateway.frames(), once);
    let changed = scope(
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "blocked-native".into(),
            tool_call: "rewrite".into(),
        }),
        host.call(SEND_TOOL, json!({"content":"Changed failure explanation"})),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        changed.contains("different content or identity"),
        "{changed}"
    );
    assert_eq!(gateway.frames(), once);
    sessions
        .finish_task_result("local", &session, &host.key.run_id, "blocked-wait")
        .unwrap();
    sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: "follow-up".into(),
                ..request
            },
        )
        .unwrap();
    host.key.invocation = 7;
    let input = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "follow-up-wait")
        .unwrap()
        .unwrap();
    let path = crate::channel_notifications::file_path(&notification.id, &selected.name);
    crate::assistant::bind(
        root.path(),
        &host.key,
        "follow-up-wait".into(),
        &input,
        vec![],
        false,
        Some(notification),
    )
    .unwrap();
    host.readonly_inputs = vec![ReadOnlyInput::new(view.join("report.md"), &path)];
    assert_eq!(
        host.notification_file(path.to_str().unwrap()).unwrap(),
        (selected, bytes)
    );
    assert!(
        host.definitions()
            .iter()
            .any(|definition| definition.name == IMAGE_TOOL)
    );
    gateway.shutdown().await;
}
