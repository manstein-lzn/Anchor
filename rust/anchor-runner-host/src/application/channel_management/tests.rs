use super::*;
use anchor_platform_session::{ChannelIdentity, ChannelInboundRequest, SessionStore};
use serde_json::json;
use std::sync::Arc;

use fake::FakeChannels;

#[test]
fn empty_groups_preserve_legacy_binding_and_members_bytes() {
    let binding =
        r#"{"plugin":"feishu","users":["ou_alice"],"send_users":["ou_alice"],"private_only":true}"#;
    let decoded: ChannelBinding = serde_json::from_str(binding).unwrap();
    assert!(decoded.groups.is_empty());
    assert_eq!(serde_json::to_string(&decoded).unwrap(), binding);
    let members = r#"{"users":["ou_alice"],"send_users":["ou_alice"],"private_only":true}"#;
    let decoded: ChannelMembers = serde_json::from_str(members).unwrap();
    assert!(decoded.groups.is_empty());
    assert_eq!(serde_json::to_string(&decoded).unwrap(), members);
}

#[test]
fn feishu_groups_are_explicit_bounded_normalized_and_do_not_authorize_members() {
    let mut binding = runtime().binding;
    binding.plugin = "feishu".into();
    binding
        .validate_for_platform(ChannelPlatform::Feishu)
        .unwrap();
    binding.groups = vec![" oc_beta ".into(), "oc_alpha".into(), "oc_beta".into()];
    binding
        .validate_for_platform(ChannelPlatform::Feishu)
        .unwrap();
    let binding = binding.normalized();
    assert_eq!(binding.groups, ["oc_alpha", "oc_beta"]);
    assert_eq!(binding.users, ["member"]);
    assert_eq!(binding.send_users, ["member"]);
    assert!(!binding.allows_inbound("oc_alpha"));
    assert!(!binding.allows_outbound("oc_alpha"));
    assert!(
        binding
            .validate_for_platform(ChannelPlatform::Wecom)
            .is_err()
    );
    let mut private = binding.clone();
    private.private_only = true;
    assert!(
        private
            .validate_for_platform(ChannelPlatform::Feishu)
            .is_err()
    );
    private.groups.clear();
    private
        .validate_for_platform(ChannelPlatform::Feishu)
        .unwrap();

    let mut binding = binding;
    for invalid in [
        "", "*", "oc_", "ou_alice", "oc_*", "oc_a/b", "oc_a,b", "oc_a b", "oc_a\nb", "oc_群",
    ] {
        binding.groups = vec![invalid.into()];
        assert!(binding.validate().is_err(), "accepted {invalid:?}");
    }
    binding.groups = vec![format!("oc_{}", "a".repeat(197))];
    binding.validate().unwrap();
    binding.groups[0].push('a');
    assert!(binding.validate().is_err());
    binding.groups = (0..1000).map(|n| format!("oc_{n}")).collect();
    binding.validate().unwrap();
    binding.groups.push("oc_extra".into());
    assert!(binding.validate().is_err());
}

#[test]
fn channel_membership_keeps_explicit_wildcard_and_broadcast_rules_identical_for_both_directions() {
    let mut binding = runtime().binding;
    assert!(binding.allows_inbound("member"));
    assert!(binding.allows_outbound("member"));
    assert!(!binding.allows_inbound("another-member"));
    assert!(!binding.allows_outbound("another-member"));
    binding.users = vec!["*".into()];
    binding.send_users = vec!["*".into()];
    assert!(binding.allows_inbound("another-member"));
    assert!(binding.allows_outbound("another-member"));
    assert!(!binding.allows_inbound("@all"));
    assert!(!binding.allows_outbound("@all"));
    binding.users.clear();
    binding.send_users.clear();
    assert!(!binding.allows_inbound("member"));
    assert!(!binding.allows_outbound("member"));
}

fn runtime() -> ChannelRuntimeState {
    let mut state = FakeChannels::bot("graph", "owner", "assistant", &["member"]);
    state.account = Some("canonical".into());
    state
}

