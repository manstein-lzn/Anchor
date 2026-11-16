//! Independently grade an ordinary immutable output against a registered suite.
use super::*;
use anchor_kernel::{Outcome, Phase};
use serde::Serialize;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrdinaryResult {
    pub schema_version: u32,
    pub run: String,
    pub node: String,
    pub artifact: ArtifactRef,
    pub config_digest: String,
    pub grade_ref: FactRef,
    pub passed: bool,
}

impl Evaluator {
    pub fn grade_ordinary(&self, run: &str, node: &str) -> Result<OrdinaryResult, String> {
        if self.suite.id == "dreaming" {
            return Err("retired Dreaming suites cannot issue ordinary Grades".into());
        }
        let assessment = assess(
            &self.state,
            &self.artifacts,
            &self.runs,
            &self.suite,
            &self.package_digest,
            run,
            node,
        )?;
        Ok(OrdinaryResult {
            schema_version: 1,
            run: run.into(),
            node: node.into(),
            artifact: assessment.artifact,
            grade_ref: self
                .recorder
                .record_exact(&assessment.grade, &assessment.config_digest)?,
            config_digest: assessment.config_digest,
            passed: assessment.grade.passed(),
        })
    }
}

pub(super) struct Assessment {
    pub(super) grade: Grade,
    pub(super) artifact: ArtifactRef,
    pub(super) config_digest: String,
}

pub(super) fn assess(
    state: &Path,
    artifacts: &Path,
    runs: &HistoryReader,
    suite: &ProtectedSuite,
    package_digest: &str,
    run: &str,
    node: &str,
) -> Result<Assessment, String> {
    if !name(run) || !name(node) {
        return Err("invalid ordinary grading identity".into());
    }
    let resource = runs
        .load(&validator::run_resource_id(run))
        .map_err(|error| error.to_string())?
        .ok_or("ordinary Run missing")?;
    if resource.validator != validator::GRAPH_VALIDATOR
        || resource.phase()
            != Some(&Phase::Settled {
                result: Outcome::Done,
            })
    {
        return Err("ordinary grading requires a completed actual Run".into());
    }
    let record = validator::record_of(&resource).map_err(|error| error.to_string())?;
    let metadata: Value = serde_json::from_slice(&read_bounded(
        &state.join("run-metadata").join(format!("{run}.json")),
        FRAME_BYTES,
    )?)
    .map_err(|_| "invalid ordinary Run metadata")?;
    if metadata["run_id"] != run
        || metadata["graph_digest"] != record.graph_digest
        || metadata["graph"] == "dreaming"
        || !metadata["experiment"].is_null()
        || !metadata["dreaming"].is_null()
    {
        return Err("registered ordinary Grade cannot label an experiment or Dreaming Run".into());
    }
    let case = suite
        .cases
        .iter()
        .find(|case| case.input == record.input)
        .ok_or("ordinary input is not a registered protected case")?;
    let result = record
        .results
        .get(node)
        .and_then(|items| items.iter().find(|item| item.key.invocation == 1))
        .ok_or("ordinary output missing")?;
    let key = InvocationKey {
        run_id: run.into(),
        graph_digest: record.graph_digest.clone(),
        node_id: node.into(),
        invocation: 1,
    };
    let reader = ArtifactStoreReader::open(artifacts).map_err(|error| error.to_string())?;
    let artifact = owned_output(artifacts, result, &key)?;
    let output = artifact_json::<Value>(
        &reader,
        &artifact_ref(&result.commit),
        suite.deliverable_path(),
        FRAME_BYTES,
    );
    let passed = output.as_ref().is_ok_and(|output| {
        anchor_facts::canonical(output) == anchor_facts::canonical(&case.expected)
    });
    let grade = Grade {
        version: anchor_facts::VERSION,
        run_id: run.into(),
        task: format!("{}:ordinary", suite.id),
        task_version: suite.version,
        checks: vec![Check {
            name: "result_json".into(),
            passed,
        }],
    };
    let config_digest = digest(
        &json!({"schema_version":1,"kind":"registered-ordinary-result",
        "package_digest":package_digest,"node":node,"invocation":1}),
    )?;
    accepted_run(runs, &resource)?;
    Ok(Assessment {
        grade,
        artifact,
        config_digest,
    })
}
