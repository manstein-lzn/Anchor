//! Turn-frozen Plugin selection and execution observations.
use super::*;

/// The reserved evidence key for a call's Plugin *mounting*, next to the
/// per-Plugin view digests: where the switches came from, and why a Plugin the
/// call named is not mounted. It is deliberately not a Plugin id.
pub(super) const MOUNT_EVIDENCE_KEY: &str = "plugin-mounts";

/// The Plugin set one node call mounts, and the Session facts behind it.
#[derive(Debug)]
pub(super) struct MountedPlugins {
    pub(super) bindings: Vec<PluginBinding>,
    pub(super) network: bool,
    /// One line per Plugin the call named that is *not* mounted, and why.
    pub(super) reasons: Vec<String>,
    /// The Session whose switches applied, when the Run belongs to one.
    pub(super) session: Option<String>,
    /// Whether a Turn snapshot pinned those switches.
    pub(super) snapshot: bool,
    /// Plugins the Graph's declaration defaulted on (no click, no audit event).
    pub(super) by_declaration: Vec<String>,
    /// Plugins a human's Session switch turned on (the audit event's subject).
    pub(super) by_switch: Vec<String>,
    /// A snapshot file exists but could not be read: the switches are live and the
    /// observation says so, rather than claiming they were frozen.
    pub(super) snapshot_unreadable: bool,
}

/// The deployment's in-process fake fixture. It is a binding the host enabled by
/// configuration, not a Plugin the Library has on disk.
pub(crate) fn is_fake_fixture(binding: &PluginBinding) -> bool {
    tool_host::is_fake_fixture(binding)
}

/// One Turn's Session switches, frozen when the Turn starts.
///
/// Run-independent on purpose. A node call reads them from its Run; the Pilot's
/// own Turn has no Run at all, and it must freeze exactly the same way: the first
/// caller of a Turn writes the snapshot, and every later caller — including a
/// retry — reads it, so a click made mid-Turn cannot change what that Turn uses.
pub(crate) struct FrozenSwitches {
    /// The switches this Turn applies: the snapshot's, once it exists.
    pub(crate) switches: BTreeMap<String, bool>,
    /// The snapshot itself, which is what `resolve` reads.
    pub(crate) snapshot: MountSnapshot,
    /// A snapshot file exists but could not be read: the switches above are the
    /// Session's *current* ones and the call must say so.
    pub(crate) snapshot_unreadable: bool,
}

/// Freeze (or read) one Turn's Session switches.
pub(crate) fn freeze_session_switches(
    state_root: &Path,
    session: &str,
    turn: &str,
    source: &dyn PluginMountSource,
) -> Result<FrozenSwitches, String> {
    // The Turn's snapshot is written by its **first** caller, whatever that call
    // needs: a Turn whose first node has no Plugins must still pin the switches it
    // started with, or a click made mid-Turn would take effect immediately.
    let path = plugin_mounts::snapshot_path(state_root, session, turn);
    let existing = plugin_mounts::read_snapshot_strict(&path, session, turn)?;
    let snapshot = match existing {
        Some(snapshot) => snapshot,
        None => {
            let (switches, network_allowed) = source.session_policy(session)?;
            let mut candidate = MountSnapshot::new(session, turn, &switches);
            candidate.network_allowed = network_allowed;
            plugin_mounts::write_snapshot(&path, &candidate)
                .map_err(|error| format!("Turn Plugin mount snapshot is unavailable: {error}"))?;
            // Another attempt of this Turn may have won the race; the snapshot on
            // disk is the one this Turn uses.
            plugin_mounts::read_snapshot_strict(&path, session, turn)?
                .ok_or("Turn Plugin mount snapshot disappeared after publication")?
        }
    };
    Ok(FrozenSwitches {
        switches: snapshot.mounts.clone(),
        snapshot,
        snapshot_unreadable: false,
    })
}

