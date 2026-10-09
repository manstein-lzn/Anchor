use super::*;
use crate::artifacts::tests::{completion, fixture, key};
use serde_json::json;

fn bind(artifacts: &HostArtifacts, key: &InvocationKey) {
    artifacts
        .bind_node_workspace(&key.run_id, &key.graph_digest, &key.node_id)
        .unwrap();
}

fn next(key: &InvocationKey) -> InvocationKey {
    InvocationKey {
        invocation: key.invocation + 1,
        ..key.clone()
    }
}

fn freeze(artifacts: &HostArtifacts, key: &InvocationKey, inputs: &[CommitRef]) -> CommitRef {
    artifacts
        .freeze_snapshot(
            key,
            &completion(),
            &ArtifactFreezeContext {
                kind: ArtifactKind::Node,
                input_commits: inputs.to_vec(),
            },
        )
        .unwrap()
}

#[test]
fn bound_invocations_keep_the_same_directory_and_immutable_success_history() {
    let (_temp, artifacts) = fixture();
    let first = key("module/write");
    bind(&artifacts, &first);
    let path = artifacts.workspace_path(&first).unwrap();
    assert_eq!(path, artifacts.stable_workspace_path(&first));
    assert!(!path.exists());
    assert_eq!(artifacts.prepare_workspace(&first, &[]).unwrap(), path);
    fs::create_dir_all(path.join("assets/empty")).unwrap();
    fs::write(path.join("draft.txt"), "first draft").unwrap();
    #[cfg(unix)]
    let first_inode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&path).unwrap().ino()
    };
    let first_commit = freeze(&artifacts, &first, &[]);
    let second = next(&first);
    assert!(artifacts.workspace_path(&second).is_err());
    assert_eq!(
        artifacts
            .prepare_workspace(&second, std::slice::from_ref(&first_commit))
            .unwrap(),
        path
    );
    assert_eq!(artifacts.workspace_path(&second).unwrap(), path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&path).unwrap().ino(), first_inode);
    }
    assert!(path.join("assets/empty").is_dir());
    assert_eq!(
        fs::read_to_string(path.join("draft.txt")).unwrap(),
        "first draft"
    );
    fs::write(path.join("draft.txt"), "second draft").unwrap();
    let second_commit = freeze(&artifacts, &second, std::slice::from_ref(&first_commit));
    assert_eq!(
        fs::read_to_string(
            artifacts
                .files_path(&first_commit)
                .unwrap()
                .join("draft.txt")
        )
        .unwrap(),
        "first draft"
    );
    assert_eq!(
        fs::read_to_string(
            artifacts
                .files_path(&second_commit)
                .unwrap()
                .join("draft.txt")
        )
        .unwrap(),
        "second draft"
    );
    let owner = artifacts.read_workspace_owner(&second).unwrap().unwrap();
    assert!(owner.initialized);
    assert_eq!(owner.key, second);
    assert_eq!(owner.previous_owner, Some(first.clone()));
    assert_eq!(
        owner.input_commits.as_slice(),
        std::slice::from_ref(&first_commit)
    );
    assert_eq!(
        owner.files["draft.txt"].sha256,
        format!("{:x}", Sha256::digest(b"first draft"))
    );
    assert_eq!(freeze(&artifacts, &first, &[]), first_commit);
    assert!(artifacts.prepare_workspace(&second, &[]).is_err());
}

#[test]
fn same_key_restart_preserves_pending_edits_and_binding_is_idempotent() {
    let (_temp, artifacts) = fixture();
    let current = key("assistant");
    bind(&artifacts, &current);
    let path = artifacts.prepare_workspace(&current, &[]).unwrap();
    fs::write(path.join("unfinished.md"), "pending edits").unwrap();
    let binding = artifacts.binding_state_path(&current).join("binding.json");
    let original_binding = fs::read(&binding).unwrap();
    let owner = artifacts.binding_state_path(&current).join("owner.json");
    let original_owner = fs::read(&owner).unwrap();
    let restarted = HostArtifacts::new(artifacts.root.clone(), artifacts.workspace_root.clone());
    bind(&restarted, &current);
    assert_eq!(fs::read(&binding).unwrap(), original_binding);
    assert_eq!(fs::read(&owner).unwrap(), original_owner);
    assert_eq!(restarted.workspace_path(&current).unwrap(), path);
    assert_eq!(restarted.prepare_workspace(&current, &[]).unwrap(), path);
    assert_eq!(
        fs::read_to_string(path.join("unfinished.md")).unwrap(),
        "pending edits"
    );
    assert!(restarted.prepare_workspace(&next(&current), &[]).is_err());
    assert_eq!(fs::read(&owner).unwrap(), original_owner);
}

