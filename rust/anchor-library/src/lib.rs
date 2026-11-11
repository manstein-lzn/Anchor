#![forbid(unsafe_code)]

mod development;
mod filesystem;
mod oauth;
mod reclaim;
mod review;
mod source;

use std::path::{Path, PathBuf};

use anchor_graph_host::{FilePluginCatalog, SystemPlugin};
use serde::{Deserialize, Serialize};

pub use development::{DevelopmentError, DevelopmentScope};
pub use oauth::{
    FileOAuthTokenStore, OAuthAuthorization, OAuthAuthorizationCode, OAuthAuthorizationMetadata,
    OAuthAuthorizationResponse, OAuthAuthorizationTransaction, OAuthBinding, OAuthClient,
    OAuthError, OAuthRefreshRequest, OAuthRefreshTransport, OAuthSecret, OAuthTokenResponse,
    OAuthTransportError,
};
pub use reclaim::{ReclaimError, ReclaimReport, reclaim, reclaim_after_install, summarize};
pub use review::{ChannelReview, PluginReview, ServerReview};
pub use source::{Checkout, GitCheckout, GithubSource};

/// Proof that this process holds the Library's Plugin installation lock.
///
/// Only [`Library::install_lease`] hands one out, and it is what
/// [`Library::install_directory_locked`] requires, so a caller cannot claim the lock
/// it does not hold. Dropping it releases the lock.
#[derive(Debug)]
pub struct InstallLease {
    _lease: anchor_file_lease::FileLease,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    pub source: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub replace_existing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallOutcome {
    pub id: String,
    pub digest: String,
    pub directory: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("source must be an HTTPS github.com tree URL identifying a Plugin directory")]
    InvalidSource,
    #[error("invalid Plugin ID")]
    InvalidId,
    #[error(
        "System Plugin `{0}` is owned by Anchor and cannot be installed or replaced through extension installation"
    )]
    SystemPluginProtected(String),
    #[error("System Plugin does not match the admitted Anchor release identity")]
    SystemPluginMismatch,
    #[error(
        "System Plugin declares existing data paths; an explicit data migration is required before upgrade"
    )]
    SystemPluginMigrationRequired,
    #[error("Plugin already exists; explicitly request replacement to update it")]
    AlreadyExists,
    #[error("Plugin catalog is being installed; retry after installation finishes")]
    CatalogBusy,
    #[error("Plugin installation paths must contain only real directories and regular files")]
    UnsafePath,
    #[error("a declared mutable path is not usable: {0}")]
    UnsafeMutablePath(String),
    #[error("Plugin source has no plugin.json or .codex-plugin/plugin.json manifest")]
    MissingManifest,
    #[error("Plugin manifest or resources failed catalog validation")]
    InvalidPlugin,
    #[error("Plugin manifest or resources failed catalog validation: {0}")]
    InvalidPluginReason(String),
    #[error("Git checkout failed")]
    CheckoutFailed,
    #[error("Git checkout timed out")]
    CheckoutTimedOut,
    #[error("Plugin installation filesystem operation failed")]
    Io(#[source] std::io::Error),
    #[error("Plugin publication durability is uncertain; inspect the Library before retrying")]
    PublicationUncertain,
    #[error(
        "replacing Plugin code would touch the mutable path `{entry}`; nothing was changed{changed}"
    )]
    MutableConflict { entry: String, changed: String },
    #[error("replacing Plugin code stopped after [{changed}]: {reason}")]
    PartialReplacement { changed: String, reason: String },
}

