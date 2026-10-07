//! Prepared host execution shared by application use cases and legacy framing.
use crate::application::RunApplication;
use crate::{HostArtifacts, HostControl, HostNodes, make_host_with_control, reject_snapshot};
use anchor_graph_host::{GraphCatalog, InProcessGraphHost, LoadedGraphBundle, PluginCatalog};
use anchor_runtime_rig::graph::{CallIdentity, PluginBinding};
use anchor_runtime_rig::graph::{
    FileRunStore, GraphError, GraphRunRecord, GraphRunner, GraphSnapshot, InvocationKey,
    NodeExecutionPort, RecoveryDecision, RunStatus, RunStore,
};
use sha2::Digest;
use std::collections::BTreeMap;
use std::{future::Future, pin::Pin};

pub(crate) struct PreparedExecution {
    pub(crate) store: FileRunStore,
    artifacts: HostArtifacts,
    nodes: HostNodes,
    control: HostControl,
    catalog_root: Option<std::path::PathBuf>,
    application: Option<RunApplication>,
}

impl PreparedExecution {
    pub(crate) fn prepare_with_catalog(
        record: &GraphRunRecord,
        control: HostControl,
        bindings: BTreeMap<String, PluginBinding>,
        catalog_root: std::path::PathBuf,
        application: RunApplication,
    ) -> Result<Self, String> {
        Self::prepare_with_bindings(
            record,
            control,
            bindings,
            Some(catalog_root),
            Some(application),
        )
    }

    pub(crate) fn prepare_with_bindings(
        record: &GraphRunRecord,
        control: HostControl,
        bindings: BTreeMap<String, PluginBinding>,
        catalog_root: Option<std::path::PathBuf>,
        application: Option<RunApplication>,
    ) -> Result<Self, String> {
        reject_snapshot(&record.snapshot)?;
        let (store, artifacts, nodes, control) =
            make_host_with_control(&record.run_id, bindings.clone(), control)?;
        // Admission validates the Plugin declarations and host wiring. The
        // per-node network flag is enforced when the node is dispatched.
        nodes
            .io_resolver
            .validate_plugin_configuration(&bindings.values().cloned().collect::<Vec<_>>())?;
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
            store,
            artifacts,
            nodes,
            control,
            catalog_root,
            application,
        };
        let metadata = crate::application::metadata::load(
            &crate::env_path("ANCHOR_RUNNER_STATE_ROOT")?,
            &record.run_id,
        )
        .map_err(|error| format!("local input Run identity unavailable: {error:?}"))?;
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
        // The framed entrypoint prepares from the submitted snapshot before
        // loading its stored cursor. Validate the durable record here too, so
        // configuration mistakes cannot turn a recoverable Run into Failed.
        if let Some(existing) = execution
            .store
            .load(&record.run_id)
            .map_err(|error| error.to_string())?
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

