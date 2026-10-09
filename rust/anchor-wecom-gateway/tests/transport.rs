mod support;

use std::{
    os::unix::fs::PermissionsExt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anchor_wecom_gateway::{
    ConnectionStatus, DeliveryStatus, Gateway, GatewayError, RunningGateway,
};
use serde_json::{Value, json};
use support::{
    HttpReply, MediaServer, Platform, ProgressFrame, ProgressServer, TOKEN, WebhookServer,
    callback, config, config_with_progress, control, media_callback, raw_control, send_request,
};

async fn wait_fact(gateway: &RunningGateway, request_id: &str, status: DeliveryStatus) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if gateway
                .delivery_facts()
                .unwrap()
                .iter()
                .any(|fact| fact.request_id == request_id && fact.status == status)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_settlement_delivered(state_dir: &std::path::Path, key: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let connection = rusqlite::Connection::open(state_dir.join("delivery.sqlite")).unwrap();
            let delivered: bool = connection
                .query_row(
                    "SELECT delivered FROM settlement_outbox WHERE receipt_key=?1",
                    [key],
                    |row| row.get(0),
                )
                .unwrap();
            if delivered {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

/// Which progress streams the fixture transport stopped early.
fn closed_streams(progress: &ProgressServer) -> Vec<String> {
    progress.closed.lock().unwrap().clone()
}

async fn wait_stream_requested(progress: &ProgressServer, event_id: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if progress
                .requested
                .lock()
                .unwrap()
                .iter()
                .any(|requested| requested == event_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_stream_closed(progress: &ProgressServer, event_id: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if closed_streams(progress)
                .iter()
                .any(|closed| closed == event_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn binary_json_auth_callback_and_reply_ack_keep_the_connection_and_delivery_identity() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(None).await;
    let webhook = WebhookServer::new(|event| {
        HttpReply::ok(json!({"text":format!("reply: {}", event["text"].as_str().unwrap())}))
    })
    .await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    let auth = platform.next("aibot_subscribe").await;
    platform
        .send_binary(json!({"headers":auth["headers"],"errcode":0}))
        .await;
    gateway
        .wait_authenticated(Duration::from_secs(1))
        .await
        .unwrap();
    let first = callback("binary-1", "alice", "连接验收");
    platform.send_binary(first.clone()).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["headers"], first["headers"]);
    assert_eq!(reply["body"]["stream"]["content"], "reply: 连接验收");
    platform
        .send_binary(json!({"headers":reply["headers"],"errcode":0}))
        .await;
    wait_fact(&gateway, "binary-1", DeliveryStatus::Confirmed).await;
    platform.send_binary(first).await;
    platform.no_command("aibot_respond_msg").await;
    platform
        .send(callback("text-after-binary", "alice", "继续"))
        .await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "reply: 继续");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "text-after-binary", DeliveryStatus::Confirmed).await;
    assert_eq!(gateway.status(), ConnectionStatus::Authenticated);
    platform.no_command("aibot_subscribe").await;
    assert_eq!(webhook.events.lock().unwrap().len(), 2);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn control_status_requires_authentication_and_observes_connection_without_sending() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(None).await;
    let mut settings = config(root.path(), &platform, None);
    settings.timing.ack_timeout = Duration::from_secs(2);
    let path = settings.socket_path();
    let gateway = Gateway::start(settings).await.unwrap();
    let auth = platform.next("aibot_subscribe").await;
    let request = json!({"operation":"status"});
    assert_eq!(
        control(&path, "wrong-token", request.clone()).await,
        json!({"error":"unauthorized channel request"})
    );
    assert_eq!(
        control(&path, TOKEN, request.clone()).await,
        json!({"status":"authenticating"})
    );
    platform.acknowledge(&auth, 0).await;
    gateway
        .wait_authenticated(Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(
        control(&path, TOKEN, request.clone()).await,
        json!({"status":"authenticated"})
    );
    platform.close().await;
    let reconnect = platform.next("aibot_subscribe").await;
    assert_eq!(
        control(&path, TOKEN, request.clone()).await,
        json!({"status":"authenticating"})
    );
    platform.acknowledge(&reconnect, 0).await;
    gateway
        .wait_authenticated(Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(
        control(&path, TOKEN, request).await,
        json!({"status":"authenticated"})
    );
    assert!(gateway.delivery_facts().unwrap().is_empty());
    platform.no_command("aibot_send_msg").await;
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn auth_requires_matching_ack_heartbeat_uses_ping_and_normal_close_reconnects() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(None).await;
    let mut settings = config(root.path(), &platform, None);
    settings.timing.heartbeat_interval = Duration::from_millis(25);
    settings.timing.ack_timeout = Duration::from_millis(80);
    let path = settings.socket_path();
    let gateway = Gateway::start(settings).await.unwrap();
    let auth = platform.next("aibot_subscribe").await;
    assert_eq!(
        auth["body"],
        json!({"bot_id":"fixture-bot","secret":"fixture-secret"})
    );
    platform
        .send(json!({"headers":{"req_id":"not-the-auth-request"},"errcode":0}))
        .await;
    assert_eq!(
        gateway.wait_authenticated(Duration::from_millis(15)).await,
        Err(GatewayError::Disconnected)
    );
    let rejected = control(&path, TOKEN, send_request("before-auth", "alice", "hello")).await;
    assert!(
        rejected["error"]
            .as_str()
            .unwrap()
            .contains("not authenticated")
    );
    assert!(gateway.delivery_facts().unwrap().is_empty());
    platform.acknowledge(&auth, 0).await;
    gateway
        .wait_authenticated(Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(gateway.status(), ConnectionStatus::Authenticated);
    let ping = platform.next("ping").await;
    assert_ne!(ping["headers"]["req_id"], auth["headers"]["req_id"]);
    platform.acknowledge_ping.store(false, Ordering::SeqCst);
    platform.next("ping").await;
    let reconnect = platform.next("aibot_subscribe").await;
    assert_ne!(reconnect["headers"]["req_id"], auth["headers"]["req_id"]);
    platform.acknowledge_ping.store(true, Ordering::SeqCst);
    platform.acknowledge(&reconnect, 0).await;
    gateway
        .wait_authenticated(Duration::from_secs(1))
        .await
        .unwrap();
    platform.close().await;
    let normal_reconnect = platform.next("aibot_subscribe").await;
    platform.acknowledge(&normal_reconnect, 0).await;
    gateway
        .wait_authenticated(Duration::from_secs(1))
        .await
        .unwrap();
    gateway.shutdown().await.unwrap();
    assert!(!path.exists());
    assert!(!root.path().join("state/control.json").exists());
}

#[tokio::test]
async fn confirmed_send_is_bound_to_identity_content_and_survives_token_rotation() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settings = config(root.path(), &platform, None);
    let path = settings.socket_path();
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    for filename in ["control.json", "control.sock", "delivery.sqlite"] {
        assert_eq!(
            std::fs::metadata(root.path().join("state").join(filename))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let request = send_request("request-1", "alice", "authorized notification");
    let wrong_token = control(&path, "wrong-secret-not-echoed", request.clone()).await;
    assert_eq!(wrong_token, json!({"error":"unauthorized channel request"}));
    let prohibited = control(&path, TOKEN, send_request("broadcast", "@all", "hello")).await;
    assert!(
        prohibited["error"]
            .as_str()
            .unwrap()
            .contains("not allowed")
    );
    let pending_path = path.clone();
    let pending_request = request.clone();
    let pending = tokio::spawn(async move { control(&pending_path, TOKEN, pending_request).await });
    let wire = platform.next("aibot_send_msg").await;
    assert_eq!(
        wire["body"],
        json!({"chatid":"alice","chat_type":1,"msgtype":"markdown",
        "markdown":{"content":"authorized notification"}})
    );
    platform
        .send(json!({"headers":{"req_id":"wrong-wire-id"},"errcode":0}))
        .await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!pending.is_finished());
    platform.acknowledge(&wire, 0).await;
    assert_eq!(
        pending.await.unwrap(),
        json!({"accepted":true,"request_id":"request-1"})
    );
    let duplicate = control(&path, TOKEN, request.clone()).await;
    assert_eq!(
        duplicate,
        json!({"accepted":true,"request_id":"request-1","duplicate":true})
    );
    for changed in [
        send_request("request-1", "bob", "authorized notification"),
        send_request("request-1", "alice", "changed notification"),
    ] {
        assert!(
            control(&path, TOKEN, changed).await["error"]
                .as_str()
                .unwrap()
                .contains("different content or identity")
        );
    }
    assert_eq!(
        gateway.delivery_facts().unwrap()[0].status,
        DeliveryStatus::Confirmed
    );
    gateway.shutdown().await.unwrap();
    let mut settings = config(root.path(), &platform, None);
    let rotated = "rotated-control-token-000000000000000000000";
    settings.control_token = rotated.into();
    let reopened = Gateway::start(settings).await.unwrap();
    reopened
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    let descriptor: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("state/control.json")).unwrap())
            .unwrap();
    assert_eq!(descriptor["token"], rotated);
    assert_eq!(control(&path, rotated, request).await["duplicate"], true);
    assert_eq!(reopened.delivery_facts().unwrap().len(), 1);
    platform.no_command("aibot_send_msg").await;
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn timeout_rejection_and_disconnect_stay_unknown_across_reconnect_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settings = config(root.path(), &platform, None);
    let path = settings.socket_path();
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    for failure in ["timeout", "rejected", "disconnected"] {
        let request = send_request(failure, "alice", "uncertain notification");
        let pending_path = path.clone();
        let pending_request = request.clone();
        let pending =
            tokio::spawn(async move { control(&pending_path, TOKEN, pending_request).await });
        let wire = platform.next("aibot_send_msg").await;
        match failure {
            "rejected" => platform.acknowledge(&wire, 400).await,
            "disconnected" => platform.close().await,
            _ => {}
        }
        let outcome = pending.await.unwrap();
        assert!(outcome["error"].as_str().unwrap().contains("unconfirmed"));
        if failure == "disconnected" {
            platform.next("aibot_subscribe").await;
            gateway
                .wait_authenticated(Duration::from_secs(2))
                .await
                .unwrap();
        } else {
            platform.acknowledge(&wire, 0).await;
        }
        assert!(
            control(&path, TOKEN, request).await["error"]
                .as_str()
                .unwrap()
                .contains("previous delivery")
        );
        wait_fact(&gateway, failure, DeliveryStatus::Unknown).await;
    }
    assert_eq!(gateway.delivery_facts().unwrap().len(), 3);
    gateway.shutdown().await.unwrap();
    let reopened = Gateway::start(config(root.path(), &platform, None))
        .await
        .unwrap();
    reopened
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    for failure in ["timeout", "rejected", "disconnected"] {
        let request = send_request(failure, "alice", "uncertain notification");
        assert!(
            control(&path, TOKEN, request).await["error"]
                .as_str()
                .unwrap()
                .contains("previous delivery")
        );
    }
    assert!(
        reopened
            .delivery_facts()
            .unwrap()
            .iter()
            .all(|fact| fact.status == DeliveryStatus::Unknown)
    );
    platform.no_command("aibot_send_msg").await;
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn text_and_mixed_callbacks_use_real_webhook_reply_ack_and_deduplicate() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        HttpReply::ok(json!({"text":format!("reply: {}", event["text"].as_str().unwrap())}))
    })
    .await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    let mut errors = gateway.subscribe_errors();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    let first = callback("text-1", "alice", "hello");
    platform.send(first.clone()).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["headers"], first["headers"]);
    assert_eq!(reply["body"]["msgtype"], "stream");
    assert_eq!(reply["body"]["stream"]["content"], "reply: hello");
    assert_eq!(reply["body"]["stream"]["finish"], true);
    assert_eq!(reply["body"]["stream"]["id"].as_str().unwrap().len(), 39);
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "text-1", DeliveryStatus::Confirmed).await;
    platform.send(first.clone()).await;
    let mut changed = first;
    changed["body"]["from"]["userid"] = json!("bob");
    platform.send(changed).await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), errors.recv())
            .await
            .unwrap()
            .unwrap(),
        GatewayError::Conflict(_)
    ));
    let mut mixed = callback("mixed-1", "bob", "unused");
    mixed["body"] = json!({"msgid":"mixed-1","from":{"userid":"bob"},"msgtype":"mixed",
        "mixed":{"msg_item":[{"msgtype":"text","text":{"content":"first"}},
            {"msgtype":"text","text":{"content":"second"}}]}});
    platform.send(mixed.clone()).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["headers"], mixed["headers"]);
    assert_eq!(reply["body"]["stream"]["content"], "reply: first\nsecond");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "mixed-1", DeliveryStatus::Confirmed).await;
    platform.send(mixed).await;
    platform.no_command("aibot_respond_msg").await;
    {
        let events = webhook.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["source"], "wecom");
        assert_eq!(events[1]["text"], "first\nsecond");
        assert_eq!(events[1]["attachments"], json!([]));
        assert_eq!(events[1]["metadata"]["request_id"], "callback-mixed-1");
    }
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn saved_unsent_reply_rechecks_current_inbound_permission_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settings = config(root.path(), &platform, None);
    let directory = settings.state_dir.clone();
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    gateway.shutdown().await.unwrap();
    let connection = rusqlite::Connection::open(directory.join("delivery.sqlite")).unwrap();
    connection.execute("INSERT INTO inbound(event_id,digest,callback_id,sender_id,conversation_id,state,reply)
        VALUES('saved','fixture-digest','callback-saved','alice','alice','ready','private saved reply')", []).unwrap();
    drop(connection);
    let mut revoked = config(root.path(), &platform, None);
    revoked.inbound_users.clear();
    let reopened = Gateway::start(revoked).await.unwrap();
    let mut errors = reopened.subscribe_errors();
    reopened
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), errors.recv())
            .await
            .unwrap()
            .unwrap(),
        GatewayError::Invalid("saved reply sender is no longer allowed")
    );
    platform.no_command("aibot_respond_msg").await;
    assert!(reopened.delivery_facts().unwrap().is_empty());
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn callback_with_lost_reply_ack_is_not_replayed_or_reexecuted_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(json!({"reply":"saved final reply"}))).await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    let input = callback("lost-reply", "alice", "hello");
    platform.send(input.clone()).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["finish"], true);
    platform.close().await;
    platform.next("aibot_subscribe").await;
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    wait_fact(&gateway, "lost-reply", DeliveryStatus::Unknown).await;
    platform.send(input.clone()).await;
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
    let reopened = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    reopened
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    platform.send(input).await;
    platform.no_command("aibot_respond_msg").await;
    assert_eq!(webhook.events.lock().unwrap().len(), 1);
    assert_eq!(reopened.delivery_facts().unwrap().len(), 1);
    assert_eq!(
        reopened.delivery_facts().unwrap()[0].status,
        DeliveryStatus::Unknown
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn slow_callback_does_not_block_control_and_older_reply_is_suppressed() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        let text = event["text"].as_str().unwrap();
        let mut reply = HttpReply::ok(json!({"text":text}));
        if text == "old" {
            reply.delay = Duration::from_millis(250);
        }
        reply
    })
    .await;
    let settings = config(root.path(), &platform, Some(&webhook));
    let path = settings.socket_path();
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    platform.send(callback("old", "alice", "old")).await;
    webhook.wait_events(1).await;
    let pending_path = path.clone();
    let pending = tokio::spawn(async move {
        control(
            &pending_path,
            TOKEN,
            send_request("during-webhook", "bob", "notification"),
        )
        .await
    });
    let send = platform.next("aibot_send_msg").await;
    platform.acknowledge(&send, 0).await;
    assert_eq!(pending.await.unwrap()["accepted"], true);
    platform.send(callback("new", "alice", "new")).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "new");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "new", DeliveryStatus::Confirmed).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    platform.no_command("aibot_respond_msg").await;
    assert_eq!(webhook.events.lock().unwrap().len(), 2);
    assert_eq!(gateway.delivery_facts().unwrap().len(), 2);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn media_and_untrusted_callbacks_are_rejected_before_webhook() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook =
        WebhookServer::new(|_| HttpReply::ok(json!({"text":"must not be invoked"}))).await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    let mut errors = gateway.subscribe_errors();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    let mut media = callback("media", "alice", "ignored text");
    media["body"]["msgtype"] = json!("mixed");
    media["body"]["mixed"] = json!({"msg_item":[{"msgtype":"text","text":{"content":"ignored text"}},
        {"msgtype":"image","image":{"url":"https://example.invalid/private"}}]});
    platform.send(media).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), errors.recv())
            .await
            .unwrap()
            .unwrap(),
        GatewayError::Invalid("media callback key is missing")
    );
    // A plaintext non-loopback URL is never a platform download.
    let mut plain = callback("plain", "alice", "");
    plain["body"]["msgtype"] = json!("file");
    plain["body"].as_object_mut().unwrap().remove("text");
    plain["body"]["file"] = json!({
        "url":"http://example.invalid/private",
        "aeskey":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
    });
    platform.send(plain).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), errors.recv())
            .await
            .unwrap()
            .unwrap(),
        GatewayError::Invalid("media callback URL is missing or unsupported")
    );
    platform
        .send(callback("untrusted", "mallory", "not admitted"))
        .await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), errors.recv())
            .await
            .unwrap()
            .unwrap(),
        GatewayError::Invalid("callback sender is not allowed")
    );
    platform.no_command("aibot_respond_msg").await;
    assert!(webhook.events.lock().unwrap().is_empty());
    assert!(gateway.delivery_facts().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn group_callbacks_are_rejected_before_host_webhook() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook =
        WebhookServer::new(|_| HttpReply::ok(json!({"text":"must not be invoked"}))).await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    let mut errors = gateway.subscribe_errors();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    let mut group = callback("group-message", "alice", "hello");
    group["body"]["chattype"] = json!("group");
    group["body"]["chatid"] = json!("group-1");
    platform.send(group).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), errors.recv())
            .await
            .unwrap()
            .unwrap(),
        GatewayError::Invalid("only private WeCom callbacks are supported")
    );
    platform.no_command("aibot_respond_msg").await;
    assert!(webhook.events.lock().unwrap().is_empty());
    assert!(gateway.delivery_facts().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn webhook_media_error_and_oversized_replies_are_not_silently_downgraded() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        let mut reply = match event["event_id"].as_str().unwrap() {
            "media" => HttpReply::ok(
                json!({"text":"do not return only this text", "msg_item":[{"msgtype":"image"}]}),
            ),
            "failed" => HttpReply::ok(json!({"error":"fixture-secret-must-not-escape"})),
            _ => {
                HttpReply::ok(json!({"text":"a".repeat(anchor_wecom_gateway::MAX_TEXT_BYTES + 1)}))
            }
        };
        // Slow enough that the progress bubble is shown before the failure.
        reply.delay = Duration::from_millis(400);
        reply
    })
    .await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_delay = Duration::from_millis(50);
    let gateway = Gateway::start(settings).await.unwrap();
    let mut errors = gateway.subscribe_errors();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    for event_id in ["media", "failed", "oversized"] {
        let input = callback(event_id, "alice", "hello");
        platform.send(input.clone()).await;
        let processing = platform.next("aibot_respond_msg").await;
        assert_eq!(processing["body"]["stream"]["content"], "⏳ 正在处理…");
        assert_eq!(processing["body"]["stream"]["finish"], false);
        platform.acknowledge(&processing, 0).await;
        let error = tokio::time::timeout(Duration::from_secs(1), errors.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!error.to_string().contains("fixture-secret"));
        if event_id == "media" {
            assert_eq!(error, GatewayError::UnsupportedMedia);
        }
        if event_id == "failed" {
            assert_eq!(error, GatewayError::Webhook);
        }
        let failure = platform.next_final_response().await;
        assert_eq!(
            failure["body"]["stream"]["content"],
            "本次处理未能完成，请稍后补充一条消息继续；已执行的操作不会自动撤销。"
        );
        assert_eq!(failure["body"]["stream"]["finish"], true);
        platform.acknowledge(&failure, 0).await;
        platform.send(input).await;
    }
    platform.no_command("aibot_respond_msg").await;
    assert_eq!(webhook.events.lock().unwrap().len(), 3);
    assert!(gateway.delivery_facts().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_webhook_is_retried_after_restart_without_reply_claim() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let webhook_attempts = attempts.clone();
    let webhook = WebhookServer::new(move |_| {
        if webhook_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut reply = HttpReply::ok(json!({}));
            reply.status = axum::http::StatusCode::SERVICE_UNAVAILABLE;
            reply.delay = Duration::from_millis(400);
            reply
        } else {
            HttpReply::ok(json!({"text":"recovered reply"}))
        }
    })
    .await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_delay = Duration::from_millis(50);
    let gateway = Gateway::start(settings).await.unwrap();
    let mut errors = gateway.subscribe_errors();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    let input = callback("failed-webhook", "alice", "hello");
    platform.send(input.clone()).await;
    let processing = platform.next("aibot_respond_msg").await;
    assert_eq!(processing["body"]["stream"]["content"], "⏳ 正在处理…");
    assert_eq!(processing["body"]["stream"]["finish"], false);
    platform.acknowledge(&processing, 0).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), errors.recv())
            .await
            .unwrap()
            .unwrap(),
        GatewayError::Webhook
    );
    let failure = platform.next_final_response().await;
    assert_eq!(
        failure["body"]["stream"]["content"],
        "本次处理未能完成，请稍后补充一条消息继续；已执行的操作不会自动撤销。"
    );
    assert_eq!(failure["body"]["stream"]["finish"], true);
    platform.acknowledge(&failure, 0).await;
    assert_eq!(webhook.events.lock().unwrap().len(), 1);
    assert!(gateway.delivery_facts().unwrap().is_empty());
    gateway.shutdown().await.unwrap();

    let reopened = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    reopened
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    webhook.wait_events(2).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "recovered reply");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&reopened, "failed-webhook", DeliveryStatus::Confirmed).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2);

    platform.send(input).await;
    platform.no_command("aibot_respond_msg").await;
    assert_eq!(webhook.events.lock().unwrap().len(), 2);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupted_webhook_is_recovered_after_gateway_restart_without_duplicate_reply() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let webhook_attempts = attempts.clone();
    let webhook = WebhookServer::new(move |_| {
        if webhook_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut reply = HttpReply::ok(json!({"text":"discarded first response"}));
            reply.delay = Duration::from_secs(1);
            reply
        } else {
            HttpReply::ok(json!({"text":"recovered reply"}))
        }
    })
    .await;
    let settings = config(root.path(), &platform, Some(&webhook));
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    let input = callback("interrupted-webhook", "alice", "hello");
    platform.send(input.clone()).await;
    webhook.wait_events(1).await;
    gateway.shutdown().await.unwrap();

    let reopened = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    reopened
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    webhook.wait_events(2).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "recovered reply");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&reopened, "interrupted-webhook", DeliveryStatus::Confirmed).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    platform.no_command("aibot_respond_msg").await;
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn unix_ndjson_limits_invalid_fields_and_private_state_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settings = config(root.path(), &platform, None);
    let path = settings.socket_path();
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    for bytes in [b"{bad-sensitive-json}\n".as_slice(), b"{}".as_slice()] {
        let response = raw_control(&path, bytes).await;
        assert!(response.get("error").is_some());
        assert!(!response.to_string().contains("sensitive"));
    }
    let oversized = format!(
        "{}\n",
        "a".repeat(anchor_wecom_gateway::MAX_REQUEST_BYTES + 1)
    );
    assert!(
        raw_control(&path, oversized.as_bytes())
            .await
            .get("error")
            .is_some()
    );
    let mut extra = send_request("extra", "alice", "hello");
    extra["secret"] = json!("must not echo");
    assert_eq!(
        control(&path, TOKEN, extra).await,
        json!({"error":"invalid channel request"})
    );
    assert!(gateway.delivery_facts().unwrap().is_empty());
    assert!(matches!(
        Gateway::start(config(root.path(), &platform, None)).await,
        Err(GatewayError::AlreadyRunning)
    ));
    gateway.shutdown().await.unwrap();
    let mut different_bot = config(root.path(), &platform, None);
    different_bot.bot_id = "different-bot".into();
    assert!(matches!(
        Gateway::start(different_bot).await,
        Err(GatewayError::Conflict(_))
    ));
    let unsafe_root = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(root.path().join("state"), unsafe_root.path().join("state"))
        .unwrap();
    assert!(matches!(
        Gateway::start(config(unsafe_root.path(), &platform, None)).await,
        Err(GatewayError::PrivateState)
    ));
    platform.no_command("aibot_send_msg").await;
}

