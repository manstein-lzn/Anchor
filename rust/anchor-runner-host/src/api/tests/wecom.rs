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
async fn deleted_previous_channel_run_does_not_block_a_new_instance() {
    let (_root, state) = fixture();
    assert!(
        !super::super::wecom::stop_previous_run(&state, "deleted-previous-run")
            .await
            .unwrap()
    );
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
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A voice event without attachment content is an invalid handoff.
    let mut body = event("");
    body["event"]["message_type"] = json!("voice");
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
            "voice",
            "",
            vec![("voice.amr", b"voice bytes".as_slice(), Some("audio/amr"))],
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
            interruption: None,
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
            interruption: None,
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

/// A message that interrupts an unanswered turn travels with the next Run of
/// the same session, so the interrupted work continues with the new
/// information instead of disappearing.
#[tokio::test]
async fn interrupted_wecom_message_travels_with_the_next_run_of_the_session() {
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
            "objective":"deterministic WeCom interruption fixture",
            "entry":"reply",
            "agents":{},
            "ops":{"reply":{"run":"printf 'reply from fixture'"}},
            "nodes":[{"id":"reply","op":"reply","plugins":[]}],
            "edges":[]
        }),
    )
    .unwrap();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();

    let mut first = event("先看第一件事");
    first["event"]["event_id"] = json!("burst-1");
    first["event"]["metadata"]["request_id"] = json!("callback-burst-1");
    let (status, one) = call(
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&first.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{one}");
    let run_one = one["run"].as_str().unwrap().to_owned();
    let session = one["session"].as_str().unwrap().to_owned();

    // The second message arrives before the first reply was confirmed.
    let mut second = event("现在改成第二件事");
    second["event"]["event_id"] = json!("burst-2");
    second["event"]["metadata"]["request_id"] = json!("callback-burst-2");
    let (status, two) = call(
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&second.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{two}");
    let run_two = two["run"].as_str().unwrap().to_owned();
    assert_ne!(run_one, run_two);
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(&run_two)
        .unwrap()
        .unwrap();
    assert_eq!(record.input["message"], "现在改成第二件事");
    assert_eq!(
        record.input["interrupted_messages"],
        json!(["先看第一件事"])
    );

    // Once the platform confirms a reply, earlier messages are no longer
    // carried into the following Run.
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let unfinished = sessions
        .list_unfinished_channel_deliveries("local", &session)
        .unwrap();
    let delivery = unfinished
        .iter()
        .find(|delivery| delivery.turn_id == run_two.trim_start_matches("channel-"))
        .unwrap();
    sessions
        .settle_channel_delivery_from_gateway(
            "local",
            &session,
            &inbound_id_for("burst-2"),
            anchor_platform_session::ChannelDeliveryRequest {
                key: delivery.key.clone(),
                kind: delivery.kind.clone(),
                content_sha256: delivery.content_sha256.clone(),
            },
            ChannelDeliveryStatus::Confirmed,
        )
        .unwrap();

    let mut third = event("第三件事");
    third["event"]["event_id"] = json!("burst-3");
    third["event"]["metadata"]["request_id"] = json!("callback-burst-3");
    let (status, three) = call(
        app,
        "POST",
        "/channels/wecom/events",
        Some(&third.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{three}");
    let run_three = three["run"].as_str().unwrap();
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(run_three)
        .unwrap()
        .unwrap();
    assert!(record.input.get("interrupted_messages").is_none());
}

/// The progress endpoint is a read-only observation: it reports the terminal
/// state of an admitted event, refuses unknown identities, and never exposes
/// anything else.
#[tokio::test]
async fn wecom_progress_reports_only_settled_state_for_a_finished_turn() {
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
            "objective":"deterministic WeCom progress fixture",
            "entry":"reply",
            "agents":{},
            "ops":{"reply":{"run":"printf 'reply from fixture'"}},
            "nodes":[{"id":"reply","op":"reply","plugins":[]}],
            "edges":[]
        }),
    )
    .unwrap();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let (status, response) = call(
        app.clone(),
        "POST",
        "/channels/wecom/events",
        Some(&event("hello from wecom").to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");

    let (status, body) = call_text(app.clone(), "GET", "/channels/wecom/progress/message-1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // An op reply node has no live Goose invocation, so the only thing this
    // turn can report is that it is over.
    assert!(body.contains("event: settled"), "{body}");
    assert!(body.contains("\"settled\":true"), "{body}");
    assert!(body.contains("\"status\":\"completed\""), "{body}");
    assert!(!body.contains("event: update"), "{body}");
    assert!(!body.contains("正在"), "{body}");

    let (status, denied) = call(app, "GET", "/channels/wecom/progress/never-admitted", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{denied}");
}

/// The progress stream is behind the same bearer authentication as every other
/// channel route.
#[tokio::test]
async fn wecom_progress_requires_authentication() {
    let (root, mut state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    state.loopback = false;
    state.api_keys = vec!["p".repeat(32)];
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    for (authorization, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (
            Some(format!("Bearer {}", "w".repeat(32))),
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        let mut builder = Request::builder()
            .method("GET")
            .uri("/channels/wecom/progress/message-1");
        if let Some(value) = &authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}

/// A persistent assistant has no per-message Run, and its `reply` node is an Op
/// without a Goose trace. The progress stream therefore has to locate the work
/// node of the round that claimed this message through the association the
/// Session store persisted — the old code looked up a *work* invocation binding
/// that never exists (bindings are keyed by the wait invocation) and silently
/// reported nothing.
#[tokio::test]
async fn persistent_assistant_progress_resolves_the_bound_round() {
    use anchor_runtime::graph::GraphSnapshot;
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};

    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();

    let mut definition: Value = serde_json::from_str(include_str!(
        "../../../../../examples/graphs/wecom-persistent-assistant.json"
    ))
    .unwrap();
    definition["nodes"][1]["plugins"] = json!([]);
    write_graph_bundle(&state.bundle_root, &definition).unwrap();

    let owner = "local";
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let admission = sessions
        .admit_channel_inbound(
            owner,
            ChannelInboundRequest {
                inbound_id: inbound_id_for("message-1"),
                identity: ChannelIdentity {
                    source: "wecom".into(),
                    account: None,
                    conversation_id: "conversation-1".into(),
                    sender_id: "user-1".into(),
                },
                graph: "fixture".into(),
                // The channel Session's result node is the node whose
                // completion carries the reply: the work agent, not the loop's
                // reply op.
                reply_node: "assistant".into(),
                text: Some("看看工作区".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: true,
            },
        )
        .unwrap();
    let session = admission.session.id.clone();
    let turn = admission.turn.id.clone();
    let run = "assistant-progress-fixture";
    let record = GraphRunRecord::create_with_id(
        GraphSnapshot::admit(definition.clone()).unwrap(),
        json!({"session":session}),
        run,
    )
    .unwrap();
    FileRunStore::new(state.data_root.join("runs"))
        .save(&record)
        .unwrap();

    sessions
        .bind_channel_assistant(owner, &session, run, "wait_input", "assistant", "reply")
        .unwrap();
    let wait_key = format!("{run}:{}:wait_input:1", record.graph_digest);
    let claimed = sessions
        .claim_channel_assistant_input(owner, &session, run, &wait_key)
        .unwrap()
        .unwrap();
    // The wait op records its own invocation for the round it claimed.
    crate::assistant::bind(
        &state.data_root,
        &InvocationKey {
            run_id: run.into(),
            graph_digest: record.graph_digest.clone(),
            node_id: "wait_input".into(),
            invocation: 1,
        },
        wait_key.clone(),
        &claimed,
        Vec::new(),
        false,
    )
    .unwrap();

    let mut saved = RunMetadata::new(
        run.into(),
        "fixture".into(),
        record.graph_digest.clone(),
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

    // The round is identified by the persisted association, and the work
    // invocation is that round with the node substituted.
    let persisted = sessions
        .channel_assistant_wait_key(owner, &session, &turn)
        .unwrap()
        .expect("the claimed round is persisted for this Turn");
    assert_eq!(persisted, wait_key);
    let work = crate::api::wecom_progress::round_work_invocation(
        &persisted,
        run,
        &record.graph_digest,
        "assistant",
    )
    .expect("a durable wait key names the work invocation of its round");
    assert_eq!(work.node_id, "assistant");
    assert_eq!(work.invocation, 1);
    let binding = crate::assistant::binding(
        &state.data_root,
        &InvocationKey {
            node_id: "wait_input".into(),
            ..work.clone()
        },
    )
    .unwrap()
    .expect("the wait invocation of the round is bound");
    assert_eq!(binding.turn, turn);
    assert_eq!(binding.key.invocation, work.invocation);

    // Seed the work invocation's in-process trace exactly as a running Goose
    // node would: same durable-key stem, same session update shape.
    let work_key = InvocationKey {
        run_id: run.into(),
        graph_digest: record.graph_digest.clone(),
        node_id: "assistant".into(),
        invocation: 1,
    };
    let stem = format!("{:x}", Sha256::digest(work_key.durable_key().as_bytes()));
    let fact = state
        .data_root
        .join("goose-acp")
        .join(format!("{stem}.json"));
    std::fs::create_dir_all(fact.parent().unwrap()).unwrap();
    std::fs::write(&fact, b"{}").unwrap();
    let live = crate::goose_acp::LiveTrace::open(&fact).unwrap();
    live.restore("native-fixture", &[]).unwrap();
    live.observe(&json!({"method":"session/update","params":{
    "sessionId":"native-fixture","update":{
        "sessionUpdate":"tool_call","toolCallId":"call-1","status":"pending",
        "_meta":{"goose":{"toolCall":{"toolName":"anchor__anchor_run"}}},
        "rawInput":{"command":["sh","-c","ls -la /workspace"]}
    }}}))
    .unwrap();
    // Reasoning and visible narration are never projected.
    live.observe(&json!({"method":"session/update","params":{
    "sessionId":"native-fixture","update":{
        "sessionUpdate":"agent_thought_chunk","content":{"text":"secret plan /workspace"}
    }}}))
    .unwrap();

    // The endpoint resolves the assistant Turn instead of answering 404, and a
    // Turn that is still running does not settle.
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let request = Request::builder()
        .method("GET")
        .uri("/channels/wecom/progress/message-1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let mut body_text = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, body.next()).await {
        body_text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
    }
    assert!(!body_text.contains("event: settled"), "{body_text}");
    assert!(!body_text.contains("no such channel event"), "{body_text}");
    // A real tool call projects to the whitelisted line, category and step.
    assert!(body_text.contains("event: update"), "{body_text}");
    assert!(
        body_text.contains("\"content\":\"正在查看文件\""),
        "{body_text}"
    );
    assert!(
        body_text.contains("\"category\":\"read_file\""),
        "{body_text}"
    );
    assert!(body_text.contains("\"step\":1"), "{body_text}");
    // Arguments, paths, commands, tool names and reasoning never appear.
    for leaked in [
        "/workspace",
        "ls -la",
        "sh",
        "rawInput",
        "anchor__anchor_run",
        "toolCallId",
        "secret plan",
        "agent_thought_chunk",
    ] {
        assert!(!body_text.contains(leaked), "{leaked} leaked: {body_text}");
    }
    drop(live);
}

/// Status and the first SSE bytes of a stream that may still be open.
async fn call_stream_prefix(
    app: Router,
    uri: &str,
    wait: std::time::Duration,
) -> (StatusCode, String) {
    use futures_util::StreamExt;
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let mut body = response.into_body().into_data_stream();
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, body.next()).await {
        text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
    }
    (status, text)
}

/// The inbound row is persisted before the conversation is serialised, so a
/// message that arrives while another round still runs is already projectable
/// instead of answering 404.
#[tokio::test]
async fn a_message_is_projectable_while_the_previous_round_still_runs() {
    let (root, mut state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    unsafe {
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh,cat,printf,sleep");
    }
    state.wecom.reply_node = "reply".into();
    write_graph_bundle(
        &state.bundle_root,
        &json!({
            "objective":"deterministic WeCom serialisation fixture",
            "entry":"reply",
            "agents":{},
            "ops":{"reply":{"run":"sleep 3"}},
            "nodes":[{"id":"reply","op":"reply","plugins":[]}],
            "edges":[]
        }),
    )
    .unwrap();
    let app = router_with_web_root(state.clone(), root.path().join("web"));

    // The first round takes the conversation and keeps it for three seconds.
    let first_body = event("第一轮还在跑").to_string();
    let app_first = app.clone();
    let first = tokio::spawn(async move {
        call(
            app_first,
            "POST",
            "/channels/wecom/events",
            Some(&first_body),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let mut second = event("第二轮");
    second["event"]["event_id"] = json!("message-2");
    second["event"]["metadata"]["request_id"] = json!("callback-2");
    let app_second = app.clone();
    let second_body = second.to_string();
    let started = tokio::time::Instant::now();
    let pending = tokio::spawn(async move {
        call(
            app_second,
            "POST",
            "/channels/wecom/events",
            Some(&second_body),
        )
        .await
    });

    let mut observed = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2_500);
    while tokio::time::Instant::now() < deadline {
        let (status, _) = call_stream_prefix(
            app.clone(),
            "/channels/wecom/progress/message-2",
            std::time::Duration::from_millis(120),
        )
        .await;
        observed = Some(status);
        if status == StatusCode::OK {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }
    assert_eq!(
        observed,
        Some(StatusCode::OK),
        "an admitted message must be projectable while the previous round still runs"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(2_500),
        "the message became projectable only after the previous round released the conversation"
    );
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), first).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), pending).await;
}

/// A message whose Run was never admitted — the window a crash leaves behind —
/// stays a running Turn, survives a restart, is admitted only once, and is
/// still carried to the model when a newer message supersedes it.
#[tokio::test]
async fn an_admitted_message_without_a_run_is_not_finished_lost_or_duplicated() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let owner = "local";
    let inbound = |event_id: &str, text: &str| ChannelInboundRequest {
        inbound_id: inbound_id_for(event_id),
        identity: ChannelIdentity {
            source: "wecom".into(),
            account: None,
            conversation_id: "conversation-1".into(),
            sender_id: "user-1".into(),
        },
        graph: "fixture".into(),
        reply_node: "reply".into(),
        text: Some(text.into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
    };

    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let first = sessions
        .admit_channel_inbound(owner, inbound("crash-1", "先看这个"))
        .unwrap();
    let saved = sessions
        .get_channel_inbound(owner, &first.session.id, &inbound_id_for("crash-1"))
        .unwrap();
    assert!(saved.relation.run_id.is_none());
    assert_eq!(
        saved.turn.status,
        TurnStatus::Running,
        "a message without a Run is not a finished Turn"
    );

    // The fact survives a restart of the platform store.
    drop(sessions);
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    // The same event again admits the same Turn instead of a second one.
    let repeat = sessions
        .admit_channel_inbound(owner, inbound("crash-1", "先看这个"))
        .unwrap();
    assert_eq!(repeat.turn.id, first.turn.id);

    // A newer message supersedes the abandoned Turn and still carries its text.
    let second = sessions
        .admit_channel_inbound(owner, inbound("crash-2", "改成这个"))
        .unwrap();
    let superseded = sessions
        .get_channel_inbound(owner, &first.session.id, &inbound_id_for("crash-1"))
        .unwrap();
    assert_eq!(
        superseded.relation.superseded_by_turn_id.as_deref(),
        Some(second.turn.id.as_str())
    );
    let pending = sessions
        .pending_channel_messages(owner, &first.session.id, &inbound_id_for("crash-2"), 8)
        .unwrap();
    assert_eq!(
        pending.len(),
        1,
        "the message the crashed window left behind must still reach the model"
    );
    assert_eq!(pending[0].text, "先看这个");
}

/// The durable shape of one *running* persistent-assistant round.
///
/// The Session claimed the admitted message for a wait invocation, the Run is
/// executing the round, and the work invocation's fact path exists on disk —
/// the runner writes that fact before it opens the invocation's live trace.
/// Returns the work invocation the progress endpoint has to resolve.
fn seed_running_assistant_round(
    state: &ApiState,
    definition: &Value,
    event_id: &str,
    invocation: u64,
) -> (String, String, InvocationKey, std::path::PathBuf) {
    use anchor_runtime::graph::GraphSnapshot;
    use sha2::{Digest, Sha256};

    write_graph_bundle(&state.bundle_root, definition).unwrap();
    let owner = "local";
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let admission = sessions
        .admit_channel_inbound(
            owner,
            ChannelInboundRequest {
                inbound_id: inbound_id_for(event_id),
                identity: ChannelIdentity {
                    source: "wecom".into(),
                    account: None,
                    conversation_id: "conversation-1".into(),
                    sender_id: "user-1".into(),
                },
                graph: "fixture".into(),
                reply_node: "assistant".into(),
                text: Some("看看工作区".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: true,
            },
        )
        .unwrap();
    let session = admission.session.id.clone();
    let turn = admission.turn.id.clone();
    let run = "assistant-progress-fixture";
    let record = GraphRunRecord::create_with_id(
        GraphSnapshot::admit(definition.clone()).unwrap(),
        json!({"session":session}),
        run,
    )
    .unwrap();
    FileRunStore::new(state.data_root.join("runs"))
        .save(&record)
        .unwrap();
    sessions
        .bind_channel_assistant(owner, &session, run, "wait_input", "assistant", "reply")
        .unwrap();
    let wait_key = format!("{run}:{}:wait_input:{invocation}", record.graph_digest);
    let claimed = sessions
        .claim_channel_assistant_input(owner, &session, run, &wait_key)
        .unwrap()
        .expect("the running round claimed the admitted message");
    crate::assistant::bind(
        &state.data_root,
        &InvocationKey {
            run_id: run.into(),
            graph_digest: record.graph_digest.clone(),
            node_id: "wait_input".into(),
            invocation,
        },
        wait_key,
        &claimed,
        Vec::new(),
        false,
    )
    .unwrap();
    let mut saved = RunMetadata::new(
        run.into(),
        "fixture".into(),
        record.graph_digest.clone(),
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

    let work = InvocationKey {
        run_id: run.into(),
        graph_digest: record.graph_digest.clone(),
        node_id: "assistant".into(),
        invocation,
    };
    let stem = format!("{:x}", Sha256::digest(work.durable_key().as_bytes()));
    let fact = state
        .data_root
        .join("goose-acp")
        .join(format!("{stem}.json"));
    std::fs::create_dir_all(fact.parent().unwrap()).unwrap();
    std::fs::write(&fact, b"{}").unwrap();
    (session, turn, work, fact)
}

/// The assistant Graph of the WeCom product fixture, with its Plugin mounts
/// cleared so the round needs no service.
fn assistant_definition() -> Value {
    let mut definition: Value = serde_json::from_str(include_str!(
        "../../../../../examples/graphs/wecom-persistent-assistant.json"
    ))
    .unwrap();
    definition["nodes"][1]["plugins"] = json!([]);
    definition
}

/// Read the first bytes of a progress stream for `wait`.
async fn progress_prefix(
    state: &ApiState,
    web_root: std::path::PathBuf,
    wait: std::time::Duration,
) -> String {
    use futures_util::StreamExt;
    let app = router_with_web_root(state.clone(), web_root);
    let request = Request::builder()
        .method("GET")
        .uri("/channels/wecom/progress/message-1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, body.next()).await {
        text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
    }
    text
}

/// A live invocation that has only produced reasoning and narration is still a
/// running Turn: the user must see the guidance line instead of nothing at all.
#[tokio::test]
async fn persistent_assistant_progress_shows_guidance_from_narration_alone() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let definition = assistant_definition();
    let (_, _, _, fact) = seed_running_assistant_round(&state, &definition, "message-1", 1);

    let live = crate::goose_acp::LiveTrace::open(&fact).unwrap();
    live.restore("native-fixture", &[]).unwrap();
    live.observe(&json!({"method":"session/update","params":{
    "sessionId":"native-fixture","update":{
        "sessionUpdate":"agent_thought_chunk","content":{"text":"secret plan /workspace"}
    }}}))
    .unwrap();
    live.observe(&json!({"method":"session/update","params":{
    "sessionId":"native-fixture","update":{
        "sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"narration"}
    }}}))
    .unwrap();

    let body = progress_prefix(
        &state,
        root.path().join("web"),
        std::time::Duration::from_millis(800),
    )
    .await;
    assert!(body.contains("event: update"), "{body}");
    assert!(body.contains("\"seq\":1"), "{body}");
    assert!(body.contains("\"kind\":\"status\""), "{body}");
    assert!(body.contains("\"content\":\"正在准备…\""), "{body}");
    assert!(body.contains("\"category\":\"preparing\""), "{body}");
    assert!(body.contains("\"settled\":false"), "{body}");
    assert!(
        !body.contains("\"step\""),
        "guidance carries no step: {body}"
    );
    assert!(!body.contains("event: settled"), "{body}");
    for leaked in [
        "secret plan",
        "/workspace",
        "narration",
        "agent_thought_chunk",
        "agent_message_chunk",
    ] {
        assert!(!body.contains(leaked), "{leaked} leaked: {body}");
    }
    drop(live);
}

/// A round that is resolved but whose work invocation has not registered a live
/// trace yet (or has retained nothing yet) must neither claim the Turn is over
/// nor stall silently: it owes the guidance line, and the first real tool step
/// then continues the same stream instead of repeating or renumbering it.
#[tokio::test]
async fn persistent_assistant_progress_reports_before_the_live_trace_registers() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let definition = assistant_definition();
    let (_, _, _, fact) = seed_running_assistant_round(&state, &definition, "message-1", 1);

    // The work node reaches its prompt a little later, exactly as it does in a
    // real round: the fact exists, the live trace does not.
    let registration = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let live = crate::goose_acp::LiveTrace::open(&fact).unwrap();
        live.restore("native-fixture", &[]).unwrap();
        live.observe(&json!({"method":"session/update","params":{
        "sessionId":"native-fixture","update":{
            "sessionUpdate":"tool_call","toolCallId":"call-1","status":"pending",
            "_meta":{"goose":{"toolCall":{"toolName":"anchor__anchor_run"}}},
            "rawInput":{"command":["sh","-c","ls -la /workspace"]}
        }}}))
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        drop(live);
    });

    let body = progress_prefix(
        &state,
        root.path().join("web"),
        std::time::Duration::from_secs(2),
    )
    .await;
    // The guidance line comes from the resolved round alone...
    assert!(body.contains("event: update"), "{body}");
    assert!(body.contains("\"seq\":1"), "{body}");
    assert!(body.contains("\"content\":\"正在准备…\""), "{body}");
    assert!(body.contains("\"category\":\"preparing\""), "{body}");
    assert_eq!(
        body.matches("正在准备…").count(),
        1,
        "the guidance line must not be repeated: {body}"
    );
    // ...and the real step continues the same stream at the next position.
    assert!(body.contains("\"seq\":2"), "{body}");
    assert!(body.contains("\"content\":\"正在查看文件\""), "{body}");
    assert!(body.contains("\"category\":\"read_file\""), "{body}");
    assert!(body.contains("\"step\":1"), "{body}");
    // A running Turn is never reported as settled.
    assert!(!body.contains("event: settled"), "{body}");
    for leaked in ["/workspace", "ls -la", "anchor__anchor_run", "toolCallId"] {
        assert!(!body.contains(leaked), "{leaked} leaked: {body}");
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), registration).await;
}
