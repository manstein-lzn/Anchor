use super::*;
use crate::{
    api::wecom as intake,
    application::channel_management::{ChannelPlatform, fake::FakeChannels},
};
use anchor_platform_session::{ChannelConversationKind, ChannelDeliveryStatus};
use base64::{Engine as _, engine::general_purpose::STANDARD};

const CALLBACK: &str = "anchor-channel-platform-fixture";

fn feishu_event(text: &str) -> Value {
    json!({"event": {
        "source":"feishu", "event_id":"om_message-1", "sender_id":"ou_user-1",
        "conversation_id":"oc_chat-1", "text":text, "reply_target":"oc_chat-1",
        "message_type":"text",
        "metadata":{"chat_type":"single", "request_id":"om_message-1"},
        "attachments":[]
    }})
}

fn install_bot(state: &mut ApiState, platform: ChannelPlatform) -> Arc<FakeChannels> {
    let mut bot = FakeChannels::bot("fixture", "local", "assistant", &["*"]);
    bot.platform = platform;
    bot.binding.plugin = platform.as_str().into();
    bot.binding.private_only = platform == ChannelPlatform::Feishu;
    let port = Arc::new(
        FakeChannels::new()
            .with(bot)
            .with_callback("fixture", CALLBACK),
    );
    state.application = state
        .application
        .clone()
        .with_channel_management(port.clone());
    port
}

#[test]
fn channel_inbound_hashes_preserve_wecom_history_and_separate_feishu() {
    for (source, expected) in [
        (
            "wecom",
            "wecom-e7bd8ba60407cf0b54aa515d669078eddc740c10276f7e1aadf7fe34f0cd9e36",
        ),
        (
            "feishu",
            "feishu-33fba02bb4ba9203f3f9733ac06177c75d3e553e1431dc176a1c20059c0d8e5c",
        ),
    ] {
        assert_eq!(intake::inbound_id_for(source, "message-1"), expected);
    }
}