/// The Plugins one frozen Session turn mounts, without a Run and without a Graph.
///
/// The Pilot's own Turn has neither: a Plugin reaches it because a human mounted
/// it in the Session. The rules are the node call's rules — the Library must have
/// it (or the deployment's fake fixture must be enabled), and the view its own
/// declaration maps to must be usable. Nothing here builds, repairs or installs
/// anything, and `reasons` says why a switched-on Plugin is absent.
/// The caller holds the Library publication lease while selecting mounts.
pub(crate) fn session_plugin_mounts(
    library_root: &Path,
    fixture: &tool_host::PluginToolHost,
    switches: &BTreeMap<String, bool>,
) -> Result<(Vec<PluginBinding>, Vec<String>), String> {
    let catalog = crate::plugin_library::Catalog::new(library_root);
    let mut installed = BTreeMap::new();
    let mut definitions = BTreeMap::new();
    for id in switches.keys() {
        let fake = fixture
            .resolve_fixture_plugins(std::slice::from_ref(id))
            .is_ok();
        let definition = if fake {
            None
        } else {
            catalog.definition(id).ok()
        };
        installed.insert(id.clone(), fake || definition.is_some());
        if let Some(definition) = definition {
            definitions.insert(id.clone(), definition);
        }
    }
    let resolved = plugin_mounts::resolve(None, switches, &[], &installed);
    let mut reasons = resolved.reasons;
    let mut bindings = Vec::new();
    for id in resolved.mounts.keys() {
        if let Ok(mut fixture_bindings) = fixture.resolve_fixture_plugins(std::slice::from_ref(id))
        {
            bindings.append(&mut fixture_bindings);
            continue;
        }
        let definition = definitions
            .get(id)
            .ok_or_else(|| format!("Plugin `{id}` is not available in this Library"))?;
        // A Plugin whose own declaration's view is not usable does not appear: a
        // mount never becomes a view build.
        if let Err(unusable) =
            crate::plugin_view::PreparedPlugin::from_definition(library_root, definition)
        {
            reasons.push(unusable.to_string());
            continue;
        }
        bindings.push(definition.binding());
    }
    Ok((bindings, reasons))
}

impl HostIoResolver {
    /// Remember what this invocation observed about its Plugins, for the Run's
    /// own record: which view each one resolved, and where its mount switches
    /// came from together with why a Plugin the call named is not mounted.
    pub(super) fn remember_plugin_observation(
        &self,
        key: &InvocationKey,
        views: BTreeMap<String, String>,
        digests: BTreeMap<String, String>,
        mounted: &MountedPlugins,
        configuration_revisions: &BTreeMap<String, u64>,
    ) {
        let mut observed = views;
        for (plugin, revision) in configuration_revisions {
            observed.insert(
                format!("configuration:{plugin}"),
                format!("revision={revision}"),
            );
        }
        if let Some(session) = &mounted.session {
            let source = if mounted.snapshot {
                "this Turn's snapshot"
            } else if mounted.snapshot_unreadable {
                // A snapshot exists but could not be read: the switches are live,
                // and the observation says so instead of claiming they were frozen.
                "no snapshot (unreadable); the Session's current switches"
            } else {
                "the Session's current switches"
            };
            let mut value = format!("source={source}; session={session}");
            value.push_str(&format!("; network={}", mounted.network));
            if !mounted.by_declaration.is_empty() {
                value.push_str(&format!(
                    "; by-declaration=[{}]",
                    mounted.by_declaration.join(",")
                ));
            }
            if !mounted.by_switch.is_empty() {
                value.push_str(&format!("; by-switch=[{}]", mounted.by_switch.join(",")));
            }
            if !mounted.reasons.is_empty() {
                value.push_str("; ");
                value.push_str(&mounted.reasons.join("; "));
            }
            observed.insert(MOUNT_EVIDENCE_KEY.to_owned(), value);
        }
        if observed.is_empty() && digests.is_empty() {
            return;
        }
        if !observed.is_empty()
            && let Ok(mut views) = self.observed_views.lock()
        {
            views.insert(key.durable_key(), observed);
        }
        if !digests.is_empty()
            && let Ok(mut observations) = self.observed_plugin_digests.lock()
        {
            observations.insert(key.durable_key(), digests);
        }
    }

    /// Take the observation for one invocation (reading it consumes it, so a long
    /// lived host does not accumulate one entry per node call).
    pub(crate) fn observed_plugin_views(&self, key: &InvocationKey) -> BTreeMap<String, String> {
        self.observed_views
            .lock()
            .ok()
            .and_then(|mut observed| observed.remove(&key.durable_key()))
            .unwrap_or_default()
    }