fn request(account: &str) -> ChannelInboundRequest {
    ChannelInboundRequest {
        inbound_id: "event".into(),
        identity: ChannelIdentity {
            source: "wecom".into(),
            account: Some(account.into()),
            conversation_kind: Default::default(),
            conversation_id: "conversation".into(),
            sender_id: "member".into(),
        },
        graph: "graph".into(),
        reply_node: "assistant".into(),
        text: Some("input".into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
        bot_origin: None,
        platform_message_id: None,
    }
}

#[test]
fn recovery_and_control_share_deployment_eligibility_without_rewriting_history() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("platform")).unwrap();
    let sessions = SessionStore::open(root.path().join("platform/sessions.sqlite")).unwrap();
    let session = sessions
        .admit_channel_inbound("owner", request("legacy"))
        .unwrap()
        .session;
    sessions
        .register_channel_account("owner", "wecom", "canonical", &["legacy".into()])
        .unwrap();
    let port = Arc::new(FakeChannels::new().with(runtime()));
    let app = RunApplication::new(root.path().into(), root.path().into())
        .with_channel_management(port.clone());
    assert!(
        app.ensure_channel_session_eligible("owner", &session.id)
            .is_ok()
    );
    for change in 0..5 {
        let mut current = runtime();
        match change {
            0 => current.enabled = false,
            1 => current.owner = "other-owner".into(),
            2 => current.account = Some("other-bot".into()),
            3 => current.reply_node = None,
            _ => current.reply_node = Some("other-node".into()),
        }
        port.put(current);
        assert!(matches!(
            app.ensure_channel_session_eligible("owner", &session.id),
            Err(ApplicationError::Conflict(_))
        ));
    }
    // Another Graph's bot does not serve this Session.
    port.clear("graph");
    let mut other = runtime();
    other.graph = "other-graph".into();
    port.put(other);
    assert!(matches!(
        app.ensure_channel_session_eligible("owner", &session.id),
        Err(ApplicationError::Conflict(_))
    ));
    assert_eq!(session, sessions.get("owner", &session.id).unwrap());
}

#[test]
fn duplicate_account_sessions_have_one_recovery_target_but_keep_both_histories() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("platform")).unwrap();
    let sessions = SessionStore::open(root.path().join("platform/sessions.sqlite")).unwrap();
    let old = sessions
        .admit_channel_inbound("owner", request("legacy"))
        .unwrap()
        .session;
    let new = sessions
        .admit_channel_inbound("owner", request("canonical"))
        .unwrap()
        .session;
    sessions
        .register_channel_account("owner", "wecom", "canonical", &["legacy".into()])
        .unwrap();
    let app = RunApplication::new(root.path().into(), root.path().into())
        .with_channel_management(Arc::new(FakeChannels::new().with(runtime())));
    assert!(matches!(
        app.ensure_channel_session_eligible("owner", &old.id),
        Err(ApplicationError::Conflict(_))
    ));
    assert!(
        app.ensure_channel_session_eligible("owner", &new.id)
            .is_ok()
    );
    assert_eq!(sessions.list("owner").unwrap().len(), 2);
}

#[tokio::test]
async fn assistant_projection_releases_liveness_lock_and_skips_retired_deployment_checks() {
    use crate::application::run_access::fixtures;
    use crate::run_metadata::{AssistantSource, RunMetadata};
    use anchor_platform_session::TurnStatus;
    use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("platform")).unwrap();
    let sessions = SessionStore::open(root.path().join("platform/sessions.sqlite")).unwrap();
    let admitted = sessions
        .admit_channel_inbound("owner", request("canonical"))
        .unwrap();
    let session = admitted.session.id;
    sessions
        .finish_turn(
            "owner",
            &session,
            &admitted.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    sessions
        .bind_channel_assistant("owner", &session, "old", "wait", "assistant", "reply")
        .unwrap();
    sessions
        .handover_channel_assistant("owner", &session, "old", "current", "wait-key")
        .unwrap();
    let application = RunApplication::new(root.path().into(), root.path().into());
    let inspection = application.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let snapshots = calls.clone();
    let port = Arc::new(
        FakeChannels::inspecting(move || {
            assert!(
                inspection
                    .kernel_owner("current")
                    .unwrap()
                    .unwrap()
                    .registry_is_unlocked(),
                "deployment lookup must not hold the global execution registry lock"
            );
            snapshots.fetch_add(1, Ordering::SeqCst);
        })
        .with(runtime()),
    );
    let application = application.with_channel_management(port.clone());
    let snapshot = GraphSnapshot::from_authoring(json!({
        "entry":"work", "ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work"}], "edges":[]
    }))
    .unwrap();
    for run in ["old", "current"] {
        let record = GraphRunRecord::create_with_id(snapshot.clone(), Value::Null, run).unwrap();
        fixtures::create(&application, &record);
        fixtures::control(&application, run, "stop", "stopped");
        let mut metadata = RunMetadata::new(
            run.into(),
            "graph".into(),
            record.graph_digest.clone(),
            root.path(),
        )
        .unwrap();
        metadata.assistant = Some(AssistantSource {
            owner: "owner".into(),
            session: session.clone(),
            wait_node: "wait".into(),
            work_node: "assistant".into(),
            reply_node: "reply".into(),
        });
        let projection = application
            .project_run(&fixtures::view(&application, run), Some(&metadata))
            .await
            .unwrap();
        assert_eq!(projection.actions.resume, run == "current");
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(run == "current"));
        if run == "current" {
            port.update("graph", |state| state.enabled = false);
            let blocked = application
                .project_run(&fixtures::view(&application, run), Some(&metadata))
                .await
                .unwrap();
            assert!(!blocked.actions.resume);
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
    }
}
