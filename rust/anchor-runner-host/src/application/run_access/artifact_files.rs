//! Fixed delivery access and retention, independent of the source Run.
use super::{
    ApplicationError, RunApplication, file_facts::FILE_VALIDATOR, kernel_runs::KernelRuns,
};
use crate::{
    HostArtifacts,
    artifacts::{ArtifactAvailability, ArtifactGcReport, ArtifactRead},
};
use anchor_kernel::Store;
use anchor_runtime::graph::CommitRef;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

#[derive(Serialize, Deserialize)]
struct ArtifactRetention {
    artifact: CommitRef,
    retained: bool,
}

#[derive(Debug)]
pub(crate) enum ArtifactAccessError {
    Application(ApplicationError),
    Unavailable(ArtifactAvailability),
}
impl From<ApplicationError> for ArtifactAccessError {
    fn from(error: ApplicationError) -> Self {
        Self::Application(error)
    }
}

fn storage(error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::Storage(error.to_string())
}

impl RunApplication {
    fn delivery_store(&self, workspace_root: &Path) -> HostArtifacts {
        HostArtifacts::new(self.data_root.join("artifacts"), workspace_root.into())
    }

    pub(crate) fn delivery_listing(
        &self,
        id: &str,
        workspace_root: &Path,
    ) -> Result<Value, ApplicationError> {
        let artifacts = self.delivery_store(workspace_root);
        let commit = artifacts
            .reference(id)
            .map_err(storage)?
            .ok_or(ApplicationError::Missing)?;
        let files = artifacts.list_files(&commit).map_err(storage)?;
        let availability = artifacts.availability(&commit).map_err(storage)?;
        Ok(
            json!({"artifact":commit,"artifact_availability":availability,
            "files":files.into_iter().map(|(path,size)| json!({"path":path,"size":size})).collect::<Vec<_>>(),
            "truncated":false}),
        )
    }

    pub(crate) fn node_delivery(
        &self,
        run: &str,
        node: &str,
    ) -> Result<CommitRef, ApplicationError> {
        self.channel_file_snapshot(run, node)
            .map_err(storage)?
            .and_then(|snapshot| snapshot.commit)
            .ok_or(ApplicationError::Missing)
    }

    pub(crate) fn open_delivery_file(
        &self,
        id: &str,
        path: &str,
        workspace_root: &Path,
    ) -> Result<ArtifactRead, ArtifactAccessError> {
        if !anchor_runtime::artifact::is_delivery_path(path) {
            return Err(ApplicationError::Invalid("invalid delivery file path".into()).into());
        }
        let artifacts = self.delivery_store(workspace_root);
        let _lease = artifacts.input_lease().map_err(storage)?;
        let commit = artifacts
            .reference(id)
            .map_err(storage)?
            .ok_or(ApplicationError::Missing)?;
        let availability = artifacts.availability(&commit).map_err(storage)?;
        if availability != ArtifactAvailability::Available {
            return Err(ArtifactAccessError::Unavailable(availability));
        }
        if !artifacts
            .list_files(&commit)
            .map_err(storage)?
            .iter()
            .any(|(name, _)| name == path)
        {
            return Err(ApplicationError::Missing.into());
        }
        artifacts
            .open_file(&commit, path)
            .map_err(storage)
            .map_err(Into::into)
    }

    pub(crate) fn retain_delivery(
        &self,
        id: &str,
        retained: bool,
        workspace_root: &Path,
    ) -> Result<Value, ArtifactAccessError> {
        let runs = self.kernel_runs(true)?.expect("Kernel store initialized");
        let _facts = runs.file_facts.lock().unwrap();
        let artifacts = self.delivery_store(workspace_root);
        let _lease = artifacts.input_lease().map_err(storage)?;
        let commit = artifacts
            .reference(id)
            .map_err(storage)?
            .ok_or(ApplicationError::Missing)?;
        let availability = artifacts.availability(&commit).map_err(storage)?;
        if retained && availability != ArtifactAvailability::Available {
            return Err(ArtifactAccessError::Unavailable(availability));
        }
        let fact = json!({"artifact":commit,"retained":retained});
        runs.put_file_fact(&format!("retention/{id}"), fact.clone())?;
        Ok(fact)
    }