impl From<std::io::Error> for InstallError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone)]
pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn install(&self, request: &InstallRequest) -> Result<InstallOutcome, InstallError> {
        self.install_with_checkout(request, &GitCheckout)
    }

    /// Shared preflight for extension publishers, including Host approval forms.
    pub fn validate_extension_install(id: &str) -> Result<(), InstallError> {
        validate_id(id)?;
        if SystemPlugin::from_id(id).is_some() {
            return Err(InstallError::SystemPluginProtected(id.to_owned()));
        }
        Ok(())
    }

    pub fn catalog_read_guard(&self) -> Result<Option<anchor_file_lease::FileLease>, InstallError> {
        match filesystem::directory(&self.root.join("plugins"), false) {
            Ok((_, directory)) => filesystem::lease(
                &directory,
                rustix::fs::FlockOperation::NonBlockingLockShared,
            )
            .map(Some),
            Err(InstallError::Io(failure)) if failure.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            }
            Err(failure) => Err(failure),
        }
    }

    pub fn install_with_checkout(
        &self,
        request: &InstallRequest,
        checkout: &dyn Checkout,
    ) -> Result<InstallOutcome, InstallError> {
        let source = GithubSource::parse(&request.source)?;
        let id = request.id.as_deref().unwrap_or_else(|| source.default_id());
        self.install_source(
            id,
            request.replace_existing,
            None,
            Publication::Extension,
            |transaction: &Path| {
                let directory = transaction.join("source");
                checkout.checkout(&source, &directory)?;
                Ok(directory.join(source.relative_path()))
            },
        )
    }

    pub fn install_directory(
        &self,
        id: &str,
        source_dir: impl AsRef<Path>,
        replace_existing: bool,
    ) -> Result<InstallOutcome, InstallError> {
        let source_dir = source_dir.as_ref().to_owned();
        self.install_source(id, replace_existing, None, Publication::Extension, |_| {
            Ok(source_dir)
        })
    }

    /// Bootstrap a missing platform resource from an already admitted release.
    /// Existing content is never replaced, even when its reserved ID collides.
    pub fn install_system_directory(
        &self,
        system: SystemPlugin,
        source_dir: impl AsRef<Path>,
        expected_digest: &str,
    ) -> Result<InstallOutcome, InstallError> {
        let source_dir = source_dir.as_ref().to_owned();
        self.install_source(
            system.id(),
            false,
            None,
            Publication::System {
                system,
                expected_digest,
                expected_existing: None,
            },
            |_| Ok(source_dir),
        )
    }

    /// Upgrade only the exact existing bytes the operator inspected. Unlike
    /// bootstrap this is explicit; a Host restart must never invoke it.
    pub fn upgrade_system_directory(
        &self,
        system: SystemPlugin,
        source_dir: impl AsRef<Path>,
        expected_digest: &str,
        expected_existing: &str,
    ) -> Result<InstallOutcome, InstallError> {
        let source_dir = source_dir.as_ref().to_owned();
        self.install_source(
            system.id(),
            true,
            None,
            Publication::System {
                system,
                expected_digest,
                expected_existing: Some(expected_existing),
            },
            |_| Ok(source_dir),
        )
    }

    /// Take the Library's Plugin installation lock for a longer operation.
    ///
    /// A host that lands dependency views *and then* publishes the Plugin must hold
    /// this across both: a reclamation running in between would see a view whose
    /// Plugin is not installed yet and judge it an orphan. The lock is the same one
    /// installation and reclamation use, so the three can never overlap. This
    /// operator-facing method waits; a cancellable host uses
    /// [`Library::try_install_lease`] instead.
    pub fn install_lease(&self) -> Result<InstallLease, InstallError> {
        self.install_lease_with(rustix::fs::FlockOperation::LockExclusive)
    }

    /// The same publication lease without blocking, for hosts whose pending
    /// installation must remain cancellable while another installer runs.
    pub fn try_install_lease(&self) -> Result<InstallLease, InstallError> {
        self.install_lease_with(rustix::fs::FlockOperation::NonBlockingLockExclusive)
    }

    fn install_lease_with(
        &self,
        operation: rustix::fs::FlockOperation,
    ) -> Result<InstallLease, InstallError> {
        let (root, _) = filesystem::directory(&self.root, true)?;
        let (plugins, plugins_file) = filesystem::directory(&root.join("plugins"), true)?;
        let _ = plugins;
        let lease = filesystem::lease(&plugins_file, operation)?;
        Ok(InstallLease { _lease: lease })
    }

    /// Install while the caller holds [`Library::install_lease`].
    pub fn install_directory_locked(
        &self,
        id: &str,
        source_dir: impl AsRef<Path>,
        replace_existing: bool,
        lease: &InstallLease,
    ) -> Result<InstallOutcome, InstallError> {
        let source_dir = source_dir.as_ref().to_owned();
        self.install_source(
            id,
            replace_existing,
            Some(lease),
            Publication::Extension,
            |_| Ok(source_dir),
        )
    }

    /// Describe a candidate Plugin without publishing it.
    ///
    /// The candidate is staged and validated through the same catalog an
    /// installation uses, so `PluginReview::digest` is the digest the installed
    /// Plugin will carry. A review never writes to the Library: it stages in the
    /// system temporary directory and drops the tree, which is why it also works
    /// when the Library is read-only for the reviewer.
    pub fn review_directory(
        &self,
        id: &str,
        source_dir: impl AsRef<Path>,
    ) -> Result<PluginReview, InstallError> {
        validate_id(id)?;
        let source_dir = source_dir.as_ref().to_owned();
        self.review_candidate(id, &source_dir)
    }

    /// Stage a confined, non-published candidate using the installer's exact
    /// file rules. The caller owns the temporary transaction and its cleanup.
    pub fn stage_directory(
        &self,
        id: &str,
        source_dir: impl AsRef<Path>,
        transaction: impl AsRef<Path>,
    ) -> Result<(PathBuf, PluginReview), InstallError> {
        validate_id(id)?;
        let (root, _plugins, _staged) =
            stage_candidate(id, source_dir.as_ref(), transaction.as_ref())?;
        let catalog = FilePluginCatalog::new(&root);
        let definition = catalog
            .definition(id)
            .map_err(|error| InstallError::InvalidPluginReason(error.to_string()))?;
        let review = review::summarize(id, &catalog, &definition)?;
        Ok((root, review))
    }

    /// Review a GitHub tree source without publishing it.
    pub fn review(&self, request: &InstallRequest) -> Result<PluginReview, InstallError> {
        self.review_with_checkout(request, &GitCheckout)
    }

    pub fn review_with_checkout(
        &self,
        request: &InstallRequest,
        checkout: &dyn Checkout,
    ) -> Result<PluginReview, InstallError> {
        let source = GithubSource::parse(&request.source)?;
        let id = request.id.as_deref().unwrap_or_else(|| source.default_id());
        validate_id(id)?;
        let transaction = tempfile::tempdir()?;
        let directory = transaction.path().join("source");
        checkout.checkout(&source, &directory)?;
        self.review_candidate(id, &directory.join(source.relative_path()))
    }

    fn review_candidate(&self, id: &str, source_dir: &Path) -> Result<PluginReview, InstallError> {
        // A review stages outside the Library, so it never needs write access
        // to the installed Plugins.
        let transaction = tempfile::Builder::new()
            .prefix(".anchor-plugin-review-")
            .tempdir()?;
        let (validation_root, _plugins, _staged) =
            stage_candidate(id, source_dir, transaction.path())?;
        let catalog = FilePluginCatalog::new(&validation_root);
        let definition = catalog
            .definition(id)
            .map_err(|error| InstallError::InvalidPluginReason(error.to_string()))?;
        review::summarize(id, &catalog, &definition)
    }

    fn install_source(
        &self,
        id: &str,
        replace_existing: bool,
        held: Option<&InstallLease>,
        publication: Publication<'_>,
        prepare: impl FnOnce(&Path) -> Result<PathBuf, InstallError>,
    ) -> Result<InstallOutcome, InstallError> {
        if matches!(publication, Publication::Extension) {
            Self::validate_extension_install(id)?;
        }
        let (root, _) = filesystem::directory(&self.root, true)?;
        let (plugins, plugins_file) = filesystem::directory(&root.join("plugins"), true)?;
        // The caller may already hold the lock (a host that must cover its whole
        // land-then-publish window). Taking it twice would deadlock against itself.
        let _acquired;
        let _lease = match held {
            Some(lease) => &lease._lease,
            None => {
                _acquired =
                    filesystem::lease(&plugins_file, rustix::fs::FlockOperation::LockExclusive)?;
                &_acquired
            }
        };
        let destination = plugins.join(id);
        let replacing = filesystem::existing_destination(&destination, replace_existing)?;
        if matches!(
            publication,
            Publication::System {
                expected_existing: Some(_),
                ..
            }
        ) && !replacing
        {
            return Err(InstallError::SystemPluginMismatch);
        }
        let transaction = tempfile::Builder::new()
            .prefix(".anchor-plugin-")
            .tempdir_in(&plugins)?;
        let source_dir = prepare(transaction.path())?;
        let (validation_root, validation_plugins, staged) =
            stage_candidate(id, &source_dir, transaction.path())?;
        let definition = FilePluginCatalog::new(&validation_root)
            .definition(id)
            .map_err(|_| InstallError::InvalidPlugin)?;
        if let Publication::System {
            system,
            expected_digest,
            ..
        } = publication
            && (definition.system != Some(system) || definition.digest != expected_digest)
        {
            return Err(InstallError::SystemPluginMismatch);
        }
        filesystem::sync_tree(&staged)?;
        validation_plugins.sync_all()?;
        std::fs::File::open(&validation_root)?.sync_all()?;
        std::fs::File::open(transaction.path())?.sync_all()?;
        plugins_file.sync_all()?;
        if !replacing {
            // An import: the source is copied once, into the one place the Plugin
            // exists, in a single atomic rename.
            filesystem::publish(
                &validation_plugins,
                &plugins_file,
                id,
                false,
                filesystem::sync_publication,
            )?;
            // The locations the declaration names are provided, empty, so the
            // Plugin's process has somewhere to write from its first run.
            provide_mutable_paths(&destination, &definition.mutable)?;
        } else {
            // An upgrade: only code is replaced. Everything the Plugin declared as
            // its own (the *old* declaration and the new one) is left alone, and
            // the mutable locations the new declaration needs are provided if they
            // do not exist yet.
            let installed_mutable = match publication {
                Publication::Extension => {
                    FilePluginCatalog::new(&root)
                        .definition(id)
                        .map_err(|_| InstallError::InvalidPlugin)?
                        .mutable
                }
                Publication::System {
                    system,
                    expected_existing: Some(expected),
                    ..
                } => {
                    let installed = FilePluginCatalog::new(&root)
                        .inspect_system_plugin(system)
                        .map_err(|_| InstallError::InvalidPlugin)?;
                    if installed.digest != expected {
                        return Err(InstallError::SystemPluginMismatch);
                    }
                    if !installed.mutable.paths().is_empty() {
                        return Err(InstallError::SystemPluginMigrationRequired);
                    }
                    installed.mutable
                }
                Publication::System { .. } => return Err(InstallError::SystemPluginMismatch),
            };
            filesystem::replace_code(
                &staged,
                &destination,
                &installed_mutable,
                &definition.mutable,
            )?;
            // Provided *after* the plan: a location the new declaration adds must
            // not look like an installed code entry the plan would remove.
            provide_mutable_paths(&destination, &definition.mutable)?;
            sync_tree_entries(&destination, &definition.mutable)?;
            plugins_file.sync_all()?;
        }
        // Reclaiming here is opportunistic and must never turn a successful
        // installation into a failure: the previous view of this Plugin is garbage
        // the moment this declaration is published, and its exclusive packages go
        // with it. The Plugin lock is already held, so no other writer is inside.
        // A Library that never landed a dependency has no store to sweep.
        if self.root.join("dependencies").is_dir() {
            match reclaim_after_install(&self.root, true) {
                // Silence only when there was nothing to say: a removal is an
                // operator-visible fact, and a refused pass says why.
                Ok(report)
                    if report.removed_views.is_empty()
                        && report.removed_package_versions.is_empty()
                        && report.deferred.is_none()
                        && report.store_phase_abandoned.is_none() => {}
                Ok(report) => eprintln!(
                    "anchor-library: reclamation after installing `{id}`: {}",
                    serde_json::to_string(&crate::reclaim::summarize(&report))
                        .unwrap_or_else(|_| "{}".to_owned())
                ),
                Err(error) => {
                    eprintln!(
                        "anchor-library: reclamation after installing `{id}` did not run: {error}"
                    )
                }
            }
        }
        Ok(InstallOutcome {
            id: id.to_owned(),
            digest: definition.digest,
            directory: destination,
        })
    }
}

