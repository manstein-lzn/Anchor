use super::*;
use anchor_kernel::{Authority, FileStore, Kernel, Principal, Submission};
use anchor_runtime::{
    artifact::{ArtifactFile, store::Manifest},
    graph::{
        ArtifactFreezeContext, GraphRunRecord, GraphSnapshot, NodeCompletion,
        validator::{GraphValidator, NodeSubmission},
    },
};
use std::{collections::BTreeMap, fs, sync::Arc};

struct Allowed;
impl Authority for Allowed {
    fn permits(&self, _: &Principal, _: Action, _: &str) -> bool {
        true
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    suite: ProtectedSuite,
    kernel: Kernel<FileStore>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("artifacts")).unwrap();
        fs::File::create(directory.path().join("artifacts/.store.lock")).unwrap();
        fs::create_dir(directory.path().join("run-metadata")).unwrap();
        let suite = serde_json::from_slice(include_bytes!(
            "../../../../resources/evaluations/input-discovery/manifest.json"
        ))
        .unwrap();
        let mut kernel = Kernel::new(
            FileStore::new(directory.path().join("kernel-runs")).unwrap(),
            Box::new(Allowed),
        )
        .unwrap();
        kernel
            .install(validator::GRAPH_VALIDATOR, Arc::new(GraphValidator))
            .unwrap();
        Self {
            directory,
            suite,
            kernel,
        }
    }

    fn evaluator(&self) -> Evaluator {
        let path = self.directory.path().join("suite.json");
        let bytes = serde_json::to_vec(&self.suite).unwrap();
        fs::write(&path, &bytes).unwrap();
        Evaluator::open(
            self.directory.path(),
            &self.directory.path().join("artifacts"),
            &self.directory.path().join("kernel-runs"),
            &path,
            &package_digest(&bytes),
        )
        .unwrap()
    }

    fn complete(&self, case: usize, bytes: Option<Vec<u8>>) -> ArtifactRef {
        let snapshot = GraphSnapshot::from_authoring(json!({
            "objective":"produce a structured result", "entry":"worker",
            "agents":{"worker":{"model":"local", "instructions":"inspect delivered files"}},
            "nodes":[{"id":"worker","agent":"worker"}], "edges":[],
        }))
        .unwrap();
        let record = GraphRunRecord::create_with_id(
            snapshot,
            self.suite.cases[case].input.clone(),
            format!("run-{case}"),
        )
        .unwrap();
        let resource = validator::run_resource(&record, 100).unwrap();
        self.kernel.create(resource.clone()).unwrap();
        let key = InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: "worker".into(),
            invocation: 1,
        };
        let commit = store::commit_for(&key);
        let root = self.directory.path().join("artifacts").join(&commit.id);
        let view = root.join("view");
        fs::create_dir_all(&view).unwrap();
        let file = self.suite.deliverable_path();
        let files = bytes
            .map(|bytes| {
                fs::write(view.join(file), &bytes).unwrap();
                BTreeMap::from([(
                    file.to_owned(),
                    ArtifactFile {
                        sha256: package_digest(&bytes),
                        bytes: bytes.len() as u64,
                        executable: false,
                    },
                )])
            })
            .unwrap_or_default();
        let completion = NodeCompletion {
            submission: "finished".into(),
            route: None,
            model_requests: 1,
            output: json!({"files":files.keys().collect::<Vec<_>>()}),
        };
        let context = ArtifactFreezeContext {
            kind: ArtifactKind::Node,
            input_commits: vec![],
        };
        let manifest = Manifest {
            format: 3,
            key,
            completion: completion.clone(),
            files,
            directories: vec![],
            context_sha256: Some(store::context_hash(&context).unwrap()),
            context: Some(context),
        };
        fs::write(
            root.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            view.join(anchor_runtime::artifact::ARTIFACT_DESCRIPTOR_FILE),
            serde_json::to_vec(&manifest.descriptor(&root).unwrap()).unwrap(),
        )
        .unwrap();
        let submission = NodeSubmission {
            summary: completion.submission,
            interruption: None,
            route: completion.route,
            model_requests: completion.model_requests,
            output: completion.output,
            commit: commit.clone(),
            plugin_views: BTreeMap::new(),
            plugin_digests: BTreeMap::new(),
        };
        assert!(
            self.kernel
                .submit(
                    &Principal::new("worker", PrincipalKind::Agent),
                    0,
                    Submission::new(&resource.id, 0, Action::Write)
                        .slot("worker#1")
                        .content(serde_json::to_value(submission).unwrap()),
                )
                .unwrap()
                .is_accepted()
        );
        fs::write(
            self.metadata_path(case),
            serde_json::to_vec(&json!({"run_id":record.run_id,
                "graph":"input-discovery", "graph_digest":record.graph_digest}))
            .unwrap(),
        )
        .unwrap();
        artifact_ref(&commit)
    }

    fn metadata_path(&self, case: usize) -> PathBuf {
        self.directory
            .path()
            .join(format!("run-metadata/run-{case}.json"))
    }
}

