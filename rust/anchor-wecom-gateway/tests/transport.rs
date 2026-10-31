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
    HttpReply, MediaServer, Platform, ProgressFrame, ProgressServer, ReplyGate, TOKEN,
    WebhookServer, callback, config, config_with_progress, control, group_callback, media_callback,
    next_error, poll_until, raw_control, send_request, start_authenticated,
};

async fn wait_fact(gateway: &RunningGateway, request_id: &str, status: DeliveryStatus) {
    poll_until(|| {
        gateway
            .delivery_facts()
            .unwrap()
            .iter()
            .any(|fact| fact.request_id == request_id && fact.status == status)
    })
    .await;
}

async fn wait_settlement_delivered(state_dir: &std::path::Path, key: &str) {
    poll_until(|| {
        let connection = rusqlite::Connection::open(state_dir.join("delivery.sqlite")).unwrap();
        connection
            .query_row(
                "SELECT delivered FROM settlement_outbox WHERE receipt_key=?1",
                [key],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    })
    .await;
}

/// Which progress streams the fixture transport stopped early.
fn closed_streams(progress: &ProgressServer) -> Vec<String> {
    progress.closed.lock().unwrap().clone()
}

async fn wait_stream_requested(progress: &ProgressServer, event_id: &str) {
    poll_until(|| {
        progress
            .requested
            .lock()
            .unwrap()
            .iter()
            .any(|requested| requested == event_id)
    })
    .await;
}

async fn wait_stream_closed(progress: &ProgressServer, event_id: &str) {
    poll_until(|| {
        closed_streams(progress)
            .iter()
            .any(|closed| closed == event_id)
    })
    .await;
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
    let gateway = start_authenticated(settings).await;
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
    let reopened = start_authenticated(settings).await;
    let descriptor: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("state/control.json")).unwrap())
            .unwrap();
    assert_eq!(descriptor["token"], rotated);
    assert_eq!(control(&path, rotated, request).await["duplicate"], true);
    assert_eq!(reopened.delivery_facts().unwrap().len(), 1);
    platform.no_command("aibot_send_msg").await;
    reopened.shutdown().await.unwrap();
}

fn file_request(request_id: &str, name: &str, bytes: &[u8]) -> Value {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use md5::{Digest as _, Md5};

    json!({"operation":"send_file", "request_id":request_id, "userid":"alice",
        "name":name, "data_base64":STANDARD.encode(bytes),
        "md5":format!("{:x}", Md5::digest(bytes))})
}

