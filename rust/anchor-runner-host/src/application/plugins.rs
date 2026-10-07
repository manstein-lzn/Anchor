use super::{ApplicationError, RunApplication};
use anchor_library::{Checkout, InstallError, InstallOutcome, InstallRequest, Library};
use std::{path::PathBuf, sync::Arc};

impl RunApplication {
    pub(crate) async fn install_plugin(
        &self,
        library_root: PathBuf,
        request: InstallRequest,
        checkout: Option<Arc<dyn Checkout>>,
    ) -> Result<InstallOutcome, ApplicationError> {
        let catalog_guard = self.graph_catalog_mutation_guard().await;
        tokio::task::spawn_blocking(move || {
            let _catalog_guard = catalog_guard;
            let library = Library::new(library_root);
            let installed = match checkout {
                Some(checkout) => library.install_with_checkout(&request, checkout.as_ref()),
                None => library.install(&request),
            };
            installed.map_err(|failure| match failure {
                InstallError::Io(_)
                | InstallError::PublicationUncertain
                | InstallError::CheckoutFailed
                | InstallError::CheckoutTimedOut => ApplicationError::Storage(failure.to_string()),
                _ => ApplicationError::Invalid(failure.to_string()),
            })
        })
        .await
        .map_err(|_| ApplicationError::Storage("Plugin installer task failed".into()))?
    }
}
