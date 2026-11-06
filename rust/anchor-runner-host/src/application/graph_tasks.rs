//! Delegation uses ordinary Graph assets and Run admission. Operator receipts
//! bound the authority; a generated asset carries ownership, not a new runtime.
use super::{AdmissionOptions, ApplicationError, RunApplication, graphs};
use crate::{
    assistant_plugins::{self, Publication},
    run_metadata::RunTrigger,
    task_delegation::{TaskGraphOwnership, TaskOrigin, TaskOriginSource},
};
use anchor_platform_session::{SessionStore, TurnStatus};
use anchor_runtime::graph::{GraphSnapshot, GraphTaskPolicy, InvocationKey, PluginBinding};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[cfg(test)]
mod admission_tests;
mod assignment;
#[cfg(test)]
pub(crate) mod assignment_tests;
#[cfg(test)]
mod tests;

pub(super) fn invalid(error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::Invalid(error.to_string())
}

fn declaration<'a>(
    snapshot: &'a GraphSnapshot,
    node: &str,
) -> Option<&'a anchor_runtime::graph::PluginRef> {
    snapshot
        .nodes
        .iter()
        .find(|item| item.id == node)?
        .plugins
        .iter()
        .find(|plugin| plugin.id == "graph-tasks")
}

impl RunApplication {
    /// Called only inside the operator save's catalog/Graph leases. Static
    /// calls are approved as a closure, so editing a callee cannot add grants.
    pub(super) fn publish_graph_task_permissions(
        &self,
        source: &Path,
        snapshot: &GraphSnapshot,
    ) -> Result<(), ApplicationError> {
        let mut pending = snapshot
            .nodes
            .iter()
            .flat_map(|node| &node.plugins)
            .filter_map(|plugin| plugin.graph_tasks.as_ref())
            .flat_map(|policy| policy.graphs.iter().cloned())
            .collect::<Vec<_>>();
        let mut targets = BTreeMap::new();
        while let Some(name) = pending.pop() {
            if targets.contains_key(&name) {
                continue;
            }
            let (path, bundle) = self.load_graph(&name).map_err(invalid)?;
            if crate::run_metadata::graph_identity(&path)?
                == crate::run_metadata::graph_identity(source)?
            {
                return Err(invalid(
                    "a delegated Graph cannot call the source assistant",
                ));
            }
            validate_task_structure(&bundle.snapshot)?;
            pending.extend(
                call_targets(&bundle.snapshot)?
                    .into_iter()
                    .map(str::to_owned),
            );
            targets.insert(name, bundle.snapshot);
        }
        assistant_plugins::publish_with_tasks(
            &self.data_root,
            source,
            snapshot,
            self.library_root(),
            &targets,
        )
        .map_err(invalid)
    }

