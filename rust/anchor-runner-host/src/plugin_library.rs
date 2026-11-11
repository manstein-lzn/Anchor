//! Host ownership of Plugin catalogs, Library construction and publication.
//! Observation adds no lock. Callers retain their existing Graph/catalog lock
//! order; read guards and publication leases cover only their original scopes.
use anchor_graph_host::{
    ChannelDefinition, FilePluginCatalog, McpServerDefinition, PluginCatalog, PluginDefinition,
    PluginMetadata, SystemPlugin,
};
use anchor_library::{
    DevelopmentError, DevelopmentScope, InstallError, InstallLease, InstallOutcome, Library,
    PluginReview,
};
use anchor_runtime::graph::{GraphError, PluginBinding};
use std::path::{Path, PathBuf};

pub(crate) mod tools;

pub(crate) struct Catalog {
    inner: FilePluginCatalog,
}

impl Catalog {
    /// An unlocked observation, including private candidate trees. The caller
    /// owns any surrounding read or publication lease; never acquire it twice.
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            inner: FilePluginCatalog::new(root),
        }
    }

    pub(crate) fn definition(&self, id: &str) -> Result<PluginDefinition, GraphError> {
        self.inner.definition(id)
    }
    pub(crate) fn metadata(&self, id: &str) -> Result<PluginMetadata, GraphError> {
        self.inner.metadata(id)
    }
    pub(crate) fn snapshot_requires_network(
        &self,
        snapshot: &anchor_runtime::graph::GraphSnapshot,
    ) -> Result<bool, GraphError> {
        self.inner.snapshot_requires_network(snapshot)
    }
    pub(crate) fn channels(&self, id: &str) -> Result<Vec<ChannelDefinition>, GraphError> {
        self.inner.channels(id)
    }
    pub(crate) fn plugin_directory(&self, id: &str) -> Result<PathBuf, GraphError> {
        self.inner.plugin_directory(id)
    }
    pub(crate) fn mcp_servers(
        &self,
        id: &str,
        resolve_env: bool,
    ) -> Result<Vec<McpServerDefinition>, GraphError> {
        self.inner.mcp_servers(id, resolve_env)
    }
    pub(crate) fn resolve_mcp_servers_with(
        &self,
        definition: &PluginDefinition,
        environment: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Vec<McpServerDefinition>, GraphError> {
        self.inner.resolve_mcp_servers_with(definition, environment)
    }

    pub(crate) fn resolve_mcp_servers(
        &self,
        definition: &PluginDefinition,
    ) -> Result<Vec<McpServerDefinition>, GraphError> {
        self.inner.resolve_mcp_servers(definition)
    }
    #[cfg(test)]
    pub(crate) fn view_path(&self, id: &str) -> Result<Option<PathBuf>, GraphError> {
        self.inner.view_path(id)
    }

    pub(crate) fn checked_definition(
        &self,
        id: &str,
        expected: ExpectedDefinition<'_>,
    ) -> Result<PluginDefinition, DefinitionError> {
        let definition = self.definition(id).map_err(DefinitionError::Read)?;
        if expected.matches(&definition) {
            Ok(definition)
        } else {
            Err(DefinitionError::Mismatch)
        }
    }
}

impl ExpectedDefinition<'_> {
    /// Also verify definitions already captured by a preparation step, without
    /// reading them again or changing the caller's validation order.
    pub(crate) fn matches(&self, definition: &PluginDefinition) -> bool {
        match self {
            ExpectedDefinition::Authorization(binding) => {
                binding.same_authorization(&definition.binding())
            }
            ExpectedDefinition::AuthorizationDigest(digest) => {
                definition.authorization_digest.as_ref() == *digest
            }
            ExpectedDefinition::CodeDigest(digest) => definition.digest == *digest,
            ExpectedDefinition::ExactBinding(binding) => definition.binding() == **binding,
        }
    }
}

impl PluginCatalog for Catalog {
    fn resolve(&self, ids: &[String]) -> Result<Vec<PluginBinding>, GraphError> {
        self.inner.resolve(ids)
    }
    fn resolve_snapshot(
        &self,
        snapshot: &anchor_runtime::graph::GraphSnapshot,
    ) -> Result<Vec<PluginBinding>, GraphError> {
        self.inner.resolve_snapshot(snapshot)
    }
}

