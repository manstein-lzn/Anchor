use super::*;
use crate::application::run_access::fixtures;
use anchor_platform_session::{ChannelIdentity, ChannelInboundRequest, TurnStatus};
use anchor_runtime::graph::{GraphRunRecord, GraphTaskPolicy, InvocationKey};
use std::{future::Future, task::Poll};

fn session(sessions: &SessionStore, owner: &str, conversation: &str) -> String {
    let admission = sessions
        .admit_channel_inbound(
            owner,
            ChannelInboundRequest {
                inbound_id: "initial".into(),
                identity: ChannelIdentity {
                    source: "wecom".into(),
                    account: Some("bot".into()),
                    conversation_kind: Default::default(),
                    conversation_id: conversation.into(),
                    sender_id: conversation.into(),
                },
                graph: "assistant".into(),
                reply_node: "agent".into(),
                text: Some("schedule a task".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: false,
            },
        )
        .unwrap();
    sessions
        .finish_turn(
            owner,
            &admission.session.id,
            &admission.turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    admission.session.id
}

fn outgoing(application: &RunApplication, session: &str) -> GraphRunRecord {
    let record = fixtures::work_record("outgoing");
    let path = application.graph_bundle_path("target");
    crate::host_fs::directory(&path).unwrap();
    let mut metadata = RunMetadata::new(
        record.run_id.clone(),
        "target".into(),
        record.graph_digest.clone(),
        &path,
    )
    .unwrap();
    metadata.task = Some(TaskOrigin {
        owner: "owner".into(),
        session: Some(session.into()),
        graph: "source".into(),
        source: crate::task_delegation::TaskOriginSource::AgentDelegation {
            key: InvocationKey {
                run_id: "retired-source-run".into(),
                graph_digest: "a".repeat(64),
                node_id: "agent".into(),
                invocation: 1,
            },
        },
        authorization_digest: "a".repeat(64),
        policy: GraphTaskPolicy::default(),
        bindings: Default::default(),
        targets: Default::default(),
    });
    metadata::save(&application.data_root, &metadata).unwrap();
    record
}

async fn assert_outgoing_blocks(
    application: &RunApplication,
    sessions: &SessionStore,
    session: &str,
) {
    let error = application
        .delete_task_session("owner", session)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ApplicationError::Conflict(reason) if reason.contains("delegated Graph task")),
        "{error:?}"
    );
    assert!(sessions.get("owner", session).is_ok());
}

#[tokio::test]
async fn session_deletion_preserves_outgoing_lifecycle_and_releases_handled_history() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    crate::host_fs::directory(&application.data_root.join("platform")).unwrap();
    let sessions =
        SessionStore::open(application.data_root.join("platform/sessions.sqlite")).unwrap();
    let recipient = session(&sessions, "owner", "alice");
    let another_session = session(&sessions, "owner", "bob");
    let another_owner = session(&sessions, "other", "alice");
    let record = outgoing(&application, &recipient);

    assert_outgoing_blocks(&application, &sessions, &recipient).await;
    application
        .delete_task_session("owner", &another_session)
        .await
        .unwrap();
    application
        .delete_task_session("other", &another_owner)
        .await
        .unwrap();
    fixtures::create(&application, &record);
    assert_outgoing_blocks(&application, &sessions, &recipient).await;
    fixtures::fail(&application, &record.run_id, "worker", "fixture complete");
    assert_outgoing_blocks(&application, &sessions, &recipient).await;
    let workspace = root.path().join("workspaces");
    assert_eq!(
        application
            .reconcile_task_results(&workspace)
            .await
            .unwrap(),
        1
    );
    assert_outgoing_blocks(&application, &sessions, &recipient).await;
    // Session tests cover claim/ACK. Seed its handled receipt without adding
    // an unrelated assistant Run to this deletion-boundary fixture.
    sessions
        .append_event(
            "owner",
            &recipient,
            "task.result.handled",
            std::collections::BTreeMap::from([("id".into(), json!(result_id(&record.run_id)))]),
        )
        .unwrap();

    application
        .delete_task_session("owner", &recipient)
        .await
        .unwrap();
    assert_eq!(
        sessions.get("owner", &recipient),
        Err(SessionError::Missing)
    );
    assert_eq!(
        application
            .reconcile_task_results(&workspace)
            .await
            .unwrap(),
        0
    );
    application
        .delete_run(&record.run_id, &workspace)
        .await
        .unwrap();
    assert!(application.read_run(&record.run_id).unwrap().is_none());
}

#[tokio::test]
async fn session_deletion_rechecks_references_after_waiting_for_admission() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    crate::host_fs::directory(&application.data_root.join("platform")).unwrap();
    let sessions =
        SessionStore::open(application.data_root.join("platform/sessions.sqlite")).unwrap();
    let recipient = session(&sessions, "owner", "alice");
    let admission = application.admission.lock().await;
    let mut deletion = std::pin::pin!(application.delete_task_session("owner", &recipient));
    std::future::poll_fn(|cx| {
        assert!(deletion.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    outgoing(&application, &recipient);
    drop(admission);
    assert!(matches!(deletion.await, Err(ApplicationError::Conflict(_))));
    assert!(sessions.get("owner", &recipient).is_ok());
}
