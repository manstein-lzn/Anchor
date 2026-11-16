use super::*;
use serde_json::json;

#[test]
fn ordinary_metadata_and_historical_dreaming_provenance_remain_readable() {
    let root = tempfile::tempdir().unwrap();
    let old = json!({
        "format":2,"run_id":"old","graph":"fixture","graph_digest":"digest",
        "bundle_source":root.path(),"created":"2026-11-11T00:00:00Z","trigger_source":"manual"
    });
    let mut metadata: RunMetadata = serde_json::from_value(old).unwrap();
    assert!(metadata.dreaming.is_none());
    assert!(metadata.experiment.is_none());
    assert!(!metadata.is_retired_automation());
    save(root.path(), &metadata).unwrap();
    let loaded = load(root.path(), "old").unwrap().unwrap();
    assert!(loaded.dreaming.is_none());
    assert!(
        serde_json::to_value(loaded)
            .unwrap()
            .get("dreaming")
            .is_none()
    );

    let context = DreamingRunContext {
        schema_version: 1,
        kind: DreamingRunKind::Cycle,
        cycle: format!("cycle/{}", anchor_facts::text_digest("fixture")),
        managed_source: "source-proof".into(),
    };
    metadata.run_id = "new".into();
    metadata.dreaming = Some(context.clone());
    save(root.path(), &metadata).unwrap();
    let loaded = load(root.path(), "new").unwrap().unwrap();
    assert_eq!(loaded.format, 2);
    assert_eq!(loaded.dreaming, Some(context));
    assert!(loaded.is_retired_automation());
    let mut encoded = serde_json::to_value(loaded).unwrap();
    encoded["dreaming"]["kind"] = json!("unrecognized");
    assert!(serde_json::from_value::<RunMetadata>(encoded).is_err());
}

#[test]
fn experiment_identity_roundtrips_without_granting_other_run_authority() {
    let root = tempfile::tempdir().unwrap();
    let snapshot_digest = anchor_facts::text_digest("snapshot");
    let mut metadata = RunMetadata::new(
        "experiment-run".into(),
        "target".into(),
        snapshot_digest.clone(),
        root.path(),
    )
    .unwrap();
    let encoded = serde_json::to_value(&metadata).unwrap();
    assert!(encoded.get("experiment").is_none());
    metadata.request = Some(AdmissionRequest {
        id: "experiment-request".into(),
        content_digest: anchor_facts::text_digest("input and context"),
    });
    metadata.experiment = Some(ExperimentRunContext {
        schema_version: 1,
        experiment: "experiment".into(),
        assignment: "candidate-one".into(),
        manifest_digest: anchor_facts::text_digest("manifest"),
        snapshot_digest,
        input_digest: anchor_facts::text_digest("input"),
        execution_closure_digest: None,
    });
    save(root.path(), &metadata).unwrap();
    let loaded = load(root.path(), &metadata.run_id).unwrap().unwrap();
    assert_eq!(loaded.experiment, metadata.experiment);
    assert!(loaded.is_retired_automation());
    assert!(loaded.dreaming.is_none());
    metadata
        .experiment
        .as_mut()
        .unwrap()
        .execution_closure_digest = Some(anchor_facts::text_digest("closed runtime"));
    save(root.path(), &metadata).unwrap();
    assert_eq!(
        load(root.path(), &metadata.run_id)
            .unwrap()
            .unwrap()
            .experiment,
        metadata.experiment
    );
    let valid = serde_json::to_value(&metadata).unwrap();
    for (field, value) in [
        (
            "dreaming",
            json!({"schema_version":1,"kind":"cycle","cycle":"cycle","managed_source":"proof"}),
        ),
        (
            "assistant",
            json!({"owner":"owner","session":"session","wait_node":"wait","work_node":"worker","reply_node":"reply"}),
        ),
        (
            "pilot",
            json!({"owner":"owner","session":"session","turn":"turn"}),
        ),
        ("request", Value::Null),
        ("trigger_source", json!("schedule")),
        ("oauth_owner", json!("owner")),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        let invalid: RunMetadata = serde_json::from_value(invalid).unwrap();
        save(root.path(), &invalid).unwrap();
        assert!(load(root.path(), &metadata.run_id).is_err(), "{field}");
    }
    for (field, value) in [
        ("schema_version", json!(2)),
        ("experiment", json!("../other")),
        ("assignment", json!("*")),
        ("manifest_digest", json!("unbound")),
        (
            "snapshot_digest",
            json!(anchor_facts::text_digest("different")),
        ),
        ("input_digest", json!("unbound")),
        ("execution_closure_digest", json!("invalid-closure-digest")),
    ] {
        let mut invalid = valid.clone();
        invalid["experiment"][field] = value;
        let invalid: RunMetadata = serde_json::from_value(invalid).unwrap();
        save(root.path(), &invalid).unwrap();
        assert!(load(root.path(), &metadata.run_id).is_err(), "{field}");
    }
    let mut unknown = valid;
    unknown["experiment"]["dreaming"] = json!(true);
    assert!(serde_json::from_value::<RunMetadata>(unknown).is_err());
}
