use super::tests::{command, fixture, write};
use super::*;
use crate::{
    application::{
        channel_management::fake::{FakeChannels, install_channel_plugin},
        run_access::fixtures,
        scheduling::ScheduleAssignmentTarget,
    },
    run_metadata::{self as metadata, AssistantSource, RunMetadata},
};
use anchor_platform_session::{
    ChannelConversationKind, ChannelIdentity, ChannelInboundRequest, CreateSession,
};
use anchor_runtime::graph::GraphRunRecord;
use std::sync::Arc;

fn assistant() -> Value {
    json!({"entry":"wait","agents":{"worker":{"model":"fixture"}},
        "ops":{"wait":{"host":{"operation":"session.wait_input"}},"reply":{"host":{"operation":"session.reply"}}},
        "nodes":[{"id":"wait","op":"wait"},{"id":"work","agent":"worker","plugins":["wecom",{"id":"graph-tasks","required":false,"default":true,"graph_tasks":{"graphs":["producer"]}}]},{"id":"reply","op":"reply"}],
        "edges":[{"from":"wait","to":"work"},{"from":"work","to":"reply"},{"from":"reply","to":"wait"}]})
}

fn inbound(sessions: &SessionStore, conversation: &str, kind: ChannelConversationKind) -> String {
    let admission = sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: format!("input-{conversation}"),
                identity: ChannelIdentity {
                    source: "wecom".into(),
                    account: Some("corp-a".into()),
                    conversation_kind: kind,
                    conversation_id: conversation.into(),
                    sender_id: "recipient".into(),
                },
                graph: "assistant".into(),
                reply_node: "work".into(),
                text: Some("existing conversation".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: false,
            },
        )
        .unwrap();
    sessions
        .finish_turn(
            "local",
            &admission.session.id,
            &admission.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    admission.session.id
}

pub(crate) async fn prepared() -> (
    tempfile::TempDir,
    RunApplication,
    Arc<FakeChannels>,
    SessionStore,
    String,
    ScheduleAssignmentTarget,
) {
    let (root, application) = fixture();
    install_channel_plugin(application.library_root(), "wecom");
    write(&application, "producer", &command(false));
    write(&application, "assistant", &assistant());
    application
        .update_graph("assistant", assistant())
        .await
        .unwrap();
    let channels = Arc::new(FakeChannels::new().with(FakeChannels::bot(
        "assistant",
        "local",
        "work",
        &["recipient"],
    )));
    let application = application.with_channel_management(channels.clone());
    crate::host_fs::directory(&application.data_root.join("platform")).unwrap();
    let sessions =
        SessionStore::open(application.data_root.join("platform/sessions.sqlite")).unwrap();
    let session = inbound(
        &sessions,
        "private-conversation",
        ChannelConversationKind::Single,
    );
    let (_, bundle) = application.load_graph("assistant").unwrap();
    let record =
        GraphRunRecord::create_with_id(bundle.snapshot, json!({}), "existing-assistant").unwrap();
    fixtures::create(&application, &record);
    fixtures::control(&application, &record.run_id, "stop", "test-stopped");
    let mut metadata = RunMetadata::new(
        record.run_id.clone(),
        "assistant".into(),
        record.graph_digest,
        &application.graph_bundle_path("assistant"),
    )
    .unwrap();
    metadata.assistant = Some(AssistantSource {
        owner: "local".into(),
        session: session.clone(),
        wait_node: "wait".into(),
        work_node: "work".into(),
        reply_node: "reply".into(),
    });
    metadata::save(&application.data_root, &metadata).unwrap();
    sessions
        .bind_channel_assistant("local", &session, &record.run_id, "wait", "work", "reply")
        .unwrap();
    let target = ScheduleAssignmentTarget {
        graph: "producer".into(),
        input: json!({"bot_graph":"assistant","userid":"recipient","send_not_before":"09:05"}),
        delegation: None,
    };
    (root, application, channels, sessions, session, target)
}

