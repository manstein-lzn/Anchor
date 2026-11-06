//! Rebuild task completions and blocked notices into the Session-owned queue.
use super::{ApplicationError, RunApplication, RunMetadata, metadata, run_access, storage};
use crate::task_delegation::TaskOrigin;
use anchor_platform_session::{ChannelNotificationFile, SessionError, SessionStore, TaskResult};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(test)]
mod session_deletion_tests;
#[cfg(test)]
mod tests;

const INLINE_SUBMISSION_BYTES: usize = 32 * 1024;
const INLINE_SUBMISSIONS_BUDGET: usize = 96 * 1024;

fn result_id(run: &str) -> String {
    format!("task-{:x}", Sha256::digest(run.as_bytes()))
}

impl RunApplication {
    pub(crate) fn record_schedule_blocked(
        &self,
        origin: &TaskOrigin,
        schedule: &str,
        scheduled_at: &str,
        graph: &str,
        error: &ApplicationError,
    ) -> Result<(), ApplicationError> {
        let identity = serde_json::to_vec(&(schedule, scheduled_at)).map_err(storage)?;
        self.queue_task_input(
            origin,
            &TaskResult {
                id: format!("schedule-blocked-{:x}", Sha256::digest(identity)),
                source_run: None,
                outcome: "blocked".into(),
                content: json!({"event":"schedule.blocked","graph":graph,
                    "schedule":schedule,"scheduled_at":scheduled_at,"reason":format!("{error:?}"),
                    "request":{"graph":origin.graph,"source":origin.source},
                    "admission_confirmed":false,"retry":"The same occurrence remains pending. Inspect retained admission facts before starting another Run."}),
                files: vec![],
            },
        )?;
        Ok(())
    }

    fn queue_task_input(
        &self,
        origin: &TaskOrigin,
        result: &TaskResult,
    ) -> Result<bool, ApplicationError> {
        let Some(session) = origin.session.as_deref() else {
            return Ok(false);
        };
        let sessions = crate::assistant::sessions(&self.data_root).map_err(storage)?;
        match sessions.task_result(&origin.owner, session, &result.id) {
            Ok(Some(_)) | Err(SessionError::Missing) => return Ok(false),
            Ok(None) => {}
            Err(error) => return Err(storage(error)),
        }
        sessions
            .record_task_result(&origin.owner, session, result)
            .map_err(storage)?;
        crate::assistant::signal(session).notify_waiters();
        Ok(true)
    }

    fn record_run_blocked(
        &self,
        metadata: &RunMetadata,
        view: &run_access::RunView<'_>,
        event: &str,
        reason: &str,
    ) -> Result<bool, ApplicationError> {
        let origin = metadata.task.as_ref().ok_or(ApplicationError::Missing)?;
        let identity = serde_json::to_vec(&(&metadata.run_id, event, view.updated(), reason))
            .map_err(storage)?;
        self.queue_task_input(
            origin,
            &TaskResult {
                id: format!("task-blocked-{:x}", Sha256::digest(identity)),
                source_run: Some(metadata.run_id.clone()),
                outcome: "blocked".into(),
                content: json!({"event":event,"graph":metadata.graph,
                    "graph_digest":metadata.graph_digest,"run":metadata.run_id,
                    "request":{"graph":origin.graph,"source":origin.source},
                    "reason":reason,"terminal":view.is_settled(),
                    "read_tool":"graph_tasks_status"}),
                files: vec![],
            },
        )
    }

    pub(crate) async fn delete_task_session(
        &self,
        owner: &str,
        session: &str,
    ) -> Result<(), ApplicationError> {
        // Match the existing catalog -> admission order. Plans and new Runs
        // cannot publish a callback reference between this check and deletion.
        let _catalog = self.graph_catalog_mutation_guard().await;
        let _admission = self.admission.lock().await;
        self.check_task_origin_deletion(&|origin| {
            Ok(origin.owner == owner && origin.session.as_deref() == Some(session))
        })?;
        SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(super::assistants::assistant_session_error)?
            .delete_channel_session(owner, session)
            .map_err(super::assistants::assistant_session_error)
    }

    pub(super) fn check_task_origin_deletion(
        &self,
        matches: &dyn Fn(&TaskOrigin) -> Result<bool, ApplicationError>,
    ) -> Result<(), ApplicationError> {
        self.check_task_reference_deletion(&|_, origin| matches(origin))?;
        match self.schedules.as_deref() {
            Some(port) => port.check_task_origin_deletion(matches),
            None => Ok(()),
        }
    }

    pub(super) fn check_task_runs_deletion(
        &self,
        runs: &BTreeSet<String>,
    ) -> Result<(), ApplicationError> {
        if runs.is_empty() {
            return Ok(());
        }
        self.check_task_reference_deletion(&|metadata, _| Ok(runs.contains(&metadata.run_id)))
    }