#[tokio::test]
async fn proactive_file_uploads_exact_bytes_and_confirms_only_the_final_send() {
    use anchor_wecom_gateway::MEDIA_CHUNK_BYTES;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let mut settings = config(root.path(), &platform, None);
    settings.timing.ack_timeout = Duration::from_secs(2);
    let path = settings.socket_path();
    let gateway = start_authenticated(settings).await;
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.resize(MEDIA_CHUNK_BYTES + 16, b'x');
    let request = file_request("daily-report", "AI每日晨报_20261031.pdf", &bytes);
    for (field, value) in [
        ("userid", json!("@all")),
        ("md5", json!("0".repeat(32))),
        ("name", json!("../report.pdf")),
    ] {
        let mut invalid = request.clone();
        invalid[field] = value;
        assert!(control(&path, TOKEN, invalid).await["error"].is_string());
    }
    assert!(control(&path, "wrong-token", request.clone()).await["error"].is_string());
    let pending_path = path.clone();
    let pending_request = request.clone();
    let pending = tokio::spawn(async move { control(&pending_path, TOKEN, pending_request).await });
    let init = platform.next("aibot_upload_media_init").await;
    assert_eq!(
        init["body"],
        json!({"type":"file", "filename":"AI每日晨报_20261031.pdf",
        "total_size":bytes.len(), "total_chunks":2, "md5":request["md5"]})
    );
    platform
        .acknowledge_body(&init, 0, json!({"upload_id":"report-upload"}))
        .await;
    let mut uploaded = Vec::new();
    for index in 0..2 {
        let chunk = platform.next("aibot_upload_media_chunk").await;
        assert_eq!(chunk["body"]["chunk_index"], index);
        assert_eq!(chunk["body"]["upload_id"], "report-upload");
        uploaded.extend(
            STANDARD
                .decode(chunk["body"]["base64_data"].as_str().unwrap())
                .unwrap(),
        );
        platform.acknowledge(&chunk, 0).await;
    }
    assert_eq!(uploaded, bytes);
    let finish = platform.next("aibot_upload_media_finish").await;
    platform
        .acknowledge_body(&finish, 0, json!({"media_id":"report-media"}))
        .await;
    let wire = platform.next("aibot_send_msg").await;
    assert_eq!(
        wire["body"],
        json!({"chatid":"alice", "chat_type":1, "msgtype":"file",
        "file":{"media_id":"report-media"}})
    );
    assert!(!pending.is_finished());
    wait_fact(&gateway, "daily-report", DeliveryStatus::Unknown).await;
    platform.acknowledge(&wire, 0).await;
    assert_eq!(
        pending.await.unwrap(),
        json!({"accepted":true,"request_id":"daily-report"})
    );
    wait_fact(&gateway, "daily-report", DeliveryStatus::Confirmed).await;
    assert_eq!(
        control(&path, TOKEN, request.clone()).await["duplicate"],
        true
    );
    for changed in [
        file_request("daily-report", "changed.pdf", &bytes),
        file_request(
            "daily-report",
            "AI每日晨报_20261031.pdf",
            b"%PDF-1.7 changed",
        ),
        send_request("daily-report", "alice", "different message kind"),
    ] {
        assert!(
            control(&path, TOKEN, changed).await["error"]
                .as_str()
                .unwrap()
                .contains("different content or identity")
        );
    }
    gateway.shutdown().await.unwrap();
    let reopened = start_authenticated(config(root.path(), &platform, None)).await;
    assert_eq!(control(&path, TOKEN, request).await["duplicate"], true);
    platform.no_command("aibot_upload_media_init").await;
    platform.no_command("aibot_send_msg").await;
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn proactive_file_failure_distinguishes_upload_from_unknown_platform_send() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settings = config(root.path(), &platform, None);
    let path = settings.socket_path();
    let gateway = start_authenticated(settings).await;

    // A rejected upload creates no delivery claim and remains safe to retry.
    let request = file_request("upload-retry", "report.pdf", b"%PDF-1.7 first issue");
    let pending_path = path.clone();
    let pending_request = request.clone();
    let pending = tokio::spawn(async move { control(&pending_path, TOKEN, pending_request).await });
    for _ in 0..3 {
        let init = platform.next("aibot_upload_media_init").await;
        platform.acknowledge(&init, 400).await;
    }
    assert!(
        pending.await.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("no message was dispatched")
    );
    assert!(gateway.delivery_facts().unwrap().is_empty());
    let pending_path = path.clone();
    let pending = tokio::spawn(async move { control(&pending_path, TOKEN, request).await });
    platform
        .acknowledge_upload("retry-upload", "retry-media")
        .await;
    let wire = platform.next("aibot_send_msg").await;
    platform.acknowledge(&wire, 0).await;
    assert_eq!(pending.await.unwrap()["accepted"], true);

    for failure in ["timeout", "rejected", "disconnected"] {
        let request = file_request(failure, "report.pdf", b"%PDF-1.7 uncertain issue");
        let pending_path = path.clone();
        let pending_request = request.clone();
        let pending =
            tokio::spawn(async move { control(&pending_path, TOKEN, pending_request).await });
        platform
            .acknowledge_upload("unknown-upload", "unknown-media")
            .await;
        let wire = platform.next("aibot_send_msg").await;
        match failure {
            "rejected" => platform.acknowledge(&wire, 400).await,
            "disconnected" => platform.close().await,
            _ => {}
        }
        assert!(
            pending.await.unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("unconfirmed")
        );
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
    gateway.shutdown().await.unwrap();
    let reopened = start_authenticated(config(root.path(), &platform, None)).await;
    for failure in ["timeout", "rejected", "disconnected"] {
        let request = file_request(failure, "report.pdf", b"%PDF-1.7 uncertain issue");
        assert!(
            control(&path, TOKEN, request).await["error"]
                .as_str()
                .unwrap()
                .contains("previous delivery")
        );
    }
    platform.no_command("aibot_upload_media_init").await;
    platform.no_command("aibot_send_msg").await;
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn timeout_rejection_and_disconnect_stay_unknown_across_reconnect_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let settings = config(root.path(), &platform, None);
    let path = settings.socket_path();
    let gateway = start_authenticated(settings).await;
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
    let reopened = start_authenticated(config(root.path(), &platform, None)).await;
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
        next_error(&mut errors, Duration::from_secs(1)).await,
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
    let gateway = start_authenticated(settings).await;
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
        next_error(&mut errors, Duration::from_secs(1)).await,
        GatewayError::Invalid("saved reply sender is no longer allowed")
    );
    platform.no_command("aibot_respond_msg").await;
    assert!(reopened.delivery_facts().unwrap().is_empty());
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn callback_with_lost_reply_ack_is_not_replayed_or_reexecuted_after_restart() {
    assert_lost_reply_ack_is_not_replayed(false).await;
}

#[tokio::test]
async fn group_callback_with_lost_reply_ack_is_not_replayed_or_reexecuted_after_restart() {
    assert_lost_reply_ack_is_not_replayed(true).await;
}

async fn assert_lost_reply_ack_is_not_replayed(group: bool) {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(json!({"reply":"saved final reply"}))).await;
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
    let input = if group {
        group_callback("lost-reply", "bob", "group-1", "hello")
    } else {
        callback("lost-reply", "alice", "hello")
    };
    platform.send(input.clone()).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["finish"], true);
    assert_eq!(reply["headers"], input["headers"]);
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
    let reopened = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
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
    let old_reply = ReplyGate::default();
    let response_gate = old_reply.clone();
    let webhook = WebhookServer::new(move |event| {
        let text = event["text"].as_str().unwrap();
        let reply = HttpReply::ok(json!({"text":text}));
        if text == "old" {
            reply.gated(&response_gate)
        } else {
            reply
        }
    })
    .await;
    let settings = config(root.path(), &platform, Some(&webhook));
    let path = settings.socket_path();
    let ledger_path = settings.state_dir.join("delivery.sqlite");
    let gateway = start_authenticated(settings).await;
    let ledger = rusqlite::Connection::open(ledger_path).unwrap();
    let old_state = || {
        ledger
            .query_row::<String, _, _>(
                "SELECT state FROM inbound WHERE event_id='old'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    platform.send(callback("old", "alice", "old")).await;
    old_reply.wait_entered().await;
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
    assert_eq!(old_state(), "processing");
    platform.send(callback("new", "alice", "new")).await;
    let reply = platform.next_final_response().await;
    assert_eq!(reply["body"]["stream"]["content"], "new");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "new", DeliveryStatus::Confirmed).await;
    assert_eq!(old_state(), "processing");
    old_reply.release();
    tokio::time::timeout(Duration::from_millis(300), async {
        while old_state() != "completed" {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("released old callback was not suppressed");
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
        next_error(&mut errors, Duration::from_secs(1)).await,
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
        next_error(&mut errors, Duration::from_secs(1)).await,
        GatewayError::Invalid("media callback URL is missing or unsupported")
    );
    platform
        .send(callback("untrusted", "mallory", "not admitted"))
        .await;
    assert_eq!(
        next_error(&mut errors, Duration::from_secs(1)).await,
        GatewayError::Invalid("callback sender is not allowed")
    );
    platform.no_command("aibot_respond_msg").await;
    assert!(webhook.events.lock().unwrap().is_empty());
    assert!(gateway.delivery_facts().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn group_callbacks_check_the_real_actor_and_require_a_chatid_before_host_webhook() {
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

    platform
        .send(group_callback("group-message", "mallory", "alice", "hello"))
        .await;
    assert_eq!(
        next_error(&mut errors, Duration::from_secs(1)).await,
        GatewayError::Invalid("callback sender is not allowed")
    );
    let mut missing_chat = group_callback("missing-chat", "alice", "group-1", "hello");
    missing_chat["body"]
        .as_object_mut()
        .unwrap()
        .remove("chatid");
    platform.send(missing_chat).await;
    assert_eq!(
        next_error(&mut errors, Duration::from_secs(1)).await,
        GatewayError::Invalid("group callback has no chatid")
    );
    platform.no_command("aibot_respond_msg").await;
    assert!(webhook.events.lock().unwrap().is_empty());
    assert!(gateway.delivery_facts().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn different_members_of_a_group_share_progress_and_only_the_latest_reply_is_delivered() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        HttpReply::ok(json!({"text":format!("reply for {}", event["event_id"].as_str().unwrap())}))
            .delayed(Duration::from_millis(600))
    })
    .await;
    let progress = ProgressServer::new(|event_id| {
        if event_id == "group-old" {
            vec![
                ProgressFrame::status("正在查看文件", "read_file", 1, Duration::ZERO),
                ProgressFrame::status("正在工作区执行命令", "command", 2, Duration::from_secs(10)),
            ]
        } else {
            vec![
                ProgressFrame::status("正在查询业务系统", "plugin", 1, Duration::ZERO),
                ProgressFrame::status("正在查询业务系统", "plugin", 1, Duration::from_millis(80)),
                ProgressFrame::status("正在查询业务系统", "plugin", 1, Duration::from_secs(10)),
            ]
        }
    })
    .await;
    let mut settings =
        config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    settings.timing.ack_delay = Duration::from_millis(80);
    let gateway = start_authenticated(settings).await;

    platform
        .send(group_callback("group-old", "alice", "group-1", "first"))
        .await;
    let first = platform.next("aibot_respond_msg").await;
    assert_eq!(first["headers"]["req_id"], "callback-group-old");
    assert_eq!(first["body"]["stream"]["finish"], false);
    assert_eq!(
        first["body"]["stream"]["content"],
        "第 1 步 · 🔍 正在查看文件"
    );
    platform.acknowledge(&first, 0).await;

    platform
        .send(group_callback("group-new", "bob", "group-1", "continue"))
        .await;
    let closed = platform.next("aibot_respond_msg").await;
    assert_eq!(closed["headers"]["req_id"], "callback-group-old");
    assert_eq!(closed["body"]["stream"]["finish"], true);
    assert_eq!(
        closed["body"]["stream"]["content"],
        "已由群内的新消息接续处理。"
    );
    platform.acknowledge(&closed, 0).await;
    wait_stream_closed(&progress, "group-old").await;

    let newest = platform.next("aibot_respond_msg").await;
    assert_eq!(newest["headers"]["req_id"], "callback-group-new");
    assert_eq!(
        newest["body"]["stream"]["content"],
        "第 1 步 · 🔎 正在查询业务系统"
    );
    assert_eq!(newest["body"]["stream"]["finish"], false);
    platform.acknowledge(&newest, 0).await;
    // The repeated status is not another animation frame or another bubble.
    let reply = platform.next("aibot_respond_msg").await;
    assert_eq!(reply["headers"]["req_id"], "callback-group-new");
    assert_eq!(reply["body"]["stream"]["finish"], true);
    assert_eq!(reply["body"]["stream"]["content"], "reply for group-new");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "group-new", DeliveryStatus::Confirmed).await;
    let accepted = webhook.events.lock().unwrap().clone();
    assert_eq!(accepted.len(), 2);
    assert_eq!(accepted[0]["sender_id"], "alice");
    assert_eq!(accepted[1]["sender_id"], "bob");
    assert_eq!(accepted[1]["conversation_id"], "group-1");
    assert_eq!(accepted[1]["reply_target"], "group-1");
    assert_eq!(gateway.delivery_facts().unwrap().len(), 1);
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn groups_and_a_private_chat_with_the_same_id_keep_independent_progress_and_replies() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        HttpReply::ok(json!({"text":event["event_id"]})).delayed(Duration::from_millis(300))
    })
    .await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_delay = Duration::from_millis(40);
    // Three simultaneous streams share this fixture connection. Give the test
    // reader time to acknowledge each one under the crate's parallel workload.
    settings.timing.ack_timeout = Duration::from_secs(1);
    let gateway = start_authenticated(settings).await;
    platform
        .send(group_callback(
            "group-one",
            "alice",
            "alice",
            "in first group",
        ))
        .await;
    platform
        .send(group_callback(
            "group-two",
            "alice",
            "other-group",
            "in other group",
        ))
        .await;
    platform
        .send(callback("private-one", "alice", "private"))
        .await;
    let mut opened = std::collections::BTreeSet::new();
    let mut finished = std::collections::BTreeSet::new();
    while finished.len() != 3 {
        let frame = platform.next("aibot_respond_msg").await;
        let request = frame["headers"]["req_id"].as_str().unwrap().to_owned();
        if frame["body"]["stream"]["finish"] == false {
            assert!(opened.insert(request), "duplicate bubble");
        } else {
            assert_eq!(
                frame["body"]["stream"]["content"],
                request.strip_prefix("callback-").unwrap()
            );
            assert!(finished.insert(request), "duplicate reply");
        }
        platform.acknowledge(&frame, 0).await;
    }
    assert_eq!(opened, finished);
    assert_eq!(opened.len(), 3);
    for event in ["group-one", "group-two", "private-one"] {
        wait_fact(&gateway, event, DeliveryStatus::Confirmed).await;
    }
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn webhook_media_error_and_oversized_replies_are_not_silently_downgraded() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|event| {
        let reply = match event["event_id"].as_str().unwrap() {
            "media" => HttpReply::ok(
                json!({"text":"do not return only this text", "msg_item":[{"msgtype":"image"}]}),
            ),
            "failed" => HttpReply::ok(json!({"error":"fixture-secret-must-not-escape"})),
            _ => {
                HttpReply::ok(json!({"text":"a".repeat(anchor_wecom_gateway::MAX_TEXT_BYTES + 1)}))
            }
        };
        // Slow enough that the progress bubble is shown before the failure.
        reply.delayed(Duration::from_millis(400))
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
        assert_eq!(processing["body"]["stream"]["content"], "⏳ 正在处理");
        assert_eq!(processing["body"]["stream"]["finish"], false);
        platform.acknowledge(&processing, 0).await;
        let error = next_error(&mut errors, Duration::from_secs(1)).await;
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
            reply.delayed(Duration::from_millis(400))
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
    assert_eq!(processing["body"]["stream"]["content"], "⏳ 正在处理");
    assert_eq!(processing["body"]["stream"]["finish"], false);
    platform.acknowledge(&processing, 0).await;
    assert_eq!(
        next_error(&mut errors, Duration::from_secs(1)).await,
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

    let reopened = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
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
            HttpReply::ok(json!({"text":"discarded first response"}))
                .delayed(Duration::from_secs(1))
        } else {
            HttpReply::ok(json!({"text":"recovered reply"}))
        }
    })
    .await;
    let settings = config(root.path(), &platform, Some(&webhook));
    let gateway = start_authenticated(settings).await;

    let input = callback("interrupted-webhook", "alice", "hello");
    platform.send(input.clone()).await;
    webhook.wait_events(1).await;
    gateway.shutdown().await.unwrap();

    let reopened = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
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
    let gateway = start_authenticated(settings).await;
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
    let gateway = start_authenticated(settings).await;
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

    let reopened = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
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
    let gateway = start_authenticated(settings).await;
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

    let reopened = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
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
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
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
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;

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
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;

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
async fn group_mixed_media_preserves_the_actor_and_replies_with_text_and_image_to_the_original_callback()
 {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, TINY_PNG_BASE64)
        .unwrap();
    let media = MediaServer::new(
        encrypt_platform_media(&png, &PLATFORM_MEDIA_KEY),
        Some("attachment; filename=group.png"),
    )
    .await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(image_reply("群内图片结果"))).await;
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
    let mut incoming = group_callback("group-image", "bob", "group-1", "看看附件");
    incoming["body"].as_object_mut().unwrap().remove("text");
    incoming["body"]["msgtype"] = json!("mixed");
    incoming["body"]["mixed"] = json!({"msg_item":[
        {"msgtype":"text","text":{"content":"看看附件"}},
        {"msgtype":"image","image":{"url":media.url,"aeskey":platform_media_key()}}
    ]});
    platform.send(incoming.clone()).await;
    let text = next_text_final(&mut platform).await;
    assert_eq!(text["headers"], incoming["headers"]);
    assert_eq!(text["body"]["stream"]["content"], "群内图片结果");
    platform.acknowledge(&text, 0).await;
    let accepted = webhook.events.lock().unwrap()[0].clone();
    assert_eq!(accepted["sender_id"], "bob");
    assert_eq!(accepted["conversation_id"], "group-1");
    assert_eq!(accepted["reply_target"], "group-1");
    assert_eq!(accepted["metadata"]["chat_type"], "group");
    assert_eq!(accepted["message_type"], "mixed");
    assert_eq!(accepted["text"], "看看附件");
    let attachment = &accepted["attachments"][0];
    assert_eq!(attachment["name"], "group.png");
    assert_eq!(attachment["data_base64"], TINY_PNG_BASE64);
    assert!(attachment.get("url").is_none());
    assert!(attachment.get("aeskey").is_none());
    assert_eq!(media.requests().len(), 1);

    platform
        .acknowledge_upload("group-upload", "group-image-media")
        .await;
    let image = platform.next("aibot_respond_msg").await;
    assert_eq!(image["headers"], incoming["headers"]);
    assert_eq!(image["body"]["msgtype"], "image");
    assert_eq!(image["body"]["image"]["media_id"], "group-image-media");
    platform.acknowledge(&image, 0).await;
    wait_fact(&gateway, "group-image:0", DeliveryStatus::Confirmed).await;
    platform.no_command("aibot_send_msg").await;
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn unacknowledged_image_send_is_not_repeated_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(image_reply("结果图片"))).await;
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;

    platform
        .send(callback("image-unknown", "alice", "画一张图"))
        .await;
    let text = next_text_final(&mut platform).await;
    platform.acknowledge(&text, 0).await;
    platform
        .acknowledge_upload("upload-fixture", "media-fixture")
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
    let gateway = start_authenticated(restarted_settings).await;
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
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;

    platform
        .send(callback("two-images", "alice", "两张图"))
        .await;
    let text = next_text_final(&mut platform).await;
    platform.acknowledge(&text, 0).await;

    for index in 0..2 {
        platform
            .acknowledge_upload(&format!("upload-{index}"), &format!("media-{index}"))
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
    let gateway = start_authenticated(settings).await;

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
    let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, TINY_PNG_BASE64)
        .unwrap();
    let server = MediaServer::new(
        encrypt_platform_media(&png, &PLATFORM_MEDIA_KEY),
        Some("attachment; filename=\"pelican.png\""),
    )
    .await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(json!({"text":"收到图片"}))).await;
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;

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
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
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
        next_error(&mut errors, Duration::from_secs(2)).await,
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
    let error = next_error(&mut errors, Duration::from_secs(5)).await;
    assert_eq!(
        error,
        GatewayError::Invalid("inbound media exceeds the per-file limit")
    );
    assert!(webhook.events.lock().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_newer_message_suppresses_an_image_still_being_uploaded() {
    assert_a_newer_message_suppresses_an_image(false).await;
}

#[tokio::test]
async fn another_group_member_suppresses_an_image_still_being_uploaded() {
    assert_a_newer_message_suppresses_an_image(true).await;
}

async fn assert_a_newer_message_suppresses_an_image(group: bool) {
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
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;

    let older = if group {
        group_callback("stale-image", "alice", "group-1", "画一张图")
    } else {
        callback("stale-image", "alice", "画一张图")
    };
    platform.send(older).await;
    let text = next_text_final(&mut platform).await;
    platform.acknowledge(&text, 0).await;
    // The upload starts and its acknowledgement is deliberately withheld.
    let init = platform.next("aibot_upload_media_init").await;
    assert_eq!(init["body"]["type"], "image");

    let newer = if group {
        group_callback("newer-message", "bob", "group-1", "算了")
    } else {
        callback("newer-message", "alice", "算了")
    };
    platform.send(newer).await;
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
    assert_host_refusal(
        json!({"error":"Run id was already used for different conversation input"}),
        GatewayError::Rejected,
        "本次处理未能完成，请稍后补充一条消息继续；已执行的操作不会自动撤销。",
    )
    .await;
}

#[tokio::test]
async fn stopped_assistant_refusal_explains_recovery_and_never_replays_it() {
    assert_host_refusal(
        json!({"code":"assistant_stopped","error":"assistant is stopped"}),
        GatewayError::AssistantStopped,
        "助手已停止。请在 Anchor 运行看板的「消息会话」中恢复该助手后再发消息。",
    )
    .await;
}

async fn assert_host_refusal(body: Value, expected_error: GatewayError, notice: &str) {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(move |_| HttpReply {
        status: axum::http::StatusCode::CONFLICT,
        body: body.clone(),
        delay: Duration::ZERO,
        gate: None,
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
    assert_eq!(failure["body"]["stream"]["content"], notice);
    platform.acknowledge(&failure, 0).await;
    assert_eq!(
        next_error(&mut errors, Duration::from_secs(1)).await,
        expected_error
    );
    assert_eq!(webhook.events.lock().unwrap().len(), 1);
    gateway.shutdown().await.unwrap();

    // Restarting must not replay the refused event.
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
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
        gate: None,
    })
    .await;
    let gateway = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
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
    let gateway = start_authenticated(restarted).await;
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
        HttpReply::ok(json!({
            "text": format!("reply for {}", event["event_id"].as_str().unwrap()),
        }))
        .delayed(Duration::from_millis(400))
    })
    .await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_delay = Duration::from_millis(50);
    let gateway = start_authenticated(settings).await;

    for event_id in ["m1", "m2", "m3"] {
        platform
            .send(callback(event_id, "alice", "burst message"))
            .await;
    }
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["body"]["stream"]["content"], "⏳ 正在处理");
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