    /// Mounted tools never accept a scope supplied by the model. A native
    /// invocation and (for assistants) the frozen Turn are its sole source.
    pub(crate) fn graph_task_origin(
        &self,
        key: &InvocationKey,
        mounted: &[PluginBinding],
    ) -> Result<TaskOrigin, ApplicationError> {
        let metadata = self
            .metadata(&key.run_id)?
            .ok_or(ApplicationError::Missing)?;
        let view = self
            .read_run(&key.run_id)?
            .ok_or(ApplicationError::Missing)?;
        if view.progress().graph_digest != key.graph_digest {
            return Err(invalid("task invocation Graph identity changed"));
        }
        if !view
            .dispatches()
            .iter()
            .any(|dispatch| dispatch.key == *key)
        {
            return Err(invalid("this task invocation is no longer current"));
        }
        let (owner, session, digest, declarations, grants) = match &metadata.assistant {
            Some(source) if source.work_node == key.node_id => {
                self.ensure_current_assistant(&metadata)?;
                let binding = crate::assistant::binding(&self.data_root, key)
                    .map_err(invalid)?
                    .ok_or_else(|| invalid("assistant Turn binding unavailable"))?;
                let turn = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
                    .map_err(invalid)?
                    .get_turn(&source.owner, &source.session, &binding.turn)
                    .map_err(invalid)?;
                if turn.status != TurnStatus::Running {
                    return Err(invalid("this assistant Turn is no longer active"));
                }
                let frozen = assistant_plugins::load(
                    &self.data_root,
                    &key.run_id,
                    &source.session,
                    &binding.turn,
                )
                .map_err(invalid)?
                .ok_or_else(|| invalid("frozen Turn permissions unavailable"))?;
                (
                    source.owner.clone(),
                    Some(source.session.clone()),
                    frozen.graph_digest,
                    frozen.declarations,
                    frozen.task_grants,
                )
            }
            Some(_) => return Err(invalid("only the assistant work node can delegate tasks")),
            None => {
                let node = view
                    .progress()
                    .snapshot
                    .nodes
                    .iter()
                    .find(|node| node.id == key.node_id)
                    .ok_or_else(|| invalid("task invocation node unavailable"))?;
                (
                    metadata
                        .oauth_owner
                        .clone()
                        .ok_or_else(|| invalid("delegation requires a trusted owner"))?,
                    None,
                    key.graph_digest.clone(),
                    node.plugins.clone(),
                    metadata.task_grants.clone(),
                )
            }
        };
        let policy = declarations
            .iter()
            .find(|plugin| plugin.id == "graph-tasks")
            .and_then(|plugin| plugin.graph_tasks.clone())
            .ok_or_else(|| invalid("graph-tasks has no node authorization policy"))?;
        let grants = grants.ok_or_else(|| invalid("this invocation has no frozen Graph task authorization; save the Graph in the operator interface and start a new Run or Turn"))?;
        let binding = mounted
            .iter()
            .find(|binding| binding.id == "graph-tasks")
            .ok_or_else(|| invalid("graph-tasks is not mounted in this invocation"))?;
        if !grants
            .bindings
            .get("graph-tasks")
            .is_some_and(|approved| approved.same_authorization(binding))
        {
            return Err(invalid("graph-tasks Plugin authorization changed"));
        }
        Ok(TaskOrigin {
            owner,
            session,
            graph: metadata.graph,
            source: TaskOriginSource::AgentDelegation { key: key.clone() },
            authorization_digest: digest,
            policy,
            bindings: grants.bindings,
            targets: grants.targets,
        })
    }

    /// Future dispatches intersect the original grant with today's operator
    /// grant. A newer save can revoke old work, never enlarge its authority.
    fn current_task_grant(
        &self,
        origin: &TaskOrigin,
    ) -> Result<(GraphTaskPolicy, Publication), ApplicationError> {
        let (source, bundle) = self.load_graph(&origin.graph).map_err(invalid)?;
        let original =
            assistant_plugins::publication(&self.data_root, &source, &origin.authorization_digest)
                .map_err(invalid)?
                .ok_or_else(|| invalid("original Graph task authorization unavailable"))?;
        if original.task_graphs != origin.targets || original.bindings != origin.bindings {
            return Err(invalid(
                "original Graph task authorization changed; renew this task explicitly",
            ));
        }
        let reference = declaration(&bundle.snapshot, origin.node_id())
            .ok_or_else(|| invalid("graph-tasks authorization was removed"))?;
        let policy = reference
            .graph_tasks
            .clone()
            .ok_or_else(|| invalid("Graph task policy was removed"))?;
        let mut enabled = reference.required || reference.default.unwrap_or(true);
        if let Some(session) = &origin.session {
            let session = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
                .map_err(invalid)?
                .get(&origin.owner, session)
                .map_err(invalid)?;
            enabled = reference.required
                || session
                    .plugin_mounts
                    .get("graph-tasks")
                    .copied()
                    .unwrap_or(reference.default.unwrap_or(true));
        }
        if !enabled {
            return Err(invalid("graph-tasks is disabled"));
        }
        let digest = bundle.snapshot.digest()?;
        let current = assistant_plugins::publication(&self.data_root, &source, &digest)
            .map_err(invalid)?
            .ok_or_else(|| {
                invalid("current Graph task permissions have not been approved by the operator")
            })?;
        Ok((policy, current))
    }

