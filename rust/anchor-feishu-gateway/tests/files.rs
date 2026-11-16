mod support;

use std::{collections::BTreeSet, path::Path, sync::atomic::Ordering, time::Duration};

use anchor_channel_transport::{digest, ledger::Ledger};
use anchor_feishu_gateway::{
    ChannelEvent, ConnectionStatus, DeliveryStatus, Gateway, GatewayError,
    protocol::normalize_message,
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

fn post_event(id: &str, text: &str, attachments: Vec<Value>) -> ChannelEvent {
    let mut event = normalize_message(&event(id, "post"), "cli_fixture_app")
        .unwrap()
        .unwrap();
    event.message_type = "post".into();
    event.text = text.into();
    event.attachments = attachments;
    event
}

// Open only on a fresh gateway, before admitting any work. Subsequent tests
// use this handle to exercise stored descriptors independently of the parser.
fn stored_events(fixture: &Fixture, root: &Path) -> Ledger {
    let profile =
        digest(&serde_json::to_vec(&json!(["feishu", "cli_fixture_app", fixture.base])).unwrap());
    Ledger::open(&root.join("state/delivery.sqlite"), &profile).unwrap()
}

async fn release_resource(fixture: &Fixture, bytes: &[u8], mode: usize) {
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.shared.resource_entered.notified(),
    )
    .await
    .unwrap();
    *fixture.shared.resource_body.lock().unwrap() = bytes.to_vec();
    fixture.shared.resource_mode.store(mode, Ordering::SeqCst);
    fixture.shared.resource_release.notify_one();
}

