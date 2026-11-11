use super::*;
use crate::{
    application::channel_management::fake::{FakeChannels, install_channel_plugin},
    goose_tool_context::{GooseToolIdentity, scope},
    run_metadata::{AssistantSource, RunMetadata},
};
use anchor_platform_session::{
    ChannelIdentity, ChannelInboundRequest, SessionStore, TaskResult, TurnStatus,
};

struct FeishuFixture {
    host: ChannelTools,
    channels: Arc<FakeChannels>,
    listener: tokio::net::UnixListener,
    sessions: SessionStore,
    session: String,
    inbound: ChannelInboundRequest,
}

fn feishu_fixture(root: &Path) -> FeishuFixture {
    let mut host = tools(root);
    install_channel_plugin(root, "feishu");
    host.channels = crate::plugin_library::Catalog::new(root)
        .resolve(&["feishu".into()])
        .unwrap();
    let socket = root.join("notification.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let descriptor = root.join("control.json");
    fs::write(
        &descriptor,
        json!({"socket":socket,"token":"private-feishu-token"}).to_string(),
    )
    .unwrap();
    let mut bot = FakeChannels::bot("graph", "local", "reply", &["ou_alice", "ou_bob"]);
    bot.platform = ChannelPlatform::Feishu;
    bot.binding.plugin = "feishu".into();
    bot.binding.private_only = true;
    bot.account = Some("feishu:fixture".into());
    bot.descriptor = Some(descriptor);
    let channels = Arc::new(FakeChannels::new().with(bot));
    host.application = host.application.with_channel_management(channels.clone());
    fs::create_dir_all(root.join("platform")).unwrap();
    let sessions = SessionStore::open(root.join("platform/sessions.sqlite")).unwrap();
    let inbound = ChannelInboundRequest {
        inbound_id: "initial".into(),
        identity: ChannelIdentity {
            source: "feishu".into(),
            account: Some("feishu:fixture".into()),
            conversation_kind: Default::default(),
            conversation_id: "oc_private".into(),
            sender_id: "ou_alice".into(),
        },
        graph: "graph".into(),
        reply_node: "reply".into(),
        text: Some("initial".into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
    };
    let admitted = sessions
        .admit_channel_inbound("local", inbound.clone())
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
    let input = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "initial")
        .unwrap()
        .unwrap();
    let mut metadata = RunMetadata::new(
        host.key.run_id.clone(),
        "graph".into(),
        host.key.graph_digest.clone(),
        root,
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
    crate::run_metadata::save(root, &metadata).unwrap();
    host.key.invocation = 2;
    crate::assistant::bind(
        root,
        &host.key,
        "initial".into(),
        &input,
        vec![],
        false,
        None,
    )
    .unwrap();
    FeishuFixture {
        host,
        channels,
        listener,
        sessions,
        session,
        inbound,
    }
}

#[tokio::test]
async fn feishu_task_tool_derives_private_destination_and_never_exposes_wecom_or_user_turn_sends() {
    let root = tempfile::tempdir().unwrap();
    let FeishuFixture {
        mut host,
        channels,
        listener,
        sessions,
        session,
        inbound,
    } = feishu_fixture(root.path());
    let definitions = host.definitions();
    assert_eq!(definitions.len(), 2);
    assert_eq!(definitions[1].name, FEISHU_SEND_FILE_TOOL);
    assert_eq!(
        definitions[1].parameters["required"],
        json!(["path", "content"])
    );
    assert!(
        definitions
            .iter()
            .all(|definition| !host.is_read_only(definition.name.as_str()))
    );
    assert_eq!(definitions[0].name, FEISHU_SEND_TOOL);
    assert_eq!(definitions[0].parameters["required"], json!(["content"]));
    let args = json!({"content":"The scheduled task completed."});
    assert!(
        host.call(FEISHU_SEND_TOOL, args.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("active task result")
    );
    for name in [SEND_TOOL, SEND_FILE_TOOL, IMAGE_TOOL] {
        assert!(matches!(
            host.call(name, json!({})).await,
            Err(ToolError::Unknown(_))
        ));
    }

    let initial = crate::assistant::input(root.path(), &host.key)
        .unwrap()
        .unwrap();
    sessions
        .finish_turn(
            "local",
            &session,
            &initial.turn().id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    let wrapped = wrap(
        host.inner.clone(),
        &host.channels,
        host.key.clone(),
        host.application.clone(),
        host.workspace.clone(),
        vec![],
        host.cancellation.clone(),
    );
    assert_eq!(wrapped.definitions()[0].name, FEISHU_SEND_TOOL);
    sessions
        .record_task_result(
            "local",
            &session,
            &TaskResult {
                id: "scheduled-task".into(),
                source_run: Some("producer".into()),
                outcome: "done".into(),
                content: json!({}),
                files: vec![],
            },
        )
        .unwrap();
    host.key.invocation = 3;
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
    for bad in [
        json!({"userid":"ou_bob","content":"private"}),
        json!({"bot_graph":"other","content":"private"}),
        json!({"content":""}),
    ] {
        assert!(host.call(FEISHU_SEND_TOOL, bad).await.is_err());
    }
    assert!(
        host.call(FEISHU_SEND_TOOL, args.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("no message was sent")
    );
    let native = || {
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "native-feishu".into(),
            tool_call: "notify".into(),
        })
    };
    let original_bot = channels.get("graph").unwrap();
    for change in 0..7 {
        channels.update("graph", |bot| match change {
            0 => bot.enabled = false,
            1 => bot.account = Some("another-app".into()),
            2 => bot.binding.send_users.clear(),
            3 => bot.platform = ChannelPlatform::Wecom,
            4 => bot.binding.private_only = false,
            5 => bot.binding.users.clear(),
            _ => bot.owner = "another-owner".into(),
        });
        assert!(
            scope(native(), host.call(FEISHU_SEND_TOOL, args.clone()))
                .await
                .is_err()
        );
        channels.put(original_bot.clone());
    }
    assert!(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    let send = scope(native(), host.call(FEISHU_SEND_TOOL, args.clone()));
    let gateway = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let payload = gateway_request(&mut stream).await;
        assert_eq!(payload["open_id"], "ou_alice");
        assert_eq!(payload["chat_id"], "oc_private");
        assert!(payload.get("userid").is_none());
        let ack = json!({"accepted":true,"request_id":payload["request_id"]});
        stream
            .write_all(format!("{ack}\n").as_bytes())
            .await
            .unwrap();
        payload
    };
    let (sent, payload) = tokio::join!(send, gateway);
    let receipt = sent.unwrap()[0].as_json().unwrap().clone();
    assert_eq!(receipt["status"], "confirmed");
    let first_id = payload["request_id"].as_str().unwrap();
    assert!(first_id.starts_with("channel-task-"));
    sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: "urgent".into(),
                ..inbound
            },
        )
        .unwrap();
    assert!(
        scope(native(), host.call(FEISHU_SEND_TOOL, args))
            .await
            .is_err()
    );
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
    host.key.invocation = 4;
    let retry = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "retry")
        .unwrap()
        .unwrap();
    crate::assistant::bind(
        root.path(),
        &host.key,
        "retry".into(),
        &retry,
        vec![],
        false,
        None,
    )
    .unwrap();
    let retry_id = scope(
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "new-native-feishu".into(),
            tool_call: "retry-native".into(),
        }),
        async { host.delivery_request_id("ou_alice", None) },
    )
    .await
    .unwrap();
    assert_eq!(first_id, retry_id);
    assert!(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    host.assistant_graph = None;
    assert!(host.definitions().is_empty());
    assert!(matches!(
        host.call(FEISHU_SEND_TOOL, json!({"content":"ordinary task"}))
            .await,
        Err(ToolError::Unknown(_))
    ));
}

