use super::tests::{completion, fixture, key, workspace};
use super::*;
use anchor_runtime::graph::CallFileSelection;
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

async fn committed_parent(artifacts: &HostArtifacts, node: &str) -> CommitRef {
    let parent = key(node);
    workspace(artifacts, &parent);
    artifacts.freeze(&parent, &completion()).await.unwrap()
}

async fn committed_child_result(artifacts: &HostArtifacts, run_id: &str) -> CommitRef {
    let child = InvocationKey {
        run_id: run_id.into(),
        graph_digest: format!("{run_id}-digest"),
        node_id: "answer".into(),
        invocation: 1,
    };
    let workspace = artifacts.workspace_path(&child).unwrap();
    fs::create_dir_all(workspace.join("nested")).unwrap();
    fs::write(workspace.join("nested/answer.md"), "child-result").unwrap();
    fs::write(workspace.join("summary.txt"), "summary").unwrap();
    artifacts.freeze(&child, &completion()).await.unwrap()
}

fn call_selection(node: &str, path: &str, alias: &str) -> CallFileSelection {
    CallFileSelection {
        node: node.into(),
        path: path.into(),
        alias: alias.into(),
    }
}

fn read_call_manifest(artifacts: &HostArtifacts, run_id: &str) -> CallInputManifest {
    serde_json::from_slice(&fs::read(artifacts.call_inputs_manifest_path(run_id).unwrap()).unwrap())
        .unwrap()
}

fn write_call_manifest(artifacts: &HostArtifacts, run_id: &str, manifest: &CallInputManifest) {
    write_json_atomic(
        &artifacts.call_inputs_manifest_path(run_id).unwrap(),
        manifest,
    )
    .unwrap();
}

