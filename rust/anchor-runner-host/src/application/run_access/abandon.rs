use crate::application::{
    ApplicationError, RunApplication,
    abandon::{ABANDON_DIR, AbandonAction, AbandonIntent},
    storage,
};

#[derive(Debug, Clone)]
pub(crate) struct AbandonResult {
    pub(crate) action: AbandonAction,
    pub(crate) reason: Option<String>,
    pub(crate) requested_at: Option<String>,
}

impl RunApplication {
    pub(in crate::application) async fn wait_abandon_drain(
        &self,
        id: &str,
    ) -> Result<(), ApplicationError> {
        let runs = self.kernel_owner(id)?.ok_or(ApplicationError::Missing)?;
        runs.wait_idle(id).await;
        Ok(())
    }

    pub(in crate::application) fn abandon_wait_children(
        &self,
        id: &str,
    ) -> Result<Vec<String>, ApplicationError> {
        Ok(self
            .read_run(id)?
            .map(|view| super::wait_child_ids(&view.record))
            .unwrap_or_default())
    }

    pub(in crate::application) async fn finish_kernel_abandon(
        &self,
        id: &str,
        intent: &AbandonIntent,
    ) -> Result<AbandonResult, ApplicationError> {
        let runs = self.kernel_owner(id)?.ok_or(ApplicationError::Missing)?;
        runs.control(id, "abandon", Some(&intent.reason))?;
        runs.wait_idle(id).await;
        self.settle_channel_run(id)?;
        self.kernel_child_finished(id).await?;
        Ok(AbandonResult {
            action: AbandonAction::Abandoned,
            reason: Some(intent.reason.clone()),
            requested_at: Some(intent.requested_at.clone()),
        })
    }