    fn check_task_reference_deletion(
        &self,
        matches: &dyn Fn(&RunMetadata, &TaskOrigin) -> Result<bool, ApplicationError>,
    ) -> Result<(), ApplicationError> {
        let mut retained = BTreeMap::new();
        for run in metadata::ids(&self.data_root)? {
            if let Some(metadata) = self.metadata(&run)? {
                retained.insert(run, metadata);
            }
        }
        let mut descendants = BTreeMap::<&str, Vec<&str>>::new();
        for metadata in retained.values() {
            if let Some(source) = &metadata.graph_call {
                descendants
                    .entry(&source.root_run)
                    .or_default()
                    .push(&metadata.run_id);
            }
        }
        let conflict = || {
            ApplicationError::Conflict(
                "a delegated Graph task or its child is still running, or its result has not been handled"
                    .into(),
            )
        };
        for metadata in retained.values() {
            let Some(origin) = metadata.task.as_ref() else {
                continue;
            };
            if !matches(metadata, origin)? {
                continue;
            }
            if self.task_result_delivery_pending(metadata)? {
                return Err(conflict());
            }
            let mut pending = vec![metadata.run_id.clone()];
            pending.extend(
                descendants
                    .get(metadata.run_id.as_str())
                    .into_iter()
                    .flatten()
                    .map(|run| (*run).to_owned()),
            );
            let mut seen = BTreeSet::new();
            while let Some(run) = pending.pop() {
                if !seen.insert(run.clone()) {
                    continue;
                }
                let has_metadata = retained.contains_key(&run) || self.metadata(&run)?.is_some();
                let Some(view) = self.read_run(&run)? else {
                    if has_metadata {
                        return Err(conflict());
                    }
                    // A completed child Graph may already have released both
                    // its Run and metadata through the ordinary cascade.
                    continue;
                };
                if !has_metadata || !view.is_settled() || view.active() {
                    return Err(conflict());
                }
                // Static admission does not take the application gate. Read
                // terminal parents' durable children to include admissions
                // published after the initial metadata inventory.
                pending.extend(view.child_run_ids().map(str::to_owned));
            }
        }
        Ok(())
    }