/// Drop both durable halves of a child bundle so a fresh admission may rebuild
/// the frozen selection. Only a full reset is allowed to discard a bundle.
fn reset_call_bundle(artifacts: &HostArtifacts, run_id: &str) {
    let dir = artifacts.call_inputs_path(run_id).unwrap();
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    let manifest = artifacts.call_inputs_manifest_path(run_id).unwrap();
    if manifest.exists() {
        fs::remove_file(&manifest).unwrap();
    }
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
    if path.join("git-view").exists() {
        fs::remove_dir_all(path.join("git-view")).unwrap();
        fs::remove_file(path.join(".git-view.lock")).unwrap();
    }
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

#[tokio::test]
async fn op_call_files_and_result_round_trip_through_workspaces() {
    let (_temp, artifacts) = fixture();
    // Parent call node sees the committed `producer` snapshot.
    let producer = key("producer");
    workspace(&artifacts, &producer);
    let producer = artifacts.freeze(&producer, &completion()).await.unwrap();

    // `files` stages the selected committed input into the child `/in/call` bundle.
    let child_run = "call-child-run";
    let selections = [CallFileSelection {
        node: "producer".into(),
        path: "nested/report.txt".into(),
        alias: "input/report.txt".into(),
    }];
    artifacts
        .stage_call_inputs(child_run, std::slice::from_ref(&producer), &selections)
        .await
        .unwrap();
    // Idempotent: a retry must observe the frozen bundle unchanged.
    artifacts
        .stage_call_inputs(child_run, std::slice::from_ref(&producer), &selections)
        .await
        .unwrap();
    // Tampering with a staged file fails closed on re-stage and on mount.
    fs::write(
        artifacts
            .call_inputs_path(child_run)
            .unwrap()
            .join("input/report.txt"),
        "tampered",
    )
    .unwrap();
    assert!(
        artifacts
            .stage_call_inputs(child_run, std::slice::from_ref(&producer), &selections)
            .await
            .is_err()
    );
    assert!(
        artifacts
            .input_mounts(&[], child_run, "child-digest")
            .is_err()
    );
    // A different parent selection can never reuse this child's bundle.
    let other_key = key("other");
    workspace(&artifacts, &other_key);
    let other = artifacts.freeze(&other_key, &completion()).await.unwrap();
    assert!(
        artifacts
            .stage_call_inputs(child_run, &[other], &selections)
            .await
            .is_err()
    );
    // Restore a valid bundle for the remaining assertions.
    fs::remove_dir_all(artifacts.call_inputs_path(child_run).unwrap()).unwrap();
    fs::remove_file(artifacts.call_inputs_manifest_path(child_run).unwrap()).unwrap();
    artifacts
        .stage_call_inputs(child_run, std::slice::from_ref(&producer), &selections)
        .await
        .unwrap();
    let child_mounts = artifacts
        .input_mounts(&[], child_run, "child-digest")
        .unwrap();
    assert_eq!(
        destinations(&child_mounts),
        ["/in/call"],
        "the child must see its staged call inputs at /in/call"
    );
    let call_bundle = child_mounts
        .iter()
        .find(|mount| mount.destination == Path::new("/in/call"))
        .unwrap();
    assert_eq!(
        fs::read_to_string(call_bundle.source.join("input/report.txt")).unwrap(),
        "original"
    );
    // A selection the caller cannot see fails closed.
    assert!(
        artifacts
            .stage_call_inputs(
                "call-other",
                std::slice::from_ref(&producer),
                &[CallFileSelection {
                    node: "missing".into(),
                    path: "nested/report.txt".into(),
                    alias: "input/report.txt".into(),
                }],
            )
            .await
            .is_err()
    );

    // Child result files are copied into the parent call node workspace.
    let child_answer = InvocationKey {
        run_id: child_run.into(),
        graph_digest: "child-digest".into(),
        node_id: "answer".into(),
        invocation: 1,
    };
    let answer_workspace = artifacts.workspace_path(&child_answer).unwrap();
    fs::create_dir_all(answer_workspace.join("nested")).unwrap();
    fs::write(answer_workspace.join("nested/answer.md"), "child-result").unwrap();
    let answer = artifacts
        .freeze(&child_answer, &completion())
        .await
        .unwrap();

    let call_key = key("notify");
    let copied = artifacts
        .export_call_result_files(&call_key, &answer, &["nested/answer.md".to_owned()])
        .await
        .unwrap();
    assert_eq!(copied, ["nested/answer.md"]);
    let call_workspace = artifacts.workspace_path(&call_key).unwrap();
    assert_eq!(
        fs::read_to_string(call_workspace.join("result/nested/answer.md")).unwrap(),
        "child-result"
    );

    // Freezing the call node commits the selected result files for downstream nodes.
    let call_commit = artifacts
        .freeze_with_context(
            &call_key,
            &completion(),
            &ArtifactFreezeContext {
                kind: ArtifactKind::GraphCall,
                input_commits: vec![producer],
            },
        )
        .await
        .unwrap();
    let downstream = artifacts
        .input_mounts(&[call_commit], "run-1", "digest-1")
        .unwrap();
    let notify = downstream
        .iter()
        .find(|mount| mount.destination == Path::new("/in/notify"))
        .unwrap();
    assert_eq!(
        fs::read_to_string(notify.source.join("result/nested/answer.md")).unwrap(),
        "child-result"
    );
}

#[tokio::test]
async fn stage_call_inputs_recovers_each_publish_breakpoint_without_rewriting_inputs() {
    let (_temp, artifacts) = fixture();
    let producer = committed_parent(&artifacts, "producer").await;
    let parents = std::slice::from_ref(&producer);
    let selections = [call_selection(
        "producer",
        "nested/report.txt",
        "input/report.txt",
    )];
    let run_id = "call-child";
    let final_dir = artifacts.call_inputs_path(run_id).unwrap();
    let manifest_path = artifacts.call_inputs_manifest_path(run_id).unwrap();

    // Reference publish.
    artifacts
        .stage_call_inputs(run_id, parents, &selections)
        .await
        .unwrap();
    let bundle = fs::read(final_dir.join("input/report.txt")).unwrap();
    let manifest = fs::read(&manifest_path).unwrap();
    assert_eq!(bundle, b"original");

    // Breakpoint: the bundle directory rename already landed. A retry observes
    // the frozen bundle and must not rewrite it.
    artifacts
        .stage_call_inputs(run_id, parents, &selections)
        .await
        .unwrap();
    assert_eq!(
        fs::read(final_dir.join("input/report.txt")).unwrap(),
        bundle
    );
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest);

    // Breakpoint: the manifest was published but the temporary tree had not yet
    // been renamed into place.
    fs::remove_dir_all(&final_dir).unwrap();
    artifacts
        .stage_call_inputs(run_id, parents, &selections)
        .await
        .unwrap();
    assert_eq!(
        fs::read(final_dir.join("input/report.txt")).unwrap(),
        bundle
    );
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest);

    // Breakpoint: the temporary tree was fully built but nothing was published.
    // The orphaned temp must be reclaimed and the bundle rebuilt identically.
    fs::remove_dir_all(&final_dir).unwrap();
    fs::remove_file(&manifest_path).unwrap();
    let orphan = final_dir.parent().unwrap().join(format!(
        ".call-inputs-{run_id}-{}-999999.tmp",
        std::process::id()
    ));
    fs::create_dir_all(orphan.join("input")).unwrap();
    fs::write(orphan.join("input/report.txt"), "partial").unwrap();
    artifacts
        .stage_call_inputs(run_id, parents, &selections)
        .await
        .unwrap();
    assert!(
        !orphan.exists(),
        "a crashed temporary tree must be reclaimed"
    );
    assert_eq!(
        fs::read(final_dir.join("input/report.txt")).unwrap(),
        bundle
    );
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest);
}