#[tokio::test]
async fn operator_assignment_uses_the_approved_source_without_a_turn_or_running_assistant() {
    let (_root, application, _channels, sessions, session, target) = prepared().await;
    let before =
        serde_json::to_value(fixtures::record(&application, "existing-assistant")).unwrap();
    let before_session = sessions.get("local", &session).unwrap();
    let origin = application
        .operator_assignment_origin(
            "local",
            &session,
            "plan",
            "Deliver the published PDF here",
            &target,
            &json!({"send_not_before":"09:05"}),
        )
        .unwrap();
    assert_eq!(origin.owner, "local");
    assert_eq!(origin.session.as_deref(), Some(session.as_str()));
    assert_eq!(origin.graph, "assistant");
    assert_eq!(origin.node_id(), "work");
    assert!(origin.agent_key().is_none());
    assert!(
        matches!(&origin.source, TaskOriginSource::OperatorAssignment { schedule, instruction, .. } if schedule == "plan" && instruction == "Deliver the published PDF here")
    );
    let (_, producer) = application.load_graph("producer").unwrap();
    assert_eq!(
        origin.targets["producer"],
        producer.snapshot.digest().unwrap()
    );
    assert_eq!(
        serde_json::to_value(fixtures::record(&application, "existing-assistant")).unwrap(),
        before
    );
    assert_eq!(sessions.get("local", &session).unwrap(), before_session);
    assert!(application.active_runs(None).await.is_empty());
    assert_eq!(application.records().unwrap().len(), 1);
}

#[tokio::test]
async fn assignment_requires_real_private_same_recipient_sessions_and_current_channel_authority() {
    let (_root, application, channels, sessions, session, target) = prepared().await;
    let check = |owner: &str, session: &str, target: &ScheduleAssignmentTarget, input: &Value| {
        application.operator_assignment_origin(
            owner,
            session,
            "plan",
            "Deliver here",
            target,
            input,
        )
    };
    assert!(check("another-owner", &session, &target, &json!({})).is_err());
    let generic = sessions.create("local", CreateSession::default()).unwrap();
    assert!(check("local", &generic.id, &target, &json!({})).is_err());
    let group = inbound(
        &sessions,
        "group-conversation",
        ChannelConversationKind::Group,
    );
    assert!(check("local", &group, &target, &json!({})).is_err());
    for (field, value) in [("bot_graph", "other-bot"), ("userid", "other-recipient")] {
        let mut changed = target.clone();
        changed.input[field] = json!(value);
        assert!(check("local", &session, &changed, &json!({})).is_err());
        assert!(check("local", &session, &target, &json!({field:value})).is_err());
    }
    let original = channels.get("assistant").unwrap();
    for field in ["owner", "account", "enabled", "users", "send_users"] {
        let mut changed = original.clone();
        match field {
            "owner" => changed.owner = "another-owner".into(),
            "account" => changed.account = Some("other-account".into()),
            "enabled" => changed.enabled = false,
            "users" => changed.binding.users.clear(),
            "send_users" => changed.binding.send_users.clear(),
            _ => unreachable!(),
        }
        channels.put(changed);
        assert!(
            check("local", &session, &target, &json!({})).is_err(),
            "{field}"
        );
    }
    channels.put(original);
    sessions
        .set_plugin_mount("local", &session, "graph-tasks", false, "operator", "test")
        .unwrap();
    assert!(check("local", &session, &target, &json!({})).is_err());
    sessions
        .set_plugin_mount("local", &session, "graph-tasks", true, "operator", "test")
        .unwrap();
    assert!(check("local", &session, &target, &json!({})).is_ok());
    channels.update("assistant", |bot| {
        bot.binding.users = vec!["*".into()];
        bot.binding.send_users = vec!["*".into()];
    });
    assert!(check("local", &session, &target, &json!({})).is_ok());
}

#[tokio::test]
async fn assignment_does_not_approve_a_changed_target_or_an_unsigned_source_revision() {
    let (_root, application, _channels, _sessions, session, target) = prepared().await;
    let mut changed = command(false);
    changed["ops"]["work"]["run"] = json!("echo changed");
    write(&application, "producer", &changed);
    assert!(
        application
            .operator_assignment_origin(
                "local",
                &session,
                "plan",
                "Deliver here",
                &target,
                &json!({})
            )
            .is_err()
    );
    write(&application, "producer", &command(false));
    let mut changed = assistant();
    changed["objective"] = json!("not approved");
    write(&application, "assistant", &changed);
    assert!(
        application
            .operator_assignment_origin(
                "local",
                &session,
                "plan",
                "Deliver here",
                &target,
                &json!({})
            )
            .is_err()
    );
}
