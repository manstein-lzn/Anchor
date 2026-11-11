mod support;

use anchor_feishu_gateway::{ConnectionStatus, DeliveryStatus, Gateway, GatewayError};
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, time::Duration};
use support::{Fixture, TOKEN, control_request, poll};

fn request(id: &str) -> Value {
    json!({"operation":"send","token":TOKEN,"request_id":id,"open_id":"ou_alice","chat_id":"oc_private","content":"Task completed"})
}

#[tokio::test]
async fn notification_is_authenticated_bounded_and_scoped_before_claim_or_network() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let config = fixture.config(root.path());
    let socket = config.socket_path();
    let gateway = Gateway::start(config).await.unwrap();
    for (field, value) in [
        ("token", json!("wrong")),
        ("request_id", json!("../bad")),
        ("open_id", json!("ou_unknown")),
        ("open_id", json!("ou_alice?x=y")),
        ("chat_id", json!("")),
        ("content", json!(" \n")),
        ("content", json!("x".repeat(20481))),
        // Source text fits, but double serialization would exceed Feishu's
        // request-body limit. Local rejection must not become an unknown send.
        ("content", json!("\"".repeat(9000))),
        ("recipient", json!("spoof")),
    ] {
        let mut bad = request("invalid");
        bad[field] = value;
        assert!(control_request(&socket, bad).await.get("error").is_some());
    }
    assert!(gateway.delivery_facts().unwrap().is_empty());
    assert!(fixture.shared.sends.lock().unwrap().is_empty());
    assert_eq!(fixture.shared.tokens.load(Ordering::SeqCst), 0);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn confirmed_notification_reentry_reads_the_old_fact_and_changed_identity_or_content_conflicts()
 {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let socket = fixture.config(root.path()).socket_path();
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    let args = request("channel-task-fixture");
    let (first, concurrent) = tokio::join!(
        control_request(&socket, args.clone()),
        control_request(&socket, args.clone()),
    );
    assert!(first["accepted"] == true || concurrent["accepted"] == true);
    let ack = control_request(&socket, args.clone()).await;
    assert_eq!(
        ack,
        json!({"accepted":true,"request_id":"channel-task-fixture"})
    );
    let bodies = fixture.shared.sends.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["receive_id"], "ou_alice");
    assert_eq!(support::card_text(&bodies[0]), "Task completed");
    assert_eq!(
        gateway.delivery_facts().unwrap()[0].status,
        DeliveryStatus::Confirmed
    );
    gateway.shutdown().await.unwrap();
    // No token or platform request is needed to recover a confirmed outcome.
    fixture.shared.token_failure.store(true, Ordering::SeqCst);
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    assert_eq!(control_request(&socket, args.clone()).await, ack);
    for (field, value) in [
        ("content", "Different"),
        ("open_id", "ou_bob"),
        ("chat_id", "oc_other"),
    ] {
        let mut changed = args.clone();
        changed[field] = json!(value);
        assert!(
            control_request(&socket, changed).await["error"]
                .as_str()
                .unwrap()
                .contains("different content or identity")
        );
    }
    assert_eq!(fixture.shared.tokens.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.shared.sends.lock().unwrap().len(), 1);
    gateway.shutdown().await.unwrap();
    // Revocation still applies to receipt reentry.
    let mut config = fixture.config(root.path());
    config.send_users.clear();
    let gateway = Gateway::start(config).await.unwrap();
    assert_eq!(
        control_request(&socket, args).await["error"],
        "recipient is not allowed"
    );
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_http_business_and_missing_message_id_outcomes_never_replay_after_restart() {
    for mode in [1, 2, 3, 4] {
        let root = tempfile::tempdir().unwrap();
        let fixture = Fixture::new().await;
        fixture.shared.send_mode.store(mode, Ordering::SeqCst);
        let socket = fixture.config(root.path()).socket_path();
        let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
        poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
        let args = request("unknown");
        let send = tokio::spawn({
            let socket = socket.clone();
            let args = args.clone();
            async move { control_request(&socket, args).await }
        });
        if mode == 4 {
            tokio::time::timeout(
                Duration::from_secs(3),
                fixture.shared.send_entered.notified(),
            )
            .await
            .unwrap();
            gateway.shutdown().await.unwrap();
            fixture.shared.send_release.notify_one();
            assert!(send.await.unwrap().get("error").is_some());
        } else {
            assert!(send.await.unwrap().get("error").is_some());
            assert_eq!(
                gateway.delivery_facts().unwrap()[0].status,
                DeliveryStatus::Unknown
            );
            gateway.shutdown().await.unwrap();
        }
        let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
        assert!(
            control_request(&socket, args).await["error"]
                .as_str()
                .unwrap()
                .contains("unconfirmed")
        );
        assert_eq!(fixture.shared.sends.lock().unwrap().len(), 1);
        assert_eq!(
            gateway.delivery_facts().unwrap()[0].status,
            DeliveryStatus::Unknown
        );
        gateway.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn token_failure_does_not_claim_and_shutdown_joins_blocking_claim_before_releasing_state() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    fixture.shared.token_failure.store(true, Ordering::SeqCst);
    let socket = fixture.config(root.path()).socket_path();
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    assert!(
        control_request(&socket, request("token-failure"))
            .await
            .get("error")
            .is_some()
    );
    assert!(gateway.delivery_facts().unwrap().is_empty());
    fixture.shared.token_failure.store(false, Ordering::SeqCst);
    fixture.shared.token_blocked.store(true, Ordering::SeqCst);
    let blocker = rusqlite::Connection::open(root.path().join("state/delivery.sqlite")).unwrap();
    let send = tokio::spawn({
        let socket = socket.clone();
        async move { control_request(&socket, request("blocked-claim")).await }
    });
    // Reach the control request's token lookup before blocking SQLite; the
    // unrelated inbound worker may otherwise acquire the ledger mutex first.
    tokio::time::timeout(
        Duration::from_secs(3),
        fixture.shared.token_entered.notified(),
    )
    .await
    .unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    fixture.shared.token_release.notify_one();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let shutdown = tokio::spawn(gateway.shutdown());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!shutdown.is_finished());
    assert!(matches!(
        Gateway::start(fixture.config(root.path())).await,
        Err(GatewayError::AlreadyRunning)
    ));
    blocker.execute_batch("ROLLBACK").unwrap();
    shutdown.await.unwrap().unwrap();
    let _ = send.await.unwrap();
    let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
    assert_eq!(
        gateway.delivery_facts().unwrap()[0].status,
        DeliveryStatus::Unknown
    );
    assert!(fixture.shared.sends.lock().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}
