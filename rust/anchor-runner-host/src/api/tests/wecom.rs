use super::*;
use anchor_platform_session::{ChannelDeliveryStatus, SessionStore};

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

fn inbound_id() -> String {
    let mut digest = Sha256::new();
    digest.update(b"anchor-wecom-inbound-v1\0");
    digest.update(b"wecom");
    digest.update([0]);
    digest.update(b"message-1");
    format!("wecom-{:x}", digest.finalize())
}