    fn validate_configuration(&self, root: &GraphRunRecord) -> Result<(), String> {
        let mut pending = vec![root.clone()];
        let mut checked = std::collections::BTreeSet::new();
        while let Some(record) = pending.pop() {
            if !checked.insert(record.run_id.clone()) {
                continue;
            }
            let metadata = crate::application::metadata::load(
                &crate::env_path("ANCHOR_RUNNER_STATE_ROOT")?,
                &record.run_id,
            )
            .map_err(|error| format!("Run configuration identity unavailable: {error:?}"))?;
            self.nodes.io_resolver.bind_local_inputs(
                &record,
                metadata.as_ref().map(|metadata| metadata.graph.as_str()),
            )?;
            self.nodes.io_resolver.verify_local_inputs(&record)?;
            #[cfg(not(feature = "legacy-regression"))]
            for key in crate::run_data::invocation_keys(&record) {
                if crate::run_data::legacy_invocation_exists(
                    &crate::env_path("ANCHOR_RUNNER_STATE_ROOT")?,
                    &key,
                )? {
                    return Err(
                        "legacy Agent state is not resumable by the standard Goose Host".into(),
                    );
                }
            }
            #[cfg(feature = "legacy-regression")]
            if let Some(nodes) = &self.nodes.io_nodes {
                let mut cursors = record.cursor.iter().collect::<Vec<_>>();
                if let Some(parallel) = &record.parallel {
                    cursors.extend(
                        parallel
                            .branches
                            .iter()
                            .filter_map(|branch| branch.cursor.as_ref()),
                    );
                }
                for cursor in cursors {
                    if let Some(agent) = record
                        .snapshot
                        .nodes
                        .iter()
                        .find(|node| node.id == cursor.node_id)
                        .and_then(|node| node.agent.as_ref())
                        .and_then(|agent| record.snapshot.agents.get(agent))
                    {
                        nodes
                            .validate_model_binding(&cursor.key, Some(&agent.model))
                            .map_err(|error| error.to_string())?;
                    }
                }
            }
            // A waiting parent may resume an already admitted child in-process.
            // Its authority and model configuration must pass before either Run
            // enters execution; the parent's grants never authorize the child.
            for call in record.graph_calls.values() {
                if call.mode != "wait"
                    || !record.cursor.as_ref().is_some_and(|cursor| {
                        cursor.node_id == call.identity.node_id
                            && cursor.key.invocation == call.identity.invocation
                    })
                {
                    continue;
                }
                if let Some(child) = &call.child_run_id
                    && let Some(child) =
                        self.store.load(child).map_err(|error| error.to_string())?
                    && !matches!(
                        child.status,
                        RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
                    )
                {
                    pending.push(child);
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn run(self, record: GraphRunRecord) -> Result<GraphRunRecord, String> {
        self.validate_configuration(&record)?;
        let run_id = record.run_id.clone();
        let graph_calls = record
            .snapshot
            .ops
            .values()
            .any(|op| op.get("call").is_some());
        let outcome = if graph_calls {
            let catalog_root = self
                .catalog_root
                .clone()
                .or_else(|| {
                    std::env::var_os("ANCHOR_RUNNER_CATALOG_ROOT").map(std::path::PathBuf::from)
                })
                .ok_or_else(|| "ANCHOR_RUNNER_CATALOG_ROOT is required for Op.call".to_owned())?;
            let state_root = std::env::var_os("ANCHOR_RUNNER_STATE_ROOT")
                .map(std::path::PathBuf::from)
                .ok_or_else(|| "ANCHOR_RUNNER_STATE_ROOT is required".to_owned())?;
            let catalog = RunnerGraphCatalog {
                root: catalog_root,
                state: state_root,
                application: self.application.clone(),
            };
            let host = InProcessGraphHost::new(
                &catalog,
                &self.store,
                &self.artifacts,
                &self.nodes,
                &self.control,
            );
            GraphRunner::new(&self.store, &self.artifacts, &host, &self.control)
                .run(record)
                .await
        } else {
            GraphRunner::new(&self.store, &self.artifacts, &self.nodes, &self.control)
                .run(record)
                .await
        };
        match outcome {
            Ok(record) => Ok(record),
            Err(error) => {
                // Only the fresh durable record may be updated after Runner releases its lease.
                if matches!(error, GraphError::RunBusy(_) | GraphError::RunConflict) {
                    return Err(error.to_string());
                }
                let _lease = self
                    .store
                    .acquire_lease(&run_id)
                    .map_err(|e| e.to_string())?;
                let mut durable = self
                    .store
                    .load(&run_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("{error}; accepted Run is missing"))?;
                if matches!(durable.status, RunStatus::Ready | RunStatus::Running) {
                    durable.status = RunStatus::Failed;
                    durable.error = Some(format!(
                        "host execution interrupted: {error}; unfinished effects are not automatically replayed"
                    ));
                    self.store.save(&durable).map_err(|e| e.to_string())?;
                }
                Ok(durable)
            }
        }
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
}

struct RunnerGraphCatalog {
    root: std::path::PathBuf,
    state: std::path::PathBuf,
    application: Option<RunApplication>,
}

impl RunnerGraphCatalog {
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

impl GraphCatalog for RunnerGraphCatalog {
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
                let metadata = crate::application::metadata::load(&self.state, &id)
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
            if let Some(metadata) = crate::application::metadata::load(
                &self.state,
                &format!("call-{}", identity.durable_key()),
            )
            .map_err(|error| GraphError::Unsupported(format!("Session identity: {error:?}")))?
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
        cancellation: anchor_runtime_rig::Cancellation,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        Option<anchor_runtime_rig::graph::GraphCallOutcome>,
                        GraphError,
                    >,
                > + Send
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
        if bundle
            .snapshot
            .ops
            .values()
            .any(|op| op.get("call").is_some())
        {
            return Err(GraphError::Unsupported(
                "nested Graph calls are not supported by this host".into(),
            ));
        }
        Ok(Some(bundle))
    }

    fn lock_admission(
        &self,
        name: &str,
    ) -> Result<Box<dyn anchor_runtime_rig::graph::RunLease>, GraphError> {
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
                FileRunStore::new(self.state.join("admission-locks")).acquire_lease(&id)
            }
        }
    }

    fn record_child_admission(
        &self,
        run_id: &str,
        graph: &str,
        snapshot: &GraphSnapshot,
        _plugins: &[PluginBinding],
        identity: &CallIdentity,
        mode: &str,
    ) -> Result<(), GraphError> {
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
        let parent_metadata =
            crate::application::metadata::load(&self.state, &identity.parent_run_id)
                .map_err(|error| GraphError::Unsupported(format!("parent metadata: {error:?}")))?
                .ok_or_else(|| {
                    GraphError::CorruptRun("parent Run source metadata is missing".into())
                })?;
        let root_run = parent_metadata
            .graph_call
            .as_ref()
            .map(|source| source.root_run.clone())
            .unwrap_or_else(|| identity.parent_run_id.clone());
        crate::application::metadata::save_child_once(
            &self.state,
            run_id.to_owned(),
            graph.to_owned(),
            snapshot.digest()?,
            &path,
            crate::application::metadata::GraphCallSource {
                parent_run: identity.parent_run_id.clone(),
                parent_graph: parent_metadata.graph,
                parent_graph_digest: identity.parent_graph_digest.clone(),
                node: identity.node_id.clone(),
                invocation: identity.invocation,
                mode: mode.to_owned(),
                root_run,
            },
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
        let metadata = crate::application::metadata::load(&self.state, run_id)
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
        let child = FileRunStore::new(self.state.join("runs")).load(run_id)?;
        if child.is_some_and(|child| {
            matches!(
                child.status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
            )
        }) {
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
        crate::application::metadata::load(&self.state, run_id)
            .map(|metadata| {
                metadata.is_some_and(|metadata| metadata.trigger_source == "graph_call")
            })
            .map_err(|error| GraphError::Unsupported(format!("child metadata: {error:?}")))
    }

    fn verify_child_plugins(
        &self,
        graph: &str,
        bindings: &[PluginBinding],
    ) -> Result<(), GraphError> {
        let path = self.path(graph)?;
        let ids = bindings
            .iter()
            .map(|binding| binding.id.clone())
            .collect::<Vec<_>>();
        let current = anchor_graph_host::FilePluginCatalog::new(path).resolve(&ids)?;
        if current != bindings {
            return Err(GraphError::RunConflict);
        }
        Ok(())
    }

    fn begin_child_execution<'a>(
        &'a self,
        run_id: &'a str,
        graph: &'a str,
        parent_cancellation: anchor_runtime_rig::Cancellation,
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
        graph: &'a str,
        status: RunStatus,
    ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(application) = &self.application {
                application
                    .child_finished(run_id, graph, status)
                    .await
                    .map_err(|error| {
                        GraphError::Unsupported(format!("resume waiting parent: {error:?}"))
                    })?;
            }
            Ok(())
        })
    }
}