#[tokio::test]
async fn stage_call_inputs_fails_closed_on_incomplete_publishes() {
    let (_temp, artifacts) = fixture();
    let producer = committed_parent(&artifacts, "producer").await;
    let parents = std::slice::from_ref(&producer);
    let selections = [call_selection(
        "producer",
        "nested/report.txt",
        "input/report.txt",
    )];
    let run_id = "call-child";
    let final_dir = artifacts.call_inputs_path(run_id).unwrap();
    let manifest_path = artifacts.call_inputs_manifest_path(run_id).unwrap();

    artifacts
        .stage_call_inputs(run_id, parents, &selections)
        .await
        .unwrap();

    // A published bundle whose manifest vanished must not be rebuilt as a new
    // input tree and must not be mounted.
    fs::remove_file(&manifest_path).unwrap();
    assert!(
        artifacts
            .stage_call_inputs(run_id, parents, &selections)
            .await
            .is_err()
    );
    assert!(artifacts.input_mounts(&[], run_id, "child-digest").is_err());

    // A surviving manifest whose bundle vanished also fails closed when mounted
    // even though no selection mismatch is visible.
    reset_call_bundle(&artifacts, run_id);
    artifacts
        .stage_call_inputs(run_id, parents, &selections)
        .await
        .unwrap();
    fs::remove_dir_all(&final_dir).unwrap();
    assert!(artifacts.input_mounts(&[], run_id, "child-digest").is_err());

    // Only a full re-admission rebuilds the identical frozen selection.
    artifacts
        .stage_call_inputs(run_id, parents, &selections)
        .await
        .unwrap();
    assert_eq!(
        fs::read(final_dir.join("input/report.txt")).unwrap(),
        b"original"
    );
    assert!(manifest_path.is_file());
}