#[test]
fn interrupted_checkpoint_preserves_scene_and_allows_the_next_owner_without_success() {
    let (_temp, artifacts) = fixture();
    let first = key("assistant");
    bind(&artifacts, &first);
    let path = artifacts.prepare_workspace(&first, &[]).unwrap();
    fs::create_dir_all(path.join("research/empty")).unwrap();
    fs::write(path.join("research/notes.md"), "partially researched").unwrap();
    let second = next(&first);
    assert!(artifacts.prepare_workspace(&second, &[]).is_err());
    artifacts.retain_interrupted_workspace(&first).unwrap();
    artifacts.retain_interrupted_workspace(&first).unwrap();
    let checkpoint = artifacts.interrupted_workspace_path(&first);
    let saved_manifest = fs::read(checkpoint.join("checkpoint.json")).unwrap();
    assert!(!artifacts.root.join(&commit_for(&first).id).exists());
    assert!(artifacts.files_path(&commit_for(&first)).is_err());
    let restarted = HostArtifacts::new(artifacts.root.clone(), artifacts.workspace_root.clone());
    assert_eq!(restarted.prepare_workspace(&second, &[]).unwrap(), path);
    assert_eq!(
        fs::read_to_string(path.join("research/notes.md")).unwrap(),
        "partially researched"
    );
    assert!(path.join("research/empty").is_dir());
    fs::write(path.join("research/notes.md"), "finished research").unwrap();
    let second_commit = freeze(&restarted, &second, &[]);
    assert_eq!(
        fs::read(checkpoint.join("checkpoint.json")).unwrap(),
        saved_manifest
    );
    assert_eq!(
        fs::read_to_string(checkpoint.join("files/research/notes.md")).unwrap(),
        "partially researched"
    );
    assert!(checkpoint.join("files/research/empty").is_dir());
    assert_eq!(
        fs::read_to_string(
            restarted
                .files_path(&second_commit)
                .unwrap()
                .join("research/notes.md")
        )
        .unwrap(),
        "finished research"
    );
    assert!(restarted.retain_interrupted_workspace(&first).is_err());
    restarted.read_interrupted_workspace(&first).unwrap();
}

#[test]
fn interruption_artifact_freezes_only_control_evidence_and_is_not_owner_release() {
    let (_temp, artifacts) = fixture();
    let first = key("assistant");
    bind(&artifacts, &first);
    let path = artifacts.prepare_workspace(&first, &[]).unwrap();
    fs::write(path.join("draft.txt"), "unfinished work").unwrap();
    let context = ArtifactFreezeContext {
        kind: ArtifactKind::Interruption,
        input_commits: vec![],
    };
    let completion = NodeCompletion {
        submission: "interrupted".into(),
        output: json!({"status": "cancelled", "reason": "user stop"}),
        ..completion()
    };
    let control = artifacts
        .freeze_snapshot(&first, &completion, &context)
        .unwrap();
    let files = artifacts.files_path(&control).unwrap();
    assert_eq!(artifacts.list_files(&control).unwrap().len(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(files.join("interruption.json")).unwrap())
            .unwrap(),
        completion.output
    );
    assert!(!files.join("draft.txt").exists());
    let manifest = fs::read(artifacts.root.join(&control.id).join("manifest.json")).unwrap();
    assert!(artifacts.prepare_workspace(&next(&first), &[]).is_err());
    artifacts.retain_interrupted_workspace(&first).unwrap();
    assert_eq!(
        artifacts
            .prepare_workspace(&next(&first), std::slice::from_ref(&control))
            .unwrap(),
        path
    );
    assert!(!path.join("interruption.json").exists());
    fs::write(path.join("draft.txt"), "later work").unwrap();
    assert_eq!(
        fs::read_to_string(path.join("draft.txt")).unwrap(),
        "later work"
    );
    assert_eq!(
        fs::read(artifacts.root.join(&control.id).join("manifest.json")).unwrap(),
        manifest
    );
    assert_eq!(
        artifacts
            .freeze_snapshot(&first, &completion, &context)
            .unwrap(),
        control
    );
    fs::write(files.join("interruption.json"), "{}").unwrap();
    assert!(artifacts.files_path(&control).is_err());
}

#[test]
fn old_key_cannot_read_prepare_retain_or_newly_freeze_the_later_writable_scene() {
    let (_temp, artifacts) = fixture();
    let first = key("assistant");
    bind(&artifacts, &first);
    let path = artifacts.prepare_workspace(&first, &[]).unwrap();
    fs::write(path.join("draft.txt"), "unfinished").unwrap();
    artifacts.retain_interrupted_workspace(&first).unwrap();
    let second = next(&first);
    artifacts.prepare_workspace(&second, &[]).unwrap();
    fs::write(path.join("draft.txt"), "new owner").unwrap();
    assert!(matches!(
        artifacts.workspace_path(&first),
        Err(GraphError::RunConflict)
    ));
    assert!(matches!(
        artifacts.prepare_workspace(&first, &[]),
        Err(GraphError::RunConflict)
    ));
    assert!(artifacts.retain_interrupted_workspace(&first).is_err());
    assert!(
        artifacts
            .freeze_snapshot(
                &first,
                &completion(),
                &ArtifactFreezeContext {
                    kind: ArtifactKind::Node,
                    input_commits: vec![]
                },
            )
            .is_err()
    );
    assert_eq!(artifacts.workspace_path(&second).unwrap(), path);
    assert_eq!(
        fs::read_to_string(path.join("draft.txt")).unwrap(),
        "new owner"
    );
}