#[tokio::test]
async fn confirmed_settlement_retries_after_http_failure_without_resending_platform_reply() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settlement_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = settlement_attempts.clone();
    let webhook = WebhookServer::new_with_settlement(
        |_| {
            HttpReply::ok(json!({
                "text":"confirmed reply",
                "receipt":{"key":"channel-confirmed-turn","content_sha256":"a".repeat(64)}
            }))
        },
        move |_| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let mut response = HttpReply::ok(json!({}));
            if attempt == 0 {
                response.status = axum::http::StatusCode::CREATED;
            }
            response
        },
    )
    .await;
    let settings = config(root.path(), &platform, Some(&webhook));
    let state_dir = settings.state_dir.clone();
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    let input = callback("confirmed-event", "alice", "request a reply");
    platform.send(input.clone()).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "confirmed reply");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "confirmed-event", DeliveryStatus::Confirmed).await;
    webhook.wait_requests(3).await;
    wait_settlement_delivered(&state_dir, "channel-confirmed-turn").await;
    assert_eq!(settlement_attempts.load(Ordering::SeqCst), 2);
    {
        let requests = webhook.requests.lock().unwrap();
        assert_eq!(requests[0]["event"]["event_id"], "confirmed-event");
        for request in &requests[1..] {
            assert_eq!(request["event"]["event_id"], "confirmed-event");
            assert_eq!(request["settlement"]["key"], "channel-confirmed-turn");
            assert_eq!(request["settlement"]["content_sha256"], "a".repeat(64));
            assert_eq!(request["settlement"]["status"], "confirmed");
        }
    }
    gateway.shutdown().await.unwrap();

    let reopened = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    reopened
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    platform.send(input).await;
    platform.no_command("aibot_respond_msg").await;
    assert_eq!(webhook.requests.lock().unwrap().len(), 3);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_ack_is_settled_and_retried_after_restart_without_platform_resend() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settlement_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = settlement_attempts.clone();
    let webhook = WebhookServer::new_with_settlement(
        |_| {
            HttpReply::ok(json!({
                "text":"possibly delivered",
                "receipt":{"key":"channel-unknown-turn","content_sha256":"b".repeat(64)}
            }))
        },
        move |_| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let mut response = HttpReply::ok(json!({}));
            if attempt == 0 {
                response.status = axum::http::StatusCode::SERVICE_UNAVAILABLE;
            }
            response
        },
    )
    .await;
    let settings = config(root.path(), &platform, Some(&webhook));
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    let input = callback("unknown-event", "alice", "request a reply");
    platform.send(input.clone()).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["finish"], true);
    platform.close().await;
    platform.next("aibot_subscribe").await;
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    wait_fact(&gateway, "unknown-event", DeliveryStatus::Unknown).await;
    webhook.wait_requests(2).await;
    assert_eq!(
        webhook.requests.lock().unwrap()[1]["settlement"]["status"],
        "unknown"
    );
    gateway.shutdown().await.unwrap();

    let reopened = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    reopened
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    webhook.wait_requests(3).await;
    assert_eq!(settlement_attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        webhook.requests.lock().unwrap()[2]["settlement"]["status"],
        "unknown"
    );
    platform.send(input).await;
    platform.no_command("aibot_respond_msg").await;
    assert_eq!(webhook.events.lock().unwrap().len(), 1);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn newer_message_suppression_is_settled_without_dispatching_old_reply() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        let event_id = event["event_id"].as_str().unwrap();
        let mut response = HttpReply::ok(json!({
            "text":format!("reply-{event_id}"),
            "receipt":{"key":format!("channel-{event_id}"),"content_sha256":"c".repeat(64)}
        }));
        if event_id == "old-event" {
            response.delay = Duration::from_millis(250);
        }
        response
    })
    .await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    platform.send(callback("old-event", "alice", "first")).await;
    webhook.wait_events(1).await;
    platform
        .send(callback("new-event", "alice", "second"))
        .await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "reply-new-event");
    platform.acknowledge(&reply, 0).await;
    webhook.wait_requests(4).await;
    let settlements = webhook
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|request| request.get("settlement"))
        .map(|settlement| settlement["status"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(settlements.contains(&"confirmed".to_owned()));
    assert!(settlements.contains(&"suppressed".to_owned()));
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn host_superseded_response_is_settled_without_platform_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        HttpReply::ok(json!({
            "text":"",
            "superseded":true,
            "receipt":{
                "key":format!("channel-{}", event["event_id"].as_str().unwrap()),
                "content_sha256":"d".repeat(64)
            }
        }))
    })
    .await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("host-superseded", "alice", "older request"))
        .await;
    webhook.wait_requests(2).await;
    wait_settlement_delivered(&root.path().join("state"), "channel-host-superseded").await;
    assert_eq!(
        webhook.requests.lock().unwrap()[1]["settlement"]["status"],
        "suppressed"
    );
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