    fn protected_deliveries(
        &self,
        runs: &KernelRuns,
        artifacts: &HostArtifacts,
    ) -> Result<BTreeSet<String>, ApplicationError> {
        let mut protected = BTreeSet::new();
        for id in runs
            .kernel
            .store()
            .list("files/retention/")
            .map_err(storage)?
        {
            if let Some(resource) = runs.kernel.load(&id).map_err(storage)?.filter(|resource| {
                resource.validator == FILE_VALIDATOR && !resource.state.is_null()
            }) {
                let fact: ArtifactRetention =
                    serde_json::from_value(resource.state).map_err(storage)?;
                if fact.retained {
                    protected.insert(fact.artifact.id);
                }
            }
        }
        let inventory = artifacts.inventory().map_err(storage)?;
        for (run_id, view) in self.run_views()? {
            let pending_delivery = self.metadata(&run_id)?.is_some_and(|metadata| {
                metadata
                    .session_call
                    .is_some_and(|call| call.status == "pending")
            });
            if view.is_settled() && !view.active() && !pending_delivery {
                continue;
            }
            // Retain current node outputs and only their visible inputs. Old
            // versions shadowed in provenance keep metadata, not content bytes.
            for result in view
                .progress()
                .results
                .values()
                .filter_map(|results| results.last())
            {
                protected.insert(result.commit.id.clone());
                protected.extend(
                    artifacts
                        .visible_inputs(std::slice::from_ref(&result.commit))
                        .map_err(storage)?
                        .into_iter()
                        .map(|commit| commit.id),
                );
            }
            for dispatch in &view.frontier {
                protected.extend(
                    artifacts
                        .visible_inputs(&dispatch.input_commits)
                        .map_err(storage)?
                        .into_iter()
                        .map(|commit| commit.id),
                );
                // Covers publication before the corresponding Kernel result.
                protected.extend(
                    inventory
                        .iter()
                        .filter(|entry| {
                            entry.run_id == run_id
                                && entry.commit.node_id == dispatch.key.node_id
                                && entry.commit.invocation == dispatch.key.invocation
                        })
                        .map(|entry| entry.commit.id.clone()),
                );
            }
        }
        Ok(protected)
    }

    pub(in crate::application) fn release_run_deliveries(
        &self,
        run: &str,
        workspace_root: &Path,
    ) -> Result<ArtifactGcReport, ApplicationError> {
        let runs = self.kernel_runs(true)?.expect("Kernel store initialized");
        let _facts = runs.file_facts.lock().unwrap();
        let artifacts = self.delivery_store(workspace_root);
        let protected = self.protected_deliveries(&runs, &artifacts)?;
        let expire = artifacts
            .inventory()
            .map_err(storage)?
            .into_iter()
            .filter(|entry| entry.run_id == run)
            .map(|entry| entry.commit.id)
            .collect::<Vec<_>>();
        artifacts.collect(&protected, &expire).map_err(storage)
    }

    pub(in crate::application) fn reclaim_deliveries(
        &self,
        policy: &crate::retention::Policy,
        workspace_root: &Path,
        now: i64,
    ) -> Result<ArtifactGcReport, ApplicationError> {
        let runs = self.kernel_runs(true)?.expect("Kernel store initialized");
        let _facts = runs.file_facts.lock().unwrap();
        let artifacts = self.delivery_store(workspace_root);
        let protected = self.protected_deliveries(&runs, &artifacts)?;
        let inventory = artifacts.inventory().map_err(storage)?;
        let candidates = inventory
            .iter()
            .filter(|entry| {
                entry.availability == ArtifactAvailability::Available
                    && !protected.contains(&entry.commit.id)
            })
            .collect::<Vec<_>>();
        let expire = candidates
            .iter()
            .filter(|entry| {
                policy.max_age.is_some_and(|age| {
                    now.saturating_sub(entry.published_at) > age.as_secs() as i64
                })
            })
            .map(|entry| entry.commit.id.clone())
            .collect::<Vec<_>>();
        let mut report = artifacts.collect(&protected, &expire).map_err(storage)?;
        for entry in candidates {
            if policy
                .max_bytes
                .is_none_or(|limit| report.remaining_bytes <= limit)
            {
                break;
            }
            if report.expired.contains(&entry.commit.id) {
                continue;
            }
            let collected = artifacts
                .collect(&protected, std::slice::from_ref(&entry.commit.id))
                .map_err(storage)?;
            report.expired.extend(collected.expired);
            report.removed_objects += collected.removed_objects;
            report.freed_bytes += collected.freed_bytes;
            report.remaining_bytes = collected.remaining_bytes;
        }
        Ok(report)
    }
}
