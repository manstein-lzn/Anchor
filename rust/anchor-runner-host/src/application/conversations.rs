use super::*;
use anchor_platform_session::{SessionError, SessionStore, TurnStatus};
use serde::Deserialize;

/// Identity is supplied by the trusted host, separately from model-visible input.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConversationAdmission {
    pub(crate) graph: String,
    pub(crate) run: String,
    pub(crate) session: String,
    pub(crate) reply_node: String,
    #[serde(default)]
    pub(crate) input: Value,
    #[serde(default)]
    pub(crate) previous_run: Option<String>,
    #[serde(default)]
    pub(crate) attachments: Vec<crate::channel_inputs::UploadedAttachment>,
    #[serde(default)]
    pub(crate) channel_inbound: Option<String>,
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

impl ConversationAdmission {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let uuid = self.run.strip_prefix("channel-").unwrap_or("");
        if uuid.len() != 36
            || !uuid.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
        {
            return Err("run must be channel-<turn UUID>".into());
        }
        if !valid_id(&self.session) || self.reply_node.trim().is_empty() {
            return Err("session and reply_node are required".into());
        }
        if self.previous_run.as_ref().is_some_and(|id| !valid_id(id)) {
            return Err("invalid previous_run".into());
        }
        if self
            .channel_inbound
            .as_ref()
            .is_some_and(|id| !valid_id(id))
        {
            return Err("invalid channel_inbound".into());
        }
        if !self.input.is_null() && !self.input.is_object() {
            return Err("input must be an object".into());
        }
        Ok(())
    }

    fn source(&self) -> ConversationSource {
        ConversationSource {
            session: self.session.clone(),
            reply_node: self.reply_node.clone(),
            previous_run: self.previous_run.clone(),
        }
    }
}

impl RunApplication {
    pub(crate) fn settle_channel_run(
        &self,
        run_id: &str,
        status: RunStatus,
    ) -> Result<(), ApplicationError> {
        let Some(metadata) = self.metadata(run_id)? else {
            return Ok(());
        };
        let Some(channel) = metadata.channel else {
            return Ok(());
        };
        let turn_status = match status {
            RunStatus::Completed => TurnStatus::Completed,
            RunStatus::Failed | RunStatus::Aborted => TurnStatus::Failed,
            RunStatus::Stopped => TurnStatus::Stopped,
            RunStatus::Ready
            | RunStatus::Running
            | RunStatus::Paused
            | RunStatus::BudgetStopped
            | RunStatus::WaitingCall
            | RunStatus::WaitingRecovery => return Ok(()),
        };
        let sessions = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(|error| ApplicationError::Storage(error.to_string()))?;
        match sessions.finish_turn(
            &channel.owner,
            &channel.session,
            &channel.turn,
            turn_status,
            None,
        ) {
            Ok(_) | Err(SessionError::Missing) => Ok(()),
            Err(error) => Err(ApplicationError::Storage(error.to_string())),
        }
    }

    fn retry_conversation(
        &self,
        request: &ConversationAdmission,
        graph_path: &Path,
        attachments: &crate::channel_inputs::PreparedAttachments,
        channel: Option<&ChannelRunSource>,
    ) -> Result<Option<String>, ApplicationError> {
        let Some(record) = self.store().load(&request.run)? else {
            return Ok(None);
        };
        let metadata = self.metadata(&request.run)?.ok_or_else(|| {
            ApplicationError::Conflict("Run has no immutable conversation identity".into())
        })?;
        let repeated = GraphRunRecord::create_with_id(
            record.snapshot.clone(),
            request.input.clone(),
            request.run.clone(),
        )?;
        if metadata.graph != request.graph
            || Self::graph_identity(&metadata.bundle_source)? != Self::graph_identity(graph_path)?
            || metadata.graph_digest != record.graph_digest
            || metadata.conversation.as_ref() != Some(&request.source())
            || metadata.channel.as_ref() != channel
            || repeated.input != record.input
            || metadata.attachments != attachments.manifest()
        {
            return Err(ApplicationError::Conflict(
                "Run id was already used for different conversation input".into(),
            ));
        }
        crate::channel_inputs::verify(&self.data_root, &metadata)
            .map_err(ApplicationError::Conflict)?;
        Ok(Some(request.run.clone()))
    }

