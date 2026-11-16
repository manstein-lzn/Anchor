use super::*;

impl HostArtifacts {
    pub(super) fn expanded_snapshots(
        &self,
        commits: &[CommitRef],
        expected: Option<(&str, &str)>,
    ) -> Result<Vec<(PathBuf, Manifest)>, GraphError> {
        anchor_runtime::artifact::store::expanded_snapshots(&self.root, commits, expected)
    }
}