#[test]
fn nodes_and_runs_are_isolated_and_unbound_invocations_remain_independent() {
    let (_temp, artifacts) = fixture();
    let first = key("module/write");
    let other_node = key("other/write");
    let other_run = InvocationKey {
        run_id: "run-2".into(),
        ..first.clone()
    };
    for current in [&first, &other_node, &other_run] {
        bind(&artifacts, current);
    }
    let path = artifacts.prepare_workspace(&first, &[]).unwrap();
    fs::write(path.join("private.txt"), "own work").unwrap();
    for current in [&other_node, &other_run] {
        let other_path = artifacts.prepare_workspace(current, &[]).unwrap();
        assert_ne!(other_path, path);
        assert_eq!(fs::read_dir(&other_path).unwrap().count(), 0);
    }
    let unbound = key("unbound");
    let first_unbound = artifacts.prepare_workspace(&unbound, &[]).unwrap();
    assert_eq!(
        first_unbound,
        artifacts
            .workspace_root
            .join(&unbound.run_id)
            .join(key_hash(&unbound))
    );
    fs::write(first_unbound.join("draft.txt"), "committed").unwrap();
    let commit = freeze(&artifacts, &unbound, &[]);
    fs::write(first_unbound.join("draft.txt"), "not committed").unwrap();
    let second_unbound = artifacts
        .prepare_workspace(&next(&unbound), &[commit])
        .unwrap();
    assert_ne!(first_unbound, second_unbound);
    assert_eq!(
        fs::read_to_string(second_unbound.join("draft.txt")).unwrap(),
        "committed"
    );
    assert_eq!(artifacts.workspace_path(&unbound).unwrap(), first_unbound);
}

#[test]
fn existing_stable_directory_is_not_an_initialization_or_a_release_fact() {
    let (_temp, artifacts) = fixture();
    let first = key("assistant");
    bind(&artifacts, &first);
    let path = artifacts.workspace_path(&first).unwrap();
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("forged.txt"), "not initialized").unwrap();
    assert!(artifacts.workspace_path(&first).is_err());
    assert!(artifacts.prepare_workspace(&first, &[]).is_err());
    assert!(artifacts.prepare_workspace(&next(&first), &[]).is_err());
    assert!(artifacts.retain_interrupted_workspace(&first).is_err());
    assert!(artifacts.read_workspace_owner(&first).unwrap().is_none());
    assert_eq!(
        fs::read_to_string(path.join("forged.txt")).unwrap(),
        "not initialized"
    );
}

#[test]
fn initial_preparation_recovers_only_the_same_keys_saved_initialization() {
    let (_temp, artifacts) = fixture();
    let first = key("assistant");
    bind(&artifacts, &first);
    let path = artifacts.prepare_workspace(&first, &[]).unwrap();
    let mut owner = artifacts.read_workspace_owner(&first).unwrap().unwrap();
    owner.initialized = false;
    artifacts.write_workspace_owner(&owner).unwrap();
    assert!(artifacts.workspace_path(&first).is_err());
    assert!(artifacts.prepare_workspace(&next(&first), &[]).is_err());
    assert_eq!(artifacts.prepare_workspace(&first, &[]).unwrap(), path);
    assert!(
        artifacts
            .read_workspace_owner(&first)
            .unwrap()
            .unwrap()
            .initialized
    );
    fs::remove_dir(&path).unwrap();
    owner.initialized = false;
    artifacts.write_workspace_owner(&owner).unwrap();
    assert_eq!(artifacts.prepare_workspace(&first, &[]).unwrap(), path);
    owner.initialized = false;
    artifacts.write_workspace_owner(&owner).unwrap();
    fs::write(path.join("unverified.txt"), "not initialized").unwrap();
    assert!(artifacts.prepare_workspace(&first, &[]).is_err());
    assert!(artifacts.prepare_workspace(&next(&first), &[]).is_err());
    assert_eq!(
        fs::read_to_string(path.join("unverified.txt")).unwrap(),
        "not initialized"
    );
}

