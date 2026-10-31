//! Kernel Run authority and execution registration.
use super::{ApplicationError, RunApplication};
use anchor_kernel::{
    Action, Authority, FileStore, Kernel, Phase, Principal, PrincipalKind, Resource, Store,
    Submission,
};
use anchor_runtime::graph::{ArtifactPort, GraphError, GraphRunRecord, NodeExecutionPort};
use anchor_runtime::graph::{
    scheduler::{
        EXECUTOR_ID, HOST_ID, KernelScheduler, OPERATOR_ID, kernel_error, submit_with_retry,
    },
    validator::{GRAPH_VALIDATOR, GraphValidator, record_of, run_resource, run_resource_id},
};
use std::{
    collections::{BTreeSet, HashMap},
    fs::OpenOptions,
    path::Path,
    sync::{Arc, Mutex, atomic::Ordering},
};

pub(super) mod deletion;
mod pause;
#[path = "kernel_probe.rs"]
mod probe;
mod recovery;

struct KernelExecution {
    cancellation: anchor_runtime::Cancellation,
    pause: tokio::sync::mpsc::UnboundedSender<()>,
    wait_pause: Arc<tokio::sync::Notify>,
    finished: Arc<tokio::sync::Notify>,
}

struct RunAuthority;
impl Authority for RunAuthority {
    fn permits(&self, p: &Principal, action: Action, _: &str) -> bool {
        match (p.id.as_str(), p.kind) {
            (EXECUTOR_ID, PrincipalKind::Agent) => action == Action::Write,
            (HOST_ID, PrincipalKind::Code) => {
                matches!(action, Action::Write | Action::Settle | Action::Request)
            }
            (OPERATOR_ID, PrincipalKind::Human) => action == Action::Request,
            _ => false,
        }
    }
}

pub(crate) struct KernelRuns {
    pub(crate) kernel: Kernel<FileStore>,
    observations: super::run_observations::RunObservations,
    active: Arc<Mutex<HashMap<String, KernelExecution>>>,
    // Deletion reserves inactive Runs across filesystem cleanup. Registration,
    // creation and controls take active before this mutex.
    deleting: Mutex<BTreeSet<String>>,
    pub(super) file_facts: Mutex<()>,
    pause_root: std::path::PathBuf,
    _lease: anchor_file_lease::FileLease,
}

struct ExecutionGuard {
    active: Arc<Mutex<HashMap<String, KernelExecution>>>,
    id: String,
    finished: Arc<tokio::sync::Notify>,
}
impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        self.active.lock().unwrap().remove(&self.id);
        self.finished.notify_waiters();
    }
}

struct RegisteredExecution {
    guard: ExecutionGuard,
    requests: tokio::sync::mpsc::UnboundedReceiver<()>,
    cancellation: anchor_runtime::Cancellation,
}

