//! Run admission and control. Graph routing and node scheduling belong to GraphRunner.
pub(crate) mod abandon;
mod assistants;
pub(crate) use abandon::AbandonAction;
mod conversations;
pub(crate) use assistants::AssistantAdmission;
pub(crate) mod graphs;
pub(crate) mod metadata;
mod plugins;
pub(crate) mod session_calls;
pub(crate) use conversations::{ConversationAdmission, WECOM_REPLY_KEY_PREFIX, WECOM_REPLY_KIND};
pub(crate) use metadata::ChannelRunSource;
pub(crate) use metadata::ConversationSource;
pub(crate) use metadata::RunMetadata;
pub(crate) use metadata::RunTrigger;

use crate::{HostControl, create_durable_directory, execution::PreparedExecution, run_data};
use anchor_graph_host::{
    FileGraphBundleLoader, FilePluginCatalog, LoadedGraphBundle, PluginCatalog,
};
use anchor_runtime::graph::{
    FileRunStore, GraphError, GraphRunRecord, GraphSnapshot, InvocationKey, RecoveryDecision,
    RunLease, RunStatus, RunStore,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

#[derive(Debug)]
pub(crate) enum ApplicationError {
    Missing,
    Conflict(String),
    Invalid(String),
    Storage(String),
}

pub(crate) struct AdmissionOptions {
    pub(crate) objective: Option<String>,
    pub(crate) trigger: RunTrigger,
    pub(crate) pilot: Option<metadata::PilotRunSource>,
    pub(crate) oauth_owner: Option<String>,
}

impl From<GraphError> for ApplicationError {
    fn from(error: GraphError) -> Self {
        match error {
            GraphError::RunBusy(_) | GraphError::RunConflict => Self::Conflict(error.to_string()),
            _ => Self::Storage(error.to_string()),
        }
    }
}

struct ActiveRun {
    graph_path: PathBuf,
    control: HostControl,
}

#[derive(Clone)]
pub(crate) struct RunApplication {
    data_root: PathBuf,
    catalog_root: PathBuf,
    configured_graph: Option<(String, PathBuf)>,
    active: Arc<Mutex<HashMap<String, ActiveRun>>>,
    catalog_mutations: Arc<Mutex<()>>,
    executor: Option<tokio::runtime::Handle>,
}

impl RunApplication {
    pub(crate) fn new(data_root: PathBuf, catalog_root: PathBuf) -> Self {
        Self {
            data_root,
            catalog_root,
            configured_graph: None,
            active: Arc::new(Mutex::new(HashMap::new())),
            catalog_mutations: Arc::new(Mutex::new(())),
            executor: tokio::runtime::Handle::try_current().ok(),
        }
    }

    /// Serialize Graph definition mutations with reference checks and deletion.
    /// Run admission remains protected by each Graph's own lease.
    pub(crate) async fn graph_catalog_mutation_guard(&self) -> OwnedMutexGuard<()> {
        Arc::clone(&self.catalog_mutations).lock_owned().await
    }

    pub(crate) fn with_configured_graph(mut self, name: String, bundle_root: PathBuf) -> Self {
        self.configured_graph = Some((name, bundle_root));
        self
    }

    pub(crate) fn graph_bundle_path(&self, name: &str) -> PathBuf {
        self.configured_graph
            .as_ref()
            .filter(|(configured_name, _)| configured_name == name)
            .map(|(_, path)| path.clone())
            .unwrap_or_else(|| self.catalog_root.join(name))
    }

    pub(crate) fn graph_identity(path: &Path) -> Result<PathBuf, ApplicationError> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().map_err(storage)?.join(path)
        };
        let mut normalized = PathBuf::new();
        for component in absolute.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                other => normalized.push(other.as_os_str()),
            }
        }
        let mut missing = VecDeque::new();
        let mut ancestor = normalized.as_path();
        while !ancestor.exists() {
            let name = ancestor.file_name().ok_or_else(|| {
                ApplicationError::Invalid("Graph bundle path has no existing ancestor".into())
            })?;
            missing.push_front(name.to_os_string());
            ancestor = ancestor.parent().ok_or_else(|| {
                ApplicationError::Invalid("Graph bundle path has no parent".into())
            })?;
        }
        let mut identity = ancestor.canonicalize().map_err(storage)?;
        for component in missing {
            identity.push(component);
        }
        Ok(identity)
    }

    fn graph_lease(&self, graph_path: &Path) -> Result<Box<dyn RunLease>, ApplicationError> {
        let identity = Self::graph_identity(graph_path)?;
        let id = format!(
            "graph-{:x}",
            Sha256::digest(identity.as_os_str().as_encoded_bytes())
        );
        Ok(FileRunStore::new(self.data_root.join("admission-locks")).acquire_lease(&id)?)
    }

    pub(crate) async fn recover_detached_at_startup(&self) -> Result<(), ApplicationError> {
        self.recover_detached().await?;
        // An abandon request accepted just before a crash must still make its
        // Run terminal: it is the precondition for replacing a Graph's Plugin
        // resources, and channel assistant recovery follows this step.
        self.finalize_recorded_abandons_at_startup().await?;
        // A delivery receipt can be committed just before the service exits,
        // leaving its wait parent parked. Reuse ordinary child completion.
        for (id, _) in self.records()? {
            if let Some(metadata) = self.metadata(&id)?
                && metadata
                    .session_call
                    .as_ref()
                    .is_some_and(|call| call.status != "pending")
            {
                self.child_finished(&id, &metadata.graph, RunStatus::Completed)
                    .await?;
            }
        }
        Ok(())
    }

    fn store(&self) -> FileRunStore {
        FileRunStore::new(self.data_root.join("runs"))
    }

    pub(crate) async fn active_runs(&self, graph: Option<&str>) -> Vec<String> {
        let graph_path =
            graph.and_then(|name| Self::graph_identity(&self.graph_bundle_path(name)).ok());
        self.active
            .lock()
            .await
            .iter()
            .filter(|(_, active)| {
                graph_path
                    .as_ref()
                    .is_none_or(|path| &active.graph_path == path)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub(crate) async fn control_requested(&self, run_id: &str) -> Option<&'static str> {
        let active = self.active.lock().await;
        let run = active.get(run_id)?;
        if run.control.cancellation.load(Ordering::Relaxed) {
            Some("stop")
        } else if run.control.pause.load(Ordering::Relaxed) {
            Some("pause")
        } else {
            None
        }
    }

    pub(crate) async fn run_is_active(&self, run_id: &str) -> bool {
        self.active.lock().await.contains_key(run_id)
    }

    pub(crate) fn metadata(&self, run_id: &str) -> Result<Option<RunMetadata>, ApplicationError> {
        metadata::load(&self.data_root, run_id)
    }

    /// FileRunStore's persisted modification time, not an independent audit clock.
    pub(crate) fn run_updated(&self, run_id: &str) -> Result<String, ApplicationError> {
        if run_id.is_empty()
            || run_id == "."
            || run_id == ".."
            || !run_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        {
            return Err(ApplicationError::Invalid("invalid Run id".into()));
        }
        let modified =
            std::fs::metadata(self.data_root.join("runs").join(format!("{run_id}.json")))
                .and_then(|metadata| metadata.modified())
                .map_err(storage)?;
        Ok(chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339())
    }

    pub(crate) fn child_metadata(
        &self,
        parent_run: &str,
    ) -> Result<Vec<RunMetadata>, ApplicationError> {
        let root = self.data_root.join("run-metadata");
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut children = Vec::new();
        for entry in std::fs::read_dir(root).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            let Some(run_id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .map(str::to_owned)
            else {
                continue;
            };
            if let Some(metadata) = self.metadata(&run_id)?
                && metadata
                    .graph_call
                    .as_ref()
                    .is_some_and(|source| source.parent_run == parent_run)
            {
                children.push(metadata);
            }
        }
        children.sort_by(|a, b| a.run_id.cmp(&b.run_id));
        Ok(children)
    }

    /// Stop every child Run a parked parent still waits on, plus their pending
    /// Session deliveries, exactly as an explicit `stop` does.
    pub(super) async fn stop_wait_children(
        &self,
        run_id: &str,
        wait_children: Vec<String>,
    ) -> Result<(), ApplicationError> {
        self.stop_wait_session_children(run_id).await?;
        for child_id in wait_children {
            if self.store().load(&child_id)?.is_some_and(|child| {
                !matches!(
                    child.status,
                    RunStatus::Completed
                        | RunStatus::Failed
                        | RunStatus::Aborted
                        | RunStatus::Stopped
                )
            }) {
                let _ = Box::pin(self.control(&child_id, "stop")).await;
            }
        }
        Ok(())
    }

    pub(crate) fn records(&self) -> Result<Vec<(String, GraphRunRecord)>, ApplicationError> {
        let root = self.data_root.join("runs");
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in std::fs::read_dir(root).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".json")
                && let Some(record) = self.store().load(id)?
            {
                records.push((id.to_owned(), record));
            }
        }
        records.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(records)
    }

    pub(crate) async fn has_unfinished_plugin_run(
        &self,
        graph_path: &Path,
    ) -> Result<bool, ApplicationError> {
        let graph_path = Self::graph_identity(graph_path)?;
        let active = self.active.lock().await;
        for (run_id, record) in self.records()? {
            if record.plugin_bindings.is_empty()
                || matches!(
                    record.status,
                    RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
                )
            {
                continue;
            }
            if let Some(metadata) = self.metadata(&run_id)?
                && Self::graph_identity(&metadata.bundle_source)? == graph_path
            {
                if self.superseded_conversation_ancestor(&run_id)?.is_some()
                    && self.ensure_predecessor_settled(&record, &active).is_ok()
                {
                    continue;
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Runs whose immutable Graph identity resolves to this exact bundle path.
    /// Runs without metadata cannot be attributed and are left untouched.
    pub(crate) fn runs_for_graph(
        &self,
        graph_path: &Path,
    ) -> Result<Vec<(String, GraphRunRecord)>, ApplicationError> {
        let mut runs = Vec::new();
        for (id, record) in self.records()? {
            let Some(metadata) = self.metadata(&id)? else {
                continue;
            };
            if Self::graph_identity(&metadata.bundle_source)? == graph_path {
                runs.push((id, record));
            }
        }
        Ok(runs)
    }

    /// Acquire the shared Graph lease, retrying briefly so an in-flight child
    /// admission is not surfaced as a spurious delete conflict.
    pub(crate) async fn acquire_graph_lease_waiting(
        &self,
        graph_path: &Path,
    ) -> Result<Box<dyn RunLease>, ApplicationError> {
        for attempt in 0..100 {
            match self.graph_lease(graph_path) {
                Ok(lease) => return Ok(lease),
                Err(ApplicationError::Conflict(_)) if attempt < 99 => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(ApplicationError::Conflict(
            "Graph is busy with an admission; retry the operation".into(),
        ))
    }

    fn snapshot_calls_graph(
        &self,
        snapshot: &GraphSnapshot,
        target_path: &Path,
    ) -> Result<bool, ApplicationError> {
        for node in &snapshot.nodes {
            let Some(op_name) = &node.op else { continue };
            let Some(graph) = snapshot.ops[op_name]
                .get("call")
                .and_then(|call| call.get("graph"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if Self::graph_identity(&self.graph_bundle_path(graph))? == target_path {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn graph_bundles(&self) -> Result<Vec<(String, LoadedGraphBundle)>, ApplicationError> {
        let mut candidates = Vec::new();
        if self.catalog_root.is_dir() {
            for entry in std::fs::read_dir(&self.catalog_root).map_err(storage)? {
                let entry = entry.map_err(storage)?;
                if entry.file_type().map_err(storage)?.is_dir() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    candidates.push((name, entry.path()));
                }
            }
        }
        if let Some((name, path)) = &self.configured_graph {
            candidates.push((name.clone(), path.clone()));
        }
        candidates.sort_by(|left, right| left.0.cmp(&right.0));

        let mut seen = BTreeSet::new();
        let mut bundles = Vec::new();
        for (name, path) in candidates {
            let identity = Self::graph_identity(&path)?;
            if !seen.insert(identity) {
                continue;
            }
            if let Ok(bundle) = FileGraphBundleLoader::new(path).load() {
                bundles.push((name, bundle));
            }
        }
        Ok(bundles)
    }

    fn graph_callers(&self, target_path: &Path) -> Result<Vec<String>, ApplicationError> {
        let mut callers = BTreeSet::new();
        for (name, bundle) in self.graph_bundles()? {
            if self.snapshot_calls_graph(&bundle.snapshot, target_path)? {
                callers.insert(format!("Graph `{name}`"));
            }
        }
        for (run_id, record) in self.records()? {
            if is_unfinished(record.status)
                && self.snapshot_calls_graph(&record.snapshot, target_path)?
            {
                let graph = self
                    .metadata(&run_id)?
                    .map(|metadata| metadata.graph)
                    .unwrap_or_else(|| "unknown Graph".into());
                callers.insert(format!("unfinished Run `{run_id}` of `{graph}`"));
            }
        }
        Ok(callers.into_iter().collect())
    }

    /// Delete an unreferenced Graph and its own terminal Run data.
    /// Current Graph definitions and unfinished Run snapshots keep their call
    /// targets alive; completed history does not prevent deletion.
    pub(crate) async fn delete_graph(
        &self,
        bundle_path: &Path,
        workspace_root: &Path,
    ) -> Result<usize, ApplicationError> {
        let graph_path = Self::graph_identity(bundle_path)?;
        let graph_lease = self.acquire_graph_lease_waiting(&graph_path).await?;
        self.delete_graph_with_lease(bundle_path, workspace_root, graph_lease)
            .await
    }

    pub(crate) async fn delete_graph_with_lease(
        &self,
        bundle_path: &Path,
        workspace_root: &Path,
        _graph_lease: Box<dyn RunLease>,
    ) -> Result<usize, ApplicationError> {
        let graph_path = Self::graph_identity(bundle_path)?;
        let callers = self.graph_callers(&graph_path)?;
        if !callers.is_empty() {
            return Err(ApplicationError::Conflict(format!(
                "cannot delete this Graph while it is called by {}",
                callers.join(", ")
            )));
        }
        let active = self.active.lock().await;
        let targets = self.runs_for_graph(&graph_path)?;
        for (run_id, record) in &targets {
            self.reject_pending_session_delivery(run_id)?;
            if active.contains_key(run_id) {
                return Err(ApplicationError::Conflict(
                    "Graph still has active execution".into(),
                ));
            }
            if is_unfinished(record.status) {
                let conversation = self
                    .metadata(run_id)?
                    .is_some_and(|metadata| metadata.conversation.is_some());
                if !conversation
                    || (record.status != RunStatus::Stopped
                        && !self.has_conversation_successor(run_id)?)
                {
                    return Err(ApplicationError::Conflict(format!(
                        "Graph has unfinished Run `{run_id}`; resume it, or stop and delete its record before deleting the Graph"
                    )));
                }
            }
            if self
                .metadata(run_id)?
                .is_some_and(|metadata| metadata.conversation.is_some())
            {
                self.ensure_conversation_delete_settled(record, &active)?;
            }
        }
        for (run_id, _) in &targets {
            self.retire_assistant_for_deletion(run_id)?;
        }
        let mut conversations = std::collections::BTreeMap::new();
        for (run_id, record) in &targets {
            if let Some(metadata) = self.metadata(run_id)? {
                for node in &record.snapshot.nodes {
                    if let Some(hint) = crate::node_host::conversation_hint_for(&metadata, &node.id)
                    {
                        conversations.entry(hint.key.clone()).or_insert(hint);
                    }
                }
            }
        }
        for hint in conversations.into_values() {
            crate::goose_acp::remove_conversation(
                &workspace_root.join(".goose-process"),
                &hint.key,
            )
            .map_err(ApplicationError::Storage)?;
            run_data::remove_legacy_conversation(&self.data_root, &hint.key)
                .map_err(ApplicationError::Storage)?;
        }
        for (id, record) in &targets {
            // Cascade deletion removes whole lineages, but a Host that dies
            // halfway through must still leave every removed link explained.
            if let Some(metadata) = self.metadata(id)?
                && let Some(source) = metadata.conversation.as_ref()
            {
                crate::run_deletions::save(
                    &self.data_root,
                    &crate::run_deletions::RunDeletion::new(
                        id,
                        &Self::graph_identity(&metadata.bundle_source)?.to_string_lossy(),
                        &source.session,
                        &source.reply_node,
                        source.previous_run.as_deref(),
                    ),
                )
                .map_err(ApplicationError::Storage)?;
            }
            run_data::delete_run_data(record, &self.data_root, workspace_root)
                .map_err(ApplicationError::Storage)?;
            run_data::remove_run_files(&self.data_root, id).map_err(ApplicationError::Storage)?;
        }

        // Remove the bundle itself while still holding the Graph lease, so no
        // admission can slip in between the Run cleanup and the directory drop.
        if bundle_path.is_dir() {
            std::fs::remove_dir_all(bundle_path).map_err(storage)?;
        }
        Ok(targets.len())
    }

    pub(crate) async fn delete_run(
        &self,
        run_id: &str,
        workspace_root: &Path,
    ) -> Result<(), ApplicationError> {
        let active = self.active.lock().await;
        if active.contains_key(run_id) {
            return Err(ApplicationError::Conflict(
                "that Run is still running".into(),
            ));
        }
        let store = self.store();
        let record = store.load(run_id)?.ok_or(ApplicationError::Missing)?;
        let metadata = self.metadata(run_id)?;
        self.reject_pending_session_delivery(run_id)?;
        if let Some(metadata) = metadata.as_ref() {
            self.ensure_channel_delivery_settled(metadata)?;
        }
        match metadata
            .as_ref()
            .filter(|metadata| metadata.conversation.is_some())
        {
            // A conversation Run may only go once every later Run has stopped
            // reading it; see `ensure_conversation_delete_allowed`.
            Some(metadata) => {
                self.ensure_conversation_delete_allowed(&record, metadata, &active)?;
            }
            None => self.reject_conversation_successor(run_id)?,
        }
        if !record.graph_calls.is_empty() {
            return Err(ApplicationError::Conflict(
                "Run is referenced by or owns a Graph call".into(),
            ));
        }
        for (other_id, other) in self.records()? {
            if other_id != run_id
                && other.graph_calls.values().any(|call| {
                    call.child_run_id.as_deref() == Some(run_id)
                        && call.status != anchor_runtime::graph::GraphCallStatus::Deleted
                })
            {
                return Err(ApplicationError::Conflict(
                    "Run is referenced by another Graph call".into(),
                ));
            }
        }
        let _lease = store.acquire_lease(run_id)?;
        let artifacts = crate::HostArtifacts::new(
            self.data_root.join("artifacts"),
            workspace_root.to_path_buf(),
        );
        for result in record.results.values().flatten() {
            if self
                .data_root
                .join("artifacts")
                .join(&result.commit.id)
                .exists()
            {
                artifacts
                    .list_files(&result.commit)
                    .map_err(|error| ApplicationError::Invalid(error.to_string()))?;
            }
            if result.key.run_id != run_id
                || result.key.graph_digest != record.graph_digest
                || result.commit.node_id != result.key.node_id
                || result.commit.invocation != result.key.invocation
            {
                return Err(ApplicationError::Invalid(
                    "Run artifact identity mismatch".into(),
                ));
            }
        }
        // Durable before anything is removed: a Host that dies in the middle of
        // this cleanup leaves an explained lineage gap, not corruption.
        if let Some(metadata) = metadata.as_ref()
            && let Some(source) = metadata.conversation.as_ref()
        {
            crate::run_deletions::save(
                &self.data_root,
                &crate::run_deletions::RunDeletion::new(
                    run_id,
                    &Self::graph_identity(&metadata.bundle_source)?.to_string_lossy(),
                    &source.session,
                    &source.reply_node,
                    source.previous_run.as_deref(),
                ),
            )
            .map_err(ApplicationError::Storage)?;
        }
        run_data::delete_run_data(&record, &self.data_root, workspace_root)
            .map_err(ApplicationError::Storage)?;
        run_data::remove_run_files(&self.data_root, run_id).map_err(ApplicationError::Storage)
    }

    pub(crate) async fn admit(
        &self,
        graph: String,
        source: &Path,
        mut bundle: LoadedGraphBundle,
        input: Value,
        options: AdmissionOptions,
        graph_lease: Box<dyn RunLease>,
    ) -> Result<String, ApplicationError> {
        options
            .trigger
            .validate()
            .map_err(ApplicationError::Invalid)?;
        if let Some(objective) = options.objective.filter(|value| !value.is_empty()) {
            bundle.snapshot.objective = objective;
        }
        let graph_path = Self::graph_identity(source)?;
        let mut active = self.active.lock().await;
        if active.values().any(|run| run.graph_path == graph_path) {
            return Err(ApplicationError::Conflict(
                "this graph is already running".into(),
            ));
        }
        // A service restart does not silently launch another Run over an orphan.
        for (id, record) in self.records()? {
            if matches!(
                record.status,
                RunStatus::Ready
                    | RunStatus::Running
                    | RunStatus::Paused
                    | RunStatus::WaitingCall
                    | RunStatus::WaitingRecovery
                    | RunStatus::BudgetStopped
                    | RunStatus::Stopped
            ) {
                match self.metadata(&id)? {
                    Some(meta)
                        if Self::graph_identity(&meta.bundle_source)? == graph_path =>
                    {
                        return Err(ApplicationError::Conflict(
                            "this graph has an unfinished Run; resume it, or stop and delete its record".into(),
                        ));
                    }
                    None => return Err(ApplicationError::Conflict(
                        "an unfinished Run has no immutable Graph identity; admission is quarantined".into(),
                    )),
                    Some(_) => {}
                }
            }
        }
        let run_id = format!("rust-{:x}", metadata::now_nanos());
        let mut record = GraphRunRecord::create_with_id(bundle.snapshot, input, run_id.clone())?;
        record.plugin_bindings = bundle
            .plugins
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect();
        record.plugin_bindings_initialized = true;
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
        self.check_store()?;
        let mut metadata = RunMetadata::new(
            run_id.clone(),
            graph.clone(),
            record.graph_digest.clone(),
            source,
        )?;
        options.trigger.apply(&mut metadata);
        metadata.pilot = options.pilot;
        metadata.oauth_owner = options.oauth_owner;
        create_durable_directory(&self.data_root.join("runs")).map_err(storage)?;
        let lease = self.store().acquire_lease(&run_id)?;
        // Metadata is saved first: a crash can leave unused metadata, never an accepted
        // Run without its immutable Graph identity. The response follows both saves.
        metadata::save(&self.data_root, &metadata)?;
        self.store().save(&record)?;
        active.insert(
            run_id.clone(),
            ActiveRun {
                graph_path,
                control,
            },
        );
        drop(lease);
        // The durable snapshot and immutable identity now serialize future
        // admissions. The catalog lock only protects reading/writing the bundle.
        drop(graph_lease);
        self.spawn(record, execution)?;
        Ok(run_id)
    }

    pub(crate) fn graph_admission_lease(
        &self,
        graph_path: &Path,
    ) -> Result<Box<dyn RunLease>, ApplicationError> {
        let graph_path = Self::graph_identity(graph_path)?;
        let lease = self.graph_lease(&graph_path)?;
        // An unfinished Run with no immutable Graph identity cannot be attributed
        // to any Graph, so refuse to admit or create Runs until it is resolved.
        for (id, record) in self.records()? {
            if is_unfinished(record.status) && self.metadata(&id)?.is_none() {
                return Err(ApplicationError::Conflict(
                    "an unfinished Run has no immutable Graph identity; admission is quarantined"
                        .into(),
                ));
            }
        }
        Ok(lease)
    }

    fn check_store(&self) -> Result<(), ApplicationError> {
        self.executor()?;
        // The host uses deployment environment, never HTTP-supplied paths.
        let expected = self
            .data_root
            .canonicalize()
            .or_else(|_| {
                create_durable_directory(&self.data_root)?;
                self.data_root.canonicalize()
            })
            .map_err(storage)?;
        let configured = crate::env_path("ANCHOR_RUNNER_STATE_ROOT")
            .map_err(ApplicationError::Invalid)?
            .canonicalize()
            .map_err(storage)?;
        if expected != configured {
            return Err(ApplicationError::Invalid(
                "host state root does not match application state root".into(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn control(
        &self,
        run_id: &str,
        operation: &str,
    ) -> Result<(), ApplicationError> {
        self.control_expected(run_id, operation, None).await
    }

    async fn control_expected(
        &self,
        run_id: &str,
        operation: &str,
        waiting: Option<(&str, u64)>,
    ) -> Result<(), ApplicationError> {
        if !matches!(operation, "pause" | "resume" | "stop") {
            return Err(ApplicationError::Invalid("unknown Run control".into()));
        }
        if operation == "resume" && self.abandon_intent(run_id)?.is_some() {
            // Abandonment is one-way: a resumed Run would continue the round the
            // operator gave up. The intent is durable, so this holds across a
            // restart and while the terminal write is still in flight.
            return Err(ApplicationError::Conflict(
                "that Run was abandoned; start a new Run instead of resuming it".into(),
            ));
        }
        if self
            .metadata(run_id)?
            .is_some_and(|metadata| metadata.session_call.is_some())
        {
            match operation {
                "resume" | "pause" => {
                    return Err(ApplicationError::Conflict(
                        "Session calls are coordinated through the Session host".into(),
                    ));
                }
                "stop" => return self.stop_session_call(run_id).await,
                _ => unreachable!(),
            }
        }
        // An unresolved tool result is part of the Agent's recovery context,
        // not a separate operator workflow. Resume the same Harness run with
        // a durable observation that tells the Agent to inspect the effect;
        // the Agent can then continue, compensate, or ask the user normally.
        // This deliberately never replays the uncertain tool automatically.
        if operation == "resume"
            && let Some(record) = self.store().load(run_id)?
            && record.status == RunStatus::WaitingRecovery
            && !record.recovery.is_empty()
        {
            // Resolve every pending branch as one ordinary resume. Each
            // synthetic observation is part of that Agent's context; no
            // attempt id or operator decision is exposed to the user.
            for pending in record.recovery {
                let observation = format!(
                    "The previous `{}` tool call (step {}) was interrupted before its result was recorded. Its external effect is unknown. Inspect the workspace, artifacts, and any available external state before deciding what to do; do not blindly repeat the call.",
                    pending.attempt.tool, pending.attempt.step
                );
                self.recover(
                    run_id,
                    &pending.key.node_id,
                    pending.key.invocation,
                    pending.attempt.attempt_id,
                    RecoveryDecision::Completed { observation },
                )
                .await?;
            }
            return Ok(());
        }
        let mut active = self.active.lock().await;
        if let Some((node, invocation)) = waiting {
            let expected = self.store().load(run_id)?.is_some_and(|parent| {
                parent.status == RunStatus::WaitingCall
                    && parent.cursor.as_ref().is_some_and(|cursor| {
                        cursor.node_id == node && cursor.key.invocation == invocation
                    })
            });
            if !expected || active.contains_key(run_id) {
                return Ok(());
            }
        }
        if operation == "resume" {
            self.reject_superseded_conversation_execution(run_id)?;
        }
        if let Some(run) = active.get(run_id) {
            match operation {
                "pause" => run.control.pause.store(true, Ordering::Relaxed),
                "stop" => run.control.cancellation.store(true, Ordering::Relaxed),
                _ => {
                    return Err(ApplicationError::Conflict(
                        "that Run is already active".into(),
                    ));
                }
            }
            if operation == "stop" {
                drop(active);
                self.stop_wait_session_children(run_id).await?;
            }
            return Ok(());
        }
        let record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        if record.status == RunStatus::WaitingRecovery && operation != "stop" {
            return Err(ApplicationError::Conflict(
                "Run recovery context is not ready".into(),
            ));
        }
        if matches!(
            record.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
        ) {
            return Err(ApplicationError::Conflict(
                "terminal Run cannot be controlled".into(),
            ));
        }
        if operation == "pause" {
            return Err(ApplicationError::Conflict("that Run is not active".into()));
        }
        let metadata = self.metadata(run_id)?.ok_or_else(|| {
            ApplicationError::Invalid(
                "Run has no immutable Graph identity metadata; it cannot be resumed".into(),
            )
        })?;
        if metadata.graph_digest != record.graph_digest {
            return Err(ApplicationError::Invalid(
                "Run Graph identity metadata does not match its frozen snapshot".into(),
            ));
        }
        let is_child = metadata.trigger_source == "graph_call";
        let graph_path = Self::graph_identity(&metadata.bundle_source)?;
        if operation == "resume" {
            self.ensure_current_assistant(&metadata)?;
            self.check_execution_scope(&metadata, &active)?;
        }
        let graph_lease = if is_child {
            None
        } else {
            Some(self.graph_lease(&metadata.bundle_source)?)
        };
        let lease = self.store().acquire_lease(run_id)?;
        let mut record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        if matches!(
            record.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
        ) {
            return Err(ApplicationError::Conflict(
                "terminal Run cannot be controlled".into(),
            ));
        }
        if operation == "stop" && record.status == RunStatus::WaitingCall {
            record.status = RunStatus::Stopped;
            self.store().save(&record)?;
            let wait_children = wait_child_ids(&record);
            drop(active);
            drop(lease);
            self.stop_wait_children(run_id, wait_children).await?;
            let lease = self.store().acquire_lease(run_id)?;
            if let Some(mut current) = self.store().load(run_id)?
                && current.status == RunStatus::WaitingCall
            {
                current.status = RunStatus::Stopped;
                self.store().save(&current)?;
            }
            drop(lease);
            return Ok(());
        }
        if operation == "stop" {
            // Inactive stop preserves cursors and all completion facts; it dispatches no work.
            record.status = RunStatus::Stopped;
            self.store().save(&record)?;
            self.settle_channel_run(run_id, RunStatus::Stopped)?;
            return Ok(());
        }
        if record.status == RunStatus::BudgetStopped {
            return Err(ApplicationError::Conflict(
                "budget-stopped Run requires an explicit budget policy".into(),
            ));
        }
        let ids = record.plugin_bindings.keys().cloned().collect::<Vec<_>>();
        if !ids.is_empty() {
            let resolved = FilePluginCatalog::new(&metadata.bundle_source)
                .resolve(&ids)
                .map_err(|e| {
                    ApplicationError::Invalid(format!("original Plugin resources unavailable: {e}"))
                })?;
            let bindings = resolved.into_iter().map(|p| (p.id.clone(), p)).collect();
            if record.plugin_bindings != bindings {
                return Err(ApplicationError::Invalid(
                    "Plugin manifest changed since this Run was admitted".into(),
                ));
            }
        }
        let control = new_control();
        let execution = PreparedExecution::prepare_with_catalog(
            &record,
            control.clone(),
            record.plugin_bindings.clone(),
            self.catalog_root.clone(),
            self.clone(),
        )
        .map_err(ApplicationError::Invalid)?;
        self.check_store()?;
        active.insert(
            run_id.to_owned(),
            ActiveRun {
                graph_path,
                control,
            },
        );
        drop(lease);
        drop(graph_lease);
        self.spawn(record, execution)?;
        Ok(())
    }

    pub(crate) async fn recover(
        &self,
        run_id: &str,
        node_id: &str,
        invocation: u64,
        attempt_id: i64,
        decision: RecoveryDecision,
    ) -> Result<(), ApplicationError> {
        if self
            .metadata(run_id)?
            .is_some_and(|metadata| metadata.session_call.is_some())
        {
            return Err(ApplicationError::Conflict(
                "Session calls recover through the Session host".into(),
            ));
        }
        if attempt_id <= 0 || invocation == 0 || node_id.is_empty() {
            return Err(ApplicationError::Invalid(
                "node_id, invocation, and positive attempt_id are required".into(),
            ));
        }
        if self.abandon_intent(run_id)?.is_some() {
            // A recorded abandon request is one-way: a recovery decision must
            // not restart the round the operator gave up on.
            return Err(ApplicationError::Conflict(
                "that Run was abandoned; start a new Run instead of recovering it".into(),
            ));
        }
        let mut active = self.active.lock().await;
        self.reject_superseded_conversation_execution(run_id)?;
        let mut record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        let metadata = self.metadata(run_id)?.ok_or_else(|| {
            ApplicationError::Invalid(
                "Run has no immutable Graph identity metadata; it cannot be recovered".into(),
            )
        })?;
        if metadata.graph_digest != record.graph_digest {
            return Err(ApplicationError::Invalid(
                "Run Graph identity metadata does not match its frozen snapshot".into(),
            ));
        }
        let key = InvocationKey {
            run_id: run_id.to_owned(),
            graph_digest: record.graph_digest.clone(),
            node_id: node_id.to_owned(),
            invocation,
        };
        if let Some(existing) = record
            .recovery_submissions
            .iter()
            .find(|submission| submission.key == key && submission.attempt_id == attempt_id)
        {
            if existing.decision != decision {
                return Err(ApplicationError::Conflict(
                    "a different decision is already recorded for this attempt".into(),
                ));
            }
            // The response may have been lost after the Run was committed as
            // Running but before its task entered the active map. Replaying the
            // same decision must be able to kick that same durable cursor.
            if record.status == RunStatus::Running && !active.contains_key(run_id) {
                drop(active);
                // The same decision may be submitted again after its response
                // was lost. Re-enter control through one heap indirection so
                // the idempotent kick does not create an infinitely sized
                // async future (control itself may handle a waiting record).
                Box::pin(self.control(run_id, "resume")).await?;
            }
            return Ok(());
        }
        self.check_execution_scope(&metadata, &active)?;
        if active.contains_key(run_id) {
            return Err(ApplicationError::Conflict("Run is already active".into()));
        }
        let is_pending = record
            .recovery
            .iter()
            .any(|pending| pending.key == key && pending.attempt.attempt_id == attempt_id);
        let active_cursor = record
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.key == key)
            || record.parallel.as_ref().is_some_and(|activation| {
                activation.branches.iter().any(|branch| {
                    branch
                        .cursor
                        .as_ref()
                        .is_some_and(|cursor| cursor.key == key)
                })
            });
        if !active_cursor {
            return Err(ApplicationError::Conflict(
                "recovery invocation does not match an active cursor in this Run".into(),
            ));
        }
        if !is_pending {
            return Err(ApplicationError::Conflict(
                "attempt is not pending recovery for this Run".into(),
            ));
        }
        if is_pending && record.status != RunStatus::WaitingRecovery {
            return Err(ApplicationError::Conflict(
                "Run is not waiting for a recovery decision".into(),
            ));
        }
        if !record
            .snapshot
            .nodes
            .iter()
            .any(|node| node.id == node_id && node.agent.is_some())
        {
            return Err(ApplicationError::Conflict(
                "only an AgentNode can own an io-harness recovery attempt".into(),
            ));
        }
        let graph_path = Self::graph_identity(&metadata.bundle_source)?;
        let _graph_lease = self.graph_lease(&metadata.bundle_source)?;
        let _lease = self.store().acquire_lease(run_id)?;
        record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        if let Some(existing) = record
            .recovery_submissions
            .iter()
            .find(|submission| submission.key == key && submission.attempt_id == attempt_id)
        {
            return if existing.decision == decision {
                Ok(())
            } else {
                Err(ApplicationError::Conflict(
                    "a different decision is already recorded for this attempt".into(),
                ))
            };
        }
        let still_pending = record
            .recovery
            .iter()
            .any(|pending| pending.key == key && pending.attempt.attempt_id == attempt_id);
        let still_active = record
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.key == key)
            || record.parallel.as_ref().is_some_and(|activation| {
                activation.branches.iter().any(|branch| {
                    branch
                        .cursor
                        .as_ref()
                        .is_some_and(|cursor| cursor.key == key)
                })
            });
        if !still_active || !still_pending || record.status != RunStatus::WaitingRecovery {
            return Err(ApplicationError::Conflict(
                "Run recovery state changed before the decision was accepted".into(),
            ));
        }
        let control = new_control();
        let is_abort = decision == RecoveryDecision::Abort;
        // The Harness intent is the first durable side of the handoff. If the
        // following Graph write fails or the host dies, the still-pending Graph
        // attempt can safely resubmit this exact idempotent decision.
        let execution = self.prepare_resume(&record, &metadata, control.clone())?;
        execution
            .record_recovery_decision(&key, attempt_id, decision.clone())
            .map_err(ApplicationError::Conflict)?;
        record
            .recovery_submissions
            .push(anchor_runtime::graph::RecoverySubmission {
                key: key.clone(),
                attempt_id,
                decision: decision.clone(),
            });
        record
            .recovery
            .retain(|pending| !(pending.key == key && pending.attempt.attempt_id == attempt_id));
        if is_abort {
            // Abort is a Run-level stop: make the Run terminal before touching
            // any other branch. Remaining unresolved attempts are retained as
            // read-only facts and no cached Retry/Completed intent is executed.
            // The Harness abort intent and journal close are already durable.
            record.status = RunStatus::Aborted;
            record.error = Some(format!("{} aborted by operator", node_id));
            self.store().save(&record)?;
            return Ok(());
        }
        if !record.recovery.is_empty() {
            record.status = RunStatus::WaitingRecovery;
            self.store().save(&record)?;
            return Ok(());
        }
        record.status = RunStatus::Running;
        self.store().save(&record)?;
        self.check_store()?;
        active.insert(
            run_id.to_owned(),
            ActiveRun {
                graph_path,
                control,
            },
        );
        drop(_lease);
        drop(_graph_lease);
        self.spawn(record, execution)?;
        Ok(())
    }

    fn prepare_resume(
        &self,
        record: &GraphRunRecord,
        metadata: &RunMetadata,
        control: HostControl,
    ) -> Result<PreparedExecution, ApplicationError> {
        self.ensure_current_assistant(metadata)?;
        let ids = record.plugin_bindings.keys().cloned().collect::<Vec<_>>();
        if !ids.is_empty() {
            let resolved = FilePluginCatalog::new(&metadata.bundle_source)
                .resolve(&ids)
                .map_err(|e| {
                    ApplicationError::Invalid(format!("original Plugin resources unavailable: {e}"))
                })?;
            let bindings = resolved.into_iter().map(|p| (p.id.clone(), p)).collect();
            if record.plugin_bindings != bindings {
                return Err(ApplicationError::Invalid(
                    "Plugin manifest changed since this Run was admitted".into(),
                ));
            }
        }
        PreparedExecution::prepare_with_catalog(
            record,
            control,
            record.plugin_bindings.clone(),
            self.catalog_root.clone(),
            self.clone(),
        )
        .map_err(ApplicationError::Invalid)
    }

    pub(crate) async fn dispatch_detached(&self, run_id: &str) -> Result<(), ApplicationError> {
        let mut active = self.active.lock().await;
        if active.contains_key(run_id) {
            return Ok(());
        }
        let metadata = self.metadata(run_id)?.ok_or(ApplicationError::Missing)?;
        if metadata.session_call.is_some() {
            return Ok(());
        }
        let source = metadata.graph_call.as_ref().ok_or_else(|| {
            ApplicationError::Invalid("detached child has no durable Graph call source".into())
        })?;
        if source.mode != "detach" || metadata.trigger_source != "graph_call" {
            return Err(ApplicationError::Invalid(
                "Run is not an admitted detached child".into(),
            ));
        }
        let lease = self.store().acquire_lease(run_id)?;
        let record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        if record.status != RunStatus::Ready {
            // Running means an execution may have escaped before a crash. Never
            // infer that it is safe to replay.
            return Ok(());
        }
        if record.graph_digest != metadata.graph_digest {
            return Err(ApplicationError::Invalid(
                "detached child snapshot digest changed".into(),
            ));
        }
        let control = new_control();
        let execution = self.prepare_resume(&record, &metadata, control.clone())?;
        active.insert(
            run_id.to_owned(),
            ActiveRun {
                graph_path: Self::graph_identity(&metadata.bundle_source)?,
                control,
            },
        );
        drop(lease);
        drop(active);
        self.spawn(record, execution)?;
        Ok(())
    }

    pub(crate) async fn recover_detached(&self) -> Result<(), ApplicationError> {
        let candidates = self
            .records()?
            .into_iter()
            .filter_map(|(id, record)| (record.status == RunStatus::Ready).then_some(id))
            .collect::<Vec<_>>();
        for run_id in candidates {
            if self
                .metadata(&run_id)?
                .and_then(|m| m.graph_call)
                .is_some_and(|source| source.mode == "detach")
            {
                self.dispatch_detached(&run_id).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn begin_wait_child(
        &self,
        run_id: &str,
        graph: &str,
        parent_cancellation: anchor_runtime::Cancellation,
    ) -> Result<anchor_graph_host::ChildRunControl, ApplicationError> {
        let mut active = self.active.lock().await;
        if let Some(existing) = active.get(run_id) {
            return Ok(anchor_graph_host::ChildRunControl {
                cancellation: existing.control.cancellation.clone(),
                pause: existing.control.pause.clone(),
            });
        }
        let graph_path = self
            .metadata(run_id)?
            .map(|metadata| Self::graph_identity(&metadata.bundle_source))
            .transpose()?
            .unwrap_or_else(|| {
                Self::graph_identity(&self.graph_bundle_path(graph))
                    .expect("validated configured Graph path")
            });
        let control = new_control();
        active.insert(
            run_id.to_owned(),
            ActiveRun {
                graph_path,
                control: control.clone(),
            },
        );
        let cancel = control.cancellation.clone();
        self.executor()?.spawn(async move {
            while !parent_cancellation.load(Ordering::Acquire) {
                if cancel.load(Ordering::Acquire) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            cancel.store(true, Ordering::Release);
        });
        Ok(anchor_graph_host::ChildRunControl {
            cancellation: control.cancellation,
            pause: control.pause,
        })
    }

    pub(crate) async fn end_wait_child(&self, run_id: &str) {
        self.active.lock().await.remove(run_id);
    }

    pub(crate) fn child_finished<'a>(
        &'a self,
        run_id: &'a str,
        _graph: &str,
        status: RunStatus,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ApplicationError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if status != RunStatus::Completed {
                return Ok(());
            }
            let metadata = self.metadata(run_id)?.ok_or(ApplicationError::Missing)?;
            if metadata
                .session_call
                .as_ref()
                .is_some_and(|call| call.status == "pending")
            {
                return Ok(());
            }
            let Some(source) = metadata.graph_call else {
                return Ok(());
            };
            if source.mode != "wait" {
                return Ok(());
            }
            if self.active.lock().await.contains_key(&source.parent_run) {
                // Inline wait execution returns the child result directly to the
                // still-running parent Runner; only independently resumed child
                // Runs need to kick a WaitingCall parent.
                return Ok(());
            }
            if let Some(parent) = self.store().load(&source.parent_run)?
                && parent.status == RunStatus::WaitingCall
                && parent.cursor.as_ref().is_some_and(|cursor| {
                    cursor.node_id == source.node && cursor.key.invocation == source.invocation
                })
            {
                let application = self.clone();
                let parent_run = source.parent_run.clone();
                self.executor()?.spawn(async move {
                    tokio::task::yield_now().await;
                    if let Err(error) = application
                        .control_expected(
                            &parent_run,
                            "resume",
                            Some((&source.node, source.invocation)),
                        )
                        .await
                    {
                        eprintln!("waiting parent {parent_run} could not resume: {error:?}");
                    }
                });
            }
            Ok(())
        })
    }

    fn executor(&self) -> Result<&tokio::runtime::Handle, ApplicationError> {
        self.executor.as_ref().ok_or_else(|| {
            ApplicationError::Invalid("Run execution requires the owning Host executor".into())
        })
    }

    fn spawn(
        &self,
        record: GraphRunRecord,
        execution: PreparedExecution,
    ) -> Result<(), ApplicationError> {
        let application = self.clone();
        self.executor()?.spawn(async move {
            let run_id = record.run_id.clone();
            let result = execution.run(record).await;
            let (graph, status) = match result {
                Ok(record) => (record.snapshot.objective.clone(), record.status),
                Err(error) => {
                    eprintln!("Run {run_id} execution failed: {error}");
                    let status = application
                        .store()
                        .load(&run_id)
                        .ok()
                        .flatten()
                        .map_or(RunStatus::Failed, |record| record.status);
                    (String::new(), status)
                }
            };
            let finished = application.active.lock().await.remove(&run_id);
            if finished.is_some_and(|run| run.control.cancellation.load(Ordering::Acquire)) {
                if let Err(error) = application.stop_wait_session_children(&run_id).await {
                    eprintln!("Run {run_id} wait cancellation failed: {error:?}");
                }
            } else if status == RunStatus::WaitingCall {
                // Settlement can race the parent's final WaitingCall write. Once the
                // execution slot is released, recheck the durable child facts once.
                if let Ok(children) = application.child_metadata(&run_id) {
                    for child in children {
                        if child
                            .session_call
                            .as_ref()
                            .is_some_and(|call| call.status != "pending")
                        {
                            let _ = application
                                .child_finished(&child.run_id, &child.graph, RunStatus::Completed)
                                .await;
                        }
                    }
                }
            }
            // A recorded abandon request makes the Run terminal even though the
            // Runner could only stop it: either the cancellation this request
            // caused, or a completion that won the race first.
            let status = match application.finalize_recorded_abandon(&run_id).await {
                Ok(Some(status)) => status,
                Ok(None) => status,
                Err(error) => {
                    eprintln!("Run {run_id} abandon finalization failed: {error:?}");
                    status
                }
            };
            if let Err(error) = application.settle_channel_run(&run_id, status) {
                eprintln!("Run {run_id} channel Turn settlement failed: {error:?}");
            }
            if let Err(error) = application.child_finished(&run_id, &graph, status).await {
                eprintln!("Run {run_id} completion callback failed: {error:?}");
            }
        });
        Ok(())
    }
}

fn new_control() -> HostControl {
    HostControl {
        cancellation: Arc::new(AtomicBool::new(false)),
        pause: Arc::new(AtomicBool::new(false)),
    }
}

fn is_unfinished(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Ready
            | RunStatus::Running
            | RunStatus::Paused
            | RunStatus::WaitingCall
            | RunStatus::WaitingRecovery
            | RunStatus::BudgetStopped
            | RunStatus::Stopped
    )
}

/// Child Runs a parked parent still waits on. Only `wait` calls remain part of
/// the caller's execution; a detach boundary has independent authority.
fn wait_child_ids(record: &GraphRunRecord) -> Vec<String> {
    record
        .graph_calls
        .values()
        .filter(|call| call.mode == "wait")
        .filter_map(|call| call.child_run_id.clone())
        .collect()
}

fn storage(error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_control_projects_active_tokens_with_stop_priority() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("state");
        let application = RunApplication::new(data_root.clone(), root.path().join("catalog"));
        let control = new_control();
        application.active.lock().await.insert(
            "active-run".into(),
            ActiveRun {
                graph_path: root.path().join("graph"),
                control: control.clone(),
            },
        );
        assert_eq!(application.control_requested("missing").await, None);
        assert_eq!(application.control_requested("active-run").await, None);
        control.pause.store(true, Ordering::Relaxed);
        assert_eq!(
            application.control_requested("active-run").await,
            Some("pause")
        );
        control.cancellation.store(true, Ordering::Relaxed);
        assert_eq!(
            application.control_requested("active-run").await,
            Some("stop")
        );
        application.active.lock().await.remove("active-run");
        assert_eq!(application.control_requested("active-run").await, None);
        assert!(!data_root.exists());
    }
}