/// A 1x1 PNG is enough to exercise the transport: the Host decodes and
/// validates real images, while this transport re-checks shape, bounds and the
/// declared digest of exactly these bytes.
const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn tiny_png_md5() -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use md5::{Digest as _, Md5};
    let digest = Md5::digest(STANDARD.decode(TINY_PNG_BASE64).unwrap());
    format!("{digest:x}")
}

fn image_reply(text: &str) -> Value {
    json!({
        "text": text,
        "receipt": {"key":"channel-fixture:image","content_sha256":"a".repeat(64)},
        "msg_item": [{"msgtype":"image","image":{"base64":TINY_PNG_BASE64,"md5":tiny_png_md5()}}],
    })
}

/// The final text stream of a reply, acknowledging intermediate progress.
async fn next_text_final(platform: &mut Platform) -> Value {
    loop {
        let frame = platform.next("aibot_respond_msg").await;
        if frame["body"]["msgtype"].as_str() == Some("stream")
            && frame["body"]["stream"]["finish"] == true
        {
            return frame;
        }
        platform.acknowledge(&frame, 0).await;
    }
}

#[tokio::test]
async fn reply_image_is_uploaded_in_chunks_then_sent_after_the_text() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(image_reply("结果图片"))).await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("image-reply", "alice", "画一张图"))
        .await;
    let text = next_text_final(&mut platform).await;
    assert_eq!(text["body"]["stream"]["content"], "结果图片");
    platform.acknowledge(&text, 0).await;

    let init = platform.next("aibot_upload_media_init").await;
    assert_eq!(init["body"]["type"], "image");
    assert_eq!(init["body"]["filename"], "reply.png");
    assert_eq!(init["body"]["md5"], tiny_png_md5());
    assert_eq!(init["body"]["total_chunks"], 1);
    platform
        .acknowledge_body(&init, 0, json!({"upload_id":"upload-fixture"}))
        .await;

    let chunk = platform.next("aibot_upload_media_chunk").await;
    assert_eq!(chunk["body"]["upload_id"], "upload-fixture");
    assert_eq!(chunk["body"]["chunk_index"], 0);
    assert_eq!(chunk["body"]["base64_data"], TINY_PNG_BASE64);
    platform.acknowledge(&chunk, 0).await;

    let finish = platform.next("aibot_upload_media_finish").await;
    assert_eq!(finish["body"]["upload_id"], "upload-fixture");
    platform
        .acknowledge_body(&finish, 0, json!({"media_id":"media-fixture"}))
        .await;

    let image = platform.next("aibot_respond_msg").await;
    assert_eq!(image["body"]["msgtype"], "image");
    assert_eq!(image["body"]["image"]["media_id"], "media-fixture");
    assert_eq!(image["headers"]["req_id"], "callback-image-reply");
    platform.acknowledge(&image, 0).await;

    wait_fact(&gateway, "image-reply:0", DeliveryStatus::Confirmed).await;
    platform.no_command("aibot_upload_media_chunk").await;
    assert_eq!(gateway.status(), ConnectionStatus::Authenticated);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn unacknowledged_image_send_is_not_repeated_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(image_reply("结果图片"))).await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("image-unknown", "alice", "画一张图"))
        .await;
    let text = next_text_final(&mut platform).await;
    platform.acknowledge(&text, 0).await;
    let init = platform.next("aibot_upload_media_init").await;
    platform
        .acknowledge_body(&init, 0, json!({"upload_id":"upload-fixture"}))
        .await;
    let chunk = platform.next("aibot_upload_media_chunk").await;
    platform.acknowledge(&chunk, 0).await;
    let finish = platform.next("aibot_upload_media_finish").await;
    platform
        .acknowledge_body(&finish, 0, json!({"media_id":"media-fixture"}))
        .await;
    // The image message is claimed before it is written, then never acknowledged.
    let image = platform.next("aibot_respond_msg").await;
    assert_eq!(image["body"]["msgtype"], "image");
    wait_fact(&gateway, "image-unknown:0", DeliveryStatus::Unknown).await;
    gateway.shutdown().await.unwrap();

    // Restarting over the same state must not repeat an uncertain platform
    // send, and must not even re-upload for it.
    let mut restarted_settings = config(root.path(), &platform, Some(&webhook));
    restarted_settings.timing.ack_timeout = Duration::from_millis(400);
    let gateway = Gateway::start(restarted_settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    platform.no_command("aibot_upload_media_init").await;
    platform.no_command("aibot_respond_msg").await;
    assert!(
        gateway
            .delivery_facts()
            .unwrap()
            .iter()
            .any(|fact| fact.request_id == "image-unknown:0"
                && fact.status == DeliveryStatus::Unknown)
    );
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn reply_images_are_serialized_and_each_send_is_confirmed_once() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        let mut reply = image_reply("两张图");
        reply["msg_item"] = json!([
            {"msgtype":"image","image":{"base64":TINY_PNG_BASE64,"md5":tiny_png_md5()}},
            {"msgtype":"image","image":{"base64":TINY_PNG_BASE64,"md5":tiny_png_md5()}},
        ]);
        HttpReply::ok(reply)
    })
    .await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("two-images", "alice", "两张图"))
        .await;
    let text = next_text_final(&mut platform).await;
    platform.acknowledge(&text, 0).await;

    for index in 0..2 {
        let init = platform.next("aibot_upload_media_init").await;
        platform
            .acknowledge_body(&init, 0, json!({"upload_id":format!("upload-{index}")}))
            .await;
        let chunk = platform.next("aibot_upload_media_chunk").await;
        platform.acknowledge(&chunk, 0).await;
        let finish = platform.next("aibot_upload_media_finish").await;
        platform
            .acknowledge_body(&finish, 0, json!({"media_id":format!("media-{index}")}))
            .await;
        let image = platform.next("aibot_respond_msg").await;
        assert_eq!(image["body"]["msgtype"], "image");
        assert_eq!(image["body"]["image"]["media_id"], format!("media-{index}"));
        platform.acknowledge(&image, 0).await;
        wait_fact(
            &gateway,
            &format!("two-images:{index}"),
            DeliveryStatus::Confirmed,
        )
        .await;
    }
    let facts = gateway.delivery_facts().unwrap();
    assert_eq!(
        facts
            .iter()
            .filter(|fact| fact.kind == "image" && fact.status == DeliveryStatus::Confirmed)
            .count(),
        2
    );
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn images_are_dropped_when_the_text_reply_is_never_confirmed() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(image_reply("结果图片"))).await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_timeout = Duration::from_millis(120);
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("no-text-ack", "alice", "画一张图"))
        .await;
    let text = next_text_final(&mut platform).await;
    // The text reply is never acknowledged, so the turn has no confirmed answer.
    assert_eq!(text["body"]["stream"]["finish"], true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    platform.no_command("aibot_upload_media_init").await;
    assert!(
        gateway
            .delivery_facts()
            .unwrap()
            .iter()
            .all(|fact| fact.kind != "image")
    );
    gateway.shutdown().await.unwrap();
}

