use std::sync::{Arc, Barrier};
use std::thread;

use anchor_platform_session::{
    AttachmentManifest, AttachmentManifestEntry, ChannelDeliveryRequest, ChannelDeliveryStatus,
    ChannelIdentity, ChannelInboundRequest, CreateSession, SessionError, SessionStore, TurnStatus,
};
use tempfile::TempDir;

fn fixture() -> (TempDir, SessionStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    (directory, store)
}

fn request(inbound_id: &str, identity: &str) -> ChannelInboundRequest {
    ChannelInboundRequest {
        inbound_id: inbound_id.into(),
        identity: ChannelIdentity {
            source: "wecom".into(),
            account: Some("corp-a".into()),
            conversation_id: identity.into(),
            sender_id: "user-1".into(),
        },
        graph: "support-graph".into(),
        reply_node: "reply".into(),
        text: Some("hello".into()),
        attachments: AttachmentManifest::default(),
        run_id: None,
        replace_running: false,
    }
}

fn attachment_request() -> ChannelInboundRequest {
    let mut request = request("message-1", "conversation-1");
    request.text = None;
    request.attachments.files.push(AttachmentManifestEntry {
        name: "report.txt".into(),
        path: "/workspace/inbox/report.txt".into(),
        sha256: "a".repeat(64),
        size: 12,
        media_type: Some("text/plain".into()),
    });
    request
}

fn delivery(key: &str) -> ChannelDeliveryRequest {
    ChannelDeliveryRequest {
        key: key.into(),
        kind: "text".into(),
        content_sha256: "b".repeat(64),
    }
}

fn assert_conflict<T>(result: Result<T, SessionError>) {
    assert!(matches!(result, Err(SessionError::Conflict(_))));
}

fn assert_invalid<T>(result: Result<T, SessionError>) {
    assert!(matches!(result, Err(SessionError::Invalid(_))));
}

