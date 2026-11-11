use super::graphs::{ImpactCoverage, ImpactScan, coverage_of};
use super::{ApplicationError, RunApplication};
use crate::dependency_provision::{
    DependencyAccess, LockedPackage, PackageSource, ProvisionError, ResolvedRequirements,
    resolve_requirements_with_access, verify_index_json,
};
use crate::plugin_library::Catalog;
use crate::plugin_library::PluginLibrary;
use anchor_graph_host::{SystemPlugin, plugin_view_path, resolved_library_root, views_root};
use anchor_library::{InstallError, InstallOutcome, PluginReview};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;

pub(crate) mod catalog;

#[cfg(test)]
mod reference_tests;

/// What a human is asked to approve for one Plugin candidate.
///
/// The digest is the one review produced, so the approval is bound to exactly
/// the tree that would be published. `existing_digest` records what the Library
/// holds for the same id at review time, which is what the unchanged check
/// compares against after the answer arrives.
#[derive(Debug, Clone)]
pub(crate) struct PluginInstallPrecondition {
    pub(crate) review: PluginReview,
    pub(crate) authorization_digest: String,
    pub(crate) network: bool,
    pub(crate) mutable: Vec<String>,
    pub(crate) directory: PathBuf,
    pub(crate) existing_digest: Option<String>,
    /// Saved Graphs affected by replacing this Plugin. Runs resolve the current
    /// Plugin at their next node call and retain their admission authorization.
    pub(crate) impact: PluginImpact,
    /// The resolved dependency set this approval covers, if the Plugin declares
    /// one. The resolution is frozen here: what the human approves is what gets
    /// installed, and a declaration that cannot be solved never reaches a form.
    pub(crate) dependencies: Option<PluginDependencyPlan>,
    /// The same resolution in the shape the install step consumes. Carried, not
    /// displayed: it is what the approved set will be reproduced from.
    pub(crate) resolved: Option<ResolvedRequirements>,
    pub(crate) access: DependencyAccess,
}

/// The deployment-authorized, resolved answer to one Plugin's declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginDependencyPlan {
    /// The deployment sources the resolution was allowed to use.
    pub(crate) sources: Vec<String>,
    pub(crate) interpreter: String,
    pub(crate) interpreter_path: PathBuf,
    pub(crate) interpreter_digest: String,
    /// The identity of the locked set, which is what the install reproduces.
    pub(crate) lock_digest: String,
    pub(crate) packages: Vec<LockedPackage>,
    /// Where the view will live. Absolute, and derived exactly as the launcher
    /// derives it, so the form can show the interpreter the server will run on.
    pub(crate) view: PathBuf,
    /// The declaration digest that names the view.
    pub(crate) declaration_digest: String,
}

/// Who currently depends on one Plugin, as a derived view.
///
/// Never stored: it is a projection over the Graph catalog and the Run records,
/// so it cannot drift from them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PluginImpact {
    /// Saved Graphs that bind this Plugin.
    pub(crate) graphs: Vec<String>,
    /// How much of the catalog this report actually saw. A Graph this host
    /// cannot load is a gap, never a silent omission: the operator is being
    /// asked to trust a count.
    pub(crate) coverage: ImpactCoverage,
}

impl PluginInstallPrecondition {
    pub(crate) fn replacing(&self) -> bool {
        self.existing_digest.is_some()
    }

    /// True when the Library already holds this exact tree.
    pub(crate) fn already_installed(&self) -> bool {
        self.existing_digest.as_deref() == Some(self.review.digest.as_str())
    }
}

impl RunApplication {
    /// Review a candidate and freeze what the operator is about to approve.
    ///
    /// Read-only: it publishes nothing and needs no Library write access.
    #[cfg(test)]
    pub(crate) async fn plugin_install_precondition(
        &self,
        library_root: PathBuf,
        directory: PathBuf,
        id: String,
    ) -> Result<PluginInstallPrecondition, ApplicationError> {
        self.plugin_install_precondition_with_access(
            library_root,
            directory,
            id,
            DependencyAccess::default(),
        )
        .await
    }

    pub(crate) async fn plugin_install_precondition_with_access(
        &self,
        library_root: PathBuf,
        directory: PathBuf,
        id: String,
        access: DependencyAccess,
    ) -> Result<PluginInstallPrecondition, ApplicationError> {
        access.check().map_err(plugin_dependency_error)?;
        PluginLibrary::validate_extension_install(&id).map_err(plugin_error)?;
        let reviewed = directory.clone();
        let review_root = library_root.clone();
        let (review, existing_digest, authorization_digest, network, mutable) =
            tokio::task::spawn_blocking(move || {
                let transaction = tempfile::tempdir()
                    .map_err(|error| ApplicationError::Storage(error.to_string()))?;
                let (staged, review) = PluginLibrary::new(&review_root)
                    .stage_directory(&id, &reviewed, transaction.path())
                    .map_err(plugin_error)?;
                let definition = Catalog::new(staged)
                    .definition(&id)
                    .map_err(ApplicationError::from)?;
                let authorization_digest = definition.authorization_digest.to_string();
                let network = crate::plugin_network::requires_network(&definition);
                let mutable = definition.mutable.paths().to_vec();
                let existing_digest = Catalog::new(&review_root)
                    .definition(&id)
                    .ok()
                    .map(|definition| definition.digest);
                Ok::<_, ApplicationError>((
                    review,
                    existing_digest,
                    authorization_digest,
                    network,
                    mutable,
                ))
            })
            .await
            .map_err(|_| ApplicationError::Storage("Plugin review task failed".into()))??;
        // What the replacement would reach is a host projection over the Graph
        // catalog and the Run records, so it is read here rather than guessed.
        let impact = self.plugin_impact(&review.id)?;
        // A declaration is solved before anybody is asked to approve anything:
        // approving a Plugin whose packages cannot be resolved would be
        // approving an installation that cannot happen.
        let (dependencies, resolved) = self
            .plugin_dependency_plan(&library_root, &review, &access)
            .await?;
        Ok(PluginInstallPrecondition {
            review,
            authorization_digest,
            network,
            mutable,
            directory,
            existing_digest,
            impact,
            dependencies,
            resolved,
            access,
        })
    }

    /// Solve one Plugin's declaration against the deployment's authorized sources.
    ///
    /// Fail closed: a Plugin that declares dependencies on a deployment that
    /// authorizes no package installation is refused here, before anything is
    /// written, instead of being installed into a state where its tools can
    /// never start.
    pub(crate) async fn plugin_dependency_plan(
        &self,
        library_root: &Path,
        review: &PluginReview,
        access: &DependencyAccess,
    ) -> Result<(Option<PluginDependencyPlan>, Option<ResolvedRequirements>), ApplicationError>
    {
        if review.dependencies.is_empty() {
            return Ok((None, None));
        }
        // Fail closed with a reason the operator can act on: a Plugin whose
        // dependencies cannot be installed must not be installed at all, because
        // its tools could never start.
        let provisioner = self.dependency_provisioner().map_err(|_| {
            ApplicationError::Invalid(format!(
                "Plugin `{}` declares dependencies, but this deployment does not authorize installing them (no ANCHOR_DEPENDENCIES_* configuration)",
                review.id
            ))
        })?;
        let declaration_digest = review.dependencies.declaration_digest();
        let sources = provisioner
            .config
            .sources
            .iter()
            .map(|source| source.as_str())
            .collect::<Vec<_>>();
        let resolved = resolve_requirements_with_access(
            &provisioner.config,
            None,
            &review.dependencies.python,
            access,
        )
        .await
        .map_err(|error| {
            ApplicationError::Invalid(format!(
                "Plugin `{}` dependency resolution failed: {error}",
                review.id
            ))
        })?;
        // An index that answers the package page with HTML resolves fine and
        // fetches never, so that is found now — with the failure the operator can
        // act on, before they are asked to approve the set.
        //
        // The check mirrors the fetch rather than being stricter than it: a
        // package is deliverable as soon as one authorized index answers a
        // PEP 691 page, and a deployment that also authorizes a directory may
        // still deliver a package no index has (the fetch reports that per
        // source).
        let has_directory = provisioner
            .config
            .sources
            .iter()
            .any(|source| matches!(source, PackageSource::Directory(_)));
        for package in resolved.packages.iter().filter(|_| access.network) {
            let mut reasons = Vec::new();
            let mut served = false;
            for source in &provisioner.config.sources {
                if let PackageSource::Index(index) = source {
                    access.check().map_err(plugin_dependency_error)?;
                    let verification = tokio::select! {
                        result = tokio::time::timeout(std::time::Duration::from_secs(20), verify_index_json(index, &package.name)) => result
                            .map_err(|_| plugin_dependency_error(ProvisionError::Failed(format!("package index `{index}` JSON verification timed out"))))?,
                        _ = access.cancelled() => return Err(ApplicationError::Invalid("Plugin dependency planning was cancelled while checking its package index".into())),
                    };
                    match verification {
                        Ok(()) => {
                            served = true;
                            break;
                        }
                        Err(error) => reasons.push(error.to_string()),
                    }
                }
            }
            if !served && !has_directory {
                return Err(ApplicationError::Invalid(format!(
                    "Plugin `{}` dependency resolution failed: no authorized index serves `{}` as a PEP 691 JSON package page ({})",
                    review.id,
                    package.name,
                    reasons.join("; ")
                )));
            }
        }
        let root = resolved_library_root(library_root);
        let view = plugin_view_path(&root, &review.id, &declaration_digest);
        let plan = PluginDependencyPlan {
            sources,
            interpreter: resolved.interpreter.clone(),
            interpreter_path: resolved.interpreter_path.clone(),
            interpreter_digest: resolved.interpreter_digest.clone(),
            lock_digest: resolved.lock_digest.clone(),
            packages: resolved.packages.clone(),
            view,
            declaration_digest,
        };
        Ok((Some(plan), Some(resolved)))
    }