    /// The queue is durable; this notifier only reduces the wait-loop latency.
    pub(crate) fn start_task_result_delivery(&self, workspace_root: PathBuf) {
        let application = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if let Err(error) = application.reconcile_task_results(&workspace_root).await {
                    eprintln!("anchor-runner-host: Graph task result delivery: {error:?}");
                }
            }
        });
    }

    /// No enqueue marker is stored beside the Session's authoritative receipt.
    /// Restart after any crash window simply retries the same terminal Run.
    pub(crate) async fn reconcile_task_results(
        &self,
        workspace_root: &Path,
    ) -> Result<usize, ApplicationError> {
        let mut delivered = 0;
        let mut failure = None;
        for run in metadata::ids(&self.data_root)? {
            match self.deliver_task_result(&run, workspace_root).await {
                Ok(true) => delivered += 1,
                Ok(false) => {}
                Err(error) => {
                    // One broken task must not starve independent Sessions.
                    eprintln!("anchor-runner-host: task Run {run}: {error:?}");
                    failure.get_or_insert(error);
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(delivered),
        }
    }

    async fn deliver_task_result(
        &self,
        run: &str,
        workspace_root: &Path,
    ) -> Result<bool, ApplicationError> {
        let Some(metadata) = self.metadata(run)? else {
            return Ok(false);
        };
        let Some(origin) = metadata.task.as_ref() else {
            return Ok(false);
        };
        let Some(session) = origin.session.as_deref() else {
            return Ok(false);
        };
        let Some(view) = self.read_run(run)? else {
            return Ok(false);
        };
        if !view.is_settled() {
            if let run_access::RunPhase::Waiting {
                for_what: run_access::RunWait::Human | run_access::RunWait::Effect,
                reason,
            } = view.phase()
            {
                return self.record_run_blocked(
                    &metadata,
                    &view,
                    "run.blocked",
                    reason.unwrap_or("task is waiting for an unresolved external effect"),
                );
            }
            return Ok(false);
        }
        if view.active() {
            return Ok(false);
        }
        let sessions = crate::assistant::sessions(&self.data_root).map_err(storage)?;
        let id = result_id(run);
        if !task_result_pending(&sessions, &origin.owner, session, run)?
            || sessions
                .task_result(&origin.owner, session, &id)
                .map_err(storage)?
                .is_some()
        {
            return Ok(false);
        }
        let result = match self.task_completion(&metadata, &view, workspace_root).await {
            Ok(result) => result,
            Err(error) => {
                self.record_run_blocked(&metadata, &view, "result.blocked", &format!("{error:?}"))?;
                return Err(error);
            }
        };
        if let Err(error) = sessions.record_task_result(&origin.owner, session, &result) {
            self.record_run_blocked(&metadata, &view, "result.blocked", &error.to_string())?;
            return Err(storage(error));
        }
        crate::assistant::signal(session).notify_waiters();
        Ok(true)
    }

    async fn task_completion(
        &self,
        metadata: &RunMetadata,
        view: &run_access::RunView<'_>,
        workspace_root: &Path,
    ) -> Result<TaskResult, ApplicationError> {
        let origin = metadata.task.as_ref().ok_or(ApplicationError::Missing)?;
        let projection = self.project_run(view.clone(), Some(metadata)).await?;
        let outcome = projection.outcome.ok_or_else(|| {
            ApplicationError::Conflict("only a terminal task has a completion result".into())
        })?;
        let progress = view.progress();
        if progress.graph_digest != metadata.graph_digest {
            return Err(ApplicationError::Conflict(
                "task Graph identity changed".into(),
            ));
        }
        let artifacts =
            crate::HostArtifacts::new(self.data_root.join("artifacts"), workspace_root.into());
        let _lease = artifacts.input_lease().map_err(storage)?;
        let mut files = Vec::new();
        let mut nodes = Vec::new();
        let mut inline_remaining = INLINE_SUBMISSIONS_BUDGET;
        let output_nodes: BTreeSet<_> = progress
            .snapshot
            .nodes
            .iter()
            .filter(|node| {
                progress
                    .results
                    .get(&node.id)
                    .is_some_and(|results| !results.is_empty())
                    && !progress
                        .snapshot
                        .edges
                        .iter()
                        .any(|edge| edge.from_node == node.id)
            })
            .map(|node| node.id.as_str())
            .collect();
        for result in progress
            .results
            .values()
            .filter_map(|results| results.last())
        {
            // Intermediate research, previews and checkpoints remain Run facts,
            // not the assistant's delivery set. Failed tasks have no final files.
            let selected = if outcome == "done" && output_nodes.contains(result.node_id.as_str()) {
                artifacts.list_files(&result.commit).map_err(storage)?
            } else {
                vec![]
            };
            for (path, _) in selected {
                let basename = Path::new(&path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        ApplicationError::Invalid("task output file has no name".into())
                    })?;
                // Module node ids contain separators; aliases are single path
                // components and remain unique even for repeated basenames.
                let name = if basename.len() <= 180 {
                    format!("{}-{basename}", files.len() + 1)
                } else {
                    let extension = Path::new(basename)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .filter(|extension| extension.len() <= 32)
                        .unwrap_or("output");
                    format!("{}.{}", files.len() + 1, extension)
                };
                files.push(ChannelNotificationFile {
                    artifact_id: result.commit.id.clone(),
                    path,
                    name,
                });
            }
            let encoded = serde_json::to_vec(&result.completion.submission).map_err(storage)?;
            // Queue metadata stays bounded; the authoritative complete value
            // remains available through this Run/node's status projection.
            let submission =
                if encoded.len() <= INLINE_SUBMISSION_BYTES && encoded.len() <= inline_remaining {
                    inline_remaining -= encoded.len();
                    json!(result.completion.submission)
                } else {
                    json!({"omitted":true,"bytes":encoded.len(),
                    "sha256":format!("{:x}", Sha256::digest(&encoded)),
                    "read_tool":"graph_tasks_status"})
                };
            nodes.push(
                json!({"node":result.node_id,"invocation":result.key.invocation,
                "artifact":result.commit,"submission":submission}),
            );
        }
        Ok(TaskResult {
            id: result_id(&metadata.run_id),
            source_run: Some(metadata.run_id.clone()),
            outcome: outcome.into(),
            content: json!({"event":"run.completed","graph":metadata.graph,"graph_digest":metadata.graph_digest,
                "run":metadata.run_id,"request":{"graph":origin.graph,"source":origin.source},
                "output_nodes":output_nodes,
                "outcome":outcome,"reason":projection.reason,"nodes":nodes}),
            files,
        })
    }

    pub(in crate::application) fn task_result_delivery_pending(
        &self,
        metadata: &RunMetadata,
    ) -> Result<bool, ApplicationError> {
        let Some(origin) = metadata.task.as_ref() else {
            return Ok(false);
        };
        let Some(session) = origin.session.as_deref() else {
            return Ok(false);
        };
        let sessions = crate::assistant::sessions(&self.data_root).map_err(storage)?;
        task_result_pending(&sessions, &origin.owner, session, &metadata.run_id)
    }

    pub(in crate::application) fn reject_pending_task_result(
        &self,
        run: &str,
    ) -> Result<(), ApplicationError> {
        if let Some(metadata) = self.metadata(run)?
            && self.task_result_delivery_pending(&metadata)?
        {
            return Err(ApplicationError::Conflict(
                "Run has an undelivered or unhandled Graph task result".into(),
            ));
        }
        Ok(())
    }
}

fn task_result_pending(
    sessions: &SessionStore,
    owner: &str,
    session: &str,
    run: &str,
) -> Result<bool, ApplicationError> {
    let id = result_id(run);
    let pending = || -> Result<bool, SessionError> {
        if sessions.task_result(owner, session, &id)?.is_none() {
            return Ok(true);
        }
        Ok(sessions
            .pending_task_results(owner, session)?
            .iter()
            .any(|result| result.id == id))
    };
    match pending() {
        Ok(pending) => Ok(pending),
        // Explicit Session deletion checked every outgoing reference before
        // removing its handled history; it must not recreate that recipient.
        Err(SessionError::Missing) => Ok(false),
        Err(error) => Err(storage(error)),
    }
}
