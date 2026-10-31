//! Shared Call admission over authoritative Kernel resources and dispatch facts.
use super::{ApplicationError, RunApplication, kernel_runs::KernelRuns};
use crate::plugin_library::Catalog;
use anchor_graph_host::{CallRun, CallRunAccess, ChildExecutionPort, InProcessGraphHost};
use anchor_kernel::{Phase, Waiter};
use anchor_runtime::{Cancellation, graph::*};
use std::{collections::BTreeMap, future::Future, pin::Pin};

fn application_error(error: ApplicationError) -> GraphError {
    GraphError::CorruptRun(format!("Kernel Graph call: {error:?}"))
}

pub(crate) struct KernelCallNodes<'a, A, N> {
    application: &'a RunApplication,
    artifacts: &'a A,
    nodes: &'a N,
    workspaces: crate::workspaces::Workspaces,
}
impl<'a, N> KernelCallNodes<'a, crate::HostArtifacts, N> {
    pub(crate) fn new(
        application: &'a RunApplication,
        artifacts: &'a crate::HostArtifacts,
        nodes: &'a N,
    ) -> Self {
        Self {
            application,
            artifacts,
            nodes,
            workspaces: artifacts.workspaces(),
        }
    }
}
impl<N: NodeExecutionPort> NodeExecutionPort for KernelCallNodes<'_, crate::HostArtifacts, N> {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        self.nodes.capabilities()
    }
    fn graph_call_port(&self) -> Option<&dyn GraphCallPort> {
        Some(self)
    }
    fn resolve_plugins(&self, ids: &[String]) -> Result<Vec<PluginBinding>, GraphError> {
        self.nodes.resolve_plugins(ids)
    }
    fn invocation_plugin_authority(&self, record: &GraphRunRecord) -> Result<bool, GraphError> {
        self.nodes.invocation_plugin_authority(record)
    }
    fn observed_plugin_views(&self, key: &InvocationKey) -> BTreeMap<String, String> {
        self.nodes.observed_plugin_views(key)
    }
    fn observed_plugin_digests(&self, key: &InvocationKey) -> BTreeMap<String, String> {
        self.nodes.observed_plugin_digests(key)
    }
    fn completion_fact<'a>(
        &'a self,
        key: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            self.application
                .ensure_node_workspace_binding(key, &self.workspaces)
                .map_err(application_error)?;
            self.nodes.completion_fact(key).await
        })
    }
    fn record_recovery_decision(
        &self,
        key: &InvocationKey,
        attempt: i64,
        decision: RecoveryDecision,
    ) -> Result<(), GraphError> {
        self.nodes.record_recovery_decision(key, attempt, decision)
    }
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            self.application
                .ensure_node_workspace_binding(&request.key, &self.workspaces)
                .map_err(application_error)?;
            self.nodes.execute(request).await
        })
    }
}

struct KernelCallAccess<'a, A> {
    application: &'a RunApplication,
    artifacts: &'a A,
}
impl<A: ArtifactPort> CallRunAccess for KernelCallAccess<'_, A> {
    fn load(&self, id: &str) -> Result<Option<CallRun>, GraphError> {
        let Some(owner) = self
            .application
            .kernel_owner(id)
            .map_err(application_error)?
        else {
            return Ok(None);
        };
        let resource = owner
            .load(id)
            .map_err(application_error)?
            .ok_or_else(|| GraphError::CorruptRun("missing Call Run".into()))?;
        Ok(Some(CallRun {
            record: validator::record_of(&resource)?,
            phase: resource
                .phase()
                .ok_or_else(|| GraphError::CorruptRun("missing Call Run phase".into()))?
                .clone(),
        }))
    }
    fn admit(&self, record: &GraphRunRecord) -> Result<(), GraphError> {
        self.application
            .kernel_runs(true)
            .map_err(application_error)?
            .expect("Run store initialized")
            .create(record, u64::MAX)
            .map_err(application_error)
    }
    fn acquire_lease(&self, id: &str) -> Result<Box<dyn RunLease>, GraphError> {
        self.application.run_leases().acquire_lease(id)
    }
    fn validate_parent<'a>(
        &'a self,
        identity: &'a CallIdentity,
        input: &'a serde_json::Value,
        commits: &'a [CommitRef],
    ) -> Pin<Box<dyn Future<Output = Result<GraphRunRecord, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let parent = self
                .load(&identity.parent_run_id)?
                .ok_or_else(|| GraphError::CorruptRun("missing Call parent".into()))?;
            let owner = self
                .application
                .kernel_owner(&identity.parent_run_id)
                .map_err(application_error)?
                .expect("parent loaded");
            let resource = owner
                .load(&identity.parent_run_id)
                .map_err(application_error)?
                .expect("parent loaded");
            let dispatch = validator::GraphValidator::frontier(&resource)?
                .into_iter()
                .find(|dispatch| {
                    dispatch.kind == validator::DispatchKind::Call
                        && dispatch.node.id == identity.node_id
                        && dispatch.invocation == identity.invocation
                })
                .ok_or_else(|| {
                    GraphError::CorruptRun("call is not a current Kernel dispatch".into())
                })?;
            if validator::call_identity(&parent.record, &dispatch)? != *identity
                || dispatch.inputs != commits
            {
                return Err(GraphError::CorruptRun(
                    "call identity or provenance differs from Kernel dispatch".into(),
                ));
            }
            let key = InvocationKey {
                run_id: identity.parent_run_id.clone(),
                graph_digest: identity.parent_graph_digest.clone(),
                node_id: identity.node_id.clone(),
                invocation: identity.invocation,
            };
            let prepared =
                scheduler::dispatch_input(self.artifacts, &parent.record, &dispatch, key).await?;
            if prepared.prepared_input != *input {
                return Err(GraphError::CorruptRun(
                    "call input differs from Kernel dispatch".into(),
                ));
            }
            Ok(parent.record)
        })
    }
}

