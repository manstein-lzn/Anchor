//! Run admission and control. Graph routing and node scheduling belong to the Kernel scheduler.
pub(crate) mod abandon;
mod admission;
mod assistants;
mod conversations;
mod deletion;
mod dispatch;
#[cfg(test)]
mod liveness;
mod queries;
mod recovery;
pub(crate) use assistants::AssistantAdmission;
pub(crate) mod channel_management;
pub(crate) mod development_dependencies;
pub(crate) mod development_environment;
pub(crate) mod graphs;
pub(crate) mod oauth_management;
pub(crate) mod pilot_sessions;
pub(crate) mod plugin_candidates;
pub(crate) mod plugins;
pub(crate) mod run_access;
pub(crate) mod run_projection;
pub(crate) mod scheduling;
pub(crate) mod session_calls;
use crate::run_metadata::{self as metadata, ConversationSource, RunMetadata, RunTrigger};
pub(crate) use conversations::{WECOM_REPLY_KEY_PREFIX, WECOM_REPLY_KIND};

use crate::{HostControl, create_durable_directory};
#[cfg(test)]
use anchor_runtime::graph::GraphRunRecord;
use anchor_runtime::graph::{GraphError, RunLease};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::{Mutex, OwnedMutexGuard};

#[derive(Debug)]
pub(crate) enum ApplicationError {
    Missing,
    Conflict(String),
    Invalid(String),
    Storage(String),
}

