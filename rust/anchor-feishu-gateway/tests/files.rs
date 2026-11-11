mod support;

use std::{sync::atomic::Ordering, time::Duration};

use anchor_feishu_gateway::{
    ConnectionStatus, DeliveryStatus, Gateway, protocol::normalize_message,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::{Digest, Md5};
use serde_json::{Value, json};
use support::{Fixture, TOKEN, control_request, event, event_frame, poll};

fn media_event(message_id: &str, kind: &str, content: Value) -> Value {
    let mut event = event(message_id, "");
    event["event"]["message"]["message_type"] = json!(kind);
    event["event"]["message"]["content"] = json!(content.to_string());
    event
}

async fn send_media(fixture: &Fixture, message_id: &str, kind: &str, content: Value) {
    let mut frame = event_frame(message_id, "");
    frame.payload = serde_json::to_vec(&media_event(message_id, kind, content)).unwrap();
    fixture.send(frame).await;
}

fn send_file(id: &str, name: &str, bytes: &[u8]) -> Value {
    json!({"operation":"send_file","token":TOKEN,"request_id":id,"open_id":"ou_alice",
        "chat_id":"oc_private","name":name,"data_base64":STANDARD.encode(bytes),
        "md5":format!("{:x}",Md5::digest(bytes))})
}

fn png() -> Vec<u8> {
    STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aAdkAAAAASUVORK5CYII=").unwrap()
}

#[tokio::test]
async fn incoming_resource_is_acked_before_fetch_bound_to_message_and_stored_only_as_descriptor() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    fixture
        .shared
        .resource_blocked
        .store(true, Ordering::SeqCst);
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let content = json!({"file_key":"file_original", "file_name":"report.csv"});
    send_media(&fixture, "om_file", "file", content.clone()).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), fixture.ack())
            .await
            .unwrap()["code"],
        200
    );
    tokio::time::timeout(
        Duration::from_secs(2),
        fixture.shared.resource_entered.notified(),
    )
    .await
    .unwrap();
    assert!(fixture.shared.events.lock().unwrap().is_empty());
    // Duplicate admission is independent of a blocked download.
    send_media(&fixture, "om_file", "file", content.clone()).await;
    assert_eq!(fixture.ack().await["code"], 200);
    assert_eq!(fixture.shared.downloads.lock().unwrap().len(), 1);
    fixture
        .shared
        .resource_blocked
        .store(false, Ordering::SeqCst);
    fixture.shared.resource_release.notify_one();
    fixture.wait_settlements(1).await;
    let downloads = fixture.shared.downloads.lock().unwrap().clone();
    assert_eq!(
        downloads,
        vec![json!({"message_id":"om_file","key":"file_original","type":"file"})]
    );
    let events = fixture.shared.events.lock().unwrap().clone();
    let normalized = &events[0]["event"];
    assert_eq!(normalized["message_type"], "file");
    assert_eq!(
        normalized["metadata"],
        json!({"chat_type":"single","request_id":"om_file"})
    );
    assert_eq!(
        normalized["attachments"],
        json!([{"name":"report.csv","data_base64":STANDARD.encode(b"fixture inbound file contents")} ])
    );
    let database = rusqlite::Connection::open(root.path().join("state/delivery.sqlite")).unwrap();
    let stored: String = database
        .query_row(
            "SELECT event_json FROM inbound WHERE event_id='om_file'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap(),
        serde_json::to_value(
            normalize_message(&media_event("om_file", "file", content), "cli_fixture_app")
                .unwrap()
                .unwrap()
        )
        .unwrap()
    );
    assert!(!stored.contains("data_base64"));
    assert_eq!(
        fixture.shared.settlements.lock().unwrap()[0]["event"]["attachments"],
        json!([])
    );
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn media_recovery_refetches_original_descriptor_and_images_reach_host_with_verified_format() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    *fixture.shared.resource_body.lock().unwrap() = png();
    fixture.shared.blocked.store(true, Ordering::SeqCst);
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    send_media(
        &fixture,
        "om_image",
        "image",
        json!({"image_key":"img_original"}),
    )
    .await;
    assert_eq!(fixture.ack().await["code"], 200);
    tokio::time::timeout(Duration::from_secs(2), fixture.shared.entered.notified())
        .await
        .unwrap();
    gateway.shutdown().await.unwrap();
    fixture.shared.blocked.store(false, Ordering::SeqCst);
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    fixture.wait_settlements(1).await;
    assert_eq!(fixture.shared.downloads.lock().unwrap().len(), 2);
    let events = fixture.shared.events.lock().unwrap().clone();
    assert_eq!(events[0], events[1]);
    assert_eq!(events[1]["event"]["message_type"], "image");
    assert_eq!(
        events[1]["event"]["attachments"],
        json!([{"name":"image.png","data_base64":STANDARD.encode(png()),"media_type":"image/png"}])
    );
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn media_permissions_urls_redirects_and_byte_bounds_fail_before_host_admission() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let mut errors = gateway.subscribe_errors();
    let mut frame = event_frame("om_denied", "");
    let mut source = media_event(
        "om_denied",
        "file",
        json!({"file_key":"file_key","file_name":"safe.txt"}),
    );
    source["event"]["sender"]["sender_id"]["open_id"] = json!("ou_not_allowed");
    frame.payload = serde_json::to_vec(&source).unwrap();
    fixture.send(frame).await;
    assert_eq!(fixture.ack().await["code"], 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fixture.shared.downloads.lock().unwrap().is_empty());
    for content in [
        json!({"file_key":"https://example.com/private","file_name":"safe.txt"}),
        json!({"file_key":"file_key","file_name":"../unsafe"}),
        json!({"file_key":"file_key","file_name":"safe.txt","url":"https://example.com"}),
    ] {
        send_media(&fixture, "om_invalid", "file", content).await;
        assert_eq!(fixture.ack().await["code"], 500);
    }
    assert!(fixture.shared.downloads.lock().unwrap().is_empty());
    for (id, kind, size, mode) in [
        ("om_image_limit", "image", 10 * 1024 * 1024 + 1, 0),
        ("om_file_limit", "file", 20 * 1024 * 1024 + 1, 0),
        ("om_redirect", "file", 10, 2),
    ] {
        *fixture.shared.resource_body.lock().unwrap() = vec![b'x'; size];
        fixture.shared.resource_mode.store(mode, Ordering::SeqCst);
        send_media(
            &fixture,
            id,
            kind,
            if kind == "image" {
                json!({"image_key":"img_key"})
            } else {
                json!({"file_key":"file_key","file_name":"safe.txt"})
            },
        )
        .await;
        assert_eq!(fixture.ack().await["code"], 200);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(
                    errors.recv().await.unwrap(),
                    anchor_feishu_gateway::GatewayError::Media
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(fixture.shared.events.lock().unwrap().is_empty());
    }
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn outgoing_file_uses_multipart_then_claim_and_reentry_binds_name_bytes_recipient() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let config = fixture.config(root.path());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    let args = send_file("file-send", "report.md", b"# completed\n");
    assert_eq!(
        control_request(&socket, args.clone()).await["accepted"],
        true
    );
    let uploads = fixture.shared.uploads.lock().unwrap().clone();
    assert_eq!(
        uploads,
        vec![
            json!({"file_type":"stream","file_name":"report.md","file":{"name":"report.md","data_base64":STANDARD.encode(b"# completed\n")}})
        ]
    );
    let sends = fixture.shared.sends.lock().unwrap().clone();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["msg_type"], "file");
    assert_eq!(
        serde_json::from_str::<Value>(sends[0]["content"].as_str().unwrap()).unwrap(),
        json!({"file_key":"file_uploaded_fixture"})
    );
    assert_eq!(
        gateway.delivery_facts().unwrap()[0].status,
        DeliveryStatus::Confirmed
    );
    gateway.shutdown().await.unwrap();
    fixture.shared.token_failure.store(true, Ordering::SeqCst);
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    assert_eq!(
        control_request(&socket, args.clone()).await["accepted"],
        true
    );
    for changed in [
        send_file("file-send", "report.md", b"different"),
        send_file("file-send", "renamed.md", b"# completed\n"),
        {
            let mut changed = args.clone();
            changed["open_id"] = json!("ou_bob");
            changed
        },
        {
            let mut changed = args.clone();
            changed["chat_id"] = json!("oc_other");
            changed
        },
    ] {
        assert!(
            control_request(&socket, changed).await["error"]
                .as_str()
                .unwrap()
                .contains("different content or identity")
        );
    }
    assert_eq!(fixture.shared.uploads.lock().unwrap().len(), 1);
    assert_eq!(fixture.shared.sends.lock().unwrap().len(), 1);
    assert_eq!(fixture.shared.tokens.load(Ordering::SeqCst), 1);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn outgoing_file_upload_is_retryable_but_unknown_message_send_is_not() {
    for mode in [1, 2, 3] {
        let root = tempfile::tempdir().unwrap();
        let fixture = Fixture::new().await;
        let socket = fixture.config(root.path()).socket_path();
        let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
        fixture.shared.upload_mode.store(mode, Ordering::SeqCst);
        let args = send_file("file-retry", "image.png", &png());
        let failed_upload = control_request(&socket, args.clone()).await;
        let error = failed_upload["error"].as_str().unwrap();
        assert!(error.contains("no message was dispatched"));
        assert!(error.contains("im:resource:upload"));
        assert!(error.contains("publish the app version"));
        assert!(!error.contains("untrusted-upstream-upload-detail"));
        assert!(gateway.delivery_facts().unwrap().is_empty());
        assert!(fixture.shared.sends.lock().unwrap().is_empty());
        fixture.shared.upload_mode.store(0, Ordering::SeqCst);
        fixture.shared.send_mode.store(mode, Ordering::SeqCst);
        assert!(
            control_request(&socket, args.clone())
                .await
                .get("error")
                .is_some()
        );
        assert_eq!(
            gateway.delivery_facts().unwrap()[0].status,
            DeliveryStatus::Unknown
        );
        gateway.shutdown().await.unwrap();
        let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
        assert!(
            control_request(&socket, args).await["error"]
                .as_str()
                .unwrap()
                .contains("unconfirmed")
        );
        assert_eq!(fixture.shared.uploads.lock().unwrap().len(), 2);
        assert_eq!(fixture.shared.sends.lock().unwrap().len(), 1);
        gateway.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn shutdown_before_upload_completion_has_no_claim_but_after_send_has_unknown_no_replay() {
    for phase in ["upload", "send"] {
        let root = tempfile::tempdir().unwrap();
        let fixture = Fixture::new().await;
        let socket = fixture.config(root.path()).socket_path();
        if phase == "upload" {
            fixture.shared.upload_mode.store(4, Ordering::SeqCst);
        } else {
            fixture.shared.send_mode.store(4, Ordering::SeqCst);
        }
        let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
        let args = send_file("shutdown-file", "report.txt", b"contents");
        let send = tokio::spawn({
            let args = args.clone();
            let socket = socket.clone();
            async move { control_request(&socket, args).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            if phase == "upload" {
                fixture.shared.upload_entered.notified().await;
            } else {
                fixture.shared.send_entered.notified().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            gateway.delivery_facts().unwrap().len(),
            usize::from(phase == "send")
        );
        gateway.shutdown().await.unwrap();
        assert!(send.await.unwrap().get("error").is_some());
        fixture.shared.upload_release.notify_one();
        fixture.shared.send_release.notify_one();
        fixture.shared.upload_mode.store(0, Ordering::SeqCst);
        fixture.shared.send_mode.store(0, Ordering::SeqCst);
        let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
        let result = control_request(&socket, args).await;
        if phase == "upload" {
            assert_eq!(result["accepted"], true);
            assert_eq!(fixture.shared.uploads.lock().unwrap().len(), 2);
        } else {
            assert!(result["error"].as_str().unwrap().contains("unconfirmed"));
            assert_eq!(fixture.shared.uploads.lock().unwrap().len(), 1);
            assert_eq!(
                gateway.delivery_facts().unwrap()[0].status,
                DeliveryStatus::Unknown
            );
        }
        assert_eq!(fixture.shared.sends.lock().unwrap().len(), 1);
        gateway.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn outgoing_file_validates_bytes_names_and_control_shape_before_network() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let config = fixture.config(root.path());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    for (field, value) in [
        ("token", json!("wrong")),
        ("name", json!("../file.txt")),
        ("md5", json!("0".repeat(32))),
        ("data_base64", json!("?bad")),
        ("data_base64", json!("")),
        ("path", json!("/tmp/file.txt")),
        ("url", json!("https://example.com/file")),
        ("open_id", json!("ou_not_allowed")),
    ] {
        let mut args = send_file("invalid-file", "report.txt", b"bytes");
        args[field] = value;
        assert!(control_request(&socket, args).await.get("error").is_some());
    }
    assert!(
        control_request(
            &socket,
            send_file(
                "oversized-file",
                "large.bin",
                &vec![0; 20 * 1024 * 1024 + 1]
            )
        )
        .await
        .get("error")
        .is_some()
    );
    assert!(fixture.shared.uploads.lock().unwrap().is_empty());
    assert!(fixture.shared.sends.lock().unwrap().is_empty());
    assert!(gateway.delivery_facts().unwrap().is_empty());
    assert_eq!(fixture.shared.tokens.load(Ordering::SeqCst), 0);
    assert_eq!(
        control_request(&socket, send_file("small-file", "one.txt", b"x")).await["accepted"],
        true
    );
    gateway.shutdown().await.unwrap();
}