    pub(crate) async fn finalize_recorded_abandons_at_startup(
        &self,
    ) -> Result<(), ApplicationError> {
        let entries = match std::fs::read_dir(self.data_root.join(ABANDON_DIR)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(storage(error)),
        };
        for entry in entries {
            let entry = entry.map_err(storage)?;
            if let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                && let Some(intent) = self.abandon_intent(id)?
                && let Some(view) = self.read_run(id)?
            {
                if !view.is_settled() {
                    self.finish_kernel_abandon(id, &intent).await?;
                } else {
                    self.settle_channel_run(id)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;
    use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot};
    use serde_json::json;
    use std::{future::Future, task::Poll, time::Duration};

    fn intent(id: &str) -> AbandonIntent {
        serde_json::from_value(json!({
            "format":1, "run_id":id, "reason":"operator",
            "requested_at":"2026-10-19T00:00:00Z"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn initial_abandon_outcomes_preserve_kernel_facts_and_intent_details() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        fixtures::scenarios(&application);
        for id in [
            "executing",
            "paused",
            "stopped",
            "budget",
            "recovery",
            "done",
            "failed",
            "aborted",
        ] {
            let result = application.abandon_run(id, Some("operator")).await;
            if matches!(id, "done" | "failed") {
                assert!(
                    matches!(result, Err(ApplicationError::Conflict(message))
                    if message == "terminal Run cannot be abandoned"),
                    "{id}"
                );
                assert!(application.abandon_intent(id).unwrap().is_none());
                continue;
            }
            let result = result.unwrap();
            if id == "aborted" {
                assert_eq!(result.action, AbandonAction::AlreadyAborted);
                assert!(result.reason.is_none());
                assert!(result.requested_at.is_none());
            } else {
                assert_eq!(result.action, AbandonAction::Abandoned);
                let durable = application.abandon_intent(id).unwrap().unwrap();
                assert_eq!(result.reason.as_deref(), Some("operator"));
                assert_eq!(
                    result.requested_at.as_deref(),
                    Some(durable.requested_at.as_str())
                );
                let repeated = application.abandon_run(id, Some("operator")).await.unwrap();
                assert_eq!(repeated.action, AbandonAction::Abandoned);
                assert_eq!(repeated.requested_at, result.requested_at);
                assert!(matches!(
                    application.abandon_run(id, Some("plugin_update")).await,
                    Err(ApplicationError::Conflict(_))
                ));
            }
            assert!(matches!(
                fixtures::view(&application, id).phase(),
                super::super::RunPhase::Settled {
                    outcome: super::super::RunOutcome::Aborted,
                    ..
                }
            ));
        }
    }

    #[tokio::test]
    async fn repeated_abandon_waits_for_the_original_worker_without_repeating_control() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        let id = "abandon-idempotent-drain";
        fixtures::create(&application, &fixtures::work_record(id));
        let drive = fixtures::gated_drive(&application, id).await;
        let first = application.abandon_run(id, Some("operator"));
        tokio::pin!(first);
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(first.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert!(drive.cancelled());
        let view = fixtures::view(&application, id);
        assert!(view.active());
        assert!(matches!(
            view.phase(),
            super::super::RunPhase::Settled {
                outcome: super::super::RunOutcome::Aborted,
                ..
            }
        ));
        let durable = application.abandon_intent(id).unwrap().unwrap();
        let runs = application.kernel_owner(id).unwrap().unwrap();
        let history = runs
            .kernel
            .history(&anchor_runtime::graph::validator::run_resource_id(id))
            .unwrap();
        assert_eq!(history.len(), 1);

        let repeated = application.abandon_run(id, Some("operator"));
        tokio::pin!(repeated);
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(repeated.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert_eq!(
            runs.kernel
                .history(&anchor_runtime::graph::validator::run_resource_id(id))
                .unwrap(),
            history
        );
        assert!(matches!(
            application.abandon_run(id, Some("plugin_update")).await,
            Err(ApplicationError::Conflict(_))
        ));
        assert!(fixtures::view(&application, id).active());

        drive.drain(&application, id).await;
        let (first, repeated) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(first, repeated)
        })
        .await
        .unwrap();
        for result in [first.unwrap(), repeated.unwrap()] {
            assert_eq!(result.action, AbandonAction::Abandoned);
            assert_eq!(result.reason.as_deref(), Some("operator"));
            assert_eq!(
                result.requested_at.as_deref(),
                Some(durable.requested_at.as_str())
            );
        }
        assert!(!fixtures::view(&application, id).active());
        assert_eq!(application.abandon_intent(id).unwrap(), Some(durable));
    }

    #[tokio::test]
    async fn already_aborted_without_an_intent_waits_for_worker_exit_without_control() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        let id = "abandon-already-aborted-drain";
        fixtures::create(&application, &fixtures::work_record(id));
        let drive = fixtures::gated_drive(&application, id).await;
        fixtures::control(&application, id, "abandon", "operator");
        let runs = application.kernel_owner(id).unwrap().unwrap();
        let history = runs
            .kernel
            .history(&anchor_runtime::graph::validator::run_resource_id(id))
            .unwrap();
        let before = fixtures::record(&application, id);
        assert!(application.abandon_intent(id).unwrap().is_none());

        let request = application.abandon_run(id, Some("operator"));
        tokio::pin!(request);
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(request.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert!(fixtures::view(&application, id).active());
        assert_eq!(
            runs.kernel
                .history(&anchor_runtime::graph::validator::run_resource_id(id))
                .unwrap(),
            history
        );
        assert_eq!(fixtures::record(&application, id), before);
        assert!(application.abandon_intent(id).unwrap().is_none());

        drive.drain(&application, id).await;
        let result = tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.action, AbandonAction::AlreadyAborted);
        assert!(result.reason.is_none());
        assert!(result.requested_at.is_none());
        assert!(!fixtures::view(&application, id).active());
        assert!(application.abandon_intent(id).unwrap().is_none());
    }

    #[tokio::test]
    async fn retired_assistant_can_be_abandoned_without_restoring_its_session_binding() {
        use crate::run_metadata::{self as metadata, RunMetadata};
        use anchor_platform_session::{
            ChannelIdentity, ChannelInboundRequest, SessionStore, TurnStatus,
        };

        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        std::fs::create_dir_all(application.data_root.join("platform")).unwrap();
        let sessions =
            SessionStore::open(application.data_root.join("platform/sessions.sqlite")).unwrap();
        let admission = sessions
            .admit_channel_inbound(
                "owner",
                ChannelInboundRequest {
                    inbound_id: "retired-assistant-input".into(),
                    identity: ChannelIdentity {
                        conversation_kind: Default::default(),
                        source: "wecom".into(),
                        account: Some("fixture".into()),
                        conversation_id: "fixture".into(),
                        sender_id: "user".into(),
                    },
                    graph: "fixture".into(),
                    reply_node: "assistant".into(),
                    text: Some("hello".into()),
                    attachments: Default::default(),
                    run_id: None,
                    replace_running: true,
                    bot_origin: None,
                    platform_message_id: None,
                },
            )
            .unwrap();
        let session = admission.session.id;
        let id = "retired-assistant-abandon";
        let mut definition: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/graphs/wecom-persistent-assistant.json"
        )))
        .unwrap();
        definition["nodes"][1]["plugins"] = json!([]);
        let record = GraphRunRecord::create_with_id(
            GraphSnapshot::from_authoring(definition).unwrap(),
            json!({"session":session}),
            id,
        )
        .unwrap();
        fixtures::create(&application, &record);
        fixtures::control(&application, id, "stop", "stopped");
        let mut saved = RunMetadata::new(
            id.into(),
            "fixture".into(),
            record.graph_digest.clone(),
            root.path(),
        )
        .unwrap();
        saved.conversation = Some(metadata::ConversationSource {
            session: session.clone(),
            reply_node: "assistant".into(),
            previous_run: None,
        });
        saved.assistant = Some(metadata::AssistantSource {
            owner: "owner".into(),
            session: session.clone(),
            wait_node: "wait_input".into(),
            work_node: "assistant".into(),
            reply_node: "reply".into(),
        });
        metadata::save(&application.data_root, &saved).unwrap();
        sessions
            .bind_channel_assistant("owner", &session, id, "wait_input", "assistant", "reply")
            .unwrap();
        sessions
            .claim_channel_assistant_input("owner", &session, id, "retire-fixture-wait")
            .unwrap()
            .unwrap();
        sessions
            .finish_turn(
                "owner",
                &session,
                &admission.turn.id,
                TurnStatus::Completed,
                None,
            )
            .unwrap();
        application
            .retire_assistant("owner", &session, id)
            .await
            .unwrap();
        assert!(
            sessions
                .get_channel_assistant("owner", &session)
                .unwrap()
                .is_none()
        );
        assert!(
            sessions
                .retired_channel_assistant("owner", &session, id)
                .unwrap()
        );
        let progress = fixtures::record(&application, id);
        let turns = sessions.list_turns("owner", &session).unwrap();

        let result = application.abandon_run(id, Some("operator")).await.unwrap();
        assert_eq!(result.action, AbandonAction::Abandoned);
        assert!(matches!(
            fixtures::view(&application, id).phase(),
            super::super::RunPhase::Settled {
                outcome: super::super::RunOutcome::Aborted,
                ..
            }
        ));
        assert!(!fixtures::view(&application, id).active());
        assert_eq!(fixtures::record(&application, id), progress);
        assert!(
            sessions
                .get_channel_assistant("owner", &session)
                .unwrap()
                .is_none()
        );
        assert!(
            sessions
                .retired_channel_assistant("owner", &session, id)
                .unwrap()
        );
        assert_eq!(sessions.list_turns("owner", &session).unwrap(), turns);
        let repeated = application.abandon_run(id, Some("operator")).await.unwrap();
        assert_eq!(repeated.action, AbandonAction::Abandoned);
        assert_eq!(repeated.requested_at, result.requested_at);
    }

    #[tokio::test]
    async fn finalization_preserves_committed_progress_and_retained_files() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        let record = GraphRunRecord::create_with_id(
            GraphSnapshot::from_authoring(json!({
                "entry":"first", "agents":{}, "ops":{"work":{"run":"true"}},
                "nodes":[{"id":"first","op":"work"},{"id":"later","op":"work"}],
                "edges":[{"from":"first","to":"later"}]
            }))
            .unwrap(),
            json!({"retained":"input"}),
            "abandon-view",
        )
        .unwrap();
        fixtures::create(&application, &record);
        fixtures::complete(
            &application,
            "abandon-view",
            "first",
            fixtures::completion(json!({"retained":"result"})),
            None,
        );
        let before = fixtures::record(&application, "abandon-view");
        let retained = application.data_root.join("artifacts/retained.txt");
        std::fs::create_dir_all(retained.parent().unwrap()).unwrap();
        std::fs::write(&retained, b"retained artifact").unwrap();
        let intent = intent("abandon-view");
        let result = application
            .finish_kernel_abandon("abandon-view", &intent)
            .await
            .unwrap();
        let after = fixtures::record(&application, "abandon-view");
        assert_eq!(
            result.requested_at.as_deref(),
            Some(intent.requested_at.as_str())
        );
        assert_eq!(after.snapshot, before.snapshot);
        assert_eq!(after.input, before.input);
        assert_eq!(after.results, before.results);
        assert_eq!(after.invocations, before.invocations);
        assert_eq!(after.plugin_bindings, before.plugin_bindings);
        assert_eq!(std::fs::read(retained).unwrap(), b"retained artifact");
    }

