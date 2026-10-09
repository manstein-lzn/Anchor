use super::*;
use anchor_platform_session::{ChannelDeliveryRequest, SessionError, SessionStore, TurnStatus};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub(crate) const WECOM_REPLY_KIND: &str = "wecom_reply";
pub(crate) const WECOM_REPLY_KEY_PREFIX: &str = "channel-wecom-reply:";

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
    pub(crate) fn wecom_channel_delivery_request(
        metadata: &RunMetadata,
        record: &GraphRunRecord,
    ) -> Option<ChannelDeliveryRequest> {
        let channel = metadata.channel.as_ref()?;
        let conversation = metadata.conversation.as_ref()?;
        if record
            .input
            .get("channel")
            .and_then(|channel| channel.get("source"))
            .and_then(Value::as_str)
            != Some("wecom")
        {
            return None;
        }
        let submission = record
            .results
            .get(&conversation.reply_node)
            .and_then(|results| results.last())
            .map(|result| result.completion.submission.as_str())
            .filter(|submission| !submission.trim().is_empty())?;
        let content_sha256 = format!("{:x}", Sha256::digest(submission.as_bytes()));
        Some(ChannelDeliveryRequest {
            key: format!(
                "{WECOM_REPLY_KEY_PREFIX}{}:{}",
                channel.session, channel.inbound
            ),
            kind: WECOM_REPLY_KIND.into(),
            content_sha256,
        })
    }

    pub(crate) fn settle_channel_run(
        &self,
        run_id: &str,
        status: RunStatus,
    ) -> Result<(), ApplicationError> {
        let Some(metadata) = self.metadata(run_id)? else {
            return Ok(());
        };
        if let Some(source) = metadata.assistant.as_ref() {
            let turn_status = match status {
                RunStatus::Failed | RunStatus::Aborted => TurnStatus::Failed,
                RunStatus::Stopped => TurnStatus::Stopped,
                _ => return Ok(()),
            };
            let sessions = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
                .map_err(|error| ApplicationError::Storage(error.to_string()))?;
            if sessions
                .get_channel_assistant(&source.owner, &source.session)
                .map_err(|error| ApplicationError::Storage(error.to_string()))?
                .is_none_or(|assistant| assistant.run_id != run_id)
            {
                return Ok(());
            }
            for turn in sessions
                .list_turns(&source.owner, &source.session)
                .map_err(|error| ApplicationError::Storage(error.to_string()))?
            {
                if turn.status == TurnStatus::Running {
                    sessions
                        .finish_turn(
                            &source.owner,
                            &source.session,
                            &turn.id,
                            turn_status,
                            Some("assistant execution stopped"),
                        )
                        .map_err(|error| ApplicationError::Storage(error.to_string()))?;
                }
            }
            crate::assistant::signal(&source.session).notify_waiters();
            return Ok(());
        }
        let Some(channel) = metadata.channel.as_ref() else {
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
        let completed_record = if status == RunStatus::Completed {
            self.store().load(run_id)?
        } else {
            None
        };
        let sessions = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(|error| ApplicationError::Storage(error.to_string()))?;
        match sessions.get_channel_inbound(&channel.owner, &channel.session, &channel.inbound) {
            Ok(inbound) if inbound.relation.superseded_by_turn_id.is_some() => return Ok(()),
            Ok(_) | Err(SessionError::Missing) => {}
            Err(error) => return Err(ApplicationError::Storage(error.to_string())),
        }
        match sessions.finish_turn(
            &channel.owner,
            &channel.session,
            &channel.turn,
            turn_status,
            None,
        ) {
            Ok(_) | Err(SessionError::Missing) => {}
            Err(error) => return Err(ApplicationError::Storage(error.to_string())),
        };
        let Some(record) = completed_record else {
            return Ok(());
        };
        let Some(request) = Self::wecom_channel_delivery_request(&metadata, &record) else {
            return Ok(());
        };
        match sessions.admit_completed_channel_delivery(
            &channel.owner,
            &channel.session,
            &channel.turn,
            request,
        ) {
            Ok(_) | Err(SessionError::Missing) => Ok(()),
            Err(conflict @ SessionError::Conflict(_)) => {
                match sessions.get_channel_inbound(
                    &channel.owner,
                    &channel.session,
                    &channel.inbound,
                ) {
                    Ok(inbound) if inbound.relation.superseded_by_turn_id.is_some() => Ok(()),
                    Ok(_) => Err(ApplicationError::Storage(conflict.to_string())),
                    Err(error) => Err(ApplicationError::Storage(error.to_string())),
                }
            }
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
        let identity_key = identity.to_string_lossy().into_owned();
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
        // Deleted links keep their place through their tombstones: each one names
        // the predecessor the deleted Run itself held, so the surviving history
        // stays a single path whatever order links were removed in.
        let bridges = crate::run_deletions::list(&self.data_root)
            .map_err(ApplicationError::Storage)?
            .into_iter()
            .filter(|deletion| {
                deletion.explains(&identity_key, &source.session, &source.reply_node)
            })
            .collect::<Vec<_>>();
        let bridge = |id: &str| bridges.iter().find(|deletion| deletion.run_id() == id);
        let mut referenced = entries
            .iter()
            .filter_map(|(metadata, _)| metadata.conversation.as_ref()?.previous_run.clone())
            .collect::<BTreeSet<_>>();
        referenced.extend(
            bridges
                .iter()
                .filter_map(|deletion| deletion.previous_run().map(str::to_owned)),
        );
        let heads = entries
            .iter()
            .filter(|(metadata, _)| !referenced.contains(&metadata.run_id))
            .collect::<Vec<_>>();
        if heads.is_empty() {
            // Every surviving link is the predecessor of a deleted one, so the
            // Session has no live chain: the next accepted turn starts one.
            return Ok(Vec::new());
        }
        if heads.len() != 1 {
            return Err(ApplicationError::Conflict(
                "Session Run lineage is not a single chain".into(),
            ));
        }
        let mut visited = BTreeSet::new();
        let mut covered = BTreeSet::new();
        let mut at = Some(heads[0].0.run_id.clone());
        while let Some(id) = at {
            if !visited.insert(id.clone()) {
                return Err(ApplicationError::Conflict(
                    "Session Run lineage contains a cycle".into(),
                ));
            }
            let Some((metadata, _)) = entries.iter().find(|(metadata, _)| metadata.run_id == id)
            else {
                // A deleted Run is bridged by its tombstone; anything else is
                // retained corruption.
                let Some(deletion) = bridge(&id) else {
                    return Err(ApplicationError::Conflict(
                        "Session Run lineage has a missing predecessor".into(),
                    ));
                };
                at = deletion.previous_run().map(str::to_owned);
                continue;
            };
            covered.insert(id);
            at = metadata
                .conversation
                .as_ref()
                .and_then(|source| source.previous_run.clone());
        }
        if covered.len() != entries.len() {
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

    /// A channel Turn whose reply is still pending or of unknown outcome keeps
    /// its Run: the frozen reply is what a later verification is derived from,
    /// and deleting it would fake a settled delivery.
    pub(super) fn ensure_channel_delivery_settled(
        &self,
        metadata: &RunMetadata,
    ) -> Result<(), ApplicationError> {
        let Some(channel) = metadata.channel.as_ref() else {
            return Ok(());
        };
        let unsettled = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(|error| ApplicationError::Storage(error.to_string()))?
            .list_unfinished_channel_deliveries(&channel.owner, &channel.session)
            .map_err(|error| ApplicationError::Storage(error.to_string()))?
            .into_iter()
            .any(|delivery| delivery.turn_id == channel.turn);
        if unsettled {
            return Err(ApplicationError::Conflict(
                "Run has an unsettled channel delivery; settle it before deleting the record"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Decide whether one conversation Run may be deleted on its own.
    ///
    /// Conversation Runs form one chain per Session, and a later Run reads the
    /// Run it was handed over from while it executes: it inherits that Run's
    /// stable workspace scene, mounts its `/previous` snapshot, and takes its
    /// Goose invocation identity as the native predecessor. A Run is therefore
    /// only deletable once nothing that can still execute reads it:
    ///
    /// - it must not be the current assistant instance, which the Session still
    ///   points at (retire that instance first);
    /// - it must have no unsettled channel delivery;
    /// - every later Run that names it must either be unable to execute again
    ///   or have completed the first invocation of every node that reads it.
    ///
    /// Any link may go, in any order: its tombstone keeps the place it held, so
    /// the surviving lineage stays a single path.
    pub(super) fn ensure_conversation_delete_allowed(
        &self,
        record: &GraphRunRecord,
        metadata: &RunMetadata,
        active: &HashMap<String, ActiveRun>,
    ) -> Result<(), ApplicationError> {
        if metadata.conversation.is_none() {
            return Err(ApplicationError::Invalid(
                "Run has no conversation identity".into(),
            ));
        }
        if active.contains_key(&record.run_id) {
            return Err(ApplicationError::Conflict(
                "that Run is still running".into(),
            ));
        }
        self.ensure_current_assistant_is_replaced(metadata)?;
        for successor_id in self.conversation_successors(&record.run_id)? {
            let Some(successor_metadata) = self.metadata(&successor_id)? else {
                continue;
            };
            let successor = self.store().load(&successor_id)?;
            if self.successor_reads_predecessor(record, &successor_metadata, successor.as_ref())? {
                return Err(ApplicationError::Conflict(format!(
                    "Run is retained as the previous turn of a conversation: the newer Run `{successor_id}` still inherits from it on its first execution"
                )));
            }
        }
        Ok(())
    }

    /// Every Run whose immutable metadata names this Run as its predecessor.
    ///
    /// Metadata is listed instead of Run records because a handover that
    /// committed its binding and metadata but died before admitting the Run
    /// still retains the predecessor it needs to be admitted at all.
    fn conversation_successors(&self, run_id: &str) -> Result<Vec<String>, ApplicationError> {
        let mut successors = Vec::new();
        for id in crate::application::metadata::ids(&self.data_root)? {
            if id == run_id {
                continue;
            }
            if self
                .metadata(&id)?
                .and_then(|metadata| metadata.conversation)
                .is_some_and(|source| source.previous_run.as_deref() == Some(run_id))
            {
                successors.push(id);
            }
        }
        Ok(successors)
    }

    fn ensure_current_assistant_is_replaced(
        &self,
        metadata: &RunMetadata,
    ) -> Result<(), ApplicationError> {
        let Some(assistant) = metadata.assistant.as_ref() else {
            return Ok(());
        };
        let current = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(|error| ApplicationError::Storage(error.to_string()))?
            .get_channel_assistant(&assistant.owner, &assistant.session)
            .map_err(|error| ApplicationError::Storage(error.to_string()))?;
        if current.is_some_and(|current| current.run_id == metadata.run_id) {
            return Err(ApplicationError::Conflict(
                "this Run is the current assistant instance; retire it before deleting its record"
                    .into(),
            ));
        }
        Ok(())
    }

    /// True while this later Run can still read the Run it names as its
    /// predecessor. Every such read happens while the later Run executes the
    /// first invocation of a node, so a recorded first invocation is the
    /// durable evidence that the inheritance is complete.
    fn successor_reads_predecessor(
        &self,
        predecessor: &GraphRunRecord,
        successor_metadata: &RunMetadata,
        successor: Option<&GraphRunRecord>,
    ) -> Result<bool, ApplicationError> {
        let Some(successor) = successor else {
            // A handover that committed its binding and metadata but not its Run
            // record still needs this Run to be re-admitted, so its progress is
            // unknown and this Run stays retained.
            return Ok(true);
        };
        if matches!(
            successor.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
        ) {
            return Ok(false);
        }
        // A Run with its own successor can never resume or recover: the
        // conversation scope check refuses it, so its history is frozen.
        if self.has_conversation_successor(&successor.run_id)? {
            return Ok(false);
        }
        let mut reading = BTreeSet::new();
        for node in &predecessor.snapshot.nodes {
            let executed = predecessor.invocations.get(&node.id).copied().unwrap_or(0) > 0
                || predecessor
                    .results
                    .get(&node.id)
                    .is_some_and(|results| !results.is_empty());
            if executed {
                reading.insert(node.id.clone());
            }
        }
        if let Some(assistant) = successor_metadata.assistant.as_ref() {
            reading.insert(assistant.work_node.clone());
        }
        for node in reading {
            if !successor
                .snapshot
                .nodes
                .iter()
                .any(|candidate| candidate.id == node)
            {
                continue;
            }
            let injected = successor
                .results
                .get(&node)
                .is_some_and(|results| results.iter().any(|result| result.key.invocation == 1));
            if !injected {
                return Ok(true);
            }
        }
        Ok(false)
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn admit_conversation(
        &self,
        request: ConversationAdmission,
        source: &Path,
        bundle: LoadedGraphBundle,
        graph_lease: Box<dyn RunLease>,
        attachments: &crate::channel_inputs::PreparedAttachments,
        channel: Option<ChannelRunSource>,
        oauth_owner: String,
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
        metadata.oauth_owner = Some(oauth_owner);
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