#[tokio::test]
async fn private_post_text_and_multiple_images_keep_only_descriptors_in_ledger() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    *fixture.shared.resource_body.lock().unwrap() = png();
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let content = json!({"title":"Compare","content":[
        [{"tag":"text","text":"these images"}],
        [{"tag":"img","image_key":"img_first"},{"tag":"img","image_key":"img_second"}]
    ]});
    let original = normalize_message(
        &media_event("om_post_images", "post", content.clone()),
        "cli_fixture_app",
    )
    .unwrap()
    .unwrap();
    send_media(&fixture, "om_post_images", "post", content).await;
    assert_eq!(fixture.ack().await["code"], 200);
    fixture.wait_settlements(1).await;
    let mut expected = serde_json::to_value(&original).unwrap();
    expected["attachments"] = json!([
        {"name":"01-image.png","data_base64":STANDARD.encode(png()),"media_type":"image/png"},
        {"name":"02-image.png","data_base64":STANDARD.encode(png()),"media_type":"image/png"}
    ]);
    assert_eq!(fixture.shared.events.lock().unwrap()[0]["event"], expected);
    assert_eq!(original.text, "Compare\nthese images");
    assert_eq!(
        *fixture.shared.downloads.lock().unwrap(),
        vec![
            json!({"message_id":"om_post_images","key":"img_first","type":"image"}),
            json!({"message_id":"om_post_images","key":"img_second","type":"image"})
        ]
    );
    let database = rusqlite::Connection::open(root.path().join("state/delivery.sqlite")).unwrap();
    let stored: String = database
        .query_row(
            "SELECT event_json FROM inbound WHERE event_id='om_post_images'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<ChannelEvent>(&stored).unwrap(),
        original
    );
    assert!(!stored.contains("data_base64"));
    assert_eq!(
        fixture.shared.settlements.lock().unwrap()[0]["event"]["attachments"],
        json!([])
    );
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn posts_allow_no_resources_and_empty_text_and_preserve_single_image_name() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    *fixture.shared.resource_body.lock().unwrap() = png();
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let ledger = stored_events(&fixture, root.path());
    for (index, (text, attachments)) in [
        ("only text", vec![]),
        ("", vec![]),
        (
            "",
            vec![json!({"kind":"image","key":"img_single","name":"image"})],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let original = post_event(&format!("om_post_optional_{index}"), text, attachments);
        ledger.admit(&original).unwrap();
        fixture.wait_settlements(index + 1).await;
        let mut expected = serde_json::to_value(&original).unwrap();
        if !original.attachments.is_empty() {
            expected["attachments"] = json!([
                {"name":"image.png","data_base64":STANDARD.encode(png()),"media_type":"image/png"}
            ]);
        }
        assert_eq!(
            fixture.shared.events.lock().unwrap()[index]["event"],
            expected
        );
        assert_eq!(
            fixture.shared.downloads.lock().unwrap().len(),
            usize::from(index == 2)
        );
        assert_eq!(ledger.event(&original.event_id).unwrap(), original);
    }
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_mixed_resources_use_unique_names_despite_duplicates_and_utf8_truncation() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    fixture
        .shared
        .resource_blocked
        .store(true, Ordering::SeqCst);
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let ledger = stored_events(&fixture, root.path());
    let mut resources = vec![json!({"kind":"image","key":"img_first","name":"image"})];
    let mut names = vec![
        "image.png".to_owned(),
        "image.png".to_owned(),
        "01-image.png".to_owned(),
        format!("a{}.txt", "x".repeat(250)),
        format!("b{}.txt", "x".repeat(250)),
        format!("{}abc.txt", "\u{1f4c4}".repeat(62)),
    ];
    names.resize(15, "same.txt".into());
    for (index, name) in names.iter().enumerate() {
        resources.push(json!({"kind":"file","key":format!("file_{index}"),"name":name}));
    }
    let original = post_event("om_post_mixed", "mixed files", resources);
    ledger.admit(&original).unwrap();
    release_resource(&fixture, &png(), 0).await;
    for _ in &names {
        release_resource(&fixture, b"file contents", 0).await;
    }
    fixture.wait_settlements(1).await;
    let event = fixture.shared.events.lock().unwrap()[0]["event"].clone();
    let attachments = event["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 16);
    let mut unique = BTreeSet::new();
    for (index, attachment) in attachments.iter().enumerate() {
        let name = attachment["name"].as_str().unwrap();
        assert!(unique.insert(name));
        assert!(name.len() <= 255);
        assert!(name.starts_with(&format!("{:02}-", index + 1)));
        assert!(
            !name
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
        );
        let bytes = STANDARD
            .decode(attachment["data_base64"].as_str().unwrap())
            .unwrap();
        if index == 0 {
            assert_eq!(name, "01-image.png");
            assert_eq!(attachment["media_type"], "image/png");
            assert_eq!(bytes, png());
        } else {
            assert!(attachment.get("media_type").is_none());
            assert_eq!(bytes, b"file contents");
        }
    }
    assert_eq!(attachments[1]["name"], "02-image.png");
    assert_eq!(attachments[2]["name"], "03-image.png");
    assert_eq!(attachments[3]["name"], "04-01-image.png");
    for index in [4, 5, 6] {
        assert!(
            attachments[index]["name"]
                .as_str()
                .unwrap()
                .ends_with(".txt")
        );
    }
    let downloads = fixture.shared.downloads.lock().unwrap().clone();
    for (download, descriptor) in downloads.iter().zip(&original.attachments) {
        assert_eq!(download["message_id"], original.event_id);
        assert_eq!(download["key"], descriptor["key"]);
        assert_eq!(download["type"], descriptor["kind"]);
    }
    assert_eq!(downloads.len(), 16);
    assert_eq!(ledger.event(&original.event_id).unwrap(), original);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_validates_every_descriptor_and_resource_count_before_download() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let ledger = stored_events(&fixture, root.path());
    let mut errors = gateway.subscribe_errors();
    let valid = json!({"kind":"file","key":"file_good","name":"safe.txt"});
    let mut invalid = Vec::new();
    for (field, values) in [
        ("kind", vec![json!("video"), json!("Image"), Value::Null]),
        (
            "key",
            vec![
                json!(""),
                json!("../file"),
                json!("https://example.com/file"),
                json!("x".repeat(201)),
            ],
        ),
        (
            "name",
            vec![
                json!(""),
                json!(" "),
                json!("."),
                json!(".."),
                json!("dir/file"),
                json!("dir\\file"),
                json!("file\n"),
                json!("x".repeat(256)),
            ],
        ),
        ("data_base64", vec![json!("not a descriptor")]),
    ] {
        for value in values {
            let mut descriptor = valid.clone();
            descriptor[field] = value;
            invalid.push(vec![valid.clone(), descriptor]);
        }
    }
    invalid.push(vec![
        valid.clone(),
        json!({"kind":"file","name":"missing-key.txt"}),
    ]);
    invalid.push(vec![valid.clone(), Value::Null]);
    invalid.push(vec![valid; 17]);
    invalid.push(vec![
        json!({"kind":"image","key":"img_key","name":"image"});
        9
    ]);
    for (index, attachments) in invalid.into_iter().enumerate() {
        let original = post_event(&format!("om_post_invalid_{index}"), "", attachments);
        ledger.admit(&original).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), errors.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(error, GatewayError::Invalid(_)),
            "case {index}: {error:?}"
        );
        assert!(fixture.shared.downloads.lock().unwrap().is_empty());
        assert!(fixture.shared.events.lock().unwrap().is_empty());
    }
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_enforces_host_image_and_file_budgets_including_images_sent_as_files() {
    const MIB: usize = 1024 * 1024;
    for (label, kind, size, count, image, accepted) in [
        ("eight_images", "image", png().len(), 8, true, true),
        ("image_total_at_limit", "image", 10 * MIB, 2, true, true),
        ("image_total_over", "image", 7 * MIB, 3, true, false),
        ("image_size_over", "image", 10 * MIB + 1, 1, true, false),
        ("file_image_count", "file", png().len(), 9, true, false),
        ("file_image_size", "file", 10 * MIB + 1, 1, true, false),
        ("file_image_total", "file", 7 * MIB, 3, true, false),
        ("file_total_at_limit", "file", 10 * MIB, 5, false, true),
        ("file_total_over", "file", 17 * MIB, 3, false, false),
        ("file_size_at_limit", "file", 20 * MIB, 1, false, true),
        ("file_size_over", "file", 20 * MIB + 1, 1, false, false),
        ("unsupported_image", "image", 10, 1, false, false),
        ("empty_file", "file", 0, 1, false, false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let fixture = Fixture::new().await;
        let mut bytes = if image { png() } else { Vec::new() };
        bytes.resize(size, b'x');
        *fixture.shared.resource_body.lock().unwrap() = bytes;
        let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
        poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
        let ledger = stored_events(&fixture, root.path());
        let mut errors = gateway.subscribe_errors();
        let original = post_event(
            &format!("om_{label}"),
            "",
            (0..count)
                .map(|index| json!({"kind":kind,"key":format!("resource_{index}"),"name":"attachment"}))
                .collect(),
        );
        ledger.admit(&original).unwrap();
        if accepted {
            fixture.wait_settlements(1).await;
            let events = fixture.shared.events.lock().unwrap();
            let attachments = events[0]["event"]["attachments"].as_array().unwrap();
            assert_eq!(attachments.len(), count, "{label}");
            for attachment in attachments {
                assert_eq!(
                    STANDARD
                        .decode(attachment["data_base64"].as_str().unwrap())
                        .unwrap()
                        .len(),
                    size,
                    "{label}"
                );
            }
        } else {
            let error = tokio::time::timeout(Duration::from_secs(10), errors.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(error, GatewayError::Invalid(_) | GatewayError::Media),
                "{label}: {error:?}"
            );
            assert!(fixture.shared.events.lock().unwrap().is_empty(), "{label}");
            assert!(
                fixture.shared.settlements.lock().unwrap().is_empty(),
                "{label}"
            );
        }
        assert_eq!(ledger.event(&original.event_id).unwrap(), original);
        gateway.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn post_download_failure_keeps_descriptors_and_recovery_refetches_entire_batch() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    fixture
        .shared
        .resource_blocked
        .store(true, Ordering::SeqCst);
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let ledger = stored_events(&fixture, root.path());
    let mut errors = gateway.subscribe_errors();
    let original = post_event(
        "om_post_retry",
        "retry both",
        vec![
            json!({"kind":"image","key":"img_first","name":"image"}),
            json!({"kind":"image","key":"img_second","name":"image"}),
        ],
    );
    ledger.admit(&original).unwrap();
    release_resource(&fixture, &png(), 0).await;
    release_resource(&fixture, &png(), 1).await;
    let error = tokio::time::timeout(Duration::from_secs(5), errors.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(error, GatewayError::Media));
    assert!(fixture.shared.events.lock().unwrap().is_empty());
    assert!(fixture.shared.settlements.lock().unwrap().is_empty());
    assert_eq!(ledger.event(&original.event_id).unwrap(), original);
    gateway.shutdown().await.unwrap();
    fixture
        .shared
        .resource_blocked
        .store(false, Ordering::SeqCst);
    fixture.shared.resource_mode.store(0, Ordering::SeqCst);
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    fixture.wait_settlements(1).await;
    let downloads = fixture.shared.downloads.lock().unwrap().clone();
    assert_eq!(downloads.len(), 4);
    assert_eq!(downloads[..2], downloads[2..]);
    assert_eq!(fixture.shared.events.lock().unwrap().len(), 1);
    assert_eq!(
        fixture.shared.events.lock().unwrap()[0]["event"]["attachments"],
        json!([
            {"name":"01-image.png","data_base64":STANDARD.encode(png()),"media_type":"image/png"},
            {"name":"02-image.png","data_base64":STANDARD.encode(png()),"media_type":"image/png"}
        ])
    );
    assert_eq!(ledger.event(&original.event_id).unwrap(), original);
    gateway.shutdown().await.unwrap();
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