    #[tokio::test]
    async fn startup_finishes_recorded_abandon_intent_idempotently() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        fixtures::create(&application, &fixtures::work_record("abandon-view"));
        let intent = intent("abandon-view");
        crate::assistant::save_immutable(
            &application
                .data_root
                .join(ABANDON_DIR)
                .join("abandon-view.json"),
            &intent,
        )
        .unwrap();
        application
            .finalize_recorded_abandons_at_startup()
            .await
            .unwrap();
        let before = fixtures::view(&application, "abandon-view");
        assert!(matches!(
            before.phase(),
            super::super::RunPhase::Settled {
                outcome: super::super::RunOutcome::Aborted,
                ..
            }
        ));
        application
            .finalize_recorded_abandons_at_startup()
            .await
            .unwrap();
        let after = fixtures::view(&application, "abandon-view");
        assert_eq!(after.record, before.record);
        assert_eq!(after.updated(), before.updated());
        assert_eq!(
            application.abandon_intent("abandon-view").unwrap(),
            Some(intent)
        );
    }

    #[tokio::test]
    async fn abandon_uses_kernel_control_while_the_host_admission_lease_is_held() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        fixtures::create(&application, &fixtures::work_record("abandon-view"));
        let _lease = application
            .run_leases()
            .acquire_lease("abandon-view")
            .unwrap();
        let result = application
            .abandon_run("abandon-view", Some("operator"))
            .await
            .unwrap();
        assert_eq!(result.action, AbandonAction::Abandoned);
        assert!(fixtures::view(&application, "abandon-view").is_settled());
    }

    #[tokio::test]
    async fn finalization_rejects_a_terminal_kernel_fact_without_changing_progress() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        fixtures::create(&application, &fixtures::work_record("abandon-view"));
        fixtures::finish(&application, "abandon-view");
        let before = fixtures::view(&application, "abandon-view");
        assert!(matches!(
            application
                .finish_kernel_abandon("abandon-view", &intent("abandon-view"))
                .await,
            Err(ApplicationError::Conflict(_))
        ));
        let after = fixtures::view(&application, "abandon-view");
        assert_eq!(after.record, before.record);
        assert_eq!(after.updated(), before.updated());
    }
}
