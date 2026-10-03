use super::tests::{completion, fixture, key, workspace};
use super::*;
use serde_json::json;

async fn freeze(
    artifacts: &HostArtifacts,
    node: &str,
    kind: ArtifactKind,
    parents: &[CommitRef],
) -> CommitRef {
    let key = key(node);
    if kind == ArtifactKind::Node {
        workspace(artifacts, &key);
    }
    artifacts
        .freeze_with_context(
            &key,
            &completion(),
            &ArtifactFreezeContext {
                kind,
                input_commits: parents.to_vec(),
            },
        )
        .await
        .unwrap()
}

fn destinations(mounts: &[ReadOnlyInput]) -> Vec<String> {
    mounts
        .iter()
        .map(|mount| mount.destination.to_string_lossy().into_owned())
        .collect()
}

#[tokio::test]
async fn serial_fanout_branches_join_forward_exact_files_and_control_json_without_workspace() {
    let (_temp, artifacts) = fixture();
    let producer = freeze(&artifacts, "producer", ArtifactKind::Node, &[]).await;
    let fanout = freeze(
        &artifacts,
        "fanout",
        ArtifactKind::Fanout,
        std::slice::from_ref(&producer),
    )
    .await;
    assert!(!artifacts.workspace_path(&key("fanout")).unwrap().exists());
    let inputs = artifacts
        .input_mounts(std::slice::from_ref(&fanout), "run-1", "digest-1")
        .unwrap();
    assert_eq!(destinations(&inputs), ["/in/fanout", "/in/producer"]);
    assert_eq!(
        fs::read_to_string(inputs[1].source.join("nested/report.txt")).unwrap(),
        "original"
    );
    let left = freeze(
        &artifacts,
        "left",
        ArtifactKind::Node,
        std::slice::from_ref(&fanout),
    )
    .await;
    let right = freeze(
        &artifacts,
        "right",
        ArtifactKind::Node,
        std::slice::from_ref(&fanout),
    )
    .await;
    let join = freeze(
        &artifacts,
        "join",
        ArtifactKind::Join,
        &[left.clone(), right.clone()],
    )
    .await;
    assert!(!artifacts.workspace_path(&key("join")).unwrap().exists());
    let inputs = artifacts
        .input_mounts(std::slice::from_ref(&join), "run-1", "digest-1")
        .unwrap();
    assert_eq!(
        destinations(&inputs),
        [
            "/in/join",
            "/in/left",
            "/in/right",
            "/in/fanout",
            "/in/producer"
        ]
    );
    for input in &inputs {
        if input.destination == Path::new("/in/fanout")
            || input.destination == Path::new("/in/join")
        {
            let filename = if input.destination == Path::new("/in/fanout") {
                "fanout.json"
            } else {
                "join.json"
            };
            let control: Value =
                serde_json::from_slice(&fs::read(input.source.join(filename)).unwrap()).unwrap();
            assert_eq!(control, completion().output);
        } else {
            assert_eq!(
                fs::read_to_string(input.source.join("nested/report.txt")).unwrap(),
                "original"
            );
        }
    }
    let downstream = freeze(
        &artifacts,
        "downstream",
        ArtifactKind::Node,
        std::slice::from_ref(&join),
    )
    .await;
    assert_eq!(
        destinations(
            &artifacts
                .input_mounts(&[downstream], "run-1", "digest-1")
                .unwrap()
        )
        .len(),
        6
    );
    let manifest = artifacts.load_snapshot(&join).unwrap().1;
    assert_eq!(manifest.format, 2);
    assert_eq!(manifest.context.unwrap().input_commits, [left, right]);
}

#[tokio::test]
async fn completion_output_cannot_forge_file_provenance_and_normal_node_requires_workspace() {
    let (_temp, artifacts) = fixture();
    let secret = freeze(&artifacts, "unrelated", ArtifactKind::Node, &[]).await;
    let node = key("work");
    let forged = NodeCompletion {
        output: json!({"input_commits":[secret],"branches":[{"commit":secret}]}),
        ..completion()
    };
    assert!(
        artifacts
            .freeze_with_context(
                &node,
                &forged,
                &ArtifactFreezeContext {
                    kind: ArtifactKind::Node,
                    input_commits: vec![]
                }
            )
            .await
            .is_err()
    );
    workspace(&artifacts, &node);
    let commit = artifacts.freeze(&node, &forged).await.unwrap();
    assert_eq!(
        destinations(
            &artifacts
                .input_mounts(&[commit], "run-1", "digest-1")
                .unwrap()
        ),
        ["/in/work"]
    );
}