/// AES-256-CBC with PKCS#7, matching the platform's media encryption.
fn encrypt_platform_media(plain: &[u8], key: &[u8; 32]) -> Vec<u8> {
    use aes::Aes256;
    use cbc::cipher::{BlockEncryptMut as _, KeyIvInit as _, block_padding::Pkcs7};
    let mut buffer = vec![0u8; plain.len() + 16];
    buffer[..plain.len()].copy_from_slice(plain);
    cbc::Encryptor::<Aes256>::new_from_slices(key, &key[..16])
        .unwrap()
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, plain.len())
        .unwrap()
        .to_vec()
}

const PLATFORM_MEDIA_KEY: [u8; 32] = [0x37u8; 32];

fn platform_media_key() -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.encode(PLATFORM_MEDIA_KEY)
}

#[tokio::test]
async fn inbound_media_is_downloaded_decrypted_and_handed_to_the_host() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let png = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
    )
    .unwrap();
    let server = MediaServer::new(
        encrypt_platform_media(&png, &PLATFORM_MEDIA_KEY),
        Some("attachment; filename=\"pelican.png\""),
    )
    .await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(json!({"text":"收到图片"}))).await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(media_callback(
            "inbound-image",
            "alice",
            "image",
            &server.url,
            &platform_media_key(),
        ))
        .await;
    webhook.wait_events(1).await;
    {
        let events = webhook.events.lock().unwrap();
        let event = &events[0];
        assert_eq!(event["message_type"], "image");
        assert_eq!(event["text"], "");
        let attachment = &event["attachments"][0];
        assert_eq!(attachment["name"], "pelican.png");
        assert_eq!(attachment["media_type"], "image/png");
        assert_eq!(
            attachment["data_base64"],
            json!(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &png
            ))
        );
        // The temporary URL and AES key never reach the Host.
        assert!(attachment.get("url").is_none());
        assert!(attachment.get("aeskey").is_none());
    }
    assert_eq!(server.requests().len(), 1);
    let final_text = next_text_final(&mut platform).await;
    assert_eq!(final_text["body"]["stream"]["content"], "收到图片");
    platform.acknowledge(&final_text, 0).await;
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn inbound_file_media_keeps_its_decoded_platform_filename() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let payload = b"%PDF-1.7 fixture".to_vec();
    let server = MediaServer::new(
        encrypt_platform_media(&payload, &PLATFORM_MEDIA_KEY),
        Some("attachment; filename*=UTF-8''%E6%8A%A5%E5%91%8A.pdf"),
    )
    .await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(json!({"text":"收到文件"}))).await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    platform
        .send(media_callback(
            "inbound-file",
            "alice",
            "file",
            &server.url,
            &platform_media_key(),
        ))
        .await;
    webhook.wait_events(1).await;
    {
        let events = webhook.events.lock().unwrap();
        let attachment = &events[0]["attachments"][0];
        assert_eq!(attachment["name"], "报告.pdf");
        assert!(attachment["media_type"].is_null());
        assert_eq!(
            attachment["data_base64"],
            json!(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &payload
            ))
        );
    }
    let final_text = next_text_final(&mut platform).await;
    platform.acknowledge(&final_text, 0).await;
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn unreadable_inbound_media_never_reaches_the_host() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let payload = b"secret platform bytes".to_vec();
    let server =
        MediaServer::new(encrypt_platform_media(&payload, &PLATFORM_MEDIA_KEY), None).await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(json!({"text":"must not run"}))).await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    let mut errors = gateway.subscribe_errors();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    // A key that does not decrypt the payload cannot become Host input.
    let wrong_key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [9u8; 32]);
    platform
        .send(media_callback(
            "wrong-key",
            "alice",
            "file",
            &server.url,
            &wrong_key,
        ))
        .await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), errors.recv())
            .await
            .unwrap()
            .unwrap(),
        GatewayError::Media
    );
    let failure = next_text_final(&mut platform).await;
    assert!(
        failure["body"]["stream"]["content"]
            .as_str()
            .unwrap()
            .contains("本次处理未能完成")
    );
    platform.acknowledge(&failure, 0).await;
    assert!(webhook.events.lock().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_inbound_media_is_refused_before_it_is_read() {
    let root = tempfile::tempdir().unwrap();
    let platform = Platform::new(Some(0)).await;
    let server = MediaServer::new(
        vec![0u8; anchor_wecom_gateway::MAX_INBOUND_FILE_BYTES + 32],
        None,
    )
    .await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(json!({"text":"must not run"}))).await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    let mut errors = gateway.subscribe_errors();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    platform
        .send(media_callback(
            "oversized",
            "alice",
            "file",
            &server.url,
            &platform_media_key(),
        ))
        .await;
    let error = tokio::time::timeout(Duration::from_secs(5), errors.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        error,
        GatewayError::Invalid("inbound media exceeds the per-file limit")
    );
    assert!(webhook.events.lock().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_newer_message_suppresses_an_image_still_being_uploaded() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        if event["event_id"] == "stale-image" {
            HttpReply::ok(image_reply("旧图片"))
        } else {
            HttpReply::ok(json!({"text":"新回复"}))
        }
    })
    .await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("stale-image", "alice", "画一张图"))
        .await;
    let text = next_text_final(&mut platform).await;
    platform.acknowledge(&text, 0).await;
    // The upload starts and its acknowledgement is deliberately withheld.
    let init = platform.next("aibot_upload_media_init").await;
    assert_eq!(init["body"]["type"], "image");

    platform
        .send(callback("newer-message", "alice", "算了"))
        .await;
    let newer = next_text_final(&mut platform).await;
    assert_eq!(newer["body"]["stream"]["content"], "新回复");
    platform.acknowledge(&newer, 0).await;

    // The stale image never continues: no retry, no chunk, no image message,
    // and no image delivery claim was ever written.
    tokio::time::sleep(Duration::from_millis(300)).await;
    platform.no_command("aibot_upload_media_init").await;
    platform.no_command("aibot_upload_media_chunk").await;
    platform.no_command("aibot_respond_msg").await;
    assert!(
        !gateway
            .delivery_facts()
            .unwrap()
            .iter()
            .any(|fact| fact.kind == "image")
    );
    gateway.shutdown().await.unwrap();
}

