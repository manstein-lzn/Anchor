//! Stable mutable scenes and their Host-authorized resource bindings.
//!
//! Execution owns the file lease until writers have stopped and publication or
//! interruption is durable. Dropping a lease alone never certifies a handover.
use crate::host_fs::{self, validate_component, validate_key};
use anchor_file_lease::{FileLease, FlockOperation};
use anchor_runtime::graph::{GraphError, InvocationKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(crate) struct Workspaces {
    root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceBinding {
    pub(crate) format: u32,
    pub(crate) run_id: String,
    pub(crate) graph_digest: String,
    pub(crate) node_id: String,
    pub(crate) resource_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceResource {
    pub(crate) format: u32,
    pub(crate) resource_id: String,
    /// Default scenes follow their Run. Explicitly shared scenes are released
    /// separately and remain available after either bound Run is deleted.
    pub(crate) owner_run: Option<String>,
}

type BindingExclusion = (WorkspaceResource, FileLease, Option<(PathBuf, FileLease)>);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Release {
    Published,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    format: u32,
    key: InvocationKey,
    release: Option<Release>,
}

/// A missing release keeps the candidate reserved across Host restarts.
pub(crate) struct WorkspaceExecutionLease {
    owner_path: PathBuf,
    owner: Owner,
    _lease: FileLease,
}

impl WorkspaceExecutionLease {
    pub(crate) fn finish_published(mut self) -> Result<(), GraphError> {
        self.finish(Release::Published)
    }

    pub(crate) fn finish_interrupted(mut self) -> Result<(), GraphError> {
        self.finish(Release::Interrupted)
    }

    fn finish(&mut self, release: Release) -> Result<(), GraphError> {
        self.owner.release = Some(release);
        save(&self.owner_path, &self.owner)
    }
}

impl Workspaces {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn default_resource_id(key: &InvocationKey) -> Result<String, GraphError> {
        validate_key(key)?;
        Ok(default_resource(key))
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn path(&self, key: &InvocationKey) -> Result<PathBuf, GraphError> {
        let resource = self.resource(key)?;
        Ok(self.resource_directory(&resource)?.join("files"))
    }

    pub(crate) fn staging_directory(&self, key: &InvocationKey) -> Result<PathBuf, GraphError> {
        let resource = self.resource(key)?;
        Ok(self.resource_directory(&resource)?.join("staging"))
    }

    pub(crate) fn resource(&self, key: &InvocationKey) -> Result<String, GraphError> {
        validate_key(key)?;
        Ok(self
            .binding(key)?
            .map(|binding| binding.resource_id)
            .unwrap_or_else(|| default_resource(key)))
    }

    pub(crate) fn is_bound(&self, key: &InvocationKey) -> Result<bool, GraphError> {
        validate_key(key)?;
        Ok(self.binding(key)?.is_some())
    }

    pub(crate) fn resource_path(&self, resource: &str) -> Result<PathBuf, GraphError> {
        let directory = self.resource_directory(resource)?;
        self.read_resource(resource)?;
        Ok(directory.join("files"))
    }

    /// Preparation creates no execution ownership and never reads scene files.
    pub(crate) fn prepare(&self, key: &InvocationKey) -> Result<PathBuf, GraphError> {
        validate_key(key)?;
        let _registry = self.registry_lease()?;
        self.prepare_locked(key)
    }

    fn prepare_locked(&self, key: &InvocationKey) -> Result<PathBuf, GraphError> {
        let binding = match self.binding(key)? {
            Some(binding) => binding,
            None => {
                let binding = WorkspaceBinding {
                    format: 2,
                    run_id: key.run_id.clone(),
                    graph_digest: key.graph_digest.clone(),
                    node_id: key.node_id.clone(),
                    resource_id: default_resource(key),
                };
                let directory = self.resource_directory(&binding.resource_id)?;
                directory_private(&directory)?;
                let resource = WorkspaceResource {
                    format: 1,
                    resource_id: binding.resource_id.clone(),
                    owner_run: Some(key.run_id.clone()),
                };
                match read::<WorkspaceResource>(&directory.join("resource.json"))? {
                    Some(saved) if saved != resource => {
                        return Err(corrupt("workspace resource ownership changed"));
                    }
                    Some(_) => (),
                    None => save(&directory.join("resource.json"), &resource)?,
                }
                let state = self.binding_path(key);
                directory_private(state.parent().unwrap())?;
                save(&state, &binding)?;
                binding
            }
        };
        self.read_resource(&binding.resource_id)?;
        let workspace = self.resource_directory(&binding.resource_id)?.join("files");
        if !workspace.try_exists()? && read_owner(workspace.parent().unwrap())?.is_some() {
            return Err(corrupt("initialized workspace scene is missing"));
        }
        directory_private(&workspace)?;
        Ok(workspace)
    }

    /// The caller first stops any old local subprocesses for a same-key retry.
    /// Another identity requires a durable release even when no process holds
    /// the advisory lease, so an unpublished candidate cannot be overwritten.
    pub(crate) fn acquire(
        &self,
        key: &InvocationKey,
    ) -> Result<WorkspaceExecutionLease, GraphError> {
        validate_key(key)?;
        let _registry = self.registry_lease()?;
        self.prepare_locked(key)?;
        let directory = self.resource_directory(&self.resource(key)?)?;
        let lease = lease(&directory.join("execution.lock"))?;
        let owner_path = directory.join("owner.json");
        if let Some(previous) = read_owner(&directory)? {
            if previous.key != *key && previous.release.is_none() {
                return Err(GraphError::RunConflict);
            }
            if previous.key.run_id == key.run_id && previous.key.invocation > key.invocation {
                return Err(GraphError::RunConflict);
            }
        }
        let owner = Owner {
            format: 1,
            key: key.clone(),
            release: None,
        };
        save(&owner_path, &owner)?;
        Ok(WorkspaceExecutionLease {
            owner_path,
            owner,
            _lease: lease,
        })
    }

    pub(crate) fn initialized_owner(
        &self,
        key: &InvocationKey,
    ) -> Result<Option<InvocationKey>, GraphError> {
        validate_key(key)?;
        let Some(binding) = self.binding(key)? else {
            return Ok(None);
        };
        self.read_resource(&binding.resource_id)?;
        let directory = self.resource_directory(&binding.resource_id)?;
        let owner = read_owner(&directory)?;
        if owner.is_some() && !directory.join("files").is_dir() {
            return Err(corrupt("initialized workspace scene is missing"));
        }
        Ok(owner.map(|owner| owner.key))
    }

    /// Bind a Host-authorized resource id. No Graph-supplied host path enters
    /// this service. Sharing promotes the scene out of per-Run deletion.
    pub(crate) fn bind_resource(
        &self,
        key: &InvocationKey,
        resource: &str,
    ) -> Result<(), GraphError> {
        validate_key(key)?;
        let _registry = self.registry_lease()?;
        if self
            .binding(key)?
            .is_some_and(|binding| binding.resource_id == resource)
        {
            return Ok(());
        }
        let exclusion = self.binding_exclusion(key, resource)?;
        self.publish_shared(exclusion.0)?;
        let binding = WorkspaceBinding {
            format: 2,
            run_id: key.run_id.clone(),
            graph_digest: key.graph_digest.clone(),
            node_id: key.node_id.clone(),
            resource_id: resource.into(),
        };
        let path = self.binding_path(key);
        directory_private(path.parent().unwrap())?;
        save(&path, &binding)?;
        if let Some((old, _lease)) = exclusion.2 {
            fs::remove_dir_all(old)?;
        }
        Ok(())
    }

    pub(crate) fn share_resource(&self, resource: &str) -> Result<(), GraphError> {
        let _registry = self.registry_lease()?;
        self.publish_shared(self.read_resource(resource)?)
    }

    fn publish_shared(&self, mut resource: WorkspaceResource) -> Result<(), GraphError> {
        if resource.owner_run.take().is_some() {
            save(
                &self
                    .resource_directory(&resource.resource_id)?
                    .join("resource.json"),
                &resource,
            )?;
        }
        Ok(())
    }

    pub(crate) fn check_bind_resource(
        &self,
        key: &InvocationKey,
        resource: &str,
    ) -> Result<(), GraphError> {
        validate_key(key)?;
        let _registry = self.registry_lease()?;
        if self
            .binding(key)?
            .is_some_and(|binding| binding.resource_id == resource)
        {
            return Ok(());
        }
        self.binding_exclusion(key, resource)?;
        Ok(())
    }

    fn binding_exclusion(
        &self,
        key: &InvocationKey,
        resource: &str,
    ) -> Result<BindingExclusion, GraphError> {
        let saved = self.read_resource(resource)?;
        let directory = self.resource_directory(resource)?;
        let source = lease(&directory.join("execution.lock"))?;
        require_released(&directory)?;
        let previous = if let Some(existing) = self.binding(key)? {
            let old = self.resource_directory(&existing.resource_id)?;
            let target = lease(&old.join("execution.lock"))?;
            if read_owner(&old)?.is_some() || fs::read_dir(old.join("files"))?.next().is_some() {
                return Err(GraphError::RunConflict);
            }
            Some((old, target))
        } else {
            None
        };
        Ok((saved, source, previous))
    }

    pub(crate) fn release_run(&self, run: &str) -> Result<(), GraphError> {
        validate_component(run)?;
        let _registry = self.registry_lease()?;
        let bindings = self.bindings_for_run(run)?;
        let mut leases = Vec::new();
        let resources = bindings
            .iter()
            .map(|binding| &binding.resource_id)
            .collect::<BTreeSet<_>>();
        for id in resources {
            let directory = self.resource_directory(id)?;
            if !directory.try_exists()? {
                continue;
            }
            let resource = self.read_resource(id)?;
            if resource.owner_run.as_deref() == Some(run) {
                let lease = lease(&directory.join("execution.lock"))?;
                require_released(&directory)?;
                leases.push((directory, lease));
            }
        }
        // Keep bindings until every relevant resource has passed exclusion.
        for (directory, _lease) in &leases {
            fs::remove_dir_all(directory)?;
        }
        let state = self.root.join(run).join(".node-workspaces");
        match fs::remove_dir_all(state) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub(crate) fn release_resource(&self, resource: &str) -> Result<(), GraphError> {
        let _registry = self.registry_lease()?;
        let directory = self.resource_directory(resource)?;
        if !directory.try_exists()? {
            return Ok(());
        }
        self.require_unbound(resource)?;
        let _execution = lease(&directory.join("execution.lock"))?;
        require_released(&directory)?;
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    pub(crate) fn check_release_resource(&self, resource: &str) -> Result<(), GraphError> {
        let _registry = self.registry_lease()?;
        self.require_unbound(resource)?;
        let directory = self.resource_directory(resource)?;
        let _execution = lease(&directory.join("execution.lock"))?;
        require_released(&directory)
    }

    fn require_unbound(&self, resource: &str) -> Result<(), GraphError> {
        self.read_resource(resource)?;
        if self
            .bindings()?
            .iter()
            .any(|binding| binding.resource_id == resource)
        {
            return Err(GraphError::RunConflict);
        }
        Ok(())
    }

    /// Metadata-only inventory for deletion and storage accounting.
    pub(crate) fn bindings(&self) -> Result<Vec<WorkspaceBinding>, GraphError> {
        let mut bindings = Vec::new();
        for entry in entries(&self.root)? {
            let run = entry.file_name().to_string_lossy().into_owned();
            if anchor_runtime::identifier::is_name(&run)
                && !matches!(
                    run.as_str(),
                    ".resources" | ".resources.lock" | ".goose-process"
                )
            {
                bindings.extend(self.bindings_for_run(&run)?);
            }
        }
        Ok(bindings)
    }

    pub(crate) fn resources(&self) -> Result<Vec<WorkspaceResource>, GraphError> {
        entries(&self.root.join(".resources"))?
            .into_iter()
            .map(|entry| self.read_resource(&entry.file_name().to_string_lossy()))
            .collect()
    }

    fn bindings_for_run(&self, run: &str) -> Result<Vec<WorkspaceBinding>, GraphError> {
        entries(&self.root.join(run).join(".node-workspaces"))?
            .into_iter()
            .map(|entry| {
                let path = entry.path().join("binding.json");
                let binding: WorkspaceBinding =
                    read(&path)?.ok_or_else(|| corrupt("workspace binding is missing"))?;
                let key = InvocationKey {
                    run_id: run.into(),
                    graph_digest: binding.graph_digest.clone(),
                    node_id: binding.node_id.clone(),
                    invocation: 1,
                };
                if self.binding_path(&key) != path {
                    return Err(corrupt("workspace binding path changed"));
                }
                self.binding(&key)?
                    .ok_or_else(|| corrupt("workspace binding is missing"))
            })
            .collect()
    }

    fn binding(&self, key: &InvocationKey) -> Result<Option<WorkspaceBinding>, GraphError> {
        let binding: Option<WorkspaceBinding> = read(&self.binding_path(key))?;
        if let Some(binding) = &binding {
            if binding.format != 2
                || binding.run_id != key.run_id
                || binding.graph_digest != key.graph_digest
                || binding.node_id != key.node_id
            {
                return Err(corrupt(
                    "workspace binding identity changed; migration required",
                ));
            }
            validate_resource(&binding.resource_id)?;
        }
        Ok(binding)
    }

    fn binding_path(&self, key: &InvocationKey) -> PathBuf {
        self.root
            .join(&key.run_id)
            .join(".node-workspaces")
            .join(format!("{:x}", Sha256::digest(key.node_id.as_bytes())))
            .join("binding.json")
    }

    fn resource_directory(&self, resource: &str) -> Result<PathBuf, GraphError> {
        validate_resource(resource)?;
        Ok(self.root.join(".resources").join(resource))
    }

    fn read_resource(&self, resource: &str) -> Result<WorkspaceResource, GraphError> {
        let path = self.resource_directory(resource)?.join("resource.json");
        let saved: WorkspaceResource =
            read(&path)?.ok_or_else(|| corrupt("workspace resource is missing"))?;
        if saved.format != 1 || saved.resource_id != resource {
            return Err(corrupt("workspace resource identity changed"));
        }
        if let Some(owner) = &saved.owner_run {
            validate_component(owner)?;
        }
        Ok(saved)
    }

    fn registry_lease(&self) -> Result<FileLease, GraphError> {
        directory_private(&self.root)?;
        FileLease::acquire(
            open_lease(&self.root.join(".resources.lock"))?,
            FlockOperation::LockExclusive,
        )
        .map_err(|error| std::io::Error::from(error).into())
    }
}

fn default_resource(key: &InvocationKey) -> String {
    let identity = format!("{}\0{}\0{}", key.run_id, key.graph_digest, key.node_id);
    format!("wr1-{:x}", Sha256::digest(identity.as_bytes()))
}

fn validate_resource(resource: &str) -> Result<(), GraphError> {
    if !resource.strip_prefix("wr1-").is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err(corrupt("invalid workspace resource identity"));
    }
    Ok(())
}

fn require_released(directory: &Path) -> Result<(), GraphError> {
    if read_owner(directory)?.is_some_and(|owner| owner.release.is_none()) {
        return Err(GraphError::RunConflict);
    }
    Ok(())
}

fn directory_private(path: &Path) -> Result<(), GraphError> {
    host_fs::directory(path).map_err(corrupt)
}

fn lease(path: &Path) -> Result<FileLease, GraphError> {
    let file = open_lease(path)?;
    FileLease::acquire(file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == rustix::io::Errno::WOULDBLOCK {
            GraphError::RunConflict
        } else {
            std::io::Error::from(error).into()
        }
    })
}

fn open_lease(path: &Path) -> Result<fs::File, GraphError> {
    Ok(OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?)
}

fn read_owner(directory: &Path) -> Result<Option<Owner>, GraphError> {
    let owner: Option<Owner> = read(&directory.join("owner.json"))?;
    if let Some(owner) = &owner {
        validate_key(&owner.key)?;
        if owner.format != 1 {
            return Err(corrupt("unsupported workspace owner format"));
        }
    }
    Ok(owner)
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, GraphError> {
    let Some(parent) = path.parent() else {
        return Err(corrupt("workspace fact has no parent"));
    };
    if !parent.try_exists()? {
        return Ok(None);
    }
    let relative = path.file_name().unwrap().to_string_lossy();
    let mut file = match crate::resource_read::open_resource(parent, &relative) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    serde_json::from_reader(&mut file)
        .map(Some)
        .map_err(|error| corrupt(error.to_string()))
}

fn save(path: &Path, value: &impl Serialize) -> Result<(), GraphError> {
    let bytes = serde_json::to_vec(value).map_err(|error| corrupt(error.to_string()))?;
    crate::write_durable(path, &bytes)?;
    Ok(())
}

fn entries(path: &Path) -> Result<Vec<fs::DirEntry>, GraphError> {
    match fs::read_dir(path) {
        Ok(entries) => entries.collect::<Result<_, _>>().map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn corrupt(message: impl Into<String>) -> GraphError {
    GraphError::CorruptRun(message.into())
}

#[cfg(test)]
#[path = "artifacts/workspace_binding_tests.rs"]
mod tests;