#[test]
fn all_registered_input_discovery_cases_grade_exact_outputs_and_recover_once() {
    let fixture = Fixture::new();
    let evaluator = fixture.evaluator();
    let mut results = Vec::new();
    for (case, definition) in fixture.suite.cases.iter().enumerate() {
        let artifact = fixture.complete(
            case,
            Some(serde_json::to_vec(&definition.expected).unwrap()),
        );
        let result = evaluator
            .grade_ordinary(&format!("run-{case}"), "worker")
            .unwrap();
        assert!(result.passed, "{}", definition.id);
        assert_eq!(result.artifact, artifact);
        assert_eq!(result.grade_ref.place, 1);
        assert_eq!(
            result.config_digest,
            digest(&json!({"schema_version":1,
            "kind":"registered-ordinary-result","package_digest":evaluator.package_digest,
            "node":"worker","invocation":1}))
            .unwrap()
        );
        results.push(result);
    }
    drop(evaluator);
    let evaluator = fixture.evaluator();
    for original in results {
        let retried = evaluator.grade_ordinary(&original.run, "worker").unwrap();
        assert_eq!(retried.grade_ref, original.grade_ref);
        let history = evaluator
            .recorder
            .kernel
            .history(&original.grade_ref.resource)
            .unwrap();
        assert_eq!(history.len(), 1);
        original.grade_ref.verify(&history[0]).unwrap();
        let mut conflicting: Grade =
            serde_json::from_value(history[0].submission.content.clone()).unwrap();
        conflicting.checks[0].passed = false;
        assert!(
            evaluator
                .recorder
                .record_exact(&conflicting, &original.config_digest)
                .is_err()
        );
    }
}

#[test]
fn absent_malformed_oversized_and_false_pass_outputs_fail_without_becoming_null_answers() {
    for bytes in [
        None,
        Some(b"PASS".to_vec()),
        Some(vec![b'x'; FRAME_BYTES + 1]),
        Some(br#"{"passed":true}"#.to_vec()),
        Some(b"null".to_vec()),
    ] {
        let fixture = Fixture::new();
        fixture.complete(0, bytes);
        assert!(
            !fixture
                .evaluator()
                .grade_ordinary("run-0", "worker")
                .unwrap()
                .passed
        );
    }
    let mut fixture = Fixture::new();
    fixture.suite.cases[0].expected = Value::Null;
    fixture.suite.output_file = Some("answer.json".into());
    fixture.complete(0, Some(b"null".to_vec()));
    assert!(
        fixture
            .evaluator()
            .grade_ordinary("run-0", "worker")
            .unwrap()
            .passed
    );
}

#[test]
fn ordinary_grading_refuses_historical_experiment_and_dreaming_identity_without_writing_grades() {
    let fixture = Fixture::new();
    fixture.complete(
        0,
        Some(serde_json::to_vec(&fixture.suite.cases[0].expected).unwrap()),
    );
    let path = fixture.metadata_path(0);
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let evaluator = fixture.evaluator();
    for (field, value) in [
        (
            "experiment",
            json!({"schema_version":1,"experiment":"retained"}),
        ),
        ("dreaming", json!({"kind":"cycle","cycle":"retained"})),
        ("graph", json!("dreaming")),
        ("graph_digest", json!("f".repeat(64))),
        ("run_id", json!("another-run")),
    ] {
        let mut metadata = original.clone();
        metadata[field] = value;
        fs::write(&path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        assert!(
            evaluator.grade_ordinary("run-0", "worker").is_err(),
            "{field}"
        );
    }
    assert!(
        evaluator
            .recorder
            .kernel
            .history("grade/run-0")
            .unwrap()
            .is_empty()
    );
    drop(evaluator);
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    let mut evaluator = fixture.evaluator();
    evaluator.suite.id = "dreaming".into();
    for version in [1, 2, 3, 4] {
        evaluator.suite.version = version;
        assert!(evaluator.grade_ordinary("run-0", "worker").is_err());
    }
    assert!(
        evaluator
            .recorder
            .kernel
            .history("grade/run-0")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn registered_package_and_immutable_output_identity_are_required() {
    let fixture = Fixture::new();
    let artifact = fixture.complete(
        0,
        Some(serde_json::to_vec(&fixture.suite.cases[0].expected).unwrap()),
    );
    let evaluator = fixture.evaluator();
    fs::write(
        fixture
            .directory
            .path()
            .join("artifacts")
            .join(artifact.id)
            .join("view/result.json"),
        br#"{"status":"PASS"}"#,
    )
    .unwrap();
    assert!(!evaluator.grade_ordinary("run-0", "worker").unwrap().passed);
    assert!(evaluator.grade_ordinary("../run-0", "worker").is_err());
    assert!(evaluator.grade_ordinary("run-0", "another-node").is_err());
    drop(evaluator);
    let path = fixture.directory.path().join("suite.json");
    let link = fixture.directory.path().join("linked-suite.json");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let actual_digest = package_digest(&fs::read(&path).unwrap());
    for (suite_path, package) in [(&path, "f".repeat(64)), (&link, actual_digest)] {
        assert!(
            Evaluator::open(
                fixture.directory.path(),
                &fixture.directory.path().join("artifacts"),
                &fixture.directory.path().join("kernel-runs"),
                suite_path,
                &package,
            )
            .is_err()
        );
    }
}