/// A Host semantic refusal must retire the event immediately: no restart may
/// replay an event the Host will never accept.
#[tokio::test]
async fn host_refusal_retires_the_event_and_never_replays_it() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply {
        status: axum::http::StatusCode::CONFLICT,
        body: json!({"error":"Run id was already used for different conversation input"}),
        delay: Duration::ZERO,
    })
    .await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    let mut errors = gateway.subscribe_errors();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform.send(callback("refused", "alice", "hello")).await;
    // A fresh event still closes the user's progress bubble.
    let failure = platform.next_final_response().await;
    assert_eq!(
        failure["body"]["stream"]["content"],
        "本次处理未能完成，请稍后补充一条消息继续；已执行的操作不会自动撤销。"
    );
    platform.acknowledge(&failure, 0).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), errors.recv())
            .await
            .unwrap()
            .unwrap(),
        GatewayError::Rejected
    );
    assert_eq!(webhook.events.lock().unwrap().len(), 1);
    gateway.shutdown().await.unwrap();

    // Restarting must not replay the refused event.
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(webhook.events.lock().unwrap().len(), 1);
    let connection = rusqlite::Connection::open(root.path().join("state/delivery.sqlite")).unwrap();
    let state: String = connection
        .query_row(
            "SELECT state FROM inbound WHERE event_id='refused'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "rejected");
    gateway.shutdown().await.unwrap();
}

