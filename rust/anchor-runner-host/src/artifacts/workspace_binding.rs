use super::*;

impl HostArtifacts {
    pub(crate) fn initialized_workspace_owner(
        &self,
        key: &InvocationKey,
    ) -> Result<Option<InvocationKey>, GraphError> {
        self.workspaces().initialized_owner(key)
    }

    #[cfg(test)]
    pub(crate) fn bind_node_workspace(
        &self,
        run_id: &str,
        graph_digest: &str,
        node_id: &str,
    ) -> Result<(), GraphError> {
        self.workspaces().prepare(&InvocationKey {
            run_id: run_id.into(),
            graph_digest: graph_digest.into(),
            node_id: node_id.into(),
            invocation: 1,
        })?;
        Ok(())
    }

    /// The Host has proved the same-session handover. Rebind the same mutable
    /// resource after its previous execution released it; no scene is copied.
    #[cfg(test)]
    pub(crate) fn prepare_workspace_from_handover(
        &self,
        key: &InvocationKey,
        inputs: &[CommitRef],
        source: &InvocationKey,
        source_graph: &anchor_runtime::graph::GraphSnapshot,
        target_graph: &anchor_runtime::graph::GraphSnapshot,
    ) -> Result<PathBuf, GraphError> {
        if source_graph.digest()? != source.graph_digest
            || target_graph.digest()? != key.graph_digest
            || source.node_id != key.node_id
            || source.run_id == key.run_id
            || !crate::assistant_plugins::compatible(source_graph, target_graph, &key.node_id)
        {
            return Err(corrupt("workspace handover Graph identity changed"));
        }
        let workspaces = self.workspaces();
        let resource = workspaces.resource(source)?;
        workspaces.bind_resource(key, &resource)?;
        self.prepare_workspace(key, inputs)
    }
}
