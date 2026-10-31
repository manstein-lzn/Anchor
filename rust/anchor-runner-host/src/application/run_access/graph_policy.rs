use crate::application::{
    ApplicationError, RunApplication,
    assistants::{AssistantAdmission, assistant_session_error},
    new_control, storage,
};
use crate::run_metadata::{self as metadata, AssistantSource, ConversationSource, RunMetadata};
use crate::{assistant::AssistantPlan, execution::PreparedExecution};
use anchor_graph_host::LoadedGraphBundle;
use anchor_platform_session::SessionStore;
use anchor_runtime::graph::{GraphRunRecord, RunLease};
use std::path::Path;

impl RunApplication {
    pub(in crate::application) async fn admit_assistant_run(
        &self,
        request: AssistantAdmission,
        source: &Path,
        bundle: LoadedGraphBundle,
        graph_lease: Box<dyn RunLease>,
        plan: AssistantPlan,
    ) -> Result<String, ApplicationError> {
        let AssistantAdmission {
            graph,
            session,
            owner,
            oauth_owner,
            run,
            previous_run,
        } = request;
        let plugin_lease = if bundle.plugins.is_empty() {
            None
        } else {
            Some(self.plugin_reference_lease().await?)
        };
        let active = self.admission.lock().await;
        let graph_path = crate::run_metadata::graph_identity(source)?;
        if let Some(saved) = self.metadata(&run)? {
            let binding = saved.assistant.ok_or_else(|| {
                ApplicationError::Conflict("Run is not an assistant instance".into())
            })?;
            if binding.owner != owner
                || binding.session != session
                || saved.bundle_source != graph_path
            {
                return Err(ApplicationError::Conflict(
                    "assistant instance identity changed".into(),
                ));
            }
            let Some(record) = self.read_run(&run)? else {
                return Err(ApplicationError::Conflict(
                    "assistant admission is incomplete; inspect retained facts".into(),
                ));
            };
            if record.progress().graph_digest != saved.graph_digest {
                return Err(ApplicationError::Invalid(
                    "assistant admission Graph identity changed".into(),
                ));
            }
            SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
                .map_err(assistant_session_error)?
                .bind_channel_assistant(
                    &owner,
                    &session,
                    &run,
                    &binding.wait_node,
                    &binding.work_node,
                    &binding.reply_node,
                )
                .map_err(assistant_session_error)?;
            return Ok(run);
        }
        for (run_id, record) in self.run_views()? {
            if !record.is_settled() {
                let metadata = self.metadata(&run_id)?.ok_or_else(|| {
                    ApplicationError::Conflict("unfinished Run has no immutable identity".into())
                })?;
                if !record.has_stopped_execution()
                    && metadata
                        .assistant
                        .as_ref()
                        .is_some_and(|source| source.owner == owner && source.session == session)
                {
                    return Err(ApplicationError::Conflict(format!(
                        "assistant admission retains unfinished Run `{}`; resume that instance before creating another",
                        run_id
                    )));
                }
                if crate::run_metadata::graph_identity(&metadata.bundle_source)? == graph_path
                    && metadata.conversation.is_none()
                    && metadata.session_call.is_none()
                {
                    return Err(ApplicationError::Conflict(
                        "Graph has an unfinished non-conversation Run".into(),
                    ));
                }
            }
        }
        if let Some(previous) = previous_run.as_deref() {
            let record = self.read_run(previous)?.ok_or(ApplicationError::Missing)?;
            self.ensure_predecessor_settled(&record, &active)?;
            let metadata = self.metadata(previous)?.ok_or(ApplicationError::Missing)?;
            if metadata.conversation.as_ref().is_none_or(|binding| {
                binding.session != session || binding.reply_node != plan.work_node
            }) || metadata.bundle_source != graph_path
            {
                return Err(ApplicationError::Conflict(
                    "assistant predecessor belongs to another conversation".into(),
                ));
            }
        }
        let plugin_bindings = self.bundle_plugin_bindings(&bundle)?;
        let record = GraphRunRecord::create_with_plugins(
            bundle.snapshot,
            serde_json::json!({"session":session}),
            run.clone(),
            plugin_bindings.into_values().collect(),
        )?;
        let mut metadata = RunMetadata::new(
            run.clone(),
            graph.clone(),
            record.graph_digest.clone(),
            source,
        )?;
        metadata.trigger_source = "channel".into();
        metadata.oauth_owner = Some(oauth_owner);
        metadata.conversation = Some(ConversationSource {
            session: session.clone(),
            reply_node: plan.work_node.clone(),
            previous_run,
        });
        metadata.assistant = Some(AssistantSource {
            owner: owner.clone(),
            session: session.clone(),
            wait_node: plan.wait_node.clone(),
            work_node: plan.work_node.clone(),
            reply_node: plan.reply_node.clone(),
        });
        let lease = self.run_leases().acquire_lease(&run)?;
        metadata::save(&self.data_root, &metadata)?;
        let control = new_control();
        let execution = PreparedExecution::prepare_with_catalog(
            &record,
            control.clone(),
            record.plugin_bindings.clone(),
            self.clone(),
        )
        .map_err(ApplicationError::Invalid)?;
        execution
            .bind_local_inputs(&record, &graph)
            .map_err(ApplicationError::Invalid)?;
        self.kernel_runs(true)?
            .expect("Run store initialized")
            .create(&record, u64::MAX)?;
        SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(storage)?
            .bind_channel_assistant(
                &owner,
                &session,
                &run,
                &plan.wait_node,
                &plan.work_node,
                &plan.reply_node,
            )
            .map_err(storage)?;
        drop(lease);
        drop(plugin_lease);
        drop(graph_lease);
        drop(active);
        self.spawn_kernel(record, execution, control)?;
        Ok(run)
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;
    use anchor_runtime::graph::{
        CommitRef, GraphCallStatus, GraphSnapshot, validator::CallSubmission,
    };
    use serde_json::{Value, json};

    fn record(run_id: &str, mode: &str) -> GraphRunRecord {
        GraphRunRecord::create_with_id(
            GraphSnapshot::from_authoring(json!({
                "entry":"call", "agents":{},
                "ops":{"call":{"call":{"graph":"child","mode":mode}}},
                "nodes":[{"id":"call","op":"call"}], "edges":[]
            }))
            .unwrap(),
            Value::Null,
            run_id,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn graph_cascade_allows_missing_wait_child_but_handover_refuses() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        let parent = record("parent", "wait");
        fixtures::create(&application, &parent);
        fixtures::bind_child(&application, "parent", "call", "missing-child");
        fixtures::control(&application, "parent", "stop", "stopped");
        let child = record("missing-child", "wait");
        let metadata = RunMetadata::graph_call_child(
            child.run_id.clone(),
            "graph".into(),
            child.graph_digest,
            root.path(),
            metadata::GraphCallSource {
                parent_run: parent.run_id.clone(),
                parent_graph: "graph".into(),
                parent_graph_digest: parent.graph_digest.clone(),
                node: "call".into(),
                invocation: 1,
                mode: "wait".into(),
                root_run: parent.run_id.clone(),
            },
        )
        .unwrap();
        metadata::save(&application.data_root, &metadata).unwrap();
        let view = fixtures::view(&application, "parent");
        application
            .ensure_conversation_delete_settled(&view, &())
            .unwrap();
        assert!(matches!(application.ensure_predecessor_settled(&view, &()),
            Err(ApplicationError::Conflict(reason)) if reason == "previous conversation has a missing wait child"));
        assert_eq!(
            fixtures::record(&application, "parent"),
            view.record.as_ref().clone()
        );
        assert!(application.read_run("missing-child").unwrap().is_none());
    }

    #[tokio::test]
    async fn individual_delete_does_not_require_an_inactive_unreferenced_run_to_settle() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        for id in fixtures::scenarios(&application) {
            if id == "child" {
                continue;
            }
            application.delete_run(id, root.path()).await.unwrap();
            assert!(application.read_run(id).unwrap().is_none(), "{id}");
        }
        let call = record("unbound-call", "wait");
        fixtures::create(&application, &call);
        application
            .delete_run("unbound-call", root.path())
            .await
            .unwrap();
        assert!(application.read_run("unbound-call").unwrap().is_none());
    }

    #[tokio::test]
    async fn retained_graph_calls_protect_the_incoming_reference() {
        for (status, mode) in [
            (GraphCallStatus::Waiting, "wait"),
            (GraphCallStatus::Completed, "wait"),
            (GraphCallStatus::Detached, "detach"),
            (GraphCallStatus::Failed, "wait"),
            (GraphCallStatus::Uncertain, "wait"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let application = RunApplication::new(root.path().join("state"), root.path().into());
            fixtures::create(&application, &fixtures::work_record("target"));
            fixtures::create(&application, &record("parent", mode));
            let submission = match status {
                GraphCallStatus::Waiting => CallSubmission::Bound {
                    child_run_id: "target".into(),
                },
                GraphCallStatus::Completed | GraphCallStatus::Detached => {
                    CallSubmission::Completed {
                        child_run_id: "target".into(),
                        output: json!({}),
                        commit: CommitRef {
                            id: "call-commit".into(),
                            node_id: "call".into(),
                            invocation: 1,
                        },
                    }
                }
                GraphCallStatus::Failed => CallSubmission::Failed {
                    child_run_id: Some("target".into()),
                    reason: "known failure".into(),
                },
                GraphCallStatus::Uncertain => CallSubmission::Uncertain {
                    child_run_id: Some("target".into()),
                    reason: "unknown outcome".into(),
                },
                GraphCallStatus::Prepared => unreachable!(),
            };
            fixtures::call(&application, "parent", "call", submission);
            let parent = fixtures::record(&application, "parent");
            let target = fixtures::record(&application, "target");
            assert_eq!(parent.graph_calls.values().next().unwrap().status, status);
            assert!(
                matches!(application.delete_run("target", root.path()).await,
                Err(ApplicationError::Conflict(reason)) if reason == "Run is referenced by another Graph call"),
                "{status:?}"
            );
            assert_eq!(fixtures::record(&application, "target"), target);
            assert_eq!(fixtures::record(&application, "parent"), parent);
            assert!(
                matches!(application.delete_run("parent", root.path()).await,
                Err(ApplicationError::Conflict(reason)) if reason == "Run is referenced by or owns a Graph call"),
                "{status:?}"
            );
            assert_eq!(fixtures::record(&application, "parent"), parent);
        }
    }
}
