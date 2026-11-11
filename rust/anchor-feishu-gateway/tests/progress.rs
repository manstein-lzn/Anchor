mod support;

use std::{sync::atomic::Ordering, time::Duration};

use anchor_feishu_gateway::{ConnectionStatus, DeliveryStatus, Gateway};
use serde_json::json;
use support::{Fixture, card_text, poll};

fn progress_config(
    fixture: &Fixture,
    root: &std::path::Path,
) -> anchor_feishu_gateway::GatewayConfig {
    let mut config = fixture.config(root);
    config.progress_url = Some(format!("{}/anchor/progress", fixture.base));
    config
}

#[tokio::test]
async fn changing_host_steps_end_in_the_same_card_with_the_confirmed_final_answer() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    fixture.shared.blocked.store(true, Ordering::SeqCst);
    let gateway = Gateway::start(progress_config(&fixture, root.path()))
        .await
        .unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.event("om_progress", "# hello\n> quote").await;
    assert_eq!(fixture.ack().await["code"], 200);
    poll(|| fixture.shared.activity.lock().unwrap().len() == 1).await;
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    fixture
        .progress_line(
            "om_progress",
            json!({"category":"read_file","step":1,"content":"正在查看文件"}),
        )
        .await;
    poll(|| {
        fixture
            .shared
            .patches
            .lock()
            .unwrap()
            .iter()
            .any(|body| card_text(body).contains("第 1 步 · 文件: 正在查看文件"))
    })
    .await;
    fixture
        .progress_line(
            "om_progress",
            json!({"category":"command","step":2,"content":"命令执行已结束"}),
        )
        .await;
    poll(|| {
        fixture
            .shared
            .patches
            .lock()
            .unwrap()
            .iter()
            .any(|body| card_text(body).contains("第 2 步 · 工作区: 命令执行已结束"))
    })
    .await;
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    let patches = fixture.shared.patches.lock().unwrap().len();
    for _ in 0..6 {
        fixture
            .progress_line(
                "om_progress",
                json!({"category":"command","step":2,"content":"命令执行已结束"}),
            )
            .await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fixture.shared.patches.lock().unwrap().len(), patches);
    // Duplicate incoming frames neither create a second status card nor call
    // Host again; durable message identity still belongs to the original event.
    fixture.event("om_progress", "# hello\n> quote").await;
    assert_eq!(fixture.ack().await["code"], 200);
    fixture.shared.blocked.store(false, Ordering::SeqCst);
    fixture.shared.release.notify_one();
    fixture.wait_settlements(1).await;
    poll(|| fixture.shared.progress_closed.load(Ordering::SeqCst) == 1).await;
    poll(|| {
        fixture.shared.patches.lock().unwrap().iter().any(|body| {
            body["message_id"] == "om_activity_om_progress"
                && card_text(body) == "reply to # hello\n> quote"
        })
    })
    .await;
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    assert!(fixture.shared.recalls.lock().unwrap().is_empty());
    assert_eq!(fixture.shared.activity.lock().unwrap().len(), 1);
    assert_eq!(fixture.shared.events.lock().unwrap().len(), 1);
    assert_eq!(gateway.delivery_facts().unwrap().len(), 1);
    assert!(
        !fixture
            .shared
            .patches
            .lock()
            .unwrap()
            .iter()
            .any(|body| { card_text(body).contains("飞书已接受本轮回复") })
    );
    let completed_patches = fixture.shared.patches.lock().unwrap().len();
    fixture
        .progress_line(
            "om_progress",
            json!({"category":"plugin","step":99,"content":"stale"}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        fixture.shared.patches.lock().unwrap().len(),
        completed_patches
    );
    gateway.shutdown().await.unwrap();
    let gateway = Gateway::start(progress_config(&fixture, root.path()))
        .await
        .unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.event("om_progress", "# hello\n> quote").await;
    assert_eq!(fixture.ack().await["code"], 200);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fixture.shared.activity.lock().unwrap().len(), 1);
    assert_eq!(
        fixture.shared.patches.lock().unwrap().len(),
        completed_patches
    );
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    assert!(fixture.shared.recalls.lock().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn final_answer_waits_for_the_inflight_status_write_before_taking_its_card() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    fixture.shared.blocked.store(true, Ordering::SeqCst);
    let gateway = Gateway::start(progress_config(&fixture, root.path()))
        .await
        .unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.event("om_handoff", "finish in place").await;
    assert_eq!(fixture.ack().await["code"], 200);
    poll(|| fixture.shared.activity.lock().unwrap().len() == 1).await;
    fixture.shared.activity_mode.store(4, Ordering::SeqCst);
    fixture
        .progress_line(
            "om_handoff",
            json!({"category":"command","content":"命令执行已结束"}),
        )
        .await;
    tokio::time::timeout(
        Duration::from_secs(2),
        fixture.shared.activity_entered.notified(),
    )
    .await
    .unwrap();
    fixture.shared.blocked.store(false, Ordering::SeqCst);
    fixture.shared.release.notify_one();
    poll(|| fixture.shared.progress_closed.load(Ordering::SeqCst) == 1).await;
    assert!(gateway.delivery_facts().unwrap().is_empty());
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    assert!(
        !fixture
            .shared
            .patches
            .lock()
            .unwrap()
            .iter()
            .any(|body| card_text(body) == "reply to finish in place")
    );
    fixture.shared.activity_mode.store(0, Ordering::SeqCst);
    fixture.shared.activity_release.notify_waiters();
    fixture.wait_settlements(1).await;
    assert_eq!(
        card_text(fixture.shared.patches.lock().unwrap().last().unwrap()),
        "reply to finish in place"
    );
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    assert_eq!(
        gateway.delivery_facts().unwrap()[0].status,
        DeliveryStatus::Confirmed
    );
    let patches = fixture.shared.patches.lock().unwrap().len();
    gateway.shutdown().await.unwrap();
    assert_eq!(fixture.shared.patches.lock().unwrap().len(), patches);
    assert!(fixture.shared.recalls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn observation_ending_keeps_the_card_until_reply_delivery_is_known() {
    for observation in ["settled", "closed", "patch_failed"] {
        for confirmed in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let mut fixture = Fixture::new().await;
            fixture.shared.blocked.store(true, Ordering::SeqCst);
            let reuses_card = observation != "patch_failed";
            if reuses_card {
                fixture.shared.final_blocked.store(true, Ordering::SeqCst);
                fixture
                    .shared
                    .final_mode
                    .store(if confirmed { 0 } else { 2 }, Ordering::SeqCst);
            } else {
                fixture.shared.reply_blocked.store(true, Ordering::SeqCst);
                fixture
                    .shared
                    .reply_mode
                    .store(if confirmed { 0 } else { 1 }, Ordering::SeqCst);
            }
            let gateway = Gateway::start(progress_config(&fixture, root.path()))
                .await
                .unwrap();
            poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
            fixture.event("om_settled", "finish before delivery").await;
            assert_eq!(fixture.ack().await["code"], 200);
            poll(|| fixture.shared.activity.lock().unwrap().len() == 1).await;
            if observation == "settled" {
                fixture
                    .progress_line("om_settled", json!({"settled":true,"status":"completed"}))
                    .await;
            } else if observation == "closed" {
                fixture.shared.progress.lock().unwrap().remove("om_settled");
            } else {
                fixture.shared.activity_mode.store(2, Ordering::SeqCst);
                fixture
                    .progress_line(
                        "om_settled",
                        json!({"category":"command","content":"命令执行已结束"}),
                    )
                    .await;
            }
            poll(|| fixture.shared.progress_closed.load(Ordering::SeqCst) == 1).await;
            poll(|| {
                fixture.shared.patches.lock().unwrap().iter().any(|body| {
                    body["message_id"] == "om_activity_om_settled"
                        && card_text(body).contains(if observation == "settled" {
                            "已结束"
                        } else if observation == "closed" {
                            "实时进度暂不可用"
                        } else {
                            "命令执行已结束"
                        })
                })
            })
            .await;
            fixture.shared.activity_mode.store(0, Ordering::SeqCst);
            fixture.shared.blocked.store(false, Ordering::SeqCst);
            fixture.shared.release.notify_one();
            tokio::time::timeout(
                Duration::from_secs(2),
                if reuses_card {
                    fixture.shared.final_entered.notified()
                } else {
                    fixture.shared.reply_entered.notified()
                },
            )
            .await
            .unwrap();
            assert!(fixture.shared.settlements.lock().unwrap().is_empty());
            assert_eq!(
                gateway.delivery_facts().unwrap()[0].status,
                DeliveryStatus::Unknown
            );
            assert!(fixture.shared.recalls.lock().unwrap().is_empty());
            assert!(
                !fixture
                    .shared
                    .patches
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|body| { card_text(body).contains("飞书已接受本轮回复") })
            );
            if reuses_card {
                fixture.shared.final_release.notify_one();
            } else {
                fixture.shared.reply_release.notify_one();
            }
            fixture.wait_settlements(1).await;
            if reuses_card {
                assert_eq!(
                    card_text(fixture.shared.patches.lock().unwrap().last().unwrap()),
                    "reply to finish before delivery"
                );
            } else if !confirmed {
                poll(|| {
                    fixture
                        .shared
                        .patches
                        .lock()
                        .unwrap()
                        .last()
                        .is_some_and(|body| {
                            body["message_id"] == "om_activity_om_settled"
                                && card_text(body).contains("回复尚未确认送达")
                        })
                })
                .await;
            }
            assert_eq!(fixture.shared.activity.lock().unwrap().len(), 1);
            assert_eq!(
                fixture.shared.replies.lock().unwrap().len(),
                usize::from(!reuses_card)
            );
            assert_eq!(fixture.shared.progress_opened.load(Ordering::SeqCst), 1);
            assert_eq!(gateway.delivery_facts().unwrap().len(), 1);
            assert_eq!(
                fixture.shared.settlements.lock().unwrap()[0]["settlement"]["status"],
                if confirmed { "confirmed" } else { "unknown" }
            );
            gateway.shutdown().await.unwrap();
            assert!(fixture.shared.recalls.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn explicit_final_update_rejection_falls_back_under_the_original_delivery_claim() {
    for mode in [1, 3] {
        let root = tempfile::tempdir().unwrap();
        let mut fixture = Fixture::new().await;
        fixture.shared.blocked.store(true, Ordering::SeqCst);
        fixture.shared.final_mode.store(mode, Ordering::SeqCst);
        let gateway = Gateway::start(progress_config(&fixture, root.path()))
            .await
            .unwrap();
        poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
        fixture.event("om_update_rejected", "finish").await;
        assert_eq!(fixture.ack().await["code"], 200);
        poll(|| fixture.shared.activity.lock().unwrap().len() == 1).await;
        fixture.shared.blocked.store(false, Ordering::SeqCst);
        fixture.shared.release.notify_one();
        fixture.wait_settlements(1).await;
        poll(|| {
            fixture
                .shared
                .patches
                .lock()
                .unwrap()
                .last()
                .is_some_and(|body| {
                    body["message_id"] == "om_activity_om_update_rejected"
                        && card_text(body).contains("最终回复已另行发送")
                })
        })
        .await;
        assert_eq!(
            fixture
                .shared
                .patches
                .lock()
                .unwrap()
                .iter()
                .filter(|body| { card_text(body) == "reply to finish" })
                .count(),
            1
        );
        assert_eq!(fixture.shared.replies.lock().unwrap().len(), 1);
        let facts = gateway.delivery_facts().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].status, DeliveryStatus::Confirmed);
        gateway.shutdown().await.unwrap();
        let gateway = Gateway::start(progress_config(&fixture, root.path()))
            .await
            .unwrap();
        poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
        fixture.event("om_update_rejected", "finish").await;
        assert_eq!(fixture.ack().await["code"], 200);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(fixture.shared.replies.lock().unwrap().len(), 1);
        assert!(fixture.shared.recalls.lock().unwrap().is_empty());
        gateway.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn uncertain_final_updates_are_not_overwritten_or_replayed_after_restart() {
    for mode in [2, 4, 5, 6, 7] {
        let root = tempfile::tempdir().unwrap();
        let mut fixture = Fixture::new().await;
        fixture.shared.blocked.store(true, Ordering::SeqCst);
        fixture.shared.final_mode.store(mode, Ordering::SeqCst);
        let gateway = Gateway::start(progress_config(&fixture, root.path()))
            .await
            .unwrap();
        poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
        fixture.event("om_final_unknown", "do not duplicate").await;
        assert_eq!(fixture.ack().await["code"], 200);
        poll(|| fixture.shared.activity.lock().unwrap().len() == 1).await;
        fixture.shared.blocked.store(false, Ordering::SeqCst);
        fixture.shared.release.notify_one();
        fixture.wait_settlements(1).await;
        assert_eq!(
            fixture.shared.settlements.lock().unwrap()[0]["settlement"]["status"],
            "unknown"
        );
        assert_eq!(
            gateway.delivery_facts().unwrap()[0].status,
            DeliveryStatus::Unknown
        );
        assert!(fixture.shared.replies.lock().unwrap().is_empty());
        assert_eq!(
            card_text(fixture.shared.patches.lock().unwrap().last().unwrap()),
            "reply to do not duplicate"
        );
        let patches = fixture.shared.patches.lock().unwrap().len();
        // The platform may finish a timed-out PATCH late. No status write or
        // alternate final send may follow it, even across shutdown and restart.
        fixture.shared.final_release.notify_one();
        gateway.shutdown().await.unwrap();
        let gateway = Gateway::start(progress_config(&fixture, root.path()))
            .await
            .unwrap();
        poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
        fixture.event("om_final_unknown", "do not duplicate").await;
        assert_eq!(fixture.ack().await["code"], 200);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(fixture.shared.patches.lock().unwrap().len(), patches);
        assert_eq!(fixture.shared.activity.lock().unwrap().len(), 1);
        assert_eq!(fixture.shared.events.lock().unwrap().len(), 1);
        assert!(fixture.shared.replies.lock().unwrap().is_empty());
        assert!(fixture.shared.recalls.lock().unwrap().is_empty());
        gateway.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn shutdown_during_final_update_never_writes_a_status_over_the_answer() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    fixture.shared.blocked.store(true, Ordering::SeqCst);
    fixture.shared.final_blocked.store(true, Ordering::SeqCst);
    let gateway = Gateway::start(progress_config(&fixture, root.path()))
        .await
        .unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture
        .event("om_final_shutdown", "keep final content")
        .await;
    assert_eq!(fixture.ack().await["code"], 200);
    poll(|| fixture.shared.activity.lock().unwrap().len() == 1).await;
    fixture.shared.blocked.store(false, Ordering::SeqCst);
    fixture.shared.release.notify_one();
    tokio::time::timeout(
        Duration::from_secs(2),
        fixture.shared.final_entered.notified(),
    )
    .await
    .unwrap();
    let patches = fixture.shared.patches.lock().unwrap().len();
    gateway.shutdown().await.unwrap();
    fixture.shared.final_release.notify_one();
    let gateway = Gateway::start(progress_config(&fixture, root.path()))
        .await
        .unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.wait_settlements(1).await;
    assert_eq!(
        gateway.delivery_facts().unwrap()[0].status,
        DeliveryStatus::Unknown
    );
    assert_eq!(fixture.shared.patches.lock().unwrap().len(), patches);
    assert_eq!(
        card_text(fixture.shared.patches.lock().unwrap().last().unwrap()),
        "reply to keep final content"
    );
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    assert!(fixture.shared.recalls.lock().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn settlement_supersession_and_shutdown_close_activity_instead_of_leaving_it_working() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    fixture.shared.blocked.store(true, Ordering::SeqCst);
    let gateway = Gateway::start(progress_config(&fixture, root.path()))
        .await
        .unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.event("om_settled", "first").await;
    assert_eq!(fixture.ack().await["code"], 200);
    poll(|| fixture.shared.activity.lock().unwrap().len() == 1).await;
    fixture
        .progress_line("om_settled", json!({"settled":true,"status":"completed"}))
        .await;
    poll(|| {
        fixture.shared.patches.lock().unwrap().iter().any(|body| {
            body["message_id"] == "om_activity_om_settled"
                && card_text(body).contains("本轮执行已结束，正在发送回复")
        })
    })
    .await;
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    fixture.event("om_older", "older").await;
    assert_eq!(fixture.ack().await["code"], 200);
    poll(|| fixture.shared.activity.lock().unwrap().len() == 2).await;
    fixture.event("om_newer", "newer").await;
    assert_eq!(fixture.ack().await["code"], 200);
    poll(|| fixture.shared.activity.lock().unwrap().len() == 3).await;
    poll(|| {
        fixture.shared.patches.lock().unwrap().iter().any(|body| {
            body["message_id"] == "om_activity_om_older" && card_text(body).contains("后续消息")
        })
    })
    .await;
    poll(|| fixture.shared.progress_closed.load(Ordering::SeqCst) >= 2).await;
    gateway.shutdown().await.unwrap();
    assert!(
        fixture
            .shared
            .patches
            .lock()
            .unwrap()
            .iter()
            .any(|body| body["message_id"] == "om_activity_om_newer"
                && card_text(body).contains("连接正在关闭"))
    );
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    assert!(fixture.shared.progress_closed.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn failed_or_refused_progress_does_not_block_rich_final_and_stops_the_status_card() {
    for mode in [1, 2, 3, 4, 5] {
        let root = tempfile::tempdir().unwrap();
        let mut fixture = Fixture::new().await;
        fixture.shared.blocked.store(true, Ordering::SeqCst);
        fixture.shared.activity_mode.store(mode, Ordering::SeqCst);
        if mode == 5 {
            fixture
                .shared
                .progress_unavailable
                .store(true, Ordering::SeqCst);
        }
        let gateway = Gateway::start(progress_config(&fixture, root.path()))
            .await
            .unwrap();
        poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
        fixture.event("om_fallback", "Markdown **reply**").await;
        assert_eq!(fixture.ack().await["code"], 200);
        if mode == 5 {
            poll(|| fixture.shared.progress_opened.load(Ordering::SeqCst) == 1).await;
        } else {
            poll(|| fixture.shared.activity.lock().unwrap().len() == 1).await;
        }
        if mode == 2 || mode == 4 {
            fixture
                .progress_line(
                    "om_fallback",
                    json!({"category":"plugin","step":1,"content":"正在查询业务系统"}),
                )
                .await;
            poll(|| !fixture.shared.patches.lock().unwrap().is_empty()).await;
        }
        fixture.shared.blocked.store(false, Ordering::SeqCst);
        fixture.shared.release.notify_one();
        // Finalization joins at most the bounded status request, then falls
        // back instead of reusing a card with a potentially late status write.
        tokio::time::timeout(Duration::from_secs(3), fixture.wait_settlements(1))
            .await
            .unwrap();
        assert_eq!(fixture.shared.replies.lock().unwrap().len(), 1);
        assert_eq!(
            card_text(&fixture.shared.replies.lock().unwrap()[0]),
            "reply to Markdown **reply**"
        );
        fixture.shared.activity_mode.store(0, Ordering::SeqCst);
        fixture.shared.activity_release.notify_waiters();
        gateway.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn callback_failure_ends_activity_and_worker_retries_do_not_create_another_card() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new().await;
    fixture.shared.blocked.store(true, Ordering::SeqCst);
    let gateway = Gateway::start(progress_config(&fixture, root.path()))
        .await
        .unwrap();
    poll(|| gateway.status() == ConnectionStatus::Authenticated).await;
    fixture.event("om_failure", "fail").await;
    assert_eq!(fixture.ack().await["code"], 200);
    poll(|| fixture.shared.activity.lock().unwrap().len() == 1).await;
    fixture.shared.callback_status.store(503, Ordering::SeqCst);
    fixture.shared.blocked.store(false, Ordering::SeqCst);
    fixture.shared.release.notify_one();
    poll(|| {
        fixture
            .shared
            .patches
            .lock()
            .unwrap()
            .last()
            .is_some_and(|body| card_text(body).contains("回复尚未确认送达"))
    })
    .await;
    poll(|| fixture.shared.events.lock().unwrap().len() >= 2).await;
    assert_eq!(fixture.shared.activity.lock().unwrap().len(), 1);
    assert_eq!(fixture.shared.progress_opened.load(Ordering::SeqCst), 1);
    assert!(fixture.shared.replies.lock().unwrap().is_empty());
    gateway.shutdown().await.unwrap();
}