pub(crate) struct AdmissionOptions {
    pub(crate) objective: Option<String>,
    pub(crate) trigger: RunTrigger,
    pub(crate) pilot: Option<metadata::PilotRunSource>,
    pub(crate) oauth_owner: Option<String>,
    pub(crate) workspace_bindings: BTreeMap<String, WorkspaceAdmissionBinding>,
    pub(crate) workspace_root: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceAdmissionBinding {
    pub(crate) run: String,
    pub(crate) node: String,
}

impl From<crate::run_metadata::MetadataError> for ApplicationError {
    fn from(error: crate::run_metadata::MetadataError) -> Self {
        use crate::run_metadata::MetadataError;
        match error {
            MetadataError::Conflict(message) => Self::Conflict(message),
            MetadataError::Invalid(message) => Self::Invalid(message),
            MetadataError::Storage(message) => Self::Storage(message),
        }
    }
}

impl From<GraphError> for ApplicationError {
    fn from(error: GraphError) -> Self {
        match error {
            GraphError::RunBusy(_) | GraphError::RunConflict => Self::Conflict(error.to_string()),
            _ => Self::Storage(error.to_string()),
        }
    }
}

impl From<crate::plugin_library::PublicationError> for ApplicationError {
    fn from(error: crate::plugin_library::PublicationError) -> Self {
        match error {
            crate::plugin_library::PublicationError::Invalid(reason) => Self::Invalid(reason),
            crate::plugin_library::PublicationError::Storage(reason) => Self::Storage(reason),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RunApplication {
    pub(crate) data_root: PathBuf,
    pub(crate) library_root_override: Option<PathBuf>,
    pub(crate) bwrap: PathBuf,
    catalog_root: PathBuf,
    configured_graph: Option<(String, PathBuf)>,
    admission: Arc<Mutex<()>>,
    catalog_mutations: Arc<Mutex<()>>,
    /// The deployment's authorized dependency preparation, when it configured
    /// any. Absent means "this deployment cannot fetch or build dependencies", which is
    /// a valid deployment, not an error.
    dependency_provision: Option<Arc<crate::dependency_provision::DependencyProvisioner>>,
    runtime_configuration: Option<Arc<crate::runtime_configuration::RuntimeConfiguration>>,
    /// A Pilot Turn keeps the same interpreter and preparation grant for all
    /// of its tools, including an explicitly unavailable provisioner. The
    /// shared configuration remains available for preparing the next Turn.
    frozen_runtime_snapshot: Option<crate::runtime_configuration::RuntimeSnapshot>,
    pub(super) schedules: Option<Arc<dyn scheduling::ScheduleManagementPort>>,
    pub(super) oauth: Option<Arc<dyn oauth_management::OAuthManagementPort>>,
    pub(super) channels: Option<Arc<dyn channel_management::ChannelManagementPort>>,
    executor: Option<tokio::runtime::Handle>,
    kernel_run_owner: Arc<std::sync::Mutex<Option<Arc<run_access::kernel_runs::KernelRuns>>>>,
}

impl RunApplication {
    /// Wire the deployment's authorized dependency preparation.
    #[cfg(test)]
    pub(crate) fn with_dependency_provisioner(
        mut self,
        provisioner: std::sync::Arc<crate::dependency_provision::DependencyProvisioner>,
    ) -> Self {
        self.dependency_provision = Some(provisioner);
        self
    }

    /// The deployment's authorized dependency preparation, when it configured any.
    ///
    /// Absent means "this deployment does not prepare dependencies",
    /// which is a valid deployment, not an error.
    pub(crate) fn dependency_provisioner(
        &self,
    ) -> Result<std::sync::Arc<crate::dependency_provision::DependencyProvisioner>, ApplicationError>
    {
        let provisioner = match self.runtime_snapshot()? {
            Some(snapshot) => snapshot.provisioner,
            None => self.dependency_provision.clone(),
        };
        provisioner.ok_or_else(|| {
            ApplicationError::Invalid(
                "this deployment does not authorize dependency preparation".to_owned(),
            )
        })
    }

    pub(crate) fn with_frozen_runtime_snapshot(
        mut self,
        snapshot: crate::runtime_configuration::RuntimeSnapshot,
    ) -> Self {
        self.frozen_runtime_snapshot = Some(snapshot);
        self
    }

    pub(crate) fn runtime_snapshot(
        &self,
    ) -> Result<Option<crate::runtime_configuration::RuntimeSnapshot>, ApplicationError> {
        if let Some(snapshot) = &self.frozen_runtime_snapshot {
            return Ok(Some(snapshot.clone()));
        }
        self.runtime_configuration
            .as_ref()
            .map(|configuration| configuration.snapshot())
            .transpose()
            .map_err(ApplicationError::Storage)
    }

    pub(crate) fn with_runtime_configuration(
        mut self,
        configuration: Arc<crate::runtime_configuration::RuntimeConfiguration>,
    ) -> Self {
        self.runtime_configuration = Some(configuration);
        self
    }

    pub(crate) fn runtime_configuration(
        &self,
    ) -> Option<&Arc<crate::runtime_configuration::RuntimeConfiguration>> {
        self.runtime_configuration.as_ref()
    }

    pub(crate) fn with_schedule_management(
        mut self,
        port: Arc<dyn scheduling::ScheduleManagementPort>,
    ) -> Self {
        self.schedules = Some(port);
        self
    }

    pub(crate) fn with_oauth_management(
        mut self,
        port: Arc<dyn oauth_management::OAuthManagementPort>,
    ) -> Self {
        self.oauth = Some(port);
        self
    }
    pub(crate) fn with_channel_management(
        mut self,
        port: Arc<dyn channel_management::ChannelManagementPort>,
    ) -> Self {
        self.channels = Some(port);
        self
    }

    /// Safe, read-only facts about the deployment's Python dependency
    /// preparation grant and shared interpreter. Never returns configured
    /// paths, URLs, or uv digest; those are deployment details, not model inputs.
    pub(crate) fn pilot_dependency_status(&self) -> Value {
        use std::os::unix::fs::PermissionsExt;
        let snapshot = match self.runtime_snapshot() {
            Ok(snapshot) => snapshot,
            Err(reason) => {
                return serde_json::json!({"configured":false,"configuration_error":format!("{reason:?}")});
            }
        };
        let provisioner = match &snapshot {
            Some(snapshot) => snapshot.provisioner.clone(),
            None => self.dependency_provision.clone(),
        };
        let provisioner = provisioner.as_ref();
        let interpreter = provisioner
            .map(|provisioner| provisioner.config.python.clone())
            .or_else(|| {
                snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.python.clone())
            })
            .or_else(|| {
                self.runtime_configuration
                    .is_none()
                    .then(|| {
                        std::env::var_os(crate::dependency_provision::PYTHON_INTERPRETER_ENV)
                            .filter(|value| !value.is_empty())
                            .map(PathBuf::from)
                    })
                    .flatten()
            });
        let interpreter_configured = interpreter.is_some();
        let interpreter_available = interpreter.as_ref().is_some_and(|path| {
            path.is_absolute()
                && std::fs::metadata(path).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
        });
        let (uv_available, mut sources) = if let Some(provisioner) = provisioner {
            let uv_available = provisioner.config.verified_uv().is_ok();
            let sources = provisioner
                .config
                .sources
                .iter()
                .map(|source| match source {
                    crate::dependency_provision::PackageSource::Directory(path) => {
                        serde_json::json!({"type":"directory","available":path.is_dir()})
                    }
                    crate::dependency_provision::PackageSource::Index(_) => {
                        // Reachability is intentionally not probed by this read-only
                        // status call: it is network activity and a transient probe
                        // would not be an authorization fact.
                        serde_json::json!({"type":"index","configured":true,"reachability_checked":false})
                    }
                })
                .collect::<Vec<_>>();
            (uv_available, sources)
        } else {
            (false, Vec::new())
        };
        sources.sort_by(|left, right| left["type"].as_str().cmp(&right["type"].as_str()));
        serde_json::json!({
            "configured": provisioner.is_some(),
            "shared_interpreter_configured": interpreter_configured,
            "shared_interpreter_available": interpreter_available,
            "uv_available": uv_available,
            "uv_digest_verified": uv_available,
            "sources": sources,
        })
    }

    pub(crate) fn new(data_root: PathBuf, catalog_root: PathBuf) -> Self {
        Self {
            library_root_override: None,
            bwrap: "bwrap".into(),
            data_root,
            catalog_root,
            configured_graph: None,
            admission: Arc::new(Mutex::new(())),
            catalog_mutations: Arc::new(Mutex::new(())),
            dependency_provision: None,
            runtime_configuration: None,
            frozen_runtime_snapshot: None,
            schedules: None,
            oauth: None,
            channels: None,
            executor: tokio::runtime::Handle::try_current().ok(),
            kernel_run_owner: Arc::default(),
        }
    }

    pub(crate) fn with_host_configuration(
        mut self,
        configuration: Arc<crate::HostConfiguration>,
    ) -> Self {
        self.library_root_override = configuration.library_root.clone();
        self.bwrap = configuration.bwrap.clone();
        self
    }

    pub(crate) fn library_root(&self) -> &Path {
        self.library_root_override
            .as_deref()
            .unwrap_or(&self.catalog_root)
    }

    /// Serialize Graph definition mutations with reference checks and deletion.
    /// Run admission uses each Graph's own lease and the Library publication
    /// lease for its Plugin references; it must not acquire this catalog guard.
    pub(crate) async fn graph_catalog_mutation_guard(&self) -> OwnedMutexGuard<()> {
        Arc::clone(&self.catalog_mutations).lock_owned().await
    }

    pub(crate) fn with_configured_graph(mut self, name: String, bundle_root: PathBuf) -> Self {
        self.configured_graph = Some((name, bundle_root));
        self
    }

    pub(crate) fn graph_bundle_path(&self, name: &str) -> PathBuf {
        self.configured_graph
            .as_ref()
            .filter(|(configured_name, _)| configured_name == name)
            .map(|(_, path)| path.clone())
            .unwrap_or_else(|| self.catalog_root.join(name))
    }

    fn graph_lease(&self, graph_path: &Path) -> Result<Box<dyn RunLease>, ApplicationError> {
        Ok(crate::assistant_plugins::graph_lease(
            &self.data_root,
            graph_path,
        )?)
    }

    fn check_store(&self) -> Result<(), ApplicationError> {
        self.executor()?;
        // The host uses deployment environment, never HTTP-supplied paths.
        self.data_root
            .canonicalize()
            .or_else(|_| {
                create_durable_directory(&self.data_root)?;
                self.data_root.canonicalize()
            })
            .map_err(storage)?;
        Ok(())
    }
}

fn new_control() -> HostControl {
    HostControl {
        cancellation: Arc::new(AtomicBool::new(false)),
    }
}

fn storage(error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::Storage(error.to_string())
}

#[cfg(test)]
mod tests;