    pub(crate) fn observed_plugin_digests(&self, key: &InvocationKey) -> BTreeMap<String, String> {
        self.observed_plugin_digests
            .lock()
            .ok()
            .and_then(|mut observed| observed.remove(&key.durable_key()))
            .unwrap_or_default()
    }

    /// Recheck the admitted authority before exposing a prepared current binding.
    pub(super) fn validate_invocation_plugins(
        &self,
        admitted: &GraphRunRecord,
        request: &NodeExecutionRequest,
        bindings: &[PluginBinding],
    ) -> Result<(), String> {
        if let Some(source) =
            crate::assistant::source(self.local_inputs.state_root(), &request.key)?
        {
            let binding = crate::assistant::binding(self.local_inputs.state_root(), &request.key)?
                .ok_or("assistant invocation has no Turn binding")?;
            let policy = crate::assistant_plugins::load(
                self.local_inputs.state_root(),
                &request.key.run_id,
                &source.session,
                &binding.turn,
            )?
            .ok_or("assistant Turn Plugin policy is missing")?;
            return Self::validate_frozen_plugins(&policy, bindings);
        }
        for binding in bindings {
            let declared = request
                .plugins
                .iter()
                .any(|declared| declared.id == binding.id);
            match (admitted.plugin_bindings.get(&binding.id), declared) {
                // A Run freezes Host authority, while executable bytes and its
                // dependency view follow the current installed Plugin.
                (Some(frozen), _) if !frozen.same_authorization(binding) => {
                    return Err(format!(
                        "Plugin `{}` Host authorization changed since this Graph Run was admitted",
                        binding.id,
                    ));
                }
                // A Graph declaration with no admitted binding is corrupt.
                (None, true) => {
                    return Err(format!(
                        "Plugin `{}` is not bound by the admitted Run",
                        binding.id,
                    ));
                }
                // A human's frozen Session mount authorizes an additional Plugin.
                _ => {}
            }
        }
        Ok(())
    }

    /// The channel identity of the one message this assistant invocation
    /// answers, read from the admitted input rather than anything the model
    /// produced. `None` outside a channel Turn, or when the Turn also folds in
    /// messages from another conversation member.
    pub(super) fn attested_channel(
        &self,
        key: &InvocationKey,
    ) -> Result<Option<anchor_platform_session::ChannelIdentity>, String> {
        let root = self.local_inputs.state_root();
        let Some(input) = crate::assistant::input(root, key)? else {
            return Ok(None);
        };
        let Some(input) = input.user() else {
            return Ok(None);
        };
        let pending = crate::assistant::pending_inputs(root, key)?;
        Ok(crate::assistant::single_identity(
            &input.request.identity,
            pending.iter().map(|other| &other.request.identity),
        ))
    }

    pub(super) fn oauth_owner_for_run(&self, run_id: &str) -> Result<Option<String>, String> {
        crate::run_metadata::load(self.local_inputs.state_root(), run_id)
            .map(|metadata| metadata.and_then(|metadata| metadata.oauth_owner))
            .map_err(|error| format!("OAuth Run owner is unavailable: {error:?}"))
    }

    /// The Session and Turn this Run's mount switches come from.
    ///
    /// The immutable Run metadata is the only place that says which Session a Run
    /// belongs to: a pilot Run records Session and Turn, a persistent assistant
    /// records the Session and takes its Turn from the assistant invocation's
    /// own immutable binding, and a plain conversation
    /// records the Session only. When no Turn id is recorded, this Run *is* the
    /// unit whose switches are frozen, so it names itself. A Run in no Session
    /// mounts only what its Graph declares.
    pub(super) fn session_turn(
        &self,
        key: &InvocationKey,
    ) -> Result<Option<(String, String)>, String> {
        let root = self.local_inputs.state_root();
        let Some(metadata) = crate::run_metadata::load(root, &key.run_id)
            .map_err(|error| format!("Run Session metadata unavailable: {error:?}"))?
        else {
            return Ok(None);
        };
        if let Some(source) = &metadata.pilot {
            return Ok(Some((source.session.clone(), source.turn.clone())));
        }
        if let Some(source) = &metadata.assistant {
            let turn = crate::assistant::binding(root, key)?
                .map(|binding| binding.turn)
                .unwrap_or_else(|| key.run_id.clone());
            return Ok(Some((source.session.clone(), turn)));
        }
        if let Some(call) = &metadata.session_call {
            return Ok(Some((call.context.session.clone(), key.run_id.clone())));
        }
        if let Some(source) = &metadata.conversation {
            return Ok(Some((source.session.clone(), key.run_id.clone())));
        }
        Ok(None)
    }

