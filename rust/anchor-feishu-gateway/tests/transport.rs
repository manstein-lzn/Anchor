mod support;

use std::{sync::atomic::Ordering, time::Duration};

use anchor_feishu_gateway::{
    ConnectionStatus, DeliveryStatus, Gateway, GatewayError,
    protocol::{Frame, normalize_message},
};
use serde_json::{Value, json};
use support::{Fixture, TOKEN, control, event, event_frame, poll};

#[tokio::test]
async fn durable_ack_precedes_model_work_and_dedup_replies_and_settles_once_across_reconnect() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    fixture.shared.blocked.store(true, Ordering::SeqCst);
    let config = fixture.config(root.path());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    assert_eq!(
        control(&socket, TOKEN, "status").await["status"],
        "authenticated"
    );
    assert_eq!(
        control(&socket, "wrong-token", "status").await["error"],
        "unauthorized channel request"
    );
    assert!(control(&socket, TOKEN, "send").await.get("error").is_some());
    fixture.event("om_first", "hello").await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), fixture.ack())
            .await
            .unwrap()["code"],
        200
    );
    tokio::time::timeout(Duration::from_secs(2), fixture.shared.entered.notified())
        .await
        .unwrap();
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    let mut duplicate = event_frame("om_first", "hello");
    let mut payload: Value = serde_json::from_slice(&duplicate.payload).unwrap();
    payload["header"]["event_id"] = json!("a-new-envelope-event-id");
    duplicate.payload = serde_json::to_vec(&payload).unwrap();
    fixture.send(duplicate).await;
    assert_eq!(fixture.ack().await["code"], 200);
    fixture.shared.blocked.store(false, Ordering::SeqCst);
    fixture.shared.release.notify_one();
    fixture.wait_settlements(1).await;
    assert_eq!(fixture.shared.events.lock().unwrap().len(), 1);
    let replies = fixture.shared.replies.lock().unwrap().clone();
    assert_eq!(replies.len(), 1);
    assert_eq!(support::card_text(&replies[0]), "reply to hello");
    assert_eq!(
        gateway.delivery_facts().unwrap()[0].status,
        DeliveryStatus::Confirmed
    );
    assert_eq!(
        fixture.shared.settlements.lock().unwrap()[0]["settlement"]["status"],
        "confirmed"
    );
    fixture.disconnect().await;
    poll(|| fixture.shared.connections.load(Ordering::SeqCst) >= 2).await;
    fixture.event("om_second", "again").await;
    assert_eq!(fixture.ack().await["code"], 200);
    fixture.wait_settlements(2).await;
    assert_eq!(fixture.shared.tokens.load(Ordering::SeqCst), 1);
    gateway.shutdown().await.unwrap();
    assert!(!socket.exists());
}