impl KernelRuns {
    fn open(root: &Path) -> Result<Self, ApplicationError> {
        std::fs::create_dir_all(root).map_err(super::storage)?;
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".writer.lock"))
            .map_err(super::storage)?;
        let lease = anchor_file_lease::FileLease::acquire(
            lease,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .map_err(|e| {
            ApplicationError::Conflict(format!("Kernel Run store already has a writer: {e}"))
        })?;
        let mut kernel = Kernel::new(
            FileStore::new(root).map_err(|e| ApplicationError::Storage(e.to_string()))?,
            Box::new(RunAuthority),
        )
        .map_err(kernel_error)?;
        let probe = probe::HostProbe::new(
            &root.parent().expect("state root").join("kernel-probe"),
            kernel.store(),
        );
        kernel = kernel.with_probe(Box::new(probe));
        kernel
            .install(GRAPH_VALIDATOR, Arc::new(GraphValidator))
            .map_err(kernel_error)?;
        kernel
            .install(
                super::file_facts::FILE_VALIDATOR,
                Arc::new(super::file_facts::HostFiles),
            )
            .map_err(kernel_error)?;
        Ok(Self {
            kernel,
            active: Arc::default(),
            deleting: Mutex::default(),
            file_facts: Mutex::default(),
            pause_root: root
                .parent()
                .expect("state root")
                .join("kernel-pause-intents"),
            observations: super::run_observations::RunObservations::new(
                &root
                    .parent()
                    .expect("state root")
                    .join("kernel-run-observations"),
            ),
            _lease: lease,
        })
    }
    pub(crate) fn load(&self, id: &str) -> Result<Option<Resource>, ApplicationError> {
        Ok(self
            .kernel
            .load(&run_resource_id(id))
            .map_err(kernel_error)?
            .filter(|r| r.validator == GRAPH_VALIDATOR))
    }
    pub(crate) fn create(
        &self,
        record: &GraphRunRecord,
        budget: u64,
    ) -> Result<(), ApplicationError> {
        crate::reject_snapshot(&record.snapshot).map_err(ApplicationError::Invalid)?;
        let _active = self.active.lock().unwrap();
        self.ensure_not_deleting(&record.run_id)?;
        self.kernel
            .create(run_resource(record, budget)?)
            .map_err(kernel_error)?;
        if let Err(error) = anchor_runtime::graph::scheduler::SubmissionObserver::accepted(
            &self.observations,
            &run_resource_id(&record.run_id),
        ) {
            eprintln!("Host initial update observation unavailable: {error}");
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn registry_is_unlocked(&self) -> bool {
        self.active.try_lock().is_ok()
    }
    pub(crate) fn active(&self, id: &str) -> bool {
        self.active.lock().unwrap().contains_key(id)
    }
    pub(crate) async fn wait_idle(&self, id: &str) {
        loop {
            let Some(finished) = self
                .active
                .lock()
                .unwrap()
                .get(id)
                .map(|execution| Arc::clone(&execution.finished))
            else {
                return;
            };
            let notified = finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            // Enable before rechecking the same registration, so both a
            // completed worker and a replacement worker avoid lost wakeups.
            let same_worker = self
                .active
                .lock()
                .unwrap()
                .get(id)
                .is_some_and(|execution| Arc::ptr_eq(&execution.finished, &finished));
            if same_worker {
                notified.await;
            }
        }
    }
    fn ensure_idle(worker: bool) -> Result<(), ApplicationError> {
        if worker {
            return Err(ApplicationError::Conflict(
                "Kernel Run execution is still draining".into(),
            ));
        }
        Ok(())
    }
    pub(crate) fn drive<'a, A: ArtifactPort + ?Sized, N: NodeExecutionPort + ?Sized>(
        &'a self,
        id: &str,
        artifacts: &'a A,
        nodes: &'a N,
        cancellation: anchor_runtime::Cancellation,
    ) -> impl std::future::Future<Output = Result<Resource, GraphError>> + 'a {
        let registration = self.register(id, cancellation);
        async move { self.drive_registered(registration?, artifacts, nodes).await }
    }
    fn register(
        &self,
        id: &str,
        cancellation: anchor_runtime::Cancellation,
    ) -> Result<RegisteredExecution, GraphError> {
        let (pause, requests) = tokio::sync::mpsc::unbounded_channel();
        let wait_pause = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(tokio::sync::Notify::new());
        {
            let mut active = self.active.lock().unwrap();
            if active.contains_key(id) {
                return Err(GraphError::RunBusy(id.to_owned()));
            }
            self.ensure_not_deleting(id)
                .map_err(|_| GraphError::RunBusy(id.to_owned()))?;
            // Registration and deletion share this lock. A worker must observe
            // an existing Run before reserving its execution slot.
            if self
                .load(id)
                .map_err(|error| {
                    GraphError::CorruptRun(format!("cannot load Kernel Run: {error:?}"))
                })?
                .is_none()
            {
                return Err(GraphError::CorruptRun("missing Kernel Run".into()));
            }
            if self.pause_pending(id)? {
                pause.send(()).expect("new pause receiver");
                wait_pause.notify_one();
            }
            active.insert(
                id.to_owned(),
                KernelExecution {
                    cancellation: cancellation.clone(),
                    pause,
                    wait_pause,
                    finished: Arc::clone(&finished),
                },
            );
        }
        Ok(RegisteredExecution {
            guard: ExecutionGuard {
                active: Arc::clone(&self.active),
                id: id.to_owned(),
                finished,
            },
            requests,
            cancellation,
        })
    }
    async fn drive_registered<A: ArtifactPort + ?Sized, N: NodeExecutionPort + ?Sized>(
        &self,
        registration: RegisteredExecution,
        artifacts: &A,
        nodes: &N,
    ) -> Result<Resource, GraphError> {
        let RegisteredExecution {
            guard,
            requests,
            cancellation,
        } = registration;
        let boundary = pause::PauseBoundary::new(self, requests);
        KernelScheduler {
            kernel: &self.kernel,
            observer: Some(&self.observations),
            boundary: Some(&boundary),
            artifacts,
            nodes,
            cancellation,
        }
        .run(&run_resource_id(&guard.id))
        .await
    }
    pub(crate) fn control(
        &self,
        id: &str,
        operation: &str,
        reason: Option<&str>,
    ) -> Result<(), ApplicationError> {
        if operation == "pause" {
            return self.request_pause(id);
        }
        if !matches!(operation, "stop" | "resume" | "abandon") {
            return Err(ApplicationError::Invalid(
                "unknown Kernel Run control".into(),
            ));
        }
        let active = self.active.lock().unwrap();
        self.ensure_not_deleting(id)?;
        if operation == "resume" {
            Self::ensure_idle(active.contains_key(id))?;
        }
        let resource = self.load(id)?.ok_or(ApplicationError::Missing)?;
        let submission = Submission::new(&resource.id, resource.version, Action::Request)
            .content(serde_json::json!({"control":operation,"reason":reason.unwrap_or(if operation == "stop" { "stopped" } else { operation })}));
        let verdict = submit_with_retry(
            &self.kernel,
            &Principal::new(OPERATOR_ID, PrincipalKind::Human),
            0,
            submission,
            Some(&self.observations),
        )?;
        if !verdict.is_accepted() {
            return Err(ApplicationError::Conflict(format!(
                "Kernel control refused: {:?}",
                verdict.reason
            )));
        }
        if operation != "resume"
            && let Some(execution) = active.get(id)
        {
            execution.cancellation.store(true, Ordering::Release);
        }
        self.clear_pause(id)?;
        Ok(())
    }
    pub(crate) fn record(resource: &Resource) -> Result<GraphRunRecord, ApplicationError> {
        let record = record_of(resource)?;
        record.validate()?;
        Ok(record)
    }
    pub(crate) fn view(
        &self,
        resource: &Resource,
    ) -> Result<super::run_view::RunView, ApplicationError> {
        let record = record_of(resource)?;
        Self::project(
            resource,
            self.active(&record.run_id),
            self.observations.load(&record.run_id).ok().flatten(),
        )
    }
    pub(super) fn project(
        resource: &Resource,
        worker: bool,
        updated: Option<String>,
    ) -> Result<super::run_view::RunView, ApplicationError> {
        Ok(super::run_view::RunView {
            reason: None,
            phase: resource
                .phase()
                .ok_or_else(|| ApplicationError::Storage("Kernel Run has no phase".into()))?
                .clone(),
            worker,
            updated,
        })
    }
}

