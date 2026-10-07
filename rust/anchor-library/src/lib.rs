#![forbid(unsafe_code)]

mod filesystem;
mod oauth;
mod source;

use std::path::{Path, PathBuf};

use anchor_graph_host::FilePluginCatalog;
use serde::{Deserialize, Serialize};

pub use oauth::{
    FileOAuthTokenStore, OAuthAuthorization, OAuthAuthorizationMetadata, OAuthBinding, OAuthClient,
    OAuthError, OAuthRefreshRequest, OAuthRefreshTransport, OAuthSecret, OAuthTokenResponse,
    OAuthTransportError,
};
pub use source::{Checkout, GitCheckout, GithubSource};

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
    #[error("Plugin already exists; explicitly request replacement to update it")]
    AlreadyExists,
    #[error("Plugin catalog is being installed; retry after installation finishes")]
    CatalogBusy,
    #[error("Plugin installation paths must contain only real directories and regular files")]
    UnsafePath,
    #[error("Plugin source has no plugin.json or .codex-plugin/plugin.json manifest")]
    MissingManifest,
    #[error("Plugin manifest or resources failed catalog validation")]
    InvalidPlugin,
    #[error("Git checkout failed")]
    CheckoutFailed,
    #[error("Git checkout timed out")]
    CheckoutTimedOut,
    #[error("Plugin installation filesystem operation failed")]
    Io(#[source] std::io::Error),
    #[error("Plugin publication durability is uncertain; inspect the Library before retrying")]
    PublicationUncertain,
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

    pub fn catalog_read_guard(&self) -> Result<Option<std::fs::File>, InstallError> {
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
        validate_id(id)?;
        self.install_source(id, request.replace_existing, |transaction| {
            let directory = transaction.join("source");
            checkout.checkout(&source, &directory)?;
            Ok(directory.join(source.relative_path()))
        })
    }

    pub fn install_directory(
        &self,
        id: &str,
        source_dir: impl AsRef<Path>,
        replace_existing: bool,
    ) -> Result<InstallOutcome, InstallError> {
        validate_id(id)?;
        let source_dir = source_dir.as_ref().to_owned();
        self.install_source(id, replace_existing, |_| Ok(source_dir))
    }

    fn install_source(
        &self,
        id: &str,
        replace_existing: bool,
        prepare: impl FnOnce(&Path) -> Result<PathBuf, InstallError>,
    ) -> Result<InstallOutcome, InstallError> {
        let (root, _) = filesystem::directory(&self.root, true)?;
        let (plugins, plugins_file) = filesystem::directory(&root.join("plugins"), true)?;
        let _lease = filesystem::lease(&plugins_file, rustix::fs::FlockOperation::LockExclusive)?;
        let destination = plugins.join(id);
        let replacing = filesystem::existing_destination(&destination, replace_existing)?;
        let transaction = tempfile::Builder::new()
            .prefix(".anchor-plugin-")
            .tempdir_in(&plugins)?;
        let source_dir = prepare(transaction.path())?;
        let validation_root = transaction.path().join("validation");
        let (_, validation_plugins) =
            filesystem::directory(&validation_root.join("plugins"), true)?;
        let staged = validation_root.join("plugins").join(id);
        filesystem::stage(&source_dir, &staged)?;
        let definition = FilePluginCatalog::new(&validation_root)
            .definition(id)
            .map_err(|_| InstallError::InvalidPlugin)?;
        filesystem::sync_tree(&staged)?;
        validation_plugins.sync_all()?;
        std::fs::File::open(&validation_root)?.sync_all()?;
        std::fs::File::open(transaction.path())?.sync_all()?;
        plugins_file.sync_all()?;
        let result = filesystem::publish(
            &validation_plugins,
            &plugins_file,
            id,
            replacing,
            filesystem::sync_publication,
        );
        if matches!(result, Err(InstallError::PublicationUncertain)) {
            let _ = transaction.keep();
        }
        result?;
        Ok(InstallOutcome {
            id: id.to_owned(),
            digest: definition.digest,
            directory: destination,
        })
    }
}

fn validate_id(id: &str) -> Result<(), InstallError> {
    if id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    {
        Ok(())
    } else {
        Err(InstallError::InvalidId)
    }
}