    pub(crate) fn authorize_delegated_graph(
        &self,
        origin: &TaskOrigin,
        graph: &str,
    ) -> Result<(), ApplicationError> {
        let (_, bundle) = self.load_graph(graph).map_err(invalid)?;
        self.authorize_delegated_snapshot(origin, graph, &bundle.snapshot)
    }

    pub(super) fn authorize_delegated_snapshot(
        &self,
        origin: &TaskOrigin,
        graph: &str,
        snapshot: &GraphSnapshot,
    ) -> Result<(), ApplicationError> {
        if graph == origin.graph {
            return Err(invalid("a task cannot invoke its source assistant"));
        }
        let (current, receipt) = self.current_task_grant(origin)?;
        validate_task_structure(snapshot)?;
        if origin.policy.graphs.iter().any(|name| name == graph)
            && current.graphs.iter().any(|name| name == graph)
        {
            self.check_pinned_task(origin, &receipt, graph, snapshot)?;
        } else {
            self.task_graph_ownership(origin, graph)?;
            self.check_generated_task(origin, &current, &receipt, snapshot)?;
        }
        self.check_task_plugins(origin, &receipt, snapshot)
    }

    /// Called under a callee's admission lease, not before loading its bundle.
    pub(crate) fn authorize_task_call_snapshot(
        &self,
        origin: &TaskOrigin,
        graph: &str,
        snapshot: &GraphSnapshot,
    ) -> Result<(), ApplicationError> {
        let (_, receipt) = self.current_task_grant(origin)?;
        validate_task_structure(snapshot)?;
        self.check_pinned_task(origin, &receipt, graph, snapshot)?;
        self.check_task_plugins(origin, &receipt, snapshot)
    }

    fn check_pinned_task(
        &self,
        origin: &TaskOrigin,
        current: &Publication,
        graph: &str,
        snapshot: &GraphSnapshot,
    ) -> Result<(), ApplicationError> {
        let digest = snapshot.digest()?;
        if graph == origin.graph
            || origin.targets.get(graph) != Some(&digest)
            || current.task_graphs.get(graph) != Some(&digest)
        {
            return Err(invalid(
                "delegated Graph or its static call closure changed; operator reauthorization is required",
            ));
        }
        Ok(())
    }

    fn check_task_plugins(
        &self,
        origin: &TaskOrigin,
        current: &Publication,
        snapshot: &GraphSnapshot,
    ) -> Result<(), ApplicationError> {
        let mut ids = snapshot
            .nodes
            .iter()
            .flat_map(|node| &node.plugins)
            .map(|plugin| plugin.id.clone())
            .collect::<BTreeSet<_>>();
        let catalog = crate::plugin_library::Catalog::new(self.library_root());
        use anchor_graph_host::PluginCatalog;
        let mut bindings = catalog.resolve(&ids.iter().cloned().collect::<Vec<_>>())?;
        // Dependency-derived networking is also a grant, even without agent.network.
        let network = snapshot.agents.values().any(|agent| agent.network)
            || snapshot
                .ops
                .values()
                .any(|op| op.get("network").and_then(Value::as_bool) == Some(true))
            || ids.iter().any(|id| {
                catalog.metadata(id).is_ok_and(|metadata| {
                    crate::plugin_network::metadata_requires_network(&metadata)
                })
            });
        if network && ids.insert(crate::plugin_network::NETWORK_PLUGIN.into()) {
            bindings.extend(catalog.resolve(&[crate::plugin_network::NETWORK_PLUGIN.into()])?);
        }
        for binding in bindings {
            if !origin
                .bindings
                .get(&binding.id)
                .is_some_and(|approved| approved.same_authorization(&binding))
                || !current
                    .bindings
                    .get(&binding.id)
                    .is_some_and(|approved| approved.same_authorization(&binding))
            {
                return Err(invalid(format!(
                    "Plugin `{}` no longer has the delegated Host authorization",
                    binding.id
                )));
            }
        }
        Ok(())
    }