#[tokio::test]
async fn stage_call_inputs_rejects_parent_child_selection_and_hash_tampering() {
    let (_temp, artifacts) = fixture();
    let producer = committed_parent(&artifacts, "producer").await;
    let other = committed_parent(&artifacts, "other").await;
    let selections = [call_selection(
        "producer",
        "nested/report.txt",
        "input/report.txt",
    )];
    let run_id = "call-tamper";
    artifacts
        .stage_call_inputs(run_id, std::slice::from_ref(&producer), &selections)
        .await
        .unwrap();

    // A retry with a different parent set is a different request.
    assert!(
        artifacts
            .stage_call_inputs(run_id, std::slice::from_ref(&other), &selections)
            .await
            .is_err()
    );
    // So is a retry that changes the file selection for the same parents.
    let altered = [call_selection(
        "producer",
        "nested/report.txt",
        "input/renamed.txt",
    )];
    assert!(
        artifacts
            .stage_call_inputs(run_id, std::slice::from_ref(&producer), &altered)
            .await
            .is_err()
    );

    let original = read_call_manifest(&artifacts, run_id);

    // Child identity tamper.
    let mut manifest = original.clone();
    manifest.child_run_id = "call-other".into();
    write_call_manifest(&artifacts, run_id, &manifest);
    assert!(
        artifacts
            .stage_call_inputs(run_id, std::slice::from_ref(&producer), &selections)
            .await
            .is_err()
    );
    assert!(artifacts.input_mounts(&[], run_id, "d").is_err());

    // Recorded hash tamper.
    manifest = original.clone();
    manifest.files[0].sha256 = "0".repeat(64);
    write_call_manifest(&artifacts, run_id, &manifest);
    assert!(
        artifacts
            .stage_call_inputs(run_id, std::slice::from_ref(&producer), &selections)
            .await
            .is_err()
    );
    assert!(artifacts.input_mounts(&[], run_id, "d").is_err());

    // Recorded selection (alias) tamper.
    manifest = original.clone();
    manifest.files[0].alias = "input/renamed.txt".into();
    write_call_manifest(&artifacts, run_id, &manifest);
    assert!(
        artifacts
            .stage_call_inputs(run_id, std::slice::from_ref(&producer), &selections)
            .await
            .is_err()
    );

    // Recorded parent tamper.
    manifest = original.clone();
    manifest.parents = vec![other.clone()];
    write_call_manifest(&artifacts, run_id, &manifest);
    assert!(
        artifacts
            .stage_call_inputs(run_id, std::slice::from_ref(&producer), &selections)
            .await
            .is_err()
    );

    // A duplicate alias declared by the manifest is rejected even when there is
    // no caller-side selection to compare against (the mount path).
    manifest = original.clone();
    manifest.files.push(original.files[0].clone());
    write_call_manifest(&artifacts, run_id, &manifest);
    assert!(artifacts.input_mounts(&[], run_id, "d").is_err());
}

#[tokio::test]
async fn stage_call_inputs_rejects_duplicate_selection_aliases_before_publishing() {
    let (_temp, artifacts) = fixture();
    let producer = committed_parent(&artifacts, "producer").await;
    let run_id = "call-duplicate";
    let duplicate = [
        call_selection("producer", "nested/report.txt", "input/same.txt"),
        call_selection("producer", "report draft.txt", "input/same.txt"),
    ];
    assert!(
        artifacts
            .stage_call_inputs(run_id, std::slice::from_ref(&producer), &duplicate)
            .await
            .is_err()
    );
    // A malformed selection must not leave a partial durable bundle behind.
    assert!(!artifacts.call_inputs_path(run_id).unwrap().exists());
    assert!(
        !artifacts
            .call_inputs_manifest_path(run_id)
            .unwrap()
            .exists()
    );
}