/// A crash-recovery replay only covers the recent past: an undelivered event
/// older than the recovery window is retired instead of replayed.
#[tokio::test]
async fn undelivered_events_outside_the_recovery_window_are_never_replayed() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        body: json!({"error":"fixture failure"}),
        delay: Duration::ZERO,
    })
    .await;
    let gateway = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    platform
        .send(callback("long-ago", "alice", "old message"))
        .await;
    let failure = platform.next_final_response().await;
    platform.acknowledge(&failure, 0).await;
    assert_eq!(webhook.events.lock().unwrap().len(), 1);
    gateway.shutdown().await.unwrap();

    // Backdate the undelivered inbound: it was accepted long before the restart.
    let connection = rusqlite::Connection::open(root.path().join("state/delivery.sqlite")).unwrap();
    connection
        .execute(
            "UPDATE inbound SET created_at=0 WHERE event_id='long-ago'",
            [],
        )
        .unwrap();
    drop(connection);

    let mut restarted = config(root.path(), &platform, Some(&webhook));
    restarted.recovery_window = Duration::from_secs(60);
    let gateway = Gateway::start(restarted).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(webhook.events.lock().unwrap().len(), 1);
    let connection = rusqlite::Connection::open(root.path().join("state/delivery.sqlite")).unwrap();
    let state: String = connection
        .query_row(
            "SELECT state FROM inbound WHERE event_id='long-ago'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "rejected");
    gateway.shutdown().await.unwrap();
}

/// A burst of messages collapses into one progress bubble for the newest
/// message, instead of one bubble per message.
#[tokio::test]
async fn a_message_burst_shows_exactly_one_progress_bubble() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        let mut reply = HttpReply::ok(json!({
            "text": format!("reply for {}", event["event_id"].as_str().unwrap()),
        }));
        // Slow enough that the acknowledgement delay elapses for every message.
        reply.delay = Duration::from_millis(400);
        reply
    })
    .await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_delay = Duration::from_millis(50);
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    for event_id in ["m1", "m2", "m3"] {
        platform
            .send(callback(event_id, "alice", "burst message"))
            .await;
    }
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["body"]["stream"]["content"], "⏳ 正在处理…");
    assert_eq!(bubble["body"]["stream"]["finish"], false);
    // Only the newest message is acknowledged.
    assert_eq!(bubble["headers"]["req_id"], "callback-m3");
    platform.acknowledge(&bubble, 0).await;

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "reply for m3");
    platform.acknowledge(&reply, 0).await;
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

/// A bubble that is already shown is closed when a newer message replaces the
/// turn, so no conversation is left saying "processing" forever.
#[tokio::test]
async fn a_superseded_progress_bubble_is_closed_with_one_line() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        let mut reply = HttpReply::ok(json!({
            "text": format!("reply for {}", event["event_id"].as_str().unwrap()),
        }));
        reply.delay = Duration::from_millis(500);
        reply
    })
    .await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_delay = Duration::from_millis(50);
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform.send(callback("first", "alice", "first")).await;
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["body"]["stream"]["content"], "⏳ 正在处理…");
    assert_eq!(bubble["headers"]["req_id"], "callback-first");
    platform.acknowledge(&bubble, 0).await;

    platform.send(callback("second", "alice", "second")).await;
    let closing = platform.next("aibot_respond_msg").await;
    assert_eq!(closing["headers"]["req_id"], "callback-first");
    assert_eq!(
        closing["body"]["stream"]["content"],
        "已被你的新消息接续处理。"
    );
    assert_eq!(closing["body"]["stream"]["finish"], true);
    platform.acknowledge(&closing, 0).await;

    let second_bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(second_bubble["headers"]["req_id"], "callback-second");
    assert_eq!(second_bubble["body"]["stream"]["finish"], false);
    platform.acknowledge(&second_bubble, 0).await;

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "reply for second");
    platform.acknowledge(&reply, 0).await;
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

/// A fast answer needs no progress bubble at all.
#[tokio::test]
async fn a_fast_reply_skips_the_progress_bubble() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(json!({"text":"instant reply"}))).await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_delay = Duration::from_millis(50);
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform.send(callback("fast", "alice", "quick")).await;
    let first = platform.next("aibot_respond_msg").await;
    assert_eq!(first["body"]["stream"]["finish"], true);
    assert_eq!(first["body"]["stream"]["content"], "instant reply");
    platform.acknowledge(&first, 0).await;
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