#[derive(Clone, Copy)]
enum Publication<'a> {
    Extension,
    System {
        system: SystemPlugin,
        expected_digest: &'a str,
        expected_existing: Option<&'a str>,
    },
}

/// Provide the mutable locations a declaration names, without touching content.
///
/// The platform never reads, copies or rewrites a mutable path; it does have to
/// make sure the location exists, because that is what the Plugin's process is
/// granted write access to. An existing path is left exactly as it is.
fn provide_mutable_paths(
    plugin_directory: &Path,
    mutable: &anchor_graph_host::MutablePaths,
) -> Result<(), InstallError> {
    let resolved = mutable
        .resolve(plugin_directory)
        .map_err(InstallError::UnsafeMutablePath)?;
    for (declared, path) in mutable.paths().iter().zip(resolved) {
        if path.exists() {
            continue;
        }
        if declared.ends_with('/') {
            std::fs::create_dir_all(&path)?;
        } else if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            std::fs::File::create(&path)?;
        }
    }
    Ok(())
}

/// Sync the files a code replacement just wrote, without descending into data.
fn sync_tree_entries(
    destination: &Path,
    mutable: &anchor_graph_host::MutablePaths,
) -> Result<(), InstallError> {
    for entry in filesystem::code_entries(destination, mutable)? {
        if entry
            .relative
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".removed-"))
        {
            continue;
        }
        let path = destination.join(entry.relative);
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            filesystem::sync_tree(&path)?;
        } else {
            std::fs::File::open(&path)?.sync_all()?;
        }
        // A code entry can sit beside mutable children in a shared parent.
        // Sync those ancestor directories without opening any data child.
        let mut parent = path.parent();
        while let Some(directory) = parent
            && directory != destination
        {
            std::fs::File::open(directory)?.sync_all()?;
            parent = directory.parent();
        }
    }
    std::fs::File::open(destination)?.sync_all()?;
    Ok(())
}

/// Stage a candidate Plugin under `transaction` and return the validation root,
/// the staged `plugins` directory, and the staged tree.
///
/// The caller runs catalog validation so it can map a failure to its own error
/// contract. Installation and review stage through this single function, which
/// is what keeps a review's digest identical to the published one.
fn stage_candidate(
    id: &str,
    source_dir: &Path,
    transaction: &Path,
) -> Result<(PathBuf, std::fs::File, PathBuf), InstallError> {
    let validation_root = transaction.join("validation");
    let (_, validation_plugins) = filesystem::directory(&validation_root.join("plugins"), true)?;
    let staged = validation_root.join("plugins").join(id);
    filesystem::stage(source_dir, &staged)?;
    Ok((validation_root, validation_plugins, staged))
}

fn validate_id(id: &str) -> Result<(), InstallError> {
    if anchor_runtime::identifier::is_plugin_id(id) {
        Ok(())
    } else {
        Err(InstallError::InvalidId)
    }
}