impl RunApplication {
    pub(crate) fn kernel_runs(
        &self,
        create: bool,
    ) -> Result<Option<Arc<KernelRuns>>, ApplicationError> {
        let mut cached = self.kernel_run_owner.lock().unwrap();
        if cached.is_none() && (create || self.data_root.join("kernel-runs").exists()) {
            *cached = Some(Arc::new(KernelRuns::open(
                &self.data_root.join("kernel-runs"),
            )?));
        }
        Ok(cached.clone())
    }
    pub(crate) fn kernel_owner(
        &self,
        id: &str,
    ) -> Result<Option<Arc<KernelRuns>>, ApplicationError> {
        Ok(self.kernel_resource(id)?.map(|(owner, _)| owner))
    }

    pub(in crate::application::run_access) fn kernel_resource(
        &self,
        id: &str,
    ) -> Result<Option<(Arc<KernelRuns>, Resource)>, ApplicationError> {
        if !anchor_runtime::identifier::is_name(id) {
            return Err(ApplicationError::Invalid("invalid Run id".into()));
        }
        self.reject_legacy_run(id)?;
        let Some(runs) = self.kernel_runs(false)? else {
            return Ok(None);
        };
        let Some(resource) = runs.load(id)? else {
            return Ok(None);
        };
        self.reject_legacy_run(id)?;
        Ok(Some((runs, resource)))
    }

