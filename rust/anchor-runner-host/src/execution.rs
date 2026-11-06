//! Validated host ports shared by platform and standalone Kernel execution.
use crate::application::RunApplication;
use crate::{HostArtifacts, HostControl, HostNodes, make_host_with_control, reject_snapshot};
use anchor_graph_host::{GraphCatalog, LoadedGraphBundle};
use anchor_runtime::graph::{CallIdentity, PluginBinding};
use anchor_runtime::graph::{
    FileRunLeaseProvider, GraphError, GraphRunRecord, GraphSnapshot, InvocationKey,
    NodeExecutionPort, PendingRecovery, RecoveryDecision,
};
use sha2::Digest;
use std::collections::BTreeMap;
use std::{future::Future, pin::Pin};

pub(crate) mod tool_assembly;

pub(crate) struct PreparedExecution {
    artifacts: HostArtifacts,
    nodes: HostNodes,
    application: Option<RunApplication>,
}

impl PreparedExecution {
    pub(crate) fn prepare_with_catalog(
        record: &GraphRunRecord,
        control: HostControl,
        bindings: BTreeMap<String, PluginBinding>,
        application: RunApplication,
    ) -> Result<Self, String> {
        // One answer for "where does a Plugin live": the deployment's Library root,
        // or the catalog this application was configured with.
        let library_root = Some(application.library_root().to_path_buf());
        Self::prepare_with_bindings(
            record,
            control,
            bindings,
            application.clone(),
            Some(application),
            library_root,
        )
    }

    pub(crate) fn prepare_with_bindings(
        record: &GraphRunRecord,
        control: HostControl,
        bindings: BTreeMap<String, PluginBinding>,
        reader: RunApplication,
        application: Option<RunApplication>,
        library_root: Option<std::path::PathBuf>,
    ) -> Result<Self, String> {
        reject_snapshot(&record.snapshot)?;
        let state_root = reader.data_root.clone();
        // Old storage has no Kernel lifecycle authority. Refuse it before host
        // setup can publish any execution state or invocation inputs.
        crate::application::run_access::load(&state_root, &record.run_id)
            .map_err(|error| error.to_string())?;
        let (artifacts, nodes) = make_host_with_control(
            &reader,
            &record.run_id,
            control,
            library_root,
            application
                .as_ref()
                .and_then(|application| application.runtime_configuration().cloned()),
        )?;
        let _ = nodes.io_resolver.run_application.set(reader);
        // Admission validates the Plugin declarations and host wiring. The
        // per-node network flag is enforced when the node is dispatched.
        if !nodes
            .invocation_plugin_authority(record)
            .map_err(|error| error.to_string())?
        {
            Self::validate_plugin_bindings(&nodes, &bindings)?;
        }
        let caps = nodes.capabilities();
        for node in &record.snapshot.nodes {
            if node.agent.is_some() && !caps.agent {
                return Err(
                    "Agent execution requires configured provider credentials/model".into(),
                );
            }
            if let Some(op) = node.op.as_ref().and_then(|id| record.snapshot.ops.get(id))
                && let Some(command) = op.get("run").and_then(serde_json::Value::as_str)
            {
                crate::op::shell_command(command, &nodes.allowed_commands)?;
            }
        }
        let execution = Self {
            artifacts,
            nodes,
            application,
        };
        let metadata = crate::run_metadata::load(&state_root, &record.run_id)
            .map_err(|error| format!("local input Run identity unavailable: {error:?}"))?;
        if record
            .snapshot
            .ops
            .values()
            .any(|operation| operation.get("host").is_some())
            && (execution.application.is_none()
                || metadata
                    .as_ref()
                    .is_none_or(|metadata| metadata.assistant.is_none()))
        {
            return Err(
                "Session host operations require an explicitly admitted assistant instance".into(),
            );
        }
        if let Some(metadata) = metadata {
            execution.bind_local_inputs(record, &metadata.graph)?;
        } else if !execution.nodes.io_resolver.local_inputs_configured() {
            execution
                .nodes
                .io_resolver
                .bind_local_inputs(record, None)?;
        } else if execution.application.is_none() {
            let graph = std::env::var("ANCHOR_RUNNER_GRAPH_NAME")
                .ok()
                .filter(|name| !name.trim().is_empty())
                .ok_or(
                    "ANCHOR_RUNNER_GRAPH_NAME is required with ANCHOR_RUNNER_LOCAL_INPUTS_ROOT",
                )?;
            execution.bind_local_inputs(record, &graph)?;
        }
        // A saved Kernel Run may already have admitted a waiting child. Check
        // that child's independent grants before either task resumes.
        let application = execution.run_application();
        if let Some(existing) = application
            .execution_record(&record.run_id)
            .map_err(|error| format!("Run configuration: {error:?}"))?
        {
            execution.validate_configuration(&existing)?;
        }
        Ok(execution)
    }

