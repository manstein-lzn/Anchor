use super::*;
use crate::{HostArtifacts, application::metadata::AssistantSource, assistant::AssistantPlan};
use anchor_platform_session::{SessionError, SessionStore};

pub(crate) struct AssistantAdmission {
    pub(crate) graph: String,
    pub(crate) session: String,
    pub(crate) owner: String,
    pub(crate) oauth_owner: String,
    pub(crate) run: String,
    pub(crate) previous_run: Option<String>,
}

impl RunApplication {
    pub(crate) async fn retire_assistant(
        &self,
        owner: &str,
        session: &str,
        run: &str,
    ) -> Result<(), ApplicationError> {
        let active = self.active.lock().await;
        let metadata = self.metadata(run)?.ok_or(ApplicationError::Missing)?;
        let source = metadata.assistant.ok_or(ApplicationError::Missing)?;
        if source.owner != owner || source.session != session {
            return Err(ApplicationError::Missing);
        }
        let record = self.store().load(run)?.ok_or(ApplicationError::Missing)?;
        if active.contains_key(run)
            || !matches!(
                record.status,
                RunStatus::Stopped | RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
            )
        {
            return Err(ApplicationError::Conflict(
                "stop the assistant before retiring its instance".into(),
            ));
        }
        SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(assistant_session_error)?
            .retire_channel_assistant(owner, session, run)
            .map_err(assistant_session_error)
    }

    pub(super) fn ensure_current_assistant(
        &self,
        metadata: &RunMetadata,
    ) -> Result<(), ApplicationError> {
        let Some(source) = metadata.assistant.as_ref() else {
            return Ok(());
        };
        let current = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(assistant_session_error)?
            .get_channel_assistant(&source.owner, &source.session)
            .map_err(assistant_session_error)?;
        if current.is_none_or(|current| {
            current.run_id != metadata.run_id
                || current.wait_node != source.wait_node
                || current.work_node != source.work_node
                || current.reply_node != source.reply_node
        }) {
            return Err(ApplicationError::Conflict(
                "this assistant instance is retired; start a new instance explicitly".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn retire_assistant_for_deletion(&self, run: &str) -> Result<(), ApplicationError> {
        let Some(source) = self.metadata(run)?.and_then(|metadata| metadata.assistant) else {
            return Ok(());
        };
        match SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(assistant_session_error)?
            .retire_channel_assistant(&source.owner, &source.session, run)
        {
            Ok(()) | Err(SessionError::Missing) => Ok(()),
            Err(error) => Err(assistant_session_error(error)),
        }
    }

    pub(crate) async fn admit_assistant(
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
        let mut active = self.active.lock().await;
        let graph_path = Self::graph_identity(source)?;
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
            let Some(record) = self.store().load(&run)? else {
                return Err(ApplicationError::Conflict(
                    "assistant admission is incomplete; inspect retained facts".into(),
                ));
            };
            if record.graph_digest != saved.graph_digest {
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
        for (_, record) in self.records()? {
            if is_unfinished(record.status) {
                let metadata = self.metadata(&record.run_id)?.ok_or_else(|| {
                    ApplicationError::Conflict("unfinished Run has no immutable identity".into())
                })?;
                if record.status != RunStatus::Stopped
                    && metadata
                        .assistant
                        .as_ref()
                        .is_some_and(|source| source.owner == owner && source.session == session)
                {
                    return Err(ApplicationError::Conflict(format!(
                        "assistant admission retains unfinished Run `{}`; resume that instance before creating another",
                        record.run_id
                    )));
                }
                if Self::graph_identity(&metadata.bundle_source)? == graph_path
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
            let record = self
                .store()
                .load(previous)?
                .ok_or(ApplicationError::Missing)?;
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
        let mut record = GraphRunRecord::create_with_id(
            bundle.snapshot,
            serde_json::json!({"session":session}),
            run.clone(),
        )?;
        record.plugin_bindings = bundle
            .plugins
            .into_iter()
            .map(|plugin| (plugin.id.clone(), plugin))
            .collect();
        record.plugin_bindings_initialized = true;
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
        let lease = self.store().acquire_lease(&run)?;
        metadata::save(&self.data_root, &metadata)?;
        let artifacts = HostArtifacts::new(
            self.data_root.join("artifacts"),
            crate::env_path("ANCHOR_RUNNER_WORKSPACE_ROOT").map_err(ApplicationError::Invalid)?,
        );
        let control = new_control();
        let execution = PreparedExecution::prepare_with_catalog(
            &record,
            control.clone(),
            record.plugin_bindings.clone(),
            self.catalog_root.clone(),
            self.clone(),
        )
        .map_err(ApplicationError::Invalid)?;
        execution
            .bind_local_inputs(&record, &graph)
            .map_err(ApplicationError::Invalid)?;
        for node in &record.snapshot.nodes {
            artifacts.bind_node_workspace(&run, &record.graph_digest, &node.id)?;
        }
        self.store().save(&record)?;
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
        active.insert(
            run.clone(),
            ActiveRun {
                graph_path,
                control,
            },
        );
        drop(lease);
        drop(graph_lease);
        self.spawn(record, execution)?;
        Ok(run)
    }
}

fn assistant_session_error(error: SessionError) -> ApplicationError {
    match error {
        SessionError::Missing => ApplicationError::Missing,
        SessionError::Conflict(message) => ApplicationError::Conflict(message),
        SessionError::Invalid(message) => ApplicationError::Invalid(message),
        error => storage(error),
    }
}