#[tokio::test]
async fn channel_platform_is_bound_to_the_callback_for_receiving_and_settlement() {
    for platform in [ChannelPlatform::Wecom, ChannelPlatform::Feishu] {
        let (root, mut state) = fixture();
        install_bot(&mut state, platform);
        let app = router_with_web_root(state.clone(), root.path().join("web"));
        let mut body = if platform == ChannelPlatform::Wecom {
            feishu_event("hello")
        } else {
            event("hello")
        };
        for settlement in [false, true] {
            if settlement {
                body["settlement"] = json!({
                    "key":"channel-wecom-reply:irrelevant", "content_sha256":"a".repeat(64),
                    "status":"confirmed"
                });
            }
            let (status, refused) = authorized_call(
                app.clone(),
                "POST",
                "/channels/fixture/events",
                CALLBACK,
                body.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
            assert!(refused["error"].as_str().unwrap().contains("platform"));
        }
        assert!(state.application.records().unwrap().is_empty());
        assert!(!state.data_root.join("platform/sessions.sqlite").exists());
    }
}

#[tokio::test]
async fn feishu_callback_rejects_foreign_shapes_and_media_without_admission_bytes() {
    let (root, mut state) = fixture();
    install_bot(&mut state, ChannelPlatform::Feishu);
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let mut invalid = Vec::new();
    for (field, value) in [
        ("chat_type", "group"),
        ("chat_type", "p2p"),
        ("request_id", "another-message"),
    ] {
        let mut body = feishu_event("hello");
        body["event"]["metadata"][field] = json!(value);
        invalid.push(body);
    }
    for kind in ["voice", "mixed"] {
        let mut body = feishu_event("caption");
        body["event"]["message_type"] = json!(kind);
        body["event"]["attachments"] =
            json!([{"name":"note.txt", "data_base64":"aGVsbG8=", "media_type":"text/plain"}]);
        invalid.push(body);
    }
    let mut body = feishu_event("text must not smuggle attachments");
    body["event"]["attachments"] = json!([{"name":"note.txt", "data_base64":"aGVsbG8="}]);
    invalid.push(body);
    let mut body = feishu_event("hello");
    body["event"]["reply_target"] = json!("oc_another-chat");
    invalid.push(body);
    let mut body = feishu_event("hello");
    body["event"]["sender_id"] = json!("");
    invalid.push(body);

    for mut body in invalid {
        for settlement in [false, true] {
            if settlement {
                body["settlement"] = json!({
                    "key":"channel-feishu-reply:irrelevant", "content_sha256":"a".repeat(64),
                    "status":"confirmed"
                });
            }
            let (status, refused) = authorized_call(
                app.clone(),
                "POST",
                "/channels/fixture/events",
                CALLBACK,
                body.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {refused}");
        }
    }
    for kind in ["image", "file"] {
        let mut body = feishu_event("a caption does not replace media bytes");
        body["event"]["message_type"] = json!(kind);
        let (status, refused) = authorized_call(
            app.clone(),
            "POST",
            "/channels/fixture/events",
            CALLBACK,
            body,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    }
    assert!(state.application.records().unwrap().is_empty());
    assert!(!state.data_root.join("channel-inputs").exists());
    assert!(!state.data_root.join("platform/sessions.sqlite").exists());
}

#[tokio::test]
async fn feishu_admission_requires_both_inbound_and_reply_recipient_authorization() {
    let (root, mut state) = fixture();
    let port = install_bot(&mut state, ChannelPlatform::Feishu);
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    for (users, send_users) in [
        (vec!["another-user".into()], vec!["*".into()]),
        (vec!["*".into()], vec!["another-user".into()]),
    ] {
        port.update("fixture", |bot| {
            bot.binding.users = users;
            bot.binding.send_users = send_users;
        });
        let (status, refused) = authorized_call(
            app.clone(),
            "POST",
            "/channels/fixture/events",
            CALLBACK,
            feishu_event("hello"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    }
    assert!(state.application.records().unwrap().is_empty());
    assert!(!state.data_root.join("platform/sessions.sqlite").exists());
}

#[tokio::test]
async fn both_platforms_share_admission_and_keep_distinct_historical_receipts() {
    let _env = PROCESS_ENV.lock().await;
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    for (platform, kind, attachments) in [
        (ChannelPlatform::Wecom, "text", json!([])),
        (ChannelPlatform::Feishu, "text", json!([])),
        (
            ChannelPlatform::Feishu,
            "file",
            json!([{"name":"input.txt","data_base64":STANDARD.encode(b"private feishu input"),"media_type":"text/plain"}]),
        ),
        (
            ChannelPlatform::Feishu,
            "image",
            json!([{"name":"input.png","data_base64":STANDARD.encode(png.get_ref()),"media_type":"image/png"}]),
        ),
    ] {
        let (root, mut state) = fixture();
        set_host_env(root.path());
        let port = install_bot(&mut state, platform);
        let mut body = if platform == ChannelPlatform::Wecom {
            event("hello")
        } else {
            feishu_event("hello")
        };
        body["event"]["message_type"] = json!(kind);
        body["event"]["attachments"] = attachments;
        let (session, turn, work, _) =
            seed_running_assistant_round_for_event(&state, &assistant_definition(), &body, 1);
        let sessions =
            SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
        let inbound_id = intake::inbound_id_for(
            platform.as_str(),
            body["event"]["event_id"].as_str().unwrap(),
        );
        let input = sessions
            .get_channel_inbound("local", &session, &inbound_id)
            .unwrap();
        let reply_key = InvocationKey {
            node_id: "reply".into(),
            ..work.clone()
        };
        for key in [&work, &reply_key] {
            crate::assistant::bind(
                &state.data_root,
                key,
                format!("{}:{}:wait_input:1", work.run_id, work.graph_digest),
                &anchor_platform_session::AssistantInput::User(Box::new(input.clone())),
                Vec::new(),
                false,
                None,
            )
            .unwrap();
        }
        fixtures::complete(
            &state.application,
            &work.run_id,
            "wait_input",
            fixtures::completion(json!({"turn":turn})),
            None,
        );
        let commit = fixtures::complete(
            &state.application,
            &work.run_id,
            "assistant",
            fixtures::completion(json!({})),
            None,
        );
        let mut completion = fixtures::completion(json!({
            "reply_for":turn, "source_commit":commit, "items":[]
        }));
        completion.submission = "shared channel reply".into();
        fixtures::complete(&state.application, &work.run_id, "reply", completion, None);

        let app = router_with_web_root(state.clone(), root.path().join("web"));
        let (status, reply) = authorized_call(
            app.clone(),
            "POST",
            "/channels/fixture/events",
            CALLBACK,
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert_eq!(reply["text"], "shared channel reply");
        assert_eq!(reply["session"], session);
        assert_eq!(reply["run"], work.run_id);
        let expected_key = format!("channel-{}-reply:{session}:{inbound_id}", platform.as_str());
        assert_eq!(reply["receipt"]["key"], expected_key);
        assert!(reply.get("msg_item").is_none());
        let admitted = sessions
            .get_channel_inbound("local", &session, &inbound_id)
            .unwrap();
        assert_eq!(admitted.request.identity.source, platform.as_str());
        assert_eq!(
            admitted.request.identity.conversation_kind,
            ChannelConversationKind::Single
        );
        assert_eq!(
            admitted.request.identity.sender_id,
            body["event"]["sender_id"]
        );
        assert_eq!(
            admitted.request.identity.conversation_id,
            body["event"]["conversation_id"]
        );
        assert_eq!(
            admitted.request.attachments.files.len(),
            if kind == "text" { 0 } else { 1 }
        );
        let selected = crate::assistant::attachment_metadata_for_input(
            &metadata::load(&state.data_root, &work.run_id)
                .unwrap()
                .unwrap(),
            &admitted,
        );
        let files = crate::channel_inputs::node_inputs(
            &state.data_root,
            &selected,
            &InvocationKey {
                run_id: selected.run_id.clone(),
                ..work.clone()
            },
        )
        .unwrap();
        if let Some(mount) = files.mount {
            assert_eq!(mount.destination, std::path::Path::new("/in/channel"));
            let file = &admitted.request.attachments.files[0];
            let bytes = STANDARD
                .decode(
                    body["event"]["attachments"][0]["data_base64"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
            assert_eq!(std::fs::read(mount.source.join(&file.name)).unwrap(), bytes);
            assert!(
                mount.source.starts_with(
                    state
                        .data_root
                        .join("channel-inputs")
                        .join(format!("turn-{turn}"))
                )
            );
            assert_eq!(std::fs::read_dir(mount.source).unwrap().count(), 1);
        } else {
            assert_eq!(kind, "text");
        }
        assert_eq!(admitted.turn.id, turn);
        assert_eq!(admitted.turn.status, TurnStatus::Completed);
        assert_eq!(sessions.list_turns("local", &session).unwrap().len(), 1);
        let deliveries = sessions
            .list_unfinished_channel_deliveries("local", &session)
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].kind, format!("{}_reply", platform.as_str()));
        assert_eq!(deliveries[0].status, ChannelDeliveryStatus::Sending);

        // A historical receipt remains settleable by its owner after admission
        // settings change; it does not authorize another input or a new send.
        port.update("fixture", |bot| {
            bot.enabled = false;
            bot.reply_node = None;
            bot.account = Some("changed-account".into());
            bot.binding.users.clear();
            bot.binding.send_users.clear();
        });
        let mut settlement = json!({"event":body["event"], "settlement":{
            "key":reply["receipt"]["key"], "content_sha256":reply["receipt"]["content_sha256"],
            "status":"confirmed"
        }});
        // The original media resource need not remain available to settle ACKs.
        settlement["event"]["attachments"] = json!([]);
        for _ in 0..2 {
            let (status, settled) = call(
                app.clone(),
                "POST",
                "/channels/fixture/events",
                Some(&settlement.to_string()),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{settled}");
            assert_eq!(settled["delivery"]["key"], expected_key);
            assert_eq!(settled["delivery"]["status"], "confirmed");
        }
        assert!(
            sessions
                .list_unfinished_channel_deliveries("local", &session)
                .unwrap()
                .is_empty()
        );
    }
}
