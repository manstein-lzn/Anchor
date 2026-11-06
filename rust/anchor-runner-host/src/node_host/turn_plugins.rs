//! One freeze point for persistent-assistant Plugin and network permissions.
use super::*;
use crate::assistant_plugins::{self, TurnPluginPolicy};

impl HostIoResolver {
    pub(super) async fn freeze_assistant_plugins(
        &self,
        key: &InvocationKey,
        turn: &str,
        use_desired_graph: bool,
        cancellation: anchor_runtime::Cancellation,
    ) -> Result<(), String> {
        let root = self.local_inputs.state_root();
        let metadata = crate::run_metadata::load(root, &key.run_id)
            .map_err(|error| format!("assistant identity unavailable: {error:?}"))?
            .ok_or("assistant identity is missing")?;
        let source = metadata
            .assistant
            .as_ref()
            .ok_or("assistant binding is missing")?;
        if let Some(policy) = assistant_plugins::load(root, &key.run_id, &source.session, turn)? {
            if policy.node != source.work_node {
                return Err("assistant Turn Plugin node identity changed".into());
            }
            return assistant_plugins::mark_initialized(root, &key.run_id);
        }
        if !use_desired_graph && assistant_plugins::initialized(root, &key.run_id)? {
            return Err("assistant Turn Plugin policy is missing".into());
        }
        // Lock order matches publication: Graph, then Library. A save that wins
        // this lease is wholly visible to the next Turn; an in-flight Turn keeps
        // its immutable policy even when the bundle changes afterwards.
        let _graph = loop {
            if cancellation.load(Ordering::Acquire) {
                return Err("Plugin policy preparation cancelled".into());
            }
            match assistant_plugins::graph_lease(root, &metadata.bundle_source) {
                Ok(lease) => break lease,
                Err(GraphError::RunBusy(_)) => tokio::time::sleep(Duration::from_millis(20)).await,
                Err(error) => return Err(error.to_string()),
            }
        };
        if let Some(policy) = assistant_plugins::load(root, &key.run_id, &source.session, turn)? {
            if policy.node != source.work_node {
                return Err("assistant Turn Plugin node identity changed".into());
            }
            return assistant_plugins::mark_initialized(root, &key.run_id);
        }
        let _library = match &self.library_root {
            Some(library) => Some(
                crate::plugin_library::publication_lease(
                    library,
                    &crate::dependency_provision::DependencyAccess {
                        network: false,
                        cancellation,
                    },
                    "assistant Turn permission freeze",
                )
                .await
                .map_err(|error| format!("Plugin policy catalog unavailable: {error:?}"))?,
            ),
            None => None,
        };
        let admitted = self
            .load_run(&key.run_id)
            .map_err(|error| error.to_string())?
            .ok_or("assistant Run is missing")?;
        if admitted.graph_digest != metadata.graph_digest
            || admitted.graph_digest != key.graph_digest
            || !assistant_plugins::invocation_authority(root, &admitted)?
        {
            return Err("assistant Run authorization identity changed".into());
        }
        // Host restart can transfer an already claimed Turn to a new Run. The
        // verified predecessor's frozen grants follow that same Turn, even if a
        // newer Graph was published while it was interrupted.
        if key.invocation == 1
            && metadata
                .conversation
                .as_ref()
                .is_some_and(|conversation| conversation.previous_run.is_some())
            && let Some(mut policy) = self.inherited_plugin_policy(&metadata, &admitted, turn)?
        {
            policy.run = key.run_id.clone();
            crate::assistant::save_immutable(
                &assistant_plugins::turn_path(root, &key.run_id, &source.session, turn),
                &policy,
            )?;
            return assistant_plugins::mark_initialized(root, &key.run_id);
        }
        let legacy_path = plugin_mounts::snapshot_path(root, &source.session, turn);
        let legacy = plugin_mounts::read_snapshot_strict(&legacy_path, &source.session, turn)?;
        // A pre-upgrade Turn that already froze its Session policy must retain
        // the admitted Graph grants. Newly claimed input can read current desired.
        let use_desired_graph = use_desired_graph && legacy.is_none();
        let (snapshot, authority, task_grants) = if use_desired_graph {
            let current = anchor_graph_host::FileGraphBundleLoader::new(&metadata.bundle_source)
                .with_library_root(self.plugin_library_root()?)
                .load()
                .map_err(|error| format!("assistant desired Graph unavailable: {error}"))?
                .snapshot;
            if !assistant_plugins::compatible(&admitted.snapshot, &current, &source.work_node) {
                return Err("assistant_graph_restart_required: Graph structure changed; stop and retire the assistant to apply it".into());
            }
            let digest = current.digest().map_err(|error| error.to_string())?;
            let (authority, task_grants) = match assistant_plugins::publication(root, &metadata.bundle_source, &digest)? {
                Some(publication) => (publication.bindings.clone(), Some(publication.task_grants())),
                None if digest == admitted.graph_digest => (admitted.plugin_bindings.clone(), metadata.task_grants.clone()),
                None => return Err("Graph Plugin permissions have no operator authorization; save the Graph from the operator interface".into()),
            };
            (current, authority, task_grants)
        } else {
            (
                admitted.snapshot.clone(),
                admitted.plugin_bindings.clone(),
                metadata.task_grants.clone(),
            )
        };
        let mounts = if let Some(legacy) = legacy {
            legacy
        } else {
            let (switches, allowed) = self.mounts.session_policy(&source.session)?;
            let mut mounts = MountSnapshot::new(&source.session, turn, &switches);
            mounts.network_allowed = allowed;
            mounts
        };
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.id == source.work_node)
            .ok_or("assistant work node is missing")?;
        let declared = crate::plugin_library::mount_declarations(
            &node.plugins,
            Some(metadata.graph.as_str()),
            self.pilot_graph.as_deref(),
        );
        let catalog = crate::plugin_library::Catalog::new(self.plugin_library_root()?);
        let mut installed = BTreeMap::new();
        for id in declared
            .iter()
            .map(|(id, _, _)| *id)
            .chain(mounts.mounts.keys().map(String::as_str))
        {
            installed.insert(
                id.to_owned(),
                self.tools.resolve_fixture_plugins(&[id.to_owned()]).is_ok()
                    || catalog.definition(id).is_ok(),
            );
        }
        let resolved = plugin_mounts::resolve(Some(&mounts), &mounts.mounts, &declared, &installed);
        // A desired Graph Plugin cannot disappear silently between save and use.
        for reference in &node.plugins {
            let enabled = mounts
                .mounts
                .get(&reference.id)
                .copied()
                .unwrap_or(reference.default.unwrap_or(true));
            if enabled && !installed.get(&reference.id).copied().unwrap_or(false) {
                return Err(format!(
                    "Plugin `{}` selected by the Graph is not available in this Library",
                    reference.id
                ));
            }
        }
        let mut bindings =
            self.resolve_plugins(&resolved.mounts.keys().cloned().collect::<Vec<_>>())?;
        for binding in &bindings {
            if node
                .plugins
                .iter()
                .any(|reference| reference.id == binding.id)
            {
                let baseline = authority
                    .get(&binding.id)
                    .ok_or_else(|| format!("Plugin `{}` has no Graph authorization", binding.id))?;
                if !baseline.same_authorization(binding) {
                    return Err(format!(
                        "Plugin `{}` Host authorization changed since Graph permission approval",
                        binding.id
                    ));
                }
            }
        }
        let mut reasons = resolved.reasons;
        let agent_network = node
            .agent
            .as_ref()
            .and_then(|name| snapshot.agents.get(name))
            .is_some_and(|agent| agent.network);
        let network = crate::plugin_network::apply(
            self.plugin_library_root()?,
            &mut bindings,
            agent_network && mounts.network_allowed,
            &mut reasons,
        )?;
        // Auto-derived network is an explicit, frozen grant too. Initial Runs
        // predate receipts; their host admission already authorized networking.
        if network && snapshot.digest().map_err(|error| error.to_string())? != admitted.graph_digest
        {
            let current = bindings
                .iter()
                .find(|binding| binding.id == crate::plugin_network::NETWORK_PLUGIN)
                .ok_or("network authorization binding is missing")?;
            if !authority
                .get(&current.id)
                .is_some_and(|baseline| baseline.same_authorization(current))
            {
                return Err(
                    "network Plugin Host authorization changed since Graph permission approval"
                        .into(),
                );
            }
        }
        let policy = TurnPluginPolicy {
            format: 1,
            run: key.run_id.clone(),
            node: source.work_node.clone(),
            graph_digest: snapshot.digest().map_err(|error| error.to_string())?,
            mounts,
            declarations: node.plugins.clone(),
            bindings: bindings
                .into_iter()
                .map(|binding| (binding.id.clone(), binding))
                .collect(),
            network,
            reasons,
            by_declaration: resolved.by_declaration,
            by_switch: resolved.by_switch,
            task_grants,
        };
        crate::assistant::save_immutable(
            &assistant_plugins::turn_path(root, &key.run_id, &source.session, turn),
            &policy,
        )?;
        assistant_plugins::mark_initialized(root, &key.run_id)
    }

    /// Only Runs durably associated with this Turn can supply its frozen policy.
    /// Conversation history can cross an explicit instance replacement and does
    /// not establish that an older Run ever claimed this input.
    fn inherited_plugin_policy(
        &self,
        metadata: &crate::run_metadata::RunMetadata,
        current: &GraphRunRecord,
        turn: &str,
    ) -> Result<Option<TurnPluginPolicy>, String> {
        let root = self.local_inputs.state_root();
        let source = metadata
            .assistant
            .as_ref()
            .ok_or("assistant binding is missing")?;
        let sessions = crate::assistant::sessions(root)?;
        let claimed = sessions
            .get_turn(&source.owner, &source.session, turn)
            .map_err(|error| format!("assistant Turn association unavailable: {error}"))?;
        let position = claimed
            .runs
            .iter()
            .position(|run| run == &metadata.run_id)
            .ok_or("assistant Turn is not associated with this Run")?;
        // A new message first claimed by this instance has no permission policy
        // to inherit, regardless of the depth or shape of conversation history.
        if position == 0 {
            return Ok(None);
        }
        let work_key = InvocationKey {
            run_id: metadata.run_id.clone(),
            graph_digest: metadata.graph_digest.clone(),
            node_id: source.work_node.clone(),
            invocation: 1,
        };
        let mut previous = crate::assistant::handover_predecessor(root, &work_key)?
            .ok_or("assistant same-Turn handover identity is missing")?
            .run_id;
        let mut successor = metadata.run_id.clone();
        let mut ancestors = claimed.runs[..position].iter().rev().peekable();
        while let Some(expected) = ancestors.next() {
            if previous != *expected {
                return Err("assistant Plugin handover differs from Turn associations".into());
            }
            let certified = sessions
                .channel_assistant_predecessor(&source.owner, &source.session, &successor)
                .map_err(|error| error.to_string())?;
            if certified.as_ref().is_none_or(|prior| {
                prior.run_id != previous
                    || prior.wait_node != source.wait_node
                    || prior.work_node != source.work_node
                    || prior.reply_node != source.reply_node
            }) {
                return Err("assistant Plugin handover predecessor changed".into());
            }
            let prior = crate::run_metadata::load(root, &previous)
                .map_err(|error| format!("assistant Plugin predecessor unavailable: {error:?}"))?
                .ok_or("assistant Plugin predecessor metadata is missing")?;
            let prior_source = prior
                .assistant
                .as_ref()
                .ok_or("assistant Plugin predecessor has no binding")?;
            let prior_record = self
                .load_run(&previous)
                .map_err(|error| error.to_string())?
                .ok_or("assistant Plugin predecessor Run is missing")?;
            if prior_source.owner != source.owner
                || prior_source.session != source.session
                || prior_source.wait_node != source.wait_node
                || prior_source.work_node != source.work_node
                || prior_source.reply_node != source.reply_node
                || prior.bundle_source != metadata.bundle_source
                || prior.graph_digest != prior_record.graph_digest
                || prior.conversation.as_ref().is_none_or(|conversation| {
                    conversation.session != source.session
                        || conversation.reply_node != source.work_node
                })
                || !assistant_plugins::compatible(
                    &prior_record.snapshot,
                    &current.snapshot,
                    &source.work_node,
                )
            {
                return Err("assistant Plugin predecessor belongs to another instance".into());
            }
            if let Some(policy) = assistant_plugins::load(root, &previous, &source.session, turn)? {
                if policy.node != source.work_node {
                    return Err("assistant Plugin handover node changed".into());
                }
                return Ok(Some(policy));
            }
            let committed = prior_record
                .results
                .get(&source.wait_node)
                .is_some_and(|results| {
                    results.iter().any(|result| {
                        result
                            .completion
                            .output
                            .get("turn")
                            .and_then(serde_json::Value::as_str)
                            == Some(turn)
                    })
                });
            if committed && assistant_plugins::initialized(root, &previous)? {
                return Err("assistant predecessor Turn Plugin policy is missing".into());
            }
            // The first Run associated with this Turn may have crashed before
            // freezing it. No older conversation Turn contributes permission.
            if ancestors.peek().is_none() {
                return Ok(None);
            }
            let older = prior
                .conversation
                .as_ref()
                .and_then(|conversation| conversation.previous_run.clone())
                .ok_or("assistant same-Turn predecessor metadata is missing")?;
            successor = previous;
            previous = older;
        }
        Ok(None)
    }

    pub(super) fn assistant_mounted_plugins(
        &self,
        request: &NodeExecutionRequest,
    ) -> Result<Option<MountedPlugins>, String> {
        let root = self.local_inputs.state_root();
        let Some(source) = crate::assistant::source(root, &request.key)? else {
            return Ok(None);
        };
        if request.key.node_id != source.work_node {
            return Ok(Some(MountedPlugins {
                bindings: Vec::new(),
                network: false,
                reasons: Vec::new(),
                session: Some(source.session),
                snapshot: true,
                snapshot_unreadable: false,
                by_declaration: Vec::new(),
                by_switch: Vec::new(),
            }));
        }
        let binding = crate::assistant::binding(root, &request.key)?
            .ok_or("assistant invocation has no Turn binding")?;
        let policy =
            assistant_plugins::load(root, &request.key.run_id, &source.session, &binding.turn)?
                .ok_or("assistant Turn Plugin policy is missing")?;
        if policy.node != request.key.node_id {
            return Err("assistant Turn Plugin node identity changed".into());
        }
        let bindings =
            self.resolve_plugins(&policy.bindings.keys().cloned().collect::<Vec<_>>())?;
        Self::validate_frozen_plugins(&policy, &bindings)?;
        Ok(Some(MountedPlugins {
            bindings,
            network: policy.network,
            reasons: policy.reasons,
            session: Some(source.session),
            snapshot: true,
            snapshot_unreadable: false,
            by_declaration: policy.by_declaration,
            by_switch: policy.by_switch,
        }))
    }

    pub(super) fn validate_frozen_plugins(
        policy: &TurnPluginPolicy,
        bindings: &[PluginBinding],
    ) -> Result<(), String> {
        if bindings.len() != policy.bindings.len()
            || bindings.iter().any(|binding| {
                !policy
                    .bindings
                    .get(&binding.id)
                    .is_some_and(|frozen| frozen.same_authorization(binding))
            })
        {
            return Err("Plugin Host authorization changed since this Turn was frozen".into());
        }
        Ok(())
    }
}
