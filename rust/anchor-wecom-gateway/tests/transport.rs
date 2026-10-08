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
    HttpReply, Platform, TOKEN, WebhookServer, callback, config, control, raw_control, send_request,
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
    let reply = platform.next("aibot_respond_msg").await;
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
    let reply = platform.next("aibot_respond_msg").await;
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
    platform.next("aibot_respond_msg").await;
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
    let reply = platform.next("aibot_respond_msg").await;
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
        GatewayError::UnsupportedMedia
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
    let webhook = WebhookServer::new(|event| match event["event_id"].as_str().unwrap() {
        "media" => HttpReply::ok(
            json!({"text":"do not return only this text", "msg_item":[{"msgtype":"image"}]}),
        ),
        "failed" => HttpReply::ok(json!({"error":"fixture-secret-must-not-escape"})),
        _ => HttpReply::ok(json!({"text":"a".repeat(anchor_wecom_gateway::MAX_TEXT_BYTES + 1)})),
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
    for event_id in ["media", "failed", "oversized"] {
        let input = callback(event_id, "alice", "hello");
        platform.send(input.clone()).await;
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
            reply
        } else {
            HttpReply::ok(json!({"text":"recovered reply"}))
        }
    })
    .await;
    let settings = config(root.path(), &platform, Some(&webhook));
    let gateway = Gateway::start(settings).await.unwrap();
    let mut errors = gateway.subscribe_errors();
    gateway
        .wait_authenticated(Duration::from_secs(2))
        .await
        .unwrap();

    let input = callback("failed-webhook", "alice", "hello");
    platform.send(input.clone()).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), errors.recv())
            .await
            .unwrap()
            .unwrap(),
        GatewayError::Webhook
    );
    platform.no_command("aibot_respond_msg").await;
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
    let reply = platform.next("aibot_respond_msg").await;
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
    let reply = platform.next("aibot_respond_msg").await;
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
    let reply = platform.next("aibot_respond_msg").await;
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
    platform.next("aibot_respond_msg").await;
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
    let reply = platform.next("aibot_respond_msg").await;
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
