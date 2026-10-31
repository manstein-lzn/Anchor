use super::{ApplicationError, RunApplication, graphs, metadata, storage};
use crate::run_data;
use anchor_runtime::graph::{GraphSnapshot, RunLease};
use serde_json::Value;
use std::{collections::BTreeSet, path::Path, time::Duration};

impl RunApplication {
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
            if crate::run_metadata::graph_identity(&self.graph_bundle_path(graph))? == target_path {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every Graph that calls `target_path`, or a refusal when the catalog could
    /// not be read completely.
    ///
    /// Deletion protection may not read an unreadable Graph as "nobody calls
    /// this": the candidate set is the one impact scan, which names every Graph
    /// it could not load, and an incomplete scan refuses the deletion instead of
    /// allowing it.
    fn graph_callers(&self, target_path: &Path) -> Result<Vec<String>, ApplicationError> {
        let mut callers = BTreeSet::new();
        let (bundles, considered, total, missing) = match self.impact_scan() {
            graphs::ImpactScan::Read {
                bundles,
                considered,
                total,
                missing,
            } => (bundles, considered, total, missing),
            graphs::ImpactScan::Unreadable(reason) => {
                return Err(ApplicationError::Conflict(format!(
                    "cannot delete this Graph: the Graph catalog could not be read to find out who calls it ({reason})"
                )));
            }
        };
        for (name, bundle) in &bundles {
            if self.snapshot_calls_graph(&bundle.snapshot, target_path)? {
                callers.insert(format!("Graph `{name}`"));
            }
        }
        for (run_id, record) in self.run_views()? {
            if (!record.is_settled() || record.active())
                && self.snapshot_calls_graph(record.progress().snapshot, target_path)?
            {
                let graph = self
                    .metadata(&run_id)?
                    .map(|metadata| metadata.graph)
                    .unwrap_or_else(|| "unknown Graph".into());
                callers.insert(format!("unfinished Run `{run_id}` of `{graph}`"));
            }
        }
        // A named caller already refuses the deletion. With no caller found, an
        // unreadable Graph is the difference between "nobody calls this" and "we
        // could not tell", and only the first may delete.
        if callers.is_empty() {
            match graphs::coverage_of(considered, total, missing) {
                graphs::ImpactCoverage::Complete => {}
                graphs::ImpactCoverage::Partial {
                    considered,
                    total,
                    missing,
                } => {
                    return Err(ApplicationError::Conflict(format!(
                        "cannot delete this Graph: considered {considered} of {total} Graphs and could not read {}, which may call it",
                        missing
                            .iter()
                            .map(|gap| format!("`{}` ({})", gap.graph, gap.reason))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
                graphs::ImpactCoverage::Unknown { reason } => {
                    return Err(ApplicationError::Conflict(format!(
                        "cannot delete this Graph: who calls it could not be determined ({reason})"
                    )));
                }
            }
        }
        Ok(callers.into_iter().collect())
    }

    /// Delete an unreferenced Graph and its own terminal Run data.
    /// Current Graph definitions and unfinished Run snapshots keep their call
    /// targets alive; completed history does not prevent deletion.
    pub(crate) async fn delete_graph(
        &self,
        graph_name: &str,
        bundle_path: &Path,
        workspace_root: &Path,
    ) -> Result<usize, ApplicationError> {
        let graph_path = crate::run_metadata::graph_identity(bundle_path)?;
        let graph_lease = self.acquire_graph_lease_waiting(&graph_path).await?;
        self.delete_graph_with_lease(graph_name, bundle_path, workspace_root, graph_lease)
            .await
    }

    /// `graph_name` is the catalog name; the configured Graph's bundle
    /// directory may be named otherwise, and the bot is bound by name.
    pub(crate) async fn delete_graph_with_lease(
        &self,
        graph_name: &str,
        bundle_path: &Path,
        workspace_root: &Path,
        _graph_lease: Box<dyn RunLease>,
    ) -> Result<usize, ApplicationError> {
        let graph_path = crate::run_metadata::graph_identity(bundle_path)?;
        let callers = self.graph_callers(&graph_path)?;
        if !callers.is_empty() {
            return Err(ApplicationError::Conflict(format!(
                "cannot delete this Graph while it is called by {}",
                callers.join(", ")
            )));
        }
        self.check_channel_graph_deletion(graph_name)?;
        let active = self.admission.lock().await;
        let mut targets = self.runs_for_graph(&graph_path)?;
        let deletion = self
            .reserve_run_deletions(&targets.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>())?;
        // Native registration and controls cannot pass the reservation. Read
        // again so every precondition observes the reserved authority version.
        for (id, record) in &mut targets {
            *record = self.read_run(id)?.ok_or(ApplicationError::Missing)?;
        }
        // A durable hand-off may have metadata before its Run is admitted. It
        // still owns history and cannot be erased by an empty cascade.
        for id in metadata::ids(&self.data_root)? {
            if let Some(retained) = self.metadata(&id)?
                && crate::run_metadata::graph_identity(&retained.bundle_source)? == graph_path
                && !targets.iter().any(|(run, _)| run == &id)
            {
                return Err(ApplicationError::Conflict(format!(
                    "Graph has retained hand-off `{id}` without its Run; settle it before deletion"
                )));
            }
        }
        for (run_id, record) in &targets {
            self.reject_pending_session_delivery(run_id)?;
            if record.active() {
                return Err(ApplicationError::Conflict(
                    "Graph still has active execution".into(),
                ));
            }
            if !record.is_settled() {
                let conversation = self
                    .metadata(run_id)?
                    .is_some_and(|metadata| metadata.conversation.is_some());
                if !conversation
                    || (!record.is_quiescent() && !self.has_conversation_successor(run_id)?)
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
        let mut conversations = BTreeSet::new();
        for (run_id, record) in &targets {
            if let Some(metadata) = self.metadata(run_id)? {
                crate::conversation_resources::retain(&self.data_root, record, &metadata)?;
                for node in &record.progress().snapshot.nodes {
                    conversations.insert(metadata.node_conversation_key(&node.id));
                }
            }
        }
        conversations.extend(crate::conversation_resources::for_graph(
            &self.data_root,
            &graph_path,
        )?);
        for hint in conversations {
            crate::goose_acp::remove_conversation(&workspace_root.join(".goose-process"), &hint)
                .map_err(ApplicationError::Storage)?;
            run_data::remove_legacy_conversation(&self.data_root, &hint)
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
                        &crate::run_metadata::graph_identity(&metadata.bundle_source)?
                            .to_string_lossy(),
                        &source.session,
                        &source.reply_node,
                        source.previous_run.as_deref(),
                    ),
                )
                .map_err(ApplicationError::Storage)?;
            }
            self.delete_cascaded_run(id, record, workspace_root, &deletion)
                .map_err(ApplicationError::Storage)?;
        }

        // Remove the bundle itself while still holding the Graph lease, so no
        // admission can slip in between the Run cleanup and the directory drop.
        crate::assistant_plugins::cleanup_publications(&self.data_root, &graph_path)
            .map_err(ApplicationError::Storage)?;
        // A stopped bot goes with its Graph: binding, bot credentials and the
        // gateway's state (D6).
        self.remove_channel_binding(graph_name).await?;
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
        // Match admission's lock order. A saved hand-off may precede its Run,
        // so the Graph lease must cover reference inspection and cleanup.
        let _graph_lease = match self.metadata(run_id)? {
            Some(metadata) if metadata.conversation.is_some() => Some(
                self.acquire_graph_lease_waiting(&metadata.bundle_source)
                    .await?,
            ),
            _ => None,
        };
        let active = self.admission.lock().await;
        if self.read_run(run_id)?.is_some_and(|view| view.active()) {
            return Err(ApplicationError::Conflict(
                "that Run is still running".into(),
            ));
        }
        self.delete_inactive_run(run_id, workspace_root, &active)
    }

    /// One retention pass (design card 003): reclaim settled Runs past the
    /// policy through `delete_run`, so every refusal it has (referenced,
    /// conversation current, reply undelivered) holds here too.
    pub(crate) async fn reclaim(
        &self,
        policy: &crate::retention::Policy,
        workspace_root: &Path,
        now: i64,
    ) -> Result<crate::retention::Report, ApplicationError> {
        let mut report = crate::retention::Report {
            facts_bytes: crate::retention::facts_bytes(&self.data_root),
            ..Default::default()
        };
        if policy.disabled() {
            return Ok(report);
        }
        let artifacts =
            crate::HostArtifacts::new(self.data_root.join("artifacts"), workspace_root.into());
        let before = artifacts
            .storage_bytes()
            .map_err(|error| ApplicationError::Storage(error.to_string()))?
            + self
                .run_views()?
                .iter()
                .map(|(_, view)| self.run_bytes(&view.progress(), workspace_root))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .sum::<u64>();
        let collected = self.reclaim_deliveries(policy, workspace_root, now)?;
        report.expired_artifacts = collected.expired.len();
        let mut settled = Vec::new();
        let mut total = collected.remaining_bytes;
        for (id, view) in self.run_views()? {
            let bytes = self.run_bytes(&view.progress(), workspace_root)?;
            total += bytes;
            // Only a settled Run that no worker holds is a candidate; a waiting
            // Run still has a human, a child or an effect that may answer it.
            if view.active() || !matches!(view.phase(), super::run_access::RunPhase::Settled { .. })
            {
                continue;
            }
            let Some(settled_at) = view
                .updated()
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.timestamp())
            else {
                // Without an observed settle time its age is unknown: only the
                // size limit may reclaim it, as the oldest.
                settled.push(crate::retention::Candidate {
                    run_id: id,
                    settled_at: i64::MIN,
                    bytes,
                });
                continue;
            };
            settled.push(crate::retention::Candidate {
                run_id: id,
                settled_at,
                bytes,
            });
        }
        report.remaining_bytes = total;
        for candidate in crate::retention::select(policy, settled, total, now) {
            match self.delete_run(&candidate.run_id, workspace_root).await {
                Ok(()) => {
                    report.removed += 1;
                }
                Err(error) => report.skipped.push((
                    candidate.run_id,
                    match error {
                        ApplicationError::Missing => "already gone".into(),
                        ApplicationError::Conflict(reason)
                        | ApplicationError::Invalid(reason)
                        | ApplicationError::Storage(reason) => reason,
                    },
                )),
            }
        }
        report.remaining_bytes = artifacts
            .storage_bytes()
            .map_err(|error| ApplicationError::Storage(error.to_string()))?
            + self
                .run_views()?
                .iter()
                .map(|(_, view)| self.run_bytes(&view.progress(), workspace_root))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .sum::<u64>();
        report.freed_bytes = before.saturating_sub(report.remaining_bytes);
        if policy
            .max_bytes
            .is_some_and(|limit| report.remaining_bytes > limit)
        {
            report.skipped.push((
                "storage".into(),
                "remaining data is retained or required by current work".into(),
            ));
        }
        Ok(report)
    }

    /// Only Run-owned working data. Artifact objects are measured once for
    /// the entire store; explicitly shared Workspaces have their own lifecycle.
    fn run_bytes(
        &self,
        run: &super::run_access::RunProgress<'_>,
        workspace_root: &Path,
    ) -> Result<u64, ApplicationError> {
        let workspaces = crate::workspaces::Workspaces::new(workspace_root.into());
        let mut bytes = crate::retention::size(&workspace_root.join(run.run_id))
            + crate::retention::size(&self.data_root.join("spill").join(run.run_id));
        for resource in workspaces
            .resources()?
            .into_iter()
            .filter(|resource| resource.owner_run.as_deref() == Some(run.run_id))
        {
            bytes += crate::retention::size(&workspaces.resource_path(&resource.resource_id)?);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod retention_tests {
    use super::super::run_access::fixtures;
    use super::*;
    use crate::retention::Policy;

    const DAY: i64 = 86_400;

    fn application() -> (tempfile::TempDir, RunApplication) {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
        std::fs::create_dir_all(&application.catalog_root).unwrap();
        (root, application)
    }

    /// Settle time as the Host's observer records it, moved into the past.
    fn settled_at(application: &RunApplication, id: &str, at: i64) {
        let at = chrono::DateTime::from_timestamp(at, 0).unwrap();
        std::fs::write(
            application
                .data_root
                .join("kernel-run-observations")
                .join(format!("{id}.json")),
            serde_json::json!({"updated": at.to_rfc3339()}).to_string(),
        )
        .unwrap();
    }

    fn by_age(days: u64) -> Policy {
        Policy {
            max_age: Some(Duration::from_secs(days * DAY as u64)),
            max_bytes: None,
        }
    }

    fn exists(application: &RunApplication, id: &str) -> bool {
        application.read_run(id).unwrap().is_some()
    }

    #[tokio::test]
    async fn age_reclaims_only_settled_runs_past_it_and_never_live_ones() {
        // T1 and T3: every phase, all of them "old".
        let (root, application) = application();
        let work = root.path().join("work");
        let ids = fixtures::scenarios(&application);
        let now = 100 * DAY;
        for id in ids {
            settled_at(&application, id, now - 60 * DAY);
        }
        fixtures::create(&application, &fixtures::work_record("recent"));
        fixtures::finish(&application, "recent");
        settled_at(&application, "recent", now - DAY);
        let report = application.reclaim(&by_age(30), &work, now).await.unwrap();
        // Done, failed and aborted are settled; the rest still execute or wait.
        assert_eq!(report.removed, 3, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        for id in ["done", "failed", "aborted"] {
            assert!(!exists(&application, id), "{id}");
        }
        for id in [
            "executing",
            "paused",
            "stopped",
            "budget",
            "recovery",
            "child",
            "recent",
        ] {
            assert!(exists(&application, id), "{id}");
        }
    }

    #[tokio::test]
    async fn size_reclaims_the_oldest_settled_runs_until_the_rest_fit() {
        // T2: three settled Runs of 1 KB of workspace each, a limit of 2 KB.
        let (root, application) = application();
        let work = root.path().join("work");
        for (index, id) in ["first", "second", "third"].into_iter().enumerate() {
            fixtures::create(&application, &fixtures::work_record(id));
            fixtures::finish(&application, id);
            settled_at(&application, id, DAY * (index as i64 + 1));
            std::fs::create_dir_all(work.join(id)).unwrap();
            std::fs::write(work.join(id).join("output"), vec![0u8; 1024]).unwrap();
        }
        let policy = Policy {
            max_age: None,
            max_bytes: Some(2048),
        };
        let report = application.reclaim(&policy, &work, 10 * DAY).await.unwrap();
        assert_eq!(report.removed, 1, "{report:?}");
        assert_eq!(report.freed_bytes, 1024);
        assert_eq!(report.remaining_bytes, 2048);
        assert!(!exists(&application, "first"));
        assert!(!work.join("first").exists());
        assert!(exists(&application, "second") && exists(&application, "third"));
    }

    #[tokio::test]
    async fn hub_facts_and_grades_stay_and_disabled_retention_deletes_nothing() {
        // T5 and T7.
        let (root, application) = application();
        let work = root.path().join("work");
        fixtures::create(&application, &fixtures::work_record("old"));
        fixtures::finish(&application, "old");
        settled_at(&application, "old", 0);
        for name in ["facts", "grades"].map(|kind| format!("kernel-{kind}")) {
            let path = application.data_root.join(name).join("old");
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("history.jsonl"), b"{}\n").unwrap();
        }
        let off = Policy {
            max_age: None,
            max_bytes: None,
        };
        let report = application.reclaim(&off, &work, 1_000 * DAY).await.unwrap();
        assert_eq!(report.removed, 0);
        assert!(exists(&application, "old"));
        let report = application
            .reclaim(&by_age(30), &work, 1_000 * DAY)
            .await
            .unwrap();
        assert_eq!(report.removed, 1);
        assert!(report.facts_bytes > 0);
        for name in ["facts", "grades"].map(|kind| format!("kernel-{kind}")) {
            assert!(
                application
                    .data_root
                    .join(name)
                    .join("old/history.jsonl")
                    .is_file()
            );
        }
    }

    #[tokio::test]
    async fn a_run_the_deletion_path_refuses_is_skipped_with_its_reason() {
        // T4 and T6: the deletion path refuses a settled Run that an open
        // parent's Graph call still binds. Reclamation skips it, names why,
        // and goes on with the next one.
        let (root, application) = application();
        let work = root.path().join("work");
        for id in ["bound-child", "free"] {
            fixtures::create(&application, &fixtures::work_record(id));
            fixtures::finish(&application, id);
            settled_at(&application, id, 0);
        }
        let parent = anchor_runtime::graph::GraphRunRecord::create_with_id(
            GraphSnapshot::from_authoring(serde_json::json!({
                "entry":"invoke", "agents":{}, "ops":{"invoke":{"call":{"graph":"child","mode":"wait"}}},
                "nodes":[{"id":"invoke","op":"invoke"}], "edges":[]
            }))
            .unwrap(),
            serde_json::json!({}),
            "parent",
        )
        .unwrap();
        fixtures::create(&application, &parent);
        fixtures::bind_child(&application, "parent", "invoke", "bound-child");
        let report = application
            .reclaim(&by_age(30), &work, 1_000 * DAY)
            .await
            .unwrap();
        assert_eq!(report.removed, 1, "{report:?}");
        assert_eq!(report.skipped.len(), 1, "{report:?}");
        assert_eq!(report.skipped[0].0, "bound-child");
        assert!(report.skipped[0].1.contains("Graph call"), "{report:?}");
        assert!(exists(&application, "bound-child") && exists(&application, "parent"));
        assert!(!exists(&application, "free"));
        assert!(report.line().unwrap().contains("skipped 1 (bound-child:"));
    }
}
