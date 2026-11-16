//! Independently check immutable ordinary Run output against a registered suite.

pub use anchor_facts::evaluation::*;

use crate::GradeRecorder;
use anchor_facts::{Check, Grade};
use anchor_kernel::{Action, HistoryReader, PrincipalKind, Record, Resource};
use anchor_runtime::{
    artifact::store::{self, ArtifactStoreReader},
    graph::{ArtifactKind, CommitRef, InvocationKey, RunResult, validator},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

mod ordinary;
pub use ordinary::OrdinaryResult;

pub fn artifact_ref(commit: &CommitRef) -> ArtifactRef {
    ArtifactRef {
        id: commit.id.clone(),
        node_id: commit.node_id.clone(),
        invocation: commit.invocation,
    }
}

pub fn runtime_commit(reference: &ArtifactRef) -> CommitRef {
    CommitRef {
        id: reference.id.clone(),
        node_id: reference.node_id.clone(),
        invocation: reference.invocation,
    }
}

pub fn package_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    anchor_file_lease::read_regular(path, maximum).map_err(|error| error.to_string())
}

fn artifact_file(
    reader: &ArtifactStoreReader<'_>,
    commit: &ArtifactRef,
    path: &str,
    maximum: usize,
) -> Result<Vec<u8>, String> {
    let view = reader
        .read_view(&runtime_commit(commit))
        .map_err(|error| error.to_string())?;
    let metadata = view
        .descriptor
        .files
        .get(path)
        .ok_or("required file is absent from the immutable Artifact")?;
    if metadata.bytes > maximum as u64 {
        return Err("evaluation Artifact file exceeds its frame limit".into());
    }
    let bytes = view.read_file(path)?;
    if bytes.len() > maximum {
        return Err("evaluation Artifact file exceeds its frame limit".into());
    }
    Ok(bytes)
}

fn artifact_json<T: for<'de> Deserialize<'de>>(
    reader: &ArtifactStoreReader<'_>,
    commit: &ArtifactRef,
    path: &str,
    maximum: usize,
) -> Result<T, String> {
    serde_json::from_slice(&artifact_file(reader, commit, path, maximum)?)
        .map_err(|_| "invalid JSON in immutable evaluation input".into())
}

fn owned_output(
    artifacts: &Path,
    result: &RunResult,
    key: &InvocationKey,
) -> Result<ArtifactRef, String> {
    if result.key != *key
        || result.node_id != key.node_id
        || result.interruption.is_some()
        || result.commit != store::commit_for(key)
    {
        return Err("output result is not the registered completed invocation".into());
    }
    let (_, manifest) =
        store::read_snapshot(artifacts, &result.commit).map_err(|error| error.to_string())?;
    if manifest.key != *key
        || manifest.completion != result.completion
        || manifest
            .context
            .as_ref()
            .is_none_or(|context| context.kind != ArtifactKind::Node)
    {
        return Err("output Artifact does not belong to the actual completed node".into());
    }
    Ok(artifact_ref(&result.commit))
}

fn accepted_run(reader: &HistoryReader, resource: &Resource) -> Result<FactRef, String> {
    let history = reader
        .history(&resource.id)
        .map_err(|error| error.to_string())?;
    let (index, record) = history
        .iter()
        .enumerate()
        .rev()
        .find(|(_, record)| record.verdict.is_accepted() && record.after.as_ref() == Some(resource))
        .ok_or("Run has no exact accepted history record")?;
    FactRef::at(&resource.id, index, record)
}

/// Registered roots are process-start facts, not caller-controlled grading data.
pub struct Evaluator {
    recorder: GradeRecorder,
    state: PathBuf,
    artifacts: PathBuf,
    runs: HistoryReader,
    suite: ProtectedSuite,
    package_digest: String,
}

impl Evaluator {
    pub fn open(
        state: &Path,
        artifacts: &Path,
        runs: &Path,
        suite_path: &Path,
        expected_package_digest: &str,
    ) -> Result<Self, String> {
        for root in [state, artifacts, runs] {
            store::require_directory(root).map_err(|error| error.to_string())?;
        }
        let suite_bytes = read_bounded(suite_path, SUITE_BYTES)?;
        let package_digest = package_digest(&suite_bytes);
        if !anchor_facts::window::is_digest(expected_package_digest)
            || package_digest != expected_package_digest
        {
            return Err("protected evaluator package digest mismatch".into());
        }
        let suite: ProtectedSuite =
            serde_json::from_slice(&suite_bytes).map_err(|_| "invalid protected suite JSON")?;
        suite.validate()?;
        Ok(Self {
            recorder: GradeRecorder::open(state)?,
            state: state.canonicalize().map_err(|error| error.to_string())?,
            artifacts: artifacts
                .canonicalize()
                .map_err(|error| error.to_string())?,
            runs: HistoryReader::open(runs.canonicalize().map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?,
            suite,
            package_digest,
        })
    }
}

impl GradeRecorder {
    fn record_exact(&self, grade: &Grade, config: &str) -> Result<FactRef, String> {
        let resource = anchor_facts::grade_id(&grade.run_id);
        let matching = || -> Result<Vec<(usize, Record)>, String> {
            Ok(self
                .kernel
                .history(&resource)
                .map_err(|error| error.to_string())?
                .into_iter()
                .enumerate()
                .filter(|(_, record)| {
                    record.verdict.is_accepted() && record.stamp.config() == Some(config)
                })
                .collect())
        };
        let mut matches = matching()?;
        let value = serde_json::to_value(grade).map_err(|error| error.to_string())?;
        if matches.iter().any(|(_, record)| {
            record.submission.content != value
                || record.stamp.principal().id != anchor_facts::GRADER_ID
                || record.stamp.principal().kind != PrincipalKind::Code
                || record.submission.target != resource
                || record.submission.action != Action::Write
                || record
                    .after
                    .as_ref()
                    .is_none_or(|after| after.validator != anchor_facts::GRADE)
        }) {
            return Err("grade configuration identity conflicts with recorded content".into());
        }
        if matches.is_empty() {
            let verdict = self
                .record(grade, config)
                .map_err(|error| error.to_string())?;
            if !verdict.is_accepted() {
                return Err("deterministic Grade was not accepted".into());
            }
            matches = matching()?;
        }
        let (index, record) = matches.first().ok_or("accepted Grade is unavailable")?;
        FactRef::at(&resource, *index, record)
    }
}

#[cfg(test)]
mod tests;