impl<N: NodeExecutionPort> GraphCallPort for KernelCallNodes<'_, crate::HostArtifacts, N> {
    fn call<'a>(
        &'a self,
        identity: &'a CallIdentity,
        spec: &'a serde_json::Value,
        input: &'a serde_json::Value,
        commits: &'a [CommitRef],
        cancellation: Cancellation,
    ) -> Pin<Box<dyn Future<Output = Result<GraphCallOutcome, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let key = InvocationKey {
                run_id: identity.parent_run_id.clone(),
                graph_digest: identity.parent_graph_digest.clone(),
                node_id: identity.node_id.clone(),
                invocation: identity.invocation,
            };
            self.application
                .ensure_node_workspace_binding(&key, &self.workspaces)
                .map_err(application_error)?;
            let store = KernelCallAccess {
                application: self.application,
                artifacts: self.artifacts,
            };
            let parent = store
                .load(&identity.parent_run_id)?
                .ok_or_else(|| GraphError::CorruptRun("missing Kernel Call parent".into()))?;
            let defer = !parent
                .record
                .graph_calls
                .contains_key(&identity.durable_key());
            let catalog = crate::execution::HostGraphCatalog {
                root: self.application.catalog_root.clone(),
                state: self.application.data_root.clone(),
                application: Some(self.application.clone()),
            };
            let plugins = Catalog::new(self.application.library_root().to_path_buf());
            let host = InProcessGraphHost::new(&catalog, &store, self.artifacts, self.nodes)
                .with_plugin_catalog(&plugins)
                .with_child_executor(self, defer);
            match host
                .call(identity, spec, input, commits, cancellation)
                .await
            {
                Err(GraphError::InvalidSnapshot(reason)) => {
                    // Prove absence under the admission lock. An error after
                    // child admission cannot authorize a new child or replay.
                    let id = anchor_graph_host::child_run_id(identity);
                    let _lease = store.acquire_lease(&id)?;
                    if self
                        .application
                        .load_run(&id)
                        .map_err(application_error)?
                        .is_none()
                        && self
                            .application
                            .metadata(&id)
                            .map_err(application_error)?
                            .is_none()
                    {
                        Err(GraphError::CallAdmissionFailed(reason))
                    } else {
                        Err(GraphError::InvalidSnapshot(reason))
                    }
                }
                outcome => {
                    let completion = match &outcome {
                        Ok(GraphCallOutcome::Completed {
                            child_run_id,
                            output,
                        }) => Some(
                            scheduler::graph_call_completion(spec, child_run_id, output.clone()).1,
                        ),
                        Ok(GraphCallOutcome::Detached { child_run_id }) => Some(
                            scheduler::graph_call_completion(
                                spec,
                                child_run_id,
                                serde_json::Value::Null,
                            )
                            .1,
                        ),
                        _ => None,
                    };
                    if let Some(completion) = completion {
                        // Result export reserved this exact invocation. Its
                        // durable owner excludes another writer after the
                        // export's process lease drops and before publication.
                        self.artifacts
                            .freeze_with_context(
                                &key,
                                &completion,
                                &ArtifactFreezeContext {
                                    kind: ArtifactKind::GraphCall,
                                    input_commits: commits.to_vec(),
                                },
                            )
                            .await?;
                    }
                    outcome
                }
            }
        })
    }
}

