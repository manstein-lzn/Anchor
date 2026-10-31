use super::*;
use anchor_platform_session::SessionStore;
use std::collections::BTreeSet;

pub(crate) const WECOM_REPLY_KIND: &str = "wecom_reply";
pub(crate) const WECOM_REPLY_KEY_PREFIX: &str = "channel-wecom-reply:";

impl RunApplication {
    pub(super) fn conversation_chain(
        &self,
        source: &ConversationSource,
        graph_path: &Path,
    ) -> Result<Vec<(RunMetadata, run_access::RunView<'static>)>, ApplicationError> {
        let identity = crate::run_metadata::graph_identity(graph_path)?;
        let identity_key = identity.to_string_lossy().into_owned();
        let mut entries = Vec::new();
        for (id, record) in self.run_views()? {
            let Some(metadata) = self.metadata(&id)? else {
                continue;
            };
            let Some(conversation) = metadata.conversation.as_ref() else {
                continue;
            };
            if conversation.session != source.session {
                continue;
            }
            if crate::run_metadata::graph_identity(&metadata.bundle_source)? != identity
                || conversation.reply_node != source.reply_node
                || metadata.graph_digest != record.progress().graph_digest
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

    pub(crate) fn has_conversation_successor(
        &self,
        run_id: &str,
    ) -> Result<bool, ApplicationError> {
        // Metadata also reserves pending handovers; only an admitted successor
        // supersedes execution. Unrelated Run history is not part of this fact.
        for id in self.conversation_successors(run_id)? {
            if self.read_run(&id)?.is_some() {
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

    /// Decide whether one conversation Run may be deleted on its own.
    ///
    /// Conversation Runs form one chain per Session, and a later Run reads the
    /// Run it was handed over from while it executes: it inherits that Run's
    /// authorized stable workspace resource and takes its Goose invocation
    /// identity as the native predecessor. A Run is therefore
    /// only deletable once nothing that can still execute reads it:
    ///
    /// - it must not be the current assistant instance, which the Session still
    ///   points at (retire that instance first);
    /// - every later Run that names it must either be unable to execute again
    ///   or have completed the first invocation of every node that reads it.
    ///
    /// Any link may go, in any order: its tombstone keeps the place it held, so
    /// the surviving lineage stays a single path.
    pub(super) fn ensure_conversation_delete_allowed<'record>(
        &self,
        record: impl Into<run_access::RunView<'record>>,
        metadata: &RunMetadata,
        _admission: &(),
    ) -> Result<(), ApplicationError> {
        let record = record.into();
        if metadata.conversation.is_none() {
            return Err(ApplicationError::Invalid(
                "Run has no conversation identity".into(),
            ));
        }
        if record.active() {
            return Err(ApplicationError::Conflict(
                "that Run is still running".into(),
            ));
        }
        self.ensure_current_assistant_is_replaced(metadata)?;
        for successor_id in self.conversation_successors(record.progress().run_id)? {
            let Some(successor_metadata) = self.metadata(&successor_id)? else {
                continue;
            };
            let successor = self.read_run(&successor_id)?;
            if self.successor_reads_predecessor(&record, &successor_metadata, successor.as_ref())? {
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
        for id in crate::run_metadata::ids(&self.data_root)? {
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
        _admission: &(),
    ) -> Result<(), ApplicationError> {
        if metadata.graph_call.is_some() {
            self.reject_superseded_conversation_execution(&metadata.run_id)?;
            return Ok(());
        }
        let graph_path = crate::run_metadata::graph_identity(&metadata.bundle_source)?;
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
            for (id, record) in self.run_views()? {
                if id == metadata.run_id || record.is_settled() {
                    continue;
                }
                let worker = record.active();
                let Some(other) = self.metadata(&id)? else {
                    return Err(ApplicationError::Conflict(
                        "an unfinished Run has no immutable Graph identity".into(),
                    ));
                };
                if crate::run_metadata::graph_identity(&other.bundle_source)? == graph_path
                    && ((other.conversation.is_none() && other.session_call.is_none())
                        || other
                            .conversation
                            .as_ref()
                            .is_some_and(|other| other.session == source.session)
                            && (record.with_active(worker).occupies_conversation_execution()))
                {
                    return Err(ApplicationError::Conflict(
                        "conversation execution scope is busy".into(),
                    ));
                }
            }
        } else {
            for (id, record) in self.run_views()? {
                if id == metadata.run_id || record.is_settled() {
                    continue;
                }
                if let Some(other) = self.metadata(&id)?
                    && other.conversation.is_some()
                    && crate::run_metadata::graph_identity(&other.bundle_source)? == graph_path
                {
                    return Err(ApplicationError::Conflict(
                        "this graph has an unfinished conversation Run".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}