    /// Session switches can add Plugins even when the Graph declares none.
    /// Calls with neither source do not need access to the Library catalog.
    pub(super) fn mount_catalog_root(
        &self,
        request: &NodeExecutionRequest,
    ) -> Result<Option<&Path>, String> {
        match self.library_root.as_deref() {
            Some(root)
                if !request.plugins.is_empty() || self.session_turn(&request.key)?.is_some() =>
            {
                Ok(Some(root))
            }
            _ => Ok(None),
        }
    }

    /// The Plugins this node call actually mounts, and why the others are off.
    ///
    /// The Graph's declaration is a *default*: a required Plugin is always
    /// mounted, an optional one follows the Session's switches, and a Plugin the
    /// human mounted in the Session is mounted even though no Graph named it. A
    /// Plugin the Library does not have is not mounted, and `reasons` says so.
    ///
    /// The switches are frozen per Turn: the first call of a Turn writes the
    /// snapshot, and a retry of that Turn reuses it instead of reading a Session
    /// that changed in between.
    ///
    /// `required`/`default` come from the Graph's own declaration, read back out
    /// of the admitted Run by node: `required: true` is always mounted and cannot
    /// be switched off, an optional one follows "the Session's explicit value,
    /// else its `default`, else on". A bare id in an older Graph is required, which
    /// is what a declaration has always meant.
    pub(super) fn mounted_plugins(
        &self,
        request: &NodeExecutionRequest,
    ) -> Result<MountedPlugins, String> {
        let _guard = self
            .mount_catalog_root(request)?
            .map(|root| {
                crate::plugin_library::PluginLibrary::new(root)
                    .catalog_read_guard()
                    .map_err(|error| format!("Plugin mount catalog is unavailable: {error}"))
            })
            .transpose()?;
        self.mounted_plugins_unlocked(request)
    }