    pub(crate) async fn admit_delegated_graph(
        &self,
        origin: &TaskOrigin,
        graph: &str,
        input: Value,
        request_id: String,
        trigger: RunTrigger,
    ) -> Result<String, ApplicationError> {
        let source = self.graph_path_checked(&origin.graph).map_err(invalid)?;
        let _source_lease = assistant_plugins::graph_read_lease(&self.data_root, &source)?;
        let request = super::run_access::admission_request(request_id, graph, &input, &None);
        self.admit_graph_with_options(
            graph,
            input,
            AdmissionOptions {
                objective: None,
                trigger,
                pilot: None,
                oauth_owner: Some(origin.credential_owner()),
                workspace_bindings: BTreeMap::new(),
                workspace_root: None,
            },
            Some(request),
            Some(origin.clone()),
        )
        .await
    }

    pub(crate) async fn list_task_graphs(
        &self,
        origin: &TaskOrigin,
    ) -> Result<Value, ApplicationError> {
        let (current, _) = self.current_task_grant(origin)?;
        let mut names = origin
            .policy
            .graphs
            .iter()
            .filter(|name| current.graphs.contains(name))
            .cloned()
            .collect::<BTreeSet<_>>();
        if origin.policy.create_graphs && current.create_graphs {
            for entry in std::fs::read_dir(&self.catalog_root)
                .into_iter()
                .flatten()
                .flatten()
            {
                let name = entry.file_name().to_string_lossy().into_owned();
                if anchor_runtime::identifier::is_name(&name)
                    && self.task_graph_ownership(origin, &name).is_ok()
                {
                    names.insert(name);
                }
            }
        }
        let graphs = names
            .into_iter()
            .map(|name| match self.load_graph(&name) {
                Ok((_, bundle))
                    if self
                        .authorize_delegated_snapshot(origin, &name, &bundle.snapshot)
                        .is_ok() =>
                {
                    json!({"graph":name,"objective":bundle.snapshot.objective,"available":true})
                }
                Ok(_) => json!({"graph":name,"available":false}),
                Err(_) => json!({"graph":name,"available":false}),
            })
            .collect::<Vec<_>>();
        Ok(json!({"graphs":graphs,"creation":{
            "allowed":origin.policy.create_graphs && current.create_graphs,
            "plugins":origin.policy.plugins.iter().filter(|id|current.plugins.contains(id)).collect::<Vec<_>>(),
            "network":origin.policy.network && current.network,
            "commands":origin.policy.commands && current.commands},
            "develop_plugins":origin.policy.develop_plugins && current.develop_plugins}))
    }

    pub(crate) fn read_task_graph(
        &self,
        origin: &TaskOrigin,
        graph: &str,
    ) -> Result<Value, ApplicationError> {
        let (current, _) = self.current_task_grant(origin)?;
        if !(origin.policy.graphs.iter().any(|name| name == graph)
            && current.graphs.iter().any(|name| name == graph))
        {
            if !origin.policy.create_graphs || !current.create_graphs {
                return Err(ApplicationError::Missing);
            }
            self.task_graph_ownership(origin, graph)?;
        }
        self.read_graph(graph).map_err(invalid)
    }

    fn task_graph_ownership(
        &self,
        origin: &TaskOrigin,
        graph: &str,
    ) -> Result<TaskGraphOwnership, ApplicationError> {
        let owned: TaskGraphOwnership =
            crate::assistant::read_json(&task_graph_ownership_path(self, graph)?)
                .map_err(invalid)?
                .ok_or_else(|| invalid("this Graph is not owned by the task scope"))?;
        let source = crate::run_metadata::graph_identity(
            &self.graph_path_checked(&origin.graph).map_err(invalid)?,
        )?;
        if owned.scope != origin.scope_key() || owned.source != source || owned.graph != graph {
            return Err(invalid("this Graph belongs to another task scope"));
        }
        Ok(owned)
    }