impl<N: NodeExecutionPort> ChildExecutionPort for KernelCallNodes<'_, crate::HostArtifacts, N> {
    fn execute<'a>(
        &'a self,
        child: CallRun,
        cancellation: Cancellation,
    ) -> Pin<Box<dyn Future<Output = Result<CallRun, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            let owner = self
                .application
                .kernel_owner(&child.record.run_id)
                .map_err(application_error)?
                .ok_or_else(|| {
                    GraphError::CorruptRun("child admission is not Kernel-owned".into())
                })?;
            let resource = owner
                .load(&child.record.run_id)
                .map_err(application_error)?
                .expect("owned child");
            // A Human wait remains parked. Child waits are observed by the
            // scheduler; no human release is manufactured by a parent call.
            if matches!(
                resource.phase(),
                Some(Phase::Waiting {
                    waiter: Waiter::Human { .. }
                })
            ) {
                return Ok(child);
            }
            let metadata = self
                .application
                .metadata(&child.record.run_id)
                .map_err(application_error)?
                .ok_or_else(|| GraphError::CorruptRun("missing child metadata".into()))?;
            let mut control = super::new_control();
            control.cancellation = cancellation;
            let prepared = self
                .application
                .prepare_resume(&child.record, &metadata, control.clone())
                .map_err(application_error)?;
            let (artifacts, nodes) = prepared.kernel_ports(self.application.clone());
            let nodes = KernelCallNodes::new(self.application, &artifacts, &nodes);
            let resource = owner
                .drive(
                    &child.record.run_id,
                    &artifacts,
                    &nodes,
                    control.cancellation,
                )
                .await?;
            Ok(CallRun {
                record: KernelRuns::record(&resource).map_err(application_error)?,
                phase: resource.phase().expect("Graph Run phase").clone(),
            })
        })
    }
}

impl RunApplication {
    pub(crate) async fn kernel_recheck_child(&self, id: &str) -> Result<(), ApplicationError> {
        let owner = self.kernel_owner(id)?.ok_or(ApplicationError::Missing)?;
        let resource = owner.load(id)?.ok_or(ApplicationError::Missing)?;
        let Some(Phase::Waiting {
            waiter: Waiter::Child { task },
        }) = resource.phase()
        else {
            return Ok(());
        };
        let child = task
            .strip_prefix("run/")
            .ok_or_else(|| ApplicationError::Storage("invalid child wait target".into()))?;
        let Some(child_owner) = self.kernel_owner(child)? else {
            return Ok(());
        };
        if child_owner
            .load(child)?
            .is_some_and(|resource| resource.is_settled())
        {
            self.kernel_child_finished(child).await?;
        }
        Ok(())
    }

    pub(crate) async fn kernel_dispatch_detached(&self, id: &str) -> Result<(), ApplicationError> {
        self.executor()?;
        let owner = self.kernel_owner(id)?.ok_or(ApplicationError::Missing)?;
        if owner.active(id) {
            return Ok(());
        }
        let resource = owner.load(id)?.ok_or(ApplicationError::Missing)?;
        if resource.phase() != Some(&Phase::Executing) {
            return Ok(());
        }
        let record = KernelRuns::record(&resource)?;
        let metadata = self.metadata(id)?.ok_or(ApplicationError::Missing)?;
        // Unbound Session calls are queued deliveries, not detached workers.
        // The Session host owns the transition that binds their execution.
        if metadata.session_call.is_some() && metadata.conversation.is_none() {
            return Ok(());
        }
        let source = metadata.graph_call.as_ref().ok_or_else(|| {
            ApplicationError::Invalid("detached child has no Graph call source".into())
        })?;
        if source.mode != "detach" || metadata.trigger_source != "graph_call" {
            return Err(ApplicationError::Invalid(
                "Run is not an admitted detached child".into(),
            ));
        }
        let control = super::new_control();
        let prepared = self.prepare_resume(&record, &metadata, control.clone())?;
        self.spawn_kernel(record, prepared, control)
    }

    pub(crate) fn kernel_child_finished<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ApplicationError>> + Send + 'a>> {
        Box::pin(async move {
            let Some(owner) = self.kernel_owner(id)? else {
                return Ok(());
            };
            if !owner
                .load(id)?
                .is_some_and(|resource| resource.is_settled())
            {
                return Ok(());
            }
            let Some(metadata) = self.metadata(id)? else {
                return Ok(());
            };
            if metadata
                .session_call
                .as_ref()
                .is_some_and(|call| call.status == "pending")
            {
                return Ok(());
            }
            let Some(source) = metadata.graph_call else {
                return Ok(());
            };
            if source.mode != "wait" {
                return Ok(());
            }
            let Some(parent) = self.kernel_owner(&source.parent_run)? else {
                return Ok(());
            };
            if parent.active(&source.parent_run) {
                return Ok(());
            }
            let resource = parent
                .load(&source.parent_run)?
                .ok_or(ApplicationError::Missing)?;
            if resource.phase()
                != Some(&Phase::Waiting {
                    waiter: Waiter::Child {
                        task: format!("run/{id}"),
                    },
                })
            {
                return Ok(());
            }
            let record = KernelRuns::record(&resource)?;
            if !record.graph_calls.values().any(|call| {
                call.identity.node_id == source.node
                    && call.identity.invocation == source.invocation
                    && call.child_run_id.as_deref() == Some(id)
            }) {
                return Err(ApplicationError::Conflict(
                    "child completion differs from parent binding".into(),
                ));
            }
            let metadata = self
                .metadata(&source.parent_run)?
                .ok_or(ApplicationError::Missing)?;
            let control = super::new_control();
            let prepared = self.prepare_resume(&record, &metadata, control.clone())?;
            self.spawn_kernel(record, prepared, control)
        })
    }
}
