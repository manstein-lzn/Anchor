//! Session authority belongs to the platform host; execution belongs to this application.
use super::*;
use anchor_runtime::graph::{CallIdentity, GraphCallOutcome};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionContext {
    pub session: String,
    pub reply_node: String,
    pub conversation_id: String,
    pub channel: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionCall {
    pub context: SessionContext,
    pub status: String,
    #[serde(default)]
    pub error: String,
}

pub(crate) async fn resolve(
    identity: &CallIdentity,
    session: &str,
    graph: &str,
) -> Result<Value, GraphError> {
    let url = std::env::var("ANCHOR_SESSION_HOST_URL").map_err(|_| {
        GraphError::Unsupported("call.session requires a configured Session host".into())
    })?;
    let token = std::env::var("ANCHOR_SESSION_HOST_TOKEN").map_err(|_| {
        GraphError::Unsupported("call.session requires Session host authentication".into())
    })?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| GraphError::Unsupported("Session host client unavailable".into()))?;
    let response = client
        .post(format!(
            "{}/v1/runtime/session-calls/resolve",
            url.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "parent_run":identity.parent_run_id,"graph":graph,"session":session,
        }))
        .send()
        .await
        .map_err(|_| GraphError::Unsupported("Session host unavailable".into()))?;
    if !response.status().is_success() {
        return Err(GraphError::Unsupported(
            "Session host rejected call.session authorization".into(),
        ));
    }
    let context: SessionContext = response
        .json()
        .await
        .map_err(|_| GraphError::Unsupported("Session host returned invalid context".into()))?;
    if context.session != session || context.reply_node.is_empty() || !context.channel.is_object() {
        return Err(GraphError::Unsupported(
            "Session host returned a different identity".into(),
        ));
    }
    serde_json::to_value(context).map_err(GraphError::SnapshotDecode)
}

impl RunApplication {
    pub(crate) fn settled_session_call_successors(
        &self,
        metadata: &RunMetadata,
    ) -> Result<Vec<GraphRunRecord>, ApplicationError> {
        let Some(call) = &metadata.session_call else {
            return Ok(Vec::new());
        };
        if call.status != "pending" {
            return Ok(Vec::new());
        }
        let binding = metadata.conversation.as_ref().ok_or_else(|| {
            ApplicationError::Conflict("Session call has no execution binding".into())
        })?;
        if metadata.graph_call.is_none()
            || call.context.session != binding.session
            || call.context.reply_node != binding.reply_node
        {
            return Err(ApplicationError::Conflict(
                "Session call execution identity changed".into(),
            ));
        }
        let chain = self.conversation_chain(binding, &metadata.bundle_source)?;
        let mut next = chain.first().map(|(entry, _)| entry.run_id.as_str());
        let mut successors = Vec::new();
        while let Some(id) = next {
            if id == metadata.run_id {
                return Ok(successors);
            }
            let (entry, record) = chain
                .iter()
                .find(|(entry, _)| entry.run_id == id)
                .ok_or_else(|| ApplicationError::Conflict("Session successor is missing".into()))?;
            if !matches!(
                record.status,
                RunStatus::Completed | RunStatus::Stopped | RunStatus::Failed | RunStatus::Aborted
            ) {
                return Err(ApplicationError::Conflict(
                    "Session successor is not settled".into(),
                ));
            }
            successors.push(record.clone());
            next = entry
                .conversation
                .as_ref()
                .and_then(|binding| binding.previous_run.as_deref());
        }
        Err(ApplicationError::Conflict(
            "Session call is outside the admitted conversation chain".into(),
        ))
    }

    pub(crate) fn reject_pending_session_delivery(
        &self,
        run_id: &str,
    ) -> Result<(), ApplicationError> {
        if self.metadata(run_id)?.is_some_and(|metadata| {
            metadata
                .session_call
                .is_some_and(|call| call.status == "pending")
        }) {
            return Err(ApplicationError::Conflict(
                "Run has pending Session execution or delivery".into(),
            ));
        }
        Ok(())
    }