    /// Which Graphs and unfinished Runs depend on one Plugin right now.
    fn plugin_impact(&self, id: &str) -> Result<PluginImpact, ApplicationError> {
        let (bundles, coverage) = match self.impact_scan() {
            ImpactScan::Read {
                bundles,
                considered,
                total,
                missing,
            } => (bundles, coverage_of(considered, total, missing)),
            ImpactScan::Unreadable(reason) => (Vec::new(), ImpactCoverage::Unknown { reason }),
        };
        let mut graphs = Vec::new();
        for (name, bundle) in &bundles {
            if bundle.plugins.iter().any(|plugin| plugin == id) {
                graphs.push(name.clone());
            }
        }
        graphs.sort();
        // Upgrading is always allowed (standard invariant 7): a Run uses the
        // *current* Plugin and observes it from its next node call, so there is no
        // stranded-Run question to ask and nothing to confirm.
        Ok(PluginImpact { graphs, coverage })
    }

    /// Publish the reviewed candidate only if nothing moved since the review.
    ///
    /// A confirmation the operator gave for one digest must never install a
    /// different tree, so the candidate is reviewed again and the Library state
    /// is re-read before anything is written.
    /// An upgrade needs no second decision: a Run uses the current Plugin and
    /// takes the new code from its next node call (standard invariants 6 and 7).
    pub(crate) async fn install_plugin_if_unchanged(
        &self,
        library_root: PathBuf,
        precondition: PluginInstallPrecondition,
    ) -> Result<InstallOutcome, ApplicationError> {
        precondition
            .access
            .check()
            .map_err(plugin_dependency_error)?;
        // Repairing the view of an already-published tree publishes nothing, so
        // it is not a replacement.
        let already_installed = precondition.already_installed();
        // One writer for the whole operation: the catalog guard covers the
        // dependency landing as well as the Plugin publication, so two installs
        // cannot interleave their renames.
        let catalog_guard = tokio::select! {
            guard = self.graph_catalog_mutation_guard() => guard,
            _ = precondition.access.cancelled() => return Err(ApplicationError::Invalid("Plugin installation was cancelled while waiting for its catalog guard".into())),
        };
        precondition
            .access
            .check()
            .map_err(plugin_dependency_error)?;
        let id = precondition.review.id.clone();
        let directory = precondition.directory.clone();
        let replacing = precondition.replacing() && !already_installed;
        {
            let library_root = library_root.clone();
            let id = id.clone();
            let directory = directory.clone();
            let expected_digest = precondition.review.digest.clone();
            let expected_existing = precondition.existing_digest.clone();
            tokio::task::spawn_blocking(move || {
                let library = PluginLibrary::new(&library_root);
                let reviewed = library
                    .review_directory(&id, &directory)
                    .map_err(plugin_error)?;
                if reviewed.digest != expected_digest {
                    return Err(ApplicationError::Conflict(
                        "the Plugin candidate changed while the user was deciding; ask again"
                            .into(),
                    ));
                }
                let existing_digest = Catalog::new(&library_root)
                    .definition(&id)
                    .ok()
                    .map(|definition| definition.digest);
                if existing_digest != expected_existing {
                    return Err(ApplicationError::Conflict(
                        "the installed Plugin changed while the user was deciding; ask again"
                            .into(),
                    ));
                }
                Ok::<(), ApplicationError>(())
            })
            .await
            .map_err(|_| ApplicationError::Storage("Plugin installer task failed".into()))??;
        }
        // The installation lock is taken *before* the dependencies land and held
        // until the Plugin is published. The two steps are otherwise independent,
        // and in the window between them a concurrent reclamation would see a view
        // whose Plugin is not installed yet and judge it an orphan (the install
        // would then succeed with a missing view). Reclamation takes the same lock
        // without waiting, so this cannot deadlock.
        let lease = crate::plugin_library::publication_lease(
            &library_root,
            &precondition.access,
            "installation",
        )
        .await?;
        precondition
            .access
            .check()
            .map_err(plugin_dependency_error)?;
        // Copy into a private Host transaction, then validate those copied
        // bytes against the approval while holding the publication lease. The
        // workspace remains editable during dependency preparation, but none
        // of its later bytes can become this installation's input.
        let (approved_snapshot, directory) = tokio::task::spawn_blocking({
            let (library_root, id, source) = (library_root.clone(), id.clone(), directory.clone());
            let (expected_digest, expected_existing) = (precondition.review.digest.clone(), precondition.existing_digest.clone());
            move || {
                let transaction = tempfile::Builder::new().prefix(".anchor-plugin-approved-").tempdir()
                    .map_err(|error| ApplicationError::Storage(error.to_string()))?;
                let (root, review) = PluginLibrary::new(&library_root).stage_directory(&id, &source, transaction.path()).map_err(plugin_error)?;
                if review.digest != expected_digest {
                    return Err(ApplicationError::Conflict("the Plugin candidate changed before publication; review and confirm its current content".into()));
                }
                let existing = Catalog::new(&library_root).definition(&id).ok().map(|definition| definition.digest);
                if existing != expected_existing {
                    return Err(ApplicationError::Conflict("the installed Plugin changed before publication; review and confirm again".into()));
                }
                Ok::<_, ApplicationError>((transaction, root.join("plugins").join(&id)))
            }
        }).await.map_err(|_| ApplicationError::Storage("Plugin staging task failed".into()))??;
        // Dependencies land before the Plugin is published: a failed solve,
        // fetch, unpack or verification must leave the Library and every
        // existing view exactly as they were.
        let mut landed = None;
        if let Some(resolved) = precondition.resolved.clone() {
            let provisioner = self.dependency_provisioner()?;
            let declaration_digest = precondition
                .dependencies
                .as_ref()
                .map(|plan| plan.declaration_digest.clone())
                .ok_or_else(|| {
                    ApplicationError::Storage(
                        "the approved Plugin dependency plan is incomplete".into(),
                    )
                })?;
            let landing_root = library_root.clone();
            landed = Some(
                crate::plugin_dependencies::land_with_access(
                    &provisioner,
                    &landing_root,
                    &id,
                    &declaration_digest,
                    &resolved,
                    &precondition.access,
                )
                .await
                .map_err(plugin_dependency_error)?,
            );
        }
        if already_installed {
            // The Library already holds this exact tree. The only thing this
            // operation can still add is the view the declaration maps to, which
            // is how a deleted or never-built view is repaired through the one
            // installation gate — the Plugin bytes are not rewritten.
            let root = library_root.clone();
            let id = id.clone();
            return tokio::task::spawn_blocking(move || {
                let _catalog_guard = catalog_guard;
                let catalog = Catalog::new(&root);
                let definition = catalog
                    .definition(&id)
                    .map_err(|error| ApplicationError::Storage(error.to_string()))?;
                let directory = catalog
                    .plugin_directory(&id)
                    .map_err(|error| ApplicationError::Storage(error.to_string()))?;
                Ok(InstallOutcome {
                    id,
                    digest: definition.digest,
                    directory,
                })
            })
            .await
            .map_err(|_| ApplicationError::Storage("Plugin installer task failed".into()))?;
        }
        // A view this call created would become an orphan if the Plugin never
        // reached the Library, so it is removed again; a view that was already
        // there is left alone, because a Run may be using it and a retry reuses it.
        let created_view = landed
            .filter(|landed| !landed.reused)
            .map(|landed| landed.view);
        tokio::task::spawn_blocking(move || {
            let _catalog_guard = catalog_guard;
            let _approved_snapshot = approved_snapshot;
            let published = precondition
                .access
                .check()
                .map_err(plugin_dependency_error)
                .and_then(|()| {
                    PluginLibrary::new(&library_root)
                        .install_directory_locked(&id, &directory, replacing, &lease)
                        .map_err(plugin_error)
                });
            if let Err(error) = &published
                && let Some(view) = created_view
            {
                // Rollback is part of publication: another installer must not
                // reuse this view between releasing the lease and removing it.
                // A partial publication may already reference the view, in which
                // case it belongs to the installed Plugin and must be retained.
                if let Err(cleanup) = remove_unreferenced_view(&library_root, &id, &view, &lease) {
                    return Err(ApplicationError::Storage(format!(
                        "Plugin installation failed ({error:?}); dependency view cleanup failed: {cleanup}"
                    )));
                }
            }
            published
        })
        .await
        .map_err(|_| ApplicationError::Storage("Plugin installer task failed".into()))?
    }
}

impl RunApplication {
    /// Protect reading a Plugin until its durable Run or Session reference is saved.
    /// This is the same Library lease installation and uninstall use, not a
    /// separate reference lock. Run admission takes graph -> Library -> active;
    /// mount/uninstall take catalog -> Library. Admissions must never acquire
    /// catalog while holding a graph lease: Graph deletion waits in the reverse
    /// order. Drop this lease before spawning execution or doing network work.
    pub(crate) async fn plugin_reference_lease(
        &self,
    ) -> Result<anchor_library::InstallLease, ApplicationError> {
        crate::plugin_library::publication_lease(
            self.library_root(),
            &DependencyAccess::default(),
            "reference publication",
        )
        .await
        .map_err(Into::into)
    }
}