/// A fast answer needs no progress bubble at all.
#[tokio::test]
async fn a_fast_reply_skips_the_progress_bubble() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| HttpReply::ok(json!({"text":"instant reply"}))).await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_delay = Duration::from_millis(50);
    let gateway = start_authenticated(settings).await;

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
        HttpReply::ok(json!({"text":"done"})).delayed(Duration::from_millis(3200))
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
    let gateway = start_authenticated(settings).await;

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
        "第 1 步 · 🔍 正在查看文件"
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
        vec!["第 2 步 · 🔎 正在查询业务系统", "第 3 步 · ✍️ 正在整理回复",]
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
        HttpReply::ok(json!({"text":"done"})).delayed(Duration::from_millis(600))
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
    let gateway = start_authenticated(settings).await;

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
        HttpReply::ok(json!({
            "text": format!("reply for {}", event["event_id"].as_str().unwrap()),
        }))
        .delayed(Duration::from_millis(1500))
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
    let gateway = start_authenticated(settings).await;

    platform
        .send(callback("progress-old", "alice", "first"))
        .await;
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["body"]["stream"]["content"], "正在查看文件");
    platform.acknowledge(&bubble, 0).await;

    // The newer message supersedes the running turn.
    platform
        .send(callback("progress-new", "alice", "second"))
        .await;
    let closing = platform.next("aibot_respond_msg").await;
    assert_eq!(closing["headers"]["req_id"], "callback-progress-old");
    assert_eq!(closing["body"]["stream"]["finish"], true);
    assert_eq!(
        closing["body"]["stream"]["content"],
        "已被你的新消息接续处理。"
    );
    platform.acknowledge(&closing, 0).await;
    progress.wait_closed(1).await;

    let second = platform.next("aibot_respond_msg").await;
    assert_eq!(second["headers"]["req_id"], "callback-progress-new");
    assert_eq!(second["body"]["stream"]["finish"], false);
    assert_eq!(second["body"]["stream"]["content"], "正在查询业务系统");
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
    let gateway = start_authenticated(settings).await;
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
        HttpReply::ok(json!({"text":"done"})).delayed(Duration::from_millis(1500))
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
    let gateway = start_authenticated(settings).await;

    platform
        .send(callback("progress-race", "alice", "hello"))
        .await;
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["headers"]["req_id"], "callback-progress-race");
    // The retry connected before the delayed bubble opened, so the bubble shows
    // observed work rather than the placeholder.
    assert_eq!(bubble["body"]["stream"]["content"], "正在查询业务系统");
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
    let gateway = start_authenticated(settings).await;
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
    let reopened = start_authenticated(config(root.path(), &platform, Some(&webhook))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(settlement_attempts.load(Ordering::SeqCst), seen);
    reopened.shutdown().await.unwrap();
}

