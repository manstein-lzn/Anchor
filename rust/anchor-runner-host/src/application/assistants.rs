use super::*;
use crate::{
    assistant::AssistantPlan, codex_bridge_tools::BridgeGrant, project_tools::ProjectGrant,
};
use anchor_graph_host::LoadedGraphBundle;
use anchor_platform_session::{SessionError, SessionStore};
use anchor_runtime::graph::{InvocationKey, PluginBinding};

pub(crate) struct AssistantAdmission {
    pub(crate) graph: String,
    pub(crate) session: String,
    pub(crate) owner: String,
    pub(crate) oauth_owner: String,
    pub(crate) run: String,
    pub(crate) previous_run: Option<String>,
}

impl RunApplication {
    pub(crate) fn authorized_codex_bridge(
        &self,
        key: &InvocationKey,
        bindings: &[PluginBinding],
    ) -> Option<Arc<BridgeGrant>> {
        let plugin = bindings.iter().find(|b| b.id == "codex")?;
        let grant = self.codex_bridge.as_ref()?;
        let metadata = self.metadata(&key.run_id).ok()??;
        let source = metadata.assistant.as_ref()?;
        if !grant.allows_graph(&metadata.graph)
            || source.work_node != key.node_id
            || metadata.graph_digest != key.graph_digest
        {
            return None;
        }
        self.ensure_current_assistant(&metadata).ok()?;
        let sessions = crate::assistant::sessions(&self.data_root).ok()?;
        let binding = crate::assistant::binding(&self.data_root, key).ok()??;
        let turn = sessions
            .get_turn(&source.owner, &source.session, &binding.turn)
            .ok()?;
        if turn.status != anchor_platform_session::TurnStatus::Running {
            return None;
        }
        let frozen = crate::assistant_plugins::load(
            &self.data_root,
            &key.run_id,
            &source.session,
            &binding.turn,
        )
        .ok()??;
        if !frozen.network
            || frozen.node != key.node_id
            || frozen.bindings.get("codex") != Some(plugin)
        {
            return None;
        }
        Some(grant.clone())
    }

    pub(crate) fn authorized_project(
        &self,
        key: &InvocationKey,
    ) -> Result<Arc<ProjectGrant>, String> {
        let metadata = self
            .metadata(&key.run_id)
            .map_err(|e| format!("{e:?}"))?
            .ok_or("project access requires admitted Run metadata")?;
        if metadata.graph_digest != key.graph_digest {
            return Err("project invocation Graph differs from its admitted Run".into());
        }
        let grant = self
            .project_workspaces
            .get(&metadata.graph)
            .ok_or("this Graph has no direct project grant")?;
        let turn = self
            .active_channel_turn(key)
            .map_err(|e| format!("{e:?}"))?;
        if turn.source.owner != grant.owner {
            return Err("project owner differs from the active assistant".into());
        }
        self.ensure_channel_session_eligible(&turn.source.owner, &turn.source.session)
            .map_err(|e| format!("{e:?}"))?;
        for private in [self.data_root.as_path(), self.library_root()] {
            let private = private
                .canonicalize()
                .map_err(|_| "Host state or library directory is unavailable")?;
            if let Ok(relative) = private.strip_prefix(&grant.path)
                && !grant
                    .exclude
                    .iter()
                    .any(|excluded| relative.starts_with(excluded))
            {
                return Err("project grant must exclude Host state and library directories".into());
            }
        }
        grant.verify()?;
        Ok(grant)
    }

    pub(crate) async fn retire_assistant(
        &self,
        owner: &str,
        session: &str,
        run: &str,
    ) -> Result<(), ApplicationError> {
        let _admission = self.admission.lock().await;
        let metadata = self.metadata(run)?.ok_or(ApplicationError::Missing)?;
        let source = metadata.assistant.ok_or(ApplicationError::Missing)?;
        if source.owner != owner || source.session != session {
            return Err(ApplicationError::Missing);
        }
        let record = self.read_run(run)?.ok_or(ApplicationError::Missing)?;
        if !record.is_quiescent() {
            return Err(ApplicationError::Conflict(
                "stop the assistant before retiring its instance".into(),
            ));
        }
        SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(assistant_session_error)?
            .retire_channel_assistant(owner, session, run)
            .map_err(assistant_session_error)
    }

    pub(crate) fn ensure_current_assistant(
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
        self.ensure_channel_session_eligible(&source.owner, &source.session)
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
        self.ensure_channel_session_eligible(&request.owner, &request.session)?;
        self.admit_assistant_run(request, source, bundle, graph_lease, plan)
            .await
    }
}

pub(super) fn assistant_session_error(error: SessionError) -> ApplicationError {
    match error {
        SessionError::Missing => ApplicationError::Missing,
        SessionError::Conflict(message) => ApplicationError::Conflict(message),
        SessionError::Invalid(message) => ApplicationError::Invalid(message),
        error => storage(error),
    }
}