    fn check_generated_task(
        &self,
        origin: &TaskOrigin,
        current: &GraphTaskPolicy,
        receipt: &Publication,
        snapshot: &GraphSnapshot,
    ) -> Result<(), ApplicationError> {
        if !origin.policy.create_graphs || !current.create_graphs {
            return Err(invalid("Graph creation is not authorized"));
        }
        validate_task_structure(snapshot)?;
        if snapshot.ops.values().any(|op| op.get("run").is_some())
            && !(origin.policy.commands && current.commands)
        {
            return Err(invalid(
                "command operations exceed the new-Graph capability limit",
            ));
        }
        for plugin in snapshot.nodes.iter().flat_map(|node| &node.plugins) {
            if !origin.policy.plugins.contains(&plugin.id) || !current.plugins.contains(&plugin.id)
            {
                return Err(invalid(format!(
                    "Plugin `{}` exceeds the new-Graph capability limit",
                    plugin.id
                )));
            }
        }
        let catalog = crate::plugin_library::Catalog::new(self.library_root());
        let network = snapshot.agents.values().any(|agent| agent.network)
            || snapshot
                .ops
                .values()
                .any(|op| op.get("network").and_then(Value::as_bool) == Some(true))
            || snapshot
                .nodes
                .iter()
                .flat_map(|node| &node.plugins)
                .any(|plugin| {
                    catalog.metadata(&plugin.id).is_ok_and(|metadata| {
                        crate::plugin_network::metadata_requires_network(&metadata)
                    })
                });
        if network && !(origin.policy.network && current.network) {
            return Err(invalid("networking exceeds the new-Graph capability limit"));
        }
        for target in call_targets(snapshot)? {
            if !origin.policy.graphs.iter().any(|graph| graph == target)
                || !current.graphs.iter().any(|graph| graph == target)
            {
                return Err(invalid(
                    "new Graph static calls must use explicitly selected targets",
                ));
            }
            let (_, bundle) = self.load_graph(target).map_err(invalid)?;
            self.check_pinned_task(origin, receipt, target, &bundle.snapshot)?;
        }
        self.check_task_plugins(origin, receipt, snapshot)
    }

    pub(crate) fn validate_task_graph(
        &self,
        origin: &TaskOrigin,
        definition: &Value,
    ) -> Result<GraphSnapshot, ApplicationError> {
        let snapshot = self.validate_graph(definition).map_err(invalid)?;
        let (current, receipt) = self.current_task_grant(origin)?;
        self.check_generated_task(origin, &current, &receipt, &snapshot)?;
        Ok(snapshot)
    }

    pub(crate) async fn create_task_graph(
        &self,
        origin: &TaskOrigin,
        definition: Value,
        request_id: String,
    ) -> Result<Value, ApplicationError> {
        use sha2::{Digest, Sha256};
        let _catalog = self.graph_catalog_mutation_guard().await;
        let _source = assistant_plugins::graph_read_lease(
            &self.data_root,
            &self.graph_path_checked(&origin.graph).map_err(invalid)?,
        )?;
        let name = format!(
            "task-{:x}",
            Sha256::digest(
                serde_json::to_vec(&(origin.scope_key(), &request_id)).map_err(invalid)?
            )
        );
        let path = self.graph_path_checked(&name).map_err(invalid)?;
        let _lease = self.graph_admission_lease(&path)?;
        let request = super::run_access::admission_request(request_id, &name, &definition, &None);
        if path.exists() {
            let owned = self.task_graph_ownership(origin, &name)?;
            if owned.request != request {
                return Err(invalid("Graph creation request content changed"));
            }
            return self.read_task_graph(origin, &name);
        }
        self.validate_task_graph(origin, &definition)?;
        let owned = TaskGraphOwnership {
            scope: origin.scope_key(),
            source: crate::run_metadata::graph_identity(
                &self.graph_path_checked(&origin.graph).map_err(invalid)?,
            )?,
            graph: name.clone(),
            request,
        };
        // The receipt reserves this name before publication. A crash with only
        // the receipt is retryable with this exact request, never a shared edit.
        crate::assistant::save_immutable(&task_graph_ownership_path(self, &name)?, &owned)
            .map_err(invalid)?;
        graphs::create_graph_bundle_from_catalog(&path, &definition, self.library_root())
            .map_err(invalid)?;
        Ok(json!({"graph":name,"definition":definition,"created":true}))
    }