    /// The async tool path supplies its cancellable publication lease; Op.run
    /// uses the nonblocking read-guard wrapper above.
    pub(super) fn mounted_plugins_unlocked(
        &self,
        request: &NodeExecutionRequest,
    ) -> Result<MountedPlugins, String> {
        if let Some(mounted) = self.assistant_mounted_plugins(request)? {
            return Ok(mounted);
        }
        let key = &request.key;
        let admitted = self
            .load_run(&key.run_id)
            .map_err(|error| format!("load admitted Run Plugin bindings: {error}"))?
            .ok_or_else(|| {
                format!(
                    "Plugin mount resolution requires a durable admitted Run `{}`",
                    key.run_id
                )
            })?;
        let session_turn = self.session_turn(key)?;
        let frozen = match &session_turn {
            Some((session, turn)) => Some(freeze_session_switches(
                self.local_inputs.state_root(),
                session,
                turn,
                self.mounts.as_ref(),
            )?),
            None => None,
        };
        let switches = frozen
            .as_ref()
            .map(|frozen| frozen.switches.clone())
            .unwrap_or_default();
        let snapshot_unreadable = frozen
            .as_ref()
            .is_some_and(|frozen| frozen.snapshot_unreadable);
        let snapshot = frozen.as_ref().map(|frozen| &frozen.snapshot);
        let node = admitted
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == key.node_id)
            .ok_or("admitted Plugin policy node is missing")?;
        let mode = admitted.snapshot.node_network_mode(node);
        if request.plugins.is_empty()
            && switches.is_empty()
            && mode != anchor_runtime::graph::NetworkMode::Enabled
        {
            return Ok(MountedPlugins {
                bindings: Vec::new(),
                network: false,
                reasons: Vec::new(),
                session: session_turn.map(|(session, _)| session),
                snapshot: snapshot.is_some(),
                snapshot_unreadable,
                by_declaration: Vec::new(),
                by_switch: Vec::new(),
            });
        }
        let mut reasons = Vec::new();
        let catalog = crate::plugin_library::Catalog::new(self.plugin_library_root()?);
        // `declared` only scopes the reasons; the mounted set may name a Plugin
        // the Graph never declared, which is exactly the point.
        // A `required: true` declaration is an authorization, so only the
        // deployment's own pilot Graph may hand it out: that is the Graph an
        // operator authors and reviews. Any *other* Graph -- one an assistant
        // created or edited in this Session -- has its `required` downgraded to
        // optional, so the assistant cannot grant itself a Plugin the human cannot
        // switch off. It still mounts when the Session's switches say so.
        // The one authority the API's graph routes and the Session view also read.
        let declaring_graph =
            crate::run_metadata::load(self.local_inputs.state_root(), &key.run_id)
                .ok()
                .flatten()
                .map(|metadata| metadata.graph);
        let required_is_authoritative = crate::plugin_library::pilot_authority(
            declaring_graph.as_deref(),
            self.pilot_graph.as_deref(),
        );
        if !required_is_authoritative
            && admitted
                .snapshot
                .nodes
                .iter()
                .any(|node| node.plugins.iter().any(|reference| reference.required))
        {
            reasons.push(
                "a `required: true` declaration from a Graph that is not the deployment's pilot Graph is treated as optional"
                    .to_owned(),
            );
        }
        let declared = admitted
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == key.node_id)
            .map(|node| {
                crate::plugin_library::mount_declarations(
                    &node.plugins,
                    declaring_graph.as_deref(),
                    self.pilot_graph.as_deref(),
                )
            })
            .unwrap_or_default();
        let mut installed = BTreeMap::new();
        for id in declared
            .iter()
            .map(|(id, _, _)| *id)
            .chain(switches.keys().map(String::as_str))
        {
            if installed.contains_key(id) {
                continue;
            }
            let fake = self.tools.resolve_fixture_plugins(&[id.to_owned()]).is_ok();
            installed.insert(id.to_owned(), fake || catalog.definition(id).is_ok());
        }
        let mut resolved = plugin_mounts::resolve(snapshot, &switches, &declared, &installed);
        resolved.reasons.splice(0..0, reasons);
        if snapshot_unreadable {
            resolved.snapshot = false;
            resolved.reasons.push("no snapshot (unreadable)".to_owned());
        }
        let mut bindings = Vec::new();
        for id in resolved.mounts.keys() {
            // Always resolve current Library code. The Run stores an authorization
            // baseline; it does not pin executable bytes or dependency views.
            let binding = self
                .tools
                .resolve_fixture_plugins(std::slice::from_ref(id))
                .ok()
                .and_then(|mut bindings| bindings.pop())
                .map(Ok)
                .unwrap_or_else(|| {
                    catalog
                        .resolve(std::slice::from_ref(id))
                        .map_err(|error| format!("Plugin `{id}` is not available: {error}"))?
                        .pop()
                        .ok_or_else(|| format!("Plugin `{id}` is not available in this Library"))
                })?;
            bindings.push(binding);
        }
        // Check selected authority before network policy can hide an altered
        // Plugin, then recheck the resulting set including a derived provider.
        self.validate_invocation_plugins(&admitted, request, &bindings)?;
        let network = crate::plugin_network::apply_node(
            self.plugin_library_root()?,
            &mut bindings,
            mode,
            snapshot.is_none_or(|snapshot| snapshot.network_allowed),
            &mut resolved.reasons,
        )?;
        self.validate_invocation_plugins(&admitted, request, &bindings)?;
        Ok(MountedPlugins {
            bindings,
            network,
            reasons: resolved.reasons,
            session: session_turn.map(|(session, _)| session),
            snapshot: resolved.snapshot,
            snapshot_unreadable,
            by_declaration: resolved.by_declaration,
            by_switch: resolved.by_switch,
        })
    }
}
