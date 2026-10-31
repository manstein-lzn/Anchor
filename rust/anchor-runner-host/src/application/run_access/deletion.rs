//! Deletion consumes the owning store through the Run access boundary. Native
//! reservations exclude controls and worker registration during host cleanup.
use super::{RunView, kernel_runs::deletion::KernelDeletion};
use crate::application::{ApplicationError, RunApplication};
use std::path::Path;

pub(in crate::application) struct RunDeletionBatch {
    kernel: Option<KernelDeletion>,
}

impl RunDeletionBatch {
    fn remove(&self, id: &str) -> Result<(), ApplicationError> {
        if let Some(deletion) = self
            .kernel
            .as_ref()
            .filter(|deletion| deletion.contains(id))
        {
            deletion.remove(id)?;
        }
        Ok(())
    }
}

impl RunApplication {
    pub(in crate::application) fn reserve_run_deletions(
        &self,
        ids: &[String],
    ) -> Result<RunDeletionBatch, ApplicationError> {
        let Some(runs) = self.kernel_runs(false)? else {
            return Ok(RunDeletionBatch { kernel: None });
        };
        let mut kernel_ids = Vec::new();
        for id in ids {
            if self.kernel_owner(id)?.is_some() {
                kernel_ids.push(id.clone());
            }
        }
        Ok(RunDeletionBatch {
            kernel: if kernel_ids.is_empty() {
                None
            } else {
                Some(runs.reserve_deletion(&kernel_ids)?)
            },
        })
    }

    pub(in crate::application) fn delete_cascaded_run(
        &self,
        run_id: &str,
        record: &RunView<'_>,
        workspace_root: &Path,
        deletion: &RunDeletionBatch,
    ) -> Result<(), String> {
        self.release_run_workspace_facts(
            run_id,
            &crate::workspaces::Workspaces::new(workspace_root.into()),
        )
        .map_err(|error| format!("{error:?}"))?;
        crate::run_data::delete_run_data(
            &record.record,
            &record
                .frontier
                .iter()
                .map(|dispatch| dispatch.key.clone())
                .collect::<Vec<_>>(),
            &self.data_root,
            workspace_root,
        )?;
        deletion
            .remove(run_id)
            .map_err(|error| format!("{error:?}"))?;
        self.release_run_deliveries(run_id, workspace_root)
            .map_err(|error| format!("{error:?}"))?;
        crate::run_data::remove_run_files(&self.data_root, run_id)
    }

    pub(in crate::application) fn delete_inactive_run(
        &self,
        run_id: &str,
        workspace_root: &Path,
        active: &(),
    ) -> Result<(), ApplicationError> {
        let deletion = self.reserve_run_deletions(&[run_id.to_owned()])?;
        let view = self.read_run(run_id)?.ok_or(ApplicationError::Missing)?;
        if view.active() {
            return Err(ApplicationError::Conflict(
                "that Run is still running".into(),
            ));
        }
        let record = &view.record;
        let metadata = self.metadata(run_id)?;
        self.reject_pending_session_delivery(run_id)?;
        match metadata
            .as_ref()
            .filter(|metadata| metadata.conversation.is_some())
        {
            Some(metadata) => self.ensure_conversation_delete_allowed(&view, metadata, active)?,
            None => self.reject_conversation_successor(run_id)?,
        }
        if !record.graph_calls.is_empty() {
            return Err(ApplicationError::Conflict(
                "Run is referenced by or owns a Graph call".into(),
            ));
        }
        for (other_id, other) in self.raw_records()? {
            if other_id != run_id
                && other
                    .graph_calls
                    .values()
                    .any(|call| call.child_run_id.as_deref() == Some(run_id))
            {
                return Err(ApplicationError::Conflict(
                    "Run is referenced by another Graph call".into(),
                ));
            }
        }
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
        // Publish lineage evidence before cleanup so interrupted deletion leaves
        // an explained gap and may be retried with the retained owner facts.
        if let Some(metadata) = metadata.as_ref()
            && let Some(source) = metadata.conversation.as_ref()
        {
            crate::conversation_resources::retain(&self.data_root, &view, metadata)?;
            crate::run_deletions::save(
                &self.data_root,
                &crate::run_deletions::RunDeletion::new(
                    run_id,
                    &crate::run_metadata::graph_identity(&metadata.bundle_source)?
                        .to_string_lossy(),
                    &source.session,
                    &source.reply_node,
                    source.previous_run.as_deref(),
                ),
            )
            .map_err(ApplicationError::Storage)?;
            crate::conversation_resources::release_unreferenced(
                self,
                &self.data_root,
                &crate::run_metadata::graph_identity(&metadata.bundle_source)?,
                run_id,
                workspace_root,
            )?;
        } else if let Some(metadata) = &metadata {
            for node in record
                .snapshot
                .nodes
                .iter()
                .filter(|node| node.agent.is_some())
            {
                crate::goose_acp::remove_conversation(
                    &workspace_root.join(".goose-process"),
                    &metadata.node_conversation_key(&node.id),
                )
                .map_err(ApplicationError::Storage)?;
            }
        }
        self.release_run_workspace_facts(
            run_id,
            &crate::workspaces::Workspaces::new(workspace_root.into()),
        )?;
        crate::run_data::delete_run_data(
            record,
            &view
                .frontier
                .iter()
                .map(|dispatch| dispatch.key.clone())
                .collect::<Vec<_>>(),
            &self.data_root,
            workspace_root,
        )
        .map_err(ApplicationError::Storage)?;
        deletion.remove(run_id)?;
        self.release_run_deliveries(run_id, workspace_root)?;
        crate::run_data::remove_run_files(&self.data_root, run_id)
            .map_err(ApplicationError::Storage)
    }
}