    pub(crate) fn bind_local_inputs(
        &self,
        record: &GraphRunRecord,
        graph: &str,
    ) -> Result<(), String> {
        self.nodes
            .io_resolver
            .bind_local_inputs(record, Some(graph))
    }

    fn validate_plugin_bindings(
        nodes: &HostNodes,
        bindings: &BTreeMap<String, PluginBinding>,
    ) -> Result<(), String> {
        if bindings.is_empty() {
            return Ok(());
        }
        let ids = bindings.keys().cloned().collect::<Vec<_>>();
        let current = nodes
            .resolve_plugins(&ids)
            .map_err(|error| format!("Plugin manifest resolution failed: {error}"))?;
        if current.len() != ids.len()
            || current
                .iter()
                .zip(&ids)
                .any(|(binding, id)| binding.id != *id || binding.digest.is_empty())
        {
            return Err(
                "Plugin resolver returned incomplete, reordered, unhashed, or unauthorized bindings"
                    .into(),
            );
        }
        for ((id, admitted), current) in bindings.iter().zip(&current) {
            if admitted.same_authorization(current) {
                continue;
            }
            return Err(format!(
                "Plugin `{id}` Host authorization changed since this Graph Run was admitted"
            ));
        }
        // Wiring validates the admitted bindings again against its catalog read.
        nodes
            .io_resolver
            .validate_plugin_configuration(&bindings.values().cloned().collect::<Vec<_>>())
    }

    fn run_application(&self) -> RunApplication {
        self.nodes
            .io_resolver
            .run_application
            .get()
            .expect("prepared execution binds one Run reader")
            .clone()
    }

