use super::tests::{completion_with_files, fixture, key};
use super::*;
use anchor_kernel::{FileStore, Outcome, Phase, Store};
use anchor_rsi::evidence::{Config, Evidence};
use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot, RunResult, validator};
use serde_json::json;

#[tokio::test]
async fn terminal_delivery_is_readable_by_rsi_without_a_downstream_mount_and_rejects_expired_views()
{
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let (temp, artifacts) = fixture();
    let snapshot = GraphSnapshot::from_authoring(json!({
        "objective":"weekly review of Anchor",
        "entry":"publish",
        "agents":{"worker":{"model":"models.default","instructions":"publish report"}},
        "nodes":[{"id":"publish","agent":"worker"}],
        "edges":[]
    }))
    .unwrap();
    let mut record = GraphRunRecord::create_with_id(snapshot, Value::Null, "run-1").unwrap();
    let first = InvocationKey {
        graph_digest: record.graph_digest.clone(),
        ..key("publish")
    };
    let workspace = artifacts.prepare_workspace(&first, &[]).unwrap();
    fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
    fs::write(workspace.join(".git/config"), "project Git configuration").unwrap();
    fs::write(workspace.join("rsi-report.md"), "Original draft\n").unwrap();
    fs::write(
        workspace.join("evolution.json"),
        r#"{"proposals":[{"id":"P-before"}]}"#,
    )
    .unwrap();
    fs::write(workspace.join("sources.md"), "Verified source\n").unwrap();
    fs::write(workspace.join("unused.bin"), "Unrelated bytes\n").unwrap();
    let completion = completion_with_files(&[
        "rsi-report.md",
        "evolution.json",
        "sources.md",
        "unused.bin",
    ]);
    let original = artifacts.freeze(&first, &completion).await.unwrap();
    // Terminal publication itself prepares the view; no consumer has mounted it.
    let original_view = artifacts.root.join(&original.id).join("view");
    let descriptor = ArtifactDescriptor::read(&original_view).unwrap();
    assert_eq!(descriptor.artifact, original);
    assert_eq!(
        descriptor.manifest_sha256,
        format!(
            "{:x}",
            Sha256::digest(
                fs::read(artifacts.root.join(&original.id).join("manifest.json")).unwrap()
            )
        )
    );
    assert!(!original_view.join(".git").exists());
    assert!(workspace.join(".git/config").exists());
    let (_, manifest) = artifacts.load_snapshot(&original).unwrap();
    assert_eq!(
        fs::metadata(original_view.join("rsi-report.md"))
            .unwrap()
            .ino(),
        fs::metadata(
            artifacts
                .object_path(&manifest.files["rsi-report.md"])
                .unwrap()
        )
        .unwrap()
        .ino()
    );

    record.results.insert(
        "publish".into(),
        vec![RunResult {
            node_id: "publish".into(),
            key: first.clone(),
            plugin_views: Default::default(),
            plugin_digests: Default::default(),
            completion: completion.clone(),
            interruption: None,
            commit: original.clone(),
            sequence: 1,
        }],
    );
    let mut resource = validator::run_resource(&record, 100).unwrap();
    resource.task.as_mut().unwrap().phase = Phase::Settled {
        result: Outcome::Done,
    };
    FileStore::new(temp.path().join("kernel-runs"))
        .unwrap()
        .create(&resource)
        .unwrap();
    let source = temp.path().join("source");
    let data = temp.path().join("data");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&data).unwrap();
    let mut config = Config {
        source,
        data,
        rust_state: Some(temp.path().into()),
        previous: None,
        evidence: temp.path().join("evidence"),
    };
    // The collector opens the existing store lease read-only and reads only
    // the three report files, even if an unrelated view entry is absent.
    let lock = artifacts.root.join(".store.lock");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o444)).unwrap();
    fs::remove_file(original_view.join("unused.bin")).unwrap();
    let evidence = Evidence::collect(config.clone()).unwrap();
    let previous = format!("previous/published-{}/evolution.json", original.id);
    assert!(
        evidence
            .read(&previous, 0, 100)
            .unwrap()
            .to_string()
            .contains("P-before")
    );
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();

    // Both the descriptor and relevant bytes remain bound to the manifest.
    let descriptor_path = original_view.join(ARTIFACT_DESCRIPTOR_FILE);
    let mut changed = descriptor.clone();
    changed.artifact.invocation += 1;
    fs::remove_file(&descriptor_path).unwrap();
    fs::write(&descriptor_path, serde_json::to_vec(&changed).unwrap()).unwrap();
    config.evidence = temp.path().join("wrong-identity-evidence");
    let invalid = Evidence::collect(config.clone()).unwrap();
    assert!(!invalid.entries.contains_key(&previous));
    assert!(
        invalid
            .issues
            .iter()
            .any(|issue| { issue["reason"] == "published report could not be verified" })
    );
    fs::write(&descriptor_path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
    fs::remove_file(original_view.join("evolution.json")).unwrap();
    fs::write(original_view.join("evolution.json"), "changed report bytes").unwrap();
    config.evidence = temp.path().join("wrong-bytes-evidence");
    let invalid = Evidence::collect(config.clone()).unwrap();
    assert!(!invalid.entries.contains_key(&previous));

    // A retry uses the already-published bytes and can repair a missing view.
    fs::remove_dir_all(&original_view).unwrap();
    assert_eq!(
        artifacts.freeze(&first, &completion).await.unwrap(),
        original
    );
    assert_eq!(
        ArtifactDescriptor::read(&original_view).unwrap(),
        descriptor
    );

    // Simulate collection stopping after its tombstone, before deleting view.
    let expired = artifacts.root.join(&original.id).join("expired.json");
    fs::write(&expired, b"{\"format\":1}").unwrap();
    config.evidence = temp.path().join("expired-evidence");
    let invalid = Evidence::collect(config).unwrap();
    assert!(original_view.is_dir());
    assert!(!invalid.entries.contains_key(&previous));
    fs::remove_file(expired).unwrap();
    let second = InvocationKey {
        invocation: 2,
        ..first
    };
    assert_eq!(
        artifacts
            .prepare_workspace(&second, std::slice::from_ref(&original))
            .unwrap(),
        workspace
    );
    fs::write(workspace.join("rsi-report.md"), "Revised draft\n").unwrap();
    let revised = artifacts
        .freeze_with_context(
            &second,
            &completion,
            &ArtifactFreezeContext {
                kind: ArtifactKind::Node,
                input_commits: vec![original.clone()],
            },
        )
        .await
        .unwrap();
    assert_ne!(original, revised);
    assert_eq!(
        descriptor
            .read_file(&original_view, "rsi-report.md")
            .unwrap(),
        b"Original draft\n"
    );
    let revised_view = artifacts.files_path(&revised).unwrap();
    assert_eq!(
        ArtifactDescriptor::read(&revised_view)
            .unwrap()
            .read_file(&revised_view, "rsi-report.md")
            .unwrap(),
        b"Revised draft\n"
    );
    fs::remove_dir_all(&original_view).unwrap();
    let rebuilt = artifacts.files_path(&original).unwrap();
    assert_eq!(ArtifactDescriptor::read(&rebuilt).unwrap(), descriptor);
    assert_eq!(
        artifacts
            .visible_inputs(std::slice::from_ref(&revised))
            .unwrap(),
        std::slice::from_ref(&revised)
    );
    artifacts
        .collect(
            &std::collections::BTreeSet::from([revised.id.clone()]),
            &[original.id],
        )
        .unwrap();
    assert!(
        artifacts
            .input_mounts(&[revised], "run-1", &record.graph_digest)
            .is_ok()
    );
}