#[tokio::test]
async fn stage_call_inputs_rejects_partial_and_extra_bundle_trees() {
    let (_temp, artifacts) = fixture();
    let producer = committed_parent(&artifacts, "producer").await;
    let parents = std::slice::from_ref(&producer);
    let selections = [call_selection(
        "producer",
        "nested/report.txt",
        "input/report.txt",
    )];
    let run_id = "call-tree";
    let final_dir = artifacts.call_inputs_path(run_id).unwrap();

    artifacts
        .stage_call_inputs(run_id, parents, &selections)
        .await
        .unwrap();

    // Partial tree: a declared file disappeared.
    fs::remove_file(final_dir.join("input/report.txt")).unwrap();
    assert!(
        artifacts
            .stage_call_inputs(run_id, parents, &selections)
            .await
            .is_err()
    );
    assert!(artifacts.input_mounts(&[], run_id, "d").is_err());

    // Extra undeclared file.
    reset_call_bundle(&artifacts, run_id);
    artifacts
        .stage_call_inputs(run_id, parents, &selections)
        .await
        .unwrap();
    fs::write(final_dir.join("input/extra.txt"), "extra").unwrap();
    assert!(
        artifacts
            .stage_call_inputs(run_id, parents, &selections)
            .await
            .is_err()
    );
    assert!(artifacts.input_mounts(&[], run_id, "d").is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn stage_call_inputs_rejects_symlinked_bundle_entries_and_directories() {
    use std::os::unix::fs::symlink;
    let (temp, artifacts) = fixture();
    let producer = committed_parent(&artifacts, "producer").await;
    let parents = std::slice::from_ref(&producer);
    let selections = [call_selection(
        "producer",
        "nested/report.txt",
        "input/report.txt",
    )];
    let run_id = "call-symlink";
    let final_dir = artifacts.call_inputs_path(run_id).unwrap();

    artifacts
        .stage_call_inputs(run_id, parents, &selections)
        .await
        .unwrap();
    let outside = temp.path().join("outside.txt");
    fs::write(&outside, "outside").unwrap();
    fs::remove_file(final_dir.join("input/report.txt")).unwrap();
    symlink(&outside, final_dir.join("input/report.txt")).unwrap();
    assert!(
        artifacts
            .stage_call_inputs(run_id, parents, &selections)
            .await
            .is_err()
    );
    assert!(artifacts.input_mounts(&[], run_id, "d").is_err());

    // A symlink where the published bundle directory should be is never treated
    // as a valid bundle.
    reset_call_bundle(&artifacts, run_id);
    let redirected = temp.path().join("redirected");
    fs::create_dir_all(redirected.join("input")).unwrap();
    fs::write(redirected.join("input/report.txt"), "redirected").unwrap();
    symlink(&redirected, &final_dir).unwrap();
    assert!(
        artifacts
            .stage_call_inputs(run_id, parents, &selections)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn export_call_result_files_is_idempotent_across_transfer_and_swap_breakpoints() {
    let (_temp, artifacts) = fixture();
    let commit = committed_child_result(&artifacts, "child-run").await;
    let files = ["nested/answer.md".to_owned(), "summary.txt".to_owned()];
    let call_key = key("notify");
    let call_ws = artifacts.workspace_path(&call_key).unwrap();

    let copied = artifacts
        .export_call_result_files(&call_key, &commit, &files)
        .await
        .unwrap();
    assert_eq!(copied, files);
    assert_eq!(
        fs::read_to_string(call_ws.join("result/nested/answer.md")).unwrap(),
        "child-result"
    );
    assert_eq!(
        fs::read_to_string(call_ws.join("result/summary.txt")).unwrap(),
        "summary"
    );

    // Retry after a completed swap is idempotent.
    assert_eq!(
        artifacts
            .export_call_result_files(&call_key, &commit, &files)
            .await
            .unwrap(),
        files
    );
    assert_eq!(
        fs::read_to_string(call_ws.join("result/nested/answer.md")).unwrap(),
        "child-result"
    );

    // Crash before the swap: a partial temporary tree is reclaimed and the
    // published result stays complete.
    let stale = call_ws.join(format!(".result-{}-000001.tmp", std::process::id()));
    fs::create_dir_all(stale.join("nested")).unwrap();
    fs::write(stale.join("nested/answer.md"), "partial").unwrap();
    artifacts
        .export_call_result_files(&call_key, &commit, &files)
        .await
        .unwrap();
    assert!(!stale.exists());
    assert_eq!(
        fs::read_to_string(call_ws.join("result/summary.txt")).unwrap(),
        "summary"
    );

    // Crash mid-swap: `result` was renamed aside and the replacement never
    // landed. A retry restores the complete tree.
    let half = call_ws.join(format!(".result-old-{}-000002.tmp", std::process::id()));
    fs::rename(call_ws.join("result"), &half).unwrap();
    assert!(!call_ws.join("result").exists());
    artifacts
        .export_call_result_files(&call_key, &commit, &files)
        .await
        .unwrap();
    assert!(!half.exists());
    assert_eq!(
        fs::read_to_string(call_ws.join("result/nested/answer.md")).unwrap(),
        "child-result"
    );

    // Requesting a file the child commit never declared fails closed and leaves
    // the last complete result in place.
    assert!(
        artifacts
            .export_call_result_files(&call_key, &commit, &["nested/missing.md".to_owned()])
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(call_ws.join("result/nested/answer.md")).unwrap(),
        "child-result"
    );
}

#[tokio::test]
async fn graph_call_freeze_is_idempotent_and_refuses_interrupted_exports() {
    let (_temp, artifacts) = fixture();
    let producer = committed_parent(&artifacts, "producer").await;
    let commit = committed_child_result(&artifacts, "child-run").await;
    let files = ["nested/answer.md".to_owned()];
    let call_key = key("notify");
    let call_ws = artifacts.workspace_path(&call_key).unwrap();
    let context = ArtifactFreezeContext {
        kind: ArtifactKind::GraphCall,
        input_commits: vec![producer.clone()],
    };

    artifacts
        .export_call_result_files(&call_key, &commit, &files)
        .await
        .unwrap();
    let call_commit = artifacts
        .freeze_with_context(&call_key, &completion(), &context)
        .await
        .unwrap();
    assert_eq!(
        artifacts
            .freeze_with_context(&call_key, &completion(), &context)
            .await
            .unwrap(),
        call_commit
    );

    // A changed workspace after a published freeze must not create a new commit:
    // the immutable call result is what downstream nodes see.
    fs::write(call_ws.join("result/nested/answer.md"), "changed").unwrap();
    assert_eq!(
        artifacts
            .freeze_with_context(&call_key, &completion(), &context)
            .await
            .unwrap(),
        call_commit
    );

    // Conflicting completion or context stay fail-closed.
    let changed = NodeCompletion {
        submission: "different".into(),
        ..completion()
    };
    assert!(matches!(
        artifacts
            .freeze_with_context(&call_key, &changed, &context)
            .await,
        Err(GraphError::RunConflict)
    ));
    let changed_context = ArtifactFreezeContext {
        kind: ArtifactKind::GraphCall,
        input_commits: vec![],
    };
    assert!(matches!(
        artifacts
            .freeze_with_context(&call_key, &completion(), &changed_context)
            .await,
        Err(GraphError::RunConflict)
    ));

    // An interrupted export (a surviving `.result-*` entry) refuses to freeze
    // instead of committing a partial or empty result tree.
    let other_key = key("notify-interrupted");
    let other_ws = artifacts.workspace_path(&other_key).unwrap();
    fs::create_dir_all(other_ws.join("result")).unwrap();
    fs::write(other_ws.join("result/answer.md"), "partial").unwrap();
    fs::create_dir_all(other_ws.join(format!(".result-old-{}-000003.tmp", std::process::id())))
        .unwrap();
    let other_context = ArtifactFreezeContext {
        kind: ArtifactKind::GraphCall,
        input_commits: vec![producer],
    };
    assert!(
        artifacts
            .freeze_with_context(&other_key, &completion(), &other_context)
            .await
            .is_err()
    );

    // Once the interrupted export is completed, the same freeze succeeds.
    artifacts
        .export_call_result_files(&other_key, &commit, &files)
        .await
        .unwrap();
    assert!(
        artifacts
            .freeze_with_context(&other_key, &completion(), &other_context)
            .await
            .is_ok()
    );
}