fn native_file_call(host: &ChannelTools, call: &str) -> Option<GooseToolIdentity> {
    Some(GooseToolIdentity {
        key: host.key.clone(),
        session: "native-feishu-files".into(),
        tool_call: call.into(),
    })
}

#[tokio::test]
async fn feishu_user_files_use_visible_bytes_original_recipient_and_native_identity() {
    let root = tempfile::tempdir().unwrap();
    let FeishuFixture {
        host,
        channels,
        listener,
        sessions,
        session,
        inbound,
    } = feishu_fixture(root.path());
    let mut host = host;
    fs::write(host.workspace.join("report.md"), "# Generated report").unwrap();
    let input = root.path().join("selected-input.txt");
    fs::write(&input, "z").unwrap();
    host.readonly_inputs = vec![ReadOnlyInput::new(&input, "/in/channel/selected-input.txt")];
    std::os::unix::fs::symlink(&input, host.workspace.join("linked.txt")).unwrap();
    let args = json!({"path":"/workspace/report.md","content":"## Your report"});
    let original_bot = channels.get("graph").unwrap();
    for change in 0..7 {
        channels.update("graph", |bot| match change {
            0 => bot.enabled = false,
            1 => bot.account = Some("another-app".into()),
            2 => bot.binding.send_users.clear(),
            3 => bot.binding.users.clear(),
            4 => bot.owner = "foreign-owner".into(),
            5 => bot.binding.private_only = false,
            _ => bot.platform = ChannelPlatform::Wecom,
        });
        assert!(
            scope(
                native_file_call(&host, "revoked"),
                host.call(FEISHU_SEND_FILE_TOOL, args.clone())
            )
            .await
            .is_err()
        );
        channels.put(original_bot.clone());
    }
    for invalid in [
        json!({"path":"/workspace/report.md","content":"private","open_id":"ou_bob"}),
        json!({"path":"/workspace/report.md","content":"private","userid":"ou_bob"}),
        json!({"path":"/workspace/report.md","content":"private","bot_graph":"another"}),
        json!({"path":"/workspace/report.md","content":"private","context_files":["/in/channel/selected-input.txt"]}),
    ] {
        assert!(
            scope(
                native_file_call(&host, "invalid"),
                host.call(FEISHU_SEND_FILE_TOOL, invalid)
            )
            .await
            .is_err()
        );
    }
    for path in [
        "/etc/passwd",
        "../selected-input.txt",
        "/workspace/.git/config",
        "linked.txt",
        "/in/channel/other.txt",
        "/in/channel/selected-input.txt/../other.txt",
    ] {
        assert!(
            scope(
                native_file_call(&host, "hidden"),
                host.call(
                    FEISHU_SEND_FILE_TOOL,
                    json!({"path":path,"content":"private"})
                )
            )
            .await
            .is_err(),
            "{path}"
        );
    }
    assert!(
        host.call(FEISHU_SEND_FILE_TOOL, args.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("no message was sent")
    );
    assert!(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );

    let mut ids = Vec::new();
    for (path, name, bytes, native) in [
        (
            "/workspace/report.md",
            "report.md",
            b"# Generated report".as_slice(),
            "generated",
        ),
        (
            "/in/channel/selected-input.txt",
            "selected-input.txt",
            b"z".as_slice(),
            "received",
        ),
    ] {
        let send = scope(
            native_file_call(&host, native),
            host.call(
                FEISHU_SEND_FILE_TOOL,
                json!({"path":path,"content":"## Your report"}),
            ),
        );
        let gateway = async {
            let mut payloads = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let payload = gateway_request(&mut stream).await;
                assert_eq!(payload["open_id"], "ou_alice");
                assert_eq!(payload["chat_id"], "oc_private");
                assert!(payload.get("userid").is_none());
                let ack = json!({"accepted":true,"request_id":payload["request_id"]});
                stream
                    .write_all(format!("{ack}\n").as_bytes())
                    .await
                    .unwrap();
                payloads.push(payload);
            }
            payloads
        };
        let (sent, payloads) = tokio::join!(send, gateway);
        let receipt = sent.unwrap()[0].as_json().unwrap().clone();
        assert_eq!(receipt["status"], "confirmed");
        assert!(receipt.get("notification").is_none());
        let id = receipt["request_id"].as_str().unwrap().to_owned();
        assert!(!id.starts_with("channel-task-"));
        assert_eq!(payloads[0]["operation"], "send_file");
        assert_eq!(payloads[0]["name"], name);
        assert_eq!(
            STANDARD
                .decode(payloads[0]["data_base64"].as_str().unwrap())
                .unwrap(),
            bytes
        );
        assert_eq!(payloads[0]["md5"], format!("{:x}", Md5::digest(bytes)));
        assert_eq!(payloads[0]["request_id"], format!("{id}-file"));
        assert_eq!(payloads[1]["request_id"], format!("{id}-intro"));
        assert_eq!(payloads[1]["content"], "## Your report");
        ids.push(id);
    }
    assert_ne!(ids[0], ids[1]);
    assert!(
        sessions
            .latest_channel_notification("local", &session)
            .unwrap()
            .is_none()
    );
    assert!(!root.path().join("artifacts").exists());

    // A file ACK does not imply its separate introduction was sent.
    let send = scope(
        native_file_call(&host, "cancel-after-file"),
        host.call(FEISHU_SEND_FILE_TOOL, args.clone()),
    );
    let gateway = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let payload = gateway_request(&mut stream).await;
        host.cancellation.store(true, Ordering::SeqCst);
        let ack = json!({"accepted":true,"request_id":payload["request_id"]});
        stream
            .write_all(format!("{ack}\n").as_bytes())
            .await
            .unwrap();
    };
    let (partial, _) = tokio::join!(send, gateway);
    let partial = partial.unwrap_err().to_string();
    assert!(partial.contains("confirmed; introduction"), "{partial}");
    assert!(partial.contains("not confirmed"), "{partial}");
    assert!(!partial.contains("saved for the next Turn"), "{partial}");
    assert!(
        scope(
            native_file_call(&host, "stopped"),
            host.call(FEISHU_SEND_FILE_TOOL, args.clone())
        )
        .await
        .is_err()
    );
    host.cancellation.store(false, Ordering::SeqCst);
    sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: "replacement".into(),
                ..inbound
            },
        )
        .unwrap();
    assert!(
        scope(
            native_file_call(&host, "stale"),
            host.call(FEISHU_SEND_FILE_TOOL, args)
        )
        .await
        .is_err()
    );
    assert!(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn feishu_task_files_retain_exact_selection_and_record_file_ack_before_introduction() {
    use anchor_platform_session::ChannelNotificationFile;
    use anchor_runtime::graph::{ArtifactPort, NodeCompletion};
    let root = tempfile::tempdir().unwrap();
    let FeishuFixture {
        mut host,
        listener,
        sessions,
        session,
        inbound,
        ..
    } = feishu_fixture(root.path());
    let initial = crate::assistant::input(root.path(), &host.key)
        .unwrap()
        .unwrap();
    sessions
        .finish_turn(
            "local",
            &session,
            &initial.turn().id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    let artifacts =
        crate::HostArtifacts::new(root.path().join("artifacts"), root.path().join("producing"));
    let producer = anchor_runtime::graph::InvocationKey {
        run_id: "producer".into(),
        node_id: "produce".into(),
        ..host.key.clone()
    };
    let workspace = artifacts.prepare_workspace(&producer, &[]).unwrap();
    fs::write(workspace.join("report.md"), "immutable report").unwrap();
    fs::write(workspace.join("context.txt"), "immutable context").unwrap();
    fs::write(workspace.join("hidden.txt"), "unselected sibling").unwrap();
    let commit = artifacts
        .freeze(
            &producer,
            &NodeCompletion {
                submission: "ready".into(),
                route: None,
                model_requests: 0,
                output: json!({"files":["report.md","context.txt","hidden.txt"]}),
            },
        )
        .await
        .unwrap();
    let selected = vec![
        ChannelNotificationFile {
            artifact_id: commit.id.clone(),
            path: "report.md".into(),
            name: "alias.md".into(),
        },
        ChannelNotificationFile {
            artifact_id: commit.id.clone(),
            path: "context.txt".into(),
            name: "context.txt".into(),
        },
    ];
    sessions
        .record_task_result(
            "local",
            &session,
            &TaskResult {
                id: "result".into(),
                source_run: Some("producer".into()),
                outcome: "done".into(),
                content: json!({}),
                files: selected.clone(),
            },
        )
        .unwrap();
    host.key.invocation = 3;
    let input = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "task")
        .unwrap()
        .unwrap();
    crate::assistant::bind(
        root.path(),
        &host.key,
        "task".into(),
        &input,
        vec![],
        false,
        None,
    )
    .unwrap();
    let view = artifacts.files_path(&commit).unwrap();
    let mounts = selected
        .iter()
        .map(|file| {
            ReadOnlyInput::new(
                view.join(&file.path),
                format!("/in/tasks/result/{}", file.name),
            )
        })
        .collect::<Vec<_>>();
    host.readonly_inputs = mounts.clone();
    fs::write(host.workspace.join("mutable.md"), "not a result").unwrap();
    for path in [
        "/workspace/mutable.md",
        "/in/tasks/result/hidden.txt",
        "/in/tasks/other/alias.md",
        "/in/tasks/result/alias.md/../hidden.txt",
    ] {
        assert!(
            scope(
                native_file_call(&host, "invalid-task"),
                host.call(
                    FEISHU_SEND_FILE_TOOL,
                    json!({"path":path,"content":"private"})
                )
            )
            .await
            .is_err(),
            "{path}"
        );
    }
    host.readonly_inputs = vec![ReadOnlyInput::new(&view, "/in/tasks/result")];
    assert!(host.notification_file("/in/tasks/result/alias.md").is_err());
    host.readonly_inputs = mounts;
    let args = json!({"path":"/in/tasks/result/alias.md","content":"# Your immutable result","context_files":["/in/tasks/result/context.txt"]});
    let send = scope(
        native_file_call(&host, "task-file"),
        host.call(FEISHU_SEND_FILE_TOOL, args.clone()),
    );
    let gateway = async {
        let mut payloads = Vec::new();
        for index in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let payload = gateway_request(&mut stream).await;
            assert_eq!(payload["open_id"], "ou_alice");
            assert_eq!(payload["chat_id"], "oc_private");
            let ack = if index == 0 {
                json!({"accepted":true,"request_id":payload["request_id"]})
            } else {
                json!({"error":"previous delivery is unconfirmed; do not resend"})
            };
            stream
                .write_all(format!("{ack}\n").as_bytes())
                .await
                .unwrap();
            payloads.push(payload);
        }
        payloads
    };
    let (partial, payloads) = tokio::join!(send, gateway);
    assert!(
        partial
            .unwrap_err()
            .to_string()
            .contains("confirmed and saved for the next Turn; introduction")
    );
    assert_eq!(payloads[0]["name"], "report.md");
    assert_eq!(
        STANDARD
            .decode(payloads[0]["data_base64"].as_str().unwrap())
            .unwrap(),
        b"immutable report"
    );
    let notice = sessions
        .latest_channel_notification("local", &session)
        .unwrap()
        .unwrap();
    assert_eq!(notice.files, selected);
    assert_eq!(notice.source_run, "producer");
    assert!(notice.id.starts_with("channel-task-"));
    let original_id = notice.id;
    sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: "interrupt-task".into(),
                ..inbound
            },
        )
        .unwrap();
    assert!(
        scope(
            native_file_call(&host, "stale-task"),
            host.call(FEISHU_SEND_FILE_TOOL, args)
        )
        .await
        .is_err()
    );
    let urgent = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "urgent")
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
    host.key.invocation = 4;
    let retry = sessions
        .claim_assistant_input("local", &session, &host.key.run_id, "retry-task")
        .unwrap()
        .unwrap();
    crate::assistant::bind(
        root.path(),
        &host.key,
        "retry-task".into(),
        &retry,
        vec![],
        false,
        None,
    )
    .unwrap();
    let request_id = scope(native_file_call(&host, "new-native-call"), async {
        host.delivery_request_id("ou_alice", Some(&selected[0]))
    })
    .await
    .unwrap();
    assert_eq!(request_id, original_id);
    assert!(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}