/// Observed status lines update the one open bubble in place: throttled, capped
/// and never turned into extra chat messages.
#[tokio::test]
async fn progress_status_lines_update_the_open_bubble_in_place() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        let mut reply = HttpReply::ok(json!({"text":"done"}));
        reply.delay = Duration::from_millis(3200);
        reply
    })
    .await;
    let progress = ProgressServer::new(|_| {
        vec![
            ProgressFrame::status("正在查看文件", "read_file", 1, Duration::ZERO),
            ProgressFrame::status("正在查询业务系统", "plugin", 2, Duration::from_millis(1200)),
            ProgressFrame::status(
                "正在整理回复",
                "final_result",
                3,
                Duration::from_millis(1200),
            ),
            ProgressFrame::update("正在准备…", Duration::from_millis(50)),
            ProgressFrame::settled("completed"),
        ]
    })
    .await;
    let mut settings =
        config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    settings.timing.ack_delay = Duration::from_millis(400);
    // The turn deliberately outlives the fixture's default webhook timeout: it
    // must still be running after several throttled status updates.
    settings.webhook.as_mut().unwrap().timeout = Some(Duration::from_secs(8));
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("progress-1", "alice", "hello"))
        .await;
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["headers"]["req_id"], "callback-progress-1");
    assert_eq!(bubble["body"]["stream"]["finish"], false);
    // The bubble opens with observed work, rendered as a numbered step, not
    // the static placeholder.
    assert_eq!(
        bubble["body"]["stream"]["content"],
        "第 1 步 · 🔍 正在查看文件…"
    );
    platform.acknowledge(&bubble, 0).await;
    progress.wait_requested(1).await;

    // Later lines reuse the same request and stream: one bubble, updated.
    let mut contents = Vec::new();
    for _ in 0..2 {
        let update = platform.next("aibot_respond_msg").await;
        assert_eq!(update["headers"]["req_id"], "callback-progress-1");
        assert_eq!(update["body"]["stream"]["finish"], false);
        assert_eq!(
            update["body"]["stream"]["id"],
            bubble["body"]["stream"]["id"]
        );
        contents.push(
            update["body"]["stream"]["content"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        platform.acknowledge(&update, 0).await;
    }
    assert_eq!(
        contents,
        vec![
            "第 2 步 · 🔎 正在查询业务系统…",
            "第 3 步 · ✍️ 正在整理回复…",
        ]
    );

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "done");
    platform.acknowledge(&reply, 0).await;
    // The turn is over: no further status line may reach the platform.
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

/// A `settled` frame ends the turn for the transport: the subscription stops
/// instead of reading on, even when the Host keeps the connection open.
#[tokio::test]
async fn a_settled_host_stream_stops_its_subscription() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        let mut reply = HttpReply::ok(json!({"text":"done"}));
        reply.delay = Duration::from_millis(600);
        reply
    })
    .await;
    let progress = ProgressServer::new(|_| {
        vec![
            ProgressFrame::status("正在查看文件", "read_file", 1, Duration::ZERO),
            ProgressFrame::settled("completed"),
            // The Host's stream stays open, so dropping it is observable: a
            // transport that kept reading would hold this frame forever.
            ProgressFrame::status("正在处理", "default", 2, Duration::from_secs(30)),
        ]
    })
    .await;
    let settings = config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("settled-turn", "alice", "hello"))
        .await;
    wait_stream_requested(&progress, "settled-turn").await;
    wait_stream_closed(&progress, "settled-turn").await;

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "done");
    platform.acknowledge(&reply, 0).await;
    gateway.shutdown().await.unwrap();
}

/// A superseded turn stops feeding the chat, and a broken progress stream never
/// costs the user their reply.
#[tokio::test]
async fn superseded_and_broken_progress_streams_never_affect_delivery() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        let mut reply = HttpReply::ok(json!({
            "text": format!("reply for {}", event["event_id"].as_str().unwrap()),
        }));
        reply.delay = Duration::from_millis(1500);
        reply
    })
    .await;
    let progress = ProgressServer::new(|event_id| {
        if event_id == "progress-old" {
            vec![
                ProgressFrame::update("正在查看文件", Duration::ZERO),
                ProgressFrame::update("正在联网检索", Duration::from_secs(5)),
            ]
        } else {
            vec![ProgressFrame::update("正在查询业务系统", Duration::ZERO)]
        }
    })
    .await;
    let mut settings =
        config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    settings.timing.ack_delay = Duration::from_millis(50);
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("progress-old", "alice", "first"))
        .await;
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["body"]["stream"]["content"], "正在查看文件…");
    platform.acknowledge(&bubble, 0).await;

    // The newer message supersedes the running turn.
    platform
        .send(callback("progress-new", "alice", "second"))
        .await;
    let closing = platform.next("aibot_respond_msg").await;
    assert_eq!(closing["headers"]["req_id"], "callback-progress-old");
    assert_eq!(closing["body"]["stream"]["finish"], true);
    platform.acknowledge(&closing, 0).await;
    progress.wait_closed(1).await;

    let second = platform.next("aibot_respond_msg").await;
    assert_eq!(second["headers"]["req_id"], "callback-progress-new");
    assert_eq!(second["body"]["stream"]["content"], "正在查询业务系统…");
    platform.acknowledge(&second, 0).await;

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "reply for progress-new");
    platform.acknowledge(&reply, 0).await;
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();

    // A progress endpoint that cannot be reached changes nothing.
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(json!({"text":"replied"}))).await;
    let dead = ProgressServer::new(|_| vec![]).await;
    let unreachable = format!("{}/channels/wecom/progress", dead.url);
    drop(dead);
    let mut settings = config_with_progress(root.path(), &platform, Some(&webhook), None);
    settings.progress_url = Some(unreachable);
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    platform
        .send(callback("progress-dead", "alice", "hello"))
        .await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "replied");
    platform.acknowledge(&reply, 0).await;
    gateway.shutdown().await.unwrap();
}

/// Subscribing may race Host admission: a "not admitted yet" answer is retried
/// for a bounded window instead of losing the whole progress stream.
#[tokio::test]
async fn progress_subscription_retries_until_the_event_is_admitted() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        let mut reply = HttpReply::ok(json!({"text":"done"}));
        reply.delay = Duration::from_millis(1500);
        reply
    })
    .await;
    let progress = ProgressServer::new_with_refusals(2, |_| {
        vec![
            ProgressFrame::update("正在查询业务系统", Duration::ZERO),
            ProgressFrame::settled("completed"),
        ]
    })
    .await;
    let mut settings =
        config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    settings.timing.ack_delay = Duration::from_millis(900);
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("progress-race", "alice", "hello"))
        .await;
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["headers"]["req_id"], "callback-progress-race");
    // The retry connected before the delayed bubble opened, so the bubble shows
    // observed work rather than the placeholder.
    assert_eq!(bubble["body"]["stream"]["content"], "正在查询业务系统…");
    platform.acknowledge(&bubble, 0).await;

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "done");
    platform.acknowledge(&reply, 0).await;
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
    assert!(progress.requested.lock().unwrap().len() >= 3);
}

/// A receipt the Host permanently refuses must not be replayed forever.
#[tokio::test]
async fn a_refused_settlement_is_abandoned_instead_of_retried() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settlement_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = settlement_attempts.clone();
    let webhook = WebhookServer::new_with_settlement(
        |_| {
            HttpReply::ok(json!({
                "text":"confirmed reply",
                "receipt":{"key":"channel-refused-turn","content_sha256":"b".repeat(64)}
            }))
        },
        move |_| {
            attempts.fetch_add(1, Ordering::SeqCst);
            let mut response = HttpReply::ok(json!({}));
            response.status = axum::http::StatusCode::NOT_FOUND;
            response
        },
    )
    .await;
    let settings = config(root.path(), &platform, Some(&webhook));
    let state_dir = settings.state_dir.clone();
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    platform
        .send(callback("refused-settlement", "alice", "request a reply"))
        .await;
    let reply = platform.next_final_response().await;
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "refused-settlement", DeliveryStatus::Confirmed).await;
    wait_settlement_delivered(&state_dir, "channel-refused-turn").await;
    let seen = settlement_attempts.load(Ordering::SeqCst);
    assert_eq!(seen, 1);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        settlement_attempts.load(Ordering::SeqCst),
        seen,
        "a refused settlement must not be retried"
    );
    gateway.shutdown().await.unwrap();

    // A restart does not replay it either.
    let reopened = Gateway::start(config(root.path(), &platform, Some(&webhook)))
        .await
        .unwrap();
    reopened
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(settlement_attempts.load(Ordering::SeqCst), seen);
    reopened.shutdown().await.unwrap();
}

/// While a step is running the open bubble redraws its ellipsis, so the user can
/// see that work is still happening.
#[tokio::test]
async fn an_open_bubble_animates_its_ellipsis_while_the_turn_runs() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        let mut reply = HttpReply::ok(json!({"text":"done"}));
        reply.delay = Duration::from_millis(2600);
        reply
    })
    .await;
    let progress = ProgressServer::new(|_| {
        vec![
            ProgressFrame::status("正在查看文件", "read_file", 1, Duration::ZERO),
            ProgressFrame::settled("completed"),
        ]
    })
    .await;
    let mut settings =
        config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    settings.timing.ack_delay = Duration::from_millis(300);
    settings.timing.animation_interval = Duration::from_millis(60);
    settings.webhook.as_mut().unwrap().timeout = Some(Duration::from_secs(8));
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("animation-1", "alice", "hello"))
        .await;
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["headers"]["req_id"], "callback-animation-1");
    assert_eq!(bubble["body"]["stream"]["finish"], false);
    assert_eq!(
        bubble["body"]["stream"]["content"],
        "第 1 步 · 🔍 正在查看文件…"
    );
    let stream = bubble["body"]["stream"]["id"].clone();
    platform.acknowledge(&bubble, 0).await;

    // The same line keeps coming back with a moving ellipsis.
    let mut frames = Vec::new();
    for _ in 0..2 {
        let frame = platform.next("aibot_respond_msg").await;
        assert_eq!(frame["headers"]["req_id"], "callback-animation-1");
        assert_eq!(frame["body"]["stream"]["id"], stream);
        assert_eq!(frame["body"]["stream"]["finish"], false);
        frames.push(
            frame["body"]["stream"]["content"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        platform.acknowledge(&frame, 0).await;
    }
    assert_eq!(
        frames,
        vec!["第 1 步 · 🔍 正在查看文件.", "第 1 步 · 🔍 正在查看文件..",]
    );

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "done");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "animation-1", DeliveryStatus::Confirmed).await;
    // The turn is over, so the bubble stops animating.
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