    pub(crate) fn registered_run_ids(&self) -> Result<Vec<String>, ApplicationError> {
        let Some(owner) = self.kernel_runs(false)? else {
            return Ok(Vec::new());
        };
        let mut ids: Vec<_> = owner.active.lock().unwrap().keys().cloned().collect();
        ids.sort();
        Ok(ids)
    }
    pub(in crate::application::run_access) fn load_run_impl(
        &self,
        id: &str,
    ) -> Result<Option<GraphRunRecord>, ApplicationError> {
        self.kernel_resource(id)?
            .map(|(_, resource)| KernelRuns::record(&resource))
            .transpose()
    }
    #[cfg(not(test))]
    pub(in crate::application::run_access) fn load_run(
        &self,
        id: &str,
    ) -> Result<Option<GraphRunRecord>, ApplicationError> {
        self.load_run_impl(id)
    }
    #[cfg(test)]
    pub(crate) fn load_run(&self, id: &str) -> Result<Option<GraphRunRecord>, ApplicationError> {
        self.load_run_impl(id)
    }
    /// Runtime adapter only; business consumers use read_run and RunView.
    pub(crate) fn execution_record(
        &self,
        id: &str,
    ) -> Result<Option<GraphRunRecord>, ApplicationError> {
        self.load_run_impl(id)
    }
    pub(in crate::application::run_access) fn kernel_run_ids(
        &self,
    ) -> Result<Vec<String>, ApplicationError> {
        let Some(runs) = self.kernel_runs(false)? else {
            return Ok(Vec::new());
        };
        Ok(runs
            .kernel
            .store()
            .list("run/")
            .map_err(|e| ApplicationError::Storage(e.to_string()))?
            .into_iter()
            .map(|id| id.strip_prefix("run/").expect("Run namespace").to_owned())
            .collect())
    }
}

#[cfg(test)]
mod tests;

impl RunApplication {
    /// An idle Session wait can suspend on a pause event without cancelling
    /// work or polling a second Run state. The dispatch boundary owns stop.
    pub(crate) fn kernel_wait_pause(
        &self,
        id: &str,
    ) -> Result<Option<Arc<tokio::sync::Notify>>, ApplicationError> {
        let Some(runs) = self.kernel_owner(id)? else {
            return Ok(None);
        };
        let active = runs.active.lock().unwrap();
        Ok(active.get(id).map(|execution| execution.wait_pause.clone()))
    }

    pub(crate) fn spawn_kernel(
        &self,
        record: GraphRunRecord,
        execution: crate::execution::PreparedExecution,
        control: crate::HostControl,
    ) -> Result<(), ApplicationError> {
        let executor = self.executor()?.clone();
        let runs = self.kernel_runs(true)?.expect("Kernel Run enabled");
        // Reserve the sole worker slot before returning to admission. Moving
        // its guard into the task also releases an unpolled or aborted worker.
        let registration = runs.register(&record.run_id, control.cancellation)?;
        let (artifacts, nodes) = execution.kernel_ports(self.clone());
        let application = self.clone();
        executor.spawn(async move {
            let nodes = super::kernel_calls::KernelCallNodes::new(&application, &artifacts, &nodes);
            if let Err(error) = runs
                .drive_registered(registration, &artifacts, &nodes)
                .await
            {
                eprintln!("Kernel Run {} suspended: {error}", record.run_id);
            }
            if let Err(error) = application.settle_channel_run(&record.run_id) {
                eprintln!(
                    "Kernel channel {} settlement failed: {error:?}",
                    record.run_id
                );
            }
            // A child can settle while its parent worker is releasing the
            // active slot. Recheck the durable wait once after that release.
            if let Err(error) = application.kernel_recheck_child(&record.run_id).await {
                eprintln!(
                    "Kernel parent {} child observation failed: {error:?}",
                    record.run_id
                );
            }
            if let Err(error) = application.kernel_child_finished(&record.run_id).await {
                eprintln!(
                    "Kernel child {} completion callback failed: {error:?}",
                    record.run_id
                );
            }
        });
        Ok(())
    }
    pub(crate) async fn kernel_control(
        &self,
        runs: Arc<KernelRuns>,
        id: &str,
        operation: &str,
    ) -> Result<(), ApplicationError> {
        if operation != "resume" {
            return runs.control(id, operation, None);
        }
        KernelRuns::ensure_idle(runs.active(id))?;
        let resource = runs.load(id)?.ok_or(ApplicationError::Missing)?;
        // Let Kernel reject a settled control before preparing any execution
        // resources. Settlement is irreversible even for a retained attempt.
        if resource.is_settled() {
            return runs.control(id, "resume", None);
        }
        let record = KernelRuns::record(&resource)?;
        let metadata = self.metadata(id)?.ok_or(ApplicationError::Missing)?;
        let active = self.admission.lock().await;
        self.check_execution_scope(&metadata, &active)?;
        super::validate_control_identity(&record, &metadata)?;
        let control = super::new_control();
        self.reject_superseded_conversation_execution(id)?;
        let execution = self.prepare_resume(&record, &metadata, control.clone())?;
        execution
            .bind_local_inputs(&record, &metadata.graph)
            .map_err(ApplicationError::Invalid)?;
        self.inspect_interrupted_attempts(&runs, id, &execution)?;
        let resource = runs.load(id)?.ok_or(ApplicationError::Missing)?;
        if matches!(
            resource.phase(),
            Some(Phase::Waiting {
                waiter: anchor_kernel::Waiter::Child { .. }
            })
        ) {
            // Human resume asks the Host to continue the wait closure. Only
            // observing the bound child can release a Child wait in Kernel.
            drop(active);
            for child in super::wait_child_ids(&record) {
                if let Some(child_view) = self.read_run(&child)?
                    && !child_view.is_settled()
                    && !child_view.active()
                    && !child_view.waits_for_children()
                {
                    Box::pin(self.resume(&child)).await?;
                }
            }
        } else if resource.phase() != Some(&Phase::Executing) {
            runs.control(id, "resume", None)?;
        }
        self.spawn_kernel(record, execution, control)
    }
}