/// The client already shows loading. An unchanged status must not refresh the
/// bubble, even when the Host repeats it or briefly queues another status.
#[tokio::test]
async fn unchanged_progress_does_not_refresh_the_open_bubble() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        HttpReply::ok(json!({"text":"done"})).delayed(Duration::from_millis(2800))
    })
    .await;
    let progress = ProgressServer::new(|_| {
        vec![
            ProgressFrame::status("正在查看文件", "read_file", 1, Duration::ZERO),
            // Repeated status, including legacy trailing dots, is unchanged.
            ProgressFrame::status("正在查看文件…", "read_file", 1, Duration::from_millis(600)),
            // Both arrive inside the throttle window. The newest matches the
            // shown line, so the older pending update must be discarded.
            ProgressFrame::status("正在查询业务系统", "plugin", 2, Duration::from_millis(200)),
            ProgressFrame::status("正在查看文件", "read_file", 1, Duration::from_millis(200)),
            ProgressFrame::status(
                "正在查看文件...",
                "read_file",
                1,
                Duration::from_millis(600),
            ),
            ProgressFrame::settled("completed"),
        ]
    })
    .await;
    let mut settings =
        config_with_progress(root.path(), &platform, Some(&webhook), Some(&progress));
    settings.timing.ack_delay = Duration::from_millis(300);
    settings.webhook.as_mut().unwrap().timeout = Some(Duration::from_secs(8));
    let gateway = start_authenticated(settings).await;

    platform
        .send(callback("unchanged-progress", "alice", "hello"))
        .await;
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["headers"]["req_id"], "callback-unchanged-progress");
    assert_eq!(bubble["body"]["stream"]["finish"], false);
    assert_eq!(
        bubble["body"]["stream"]["content"],
        "第 1 步 · 🔍 正在查看文件"
    );
    platform.acknowledge(&bubble, 0).await;

    // Observe the very next frame, without skipping intermediate updates. The
    // wait spans the old two-second animation interval and repeated Host lines.
    let reply = platform.next("aibot_respond_msg").await;
    assert_eq!(reply["headers"]["req_id"], bubble["headers"]["req_id"]);
    assert_eq!(
        reply["body"]["stream"]["id"],
        bubble["body"]["stream"]["id"]
    );
    assert_eq!(reply["body"]["stream"]["finish"], true);
    assert_eq!(reply["body"]["stream"]["content"], "done");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "unchanged-progress", DeliveryStatus::Confirmed).await;
    platform.no_command("aibot_respond_msg").await;
    gateway.shutdown().await.unwrap();
}