pub(crate) enum ExpectedDefinition<'a> {
    Authorization(&'a PluginBinding),
    AuthorizationDigest(&'a str),
    CodeDigest(&'a str),
    ExactBinding(&'a PluginBinding),
}

pub(crate) enum DefinitionError {
    Read(GraphError),
    Mismatch,
}

pub(crate) struct PluginLibrary {
    inner: Library,
}

impl PluginLibrary {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            inner: Library::new(root),
        }
    }
    pub(crate) fn validate_extension_install(id: &str) -> Result<(), InstallError> {
        Library::validate_extension_install(id)
    }
    pub(crate) fn catalog_read_guard(
        &self,
    ) -> Result<Option<anchor_file_lease::FileLease>, InstallError> {
        self.inner.catalog_read_guard()
    }
    pub(crate) fn review_directory(
        &self,
        id: &str,
        source: impl AsRef<Path>,
    ) -> Result<PluginReview, InstallError> {
        self.inner.review_directory(id, source)
    }
    pub(crate) fn stage_directory(
        &self,
        id: &str,
        source: impl AsRef<Path>,
        transaction: impl AsRef<Path>,
    ) -> Result<(PathBuf, PluginReview), InstallError> {
        self.inner.stage_directory(id, source, transaction)
    }
    pub(crate) fn install_directory(
        &self,
        id: &str,
        source: impl AsRef<Path>,
        replace: bool,
    ) -> Result<InstallOutcome, InstallError> {
        self.inner.install_directory(id, source, replace)
    }
    pub(crate) fn install_system_directory(
        &self,
        system: SystemPlugin,
        source: impl AsRef<Path>,
        expected_digest: &str,
    ) -> Result<InstallOutcome, InstallError> {
        self.inner
            .install_system_directory(system, source, expected_digest)
    }
    pub(crate) fn install_directory_locked(
        &self,
        id: &str,
        source: impl AsRef<Path>,
        replace: bool,
        lease: &InstallLease,
    ) -> Result<InstallOutcome, InstallError> {
        self.inner
            .install_directory_locked(id, source, replace, lease)
    }
    pub(crate) fn development_scope(
        &self,
        scope: &Path,
        create: bool,
    ) -> Result<Option<DevelopmentScope>, DevelopmentError> {
        self.inner.development_scope(scope, create)
    }
    pub(crate) fn release_development_scope(&self, scope: &Path) -> Result<(), DevelopmentError> {
        self.inner.release_development_scope(scope)
    }
    #[cfg(test)]
    pub(crate) fn install_lease(&self) -> Result<InstallLease, InstallError> {
        self.inner.install_lease()
    }
    #[cfg(test)]
    pub(crate) fn try_install_lease(&self) -> Result<InstallLease, InstallError> {
        self.inner.try_install_lease()
    }
}

/// One shared read held by the management projection, including selected detail.
/// A missing plugins directory remains an empty catalog without creating it.
pub(crate) struct ReadCatalog {
    pub(crate) catalog: Catalog,
    guard: Option<anchor_file_lease::FileLease>,
}
impl ReadCatalog {
    pub(crate) fn open(root: &Path) -> Result<Self, InstallError> {
        let guard = PluginLibrary::new(root).catalog_read_guard()?;
        Ok(Self {
            catalog: Catalog::new(root),
            guard,
        })
    }
    pub(crate) fn exists(&self) -> bool {
        self.guard.is_some()
    }
}

/// Preserve the former application boundary's categories and Debug text for
/// preparation callers, without making a Host service depend on application.
#[derive(Debug)]
pub(crate) enum PublicationError {
    Invalid(String),
    Storage(String),
}

impl From<InstallError> for PublicationError {
    fn from(error: InstallError) -> Self {
        match error {
            InstallError::Io(_)
            | InstallError::PublicationUncertain
            | InstallError::CheckoutFailed
            | InstallError::CheckoutTimedOut => Self::Storage(error.to_string()),
            _ => Self::Invalid(error.to_string()),
        }
    }
}

pub(crate) async fn publication_lease(
    library_root: &Path,
    access: &crate::dependency_provision::DependencyAccess,
    operation: &str,
) -> Result<InstallLease, PublicationError> {
    loop {
        access
            .check()
            .map_err(|error| PublicationError::Invalid(error.to_string()))?;
        let lease_root = library_root.to_path_buf();
        let result =
            tokio::task::spawn_blocking(move || Library::new(lease_root).try_install_lease())
                .await
                .map_err(|_| {
                    PublicationError::Storage("Plugin publication lease task failed".into())
                })?;
        match result {
            Ok(lease) => return Ok(lease),
            Err(InstallError::CatalogBusy) => tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {},
                _ = access.cancelled() => return Err(PublicationError::Invalid(format!(
                    "Plugin {operation} was cancelled while waiting for its publication lease"
                ))),
            },
            Err(error) => return Err(error.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DeclarationProjection {
    pub(crate) required: bool,
    pub(crate) mounts_by_default: bool,
}

pub(crate) type MountDeclaration<'a> = (&'a str, bool, Option<bool>);

pub(crate) fn pilot_authority(declaring_graph: Option<&str>, pilot_graph: Option<&str>) -> bool {
    declaring_graph.is_some() && declaring_graph == pilot_graph
}

pub(crate) fn mount_declarations<'a>(
    references: &'a [anchor_runtime::graph::PluginRef],
    declaring_graph: Option<&str>,
    pilot_graph: Option<&str>,
) -> Vec<MountDeclaration<'a>> {
    let authoritative = pilot_authority(declaring_graph, pilot_graph);
    references
        .iter()
        .filter(|reference| reference.id != crate::plugin_network::NETWORK_PLUGIN)
        .map(|reference| {
            let required = reference.required && authoritative;
            (
                reference.id.as_str(),
                required,
                Some(required || reference.default.unwrap_or(true)),
            )
        })
        .collect()
}

pub(crate) fn merge_declarations(
    target: &mut std::collections::BTreeMap<String, DeclarationProjection>,
    declarations: &[MountDeclaration<'_>],
) {
    for (id, required, default) in declarations {
        let entry = target.entry((*id).to_owned()).or_default();
        entry.required |= required;
        entry.mounts_by_default |= default.unwrap_or(true);
    }
}