#[test]
fn forged_binding_identity_and_unknown_path_fields_are_rejected() {
    for field in ["format", "run_id", "graph_digest", "node_id", "workspace"] {
        let (_temp, artifacts) = fixture();
        let first = key("assistant");
        bind(&artifacts, &first);
        let binding = artifacts.binding_state_path(&first).join("binding.json");
        let mut forged: Value = serde_json::from_slice(&fs::read(&binding).unwrap()).unwrap();
        forged[field] = if field == "format" {
            json!(2)
        } else {
            json!("forged")
        };
        fs::write(&binding, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(artifacts.workspace_path(&first).is_err(), "{field}");
        assert!(artifacts.prepare_workspace(&first, &[]).is_err(), "{field}");
        assert!(
            artifacts
                .bind_node_workspace(&first.run_id, &first.graph_digest, &first.node_id)
                .is_err(),
            "{field}"
        );
    }
}

#[test]
fn forged_or_missing_owner_and_missing_authorization_fail_closed() {
    for corruption in [
        "owner_key",
        "owner_format",
        "binding_missing",
        "owner_missing",
    ] {
        let (_temp, artifacts) = fixture();
        let first = key("assistant");
        bind(&artifacts, &first);
        let path = artifacts.prepare_workspace(&first, &[]).unwrap();
        fs::write(path.join("draft.txt"), "keep this").unwrap();
        let state = artifacts.binding_state_path(&first);
        match corruption {
            "owner_key" | "owner_format" => {
                let mut owner = artifacts.read_workspace_owner(&first).unwrap().unwrap();
                if corruption == "owner_key" {
                    owner.key.node_id = "other".into();
                } else {
                    owner.format = 9;
                }
                write_json_atomic(&state.join("owner.json"), &owner).unwrap();
            }
            "binding_missing" => fs::remove_file(state.join("binding.json")).unwrap(),
            "owner_missing" => fs::remove_file(state.join("owner.json")).unwrap(),
            _ => unreachable!(),
        }
        assert!(artifacts.workspace_path(&first).is_err(), "{corruption}");
        assert!(
            artifacts.prepare_workspace(&first, &[]).is_err(),
            "{corruption}"
        );
        assert!(
            artifacts.prepare_workspace(&next(&first), &[]).is_err(),
            "{corruption}"
        );
        assert_eq!(
            fs::read_to_string(path.join("draft.txt")).unwrap(),
            "keep this"
        );
    }
}

#[test]
fn binding_cannot_change_graph_or_use_unsafe_identity_components() {
    let (_temp, artifacts) = fixture();
    let first = key("assistant");
    bind(&artifacts, &first);
    assert!(
        artifacts
            .bind_node_workspace(&first.run_id, "other-digest", &first.node_id)
            .is_err()
    );
    let wrong_graph = InvocationKey {
        graph_digest: "other-digest".into(),
        ..first.clone()
    };
    assert!(artifacts.workspace_path(&wrong_graph).is_err());
    assert!(artifacts.prepare_workspace(&wrong_graph, &[]).is_err());
    for (run, graph, node) in [
        ("../run", "digest", "node"),
        ("run", "../digest", "node"),
        ("run", "digest", "../node"),
        ("run", "digest", "node//child"),
    ] {
        assert!(artifacts.bind_node_workspace(run, graph, node).is_err());
    }
    assert_eq!(
        artifacts.prepare_workspace(&first, &[]).unwrap(),
        artifacts.workspace_path(&first).unwrap()
    );
}

#[test]
fn checkpoint_is_immutable_and_corrupt_or_changed_outcomes_do_not_release_an_owner() {
    for corruption in [
        "checkpoint_file",
        "checkpoint_identity",
        "undeclared_file",
        "live_scene",
    ] {
        let (_temp, artifacts) = fixture();
        let first = key("assistant");
        bind(&artifacts, &first);
        let path = artifacts.prepare_workspace(&first, &[]).unwrap();
        fs::write(path.join("draft.txt"), "unfinished").unwrap();
        artifacts.retain_interrupted_workspace(&first).unwrap();
        let checkpoint = artifacts.interrupted_workspace_path(&first);
        let manifest = fs::read(checkpoint.join("checkpoint.json")).unwrap();
        match corruption {
            "checkpoint_file" => fs::write(checkpoint.join("files/draft.txt"), "tampered").unwrap(),
            "checkpoint_identity" => {
                let mut saved = artifacts.read_interrupted_workspace(&first).unwrap();
                saved.key.invocation += 1;
                write_json_atomic(&checkpoint.join("checkpoint.json"), &saved).unwrap();
            }
            "undeclared_file" => fs::write(checkpoint.join("extra.txt"), "tampered").unwrap(),
            "live_scene" => fs::write(path.join("draft.txt"), "changed").unwrap(),
            _ => unreachable!(),
        }
        assert!(
            artifacts.retain_interrupted_workspace(&first).is_err(),
            "{corruption}"
        );
        assert!(
            artifacts.prepare_workspace(&next(&first), &[]).is_err(),
            "{corruption}"
        );
        assert_eq!(
            artifacts.read_workspace_owner(&first).unwrap().unwrap().key,
            first
        );
        if corruption != "checkpoint_identity" {
            assert_eq!(
                fs::read(checkpoint.join("checkpoint.json")).unwrap(),
                manifest
            );
        }
    }
}

#[test]
fn unbound_retention_does_not_bind_or_publish_a_success() {
    let (_temp, artifacts) = fixture();
    let first = key("assistant");
    let path = artifacts.prepare_workspace(&first, &[]).unwrap();
    fs::write(path.join("draft.txt"), "unfinished").unwrap();
    artifacts.retain_interrupted_workspace(&first).unwrap();
    assert!(!artifacts.binding_state_path(&first).exists());
    assert!(!artifacts.root.join(&commit_for(&first).id).exists());
    assert_eq!(artifacts.workspace_path(&first).unwrap(), path);
    let second_path = artifacts.prepare_workspace(&next(&first), &[]).unwrap();
    assert_ne!(second_path, path);
    assert_eq!(fs::read_dir(&second_path).unwrap().count(), 0);
}

#[test]
fn legacy_success_and_changed_success_scene_do_not_release_the_bound_owner() {
    for legacy in [false, true] {
        let (_temp, artifacts) = fixture();
        let first = key("assistant");
        bind(&artifacts, &first);
        let path = artifacts.prepare_workspace(&first, &[]).unwrap();
        fs::write(path.join("draft.txt"), "committed").unwrap();
        let commit = freeze(&artifacts, &first, &[]);
        if legacy {
            let (snapshot, mut manifest) = artifacts.read_snapshot(&commit).unwrap();
            manifest.format = 1;
            manifest.context = None;
            manifest.context_sha256 = None;
            write_json_atomic(&snapshot.join("manifest.json"), &manifest).unwrap();
            let legacy_commit = legacy_commit_for(&first);
            fs::rename(&snapshot, artifacts.root.join(&legacy_commit.id)).unwrap();
            artifacts.files_path(&legacy_commit).unwrap();
        } else {
            fs::write(path.join("draft.txt"), "not committed").unwrap();
        }
        assert!(artifacts.prepare_workspace(&next(&first), &[]).is_err());
        artifacts.retain_interrupted_workspace(&first).unwrap();
        assert_eq!(
            artifacts.prepare_workspace(&next(&first), &[]).unwrap(),
            path
        );
    }
}

#[test]
fn a_writable_workspace_binding_file_does_not_grant_a_host_binding() {
    let (_temp, artifacts) = fixture();
    let first = key("assistant");
    let path = artifacts.prepare_workspace(&first, &[]).unwrap();
    write_json_atomic(
        &path.join("binding.json"),
        &WorkspaceBinding {
            format: 1,
            run_id: first.run_id.clone(),
            graph_digest: first.graph_digest.clone(),
            node_id: first.node_id.clone(),
        },
    )
    .unwrap();
    assert!(!artifacts.binding_state_path(&first).exists());
    let second = next(&first);
    assert_ne!(artifacts.prepare_workspace(&second, &[]).unwrap(), path);
    assert_eq!(artifacts.workspace_path(&first).unwrap(), path);
}

#[test]
fn concurrent_preparation_serializes_initialization_and_owner_transfer() {
    let (_temp, artifacts) = fixture();
    let first = key("assistant");
    bind(&artifacts, &first);
    let prepare_twice = |current: &InvocationKey, inputs: &[CommitRef]| {
        std::thread::scope(|scope| {
            let first_preparation = scope.spawn(|| artifacts.prepare_workspace(current, inputs));
            let second_preparation = scope.spawn(|| artifacts.prepare_workspace(current, inputs));
            let first_path = first_preparation.join().unwrap().unwrap();
            assert_eq!(second_preparation.join().unwrap().unwrap(), first_path);
            first_path
        })
    };
    let path = prepare_twice(&first, &[]);
    fs::write(path.join("draft.txt"), "committed").unwrap();
    let commit = freeze(&artifacts, &first, &[]);
    let second = next(&first);
    assert_eq!(prepare_twice(&second, &[commit]), path);
    assert_eq!(
        artifacts
            .read_workspace_owner(&second)
            .unwrap()
            .unwrap()
            .key,
        second
    );
    assert!(artifacts.workspace_path(&first).is_err());
}

#[cfg(unix)]
#[test]
fn a_binding_in_non_private_host_directories_is_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, artifacts) = fixture();
    let first = key("assistant");
    bind(&artifacts, &first);
    let state = artifacts.binding_state_path(&first);
    fs::set_permissions(&state, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(artifacts.workspace_path(&first).is_err());
    assert!(artifacts.prepare_workspace(&first, &[]).is_err());
    assert!(
        artifacts
            .bind_node_workspace(&first.run_id, &first.graph_digest, &first.node_id)
            .is_err()
    );
}

