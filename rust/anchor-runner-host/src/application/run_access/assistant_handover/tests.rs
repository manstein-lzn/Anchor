use super::*;
use crate::application::run_access::fixtures;
use crate::run_metadata::{self, ConversationSource, RunMetadata};
use anchor_kernel::{Evidence, Principal, Submission};
use anchor_platform_session::{ChannelIdentity, ChannelInboundRequest};
use anchor_runtime::graph::scheduler::HOST_ID;

#[test]
fn historical_stop_fact_fences_only_an_incomplete_successor() {
    let root = tempfile::tempdir().unwrap();
    let app = RunApplication::new(root.path().into(), root.path().join("catalog"));
    std::fs::create_dir_all(root.path().join("platform")).unwrap();
    let store = sessions(&app).unwrap();
    let owner = "local";
    let session = store
        .admit_channel_inbound(
            owner,
            ChannelInboundRequest {
                inbound_id: "historical-input".into(),
                identity: ChannelIdentity {
                    source: "wecom".into(),
                    account: None,
                    conversation_kind: Default::default(),
                    conversation_id: "historical-conversation".into(),
                    sender_id: "fixture-user".into(),
                },
                graph: "fixture".into(),
                reply_node: "worker".into(),
                text: Some("hello".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: false,
                bot_origin: None,
                platform_message_id: None,
            },
        )
        .unwrap()
        .session
        .id;
    let record = fixtures::work_record("historical-source");
    fixtures::create(&app, &record);
    let source = store
        .bind_channel_assistant(
            owner,
            &session,
            &record.run_id,
            "worker",
            "worker",
            "worker",
        )
        .unwrap();
    let mut metadata = RunMetadata::new(
        source.run_id.clone(),
        "fixture".into(),
        record.graph_digest.clone(),
        root.path(),
    )
    .unwrap();
    metadata.conversation = Some(ConversationSource {
        session: session.clone(),
        reply_node: source.reply_node.clone(),
        previous_run: None,
    });
    run_metadata::save(root.path(), &metadata).unwrap();
    let authority = json!({"fixture":"historical"});
    let successor = successor(&authority, "fixture", owner, &source).unwrap();
    let assert_state = |source_pending, successor_pending, reply: Option<&str>| {
        for (run, pending) in [
            (&source.run_id, source_pending),
            (&successor, successor_pending),
        ] {
            assert_eq!(
                app.assistant_has_pending_handover(run).unwrap(),
                pending,
                "{run}"
            );
        }
        assert_eq!(
            app.assistant_handover_reply_run(owner, &session, &source.run_id)
                .unwrap()
                .as_deref(),
            reply
        );
    };
    let submit_stop = |fact: Value| {
        let runs = app.kernel_owner(&source.run_id).unwrap().unwrap();
        let resource = runs.load(&source.run_id).unwrap().unwrap();
        let verdict = runs
            .kernel
            .submit(
                &Principal::new(HOST_ID, PrincipalKind::Code),
                0,
                Submission::new(&resource.id, resource.version, Action::Request)
                    .content(json!({
                        "control":"stop", "reason":"historical fixture",
                        "assistant_handover":fact
                    }))
                    .evidence(Evidence::Observation {
                        id: "fixture/historical-handover".into(),
                    }),
            )
            .unwrap();
        assert!(verdict.is_accepted(), "{verdict:?}");
    };

    assert_state(false, false, Some(&source.run_id));
    submit_stop(json!({
        "schema_version":1, "authority":authority, "graph":"fixture", "owner":owner,
        "source":source, "successor":successor,
        "wait":{
            "run_id":source.run_id, "graph_digest":record.graph_digest,
            "node_id":source.wait_node, "invocation":1
        }
    }));
    assert_state(true, false, None);

    // No target metadata: only the Session's certified predecessor links the fact.
    store
        .handover_channel_assistant(
            owner,
            &session,
            &source.run_id,
            &successor,
            "successor-wait",
        )
        .unwrap();
    assert!(app.metadata(&successor).unwrap().is_none());
    assert!(app.read_run(&successor).unwrap().is_none());
    assert_state(true, true, None);

    metadata.run_id = successor.clone();
    metadata.conversation.as_mut().unwrap().previous_run = Some(source.run_id.clone());
    run_metadata::save(root.path(), &metadata).unwrap();
    assert_state(true, true, None);
    fixtures::create(&app, &fixtures::work_record(&successor));
    assert_state(false, false, Some(&successor));

    // A later accepted but malformed fact must not silently release either fence.
    submit_stop(json!({"schema_version":1}));
    for run in [&source.run_id, &successor] {
        assert!(app.assistant_has_pending_handover(run).is_err(), "{run}");
    }
    assert!(
        app.assistant_handover_reply_run(owner, &session, &source.run_id)
            .is_err()
    );
}