/// Called only while the Library publication lease is held. An uncertain
/// publication keeps its view if the current Plugin references it; unreadable
/// installed content also prevents destructive cleanup.
fn remove_unreferenced_view(
    library_root: &Path,
    id: &str,
    view: &Path,
    _lease: &anchor_library::InstallLease,
) -> Result<(), String> {
    match std::fs::symlink_metadata(library_root.join("plugins").join(id)) {
        Ok(_) => {
            let definition = Catalog::new(library_root).definition(id).map_err(|error| {
                format!("current Plugin is unavailable; view retained: {error}")
            })?;
            let current = plugin_view_path(
                &resolved_library_root(library_root),
                id,
                &definition.dependencies.declaration_digest(),
            );
            if !definition.dependencies.is_empty() && current == view {
                return Ok(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("current Plugin could not be inspected: {error}")),
    }
    match std::fs::remove_dir_all(view) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// A dependency failure is the operator's to fix and names what failed, not a
/// transport or storage accident inside the installer.
fn plugin_dependency_error(error: ProvisionError) -> ApplicationError {
    match error {
        ProvisionError::Invalid(reason) | ProvisionError::Failed(reason) => {
            ApplicationError::Invalid(reason)
        }
        ProvisionError::Unconfigured(reason) => ApplicationError::Invalid(reason),
    }
}

fn plugin_error(failure: InstallError) -> ApplicationError {
    crate::plugin_library::PublicationError::from(failure).into()
}

/// What a human is asked to approve before one Plugin is uninstalled.
///
/// The Plugin has exactly one copy in the world (standard invariant 13): its code
/// and its data live in the same directory, so uninstalling it deletes the data
/// too. That is what the form says, and what the data checkbox authorizes.
#[derive(Debug, Clone)]
pub(crate) struct PluginUninstallPrecondition {
    pub(crate) id: String,
    pub(crate) system: Option<SystemPlugin>,
    pub(crate) name: String,
    pub(crate) digest: String,
    /// The Plugin's own `mutable` declarations with the bytes each one holds.
    /// `None` is an honest "could not be measured", never a made-up 0.
    pub(crate) mutable: Vec<(String, Option<u64>)>,
    /// Sessions whose switches mount it, across every owner.
    pub(crate) sessions: Vec<String>,
    /// Attached Runs that are not terminal and bind it.
    pub(crate) unfinished_runs: Vec<String>,
    /// Saved Graphs that reference it. Shown on the form, not a refusal: a Graph
    /// that loses a Plugin fails its own next admission, and Graphs are the
    /// operator's to change.
    pub(crate) graphs: Vec<String>,
    /// How much of the Graph catalog the reference count actually saw.
    pub(crate) coverage: ImpactCoverage,
}

impl PluginUninstallPrecondition {
    /// Why this Plugin may not be uninstalled right now, naming the holder.
    pub(crate) fn refusal(&self) -> Option<String> {
        if self.system.is_some() {
            return Some("System Plugins are owned by Anchor and cannot be uninstalled; disable them in the desired context instead".into());
        }
        if !self.sessions.is_empty() {
            return Some(format!(
                "还有 {} 个会话挂着它（{}）：先在所有会话里卸载它，再卸载插件",
                self.sessions.len(),
                self.sessions.join("、")
            ));
        }
        if !self.unfinished_runs.is_empty() {
            return Some(format!(
                "还有 {} 个未结束的 Run 绑定它（{}）：等它们结束或先停止/放弃它们，再卸载插件",
                self.unfinished_runs.len(),
                self.unfinished_runs.join("、")
            ));
        }
        None
    }
}

/// What one uninstall actually did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PluginUninstallOutcome {
    pub(crate) id: String,
    /// Bytes the Plugin directory and its views held.
    pub(crate) removed_bytes: u64,
    /// The Plugin's views, removed with it.
    pub(crate) removed_views: Vec<String>,
    /// Package versions this uninstall freed from the shared store.
    pub(crate) reclaimed_package_versions: Vec<String>,
    /// Store bytes the reclamation pass actually freed.
    pub(crate) reclaimed_bytes: u64,
    /// Why the reclamation pass could not finish, when it could not. The Plugin
    /// itself is gone; that is reported rather than hidden behind a bare success.
    pub(crate) reclamation: Option<String>,
}

/// The bytes one path holds without following symlinks, or `None` if unreadable.
fn path_bytes(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_dir() {
        return Some(metadata.len());
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(path).ok()? {
        total = total.checked_add(path_bytes(&entry.ok()?.path())?)?;
    }
    Some(total)
}

impl RunApplication {
    /// What uninstalling one Plugin would delete, and who still holds it.
    ///
    /// Read-only, and fail-closed: an id that is not a Plugin id, or a Plugin the
    /// Library does not have, is a named refusal rather than an empty plan.
    pub(crate) async fn plugin_uninstall_precondition(
        &self,
        library_root: &Path,
        id: &str,
        sessions: &anchor_platform_session::SessionStore,
    ) -> Result<PluginUninstallPrecondition, ApplicationError> {
        let root = library_root.to_path_buf();
        let plugin = id.to_owned();
        let definition =
            tokio::task::spawn_blocking(move || Catalog::new(&root).definition(&plugin))
                .await
                .map_err(|_| ApplicationError::Storage("Plugin uninstall task failed".into()))?
                .map_err(|error| ApplicationError::Invalid(error.to_string()))?;
        let directory = definition.directory.clone();
        let declared = definition.mutable.paths().to_vec();
        let mutable_paths = definition.mutable.clone();
        let mutable = tokio::task::spawn_blocking(move || {
            match mutable_paths.resolve(&directory) {
                Ok(resolved) => declared
                    .iter()
                    .cloned()
                    .zip(resolved.iter().map(|path| path_bytes(path)))
                    .collect::<Vec<_>>(),
                // A declaration this host cannot resolve is shown with an unknown
                // size; the whole Plugin directory is removed either way.
                Err(_) => declared
                    .iter()
                    .cloned()
                    .map(|declared| (declared, None))
                    .collect::<Vec<_>>(),
            }
        })
        .await
        .map_err(|_| ApplicationError::Storage("Plugin uninstall task failed".into()))?;
        let mut sessions_mounting = sessions.sessions_mounting(id).map_err(|error| {
            ApplicationError::Storage(format!("Session Plugin mounts are unavailable: {error}"))
        })?;
        let mut unfinished_runs = self.unfinished_runs_binding(id)?;
        let mut impact = self.plugin_impact(id)?;
        sessions_mounting.sort();
        sessions_mounting.dedup();
        unfinished_runs.sort();
        unfinished_runs.dedup();
        impact.graphs.sort();
        impact.graphs.dedup();
        Ok(PluginUninstallPrecondition {
            id: definition.id,
            system: definition.system,
            name: definition.name,
            digest: definition.digest,
            mutable,
            sessions: sessions_mounting,
            unfinished_runs,
            graphs: impact.graphs,
            coverage: impact.coverage,
        })
    }

    /// Attached Runs that are not terminal and bind this Plugin.
    fn unfinished_runs_binding(&self, id: &str) -> Result<Vec<String>, ApplicationError> {
        let mut runs = Vec::new();
        for (run, record) in self.run_views()? {
            if record.is_settled() {
                continue;
            }
            let metadata = self.metadata(&run)?;
            let binds = match metadata
                .as_ref()
                .filter(|metadata| metadata.assistant.is_some())
            {
                Some(metadata) => {
                    crate::assistant_plugins::active_bindings(&self.data_root, &record, metadata)
                        .map_err(ApplicationError::Conflict)?
                        .contains_key(id)
                }
                None => {
                    record.progress().plugin_bindings.contains_key(id)
                        || record
                            .progress()
                            .snapshot
                            .nodes
                            .iter()
                            .any(|node| node.plugins.iter().any(|reference| reference.id == id))
                }
            };
            if binds {
                runs.push(run);
            }
        }
        runs.sort();
        Ok(runs)
    }

    /// Remove one Plugin — its views, then its one directory, data included —
    /// under the Library's one write lock, then reclaim what nothing references.
    ///
    /// The preconditions are re-read here: the human approved one exact state, and
    /// a Plugin that changed or an owner that appeared while they were deciding
    /// asks again instead of proceeding.
    #[cfg(test)]
    pub(crate) async fn uninstall_plugin_if_unchanged(
        &self,
        library_root: &Path,
        precondition: &PluginUninstallPrecondition,
        sessions: &anchor_platform_session::SessionStore,
    ) -> Result<PluginUninstallOutcome, ApplicationError> {
        self.uninstall_plugin_if_unchanged_with_access(
            library_root,
            precondition,
            sessions,
            &DependencyAccess::default(),
        )
        .await
    }

    pub(crate) async fn uninstall_plugin_if_unchanged_with_access(
        &self,
        library_root: &Path,
        precondition: &PluginUninstallPrecondition,
        sessions: &anchor_platform_session::SessionStore,
        access: &DependencyAccess,
    ) -> Result<PluginUninstallOutcome, ApplicationError> {
        access.check().map_err(plugin_dependency_error)?;
        if let Some(refusal) = precondition.refusal() {
            // A known refusal must not even create a Library lock file. The
            // fresh holder check below still runs for an admissible approval.
            return Err(ApplicationError::Conflict(refusal));
        }
        let catalog_guard = tokio::select! {
            guard = self.graph_catalog_mutation_guard() => guard,
            _ = access.cancelled() => return Err(ApplicationError::Invalid("Plugin uninstall was cancelled while waiting for its catalog guard".into())),
        };
        let lease =
            crate::plugin_library::publication_lease(library_root, access, "uninstall").await?;
        access.check().map_err(plugin_dependency_error)?;
        // Recheck only after both writers' guards have been acquired: waiting
        // for publication may have allowed another host to replace the Plugin.
        let current = self
            .plugin_uninstall_precondition(library_root, &precondition.id, sessions)
            .await?;
        if current.digest != precondition.digest {
            return Err(ApplicationError::Conflict(
                "the installed Plugin changed while the user was deciding; ask again".into(),
            ));
        }
        if let Some(refusal) = current.refusal() {
            return Err(ApplicationError::Conflict(format!(
                "{refusal}（确认期间状态变了；请重新确认）"
            )));
        }
        let root = resolved_library_root(library_root);
        let plugins_root = library_root.join("plugins");
        let credential_root = self.data_root.clone();
        let id = precondition.id.clone();
        let access = access.clone();
        let (removed, reclamation) = tokio::task::spawn_blocking(move || {
            let _catalog_guard = catalog_guard;
            let _lease = lease;
            if let Err(error) = access.check() {
                return (Err(error.to_string()), None);
            }
            let mut removed_views = Vec::new();
            let mut removed_bytes = 0u64;
            // Views first: they are derived from the declaration, and removing the
            // Plugin directory first would only make them orphans for the pass
            // below. A failure here leaves the Plugin installed and is reported.
            let views = views_root(&root).join(&id);
            if views.exists() {
                removed_views = std::fs::read_dir(&views)
                    .map(|entries| {
                        entries
                            .flatten()
                            .map(|entry| entry.file_name().to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                removed_views.sort();
                removed_bytes += path_bytes(&views).unwrap_or(0);
                if let Err(error) = std::fs::remove_dir_all(&views) {
                    return (
                        Err(format!(
                            "plugin `{id}` view `{}` could not be removed: {error}",
                            views.display()
                        )),
                        None,
                    );
                }
            }
            // The one directory, data included. The human's checkbox authorized
            // exactly this: the Plugin has no second copy anywhere.
            let directory = plugins_root.join(&id);
            removed_bytes += path_bytes(&directory).unwrap_or(0);
            if let Err(error) = std::fs::remove_dir_all(&directory) {
                return (
                    Err(format!(
                        "plugin `{id}` directory `{}` could not be removed: {error}",
                        directory.display()
                    )),
                    None,
                );
            }
            if let Err(error)=crate::credential_store::delete_plugin(&credential_root,&id) {
                return (Err(format!("plugin `{id}` was uninstalled but its private credentials could not be removed: {error}")),None);
            }
            let reclamation = anchor_library::reclaim_after_install(&root, true)
                .map_err(|error| error.to_string());
            (Ok((removed_bytes, removed_views)), Some(reclamation))
        })
        .await
        .map_err(|_| ApplicationError::Storage("Plugin uninstall task failed".into()))?;
        let (removed_bytes, removed_views) = removed.map_err(ApplicationError::Storage)?;
        let (reclaimed_package_versions, reclaimed_bytes, reclamation) = match reclamation {
            Some(Ok(report)) => (
                report.removed_package_versions.clone(),
                report.reclaimed_bytes,
                // A sweep that refused to run, or abandoned its second pass, is a
                // reclamation that did not happen: the outcome says so instead of
                // reporting a clean success.
                report
                    .deferred
                    .clone()
                    .or_else(|| report.store_phase_abandoned.clone()),
            ),
            Some(Err(reason)) => (Vec::new(), 0, Some(reason)),
            None => (Vec::new(), 0, None),
        };
        Ok(PluginUninstallOutcome {
            id: precondition.id.clone(),
            removed_bytes,
            removed_views,
            reclaimed_package_versions,
            reclaimed_bytes,
            reclamation,
        })
    }
}

#[cfg(test)]
async fn wait_for_catalog_guard(application: &RunApplication) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while application.catalog_mutations.try_lock().is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("mutation did not acquire the catalog guard");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{fs, path::Path};

    fn candidate(root: &Path, marker: &str) -> PathBuf {
        let directory = root.join(format!("candidate-{marker}"));
        fs::create_dir_all(directory.join("skills/check")).unwrap();
        fs::write(
            directory.join("plugin.json"),
            json!({
                "name": "Candidate",
                "description": marker,
                "skills": "skills",
                "mcpServers": {"probe": {"command": "bin/run"}}
            })
            .to_string(),
        )
        .unwrap();
        fs::write(directory.join("skills/check/SKILL.md"), "# check\n").unwrap();
        directory
    }

    fn application(root: &Path) -> RunApplication {
        RunApplication::new(root.join("state"), root.to_path_buf())
    }

    fn installed_digest(library_root: &Path, id: &str) -> Option<String> {
        Catalog::new(library_root)
            .definition(id)
            .ok()
            .map(|definition| definition.digest)
    }

    #[tokio::test]
    async fn installation_waits_cancel_without_publishing_and_a_later_install_can_continue() {
        for (catalog_wait, cancel_wait) in [(false, true), (true, true), (true, false)] {
            let temporary = tempfile::tempdir().unwrap();
            let library_root = temporary.path().join("library");
            let application = application(temporary.path());
            let source = candidate(temporary.path(), "waiting");
            let access = DependencyAccess::default();
            let precondition = application
                .plugin_install_precondition_with_access(
                    library_root.clone(),
                    source.clone(),
                    "demo".into(),
                    access.clone(),
                )
                .await
                .unwrap();
            let catalog_guard = if catalog_wait {
                Some(application.graph_catalog_mutation_guard().await)
            } else {
                None
            };
            let library_lease = if catalog_wait {
                None
            } else {
                Some(PluginLibrary::new(&library_root).install_lease().unwrap())
            };
            let installation =
                application.install_plugin_if_unchanged(library_root.clone(), precondition);
            tokio::pin!(installation);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(60), &mut installation)
                    .await
                    .is_err(),
                "the install must wait for the held guard"
            );
            if !cancel_wait {
                drop(catalog_guard);
                drop(library_lease);
                let installed =
                    tokio::time::timeout(std::time::Duration::from_secs(2), installation)
                        .await
                        .expect(
                            "the same waiting install must continue after the guard is released",
                        )
                        .unwrap();
                assert_eq!(
                    installed_digest(&library_root, "demo"),
                    Some(installed.digest)
                );
                continue;
            }
            access
                .cancellation
                .store(true, std::sync::atomic::Ordering::Release);
            let error = tokio::time::timeout(std::time::Duration::from_secs(2), installation)
                .await
                .expect("waiting for either publication guard must be cancellable")
                .unwrap_err();
            assert!(format!("{error:?}").contains(if catalog_wait {
                "catalog guard"
            } else {
                "cancel"
            }));
            assert!(installed_digest(&library_root, "demo").is_none());
            drop(catalog_guard);
            drop(library_lease);
            let next = application
                .plugin_install_precondition(library_root.clone(), source, "demo".into())
                .await
                .unwrap();
            let installed = application
                .install_plugin_if_unchanged(library_root.clone(), next)
                .await
                .unwrap();
            assert_eq!(
                installed_digest(&library_root, "demo"),
                Some(installed.digest)
            );
        }
    }

    #[tokio::test]
    async fn a_candidate_changed_while_waiting_for_the_library_lease_is_not_installed() {
        let temporary = tempfile::tempdir().unwrap();
        let library_root = temporary.path().join("library");
        let application = application(temporary.path());
        let source = candidate(temporary.path(), "approved");
        let precondition = application
            .plugin_install_precondition(library_root.clone(), source.clone(), "demo".into())
            .await
            .unwrap();
        let lease = PluginLibrary::new(&library_root).install_lease().unwrap();
        let installation = tokio::spawn({
            let (application, library_root) = (application.clone(), library_root.clone());
            async move {
                application
                    .install_plugin_if_unchanged(library_root, precondition)
                    .await
            }
        });
        wait_for_catalog_guard(&application).await;
        // The existing early validation is tiny; wait for the installer to reach
        // the externally held Library lease before changing its candidate.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        fs::write(
            source.join("skills/check/UNAPPROVED.md"),
            "not in the approval\n",
        )
        .unwrap();
        drop(lease);
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), installation)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(result, Err(ApplicationError::Conflict(_))),
            "{result:?}"
        );
        assert!(installed_digest(&library_root, "demo").is_none());
    }

    #[tokio::test]
    async fn an_installed_version_changed_during_the_library_lease_wait_is_not_overwritten() {
        let temporary = tempfile::tempdir().unwrap();
        let library_root = temporary.path().join("library");
        let application = application(temporary.path());
        let first = candidate(temporary.path(), "first");
        PluginLibrary::new(&library_root)
            .install_directory("demo", &first, false)
            .unwrap();
        let approved = candidate(temporary.path(), "approved");
        let precondition = application
            .plugin_install_precondition(library_root.clone(), approved, "demo".into())
            .await
            .unwrap();
        let lease = PluginLibrary::new(&library_root).install_lease().unwrap();
        let installation = tokio::spawn({
            let (application, library_root) = (application.clone(), library_root.clone());
            async move {
                application
                    .install_plugin_if_unchanged(library_root, precondition)
                    .await
            }
        });
        wait_for_catalog_guard(&application).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let external = candidate(temporary.path(), "external");
        let published = PluginLibrary::new(&library_root)
            .install_directory_locked("demo", &external, true, &lease)
            .unwrap();
        drop(lease);
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), installation)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(result, Err(ApplicationError::Conflict(_))),
            "{result:?}"
        );
        assert_eq!(
            installed_digest(&library_root, "demo"),
            Some(published.digest)
        );
    }

    #[tokio::test]
    async fn dependency_preparation_publishes_only_the_private_approved_snapshot() {
        use crate::dependency_provision::{DependencyProvisioner, test_support};
        let Some(toolchain) = test_support::toolchain() else {
            return;
        };
        let temporary = tempfile::tempdir().unwrap();
        let library_root = temporary.path().join("library");
        let wheels = temporary.path().join("wheels");
        fs::create_dir_all(&wheels).unwrap();
        test_support::fixture_wheel(&wheels, "probepkg", "0.1.0");
        let provisioner = Arc::new(
            DependencyProvisioner::new(test_support::config(temporary.path(), &toolchain, wheels))
                .unwrap(),
        );
        let application = application(temporary.path()).with_dependency_provisioner(provisioner);
        let source = candidate(temporary.path(), "approved");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(source.join("plugin.json")).unwrap()).unwrap();
        manifest["dependencies"] = json!({"python":["probepkg==0.1.0"]});
        fs::write(source.join("plugin.json"), manifest.to_string()).unwrap();
        let access = DependencyAccess {
            network: false,
            ..Default::default()
        };
        let precondition = application
            .plugin_install_precondition_with_access(
                library_root.clone(),
                source.clone(),
                "demo".into(),
                access,
            )
            .await
            .unwrap();
        let expected = precondition.review.digest.clone();
        let dependencies = library_root.join("dependencies");
        fs::create_dir_all(&dependencies).unwrap();
        let lock_path = dependencies.join(".lock");
        let held = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).unwrap();
        let installation = tokio::spawn({
            let (application, library_root) = (application.clone(), library_root.clone());
            async move {
                application
                    .install_plugin_if_unchanged(library_root, precondition)
                    .await
            }
        });
        // The second lock descriptor proves dependency preparation has reached
        // its wait, after the candidate was copied and validated.
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let waiting = fs::read_dir("/proc/self/fd")
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter_map(|entry| fs::read_link(entry.path()).ok())
                    .filter(|path| path == &lock_path)
                    .count()
                    > 1;
                if waiting {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("dependency preparation did not reach the held store lease");
        fs::write(
            source.join("skills/check/UNAPPROVED.md"),
            "after snapshot\n",
        )
        .unwrap();
        drop(held);
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), installation)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.digest, expected);
        assert!(!result.directory.join("skills/check/UNAPPROVED.md").exists());
        assert!(source.join("skills/check/UNAPPROVED.md").exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_publication_removes_its_new_view_before_releasing_the_library_lease() {
        use crate::dependency_provision::{DependencyProvisioner, test_support};
        let Some(toolchain) = test_support::toolchain() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let library_root = root.path().join("library");
        let original = candidate(root.path(), "original");
        fs::write(
            original.join("plugin.json"),
            json!({"name":"Original","mutable":["state/"]}).to_string(),
        )
        .unwrap();
        PluginLibrary::new(&library_root)
            .install_directory("demo", original, false)
            .unwrap();
        fs::write(library_root.join("plugins/demo/state/data"), "keep").unwrap();
        let before = fs::read(library_root.join("plugins/demo/plugin.json")).unwrap();

        let wheels = root.path().join("wheels");
        fs::create_dir(&wheels).unwrap();
        // This fixture pauses the real Python verification at a visible marker.
        // It lets an independent Library writer wait on the actual flock before
        // publication fails, without timers or a production-only test hook.
        let files = vec![
            ("holdpkg/__init__.py".into(), b"VALUE = '0.1.0'\n".to_vec()),
            ("holdpkg-0.1.0.dist-info/METADATA".into(), b"Metadata-Version: 2.1\nName: holdpkg\nVersion: 0.1.0\n".to_vec()),
            ("holdpkg-0.1.0.dist-info/WHEEL".into(), b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_vec()),
            ("holdpkg-0.1.0.dist-info/RECORD".into(), Vec::new()),
            ("hold-verification.pth".into(), b"import os, sys, time; open('/workspace/verification-ready', 'w').close(); exec(\"while not os.path.exists(os.path.join(sys.prefix, 'release-verification')):\\n time.sleep(0.005)\")\n".to_vec()),
        ];
        fs::write(
            wheels.join("holdpkg-0.1.0-py3-none-any.whl"),
            test_support::write_zip(&files),
        )
        .unwrap();
        let provisioner = Arc::new(
            DependencyProvisioner::new(test_support::config(root.path(), &toolchain, wheels))
                .unwrap(),
        );
        let verification_ready = provisioner
            .config
            .build_root
            .join("plugin-dependencies/verify/verification-ready");
        let application = application(root.path()).with_dependency_provisioner(provisioner);
        let replacement = candidate(root.path(), "replacement");
        fs::create_dir(replacement.join("state")).unwrap();
        fs::write(
            replacement.join("state/unapproved-code"),
            "must not overwrite data",
        )
        .unwrap();
        fs::write(
            replacement.join("plugin.json"),
            json!({"name":"Replacement","dependencies":{"python":["holdpkg==0.1.0"]}}).to_string(),
        )
        .unwrap();
        let precondition = application
            .plugin_install_precondition_with_access(
                library_root.clone(),
                replacement,
                "demo".into(),
                DependencyAccess {
                    network: false,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let view = precondition.dependencies.as_ref().unwrap().view.clone();
        let installation = tokio::spawn({
            let library_root = library_root.clone();
            async move {
                application
                    .install_plugin_if_unchanged(library_root, precondition)
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !verification_ready.is_file() {
                assert!(
                    !installation.is_finished(),
                    "installer ended before verification"
                );
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("real dependency verification did not reach its marker");
        let slow_cleanup = view.join("rollback-fixture");
        fs::create_dir(&slow_cleanup).unwrap();
        for index in 0..2048 {
            fs::write(slow_cleanup.join(index.to_string()), []).unwrap();
        }
        let waiting_writer = tokio::task::spawn_blocking({
            let (library_root, view) = (library_root.clone(), view.clone());
            move || {
                let _lease = PluginLibrary::new(&library_root).install_lease().unwrap();
                view.exists()
            }
        });
        let lock_path = library_root.join("plugins/.install.lock");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(Result::ok)
                .filter_map(|entry| fs::read_link(entry.path()).ok())
                .filter(|path| path == &lock_path)
                .count()
                < 2
            {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("independent Library writer did not reach the publication lease");
        fs::write(view.join("release-verification"), []).unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), installation)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(result, Err(ApplicationError::Invalid(ref reason)) if reason.contains("mutable path")),
            "{result:?}"
        );
        assert!(
            !waiting_writer.await.unwrap(),
            "another writer acquired the Library lease before rollback removed the view"
        );
        assert!(!view.exists());
        assert_eq!(
            fs::read(library_root.join("plugins/demo/plugin.json")).unwrap(),
            before
        );
        assert_eq!(
            fs::read(library_root.join("plugins/demo/state/data")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn rollback_preserves_a_view_referenced_by_the_current_plugin() {
        let root = tempfile::tempdir().unwrap();
        let library_root = root.path().join("library");
        let source = candidate(root.path(), "referenced");
        fs::write(
            source.join("plugin.json"),
            json!({"name":"Referenced","dependencies":{"python":["holdpkg==0.1.0"]}}).to_string(),
        )
        .unwrap();
        PluginLibrary::new(&library_root)
            .install_directory("demo", source, false)
            .unwrap();
        let definition = Catalog::new(&library_root).definition("demo").unwrap();
        let view = plugin_view_path(
            &resolved_library_root(&library_root),
            "demo",
            &definition.dependencies.declaration_digest(),
        );
        fs::create_dir_all(&view).unwrap();
        fs::write(
            view.join("keep"),
            "a partial publication already references this view",
        )
        .unwrap();
        let lease = PluginLibrary::new(&library_root).install_lease().unwrap();
        remove_unreferenced_view(&library_root, "demo", &view, &lease).unwrap();
        assert!(view.join("keep").is_file());
    }

    #[tokio::test]
    async fn dependency_index_planning_cancels_a_pending_http_probe_and_can_retry() {
        use crate::dependency_provision::{DependencyProvisioner, test_support};
        use sha2::Digest;
        use std::{
            os::unix::fs::PermissionsExt,
            sync::atomic::{AtomicBool, Ordering},
        };
        let Some(toolchain) = test_support::toolchain() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let uv = root.path().join("uv");
        // Freeze one fixture lock so this case targets the Host's HTTP probe,
        // independent of how many index requests a resolver release performs.
        fs::write(&uv, "#!/bin/sh\nwhile [ \"$1\" != \"--output-file\" ]; do shift; done\nshift\nprintf 'probepkg==0.1.0 --hash=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n' > \"$1\"\n").unwrap();
        fs::set_permissions(&uv, fs::Permissions::from_mode(0o755)).unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let blocked = Arc::new(AtomicBool::new(true));
        let (notification, hold) = (entered.clone(), blocked.clone());
        let app = axum::Router::new().route(
            "/simple/probepkg/",
            axum::routing::get(move || {
                let (notification, hold) = (notification.clone(), hold.clone());
                async move {
                    notification.notify_one();
                    if hold.load(Ordering::Acquire) {
                        std::future::pending::<()>().await;
                    }
                    (
                        [("content-type", "application/vnd.pypi.simple.v1+json")],
                        "{}",
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let index = format!("http://{}/simple", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = test_support::config_with_sources(
            root.path(),
            &toolchain,
            vec![PackageSource::Index(index)],
        );
        config.uv_digest = format!("{:x}", sha2::Sha256::digest(fs::read(&uv).unwrap()));
        config.uv = uv;
        let application = application(root.path())
            .with_dependency_provisioner(Arc::new(DependencyProvisioner::new(config).unwrap()));
        let source = candidate(root.path(), "http");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(source.join("plugin.json")).unwrap()).unwrap();
        manifest["dependencies"] = json!({"python":["probepkg==0.1.0"]});
        fs::write(source.join("plugin.json"), manifest.to_string()).unwrap();
        let library = root.path().join("library");
        let review = PluginLibrary::new(&library)
            .review_directory("demo", &source)
            .unwrap();
        let access = DependencyAccess::default();
        let cancel = async {
            entered.notified().await;
            access.cancellation.store(true, Ordering::Release);
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(
                application.plugin_dependency_plan(&library, &review, &access),
                cancel
            )
        })
        .await
        .expect("pending dependency index probe must respond to cancellation");
        assert!(format!("{:?}", result.unwrap_err()).contains("cancelled"));
        blocked.store(false, Ordering::Release);
        let (plan, _) = application
            .plugin_dependency_plan(&library, &review, &DependencyAccess::default())
            .await
            .unwrap();
        assert!(plan.is_some());
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn a_precondition_reports_creation_then_an_identical_install() {
        let temporary = tempfile::tempdir().unwrap();
        let library_root = temporary.path().join("library");
        let application = application(temporary.path());
        let source = candidate(temporary.path(), "first");

        let before = application
            .plugin_install_precondition(library_root.clone(), source.clone(), "demo".into())
            .await
            .unwrap();
        assert!(!before.replacing());
        assert!(!before.already_installed());

        application
            .install_plugin_if_unchanged(library_root.clone(), before.clone())
            .await
            .unwrap();
        assert_eq!(
            installed_digest(&library_root, "demo").as_deref(),
            Some(before.review.digest.as_str())
        );

        let after = application
            .plugin_install_precondition(library_root, source, "demo".into())
            .await
            .unwrap();
        assert!(after.replacing());
        assert!(
            after.already_installed(),
            "the same candidate must be reported as already installed"
        );
    }

    #[tokio::test]
    async fn a_candidate_that_changed_after_review_is_never_installed() {
        let temporary = tempfile::tempdir().unwrap();
        let library_root = temporary.path().join("library");
        let application = application(temporary.path());
        let source = candidate(temporary.path(), "first");
        let precondition = application
            .plugin_install_precondition(library_root.clone(), source.clone(), "demo".into())
            .await
            .unwrap();

        // The tree the operator looked at is no longer the tree on disk.
        fs::write(
            source.join("skills/check/EXTRA.md"),
            "changed after review\n",
        )
        .unwrap();

        let failure = application
            .install_plugin_if_unchanged(library_root.clone(), precondition)
            .await
            .unwrap_err();
        assert!(
            matches!(failure, ApplicationError::Conflict(_)),
            "{failure:?}"
        );
        assert!(installed_digest(&library_root, "demo").is_none());
    }

    #[tokio::test]
    async fn a_library_that_changed_after_review_is_not_overwritten() {
        let temporary = tempfile::tempdir().unwrap();
        let library_root = temporary.path().join("library");
        let application = application(temporary.path());
        let source = candidate(temporary.path(), "first");
        let precondition = application
            .plugin_install_precondition(library_root.clone(), source, "demo".into())
            .await
            .unwrap();

        // Another writer installed a different version after the review.
        let other = candidate(temporary.path(), "second");
        PluginLibrary::new(&library_root)
            .install_directory("demo", &other, false)
            .unwrap();
        let installed = installed_digest(&library_root, "demo");

        let failure = application
            .install_plugin_if_unchanged(library_root.clone(), precondition)
            .await
            .unwrap_err();
        assert!(
            matches!(failure, ApplicationError::Conflict(_)),
            "{failure:?}"
        );
        assert_eq!(installed_digest(&library_root, "demo"), installed);
    }

    #[tokio::test]
    async fn an_approved_precondition_replaces_only_the_reviewed_version() {
        let temporary = tempfile::tempdir().unwrap();
        let library_root = temporary.path().join("library");
        let application = application(temporary.path());
        let first = candidate(temporary.path(), "first");
        let first_precondition = application
            .plugin_install_precondition(library_root.clone(), first, "demo".into())
            .await
            .unwrap();
        application
            .install_plugin_if_unchanged(library_root.clone(), first_precondition)
            .await
            .unwrap();

        let second = candidate(temporary.path(), "second");
        let precondition = application
            .plugin_install_precondition(library_root.clone(), second, "demo".into())
            .await
            .unwrap();
        assert!(precondition.replacing());
        assert!(!precondition.already_installed());
        let outcome = application
            .install_plugin_if_unchanged(library_root.clone(), precondition.clone())
            .await
            .unwrap();
        assert_eq!(outcome.digest, precondition.review.digest);
        assert_eq!(
            installed_digest(&library_root, "demo").as_deref(),
            Some(precondition.review.digest.as_str())
        );
    }
}

#[cfg(test)]
mod impact_tests {
    use super::*;
    use crate::application::run_access::fixtures;
    use anchor_graph_host::PluginCatalog;
    use anchor_runtime::graph::GraphRunRecord;
    use serde_json::json;
    use std::fs;

    fn candidate(root: &std::path::Path, marker: &str) -> PathBuf {
        let directory = root.join(format!("candidate-{marker}"));
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("plugin.json"),
            json!({
                "name": "Candidate",
                "description": marker,
                "mcpServers": {"probe": {"command": "bin/run"}}
            })
            .to_string(),
        )
        .unwrap();
        directory
    }

    /// A non-terminal Run that froze a binding for this Plugin.
    fn unfinished_run(root: &std::path::Path, id: &str) -> String {
        let state = root.join("state");
        let application = RunApplication::new(state, root.to_path_buf());
        let mut record = GraphRunRecord::create_with_id(
            serde_json::from_value(json!({
                "objective": "fixture",
                "entry": "only",
                "agents": {},
                "ops": {"run": {"run": "true"}},
                "nodes": [{"id": "only", "op": "run"}],
                "edges": []
            }))
            .unwrap(),
            json!({}),
            "run-bound",
        )
        .unwrap();
        record.plugin_bindings.insert(
            id.to_owned(),
            anchor_runtime::graph::PluginBinding {
                id: id.to_owned(),
                digest: "a".repeat(64),
                resources: Vec::new(),
                mcp_servers: vec!["probe".into()],
                authorization_digest: "fixture-authority"
                    .try_into()
                    .expect("non-empty fixture authority"),
            },
        );
        record.plugin_bindings_initialized = true;
        fixtures::create(&application, &record);
        record.run_id
    }

    #[tokio::test]
    async fn replacing_a_plugin_takes_effect_without_a_second_decision() {
        let root = tempfile::tempdir().unwrap();
        let library_root = root.path().join("library");
        let application = RunApplication::new(root.path().join("state"), root.path().to_path_buf());
        let source = candidate(root.path(), "first");
        application
            .install_plugin_if_unchanged(
                library_root.clone(),
                application
                    .plugin_install_precondition(
                        library_root.clone(),
                        source.clone(),
                        "demo".into(),
                    )
                    .await
                    .unwrap(),
            )
            .await
            .unwrap();
        // A Run that is not terminal binds the Plugin. Upgrading is always
        // allowed (standard invariant 7), so the impact report says nothing about
        // it and the replacement happens.
        let run = unfinished_run(root.path(), "demo");
        assert!(!run.is_empty());

        let next = candidate(root.path(), "second");
        let precondition = application
            .plugin_install_precondition(library_root.clone(), next, "demo".into())
            .await
            .unwrap();
        assert!(precondition.impact.graphs.is_empty());
        application
            .install_plugin_if_unchanged(library_root.clone(), precondition.clone())
            .await
            .unwrap();
        assert_eq!(
            Catalog::new(&library_root)
                .definition("demo")
                .unwrap()
                .digest,
            precondition.review.digest
        );
    }

    /// Only `ANCHOR_RUNNER_BUNDLE_ROOT` is set — the distribution's documented
    /// minimum. The deployment has no separate Library root, so the Graph catalog
    /// (here: the bundle's parent) *is* the Plugin home, and every caller takes that
    /// answer from one place: the API that installs a Plugin and the node call that
    /// runs it cannot disagree.
    ///
    /// A bundle directory itself is never the Library: the Plugin home is
    /// `<bundle parent>/plugins`.
    #[tokio::test]
    async fn a_deployment_with_only_a_bundle_root_has_one_library_root() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let bundle = root.path().join("bundles/main");
        fs::create_dir_all(&bundle).unwrap();
        let _guard = crate::PROCESS_ENV_LOCK.lock().await;
        let mut environment = crate::test_support::Environment::cleared(&[]);
        environment.set("ANCHOR_RUNNER_BUNDLE_ROOT", &bundle);
        // The same root the API serves and the resolver binds from.
        let catalog_root = bundle.parent().unwrap();
        let application = RunApplication::new(state.clone(), catalog_root.to_path_buf());
        let library_root = application.library_root().to_path_buf();
        let source = candidate(root.path(), "only-bundle");
        let installed = application
            .install_plugin_if_unchanged(
                library_root.clone(),
                application
                    .plugin_install_precondition(library_root.clone(), source, "demo".into())
                    .await
                    .unwrap(),
            )
            .await
            .unwrap();
        // The Plugin landed in the one Plugin home, not beside the bundle.
        assert_eq!(installed.directory, catalog_root.join("plugins/demo"));
        assert!(!bundle.join("plugins").exists());
        assert!(
            crate::application::graphs::referenced_plugin_bindings(
                &library_root,
                &["demo".to_owned()]
            )
            .is_ok(),
            "the node call resolves the Plugin the API installed"
        );
    }

    /// A Graph this host cannot load is a *gap* in the report, never a silent
    /// omission: the operator is being asked to trust a count.
    #[tokio::test]
    async fn an_unreadable_graph_is_reported_as_a_gap_not_left_out() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let application = RunApplication::new(root.path().join("state"), catalog.clone());

        // A loadable bundle that binds the Plugin.
        let bound = catalog.join("bound");
        fs::create_dir_all(&bound).unwrap();
        // The Plugin lives in the Library, not in the bundle.
        fs::create_dir_all(catalog.join("plugins/demo")).unwrap();
        fs::write(
            catalog.join("plugins/demo/plugin.json"),
            json!({"name":"Demo","mcpServers":{"probe":{"command":"bin/run"}}}).to_string(),
        )
        .unwrap();
        Catalog::new(&catalog)
            .resolve(&["demo".to_owned()])
            .unwrap();
        fs::write(
            bound.join("graph.json"),
            json!({
                "objective": "uses the plugin",
                "entry": "only",
                "agents": {"worker": {"model": "fixture"}},
                "ops": {},
                "nodes": [{"id": "only", "agent": "worker", "plugins": ["demo"]}],
                "edges": []
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            bound.join("manifest.json"),
            json!({
                "format": 1,
                "graph": "graph.json",
                "plugins": [{"id": "demo"}]
            })
            .to_string(),
        )
        .unwrap();

        // A Graph directory with no manifest cannot be loaded at all.
        fs::create_dir_all(catalog.join("broken")).unwrap();
        fs::write(
            catalog.join("broken/graph.json"),
            json!({
                "objective": "broken",
                "entry": "only",
                "agents": {},
                "ops": {"run": {"run": "true"}},
                "nodes": [{"id": "only", "op": "run"}],
                "edges": []
            })
            .to_string(),
        )
        .unwrap();

        let impact = application.plugin_impact("demo").unwrap();
        assert_eq!(impact.graphs, vec!["bound".to_owned()], "{impact:?}");
        match impact.coverage {
            ImpactCoverage::Partial { missing, .. } => {
                assert_eq!(missing.len(), 1, "{missing:?}");
                assert_eq!(missing[0].graph, "broken");
                assert!(!missing[0].reason.is_empty(), "{missing:?}");
            }
            other => panic!("a Graph this host cannot load is a gap, got {other:?}"),
        }
    }

    /// A deployment that authorizes no package installation refuses a Plugin
    /// that declares dependencies, before anything is written. Installing it
    /// would produce a Plugin whose tools can never start.
    #[tokio::test]
    async fn a_declaration_on_a_deployment_that_authorizes_no_install_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let library_root = root.path().join("library");
        let application = RunApplication::new(root.path().join("state"), root.path().to_path_buf());
        let source = candidate(root.path(), "dependencies");
        fs::write(
            source.join("plugin.json"),
            json!({
                "name": "Dependent",
                "mcpServers": {"probe": {"command": "${ANCHOR_PLUGIN_VIEW}/bin/python"}},
                "dependencies": {"python": ["idna==3.7"]}
            })
            .to_string(),
        )
        .unwrap();

        let failure = application
            .plugin_install_precondition(library_root.clone(), source, "dependent".into())
            .await
            .unwrap_err();
        assert!(
            matches!(failure, ApplicationError::Invalid(ref reason)
                if reason.contains("does not authorize installing them")),
            "{failure:?}"
        );
        assert!(
            !library_root.join("plugins/dependent").exists(),
            "a refused declaration must not be installed"
        );
        assert!(
            !library_root.join("dependencies").exists(),
            "a refused declaration must not create store or views"
        );
    }
}

/// Uninstalling a Plugin: what it refuses, what it deletes, what it frees.
#[cfg(test)]
mod uninstall_tests {
    use super::*;
    use crate::application::run_access::fixtures;
    use anchor_graph_host::{plugin_view_path, store_root, views_root};
    use anchor_platform_session::{CreateSession, SessionStore};
    use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot};
    use serde_json::json;
    use std::{fs, path::Path};

    /// Every byte the Library owns: Plugin directories, views and the store.
    fn library_bytes(root: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(root: &Path, directory: &Path, files: &mut Vec<(String, Vec<u8>)>) {
            let mut entries = fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    walk(root, &path, files);
                } else {
                    let relative = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    files.push((relative, fs::read(&path).unwrap()));
                }
            }
        }
        let mut files = Vec::new();
        for subtree in ["plugins", "dependencies"] {
            let path = root.join(subtree);
            if path.is_dir() {
                walk(root, &path, &mut files);
            }
        }
        files
    }

    fn write_candidate(root: &Path, data: &[u8]) -> PathBuf {
        let candidate = root.join("candidate");
        fs::create_dir_all(candidate.join("state")).unwrap();
        fs::write(
            candidate.join("plugin.json"),
            json!({"name":"Demo","mutable":["state/"]}).to_string(),
        )
        .unwrap();
        fs::write(candidate.join("state/data.bin"), data).unwrap();
        candidate
    }

    /// Install `demo`, populate its own data, and prepare to uninstall it.
    async fn installed(
        root: &Path,
        sessions: &SessionStore,
        data: &[u8],
    ) -> (RunApplication, PluginUninstallPrecondition) {
        let library_root = root.join("library");
        let application = RunApplication::new(root.join("state"), root.to_path_buf());
        let precondition = application
            .plugin_install_precondition(
                library_root.clone(),
                write_candidate(root, data),
                "demo".into(),
            )
            .await
            .unwrap();
        application
            .install_plugin_if_unchanged(library_root.clone(), precondition)
            .await
            .unwrap();
        fs::write(library_root.join("plugins/demo/state/data.bin"), data).unwrap();
        let uninstall = application
            .plugin_uninstall_precondition(&library_root, "demo", sessions)
            .await
            .unwrap();
        (application, uninstall)
    }

    fn session_store(root: &Path) -> SessionStore {
        fs::create_dir_all(root.join("state/platform")).unwrap();
        let sessions = SessionStore::open(root.join("state/platform/sessions.sqlite")).unwrap();
        sessions
            .create(
                "local",
                CreateSession {
                    id: Some("sess".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        sessions
    }

    #[tokio::test]
    async fn uninstall_rechecks_the_approved_version_after_waiting_for_publication() {
        let root = tempfile::tempdir().unwrap();
        let sessions = session_store(root.path());
        let (application, precondition) = installed(root.path(), &sessions, b"precious").await;
        let library_root = root.path().join("library");
        let lease = PluginLibrary::new(&library_root).install_lease().unwrap();
        let deletion = tokio::spawn({
            let (application, library_root, sessions) =
                (application.clone(), library_root.clone(), sessions.clone());
            async move {
                application
                    .uninstall_plugin_if_unchanged(&library_root, &precondition, &sessions)
                    .await
            }
        });
        wait_for_catalog_guard(&application).await;
        let replacement = write_candidate(root.path(), b"unused");
        fs::write(
            replacement.join("plugin.json"),
            json!({"name":"Replacement","mutable":["state/"]}).to_string(),
        )
        .unwrap();
        PluginLibrary::new(&library_root)
            .install_directory_locked("demo", &replacement, true, &lease)
            .unwrap();
        let before = library_bytes(&library_root);
        drop(lease);
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), deletion)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(error, ApplicationError::Conflict(ref reason) if reason.contains("changed")),
            "{error:?}"
        );
        assert_eq!(library_bytes(&library_root), before);
        assert_eq!(
            fs::read(library_root.join("plugins/demo/state/data.bin")).unwrap(),
            b"precious"
        );
    }

    #[tokio::test]
    async fn uninstall_rechecks_holders_after_waiting_for_publication() {
        let root = tempfile::tempdir().unwrap();
        let sessions = session_store(root.path());
        let (application, precondition) = installed(root.path(), &sessions, b"precious").await;
        let library_root = root.path().join("library");
        let lease = PluginLibrary::new(&library_root).install_lease().unwrap();
        let before = library_bytes(&library_root);
        let deletion = tokio::spawn({
            let (application, library_root, sessions) =
                (application.clone(), library_root.clone(), sessions.clone());
            async move {
                application
                    .uninstall_plugin_if_unchanged(&library_root, &precondition, &sessions)
                    .await
            }
        });
        wait_for_catalog_guard(&application).await;
        sessions
            .set_plugin_mount("local", "sess", "demo", true, "operator", "click")
            .unwrap();
        drop(lease);
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), deletion)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(error, ApplicationError::Conflict(ref reason) if reason.contains("sess")),
            "{error:?}"
        );
        assert_eq!(library_bytes(&library_root), before);
    }

    #[tokio::test]
    async fn uninstall_waits_can_be_cancelled_without_deleting_and_then_retried() {
        for publication_wait in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let sessions = session_store(root.path());
            let (application, precondition) = installed(root.path(), &sessions, b"precious").await;
            let library_root = root.path().join("library");
            let held_catalog = if publication_wait {
                None
            } else {
                Some(application.graph_catalog_mutation_guard().await)
            };
            let held_library = if publication_wait {
                Some(PluginLibrary::new(&library_root).install_lease().unwrap())
            } else {
                None
            };
            let before = library_bytes(&library_root);
            let access = DependencyAccess::default();
            let cancel = async {
                tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                access
                    .cancellation
                    .store(true, std::sync::atomic::Ordering::Release);
            };
            let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::join!(
                    application.uninstall_plugin_if_unchanged_with_access(
                        &library_root,
                        &precondition,
                        &sessions,
                        &access
                    ),
                    cancel
                )
            })
            .await
            .expect("pending uninstall must remain cancellable");
            let error = result.unwrap_err();
            assert!(
                matches!(error, ApplicationError::Invalid(ref reason) if reason.contains("cancelled")),
                "{error:?}"
            );
            assert_eq!(library_bytes(&library_root), before);
            drop(held_catalog);
            drop(held_library);
            application
                .uninstall_plugin_if_unchanged(&library_root, &precondition, &sessions)
                .await
                .unwrap();
            assert!(!library_root.join("plugins/demo").exists());
        }
    }

    #[tokio::test]
    async fn system_plugins_cannot_be_uninstalled_even_without_holders() {
        let root = tempfile::tempdir().unwrap();
        let sessions = session_store(root.path());
        let library = root.path().join("library");
        let application = RunApplication::new(root.path().join("state"), root.path().to_path_buf());
        for plugin in anchor_graph_host::all_system_plugins() {
            let directory = library.join("plugins").join(plugin.id());
            crate::test_system_plugin(&library, *plugin);
            let precondition = application
                .plugin_uninstall_precondition(&library, plugin.id(), &sessions)
                .await
                .unwrap();
            assert_eq!(precondition.system, Some(*plugin));
            assert!(precondition.sessions.is_empty());
            assert!(precondition.unfinished_runs.is_empty());
            assert!(precondition.refusal().unwrap().contains("System Plugins"));
            let before = library_bytes(&library);
            assert!(
                application
                    .uninstall_plugin_if_unchanged(&library, &precondition, &sessions)
                    .await
                    .is_err()
            );
            assert_eq!(library_bytes(&library), before);

            // An old or fabricated approval cannot bypass the fresh check under both leases.
            let mut stale = precondition.clone();
            stale.system = None;
            assert!(
                application
                    .uninstall_plugin_if_unchanged(&library, &stale, &sessions)
                    .await
                    .is_err()
            );
            assert!(directory.exists());
        }
    }

    #[tokio::test]
    async fn a_session_mounting_the_plugin_refuses_the_uninstall_and_moves_no_bytes() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let sessions = session_store(root);
        let (application, _) = installed(root, &sessions, b"precious").await;
        sessions
            .set_plugin_mount("local", "sess", "demo", true, "operator", "click")
            .unwrap();
        let precondition = application
            .plugin_uninstall_precondition(&root.join("library"), "demo", &sessions)
            .await
            .unwrap();
        let refusal = precondition.refusal().expect("a mounted Plugin refuses");
        assert!(refusal.contains("sess"), "{refusal}");
        assert!(refusal.contains("先在所有会话里卸载它"), "{refusal}");

        let before = library_bytes(&root.join("library"));
        let error = application
            .uninstall_plugin_if_unchanged(&root.join("library"), &precondition, &sessions)
            .await
            .unwrap_err();
        let message = format!("{error:?}");
        assert!(message.contains("先在所有会话里卸载它"), "{message}");
        assert_eq!(library_bytes(&root.join("library")), before);
        assert_eq!(
            fs::read(root.join("library/plugins/demo/state/data.bin")).unwrap(),
            b"precious"
        );
    }

    #[tokio::test]
    async fn an_unfinished_run_binding_the_plugin_refuses_the_uninstall() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let sessions = session_store(root);
        let (application, _) = installed(root, &sessions, b"data").await;
        let snapshot = GraphSnapshot::admit(json!({
            "objective":"uninstall","entry":"work",
            "agents":{"worker":{"model":"fixture"}},
            "nodes":[{"id":"work","agent":"worker","plugins":["demo"]}],
            "edges":[]
        }))
        .unwrap();
        let running =
            GraphRunRecord::create_with_id(snapshot.clone(), json!({}), "running").unwrap();
        fixtures::create(&application, &running);
        let precondition = application
            .plugin_uninstall_precondition(&root.join("library"), "demo", &sessions)
            .await
            .unwrap();
        assert_eq!(precondition.unfinished_runs, ["running"]);
        let refusal = precondition.refusal().expect("an unfinished Run refuses");
        assert!(refusal.contains("running"), "{refusal}");
        assert!(refusal.contains("未结束的 Run"), "{refusal}");

        // A terminal Run is history, not a holder.
        let finished = GraphRunRecord::create_with_id(snapshot, json!({}), "finished").unwrap();
        fixtures::create(&application, &finished);
        fixtures::finish(&application, "finished");
        let precondition = application
            .plugin_uninstall_precondition(&root.join("library"), "demo", &sessions)
            .await
            .unwrap();
        assert_eq!(precondition.unfinished_runs, ["running"]);
    }

    #[tokio::test]
    async fn a_confirmed_uninstall_removes_the_plugin_its_view_and_frees_its_store_version() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let sessions = session_store(root);
        let library_root = root.join("library");
        let (application, precondition) = installed(root, &sessions, b"precious").await;
        // One other Plugin stays installed: the reclaimer conservatively refuses to
        // delete anything when the Library holds no Plugin at all, and that guard
        // is the reclaimer's, not something an uninstall may bypass.
        let other = root.join("candidate-other");
        fs::create_dir_all(&other).unwrap();
        fs::write(
            other.join("plugin.json"),
            json!({"name":"Other"}).to_string(),
        )
        .unwrap();
        let keep = application
            .plugin_install_precondition(library_root.clone(), other, "other".into())
            .await
            .unwrap();
        application
            .install_plugin_if_unchanged(library_root.clone(), keep)
            .await
            .unwrap();

        // Built after every install: an install already sweeps, and a view or a
        // store entry created before it would be reclaimed as an orphan.
        let resolved = resolved_library_root(&library_root);
        let view = plugin_view_path(
            &resolved,
            "demo",
            &Catalog::new(&library_root)
                .definition("demo")
                .unwrap()
                .dependencies
                .declaration_digest(),
        );
        fs::create_dir_all(view.join("bin")).unwrap();
        fs::write(view.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        let store = store_root(&resolved);
        let orphan = store.join("idna/3.6");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("idna.py"), "IDNA = 1\n").unwrap();
        // One package version a surviving view still points at: the sweep must keep
        // it, or the assertion below would pass for the wrong reason.
        let keeper = store.join("requests/2.32");
        fs::create_dir_all(&keeper).unwrap();
        fs::write(keeper.join("requests.py"), "REQUESTS = 1\n").unwrap();
        let keeper_view = views_root(&resolved).join("keeper/aaaa");
        fs::create_dir_all(keeper_view.join("lib/python3.12/site-packages")).unwrap();
        fs::write(keeper_view.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        std::os::unix::fs::symlink(
            &keeper,
            keeper_view.join("lib/python3.12/site-packages/requests"),
        )
        .unwrap();

        let outcome = application
            .uninstall_plugin_if_unchanged(&library_root, &precondition, &sessions)
            .await
            .unwrap();
        assert!(!library_root.join("plugins/demo").exists());
        assert!(library_root.join("plugins/other").is_dir());
        assert!(!view.exists(), "the Plugin's view goes with it");
        assert!(
            !orphan.exists(),
            "the freed package version must be reclaimed: {outcome:?}"
        );
        assert!(keeper.is_dir(), "a referenced package version must be kept");
        assert!(outcome.reclamation.is_none(), "{outcome:?}");
        assert!(
            outcome
                .reclaimed_package_versions
                .iter()
                .any(|version| version == "idna/3.6"),
            "{outcome:?}"
        );
        assert!(outcome.removed_bytes > 0);
        assert_eq!(outcome.removed_views.len(), 1, "{outcome:?}");
    }

    #[tokio::test]
    async fn a_second_uninstall_is_a_named_refusal() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let sessions = session_store(root);
        let (application, precondition) = installed(root, &sessions, b"data").await;
        application
            .uninstall_plugin_if_unchanged(&root.join("library"), &precondition, &sessions)
            .await
            .unwrap();
        let error = application
            .plugin_uninstall_precondition(&root.join("library"), "demo", &sessions)
            .await
            .unwrap_err();
        let message = format!("{error:?}");
        assert!(message.contains("not installed"), "{message}");
    }

    #[tokio::test]
    async fn the_precondition_lists_the_declared_mutable_paths_with_their_bytes() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let sessions = session_store(root);
        let (_runs, precondition) = installed(root, &sessions, b"0123456789").await;
        assert_eq!(precondition.id, "demo");
        assert_eq!(precondition.name, "Demo");
        assert_eq!(precondition.mutable, vec![("state/".to_owned(), Some(10))]);
    }
}