#[tokio::test]
async fn nearest_loop_ancestor_wins_and_equal_depth_different_versions_fail_closed() {
    let (_temp, artifacts) = fixture();
    let old_key = key("loop");
    workspace(&artifacts, &old_key);
    let old = artifacts.freeze(&old_key, &completion()).await.unwrap();
    let bridge = freeze(
        &artifacts,
        "bridge",
        ArtifactKind::Node,
        std::slice::from_ref(&old),
    )
    .await;
    let new_key = InvocationKey {
        invocation: 2,
        ..old_key
    };
    let new_path = workspace(&artifacts, &new_key);
    fs::write(new_path.join("nested/report.txt"), "latest").unwrap();
    let new = artifacts
        .freeze_with_context(
            &new_key,
            &completion(),
            &ArtifactFreezeContext {
                kind: ArtifactKind::Node,
                input_commits: vec![bridge],
            },
        )
        .await
        .unwrap();
    let inputs = artifacts
        .input_mounts(std::slice::from_ref(&new), "run-1", "digest-1")
        .unwrap();
    assert_eq!(destinations(&inputs), ["/in/loop", "/in/bridge"]);
    assert_eq!(
        fs::read_to_string(inputs[0].source.join("nested/report.txt")).unwrap(),
        "latest"
    );
    assert!(
        artifacts
            .input_mounts(&[new, old], "run-1", "digest-1")
            .is_err()
    );
}

fn rewrite_context(
    artifacts: &HostArtifacts,
    commit: &CommitRef,
    parents: Vec<CommitRef>,
    refresh_checksum: bool,
) {
    let path = artifacts.root.join(&commit.id).join("manifest.json");
    let mut manifest: Manifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest.context.as_mut().unwrap().input_commits = parents;
    if refresh_checksum {
        manifest.context_sha256 = Some(context_hash(manifest.context.as_ref().unwrap()).unwrap());
    }
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

#[tokio::test]
async fn corrupt_missing_cross_run_digest_and_cyclic_links_are_rejected() {
    for corruption in ["checksum", "missing", "cross-run", "cross-graph", "cycle"] {
        let (_temp, artifacts) = fixture();
        let first = freeze(&artifacts, "first", ArtifactKind::Node, &[]).await;
        let last = freeze(
            &artifacts,
            "last",
            ArtifactKind::Join,
            std::slice::from_ref(&first),
        )
        .await;
        let parent = match corruption {
            "missing" => CommitRef {
                id: format!("fs2-{}", "0".repeat(64)),
                ..first.clone()
            },
            "cross-run" | "cross-graph" => {
                let mut foreign_key = key("foreign");
                if corruption == "cross-run" {
                    foreign_key.run_id = "other-run".into();
                } else {
                    foreign_key.graph_digest = "other-graph".into();
                }
                workspace(&artifacts, &foreign_key);
                artifacts.freeze(&foreign_key, &completion()).await.unwrap()
            }
            "cycle" | "checksum" => last.clone(),
            _ => first,
        };
        rewrite_context(&artifacts, &last, vec![parent], corruption != "checksum");
        assert!(
            artifacts
                .input_mounts(std::slice::from_ref(&last), "run-1", "digest-1")
                .is_err(),
            "{corruption}"
        );
        assert!(artifacts.files_path(&last).is_err(), "{corruption}");
        assert!(artifacts.resolve(&last).await.is_err(), "{corruption}");
    }
}

#[tokio::test]
async fn context_retry_is_immutable_and_old_fs1_remains_readable() {
    let (_temp, artifacts) = fixture();
    let parent = freeze(&artifacts, "parent", ArtifactKind::Node, &[]).await;
    let context = ArtifactFreezeContext {
        kind: ArtifactKind::Join,
        input_commits: vec![parent.clone()],
    };
    let node_key = key("join");
    let commit = artifacts
        .freeze_with_context(&node_key, &completion(), &context)
        .await
        .unwrap();
    assert_eq!(
        artifacts
            .freeze_with_context(&node_key, &completion(), &context)
            .await
            .unwrap(),
        commit
    );
    let changed = ArtifactFreezeContext {
        kind: ArtifactKind::Join,
        input_commits: vec![],
    };
    assert!(matches!(
        artifacts
            .freeze_with_context(&node_key, &completion(), &changed)
            .await,
        Err(GraphError::RunConflict)
    ));

    let (path, mut manifest) = artifacts.load_snapshot(&parent).unwrap();
    manifest.format = 1;
    manifest.context = None;
    manifest.context_sha256 = None;
    fs::write(
        path.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let legacy = legacy_commit_for(&manifest.key);
    fs::rename(path, artifacts.root.join(&legacy.id)).unwrap();
    fs::remove_dir_all(artifacts.workspace_path(&manifest.key).unwrap()).unwrap();
    assert_eq!(
        artifacts.resolve(&legacy).await.unwrap(),
        serde_json::to_value(completion()).unwrap()
    );
    assert_eq!(
        artifacts
            .freeze(&manifest.key, &completion())
            .await
            .unwrap(),
        legacy
    );
    assert_eq!(
        destinations(
            &artifacts
                .input_mounts(&[legacy], "run-1", "digest-1")
                .unwrap()
        ),
        ["/in/parent"]
    );
}