#[tokio::test]
async fn bound_graph_call_exports_and_freezes_the_owned_stable_directory() {
    let (_temp, artifacts) = fixture();
    let child = key("child-answer");
    let child_path = artifacts.prepare_workspace(&child, &[]).unwrap();
    fs::write(child_path.join("answer.txt"), "child answer").unwrap();
    let child_commit = freeze(&artifacts, &child, &[]);
    let call = key("call");
    bind(&artifacts, &call);
    artifacts
        .export_call_result_files(&call, &child_commit, &["answer.txt".into()])
        .await
        .unwrap();
    let path = artifacts.workspace_path(&call).unwrap();
    assert_eq!(path, artifacts.stable_workspace_path(&call));
    assert_eq!(
        fs::read_to_string(path.join("result/answer.txt")).unwrap(),
        "child answer"
    );
    let commit = artifacts
        .freeze_snapshot(
            &call,
            &completion(),
            &ArtifactFreezeContext {
                kind: ArtifactKind::GraphCall,
                input_commits: vec![],
            },
        )
        .unwrap();
    artifacts
        .export_call_result_files(&next(&call), &child_commit, &["answer.txt".into()])
        .await
        .unwrap();
    assert_eq!(artifacts.workspace_path(&next(&call)).unwrap(), path);
    assert!(artifacts.workspace_path(&call).is_err());
    assert_eq!(
        fs::read_to_string(
            artifacts
                .files_path(&commit)
                .unwrap()
                .join("result/answer.txt")
        )
        .unwrap(),
        "child answer"
    );
}

