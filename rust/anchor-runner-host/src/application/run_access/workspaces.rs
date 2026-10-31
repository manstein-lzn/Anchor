//! Workspace grants are Kernel facts; filesystem bindings are their projection.
use crate::{
    application::{ApplicationError, RunApplication, WorkspaceAdmissionBinding},
    workspaces::{WorkspaceBinding, WorkspaceResource, Workspaces},
};
use anchor_runtime::graph::{GraphRunRecord, InvocationKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingFact {
    binding: WorkspaceBinding,
    released: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceFact {
    resource: WorkspaceResource,
    released: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceBindRequest {
    pub(crate) source_run: String,
    pub(crate) source_node: String,
    pub(crate) target_run: String,
    pub(crate) target_node: String,
}

impl RunApplication {
    /// Called by the upper execution adapter after Run and Session admission.
    pub(crate) fn ensure_node_workspace_binding(
        &self,
        key: &InvocationKey,
        workspaces: &Workspaces,
    ) -> Result<Value, ApplicationError> {
        let record = self
            .execution_record(&key.run_id)?
            .ok_or(ApplicationError::Missing)?;
        require_node(&record, key)?;
        let id = binding_id(key);
        if let Some(saved) = self.file_fact(&id)? {
            let fact: BindingFact = decode(saved)?;
            require_binding(&fact, key)?;
            self.project_workspace_binding(key, &fact.binding, workspaces)?;
            return encode(fact);
        }
        let source = crate::assistant::handover_workspace_source(
            &self.data_root,
            key,
            &crate::HostArtifacts::new(self.data_root.join("artifacts"), workspaces.root().into()),
        )
        .map_err(ApplicationError::Invalid)?;
        let resource_id = if let Some(source) = source {
            let source_fact = self.file_fact(&binding_id(&source))?.ok_or_else(|| {
                ApplicationError::Conflict("workspace predecessor has no Kernel binding".into())
            })?;
            let source_fact: BindingFact = decode(source_fact)?;
            require_binding(&source_fact, &source)?;
            workspaces.check_bind_resource(key, &source_fact.binding.resource_id)?;
            self.promote_workspace_resource(&source_fact.binding.resource_id, workspaces)?;
            source_fact.binding.resource_id
        } else {
            Workspaces::default_resource_id(key)?
        };
        let resource_id_key = resource_fact_id(&resource_id);
        if self.file_fact(&resource_id_key)?.is_none() {
            self.put_file_fact(
                &resource_id_key,
                encode(ResourceFact {
                    resource: WorkspaceResource {
                        format: 1,
                        resource_id: resource_id.clone(),
                        owner_run: Some(key.run_id.clone()),
                    },
                    released: false,
                })?,
            )?;
        }
        let fact = BindingFact {
            binding: WorkspaceBinding {
                format: 2,
                run_id: key.run_id.clone(),
                graph_digest: key.graph_digest.clone(),
                node_id: key.node_id.clone(),
                resource_id,
            },
            released: false,
        };
        let state = encode(&fact)?;
        self.put_file_fact(&id, state.clone())?;
        self.project_workspace_binding(key, &fact.binding, workspaces)?;
        Ok(state)
    }

    pub(crate) fn node_workspace_binding(
        &self,
        run: &str,
        node: &str,
    ) -> Result<Value, ApplicationError> {
        let key = self.workspace_node_key(run, node)?;
        let state = self
            .file_fact(&binding_id(&key))?
            .ok_or(ApplicationError::Missing)?;
        require_binding(&decode::<BindingFact>(state.clone())?, &key)?;
        Ok(state)
    }

    /// The HTTP installation identity may grant an existing resource to an
    /// inactive node. Graph content supplies neither resource ids nor paths.
    pub(crate) async fn bind_workspace_resource(
        &self,
        request: WorkspaceBindRequest,
        workspaces: &Workspaces,
    ) -> Result<Value, ApplicationError> {
        let _admission = self.admission.lock().await;
        let target = self.workspace_node_key(&request.target_run, &request.target_node)?;
        let view = self
            .read_run(&target.run_id)?
            .ok_or(ApplicationError::Missing)?;
        if view.active() {
            return Err(ApplicationError::Conflict("target Run is executing".into()));
        }
        self.bind_workspace_to_node(
            &view.record,
            &target,
            &WorkspaceAdmissionBinding {
                run: request.source_run,
                node: request.source_node,
            },
            workspaces,
        )
    }

    pub(crate) fn bind_admission_workspaces(
        &self,
        record: &GraphRunRecord,
        bindings: &std::collections::BTreeMap<String, WorkspaceAdmissionBinding>,
        workspaces: &Workspaces,
    ) -> Result<(), ApplicationError> {
        for (node, source) in bindings {
            let target = InvocationKey {
                run_id: record.run_id.clone(),
                graph_digest: record.graph_digest.clone(),
                node_id: node.clone(),
                invocation: 1,
            };
            self.bind_workspace_to_node(record, &target, source, workspaces)?;
        }
        Ok(())
    }

    fn bind_workspace_to_node(
        &self,
        target_record: &GraphRunRecord,
        target: &InvocationKey,
        source: &WorkspaceAdmissionBinding,
        workspaces: &Workspaces,
    ) -> Result<Value, ApplicationError> {
        require_node(target_record, target)?;
        let source = self.workspace_node_key(&source.run, &source.node)?;
        let source_fact = self.ensure_node_workspace_binding(&source, workspaces)?;
        let source_fact: BindingFact = decode(source_fact)?;
        // Exclusion is checked before submitting the new grant. The same
        // application gate prevents a target Run from being resumed here.
        workspaces.check_bind_resource(target, &source_fact.binding.resource_id)?;
        self.promote_workspace_resource(&source_fact.binding.resource_id, workspaces)?;
        let fact = BindingFact {
            binding: WorkspaceBinding {
                format: 2,
                run_id: target.run_id.clone(),
                graph_digest: target.graph_digest.clone(),
                node_id: target.node_id.clone(),
                resource_id: source_fact.binding.resource_id,
            },
            released: false,
        };
        let state = encode(&fact)?;
        self.put_file_fact(&binding_id(target), state.clone())?;
        self.project_workspace_binding(target, &fact.binding, workspaces)?;
        Ok(state)
    }

    pub(crate) fn release_run_workspace_facts(
        &self,
        run: &str,
        workspaces: &Workspaces,
    ) -> Result<(), ApplicationError> {
        let Some(record) = self.execution_record(run)? else {
            return Ok(());
        };
        for node in &record.snapshot.nodes {
            let key = InvocationKey {
                run_id: run.into(),
                graph_digest: record.graph_digest.clone(),
                node_id: node.id.clone(),
                invocation: 1,
            };
            let Some(state) = self.file_fact(&binding_id(&key))? else {
                continue;
            };
            let mut fact: BindingFact = decode(state)?;
            if !fact.released {
                fact.released = true;
                self.put_file_fact(&binding_id(&key), encode(&fact)?)?;
            }
            if let Some(state) = self.file_fact(&resource_fact_id(&fact.binding.resource_id))? {
                let mut resource: ResourceFact = decode(state)?;
                if resource.resource.owner_run.as_deref() == Some(run) {
                    resource.released = true;
                    self.put_file_fact(
                        &resource_fact_id(&fact.binding.resource_id),
                        encode(resource)?,
                    )?;
                } else if !resource.released {
                    workspaces.share_resource(&resource.resource.resource_id)?;
                }
            }
        }
        workspaces.release_run(run)?;
        Ok(())
    }

    pub(crate) fn release_workspace_resource(
        &self,
        resource: &str,
        workspaces: &Workspaces,
    ) -> Result<(), ApplicationError> {
        // Bindings and writer receipts remain exclusion evidence while the
        // released Kernel fact is published; deletion can retry afterward.
        let mut fact: ResourceFact = decode(
            self.file_fact(&resource_fact_id(resource))?
                .ok_or(ApplicationError::Missing)?,
        )?;
        if fact.released {
            workspaces.release_resource(resource)?;
            return Ok(());
        }
        workspaces.check_release_resource(resource)?;
        fact.released = true;
        self.put_file_fact(&resource_fact_id(resource), encode(fact)?)?;
        workspaces.release_resource(resource)?;
        Ok(())
    }

    fn workspace_node_key(&self, run: &str, node: &str) -> Result<InvocationKey, ApplicationError> {
        let record = self
            .execution_record(run)?
            .ok_or(ApplicationError::Missing)?;
        let key = InvocationKey {
            run_id: run.into(),
            graph_digest: record.graph_digest.clone(),
            node_id: node.into(),
            invocation: 1,
        };
        require_node(&record, &key)?;
        Ok(key)
    }

    fn promote_workspace_resource(
        &self,
        resource: &str,
        workspaces: &Workspaces,
    ) -> Result<(), ApplicationError> {
        let id = resource_fact_id(resource);
        let mut fact: ResourceFact =
            decode(self.file_fact(&id)?.ok_or(ApplicationError::Missing)?)?;
        if fact.released {
            return Err(ApplicationError::Conflict(
                "workspace resource was released".into(),
            ));
        }
        fact.resource.owner_run = None;
        self.put_file_fact(&id, encode(fact)?)?;
        workspaces.share_resource(resource)?;
        Ok(())
    }

    fn project_workspace_binding(
        &self,
        key: &InvocationKey,
        binding: &WorkspaceBinding,
        workspaces: &Workspaces,
    ) -> Result<(), ApplicationError> {
        let resource: ResourceFact = decode(
            self.file_fact(&resource_fact_id(&binding.resource_id))?
                .ok_or(ApplicationError::Missing)?,
        )?;
        if resource.released {
            return Err(ApplicationError::Conflict(
                "workspace resource was released".into(),
            ));
        }
        let is_default = binding.resource_id == Workspaces::default_resource_id(key)?;
        let is_current =
            workspaces.is_bound(key)? && workspaces.resource(key)? == binding.resource_id;
        if !is_default && !is_current {
            workspaces.bind_resource(key, &binding.resource_id)?;
        }
        workspaces.prepare(key)?;
        if resource.resource.owner_run.is_none() {
            workspaces.share_resource(&binding.resource_id)?;
        }
        Ok(())
    }
}

fn require_node(record: &GraphRunRecord, key: &InvocationKey) -> Result<(), ApplicationError> {
    crate::host_fs::validate_key(key)?;
    if record.graph_digest != key.graph_digest
        || !record
            .snapshot
            .nodes
            .iter()
            .any(|node| node.id == key.node_id)
    {
        return Err(ApplicationError::Invalid(
            "workspace node does not belong to the admitted Run".into(),
        ));
    }
    Ok(())
}

fn require_binding(fact: &BindingFact, key: &InvocationKey) -> Result<(), ApplicationError> {
    if fact.released
        || fact.binding.format != 2
        || fact.binding.run_id != key.run_id
        || fact.binding.graph_digest != key.graph_digest
        || fact.binding.node_id != key.node_id
    {
        return Err(ApplicationError::Conflict(
            "workspace binding is released or belongs to another Run node".into(),
        ));
    }
    Ok(())
}

fn binding_id(key: &InvocationKey) -> String {
    format!(
        "workspace-bindings/{:x}",
        Sha256::digest(format!("{}\0{}", key.run_id, key.node_id).as_bytes())
    )
}

fn resource_fact_id(resource: &str) -> String {
    format!("workspaces/{resource}")
}
fn encode(value: impl Serialize) -> Result<Value, ApplicationError> {
    serde_json::to_value(value).map_err(|error| ApplicationError::Storage(error.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ApplicationError> {
    serde_json::from_value(value).map_err(|error| ApplicationError::Storage(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::run_access::fixtures;
    use serde_json::json;
    use std::fs;

    #[test]
    fn kernel_grants_repair_bindings_and_preserve_shared_scenes_on_source_release() {
        let root = tempfile::tempdir().unwrap();
        let app = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
        let workspaces = Workspaces::new(root.path().join("work"));
        let key = |run: &str| {
            let record = fixtures::work_record(run);
            fixtures::create(&app, &record);
            InvocationKey {
                run_id: run.into(),
                graph_digest: record.graph_digest,
                node_id: "worker".into(),
                invocation: 1,
            }
        };
        let source = key("source");
        let target = key("target");
        let saved = app
            .ensure_node_workspace_binding(&source, &workspaces)
            .unwrap();
        let resource = saved["binding"]["resource_id"].as_str().unwrap();
        let scene = workspaces.path(&source).unwrap();
        fs::write(scene.join("project.txt"), "original scene").unwrap();
        workspaces
            .acquire(&source)
            .unwrap()
            .finish_published()
            .unwrap();
        app.bind_admission_workspaces(
            &fixtures::record(&app, &target.run_id),
            &std::collections::BTreeMap::from([(
                target.node_id.clone(),
                WorkspaceAdmissionBinding {
                    run: source.run_id.clone(),
                    node: source.node_id.clone(),
                },
            )]),
            &workspaces,
        )
        .unwrap();

        let target_projection = workspaces
            .root()
            .join("target/.node-workspaces")
            .join(format!("{:x}", Sha256::digest(b"worker")))
            .join("binding.json");
        fs::remove_file(target_projection).unwrap();
        app.ensure_node_workspace_binding(&target, &workspaces)
            .unwrap();
        assert_eq!(workspaces.path(&target).unwrap(), scene);

        // Simulate a stop after the Kernel promotion and before its metadata
        // projection. Deleting the source must first repair that projection.
        fs::write(
            scene.parent().unwrap().join("resource.json"),
            json!({
                "format":1, "resource_id":resource, "owner_run":"source"
            })
            .to_string(),
        )
        .unwrap();
        app.release_run_workspace_facts("source", &workspaces)
            .unwrap();
        assert_eq!(
            fs::read(scene.join("project.txt")).unwrap(),
            b"original scene"
        );
        assert!(
            app.release_workspace_resource(resource, &workspaces)
                .is_err()
        );
        app.release_run_workspace_facts("target", &workspaces)
            .unwrap();
        app.release_workspace_resource(resource, &workspaces)
            .unwrap();
        app.release_workspace_resource(resource, &workspaces)
            .unwrap();
        assert!(!scene.exists());
    }
}