/// Status ACKs and reply ACKs share a request id. Finishing the answer must wait
/// for the open bubble's ACK, even when the Host has already returned its reply.
#[tokio::test]
async fn the_final_reply_waits_for_the_progress_acknowledgement() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        HttpReply::ok(json!({"text":"done"})).delayed(Duration::from_millis(300))
    })
    .await;
    let mut settings = config(root.path(), &platform, Some(&webhook));
    settings.timing.ack_delay = Duration::from_millis(50);
    settings.timing.ack_timeout = Duration::from_secs(2);
    let gateway = start_authenticated(settings).await;

    platform
        .send(callback("progress-ack", "alice", "hello"))
        .await;
    let bubble = platform.next("aibot_respond_msg").await;
    assert_eq!(bubble["body"]["stream"]["content"], "⏳ 正在处理");
    assert_eq!(bubble["body"]["stream"]["finish"], false);
    // The reply is ready but cannot yet use the same callback request id.
    tokio::time::sleep(Duration::from_millis(500)).await;
    platform.no_command("aibot_respond_msg").await;
    assert!(gateway.delivery_facts().unwrap().is_empty());
    platform.acknowledge(&bubble, 0).await;

    let reply = platform.next("aibot_respond_msg").await;
    assert_eq!(reply["headers"]["req_id"], bubble["headers"]["req_id"]);
    assert_eq!(
        reply["body"]["stream"]["id"],
        bubble["body"]["stream"]["id"]
    );
    assert_eq!(reply["body"]["stream"]["finish"], true);
    assert_eq!(reply["body"]["stream"]["content"], "done");
    platform.acknowledge(&reply, 0).await;
    wait_fact(&gateway, "progress-ack", DeliveryStatus::Confirmed).await;
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
    for with_progress in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut platform = Platform::new(Some(0)).await;
        let webhook = WebhookServer::new(|event| {
            if event["event_id"] == "cancelled-later" {
                // The Host cancelled this turn for the message it received first.
                HttpReply::ok(json!({"text":"", "superseded": true}))
            } else {
                HttpReply::ok(json!({"text":"the running answer"}))
                    .delayed(Duration::from_millis(1400))
            }
        })
        .await;
        let progress = ProgressServer::new(|event_id| {
            if event_id == "running-first" {
                vec![
                    ProgressFrame::status("正在查看文件", "read_file", 1, Duration::ZERO),
                    ProgressFrame::status(
                        "正在查询业务系统",
                        "plugin",
                        2,
                        Duration::from_millis(600),
                    ),
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
        let mut settings = config_with_progress(
            root.path(),
            &platform,
            Some(&webhook),
            with_progress.then_some(&progress),
        );
        settings.timing.ack_delay = Duration::from_millis(150);
        settings.webhook.as_mut().unwrap().timeout = Some(Duration::from_secs(8));
        let gateway = start_authenticated(settings).await;

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
            if with_progress {
                "第 1 步 · 🔍 正在查看文件"
            } else {
                "⏳ 正在处理"
            }
        );
        assert_eq!(bubble["body"]["stream"]["finish"], false);
        platform.acknowledge(&bubble, 0).await;
        if with_progress {
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
                "第 2 步 · 🔎 正在查询业务系统"
            );
            assert_eq!(update["body"]["stream"]["finish"], false);
            platform.acknowledge(&update, 0).await;
        }

        let reply = platform.next_final_response().await;
        assert_eq!(reply["body"]["stream"]["content"], "the running answer");
        platform.acknowledge(&reply, 0).await;
        platform.no_command("aibot_respond_msg").await;
        gateway.shutdown().await.unwrap();
    }
}

/// An event's own callback settles its subscription: the stream stops even when
/// the Host never reports the turn settled.
#[tokio::test]
async fn an_events_own_callback_stops_its_progress_stream() {
    let root = tempfile::tempdir().unwrap();
    let mut platform = Platform::new(Some(0)).await;
    let webhook = WebhookServer::new(|_| {
        HttpReply::ok(json!({"text":"done"})).delayed(Duration::from_millis(300))
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
    let gateway = start_authenticated(settings).await;

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
        HttpReply::ok(json!({"text":"done"})).delayed(Duration::from_secs(5))
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
    let gateway = start_authenticated(settings).await;

    platform
        .send(callback("live-until-stop", "alice", "hello"))
        .await;
    wait_stream_requested(&progress, "live-until-stop").await;
    assert!(!closed_streams(&progress).contains(&"live-until-stop".to_owned()));

    gateway.shutdown().await.unwrap();
    wait_stream_closed(&progress, "live-until-stop").await;
}
