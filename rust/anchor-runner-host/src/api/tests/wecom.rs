use super::*;
use anchor_platform_session::{
    ChannelDeliveryStatus, ChannelIdentity, ChannelInboundRequest, SessionStore, TurnStatus,
};
use anchor_runtime::graph::FileRunStore;
use anchor_runtime::graph::{CommitRef, GraphRunRecord, InvocationKey, NodeCompletion, RunResult};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;

fn event(text: &str) -> Value {
    json!({
        "event": {
            "source":"wecom",
            "event_id":"message-1",
            "sender_id":"user-1",
            "conversation_id":"conversation-1",
            "text":text,
            "reply_target":"conversation-1",
            "message_type":"text",
            "metadata":{"chat_type":"single","request_id":"callback-1"},
            "attachments":[]
        }
    })
}

#[tokio::test]
async fn wecom_text_event_runs_host_graph_and_returns_idempotent_delivery_receipt() {
    let (root, mut state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    unsafe {
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh,cat,printf");
    }
    state.wecom.reply_node = "reply".into();
    write_graph_bundle(
        &state.bundle_root,
        &json!({
            "objective":"deterministic WeCom webhook fixture",
            "entry":"reply",
            "agents":{},
            "ops":{"reply":{"run":"printf 'reply from fixture'"}},
            "nodes":[{"id":"reply","op":"reply","plugins":[]}],
            "edges":[]
        }),
    )
    .unwrap();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let event_body = event("hello from wecom");
    let (status, response) = call(
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&event_body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["text"], "reply from fixture");
    let session = response["session"].as_str().unwrap();
    let run = response["run"].as_str().unwrap();
    let key = response["receipt"]["key"].as_str().unwrap();
    let content_sha256 = response["receipt"]["content_sha256"].as_str().unwrap();
    assert!(key.starts_with(&format!("channel-wecom-reply:{session}:")));
    assert_eq!(content_sha256.len(), 64);

    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let deliveries = sessions
        .list_unfinished_channel_deliveries("local", session)
        .unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].key, key);
    assert_eq!(deliveries[0].content_sha256, content_sha256);
    assert_eq!(deliveries[0].status, ChannelDeliveryStatus::Sending);
    assert_eq!(
        sessions
            .get_channel_inbound("local", session, &inbound_id())
            .unwrap()
            .relation
            .run_id
            .as_deref(),
        Some(run)
    );

    let settlement = json!({
        "event":event_body["event"].clone(),
        "settlement":{"key":key,"content_sha256":content_sha256,"status":"confirmed"}
    });
    let mut invalid_settlement = settlement.clone();
    invalid_settlement["settlement"]["content_sha256"] = json!("a".repeat(64));
    let (status, _) = call(
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&invalid_settlement.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, settled) = call(
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&settlement.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["delivery"]["status"], "confirmed");
    let (status, repeated_settlement) = call(
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&settlement.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repeated_settlement}");
    assert_eq!(repeated_settlement["delivery"]["status"], "confirmed");

    let (status, duplicate) = call(
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&event_body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{duplicate}");
    assert_eq!(duplicate["run"], run);
    assert_eq!(duplicate["receipt"]["key"], key);
    assert_eq!(duplicate["receipt"]["content_sha256"], content_sha256);
    assert!(
        sessions
            .list_unfinished_channel_deliveries("local", session)
            .unwrap()
            .is_empty()
    );

    let (status, conflict) = call(
        app,
        "POST",
        "/channels/wecom/events",
        Some(&event("different content").to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
}

#[tokio::test]
async fn wecom_event_rejects_untrusted_users_and_non_private_or_media_messages() {
    let (root, mut state) = fixture();
    state.wecom.users = HashSet::from(["user-1".into()]);
    let app = router_with_web_root(state, root.path().join("web"));

    let mut body = event("hello");
    body["event"]["sender_id"] = json!("unauthorized");
    let (status, _) = call(
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let mut body = event("hello");
    body["event"]["metadata"]["chat_type"] = json!("group");
    let (status, _) = call(
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mut body = event("hello");
    body["event"]["attachments"] = json!([{"url":"https://example.invalid/file"}]);
    let (status, _) = call(
        app,
        "POST",
        "/channels/wecom/events",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn wecom_attachment_validation_rejects_bad_bytes_names_and_counts_before_admission() {
    let (root, state) = fixture();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let invalid = vec![
        vec![json!({"name":"bad.txt","data_base64":"not base64!","media_type":null})],
        vec![json!({"name":"../host.txt","data_base64":"aG9zdA==","media_type":"text/plain"})],
        vec![json!({"name":"safe.txt","data_base64":"aG9zdA==","path":"/etc/passwd"})],
        (0..17)
            .map(|index| json!({"name":format!("{index}.txt"),"data_base64":"","media_type":null}))
            .collect::<Vec<_>>(),
    ];

    for (index, attachments) in invalid.into_iter().enumerate() {
        let mut body = event("caption");
        body["event"]["event_id"] = json!(format!("invalid-{index}"));
        body["event"]["message_type"] = json!("mixed");
        body["event"]["attachments"] = json!(attachments);
        let (status, rejected) = call(
            app.clone(),
            "POST",
            "/channels/wecom/events",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    }

    for (message_type, text, attachments) in [
        ("image", "text must not mask missing media", json!([])),
        (
            "text",
            "text events cannot smuggle attachments",
            json!([{"name":"note.txt","data_base64":"aGVsbG8="}]),
        ),
    ] {
        let mut body = event(text);
        body["event"]["event_id"] = json!(format!("mismatch-{message_type}"));
        body["event"]["message_type"] = json!(message_type);
        body["event"]["attachments"] = attachments;
        let (status, rejected) = call(
            app.clone(),
            "POST",
            "/channels/wecom/events",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    }
    assert!(state.application.records().unwrap().is_empty());
    assert!(!state.data_root.join("channel-inputs").exists());
    assert!(!state.data_root.join("platform/sessions.sqlite").exists());
}

#[tokio::test]
async fn wecom_host_handoff_freezes_image_file_and_mixed_attachment_manifests() {
    let (root, mut state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    unsafe {
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh,cat,printf");
    }
    state.wecom.reply_node = "reply".into();
    write_graph_bundle(
        &state.bundle_root,
        &json!({
            "objective":"deterministic WeCom attachment fixture",
            "entry":"reply",
            "agents":{},
            "ops":{"reply":{"run":"printf 'attachment reply'"}},
            "nodes":[{"id":"reply","op":"reply","plugins":[]}],
            "edges":[]
        }),
    )
    .unwrap();
    let mut png = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(1, 1)
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    let large_file = vec![b'x'; 2 * 1024 * 1024 + 1];
    let cases = [
        (
            "image",
            "",
            vec![("picture.png", png.as_slice(), Some("image/png"))],
        ),
        (
            "file",
            "",
            vec![("notes.txt", b"file bytes".as_slice(), Some("text/plain"))],
        ),
        (
            "mixed",
            "caption",
            vec![("mixed.png", png.as_slice(), Some("image/png"))],
        ),
        (
            "file",
            "",
            vec![(
                "large.bin",
                large_file.as_slice(),
                Some("application/octet-stream"),
            )],
        ),
    ];
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();

    for (index, (message_type, text, files)) in cases.into_iter().enumerate() {
        let event_id = format!("attachment-{message_type}-{index}");
        let mut body = event(text);
        body["event"]["event_id"] = json!(event_id);
        body["event"]["conversation_id"] = json!(format!("conversation-{message_type}-{index}"));
        body["event"]["reply_target"] = body["event"]["conversation_id"].clone();
        body["event"]["message_type"] = json!(message_type);
        body["event"]["attachments"] = json!(
            files
                .iter()
                .map(|(name, bytes, media_type)| json!({
                    "name":name,
                    "data_base64":STANDARD.encode(bytes),
                    "media_type":media_type,
                }))
                .collect::<Vec<_>>()
        );

        let (status, response) = call(
            app.clone(),
            "POST",
            "/channels/wecom/events",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{message_type}: {response}");
        assert_eq!(response["text"], "attachment reply");
        let run = response["run"].as_str().unwrap();
        let session = response["session"].as_str().unwrap();
        let inbound = sessions
            .get_channel_inbound("local", session, &inbound_id_for(&event_id))
            .unwrap();
        assert_eq!(inbound.request.attachments.format, 1);
        assert_eq!(inbound.request.attachments.files.len(), files.len());
        let expected_text = if text.is_empty() {
            "请查看随附文件并据此回复。"
        } else {
            text
        };
        assert_eq!(inbound.request.text.as_deref(), Some(expected_text));
        for (entry, (name, bytes, media_type)) in
            inbound.request.attachments.files.iter().zip(&files)
        {
            assert_eq!(entry.name, *name);
            assert_eq!(entry.path, format!("/in/channel/{name}"));
            assert_eq!(entry.sha256, format!("{:x}", Sha256::digest(bytes)));
            assert_eq!(entry.size, bytes.len() as u64);
            assert_eq!(entry.media_type.as_deref(), *media_type);
        }

        let metadata = state.application.metadata(run).unwrap().unwrap();
        assert_eq!(metadata.attachments.len(), files.len());
        let key = InvocationKey {
            run_id: run.into(),
            graph_digest: metadata.graph_digest.clone(),
            node_id: "reply".into(),
            invocation: 1,
        };
        let node_inputs =
            crate::channel_inputs::node_inputs(&state.data_root, &metadata, &key).unwrap();
        let expected_images = files
            .iter()
            .filter(|(_, _, media_type)| {
                media_type.is_some_and(|value| value.starts_with("image/"))
            })
            .collect::<Vec<_>>();
        assert_eq!(node_inputs.images.len(), expected_images.len());
        for (image, (name, bytes, media_type)) in node_inputs.images.iter().zip(expected_images) {
            assert_eq!(image.bytes, *bytes);
            assert_eq!(image.media_type, media_type.unwrap());
            assert!(name.ends_with(".png"));
        }
        let mount = node_inputs.mount.unwrap();
        assert_eq!(mount.destination, std::path::Path::new("/in/channel"));
        for (attachment, (name, bytes, _)) in metadata.attachments.iter().zip(&files) {
            assert_eq!(attachment.name, *name);
            assert_eq!(attachment.sha256, format!("{:x}", Sha256::digest(bytes)));
            assert_eq!(std::fs::read(mount.source.join(name)).unwrap(), *bytes);
        }
        let run_record = FileRunStore::new(state.data_root.join("runs"))
            .load(run)
            .unwrap()
            .unwrap();
        assert_eq!(run_record.status, RunStatus::Completed, "case {index}");
    }
}

#[tokio::test]
async fn recovery_admits_completed_wecom_delivery_once_without_rerunning_the_graph() {
    let (_root, state) = fixture();
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let run_id = "channel-00000000-0000-4000-8000-000000000001";
    let inbound_id = "wecom-recovery-inbound";
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let admission = sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: inbound_id.into(),
                identity: ChannelIdentity {
                    source: "wecom".into(),
                    account: Some("corp-a".into()),
                    conversation_id: "conversation-recovery".into(),
                    sender_id: "user-recovery".into(),
                },
                graph: "fixture".into(),
                reply_node: "work".into(),
                text: Some("recover this".into()),
                attachments: Default::default(),
                run_id: Some(run_id.into()),
                replace_running: true,
            },
        )
        .unwrap();
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let mut record = GraphRunRecord::create_with_id(
        bundle.snapshot,
        json!({
            "message":"recover this",
            "channel":{
                "source":"wecom",
                "conversation_id":"conversation-recovery",
                "sender_id":"user-recovery",
                "reply_target":"conversation-recovery"
            },
            "session":admission.session.id,
        }),
        run_id,
    )
    .unwrap();
    record.status = RunStatus::Completed;
    record.invocations.insert("work".into(), 1);
    record.passes.insert("work".into(), 1);
    record.sequence = 1;
    let key = InvocationKey {
        run_id: run_id.into(),
        graph_digest: record.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    };
    record.results.insert(
        "work".into(),
        vec![RunResult {
            node_id: "work".into(),
            key,
            completion: NodeCompletion {
                submission: "recovered reply".into(),
                route: None,
                model_requests: 0,
                output: json!({}),
            },
            commit: CommitRef {
                id: "recovery-commit".into(),
                node_id: "work".into(),
                invocation: 1,
            },
            sequence: 1,
        }],
    );
    let before = record.clone();
    let mut metadata = RunMetadata::new(
        run_id.into(),
        "fixture".into(),
        record.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    metadata.channel = Some(crate::application::ChannelRunSource {
        owner: "local".into(),
        session: admission.session.id.clone(),
        inbound: inbound_id.into(),
        turn: admission.turn.id.clone(),
    });
    metadata.conversation = Some(crate::application::ConversationSource {
        session: admission.session.id.clone(),
        reply_node: "work".into(),
        previous_run: None,
    });
    metadata::save(&state.data_root, &metadata).unwrap();
    FileRunStore::new(state.data_root.join("runs"))
        .save(&record)
        .unwrap();

    recover_pilot_turns(&state).await.unwrap();
    let deliveries = sessions
        .list_unfinished_channel_deliveries("local", &admission.session.id)
        .unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].status, ChannelDeliveryStatus::Pending);
    assert_eq!(deliveries[0].turn_id, admission.turn.id);
    assert_eq!(deliveries[0].kind, "wecom_reply");
    assert_eq!(deliveries[0].content_sha256.len(), 64);
    assert_eq!(
        sessions
            .get_turn("local", &admission.session.id, &admission.turn.id)
            .unwrap()
            .status,
        TurnStatus::Completed
    );
    assert_eq!(
        FileRunStore::new(state.data_root.join("runs"))
            .load(run_id)
            .unwrap()
            .unwrap(),
        before
    );

    recover_pilot_turns(&state).await.unwrap();
    assert_eq!(
        sessions
            .list_unfinished_channel_deliveries("local", &admission.session.id)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn superseded_completed_wecom_run_does_not_create_a_reply_delivery() {
    let (_root, state) = fixture();
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let run_id = "channel-00000000-0000-4000-8000-000000000002";
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let identity = ChannelIdentity {
        source: "wecom".into(),
        account: Some("corp-a".into()),
        conversation_id: "conversation-superseded".into(),
        sender_id: "user-superseded".into(),
    };
    let previous = sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: "wecom-superseded-1".into(),
                identity: identity.clone(),
                graph: "fixture".into(),
                reply_node: "work".into(),
                text: Some("older message".into()),
                attachments: Default::default(),
                run_id: Some(run_id.into()),
                replace_running: true,
            },
        )
        .unwrap();
    let current = sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: "wecom-superseded-2".into(),
                identity,
                graph: "fixture".into(),
                reply_node: "work".into(),
                text: Some("newer message".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: true,
            },
        )
        .unwrap();
    assert_eq!(
        sessions
            .get_channel_inbound("local", &previous.session.id, "wecom-superseded-1")
            .unwrap()
            .relation
            .superseded_by_turn_id
            .as_deref(),
        Some(current.turn.id.as_str())
    );

    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let mut record = GraphRunRecord::create_with_id(
        bundle.snapshot,
        json!({
            "message":"older message",
            "channel":{"source":"wecom"},
            "session":previous.session.id,
        }),
        run_id,
    )
    .unwrap();
    record.status = RunStatus::Completed;
    record.invocations.insert("work".into(), 1);
    record.passes.insert("work".into(), 1);
    record.sequence = 1;
    let key = InvocationKey {
        run_id: run_id.into(),
        graph_digest: record.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    };
    record.results.insert(
        "work".into(),
        vec![RunResult {
            node_id: "work".into(),
            key,
            completion: NodeCompletion {
                submission: "stale reply".into(),
                route: None,
                model_requests: 0,
                output: json!({}),
            },
            commit: CommitRef {
                id: "superseded-commit".into(),
                node_id: "work".into(),
                invocation: 1,
            },
            sequence: 1,
        }],
    );
    let mut metadata = RunMetadata::new(
        run_id.into(),
        "fixture".into(),
        record.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    metadata.channel = Some(crate::application::ChannelRunSource {
        owner: "local".into(),
        session: previous.session.id.clone(),
        inbound: "wecom-superseded-1".into(),
        turn: previous.turn.id.clone(),
    });
    metadata.conversation = Some(crate::application::ConversationSource {
        session: previous.session.id.clone(),
        reply_node: "work".into(),
        previous_run: None,
    });
    metadata::save(&state.data_root, &metadata).unwrap();
    FileRunStore::new(state.data_root.join("runs"))
        .save(&record)
        .unwrap();

    state
        .application
        .settle_channel_run(run_id, RunStatus::Completed)
        .unwrap();
    assert_eq!(
        sessions
            .get_turn("local", &previous.session.id, &previous.turn.id)
            .unwrap()
            .status,
        TurnStatus::Interrupted
    );
    assert!(
        sessions
            .list_unfinished_channel_deliveries("local", &previous.session.id)
            .unwrap()
            .is_empty()
    );
}

fn inbound_id() -> String {
    inbound_id_for("message-1")
}

fn inbound_id_for(event_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"anchor-wecom-inbound-v1\0");
    digest.update(b"wecom");
    digest.update([0]);
    digest.update(event_id.as_bytes());
    format!("wecom-{:x}", digest.finalize())
}
