//! Compatibility surface for callers of the Artifact adapter. Mutable state is
//! owned exclusively by the neutral Workspace service.
use super::*;

impl HostArtifacts {
    pub(crate) fn workspaces(&self) -> crate::workspaces::Workspaces {
        crate::workspaces::Workspaces::new(self.workspace_root.clone())
    }

    pub(crate) fn workspace_path(&self, key: &InvocationKey) -> Result<PathBuf, GraphError> {
        self.workspaces().path(key)
    }

    pub(crate) fn prepare_workspace(
        &self,
        key: &InvocationKey,
        _inputs: &[CommitRef],
    ) -> Result<PathBuf, GraphError> {
        self.workspaces().prepare(key)
    }
}