#[test]
fn same_channel_identity_is_isolated_by_owner_and_duplicate_inbound_is_read_only() {
    let (directory, first) = fixture();
    let second = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    let start = Arc::new(Barrier::new(2));
    let workers = [first.clone(), second]
        .into_iter()
        .enumerate()
        .map(|(worker, store)| {
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                let owner = if worker == 0 { "owner-a" } else { "owner-b" };
                (
                    owner,
                    store.admit_channel_inbound(owner, request("message-1", "conversation-1")),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert!(results.iter().all(|(_, result)| result.is_ok()));
    let first_admission = results[0].1.as_ref().unwrap();
    let second_admission = results[1].1.as_ref().unwrap();
    assert_ne!(first_admission.session.id, second_admission.session.id);
    assert_ne!(first_admission.turn.id, second_admission.turn.id);
    assert_eq!(first.list("owner-a").unwrap().len(), 1);
    assert_eq!(first.list("owner-b").unwrap().len(), 1);

    let duplicate = first
        .admit_channel_inbound("owner-a", request("message-1", "conversation-1"))
        .unwrap();
    assert_eq!(duplicate.session.id, first_admission.session.id);
    assert_eq!(duplicate.turn, first_admission.turn);
    assert_eq!(duplicate.relation, first_admission.relation);
    assert_eq!(
        first
            .list_turns("owner-a", &duplicate.session.id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn run_binding_populates_channel_turn_and_session_relations() {
    let (_directory, store) = fixture();
    let mut request = request("message-1", "conversation-1");
    request.run_id = Some("channel-run-1".into());
    let admission = store.admit_channel_inbound("owner", request).unwrap();

    assert_eq!(admission.relation.run_id.as_deref(), Some("channel-run-1"));
    let turn = store
        .get_turn("owner", &admission.session.id, &admission.turn.id)
        .unwrap();
    assert_eq!(turn.runs, vec!["channel-run-1"]);
    assert_eq!(
        store.get("owner", &admission.session.id).unwrap().run_ids,
        vec!["channel-run-1"]
    );
    let inbound = store
        .get_channel_inbound("owner", &admission.session.id, "message-1")
        .unwrap();
    assert_eq!(inbound.relation.run_id.as_deref(), Some("channel-run-1"));
    assert_eq!(inbound.previous_run, None);
}

#[test]
fn turn_windows_report_only_the_turns_this_run_executed() {
    let (_directory, store) = fixture();
    let first = store
        .admit_channel_inbound("owner", request("message-1", "conversation-1"))
        .unwrap();
    store
        .bind_channel_assistant(
            "owner",
            &first.session.id,
            "resident-run-1",
            "wait_input",
            "reply",
            "reply",
        )
        .unwrap();
    store
        .claim_channel_assistant_input(
            "owner",
            &first.session.id,
            "resident-run-1",
            "resident-run-1:digest:wait_input:1",
        )
        .unwrap()
        .unwrap();
    store
        .finish_turn(
            "owner",
            &first.session.id,
            &first.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();

    // The same resident Run keeps taking the Session's next Turn.
    let second = store
        .admit_channel_inbound("owner", request("message-2", "conversation-1"))
        .unwrap();
    store
        .claim_channel_assistant_input(
            "owner",
            &first.session.id,
            "resident-run-1",
            "resident-run-1:digest:wait_input:2",
        )
        .unwrap()
        .unwrap();

    let mut other = request("message-3", "conversation-2");
    other.run_id = Some("other-run".into());
    let other = store.admit_channel_inbound("owner", other).unwrap();

    let windows = store.turn_windows_for_run("resident-run-1").unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].created_at, first.turn.created_at);
    assert!(!windows[0].running);
    assert!(windows[0].updated_at > first.turn.created_at);
    assert_eq!(windows[1].created_at, second.turn.created_at);
    assert!(windows[1].running);

    let other_windows = store.turn_windows_for_run("other-run").unwrap();
    assert_eq!(other_windows.len(), 1);
    assert_eq!(other_windows[0].created_at, other.turn.created_at);
    assert!(
        store
            .turn_windows_for_run("unknown-run")
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        store.turn_windows_for_run("../escape"),
        Err(SessionError::Invalid(_))
    ));
}

#[test]
fn inbound_lookup_resolves_the_run_inside_its_owner_scope() {
    let (_directory, store) = fixture();
    // Admission without a Run yet: the message exists but has nothing to watch.
    let pending = store
        .admit_channel_inbound("owner", request("message-1", "conversation-1"))
        .unwrap();
    let lookup = store
        .channel_run_for_inbound("owner", "message-1")
        .unwrap()
        .unwrap();
    assert_eq!(lookup.session_id, pending.session.id);
    assert_eq!(lookup.turn_id, pending.turn.id);
    assert_eq!(lookup.run_id, None);

    // Once the Run is bound the same lookup reports it. A channel Session runs
    // one Turn at a time, so the second message opens its own conversation.
    let mut bound = request("message-2", "conversation-2");
    bound.run_id = Some("channel-run-2".into());
    store.admit_channel_inbound("owner", bound).unwrap();
    let lookup = store
        .channel_run_for_inbound("owner", "message-2")
        .unwrap()
        .unwrap();
    assert_eq!(lookup.run_id.as_deref(), Some("channel-run-2"));

    // Another owner sees nothing, and the identity is validated.
    assert!(
        store
            .channel_run_for_inbound("other", "message-2")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .channel_run_for_inbound("owner", "missing")
            .unwrap()
            .is_none()
    );
    assert_invalid(store.channel_run_for_inbound("owner", ""));
    assert_invalid(store.channel_run_for_inbound("owner", "bad\nid"));
    assert_invalid(store.channel_run_for_inbound("", "message-2"));
}

#[test]
fn attachment_manifest_is_frozen_and_changes_are_rejected() {
    let (_directory, store) = fixture();
    let original = attachment_request();
    let admission = store
        .admit_channel_inbound("owner", original.clone())
        .unwrap();
    let duplicate = store.admit_channel_inbound("owner", original).unwrap();
    assert_eq!(duplicate.turn, admission.turn);

    let mut changed = attachment_request();
    changed.attachments.files[0].sha256 = "c".repeat(64);
    assert_conflict(store.admit_channel_inbound("owner", changed));
    let mut changed = attachment_request();
    changed.attachments.files[0].size = 13;
    assert_conflict(store.admit_channel_inbound("owner", changed));
}

#[test]
fn running_turns_are_serial_and_replacement_tracks_delivery_state() {
    let (_directory, store) = fixture();
    let first = store
        .admit_channel_inbound("owner", request("message-1", "conversation-1"))
        .unwrap();
    assert_conflict(store.admit_channel_inbound("owner", request("message-2", "conversation-1")));

    let old_delivery = store
        .admit_channel_delivery(
            "owner",
            &first.session.id,
            &first.turn.id,
            delivery("reply-1"),
        )
        .unwrap();
    assert_eq!(old_delivery.status, ChannelDeliveryStatus::Pending);
    assert_eq!(
        store
            .begin_channel_delivery(
                "owner",
                &first.session.id,
                &first.turn.id,
                delivery("reply-1"),
            )
            .unwrap()
            .status,
        ChannelDeliveryStatus::Sending
    );

    let mut replacement = request("message-2", "conversation-1");
    replacement.replace_running = true;
    let second = store.admit_channel_inbound("owner", replacement).unwrap();
    assert_eq!(
        store
            .get_turn("owner", &first.session.id, &first.turn.id)
            .unwrap()
            .status,
        TurnStatus::Interrupted
    );
    assert_eq!(
        store
            .get_channel_relation("owner", &first.session.id, "message-1")
            .unwrap()
            .superseded_by_turn_id,
        Some(second.turn.id.clone())
    );
    let unfinished = store
        .list_unfinished_channel_deliveries("owner", &first.session.id)
        .unwrap();
    assert_eq!(unfinished.len(), 1);
    assert_eq!(unfinished[0].status, ChannelDeliveryStatus::Unknown);
    assert_eq!(unfinished[0].superseded_by_turn_id, Some(second.turn.id));
}

#[test]
fn completed_reply_delivery_is_idempotent_and_requires_a_current_completed_turn() {
    let (_directory, store) = fixture();
    let first = store
        .admit_channel_inbound("owner", request("message-1", "conversation-1"))
        .unwrap();
    assert_conflict(store.admit_completed_channel_delivery(
        "owner",
        &first.session.id,
        &first.turn.id,
        delivery("reply-1"),
    ));
    store
        .finish_turn(
            "owner",
            &first.session.id,
            &first.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    let admitted = store
        .admit_completed_channel_delivery(
            "owner",
            &first.session.id,
            &first.turn.id,
            delivery("reply-1"),
        )
        .unwrap();
    assert_eq!(admitted.status, ChannelDeliveryStatus::Pending);
    let sending = store
        .begin_channel_delivery(
            "owner",
            &first.session.id,
            &first.turn.id,
            delivery("reply-1"),
        )
        .unwrap();
    assert_eq!(sending.status, ChannelDeliveryStatus::Sending);
    assert_eq!(
        store
            .admit_completed_channel_delivery(
                "owner",
                &first.session.id,
                &first.turn.id,
                delivery("reply-1"),
            )
            .unwrap(),
        sending
    );
}

#[test]
fn previous_run_lookup_skips_inbounds_without_a_run_binding() {
    let (_directory, store) = fixture();
    let mut first_request = request("message-1", "conversation-1");
    first_request.run_id = Some("channel-run-1".into());
    let first = store.admit_channel_inbound("owner", first_request).unwrap();
    let mut second_request = request("message-2", "conversation-1");
    second_request.replace_running = true;
    let second = store
        .admit_channel_inbound("owner", second_request)
        .unwrap();
    let mut third_request = request("message-3", "conversation-1");
    third_request.replace_running = true;
    let third = store.admit_channel_inbound("owner", third_request).unwrap();
    assert_eq!(
        store
            .get_channel_inbound("owner", &third.session.id, "message-3")
            .unwrap()
            .previous_run
            .as_deref(),
        Some("channel-run-1")
    );
    assert_eq!(first.session.id, second.session.id);
}

#[test]
fn sending_delivery_becomes_unknown_after_restart_and_blocks_delete_until_settled() {
    let (directory, store) = fixture();
    let admission = store
        .admit_channel_inbound("owner", request("message-1", "conversation-1"))
        .unwrap();
    let request = delivery("reply-1");
    store
        .admit_channel_delivery(
            "owner",
            &admission.session.id,
            &admission.turn.id,
            request.clone(),
        )
        .unwrap();
    store
        .begin_channel_delivery(
            "owner",
            &admission.session.id,
            &admission.turn.id,
            request.clone(),
        )
        .unwrap();
    assert_conflict(store.delete_channel_session("owner", &admission.session.id));

    drop(store);
    let reopened = SessionStore::open(directory.path().join("sessions.sqlite")).unwrap();
    assert_eq!(reopened.recover_channel_deliveries().unwrap(), 1);
    let unknown = reopened
        .list_unfinished_channel_deliveries("owner", &admission.session.id)
        .unwrap();
    assert_eq!(unknown[0].status, ChannelDeliveryStatus::Unknown);
    assert_eq!(
        reopened
            .settle_channel_delivery(
                "owner",
                &admission.session.id,
                request,
                ChannelDeliveryStatus::Confirmed,
                None,
            )
            .unwrap()
            .status,
        ChannelDeliveryStatus::Confirmed
    );
    reopened
        .finish_turn(
            "owner",
            &admission.session.id,
            &admission.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    reopened
        .delete_channel_session("owner", &admission.session.id)
        .unwrap();
    assert_eq!(reopened.list("owner").unwrap().len(), 0);
}

#[test]
fn gateway_suppression_is_receipt_scoped_and_preserves_generic_guard() {
    let (_directory, store) = fixture();
    let first = store
        .admit_channel_inbound("owner", request("message-1", "conversation-1"))
        .unwrap();
    let first_delivery = delivery("reply-1");
    store
        .admit_channel_delivery(
            "owner",
            &first.session.id,
            &first.turn.id,
            first_delivery.clone(),
        )
        .unwrap();
    store
        .begin_channel_delivery(
            "owner",
            &first.session.id,
            &first.turn.id,
            first_delivery.clone(),
        )
        .unwrap();
    store.recover_channel_deliveries().unwrap();

    assert_conflict(store.settle_channel_delivery(
        "owner",
        &first.session.id,
        first_delivery.clone(),
        ChannelDeliveryStatus::Suppressed,
        None,
    ));
    assert_eq!(
        store
            .settle_channel_delivery_from_gateway(
                "owner",
                &first.session.id,
                "message-1",
                first_delivery,
                ChannelDeliveryStatus::Suppressed,
            )
            .unwrap()
            .status,
        ChannelDeliveryStatus::Suppressed
    );

    let second = store
        .admit_channel_inbound("owner", request("message-2", "conversation-2"))
        .unwrap();
    let second_delivery = delivery("reply-2");
    store
        .admit_channel_delivery(
            "owner",
            &second.session.id,
            &second.turn.id,
            second_delivery.clone(),
        )
        .unwrap();
    store
        .begin_channel_delivery(
            "owner",
            &second.session.id,
            &second.turn.id,
            second_delivery.clone(),
        )
        .unwrap();
    let mut replacement = request("message-3", "conversation-2");
    replacement.replace_running = true;
    store.admit_channel_inbound("owner", replacement).unwrap();

    assert_conflict(store.settle_channel_delivery(
        "owner",
        &second.session.id,
        second_delivery.clone(),
        ChannelDeliveryStatus::Suppressed,
        None,
    ));
    let suppressed = store
        .settle_channel_delivery_from_gateway(
            "owner",
            &second.session.id,
            "message-2",
            second_delivery.clone(),
            ChannelDeliveryStatus::Suppressed,
        )
        .unwrap();
    assert_eq!(suppressed.status, ChannelDeliveryStatus::Suppressed);
    assert_eq!(
        store
            .settle_channel_delivery_from_gateway(
                "owner",
                &second.session.id,
                "message-2",
                second_delivery,
                ChannelDeliveryStatus::Suppressed,
            )
            .unwrap()
            .status,
        ChannelDeliveryStatus::Suppressed
    );
}

#[test]
fn malformed_channel_inputs_fail_closed_and_pilot_turns_stay_separate() {
    let (_directory, store) = fixture();
    let mut invalid = request("message-1", "conversation-1");
    invalid.identity.sender_id = "".into();
    assert_invalid(store.admit_channel_inbound("owner", invalid));

    let mut invalid = request("message-2", "conversation-1");
    invalid.text = None;
    assert_invalid(store.admit_channel_inbound("owner", invalid));

    let mut invalid = attachment_request();
    invalid.attachments.files[0].path = "/workspace/../escape.txt".into();
    assert_invalid(store.admit_channel_inbound("owner", invalid));

    let mut invalid = attachment_request();
    invalid.attachments.files[0].sha256 = "A".repeat(64);
    assert_invalid(store.admit_channel_inbound("owner", invalid));

    let pilot = store
        .create(
            "owner",
            CreateSession {
                id: Some("pilot".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        store
            .create_turn("owner", &pilot.id, "request-1", Some("hello"))
            .unwrap()
            .1
    );
    assert_eq!(store.list("owner").unwrap().len(), 1);
}