#[cfg(unix)]
#[test]
fn binding_owner_scene_and_checkpoint_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    for resource in ["binding.json", "owner.json", "workspace", "checkpoint"] {
        let (temp, artifacts) = fixture();
        let first = key("assistant");
        bind(&artifacts, &first);
        let path = artifacts.prepare_workspace(&first, &[]).unwrap();
        fs::write(path.join("draft.txt"), "unfinished").unwrap();
        artifacts.retain_interrupted_workspace(&first).unwrap();
        let victim = match resource {
            "workspace" => path,
            "checkpoint" => artifacts.interrupted_workspace_path(&first),
            filename => artifacts.binding_state_path(&first).join(filename),
        };
        let outside = temp.path().join("outside");
        fs::rename(&victim, &outside).unwrap();
        symlink(&outside, &victim).unwrap();
        assert!(
            artifacts.retain_interrupted_workspace(&first).is_err(),
            "{resource}"
        );
        assert!(
            artifacts.prepare_workspace(&next(&first), &[]).is_err(),
            "{resource}"
        );
    }
}

fn other_run(key: &InvocationKey, run: &str) -> InvocationKey {
    InvocationKey {
        run_id: run.into(),
        ..key.clone()
    }
}

/// A handed-over assistant Run inherits the old scene, records the real
/// initialization inputs it was given, and leaves the old Run's scene and
/// Artifact bytes untouched.
#[test]
fn handed_over_scene_is_copied_and_recorded_with_the_real_initialization_inputs() {
    let (_temp, artifacts) = fixture();
    let source = key("assistant");
    bind(&artifacts, &source);
    let source_path = artifacts.prepare_workspace(&source, &[]).unwrap();
    fs::create_dir_all(source_path.join("research/empty")).unwrap();
    fs::write(source_path.join("draft.txt"), "unfinished draft").unwrap();
    let source_commit = freeze(&artifacts, &source, &[]);
    let source_inventory = fs::read(artifacts.root.join(&source_commit.id).join("manifest.json"))
        .unwrap()
        .len();

    // The work node's real inputs are the upstream wait node's commit of its
    // own Run, not the old scene.
    let target = other_run(&source, "run-2");
    bind(&artifacts, &target);
    let wait = InvocationKey {
        node_id: "wait_input".into(),
        ..target.clone()
    };
    bind(&artifacts, &wait);
    let wait_path = artifacts.prepare_workspace(&wait, &[]).unwrap();
    fs::write(wait_path.join("input.json"), "{}").unwrap();
    let wait_commit = freeze(&artifacts, &wait, &[]);

    let target_path = artifacts
        .prepare_workspace_from(&target, std::slice::from_ref(&wait_commit), Some(&source))
        .unwrap();
    assert_eq!(target_path, artifacts.stable_workspace_path(&target));
    assert_eq!(
        fs::read_to_string(target_path.join("draft.txt")).unwrap(),
        "unfinished draft"
    );
    assert!(target_path.join("research/empty").is_dir());

    let owner = artifacts.read_workspace_owner(&target).unwrap().unwrap();
    assert!(owner.initialized);
    assert_eq!(owner.key, target);
    assert_eq!(owner.previous_owner, Some(source.clone()));
    assert_eq!(owner.input_commits, std::slice::from_ref(&wait_commit));
    assert_eq!(
        (owner.files.clone(), owner.directories.clone()),
        scan_workspace(&target_path, None).unwrap()
    );
    assert_eq!(
        owner.files["draft.txt"].sha256,
        format!("{:x}", Sha256::digest(b"unfinished draft"))
    );
    assert!(
        owner
            .directories
            .iter()
            .any(|entry| entry == "research/empty")
    );

    // The old scene, its owner and its Artifact are read-only inputs.
    assert_eq!(
        fs::read_to_string(source_path.join("draft.txt")).unwrap(),
        "unfinished draft"
    );
    assert_eq!(
        artifacts
            .read_workspace_owner(&source)
            .unwrap()
            .unwrap()
            .key,
        source
    );
    assert_eq!(
        fs::read(artifacts.root.join(&source_commit.id).join("manifest.json"))
            .unwrap()
            .len(),
        source_inventory
    );
    artifacts.files_path(&source_commit).unwrap();
    // Preparation locks and owner facts live outside the scene, so they never
    // travel with it.
    assert!(!target_path.join("owner.json").exists());
    assert!(!target_path.join(".prepare.lock").exists());
    assert!(!target_path.join("binding.json").exists());
}