    pub(super) async fn stop_wait_session_children(
        &self,
        parent: &str,
    ) -> Result<(), ApplicationError> {
        for child in self.child_metadata(parent)? {
            if child
                .graph_call
                .as_ref()
                .is_some_and(|source| source.mode == "wait")
                && child
                    .session_call
                    .as_ref()
                    .is_some_and(|call| call.status == "pending")
                && self.store().load(&child.run_id)?.is_some()
            {
                self.stop_session_call(&child.run_id).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn stop_session_call(&self, run_id: &str) -> Result<(), ApplicationError> {
        let active = self.active.lock().await;
        let mut metadata = self.metadata(run_id)?.ok_or(ApplicationError::Missing)?;
        let call = metadata
            .session_call
            .as_mut()
            .ok_or_else(|| ApplicationError::Invalid("Run is not a Session call".into()))?;
        if call.status != "pending" {
            return Err(ApplicationError::Conflict(
                "Session delivery already settled".into(),
            ));
        }
        let lease = if active.contains_key(run_id) {
            None
        } else {
            Some(self.store().acquire_lease(run_id)?)
        };
        let mut record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        call.status = "failed".into();
        call.error = "background call stopped".into();
        metadata::save(&self.data_root, &metadata)?;
        if let Some(run) = active.get(run_id) {
            run.control.cancellation.store(true, Ordering::Release);
        } else if !matches!(
            record.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
        ) {
            record.status = RunStatus::Stopped;
            self.store().save(&record)?;
        }
        drop(lease);
        drop(active);
        self.child_finished(run_id, &metadata.graph, RunStatus::Completed)
            .await
    }

    pub(crate) async fn yield_session_call(&self, run_id: &str) -> Result<(), ApplicationError> {
        let active = self.active.lock().await;
        let metadata = self.metadata(run_id)?.ok_or(ApplicationError::Missing)?;
        if metadata.session_call.is_none() {
            return Err(ApplicationError::Invalid(
                "Run is not a Session call".into(),
            ));
        }
        if let Some(run) = active.get(run_id) {
            run.control.cancellation.store(true, Ordering::Release);
            return Ok(());
        }
        let _lease = self.store().acquire_lease(run_id)?;
        let mut record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        if !matches!(
            record.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
        ) {
            record.status = RunStatus::Stopped;
            self.store().save(&record)?;
        }
        Ok(())
    }

    pub(crate) fn record_session_call(
        &self,
        run_id: &str,
        context: &Value,
    ) -> Result<(), ApplicationError> {
        let context: SessionContext = serde_json::from_value(context.clone()).map_err(storage)?;
        let mut metadata = self.metadata(run_id)?.ok_or(ApplicationError::Missing)?;
        let call = SessionCall {
            context,
            status: "pending".into(),
            error: String::new(),
        };
        if let Some(existing) = &metadata.session_call {
            if existing.context != call.context {
                return Err(ApplicationError::Conflict(
                    "session call identity changed".into(),
                ));
            }
            return Ok(());
        }
        metadata.session_call = Some(call);
        metadata::save(&self.data_root, &metadata)
    }

    pub(crate) fn session_call_outcome(
        &self,
        run_id: &str,
        mode: &str,
    ) -> Result<Option<GraphCallOutcome>, ApplicationError> {
        let metadata = self.metadata(run_id)?.ok_or(ApplicationError::Missing)?;
        let call = metadata
            .session_call
            .ok_or_else(|| ApplicationError::Invalid("missing Session call context".into()))?;
        if mode == "detach" {
            return Ok(Some(GraphCallOutcome::Detached {
                child_run_id: run_id.into(),
            }));
        }
        Ok(match call.status.as_str() {
            "delivered" => None,
            "failed" => Some(GraphCallOutcome::Failed {
                child_run_id: Some(run_id.into()),
                reason: call.error,
            }),
            _ => Some(GraphCallOutcome::Waiting {
                child_run_id: run_id.into(),
            }),
        })
    }

    /// The authenticated Session host holds the same foreground/background lease used by
    /// message admission. Never expose this capability through ordinary resume.
    pub(crate) async fn execute_session_call(
        &self,
        run_id: &str,
        session: &str,
        previous: Option<String>,
    ) -> Result<(), ApplicationError> {
        let mut active = self.active.lock().await;
        let mut metadata = self.metadata(run_id)?.ok_or(ApplicationError::Missing)?;
        let call = metadata
            .session_call
            .as_ref()
            .ok_or_else(|| ApplicationError::Invalid("Run is not a Session call".into()))?;
        if call.status != "pending" || call.context.session != session {
            return Err(ApplicationError::Conflict(
                "Session call is no longer pending for this Session".into(),
            ));
        }
        let binding = ConversationSource {
            session: session.into(),
            reply_node: call.context.reply_node.clone(),
            previous_run: previous,
        };
        if metadata
            .conversation
            .as_ref()
            .is_some_and(|existing| existing != &binding)
        {
            return Err(ApplicationError::Conflict(
                "Session execution binding changed".into(),
            ));
        }
        if active.contains_key(run_id) {
            return Ok(());
        }
        let _graph_lease = self.graph_admission_lease(&metadata.bundle_source)?;
        let lease = self.store().acquire_lease(run_id)?;
        let mut record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        if record.status == RunStatus::Completed {
            return Ok(());
        }
        if matches!(
            record.status,
            RunStatus::Failed | RunStatus::Aborted | RunStatus::BudgetStopped
        ) {
            return Err(ApplicationError::Conflict(
                "Session call execution cannot be continued".into(),
            ));
        }
        if metadata.graph_digest != record.graph_digest {
            return Err(ApplicationError::Conflict(
                "Session call Graph identity changed".into(),
            ));
        }
        let graph_path = Self::graph_identity(&metadata.bundle_source)?;
        for (id, other) in self.records()? {
            if id == run_id || !is_unfinished(other.status) {
                continue;
            }
            if let Some(identity) = self.metadata(&id)?
                && Self::graph_identity(&identity.bundle_source)? == graph_path
                && identity.conversation.is_none()
                && identity.session_call.is_none()
            {
                return Err(ApplicationError::Conflict(
                    "Graph has unfinished execution outside this Session".into(),
                ));
            }
        }
        let chain = self.conversation_chain(&binding, &metadata.bundle_source)?;
        for (_, other) in &chain {
            if other.run_id != run_id {
                self.ensure_predecessor_settled(other, &active)?;
            }
        }
        if metadata.conversation.is_none() {
            if chain.first().map(|(head, _)| head.run_id.as_str())
                != binding.previous_run.as_deref()
            {
                return Err(ApplicationError::Conflict(
                    "previous_run must name the latest conversation Run".into(),
                ));
            }
            if !record
                .snapshot
                .nodes
                .iter()
                .any(|node| node.id == binding.reply_node)
            {
                return Err(ApplicationError::Invalid(
                    "reply node is absent from the accepted Graph".into(),
                ));
            }
            metadata.conversation = Some(binding);
            metadata::save(&self.data_root, &metadata)?;
        }
        let control = new_control();
        let execution = self.prepare_resume(&record, &metadata, control.clone())?;
        // Ordinary recovery records an unknown effect as context; it never replays a tool.
        for pending in std::mem::take(&mut record.recovery) {
            let decision = RecoveryDecision::Completed {
                observation: format!(
                    "The previous {} tool call was interrupted before its result was recorded. Its effect is unknown. Inspect the actual state before continuing; do not blindly repeat it.",
                    pending.attempt.tool
                ),
            };
            execution
                .record_recovery_decision(
                    &pending.key,
                    pending.attempt.attempt_id,
                    decision.clone(),
                )
                .map_err(ApplicationError::Conflict)?;
            record
                .recovery_submissions
                .push(anchor_runtime::graph::RecoverySubmission {
                    key: pending.key,
                    attempt_id: pending.attempt.attempt_id,
                    decision,
                });
        }
        record.status = RunStatus::Running;
        self.store().save(&record)?;
        active.insert(
            run_id.into(),
            ActiveRun {
                graph_path: Self::graph_identity(&metadata.bundle_source)?,
                control,
            },
        );
        drop(lease);
        drop(_graph_lease);
        self.spawn(record, execution)?;
        Ok(())
    }

    pub(crate) async fn settle_session_call(
        &self,
        run_id: &str,
        status: &str,
        error: &str,
    ) -> Result<(), ApplicationError> {
        if !matches!(status, "delivered" | "failed") {
            return Err(ApplicationError::Invalid(
                "invalid Session delivery status".into(),
            ));
        }
        let active = self.active.lock().await;
        if active.contains_key(run_id) {
            return Err(ApplicationError::Conflict(
                "Session execution has not settled".into(),
            ));
        }
        let lease = self.store().acquire_lease(run_id)?;
        let record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        if status == "delivered" && record.status != RunStatus::Completed {
            return Err(ApplicationError::Conflict(
                "only completed execution can be delivered".into(),
            ));
        }
        let mut metadata = self.metadata(run_id)?.ok_or(ApplicationError::Missing)?;
        let call = metadata
            .session_call
            .as_mut()
            .ok_or_else(|| ApplicationError::Invalid("Run is not a Session call".into()))?;
        if call.status != "pending" && call.status != status {
            return Err(ApplicationError::Conflict(
                "Session delivery already settled differently".into(),
            ));
        }
        call.status = status.into();
        call.error = error.chars().take(1000).collect();
        metadata::save(&self.data_root, &metadata)?;
        drop(lease);
        drop(active);
        self.child_finished(run_id, &metadata.graph, RunStatus::Completed)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seed(
        application: &RunApplication,
        path: &Path,
        id: &str,
        mode: Option<&str>,
        status: RunStatus,
    ) {
        let snapshot = GraphSnapshot::from_authoring(json!({
            "entry":"work","agents":{},"ops":{"work":{"run":"true"}},
            "nodes":[{"id":"work","op":"work"}],"edges":[],
        }))
        .unwrap();
        let mut record = GraphRunRecord::create_with_id(snapshot, json!({}), id).unwrap();
        record.status = status;
        let mut meta = RunMetadata::new(
            id.into(),
            "fixture".into(),
            record.graph_digest.clone(),
            path,
        )
        .unwrap();
        if let Some(mode) = mode {
            meta.trigger_source = "graph_call".into();
            meta.graph_call = Some(metadata::GraphCallSource {
                parent_run: "parent".into(),
                parent_graph: "fixture".into(),
                parent_graph_digest: record.graph_digest.clone(),
                node: id.into(),
                invocation: 1,
                mode: mode.into(),
                root_run: "parent".into(),
            });
            meta.session_call = Some(SessionCall {
                context: SessionContext {
                    session: "alice".into(),
                    reply_node: "work".into(),
                    conversation_id: "alice".into(),
                    channel: json!({"source":"wecom","sender_id":"alice"}),
                },
                status: "pending".into(),
                error: String::new(),
            });
        }
        metadata::save(&application.data_root, &meta).unwrap();
        application.store().save(&record).unwrap();
    }

    async fn stopped_parent_cancels_wait_delivery(active_parent: bool) {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().join("state"), root.path().into());
        seed(
            &application,
            root.path(),
            "parent",
            None,
            if active_parent {
                RunStatus::Running
            } else {
                RunStatus::WaitingCall
            },
        );
        seed(
            &application,
            root.path(),
            "wait-child",
            Some("wait"),
            RunStatus::Completed,
        );
        seed(
            &application,
            root.path(),
            "detach-child",
            Some("detach"),
            RunStatus::Completed,
        );
        let original = application.store().load("wait-child").unwrap().unwrap();
        if active_parent {
            application.active.lock().await.insert(
                "parent".into(),
                ActiveRun {
                    graph_path: root.path().into(),
                    control: new_control(),
                },
            );
        }
        application.control("parent", "stop").await.unwrap();
        assert_eq!(
            application
                .metadata("wait-child")
                .unwrap()
                .unwrap()
                .session_call
                .unwrap()
                .status,
            "failed"
        );
        assert_eq!(
            application
                .metadata("detach-child")
                .unwrap()
                .unwrap()
                .session_call
                .unwrap()
                .status,
            "pending"
        );
        assert_eq!(
            serde_json::to_value(application.store().load("wait-child").unwrap().unwrap()).unwrap(),
            serde_json::to_value(original).unwrap()
        );
        if active_parent {
            assert_eq!(application.control_requested("parent").await, Some("stop"));
        } else {
            assert_eq!(
                application.store().load("parent").unwrap().unwrap().status,
                RunStatus::Stopped
            );
            tokio::task::yield_now().await;
            assert!(application.active_runs(None).await.is_empty());
        }
    }

    #[tokio::test]
    async fn active_parent_stop_cancels_wait_session_delivery_but_not_detach() {
        stopped_parent_cancels_wait_delivery(true).await;
    }

    #[tokio::test]
    async fn waiting_parent_stop_cancels_completed_pending_session_child_without_waking_parent() {
        stopped_parent_cancels_wait_delivery(false).await;
    }
}