    pub(crate) async fn update_task_graph(
        &self,
        origin: &TaskOrigin,
        graph: &str,
        definition: Value,
    ) -> Result<Value, ApplicationError> {
        let _catalog = self.graph_catalog_mutation_guard().await;
        self.task_graph_ownership(origin, graph)?;
        let path = self.graph_path_checked(graph).map_err(invalid)?;
        let _lease = self.graph_admission_lease(&path)?;
        self.validate_task_graph(origin, &definition)?;
        graphs::write_graph_bundle_from_catalog(&path, &definition, self.library_root())
            .map_err(invalid)?;
        Ok(json!({"graph":graph,"definition":definition}))
    }

    pub(crate) fn authorize_task_plugin_development(
        &self,
        origin: &TaskOrigin,
    ) -> Result<(), ApplicationError> {
        let (current, _) = self.current_task_grant(origin)?;
        if !origin.policy.develop_plugins || !current.develop_plugins {
            return Err(invalid("Plugin candidate development is not authorized"));
        }
        Ok(())
    }

    pub(crate) fn task_commands_allowed(&self, run: &str) -> Result<bool, ApplicationError> {
        let Some(metadata) = self.metadata(run)? else {
            return Ok(true);
        };
        let Some(origin) = metadata.task.as_ref() else {
            return Ok(true);
        };
        if origin.policy.graphs.contains(&metadata.graph) {
            return Ok(true);
        }
        self.task_graph_ownership(origin, &metadata.graph)?;
        let (current, _) = self.current_task_grant(origin)?;
        Ok(origin.policy.commands && current.commands)
    }

    pub(crate) fn task_run_status(
        &self,
        origin: &TaskOrigin,
        id: &str,
    ) -> Result<Value, ApplicationError> {
        let metadata = self
            .metadata(id)?
            .filter(|metadata| {
                metadata
                    .task
                    .as_ref()
                    .is_some_and(|task| task.same_scope(origin))
            })
            .ok_or(ApplicationError::Missing)?;
        let view = self.read_run(id)?.ok_or(ApplicationError::Missing)?;
        let outputs = view
            .progress()
            .results
            .iter()
            .filter_map(|(node, results)| {
                results
                    .last()
                    .map(|result| json!({"node":node,"result":result}))
            })
            .collect::<Vec<_>>();
        Ok(
            json!({"run":id,"graph":metadata.graph,"status":super::run_access::status_projection(&self.data_root,id)?,"active":view.active(),"results":outputs}),
        )
    }
}

pub(super) fn task_graph_ownership_path(
    application: &RunApplication,
    graph: &str,
) -> Result<std::path::PathBuf, ApplicationError> {
    use sha2::{Digest, Sha256};
    let path = crate::run_metadata::graph_identity(
        &application.graph_path_checked(graph).map_err(invalid)?,
    )?;
    Ok(application
        .data_root
        .join("graph-task-ownership")
        .join(format!(
            "{:x}.json",
            Sha256::digest(path.as_os_str().as_encoded_bytes())
        )))
}

fn validate_task_structure(snapshot: &GraphSnapshot) -> Result<(), ApplicationError> {
    if snapshot
        .nodes
        .iter()
        .flat_map(|node| &node.plugins)
        .any(|plugin| plugin.id == "graph-tasks")
        || snapshot.ops.values().any(|op| op.get("host").is_some())
    {
        return Err(invalid(
            "delegated Graphs cannot mount graph-tasks or use Host operations",
        ));
    }
    call_targets(snapshot)?;
    Ok(())
}

fn call_targets(snapshot: &GraphSnapshot) -> Result<Vec<&str>, ApplicationError> {
    snapshot
        .ops
        .values()
        .filter_map(|op| op.get("call"))
        .map(|call| {
            if call.get("session").is_some() {
                return Err(invalid("delegated Graphs cannot enqueue Session calls"));
            }
            call.get("graph")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("a delegated Graph call requires a static Graph target"))
        })
        .collect()
}