/// Repeating the preparation — the restart/resume case — is a no-op that keeps
/// the new owner's later work, and never re-verifies it against the old scene.
#[test]
fn repeated_preparation_after_a_handover_is_a_noop_and_keeps_later_work() {
    let (_temp, artifacts) = fixture();
    let source = key("assistant");
    bind(&artifacts, &source);
    let source_path = artifacts.prepare_workspace(&source, &[]).unwrap();
    fs::write(source_path.join("draft.txt"), "before handover").unwrap();
    let target = other_run(&source, "run-2");
    bind(&artifacts, &target);
    let wait = InvocationKey {
        node_id: "wait_input".into(),
        ..target.clone()
    };
    bind(&artifacts, &wait);
    artifacts.prepare_workspace(&wait, &[]).unwrap();
    let inputs = [freeze(&artifacts, &wait, &[])];
    let target_path = artifacts
        .prepare_workspace_from(&target, &inputs, Some(&source))
        .unwrap();
    let saved_owner = fs::read(artifacts.binding_state_path(&target).join("owner.json")).unwrap();

    // The new owner works; the old scene does not change with it.
    fs::write(target_path.join("draft.txt"), "after handover").unwrap();
    assert_eq!(
        artifacts
            .prepare_workspace_from(&target, &inputs, Some(&source))
            .unwrap(),
        target_path
    );
    assert_eq!(
        artifacts.prepare_workspace(&target, &inputs).unwrap(),
        target_path
    );
    assert_eq!(
        fs::read_to_string(target_path.join("draft.txt")).unwrap(),
        "after handover"
    );
    assert_eq!(
        fs::read_to_string(source_path.join("draft.txt")).unwrap(),
        "before handover"
    );
    assert_eq!(
        fs::read(artifacts.binding_state_path(&target).join("owner.json")).unwrap(),
        saved_owner
    );
    // The recorded inputs are the ones the runner really passed, so the repeat
    // above never reports `node workspace initialization inputs changed`. A
    // genuinely different input set still fails closed.
    assert!(
        artifacts
            .prepare_workspace_from(&target, &[], Some(&source))
            .is_err()
    );
}

/// A crash can leave the staged scene without its owner fact, or a half-copied
/// temporary directory. Both retries must succeed with a complete scene.
#[test]
fn interrupted_seeding_leaves_no_half_scene_and_retries_successfully() {
    for window in ["renamed_without_owner_fact", "half_copied"] {
        let (_temp, artifacts) = fixture();
        let source = key("assistant");
        bind(&artifacts, &source);
        let source_path = artifacts.prepare_workspace(&source, &[]).unwrap();
        fs::create_dir_all(source_path.join("research/empty")).unwrap();
        fs::write(source_path.join("draft.txt"), "unfinished draft").unwrap();
        let target = other_run(&source, "run-2");
        bind(&artifacts, &target);
        let state = artifacts.binding_state_path(&target);
        match window {
            "renamed_without_owner_fact" => {
                artifacts.seed_stable_workspace(&source, &target).unwrap();
            }
            "half_copied" => {
                // A killed copy leaves its staging directory behind and never
                // reaches the rename.
                fs::create_dir_all(state.join("seed.tmp")).unwrap();
                fs::write(state.join("seed.tmp/partial.txt"), "partial").unwrap();
            }
            _ => unreachable!(),
        }
        assert!(artifacts.read_workspace_owner(&target).unwrap().is_none());
        let path = artifacts
            .prepare_workspace_from(&target, &[], Some(&source))
            .unwrap();
        assert_eq!(
            fs::read_to_string(path.join("draft.txt")).unwrap(),
            "unfinished draft"
        );
        assert!(path.join("research/empty").is_dir());
        assert!(!path.join("partial.txt").exists());
        assert!(!state.join("seed.tmp").exists());
        let owner = artifacts.read_workspace_owner(&target).unwrap().unwrap();
        assert!(owner.initialized);
        assert_eq!(owner.previous_owner, Some(source.clone()));
        assert_eq!(
            (owner.files, owner.directories),
            scan_workspace(&path, None).unwrap()
        );
    }
}