    pub(crate) async fn retry_conversation_admission(
        &self,
        request: &ConversationAdmission,
        graph_path: &Path,
        attachments: &crate::channel_inputs::PreparedAttachments,
        channel: Option<&ChannelRunSource>,
    ) -> Result<Option<String>, ApplicationError> {
        let _active = self.active.lock().await;
        self.retry_conversation(request, graph_path, attachments, channel)
    }

    pub(super) fn conversation_chain(
        &self,
        source: &ConversationSource,
        graph_path: &Path,
    ) -> Result<Vec<(RunMetadata, GraphRunRecord)>, ApplicationError> {
        let identity = Self::graph_identity(graph_path)?;
        let mut entries = Vec::new();
        for (id, record) in self.records()? {
            let Some(metadata) = self.metadata(&id)? else {
                continue;
            };
            let Some(conversation) = metadata.conversation.as_ref() else {
                continue;
            };
            if conversation.session != source.session {
                continue;
            }
            if Self::graph_identity(&metadata.bundle_source)? != identity
                || conversation.reply_node != source.reply_node
                || metadata.graph_digest != record.graph_digest
            {
                return Err(ApplicationError::Conflict(
                    "Session is bound to another Graph or reply node".into(),
                ));
            }
            entries.push((metadata, record));
        }
        if entries.is_empty() {
            return Ok(entries);
        }
        let referenced = entries
            .iter()
            .filter_map(|(metadata, _)| metadata.conversation.as_ref()?.previous_run.as_deref())
            .collect::<BTreeSet<_>>();
        let heads = entries
            .iter()
            .filter(|(metadata, _)| !referenced.contains(metadata.run_id.as_str()))
            .collect::<Vec<_>>();
        if heads.len() != 1 {
            return Err(ApplicationError::Conflict(
                "Session Run lineage is not a single chain".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        let mut at = Some(heads[0].0.run_id.as_str());
        while let Some(id) = at {
            if !seen.insert(id) {
                return Err(ApplicationError::Conflict(
                    "Session Run lineage contains a cycle".into(),
                ));
            }
            let (metadata, _) = entries
                .iter()
                .find(|(metadata, _)| metadata.run_id == id)
                .ok_or_else(|| {
                    ApplicationError::Conflict(
                        "Session Run lineage has a missing predecessor".into(),
                    )
                })?;
            at = metadata
                .conversation
                .as_ref()
                .and_then(|source| source.previous_run.as_deref());
        }
        if seen.len() != entries.len() {
            return Err(ApplicationError::Conflict(
                "Session Run lineage is disconnected".into(),
            ));
        }
        // The only unreferenced Run is the latest accepted message, independent of clocks.
        let head = heads[0].0.run_id.clone();
        entries.sort_by_key(|(metadata, _)| metadata.run_id != head);
        Ok(entries)
    }

    pub(super) fn ensure_predecessor_settled(
        &self,
        record: &GraphRunRecord,
        active: &HashMap<String, ActiveRun>,
    ) -> Result<(), ApplicationError> {
        self.ensure_wait_closure_settled(record, active, false)
    }

    pub(super) fn ensure_conversation_delete_settled(
        &self,
        record: &GraphRunRecord,
        active: &HashMap<String, ActiveRun>,
    ) -> Result<(), ApplicationError> {
        self.ensure_wait_closure_settled(record, active, true)
    }

    fn ensure_wait_closure_settled(
        &self,
        record: &GraphRunRecord,
        active: &HashMap<String, ActiveRun>,
        allow_deleted_children: bool,
    ) -> Result<(), ApplicationError> {
        let mut pending = vec![record.clone()];
        let mut seen = BTreeSet::new();
        while let Some(record) = pending.pop() {
            if !seen.insert(record.run_id.clone()) {
                continue;
            }
            if active.contains_key(&record.run_id)
                || !matches!(
                    record.status,
                    RunStatus::Completed
                        | RunStatus::Stopped
                        | RunStatus::Failed
                        | RunStatus::Aborted
                )
            {
                return Err(ApplicationError::Conflict(
                    "previous conversation execution has not stopped; wait for it to become inactive".into(),
                ));
            }
            let mut children = record
                .graph_calls
                .values()
                .filter(|call| call.mode == "wait")
                .filter_map(|call| call.child_run_id.clone())
                .collect::<BTreeSet<_>>();
            children.extend(self.child_metadata(&record.run_id)?.into_iter().filter_map(
                |metadata| {
                    metadata
                        .graph_call
                        .as_ref()
                        .filter(|source| source.mode == "wait")
                        .map(|_| metadata.run_id.clone())
                },
            ));
            for id in children {
                match self.store().load(&id)? {
                    Some(child) => pending.push(child),
                    None if allow_deleted_children => {}
                    None => {
                        return Err(ApplicationError::Conflict(
                            "previous conversation has a missing wait child".into(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn has_conversation_successor(
        &self,
        run_id: &str,
    ) -> Result<bool, ApplicationError> {
        for (id, _) in self.records()? {
            if self
                .metadata(&id)?
                .and_then(|metadata| metadata.conversation)
                .is_some_and(|source| source.previous_run.as_deref() == Some(run_id))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn reject_conversation_successor(
        &self,
        run_id: &str,
    ) -> Result<(), ApplicationError> {
        if self.has_conversation_successor(run_id)? {
            return Err(ApplicationError::Conflict(
                "Run is retained as the previous turn of a conversation".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn superseded_conversation_ancestor(
        &self,
        run_id: &str,
    ) -> Result<Option<String>, ApplicationError> {
        let mut ancestor = Some(run_id.to_owned());
        let mut seen = BTreeSet::new();
        while let Some(id) = ancestor {
            if !seen.insert(id.clone()) {
                return Err(ApplicationError::Invalid(
                    "Graph call parent lineage contains a cycle".into(),
                ));
            }
            if self.has_conversation_successor(&id)? {
                return Ok(Some(id));
            }
            // Only wait calls remain part of the caller's execution. A detach
            // boundary has independent authority and may outlive later messages.
            ancestor = self
                .metadata(&id)?
                .and_then(|metadata| metadata.graph_call)
                .filter(|source| source.mode == "wait")
                .map(|source| source.parent_run);
        }
        Ok(None)
    }

    pub(super) fn reject_superseded_conversation_execution(
        &self,
        run_id: &str,
    ) -> Result<(), ApplicationError> {
        if self.superseded_conversation_ancestor(run_id)?.is_some() {
            return Err(ApplicationError::Conflict(
                "Run belongs to a previous turn of a conversation".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn check_execution_scope(
        &self,
        metadata: &RunMetadata,
        active: &HashMap<String, ActiveRun>,
    ) -> Result<(), ApplicationError> {
        if metadata.graph_call.is_some() {
            self.reject_superseded_conversation_execution(&metadata.run_id)?;
            return Ok(());
        }
        let graph_path = Self::graph_identity(&metadata.bundle_source)?;
        if let Some(source) = metadata.conversation.as_ref() {
            self.reject_conversation_successor(&metadata.run_id)?;
            let chain = self.conversation_chain(source, &graph_path)?;
            if chain
                .first()
                .is_none_or(|(head, _)| head.run_id != metadata.run_id)
            {
                return Err(ApplicationError::Conflict(
                    "Run is not the current conversation turn".into(),
                ));
            }
            for (id, record) in self.records()? {
                if id == metadata.run_id || !is_unfinished(record.status) {
                    continue;
                }
                let Some(other) = self.metadata(&id)? else {
                    return Err(ApplicationError::Conflict(
                        "an unfinished Run has no immutable Graph identity".into(),
                    ));
                };
                if Self::graph_identity(&other.bundle_source)? == graph_path
                    && ((other.conversation.is_none() && other.session_call.is_none())
                        || other
                            .conversation
                            .as_ref()
                            .is_some_and(|other| other.session == source.session)
                            && (active.contains_key(&id)
                                || matches!(record.status, RunStatus::Ready | RunStatus::Running)))
                {
                    return Err(ApplicationError::Conflict(
                        "conversation execution scope is busy".into(),
                    ));
                }
            }
        } else {
            if active.values().any(|run| run.graph_path == graph_path) {
                return Err(ApplicationError::Conflict(
                    "this graph is already running".into(),
                ));
            }
            for (id, record) in self.records()? {
                if id == metadata.run_id || !is_unfinished(record.status) {
                    continue;
                }
                if let Some(other) = self.metadata(&id)?
                    && other.conversation.is_some()
                    && Self::graph_identity(&other.bundle_source)? == graph_path
                {
                    return Err(ApplicationError::Conflict(
                        "this graph has an unfinished conversation Run".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn admit_conversation(
        &self,
        request: ConversationAdmission,
        source: &Path,
        bundle: LoadedGraphBundle,
        graph_lease: Box<dyn RunLease>,
        attachments: &crate::channel_inputs::PreparedAttachments,
        channel: Option<ChannelRunSource>,
    ) -> Result<String, ApplicationError> {
        request.validate().map_err(ApplicationError::Invalid)?;
        let graph_path = Self::graph_identity(source)?;
        let mut active = self.active.lock().await;
        if let Some(run) =
            self.retry_conversation(&request, &graph_path, attachments, channel.as_ref())?
        {
            return Ok(run);
        }
        if self.metadata(&request.run)?.is_some() {
            return Err(ApplicationError::Conflict(
                "Run identity was saved without a complete admission; inspect the retained facts"
                    .into(),
            ));
        }
        let conversation = request.source();
        let chain = self.conversation_chain(&conversation, &graph_path)?;
        if chain.first().map(|(metadata, _)| metadata.run_id.as_str())
            != request.previous_run.as_deref()
        {
            return Err(ApplicationError::Conflict(
                "previous_run must name the latest Run of this Session".into(),
            ));
        }
        for (_, record) in &chain {
            self.ensure_predecessor_settled(record, &active)?;
        }
        for (id, record) in self.records()? {
            if !is_unfinished(record.status) {
                continue;
            }
            let metadata = self.metadata(&id)?.ok_or_else(|| {
                ApplicationError::Conflict(
                    "an unfinished Run has no immutable Graph identity; admission is quarantined"
                        .into(),
                )
            })?;
            if Self::graph_identity(&metadata.bundle_source)? == graph_path
                && metadata.conversation.is_none()
                && metadata.session_call.is_none()
            {
                return Err(ApplicationError::Conflict(
                    "this graph has an unfinished non-conversation Run".into(),
                ));
            }
        }
        if !bundle
            .snapshot
            .nodes
            .iter()
            .any(|node| node.id == request.reply_node)
        {
            return Err(ApplicationError::Invalid(
                "reply_node is not a node in this Graph".into(),
            ));
        }
        let mut record =
            GraphRunRecord::create_with_id(bundle.snapshot, request.input, request.run.clone())?;
        record.plugin_bindings = bundle
            .plugins
            .into_iter()
            .map(|plugin| (plugin.id.clone(), plugin))
            .collect();
        record.plugin_bindings_initialized = true;
        self.check_store()?;
        let mut metadata = RunMetadata::new(
            request.run.clone(),
            request.graph.clone(),
            record.graph_digest.clone(),
            source,
        )?;
        metadata.trigger_source = "channel".into();
        metadata.conversation = Some(conversation);
        metadata.channel = channel;
        metadata.attachments = attachments.manifest();
        let lease = self.store().acquire_lease(&request.run)?;
        crate::channel_inputs::freeze(&self.data_root, &metadata, attachments)
            .map_err(ApplicationError::Conflict)?;
        // Resolvers read this host-owned context; an orphan metadata file is never an accepted Run.
        metadata::save(&self.data_root, &metadata)?;
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
            .bind_local_inputs(&record, &request.graph)
            .map_err(ApplicationError::Invalid)?;
        create_durable_directory(&self.data_root.join("runs")).map_err(storage)?;
        self.store().save(&record)?;
        active.insert(
            request.run.clone(),
            ActiveRun {
                graph_path,
                control,
            },
        );
        drop(lease);
        drop(graph_lease);
        self.spawn(record, execution)?;
        Ok(request.run)
    }
}
