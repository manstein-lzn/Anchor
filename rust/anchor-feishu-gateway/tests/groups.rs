mod support;

use anchor_feishu_gateway::{ConnectionStatus, Gateway};
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, time::Duration};
use support::{Fixture, event, event_frame, poll};

fn group(id: &str) -> Value {
    let mut value = event(id, "@_user_1 check the project");
    value["event"]["message"]["chat_type"] = json!("group");
    value["event"]["message"]["chat_id"] = json!("oc_project");
    value["event"]["message"]["mentions"] = json!([{
        "key":"@_user_1", "id":{"open_id":"ou_fixture_bot"}, "mentioned_type":"bot"
    }]);
    value
}

async fn send(fixture: &mut Fixture, value: Value) {
    let mut frame = event_frame("unused", "unused");
    frame.payload = serde_json::to_vec(&value).unwrap();
    fixture.send(frame).await;
    assert_eq!(fixture.ack().await["code"], 200);
}

#[tokio::test]
async fn groups_require_an_explicit_chat_human_member_and_real_self_mention() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    let mut config = fixture.config(root.path());
    config.groups.insert("oc_project".into());
    let gateway = Gateway::start(config).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    for (index, (pointer, replacement)) in [
        ("/event/message/chat_id", json!("oc_unapproved")),
        ("/event/message/mentions", json!([])),
        (
            "/event/message/mentions/0/id/open_id",
            json!("ou_other_bot"),
        ),
        ("/event/message/mentions/0/mentioned_type", json!("user")),
        (
            "/event/message/content",
            json!(json!({"text":"@Project Manager plain text"}).to_string()),
        ),
        ("/event/sender/sender_type", json!("bot")),
        ("/event/sender/sender_id/open_id", json!("ou_not_allowed")),
        ("/event/message/message_type", json!("file")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut value = group(&format!("om_filtered_{index}"));
        *value.pointer_mut(pointer).unwrap() = replacement;
        send(&mut fixture, value).await;
    }
    let mut first_message = group("om_group_alice");
    first_message["event"]["message"]["content"] =
        json!(json!({"text":"@_user_1 compare @_user_10"}).to_string());
    send(&mut fixture, first_message).await;
    fixture.wait_settlements(1).await;
    let first = fixture.shared.events.lock().unwrap()[0].clone();
    assert_eq!(first["event"]["text"], "compare @_user_10");
    assert_eq!(first["event"]["metadata"]["chat_type"], "group");
    assert_eq!(first["event"]["sender_id"], "ou_alice");
    assert_eq!(first["event"]["reply_target"], "oc_project");
    let mut next = group("om_group_bob");
    next["event"]["sender"]["sender_id"]["open_id"] = json!("ou_bob");
    send(&mut fixture, next.clone()).await;
    fixture.wait_settlements(2).await;
    send(&mut fixture, next).await;
    fixture
        .event("om_still_private", "private conversation")
        .await;
    assert_eq!(fixture.ack().await["code"], 200);
    fixture.wait_settlements(3).await;
    assert_eq!(fixture.shared.events.lock().unwrap().len(), 3);
    {
        let replies = fixture.shared.replies.lock().unwrap();
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[0]["message_id"], "om_group_alice");
        assert_eq!(replies[1]["message_id"], "om_group_bob");
    }
    assert!(fixture.shared.sends.lock().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn removing_group_authorization_blocks_pending_and_ready_replies_after_restart() {
    use anchor_channel_transport::{digest, ledger::Ledger};
    for ready in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut fixture = Fixture::new().await;
        fixture.shared.blocked.store(true, Ordering::SeqCst);
        let mut config = fixture.config(root.path());
        config.groups.insert("oc_project".into());
        let gateway = Gateway::start(config).await.unwrap();
        poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
        send(&mut fixture, group("om_revoked_group")).await;
        tokio::time::timeout(Duration::from_secs(2), fixture.shared.entered.notified())
            .await
            .unwrap();
        gateway.shutdown().await.unwrap();
        if ready {
            let profile = digest(
                &serde_json::to_vec(&json!(["feishu", "cli_fixture_app", fixture.base])).unwrap(),
            );
            let ledger =
                Ledger::open(&root.path().join("state/delivery.sqlite"), &profile).unwrap();
            ledger
                .ready("om_revoked_group", "previously prepared reply", None, None)
                .unwrap();
        }
        fixture.shared.blocked.store(false, Ordering::SeqCst);
        let gateway = Gateway::start(fixture.config(root.path())).await.unwrap();
        poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
        send(&mut fixture, group("om_new_denied_group")).await;
        // A private round trip provides a bounded barrier for worker recovery.
        fixture.event("om_private_barrier", "still private").await;
        assert_eq!(fixture.ack().await["code"], 200);
        fixture.wait_settlements(1).await;
        assert_eq!(fixture.shared.events.lock().unwrap().len(), 2);
        {
            let replies = fixture.shared.replies.lock().unwrap();
            assert_eq!(replies.len(), 1);
            assert_eq!(replies[0]["message_id"], "om_private_barrier");
        }
        gateway.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn failed_group_identity_lookup_preserves_private_chat_and_can_recover() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    fixture
        .shared
        .bot_info_failure
        .store(true, Ordering::SeqCst);
    let mut config = fixture.config(root.path());
    config.groups.insert("oc_project".into());
    let gateway = Gateway::start(config).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let mut frame = event_frame("om_lookup_retry", "unused");
    frame.payload = serde_json::to_vec(&group("om_lookup_retry")).unwrap();
    fixture.send(frame).await;
    assert_eq!(fixture.ack().await["code"], 500);
    fixture
        .event("om_private_without_bot_info", "private still works")
        .await;
    assert_eq!(fixture.ack().await["code"], 200);
    fixture.wait_settlements(1).await;
    assert_eq!(fixture.shared.events.lock().unwrap().len(), 1);
    fixture
        .shared
        .bot_info_failure
        .store(false, Ordering::SeqCst);
    send(&mut fixture, group("om_lookup_retry")).await;
    fixture.wait_settlements(2).await;
    assert_eq!(fixture.shared.events.lock().unwrap().len(), 2);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn peer_bot_requires_exact_grant_structured_mention_and_platform_reply_ancestry() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    let mut config = fixture.config(root.path());
    config.groups.insert("oc_project".into());
    config.peer_bots.insert("ou_peer".into());
    let gateway = Gateway::start(config).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let mut bot = group("om_peer_request");
    bot["event"]["sender"]["sender_type"] = json!("bot");
    bot["event"]["sender"]["sender_id"]["open_id"] = json!("ou_peer");
    send(&mut fixture, bot.clone()).await;
    bot["event"]["message"]["root_id"] = json!("om_human_root");
    for (pointer, value) in [
        ("/event/sender/sender_id/open_id", json!("ou_other")),
        ("/event/sender/sender_id/open_id", json!("ou_fixture_bot")),
        ("/event/message/chat_type", json!("p2p")),
        ("/event/message/chat_id", json!("oc_elsewhere")),
        (
            "/event/message/mentions/0/id/open_id",
            json!("ou_elsewhere"),
        ),
    ] {
        let mut invalid = bot.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        send(&mut fixture, invalid).await;
    }
    send(&mut fixture, bot.clone()).await;
    fixture.wait_settlements(1).await;
    send(&mut fixture, bot).await;
    {
        let events = fixture.shared.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"]["metadata"]["sender_type"], "bot");
        assert_eq!(
            events[0]["event"]["metadata"]["root_inbound_id"],
            anchor_channel_transport::channel_inbound_id("feishu", "om_human_root")
        );
    }
    assert!(fixture.shared.activity.lock().unwrap().is_empty());
    assert_eq!(fixture.shared.replies.lock().unwrap().len(), 1);
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn mentioned_group_posts_deliver_text_and_images_without_relaxing_admission() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    *fixture.shared.resource_body.lock().unwrap() = STANDARD.decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aAdkAAAAASUVORK5CYII="
    ).unwrap();
    let mut config = fixture.config(root.path());
    config.groups.insert("oc_project".into());
    config.peer_bots.insert("ou_peer".into());
    let gateway = Gateway::start(config).await.unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    let mut post = group("om_group_post");
    post["event"]["message"]["message_type"] = json!("post");
    post["event"]["message"]["content"] = json!(
        json!({"zh_cn":{"content":[[
            {"tag":"at","user_id":"@_user_1"},
            {"tag":"text","text":"inspect screenshot"},
            {"tag":"img","image_key":"img_group"}
        ]]}})
        .to_string()
    );
    for (index, (pointer, value)) in [
        ("/event/message/mentions", json!([])),
        ("/event/message/chat_id", json!("oc_other")),
        ("/event/sender/sender_id/open_id", json!("ou_other")),
        (
            "/event/message/mentions/0/id/open_id",
            json!("ou_other_bot"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut denied = post.clone();
        denied["event"]["message"]["message_id"] = json!(format!("om_post_denied_{index}"));
        *denied.pointer_mut(pointer).unwrap() = value;
        send(&mut fixture, denied).await;
    }
    let mut peer = post.clone();
    peer["event"]["sender"]["sender_type"] = json!("bot");
    peer["event"]["sender"]["sender_id"]["open_id"] = json!("ou_peer");
    peer["event"]["message"]["root_id"] = json!("om_human_root");
    send(&mut fixture, peer).await;
    send(&mut fixture, post.clone()).await;
    fixture.wait_settlements(1).await;
    send(&mut fixture, post.clone()).await;
    post["event"]["message"]["message_id"] = json!("om_group_image_only_post");
    post["event"]["message"]["content"] = json!(
        json!({"content":[[
            {"tag":"at","user_id":"ou_fixture_bot"},
            {"tag":"img","image_key":"img_only"}
        ]]})
        .to_string()
    );
    send(&mut fixture, post).await;
    fixture.wait_settlements(2).await;
    {
        let events = fixture.shared.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event"]["text"], "inspect screenshot");
        assert_eq!(events[1]["event"]["text"], "");
        for event in events.iter() {
            assert_eq!(event["event"]["message_type"], "post");
            assert_eq!(event["event"]["attachments"][0]["media_type"], "image/png");
        }
        let settlements = fixture.shared.settlements.lock().unwrap();
        assert_eq!(settlements[1]["event"]["text"], "");
        assert_eq!(settlements[1]["event"]["attachments"], json!([]));
    }
    assert_eq!(fixture.shared.downloads.lock().unwrap().len(), 2);
    gateway.shutdown().await.unwrap();
}