/// Without a proven handover nothing is seeded, and an unexplained existing
/// directory still fails closed instead of being adopted.
#[test]
fn only_a_proven_handover_adopts_an_existing_scene() {
    let (_temp, artifacts) = fixture();
    let source = key("assistant");
    bind(&artifacts, &source);
    let source_path = artifacts.prepare_workspace(&source, &[]).unwrap();
    fs::write(source_path.join("draft.txt"), "handed over").unwrap();
    let target = other_run(&source, "run-2");
    bind(&artifacts, &target);
    artifacts.seed_stable_workspace(&source, &target).unwrap();
    let target_path = artifacts.stable_workspace_path(&target);

    // No seed: the same bytes are not evidence of anything.
    assert!(artifacts.prepare_workspace(&target, &[]).is_err());
    assert!(artifacts.read_workspace_owner(&target).unwrap().is_none());
    assert_eq!(
        fs::read_to_string(target_path.join("draft.txt")).unwrap(),
        "handed over"
    );
    // A source that no longer matches the staged scene is corruption, not a
    // silent adoption.
    fs::write(source_path.join("draft.txt"), "tampered").unwrap();
    assert!(
        artifacts
            .prepare_workspace_from(&target, &[], Some(&source))
            .is_err()
    );
    assert!(artifacts.read_workspace_owner(&target).unwrap().is_none());
    fs::write(source_path.join("draft.txt"), "handed over").unwrap();
    assert_eq!(
        artifacts
            .prepare_workspace_from(&target, &[], Some(&source))
            .unwrap(),
        target_path
    );
}

#[test]
fn seeding_requires_one_node_graph_and_two_runs() {
    let (_temp, artifacts) = fixture();
    let source = key("assistant");
    bind(&artifacts, &source);
    let source_path = artifacts.prepare_workspace(&source, &[]).unwrap();
    fs::write(source_path.join("draft.txt"), "scene").unwrap();
    let target = other_run(&source, "run-2");
    bind(&artifacts, &target);
    assert!(artifacts.seed_stable_workspace(&source, &source).is_err());
    assert!(
        artifacts
            .seed_stable_workspace(
                &InvocationKey {
                    node_id: "other".into(),
                    ..source.clone()
                },
                &target
            )
            .is_err()
    );
    assert!(
        artifacts
            .seed_stable_workspace(
                &InvocationKey {
                    graph_digest: "digest-2".into(),
                    ..source.clone()
                },
                &target
            )
            .is_err()
    );
    // An absent source scene is not an error; it simply seeds nothing.
    assert!(
        artifacts
            .seed_stable_workspace(
                &InvocationKey {
                    run_id: "run-missing".into(),
                    ..source.clone()
                },
                &target
            )
            .unwrap()
            .is_none()
    );
    assert!(!artifacts.stable_workspace_path(&target).exists());
    // The target's owner fact may name the handed-over Run of the same node and
    // Graph, but nothing else.
    artifacts
        .prepare_workspace_from(&target, &[], Some(&source))
        .unwrap();
    let owner_path = artifacts.binding_state_path(&target).join("owner.json");
    let owner = artifacts.read_workspace_owner(&target).unwrap().unwrap();
    assert_eq!(owner.previous_owner, Some(source.clone()));
    for forged in [
        InvocationKey {
            node_id: "other".into(),
            ..source.clone()
        },
        InvocationKey {
            graph_digest: "digest-2".into(),
            ..source.clone()
        },
        InvocationKey {
            run_id: target.run_id.clone(),
            invocation: owner.key.invocation,
            ..source.clone()
        },
    ] {
        let mut tampered = owner.clone();
        tampered.previous_owner = Some(forged.clone());
        write_json_atomic(&owner_path, &tampered).unwrap();
        assert!(
            artifacts.read_workspace_owner(&target).is_err(),
            "{forged:?}"
        );
        assert!(
            artifacts.prepare_workspace(&target, &[]).is_err(),
            "{forged:?}"
        );
        assert!(artifacts.workspace_path(&target).is_err(), "{forged:?}");
    }
    write_json_atomic(&owner_path, &owner).unwrap();
    assert_eq!(
        artifacts.prepare_workspace(&target, &[]).unwrap(),
        artifacts.stable_workspace_path(&target)
    );
}