/// A frame nobody acknowledged is decoration: it must not be read as a failed
/// delivery, and the real reply must still land and be confirmed.
#[tokio::test]
async fn a_lost_animation_acknowledgement_keeps_the_connection_and_the_reply() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        let mut reply = HttpReply::ok(json!({"text":"done"}));
        reply.delay = Duration::from_millis(2200);
        reply
    })
    .await;
    let progress = ProgressServer::new(|_| {
        vec![
            ProgressFrame::status("正在查看文件", "read_file", 1, Duration::ZERO),
            ProgressFrame::settled("completed"),
        ]
    })
    .await;
    let mut settings =
        config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    settings.timing.ack_delay = Duration::from_millis(300);
    settings.timing.animation_interval = Duration::from_millis(60);
    settings.webhook.as_mut().unwrap().timeout = Some(Duration::from_secs(8));
    let gateway = Gateway::start(settings).await.unwrap();
    let mut errors = gateway.subscribe_errors();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("animation-lost", "alice", "hello"))
        .await;
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(
        bubble["body"]["stream"]["content"],
        "第 1 步 · 🔍 正在查看文件…"
    );
    platform.acknowledge(&bubble, 0).await;
    for _ in 0..2 {
        let frame = platform.next("aibot_respond_msg").await;
        assert_eq!(frame["body"]["stream"]["finish"], false);
        // Deliberately never acknowledged.
    }

    // The fixture's acknowledgement timeout is 150ms: a strict update would have
    // failed closed by now, and a reconnect would announce itself.
    assert!(
        tokio::time::timeout(Duration::from_millis(500), errors.recv())
            .await
            .is_err(),
        "a decorative frame must not report a failed delivery"
    );
    platform.no_command("aibot_subscribe").await;

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "done");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "animation-lost", DeliveryStatus::Confirmed).await;
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

/// Arrival order does not say which turn the Host is running: when a later
/// message is the one the Host cancels, the older turn must still get the one
/// progress bubble instead of being silenced.
#[tokio::test]
async fn the_running_turn_keeps_its_bubble_when_a_later_message_is_cancelled() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        if event["event_id"] == "cancelled" {
            HttpReply::ok(json!({"text":"", "superseded": true}))
        } else {
            let mut reply = HttpReply::ok(json!({"text":"the running answer"}));
            reply.delay = Duration::from_millis(1500);
            reply
        }
    })
    .await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_delay = Duration::from_millis(300);
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("running", "alice", "long request"))
        .await;
    platform
        .send(callback("cancelled", "alice", "short request"))
        .await;

    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["headers"]["req_id"], "callback-running");
    assert_eq!(bubble["body"]["stream"]["finish"], false);
    platform.acknowledge(&bubble, 0).await;

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "the running answer");
    platform.acknowledge(&reply, 0).await;
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

/// The Host decides which turn runs, and that turn keeps its progress stream.
///
/// A message the transport receives *later* may be the one the Host cancels: it
/// only reaches the Host after its media was fetched. Stopping the other
/// subscription on arrival order silenced exactly the turn that was working, and
/// left the open bubble with nothing but the placeholder.
#[tokio::test]
async fn a_later_cancelled_message_does_not_stop_the_running_turns_progress_stream() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        if event["event_id"] == "cancelled-later" {
            // The Host cancelled this turn for the message it received first.
            HttpReply::ok(json!({"text":"", "superseded": true}))
        } else {
            let mut reply = HttpReply::ok(json!({"text":"the running answer"}));
            reply.delay = Duration::from_millis(1400);
            reply
        }
    })
    .await;
    let progress = ProgressServer::new(|event_id| {
        if event_id == "running-first" {
            vec![
                ProgressFrame::status("正在查看文件", "read_file", 1, Duration::ZERO),
                ProgressFrame::status("正在查询业务系统", "plugin", 2, Duration::from_millis(600)),
                ProgressFrame::status(
                    "正在整理回复",
                    "final_result",
                    3,
                    Duration::from_millis(3000),
                ),
                ProgressFrame::settled("completed"),
            ]
        } else {
            vec![
                ProgressFrame::status("正在准备工作区", "command", 4, Duration::ZERO),
                ProgressFrame::status("正在处理", "default", 5, Duration::from_secs(10)),
            ]
        }
    })
    .await;
    let mut settings =
        config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    settings.timing.ack_delay = Duration::from_millis(150);
    settings.webhook.as_mut().unwrap().timeout = Some(Duration::from_secs(8));
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("running-first", "alice", "long request"))
        .await;
    platform
        .send(callback("cancelled-later", "alice", "short request"))
        .await;

    // Only the running turn gets a bubble: the cancelled message is settled
    // before its acknowledgement is due, so it never opens one.
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["headers"]["req_id"], "callback-running-first");
    assert_eq!(
        bubble["body"]["stream"]["content"],
        "第 1 步 · 🔍 正在查看文件…"
    );
    assert_eq!(bubble["body"]["stream"]["finish"], false);
    platform.acknowledge(&bubble, 0).await;
    wait_stream_requested(&progress, "running-first").await;
    assert!(
        !closed_streams(&progress).contains(&"running-first".to_owned()),
        "the later callback must not stop the stream of the turn the Host runs"
    );

    // ...and that stream is still feeding its own bubble.
    let update = platform.next("aibot_respond_msg").await;
    assert_eq!(update["headers"]["req_id"], "callback-running-first");
    assert_eq!(
        update["body"]["stream"]["content"],
        "第 2 步 · 🔎 正在查询业务系统…"
    );
    assert_eq!(update["body"]["stream"]["finish"], false);
    platform.acknowledge(&update, 0).await;

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "the running answer");
    platform.acknowledge(&reply, 0).await;
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

/// An event's own callback settles its subscription: the stream stops even when
/// the Host never reports the turn settled.
#[tokio::test]
async fn an_events_own_callback_stops_its_progress_stream() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        let mut reply = HttpReply::ok(json!({"text":"done"}));
        reply.delay = Duration::from_millis(300);
        reply
    })
    .await;
    let progress = ProgressServer::new(|_| {
        vec![
            ProgressFrame::status("正在查看文件", "read_file", 1, Duration::ZERO),
            // Still open when the callback settles: only the callback ends this.
            ProgressFrame::status("正在处理", "default", 2, Duration::from_secs(10)),
        ]
    })
    .await;
    // The default acknowledgement delay keeps the bubble out of this test.
    let settings = config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("settling-event", "alice", "hello"))
        .await;
    wait_stream_requested(&progress, "settling-event").await;
    assert!(!closed_streams(&progress).contains(&"settling-event".to_owned()));

    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "done");
    platform.acknowledge(&reply, 0).await;
    wait_stream_closed(&progress, "settling-event").await;
    gateway.shutdown().await.unwrap();
}

/// Stopping the transport stops every live progress subscription instead of
/// leaving streams (and their tasks) behind.
#[tokio::test]
async fn stopping_the_transport_stops_live_progress_subscriptions() {
    let root = tempfile::tempdir().unwrap();
    let platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        let mut reply = HttpReply::ok(json!({"text":"done"}));
        reply.delay = Duration::from_secs(5);
        reply
    })
    .await;
    let progress = ProgressServer::new(|_| {
        vec![
            ProgressFrame::status("正在查看文件", "read_file", 1, Duration::ZERO),
            ProgressFrame::status("正在处理", "default", 2, Duration::from_secs(10)),
        ]
    })
    .await;
    let settings = config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    let gateway = Gateway::start(settings).await.unwrap();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    platform
        .send(callback("live-until-stop", "alice", "hello"))
        .await;
    wait_stream_requested(&progress, "live-until-stop").await;
    assert!(!closed_streams(&progress).contains(&"live-until-stop".to_owned()));

    gateway.shutdown().await.unwrap();
    wait_stream_closed(&progress, "live-until-stop").await;
}
