use super::{ApplicationError, RunApplication};

impl RunApplication {
    pub(crate) async fn recover_detached_at_startup(&self) -> Result<(), ApplicationError> {
        // Finish first creation before HTTP can report healthy or retention can run.
        self.kernel_runs(true)?;
        self.recover_detached().await?;
        // An abandon request accepted just before a crash must still make its
        // Run terminal: it is the precondition for replacing a Graph's Plugin
        // resources, and channel assistant recovery follows this step.
        self.finalize_recorded_abandons_at_startup().await?;
        // A delivery receipt can be committed just before the service exits,
        // leaving its wait parent parked. Reuse ordinary child completion.
        for (id, _) in self.run_views()? {
            if let Some(metadata) = self.metadata(&id)?
                && metadata
                    .session_call
                    .as_ref()
                    .is_some_and(|call| call.status != "pending")
            {
                self.release_parent_after_recorded_delivery(&id, &metadata.graph)
                    .await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn recover_detached(&self) -> Result<(), ApplicationError> {
        let candidates = self
            .run_views()?
            .into_iter()
            .filter_map(|(id, record)| record.can_dispatch().then_some(id))
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
}
