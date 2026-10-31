use super::{ApplicationError, RunApplication, RunMetadata, metadata, storage};

impl RunApplication {
    pub(crate) async fn active_runs(&self, graph: Option<&str>) -> Vec<String> {
        let graph_path = graph.and_then(|name| {
            crate::run_metadata::graph_identity(&self.graph_bundle_path(name)).ok()
        });
        let mut ids = Vec::new();
        for id in self.registered_run_ids().unwrap_or_default() {
            let matches_graph = graph_path.as_ref().is_none_or(|path| {
                self.metadata(&id).ok().flatten().is_some_and(|metadata| {
                    crate::run_metadata::graph_identity(&metadata.bundle_source)
                        .ok()
                        .as_ref()
                        == Some(path)
                })
            });
            if matches_graph && self.run_is_active(&id).await {
                ids.push(id);
            }
        }
        ids.sort();
        ids.dedup();
        ids
    }

    pub(crate) async fn run_is_active(&self, run_id: &str) -> bool {
        self.read_run(run_id)
            .ok()
            .flatten()
            .is_some_and(|run| run.active())
    }

    pub(crate) fn metadata(&self, run_id: &str) -> Result<Option<RunMetadata>, ApplicationError> {
        metadata::load(&self.data_root, run_id).map_err(Into::into)
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
}