impl RunApplication {
    pub(crate) fn dispatch_keys(
        &self,
        id: &str,
    ) -> Result<Vec<anchor_runtime::graph::InvocationKey>, ApplicationError> {
        let view = self.read_run(id)?.ok_or(ApplicationError::Missing)?;
        Ok(view
            .frontier
            .into_iter()
            .map(|dispatch| dispatch.key)
            .collect())
    }

    pub(crate) fn dispatch_inputs(
        &self,
        key: &anchor_runtime::graph::InvocationKey,
    ) -> Result<Vec<anchor_runtime::graph::CommitRef>, ApplicationError> {
        let view = self
            .read_run(&key.run_id)?
            .ok_or(ApplicationError::Missing)?;
        view.frontier
            .into_iter()
            .find(|dispatch| dispatch.key == *key)
            .map(|dispatch| dispatch.input_commits)
            .ok_or_else(|| {
                ApplicationError::Conflict("invocation is not a current Kernel dispatch".into())
            })
    }

    pub(crate) fn predecessor_history_allowed(&self, id: &str) -> Result<bool, ApplicationError> {
        Ok(self
            .read_run(id)?
            .is_some_and(|view| view.has_stopped_execution()))
    }

    /// The standalone protocol and platform Host share admission and execution.
    pub(crate) async fn execute_standalone(
        &self,
        record: GraphRunRecord,
        cancellation: anchor_runtime::Cancellation,
        reference_lease: Option<anchor_library::InstallLease>,
    ) -> Result<super::RunView<'static>, ApplicationError> {
        self.reject_legacy_run(&record.run_id)?;
        let mut control = super::new_control();
        control.cancellation = cancellation;
        let prepared = crate::execution::PreparedExecution::prepare_with_catalog(
            &record,
            control.clone(),
            record.plugin_bindings.clone(),
            self.clone(),
        )
        .map_err(ApplicationError::Invalid)?;
        let owner = self.kernel_runs(true)?.expect("Run store initialized");
        let should_drive = if let Some(resource) = owner.load(&record.run_id)? {
            let existing = KernelRuns::record(&resource)?;
            if existing.snapshot != record.snapshot
                || existing.input != record.input
                || existing.graph_digest != record.graph_digest
            {
                return Err(ApplicationError::Conflict(
                    "supplied Run differs from durable admission".into(),
                ));
            }
            resource.phase() == Some(&Phase::Executing)
                || matches!(
                    resource.phase(),
                    Some(Phase::Waiting {
                        waiter: anchor_kernel::Waiter::Child { .. }
                    })
                )
        } else {
            owner.create(&record, u64::MAX)?;
            true
        };
        drop(reference_lease);
        if should_drive {
            let (artifacts, nodes) = prepared.kernel_ports(self.clone());
            let nodes = super::kernel_calls::KernelCallNodes::new(self, &artifacts, &nodes);
            owner
                .drive(&record.run_id, &artifacts, &nodes, control.cancellation)
                .await?;
            self.settle_channel_run(&record.run_id)?;
            self.kernel_child_finished(&record.run_id).await?;
        }
        self.read_run(&record.run_id)?
            .ok_or(ApplicationError::Missing)
    }
}