    fn validate_configuration(&self, root: &GraphRunRecord) -> Result<(), String> {
        let application = self.run_application();
        let mut pending = vec![root.clone()];
        let mut checked = std::collections::BTreeSet::new();
        while let Some(record) = pending.pop() {
            if !checked.insert(record.run_id.clone()) {
                continue;
            }
            let settled = application
                .read_run(&record.run_id)
                .map_err(|error| format!("Run configuration: {error:?}"))?
                .is_some_and(|view| view.is_settled());
            if !settled
                && !self
                    .nodes
                    .invocation_plugin_authority(&record)
                    .map_err(|error| error.to_string())?
            {
                Self::validate_plugin_bindings(&self.nodes, &record.plugin_bindings)?;
            }
            let metadata =
                crate::run_metadata::load(self.nodes.io_resolver.state_root(), &record.run_id)
                    .map_err(|error| {
                        format!("Run configuration identity unavailable: {error:?}")
                    })?;
            self.nodes.io_resolver.bind_local_inputs(
                &record,
                metadata.as_ref().map(|metadata| metadata.graph.as_str()),
            )?;
            self.nodes.io_resolver.verify_local_inputs(&record)?;
            let mut invocation_keys = crate::run_data::invocation_keys(&record);
            invocation_keys.extend(
                application
                    .dispatch_keys(&record.run_id)
                    .map_err(|error| format!("Run dispatch identity: {error:?}"))?,
            );
            invocation_keys.sort_by_key(|key| key.durable_key());
            invocation_keys.dedup();
            for key in invocation_keys {
                if crate::run_data::legacy_invocation_exists(
                    self.nodes.io_resolver.state_root(),
                    &key,
                )? {
                    return Err(
                        "legacy Agent state is not resumable by the standard Goose Host".into(),
                    );
                }
            }
            for call in record.graph_calls.values() {
                if call.mode != "wait"
                    || call.status != anchor_runtime::graph::GraphCallStatus::Waiting
                {
                    continue;
                }
                if let Some(child) = &call.child_run_id {
                    let view = application
                        .read_run(child)
                        .map_err(|error| format!("waiting child configuration: {error:?}"))?;
                    if view.is_some_and(|view| !view.is_settled()) {
                        let child = application
                            .execution_record(child)
                            .map_err(|error| format!("waiting child configuration: {error:?}"))?
                            .ok_or("waiting child Run is missing")?;
                        pending.push(child);
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn kernel_ports(self, application: RunApplication) -> (HostArtifacts, HostNodes) {
        let _ = self.nodes.io_resolver.run_application.set(application);
        (self.artifacts, self.nodes)
    }

    pub(crate) fn record_recovery_decision(
        &self,
        key: &InvocationKey,
        attempt_id: i64,
        decision: RecoveryDecision,
    ) -> Result<(), String> {
        self.nodes
            .record_recovery_decision(key, attempt_id, decision)
            .map_err(|error| error.to_string())
    }

    /// Read native pending effects only. This never loads a Session or starts
    /// an executor; spike completion facts retain their own failure boundary.
    pub(crate) fn pending_recovery(
        &self,
        run_id: &str,
    ) -> Result<Vec<PendingRecovery>, GraphError> {
        if crate::goose_acp::runtime_mode() != "goose" {
            return Ok(Vec::new());
        }
        let Some(nodes) = &self.nodes.goose_nodes else {
            return Ok(Vec::new());
        };
        let mut pending = Vec::new();
        for key in self
            .run_application()
            .dispatch_keys(run_id)
            .map_err(|error| GraphError::CorruptRun(format!("recovery frontier: {error:?}")))?
        {
            for attempt in nodes.pending_recovery(&key)? {
                pending.push(PendingRecovery {
                    key: key.clone(),
                    attempt,
                });
            }
        }
        Ok(pending)
    }
}

pub(crate) struct HostGraphCatalog {
    pub(crate) root: std::path::PathBuf,
    pub(crate) state: std::path::PathBuf,
    pub(crate) application: Option<RunApplication>,
    pub(crate) task: Option<crate::task_delegation::TaskOrigin>,
}

struct PluginReferenceLease {
    _lease: anchor_library::InstallLease,
}

impl anchor_runtime::graph::RunLease for PluginReferenceLease {}

impl HostGraphCatalog {
    fn path(&self, name: &str) -> Result<std::path::PathBuf, GraphError> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        {
            return Err(GraphError::InvalidSnapshot(
                "invalid child Graph name".into(),
            ));
        }
        // Resolve through the same configured-name -> bundle-root mapping as the
        // HTTP layer so a child call and a PUT/delete share one identity.
        Ok(match &self.application {
            Some(application) => application.graph_bundle_path(name),
            None => self.root.join(name),
        })
    }
}

impl GraphCatalog for HostGraphCatalog {
    fn lock_plugin_references<'a>(
        &'a self,
        cancellation: anchor_runtime::Cancellation,
    ) -> anchor_graph_host::PluginReferenceLeaseFuture<'a> {
        Box::pin(async move {
            let root = self
                .application
                .as_ref()
                .map(|application| application.library_root().to_path_buf())
                .unwrap_or_else(|| self.root.clone());
            let access = crate::dependency_provision::DependencyAccess {
                network: false,
                cancellation,
            };
            let lease = crate::plugin_library::publication_lease(
                &root,
                &access,
                "child reference publication",
            )
            .await
            .map_err(|error| {
                GraphError::Unsupported(format!("child Plugin publication: {error:?}"))
            })?;
            Ok(Box::new(PluginReferenceLease { _lease: lease })
                as Box<dyn anchor_runtime::graph::RunLease>)
        })
    }

    fn graph_ancestry(&self, run_id: &str) -> Result<Vec<String>, GraphError> {
        let mut ancestry = Vec::new();
        let mut current = run_id.to_owned();
        let mut seen = std::collections::BTreeSet::new();
        let mut expected_parent: Option<(String, String, String)> = None;
        let mut expected_root: Option<String> = None;
        loop {
            if !seen.insert(current.clone()) {
                return Err(GraphError::CorruptRun("cyclic call ancestry".into()));
            }
            let metadata = crate::run_metadata::load(&self.state, &current)
                .map_err(|error| GraphError::Unsupported(format!("Run ancestry: {error:?}")))?
                .ok_or_else(|| {
                    GraphError::CorruptRun(format!("missing Run ancestry for `{current}`"))
                })?;
            if let Some((parent_run, parent_graph, parent_digest)) = expected_parent.take()
                && (metadata.run_id != parent_run
                    || metadata.graph != parent_graph
                    || metadata.graph_digest != parent_digest)
            {
                return Err(GraphError::CorruptRun(
                    "Graph call ancestry does not match parent identity".into(),
                ));
            }
            ancestry.push(metadata.graph.clone());
            match (metadata.trigger_source.as_str(), metadata.graph_call) {
                ("graph_call", Some(source)) => {
                    if source.parent_run.is_empty()
                        || source.parent_graph.is_empty()
                        || source.parent_graph_digest.is_empty()
                        || source.root_run.is_empty()
                        || source.node.is_empty()
                        || source.invocation == 0
                        || !matches!(source.mode.as_str(), "wait" | "detach")
                    {
                        return Err(GraphError::CorruptRun(
                            "Graph call ancestry metadata is incomplete".into(),
                        ));
                    }
                    if expected_root
                        .replace(source.root_run.clone())
                        .is_some_and(|previous| previous != source.root_run)
                    {
                        return Err(GraphError::CorruptRun(
                            "Graph call ancestry root identity changed".into(),
                        ));
                    }
                    expected_parent = Some((
                        source.parent_run.clone(),
                        source.parent_graph,
                        source.parent_graph_digest,
                    ));
                    current = source.parent_run;
                }
                ("graph_call", None) | (_, Some(_)) => {
                    return Err(GraphError::CorruptRun(
                        "Run trigger and Graph call ancestry disagree".into(),
                    ));
                }
                (_, None) => {
                    if expected_root.as_deref().is_some_and(|root| root != current) {
                        return Err(GraphError::CorruptRun(
                            "Graph call ancestry root identity does not match".into(),
                        ));
                    }
                    return Ok(ancestry);
                }
            }
        }
    }

    fn prepare_session_call<'a>(
        &'a self,
        identity: &'a CallIdentity,
        session: &'a str,
        graph: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            if self.application.is_none() {
                return Err(GraphError::Unsupported(
                    "call.session requires a platform application".into(),
                ));
            }
            let mut ancestor = Some(identity.parent_run_id.clone());
            let mut seen = std::collections::BTreeSet::new();
            while let Some(id) = ancestor {
                if !seen.insert(id.clone()) {
                    return Err(GraphError::CorruptRun("cyclic call ancestry".into()));
                }
                let metadata = crate::run_metadata::load(&self.state, &id)
                    .map_err(|error| {
                        GraphError::Unsupported(format!("parent identity: {error:?}"))
                    })?
                    .ok_or_else(|| GraphError::CorruptRun("missing parent identity".into()))?;
                if metadata.conversation.is_some() || metadata.session_call.is_some() {
                    return Err(GraphError::Unsupported(
                        "a conversation cannot enqueue work into a Session".into(),
                    ));
                }
                ancestor = metadata.graph_call.map(|source| source.parent_run);
            }
            // Admission freezes the authorized recipient. A resumed wait consumes
            // that same fact; the Session host revalidates it before execution/send.
            if let Some(metadata) =
                crate::run_metadata::load(&self.state, &format!("call-{}", identity.durable_key()))
                    .map_err(|error| {
                        GraphError::Unsupported(format!("Session identity: {error:?}"))
                    })?
                && let Some(call) = metadata.session_call
            {
                if call.context.session != session || metadata.graph != graph {
                    return Err(GraphError::RunConflict);
                }
                return serde_json::to_value(call.context).map_err(GraphError::SnapshotDecode);
            }
            crate::application::session_calls::resolve(identity, session, graph).await
        })
    }

    fn record_session_call(
        &self,
        run_id: &str,
        context: &serde_json::Value,
    ) -> Result<(), GraphError> {
        self.application
            .as_ref()
            .ok_or_else(|| GraphError::Unsupported("Session host unavailable".into()))?
            .record_session_call(run_id, context)
            .map_err(|error| GraphError::Unsupported(format!("Session admission: {error:?}")))
    }

    fn session_call_outcome<'a>(
        &'a self,
        run_id: &'a str,
        mode: &'a str,
        cancellation: anchor_runtime::Cancellation,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<anchor_runtime::graph::GraphCallOutcome>, GraphError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let application = self
                .application
                .as_ref()
                .ok_or_else(|| GraphError::Unsupported("Session host unavailable".into()))?;
            if mode == "wait" && cancellation.load(std::sync::atomic::Ordering::Acquire) {
                application
                    .stop_session_call(run_id)
                    .await
                    .map_err(|error| {
                        GraphError::Unsupported(format!("Session cancellation: {error:?}"))
                    })?;
            }
            application
                .session_call_outcome(run_id, mode)
                .map_err(|error| GraphError::Unsupported(format!("Session outcome: {error:?}")))
        })
    }

    fn snapshot(&self, name: &str) -> Result<Option<GraphSnapshot>, GraphError> {
        Ok(self.bundle(name)?.map(|bundle| bundle.snapshot))
    }

    fn bundle(&self, name: &str) -> Result<Option<LoadedGraphBundle>, GraphError> {
        let path = self.path(name)?;
        if !path.is_dir() {
            return Ok(None);
        }
        let bundle = anchor_graph_host::FileGraphBundleLoader::new(&path).load()?;
        reject_snapshot(&bundle.snapshot).map_err(GraphError::Unsupported)?;
        Ok(Some(bundle))
    }

    fn lock_admission(
        &self,
        name: &str,
    ) -> Result<Box<dyn anchor_runtime::graph::RunLease>, GraphError> {
        let path = self.path(name)?;
        match &self.application {
            Some(application) => application
                .graph_admission_lease(&path)
                .map_err(|error| GraphError::Unsupported(format!("admission lease: {error:?}"))),
            None => {
                let id = format!(
                    "graph-{:x}",
                    sha2::Sha256::digest(path.as_os_str().as_encoded_bytes())
                );
                FileRunLeaseProvider::new(self.state.join("admission-locks")).acquire_lease(&id)
            }
        }
    }

    fn record_child_admission(
        &self,
        run_id: &str,
        graph: &str,
        snapshot: &GraphSnapshot,
        plugins: &[PluginBinding],
        identity: &CallIdentity,
        mode: &str,
    ) -> Result<(), GraphError> {
        // Child admission invokes this after resolving Plugins under its
        // Library lease, not before waiting for a concurrent publication.
        let application = self.application.as_ref();
        let _empty_task_catalog = if self.task.is_some() && plugins.is_empty() {
            crate::plugin_library::PluginLibrary::new(
                application
                    .ok_or_else(|| {
                        GraphError::Unsupported("task authorization unavailable".into())
                    })?
                    .library_root(),
            )
            .catalog_read_guard()
            .map_err(|error| GraphError::Unsupported(format!("task Plugin catalog: {error}")))?
        } else {
            None
        };
        if let Some(origin) = &self.task {
            application
                .ok_or_else(|| GraphError::Unsupported("task authorization unavailable".into()))?
                .authorize_task_call_snapshot(origin, graph, snapshot)
                .map_err(|error| {
                    GraphError::Unsupported(format!("delegated static call: {error:?}"))
                })?;
        }
        let path = self.path(graph)?;
        crate::local_inputs::LocalInputs::from_env(self.state.clone())
            .and_then(|inputs| {
                inputs.freeze(
                    run_id,
                    &snapshot.digest().map_err(|error| error.to_string())?,
                    snapshot,
                    Some(graph),
                    true,
                )
            })
            .map_err(GraphError::Unsupported)?;
        let parent_metadata = crate::run_metadata::load(&self.state, &identity.parent_run_id)
            .map_err(|error| GraphError::Unsupported(format!("parent metadata: {error:?}")))?
            .ok_or_else(|| {
                GraphError::CorruptRun("parent Run source metadata is missing".into())
            })?;
        let root_run = parent_metadata
            .graph_call
            .as_ref()
            .map(|source| source.root_run.clone())
            .unwrap_or_else(|| identity.parent_run_id.clone());
        crate::run_metadata::save_child_once(
            &self.state,
            run_id.to_owned(),
            graph.to_owned(),
            snapshot.digest()?,
            &path,
            crate::run_metadata::GraphCallSource {
                parent_run: identity.parent_run_id.clone(),
                parent_graph: parent_metadata.graph,
                parent_graph_digest: identity.parent_graph_digest.clone(),
                node: identity.node_id.clone(),
                invocation: identity.invocation,
                mode: mode.to_owned(),
                root_run,
            },
            parent_metadata.oauth_owner,
        )
        .map_err(|error| GraphError::Unsupported(format!("child metadata: {error:?}")))
    }

    fn verify_child_identity(
        &self,
        run_id: &str,
        graph: &str,
        snapshot: &GraphSnapshot,
        identity: &CallIdentity,
        mode: &str,
    ) -> Result<(), GraphError> {
        let metadata = crate::run_metadata::load(&self.state, run_id)
            .map_err(|error| GraphError::Unsupported(format!("child metadata: {error:?}")))?
            .ok_or_else(|| {
                GraphError::CorruptRun("child Run has no immutable source metadata".into())
            })?;
        let Some(source) = metadata.graph_call else {
            return Err(GraphError::CorruptRun(
                "child Run has no Graph call source identity".into(),
            ));
        };
        if metadata.trigger_source != "graph_call"
            || metadata.graph != graph
            || metadata.graph_digest != snapshot.digest()?
            || source.parent_run != identity.parent_run_id
            || source.parent_graph_digest != identity.parent_graph_digest
            || source.node != identity.node_id
            || source.invocation != identity.invocation
            || source.mode != mode
        {
            return Err(GraphError::CorruptRun(
                "child Run metadata does not match its durable Graph call identity".into(),
            ));
        }
        let child = crate::application::run_access::load(&self.state, run_id)?;
        if child.is_some_and(|child| child.is_settled()) {
            return Ok(());
        }
        crate::local_inputs::LocalInputs::from_env(self.state.clone())
            .and_then(|inputs| {
                inputs.freeze(
                    run_id,
                    &snapshot.digest().map_err(|error| error.to_string())?,
                    snapshot,
                    Some(&metadata.graph),
                    false,
                )
            })
            .map_err(GraphError::Unsupported)?;
        Ok(())
    }

    fn has_child_admission(&self, run_id: &str) -> Result<bool, GraphError> {
        crate::run_metadata::load(&self.state, run_id)
            .map(|metadata| {
                metadata.is_some_and(|metadata| metadata.trigger_source == "graph_call")
            })
            .map_err(|error| GraphError::Unsupported(format!("child metadata: {error:?}")))
    }

    fn verify_child_plugins(
        &self,
        graph: &str,
        _bindings: &[PluginBinding],
    ) -> Result<(), GraphError> {
        let path = self.path(graph)?;
        if !path.is_dir() {
            return Err(GraphError::InvalidSnapshot(format!(
                "unknown child graph `{graph}`"
            )));
        }
        // Plugin authority is checked by the shared Runner against the Library.
        // Graph bundles contain references, never another Plugin copy.
        Ok(())
    }

    fn begin_child_execution<'a>(
        &'a self,
        run_id: &'a str,
        graph: &'a str,
        parent_cancellation: anchor_runtime::Cancellation,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<anchor_graph_host::ChildRunControl, GraphError>> + Send + 'a,
        >,
    > {
        Box::pin(async move {
            match &self.application {
                Some(application) => application
                    .begin_wait_child(run_id, graph, parent_cancellation)
                    .await
                    .map_err(|error| {
                        GraphError::Unsupported(format!("register wait child: {error:?}"))
                    }),
                None => Ok(anchor_graph_host::ChildRunControl::linked_to_parent(
                    parent_cancellation,
                )),
            }
        })
    }

    fn end_child_execution<'a>(
        &'a self,
        run_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some(application) = &self.application {
                application.end_wait_child(run_id).await;
            }
        })
    }

    fn dispatch_detached<'a>(
        &'a self,
        run_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(application) = &self.application {
                application
                    .dispatch_detached(run_id)
                    .await
                    .map_err(|error| {
                        GraphError::Unsupported(format!("dispatch detached child: {error:?}"))
                    })?;
            }
            Ok(())
        })
    }

    fn child_execution_finished<'a>(
        &'a self,
        run_id: &'a str,
        _graph: &'a str,
        _phase: anchor_kernel::Phase,
    ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(application) = &self.application {
                application
                    .kernel_child_finished(run_id)
                    .await
                    .map_err(|error| {
                        GraphError::Unsupported(format!("resume waiting parent: {error:?}"))
                    })?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod legacy_plugin_tests;

#[cfg(test)]
mod graph_call_ancestry_tests {
    use super::*;
    use crate::run_metadata::{self as metadata, GraphCallSource, RunMetadata};

    fn catalog(state: &std::path::Path) -> HostGraphCatalog {
        HostGraphCatalog {
            root: state.join("graphs"),
            state: state.to_path_buf(),
            application: None,
            task: None,
        }
    }

    fn root_metadata(state: &std::path::Path, id: &str, graph: &str, digest: &str) {
        let metadata =
            RunMetadata::new(id.to_owned(), graph.to_owned(), digest.to_owned(), state).unwrap();
        metadata::save(state, &metadata).unwrap();
    }

    fn graph_call_source(
        parent_run: &str,
        parent_graph: &str,
        parent_digest: &str,
        root_run: &str,
    ) -> GraphCallSource {
        GraphCallSource {
            parent_run: parent_run.to_owned(),
            parent_graph: parent_graph.to_owned(),
            parent_graph_digest: parent_digest.to_owned(),
            node: "invoke".into(),
            invocation: 1,
            mode: "wait".into(),
            root_run: root_run.to_owned(),
        }
    }

    fn save_child(
        state: &std::path::Path,
        run_id: &str,
        graph: &str,
        digest: &str,
        source: GraphCallSource,
    ) {
        metadata::save_child_once(
            state,
            run_id.to_owned(),
            graph.to_owned(),
            digest.to_owned(),
            state,
            source,
            None,
        )
        .unwrap();
    }

    #[test]
    fn graph_ancestry_validates_and_returns_durable_chain() {
        let state = tempfile::tempdir().unwrap();
        root_metadata(state.path(), "run-a", "graph-a", "digest-a");
        save_child(
            state.path(),
            "run-b",
            "graph-b",
            "digest-b",
            graph_call_source("run-a", "graph-a", "digest-a", "run-a"),
        );
        save_child(
            state.path(),
            "run-c",
            "graph-c",
            "digest-c",
            graph_call_source("run-b", "graph-b", "digest-b", "run-a"),
        );

        assert_eq!(
            catalog(state.path()).graph_ancestry("run-c").unwrap(),
            ["graph-c", "graph-b", "graph-a"]
        );
    }

    #[test]
    fn graph_ancestry_rejects_cycles_and_parent_identity_drift() {
        let state = tempfile::tempdir().unwrap();
        save_child(
            state.path(),
            "run-a",
            "graph-a",
            "digest-a",
            graph_call_source("run-b", "graph-b", "digest-b", "run-a"),
        );
        save_child(
            state.path(),
            "run-b",
            "graph-b",
            "digest-b",
            graph_call_source("run-a", "graph-a", "digest-a", "run-a"),
        );

        assert!(matches!(
            catalog(state.path()).graph_ancestry("run-a"),
            Err(GraphError::CorruptRun(message)) if message.contains("cyclic call ancestry")
        ));
    }
}
