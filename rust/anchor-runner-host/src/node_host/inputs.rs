//! Node workspace binding and frozen input authorization.
use super::*;

impl HostIoResolver {
    pub(crate) fn local_inputs_configured(&self) -> bool {
        self.local_inputs.configured()
    }

    /// Prepare the resource whose grant the upper application adapter recorded.
    pub(crate) fn prepare_node_workspace(
        &self,
        key: &InvocationKey,
        inputs: &[CommitRef],
    ) -> Result<PathBuf, String> {
        self.artifacts
            .prepare_workspace(key, inputs)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn bind_local_inputs(
        &self,
        record: &GraphRunRecord,
        graph: Option<&str>,
    ) -> Result<(), String> {
        let existing = self
            .load_run(&record.run_id)
            .map_err(|error| error.to_string())?;
        if let Some(existing) = &existing
            && (existing.snapshot != record.snapshot || existing.input != record.input)
        {
            return Err("Run local input identity differs from the admitted snapshot/input".into());
        }
        self.local_inputs.freeze(
            &record.run_id,
            &record.graph_digest,
            &record.snapshot,
            graph,
            existing.is_none(),
        )
    }

    pub(crate) fn verify_local_inputs(&self, record: &GraphRunRecord) -> Result<(), String> {
        self.local_inputs.verify(record)?;
        if self.local_inputs.configured() {
            let metadata =
                crate::run_metadata::load(self.local_inputs.state_root(), &record.run_id)
                    .map_err(|error| format!("local input Run identity unavailable: {error:?}"))?
                    .ok_or("local input authorization requires immutable Run Graph metadata")?;
            if metadata.graph_digest != record.graph_digest {
                return Err(
                    "local input Run Graph metadata differs from the admitted snapshot".into(),
                );
            }
            self.local_inputs.freeze(
                &record.run_id,
                &record.graph_digest,
                &record.snapshot,
                Some(&metadata.graph),
                false,
            )?;
        }
        Ok(())
    }

    pub(super) fn local_input_mounts(
        &self,
        key: &InvocationKey,
    ) -> Result<Vec<anchor_runtime::ReadOnlyInput>, String> {
        let record = self
            .load_run(&key.run_id)
            .map_err(|error| error.to_string())?
            .ok_or("local input mounts require a durable admitted Run")?;
        // Old Runs could not receive local inputs. Only their empty authority
        // may be recorded during migration; new grants require a new Run.
        if !self.local_inputs.configured() {
            self.local_inputs.freeze(
                &record.run_id,
                &record.graph_digest,
                &record.snapshot,
                None,
                false,
            )?;
        }
        self.verify_local_inputs(&record)?;
        let mut mounts = self.local_inputs.mounts(&record, key)?;
        if let Some(mount) = self.channel_inputs(key)?.mount {
            mounts.push(mount);
        }
        mounts.extend(
            self.pending_channel_inputs(key)?
                .into_iter()
                .filter_map(|inputs| inputs.mount),
        );
        if let Some(binding) = crate::assistant::binding(self.local_inputs.state_root(), key)?
            && let Some(notification) = binding.notification
        {
            for file in &notification.files {
                let commit = self
                    .artifacts
                    .reference(&file.artifact_id)
                    .map_err(|error| error.to_string())?
                    .ok_or("notification delivery is missing")?;
                let view = self
                    .artifacts
                    .files_path(&commit)
                    .map_err(|error| error.to_string())?;
                // Check the exact selected file; the full source delivery is never mounted.
                let _read = self
                    .artifacts
                    .open_file(&commit, &file.path)
                    .map_err(|error| error.to_string())?;
                mounts.push(anchor_runtime::ReadOnlyInput::new(
                    view.join(&file.path),
                    crate::channel_notifications::file_path(&notification.id, &file.name),
                ));
            }
        }
        Ok(mounts)
    }
}