#[tokio::test]
async fn admission_filters_app_actor_chat_type_media_and_both_allowlists() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    let mut config = fixture.config(root.path());
    config.send_users.remove("ou_bob");
    let gateway = Gateway::start(config).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    for (pointer, value, expected) in [
        ("/header/app_id", "other-app", 500),
        ("/event/sender/sender_type", "app", 200),
        ("/event/message/chat_type", "group", 200),
        ("/event/message/message_type", "voice", 200),
        ("/event/sender/sender_id/open_id", "ou_unknown", 200),
        ("/event/sender/sender_id/open_id", "ou_bob", 200),
    ] {
        let mut payload = event("om_filtered", "do not admit");
        *payload.pointer_mut(pointer).unwrap() = json!(value);
        let mut frame = event_frame("om_filtered", "unused");
        frame.payload = serde_json::to_vec(&payload).unwrap();
        fixture.send(frame).await;
        assert_eq!(fixture.ack().await["code"], expected);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fixture.shared.events.lock().unwrap().is_empty());
    assert!(gateway.delivery_facts().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn official_split_frames_assemble_out_of_order_before_admission_and_echo_final_frame_identity()
 {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let source = event_frame("om_split", "split hello");
    let chunks: Vec<_> = source
        .payload
        .chunks(source.payload.len().div_ceil(3))
        .map(<[u8]>::to_vec)
        .collect();
    for index in [2, 0, 1] {
        let mut frame = source.clone();
        frame.seq_id = 100 + index as u64;
        frame.payload = chunks[index].clone();
        frame
            .headers
            .iter_mut()
            .find(|header| header.key == "sum")
            .unwrap()
            .value = "3".into();
        frame
            .headers
            .iter_mut()
            .find(|header| header.key == "seq")
            .unwrap()
            .value = index.to_string();
        fixture.send(frame).await;
    }
    let ack = fixture.next("event").await;
    assert_eq!(ack.seq_id, 101);
    assert_eq!(ack.header("seq"), Some("1"));
    assert_eq!(
        serde_json::from_slice::<Value>(&ack.payload).unwrap()["code"],
        200
    );
    fixture.wait_settlements(1).await;
    assert_eq!(
        fixture.shared.events.lock().unwrap()[0]["event"]["text"],
        "split hello"
    );
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn admitted_input_survives_worker_cancellation_but_unknown_sends_never_replay_after_restart()
{
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    fixture.shared.blocked.store(true, Ordering::SeqCst);
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.event("om_recover", "survive restart").await;
    assert_eq!(fixture.ack().await["code"], 200);
    tokio::time::timeout(Duration::from_secs(2), fixture.shared.entered.notified())
        .await
        .unwrap();
    gateway.shutdown().await.unwrap();
    fixture.shared.blocked.store(false, Ordering::SeqCst);
    fixture.shared.reply_mode.store(1, Ordering::SeqCst);
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    fixture.wait_settlements(1).await;
    assert_eq!(fixture.shared.replies.lock().unwrap().len(), 1);
    assert_eq!(
        fixture.shared.settlements.lock().unwrap()[0]["settlement"]["status"],
        "unknown"
    );
    assert_eq!(
        gateway.delivery_facts().unwrap()[0].status,
        DeliveryStatus::Unknown
    );
    gateway.shutdown().await.unwrap();
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.event("om_recover", "survive restart").await;
    assert_eq!(fixture.ack().await["code"], 200);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(fixture.shared.replies.lock().unwrap().len(), 1);
    assert_eq!(fixture.shared.events.lock().unwrap().len(), 2);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn non_success_http_or_api_code_cannot_confirm_an_external_send() {
    for mode in [2, 3] {
        let root = tempfile::tempdir().unwrap();
        let mut fixture = Fixture::new().await;
        fixture.shared.reply_mode.store(mode, Ordering::SeqCst);
        let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
        poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
        fixture.event("om_unconfirmed", "test").await;
        assert_eq!(fixture.ack().await["code"], 200);
        fixture.wait_settlements(1).await;
        assert_eq!(
            fixture.shared.settlements.lock().unwrap()[0]["settlement"]["status"],
            "unknown"
        );
        assert_eq!(
            gateway.delivery_facts().unwrap()[0].status,
            DeliveryStatus::Unknown
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(fixture.shared.replies.lock().unwrap().len(), 1);
        gateway.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn oversized_render_settles_the_host_receipt_without_claiming_or_replaying_a_send() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    *fixture.shared.callback_response.lock().unwrap() = Some(json!({
        "text":"\\".repeat(20_480),
        "receipt":{"key":"channel-oversized-render","content_sha256":"b".repeat(64)},
    }));
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.event("om_oversized_render", "format this").await;
    assert_eq!(fixture.ack().await["code"], 200);
    fixture.wait_settlements(1).await;
    assert_eq!(
        fixture.shared.settlements.lock().unwrap()[0]["settlement"],
        json!({"key":"channel-oversized-render","content_sha256":"b".repeat(64),"status":"suppressed"})
    );
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    assert!(gateway.delivery_facts().unwrap().is_empty());
    gateway.shutdown().await.unwrap();

    *fixture.shared.callback_response.lock().unwrap() = None;
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.event("om_oversized_render", "format this").await;
    assert_eq!(fixture.ack().await["code"], 200);
    fixture.event("om_after_oversized", "hello").await;
    assert_eq!(fixture.ack().await["code"], 200);
    fixture.wait_settlements(2).await;
    assert_eq!(fixture.shared.events.lock().unwrap().len(), 2);
    assert_eq!(fixture.shared.replies.lock().unwrap().len(), 1);
    assert_eq!(gateway.delivery_facts().unwrap().len(), 1);
    gateway.shutdown().await.unwrap();
}

#[test]
fn protobuf_golden_and_private_text_normalization_match_official_contract() {
    // Required zero scalar fields are present in the official proto2 envelope.
    let official_ping = [
        8, 0, 16, 0, 24, 1, 32, 0, 42, 12, 10, 4, b't', b'y', b'p', b'e', 18, 4, b'p', b'i', b'n',
        b'g',
    ];
    assert_eq!(Frame::ping(1).encode_binary(), official_ping);
    assert_eq!(
        Frame::decode_binary(&official_ping).unwrap(),
        Frame::ping(1)
    );
    let normalized = normalize_message(&event("om_identity", "hello"), "cli_fixture_app")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(normalized).unwrap(),
        json!({
            "source":"feishu","event_id":"om_identity","sender_id":"ou_alice","conversation_id":"oc_private",
            "reply_target":"oc_private","message_type":"text","text":"hello",
            "metadata":{"chat_type":"single","request_id":"om_identity"},"attachments":[],
        })
    );
}

#[tokio::test]
async fn configuration_and_state_are_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    for url in [
        "http://open.feishu.cn",
        "http://localhost:8000",
        "https://example.com?token=x",
        "https://user:pass@example.com",
        "https://example.com/open-apis",
    ] {
        let mut config = fixture.config(root.path());
        config.api_base_url = url.into();
        assert!(config.validate().is_err());
    }
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    assert!(matches!(
        Gateway::start(fixture.config(root.path())).await,
        Err(GatewayError::AlreadyRunning)
    ));
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_blocked_durable_admission_gets_a_bounded_failure_ack_and_its_late_commit_is_not_lost() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let blocker = rusqlite::Connection::open(root.path().join("state/delivery.sqlite")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    fixture
        .event("om_slow_commit", "durable then asynchronous")
        .await;
    let ack = tokio::time::timeout(Duration::from_millis(1800), fixture.ack())
        .await
        .expect("admission lock must not consume Feishu's three-second callback window");
    assert_eq!(ack["code"], 500);
    let shutdown = tokio::spawn(gateway.shutdown());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !shutdown.is_finished(),
        "a pending writer must keep the state lease"
    );
    assert!(matches!(
        Gateway::start(fixture.config(root.path())).await,
        Err(GatewayError::AlreadyRunning)
    ));
    blocker.execute_batch("ROLLBACK").unwrap();
    shutdown.await.unwrap().unwrap();
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.wait_settlements(1).await;
    fixture
        .event("om_slow_commit", "durable then asynchronous")
        .await;
    assert_eq!(fixture.ack().await["code"], 200);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fixture.shared.events.lock().unwrap().len(), 1);
    assert_eq!(fixture.shared.replies.lock().unwrap().len(), 1);
    gateway.shutdown().await.unwrap();
}
