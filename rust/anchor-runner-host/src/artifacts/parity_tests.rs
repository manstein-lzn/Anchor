use super::tests::{completion, fixture, key};
use super::*;
use std::process::Command;

async fn freeze_node(
    artifacts: &HostArtifacts,
    key: &InvocationKey,
    inputs: &[CommitRef],
) -> CommitRef {
    artifacts
        .freeze_with_context(
            key,
            &completion(),
            &ArtifactFreezeContext {
                kind: ArtifactKind::Node,
                input_commits: inputs.to_vec(),
            },
        )
        .await
        .unwrap()
}

fn git(files: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(files.join(".git"))
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
async fn git_projection_commit_message_binds_typed_artifact_identity() {
    let (_temp, artifacts) = fixture();
    let invocation = key("write");
    let workspace = artifacts.prepare_workspace(&invocation, &[]).unwrap();
    fs::write(workspace.join("report.md"), "bound artifact\n").unwrap();
    let commit = freeze_node(&artifacts, &invocation, &[]).await;
    let source = artifacts
        .input_mounts(std::slice::from_ref(&commit), "run-1", "digest-1")
        .unwrap()
        .remove(0)
        .source;
    let message = git(&source, &["show", "-s", "--format=%B", "HEAD"]);
    assert!(message.starts_with(&format!(
        "Anchor Artifact {}\nArtifact-Node: {}\nArtifact-Invocation: {}\nManifest-SHA256: ",
        commit.id, commit.node_id, commit.invocation
    )));
    let digest = message
        .strip_prefix(&format!(
            "Anchor Artifact {}\nArtifact-Node: {}\nArtifact-Invocation: {}\nManifest-SHA256: ",
            commit.id, commit.node_id, commit.invocation
        ))
        .unwrap();
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[tokio::test]
async fn revisit_preserves_own_files_but_not_pending_work_and_restart_keeps_edits() {
    let (_temp, artifacts) = fixture();
    let first = key("write");
    let first_workspace = artifacts.prepare_workspace(&first, &[]).unwrap();
    fs::create_dir_all(first_workspace.join("assets/empty")).unwrap();
    fs::write(first_workspace.join("research.md"), "first evidence\n").unwrap();
    fs::write(first_workspace.join("answer.md"), "first draft\n").unwrap();
    let first_commit = freeze_node(&artifacts, &first, &[]).await;
    fs::write(first_workspace.join("uncommitted.txt"), "must not inherit").unwrap();
    fs::write(
        first_workspace.join("research.md"),
        "pending, not committed",
    )
    .unwrap();

    let review = key("review");
    let review_workspace = artifacts
        .prepare_workspace(&review, std::slice::from_ref(&first_commit))
        .unwrap();
    assert!(!review_workspace.join("research.md").exists());
    fs::write(review_workspace.join("feedback.md"), "extend evidence").unwrap();
    let feedback = freeze_node(&artifacts, &review, std::slice::from_ref(&first_commit)).await;
    let second = InvocationKey {
        invocation: 2,
        ..first.clone()
    };
    let second_workspace = artifacts
        .prepare_workspace(&second, std::slice::from_ref(&feedback))
        .unwrap();
    assert_ne!(first_workspace, second_workspace);
    assert_eq!(
        fs::read_to_string(second_workspace.join("research.md")).unwrap(),
        "first evidence\n"
    );
    assert_eq!(
        fs::read_to_string(second_workspace.join("answer.md")).unwrap(),
        "first draft\n"
    );
    assert!(second_workspace.join("assets/empty").is_dir());
    assert!(!second_workspace.join("uncommitted.txt").exists());
    fs::write(
        second_workspace.join("research.md"),
        "first evidence\nsecond evidence\n",
    )
    .unwrap();
    fs::write(second_workspace.join("interrupted.txt"), "in progress").unwrap();
    assert_eq!(
        artifacts
            .prepare_workspace(&second, std::slice::from_ref(&feedback))
            .unwrap(),
        second_workspace
    );
    assert_eq!(
        fs::read_to_string(second_workspace.join("interrupted.txt")).unwrap(),
        "in progress"
    );
    let second_commit = freeze_node(&artifacts, &second, std::slice::from_ref(&feedback)).await;
    let frozen = artifacts.files_path(&first_commit).unwrap();
    assert_eq!(
        fs::read_to_string(frozen.join("research.md")).unwrap(),
        "first evidence\n"
    );
    assert_eq!(
        fs::read_to_string(
            artifacts
                .files_path(&second_commit)
                .unwrap()
                .join("answer.md")
        )
        .unwrap(),
        "first draft\n"
    );
    let third = InvocationKey {
        invocation: 3,
        ..first.clone()
    };
    let third_workspace = artifacts
        .prepare_workspace(&third, std::slice::from_ref(&second_commit))
        .unwrap();
    assert_eq!(
        fs::read_to_string(third_workspace.join("research.md")).unwrap(),
        "first evidence\nsecond evidence\n"
    );

    let other_run = InvocationKey {
        run_id: "other-run".into(),
        ..second.clone()
    };
    assert!(
        artifacts
            .prepare_workspace(&other_run, std::slice::from_ref(&feedback))
            .is_err()
    );
    let other_workspace = artifacts.prepare_workspace(&other_run, &[]).unwrap();
    assert_ne!(other_workspace, second_workspace);
    assert_eq!(fs::read_dir(other_workspace).unwrap().count(), 0);
    assert!(
        artifacts
            .prepare_workspace(
                &InvocationKey {
                    graph_digest: "wrong-digest".into(),
                    ..second
                },
                &[feedback]
            )
            .is_err()
    );
}

#[tokio::test]
async fn git_head_is_pinned_and_changed_draft_rejects_existing_weekly_review() {
    let (_temp, artifacts) = fixture();
    let first = key("write");
    let workspace = artifacts.prepare_workspace(&first, &[]).unwrap();
    fs::write(workspace.join("report.md"), "Original draft\n").unwrap();
    let original = freeze_node(&artifacts, &first, &[]).await;
    let original_mount = artifacts
        .input_mounts(std::slice::from_ref(&original), "run-1", "digest-1")
        .unwrap()
        .remove(0);
    let original_head = git(&original_mount.source, &["rev-parse", "HEAD"]);
    assert_eq!(
        git(&original_mount.source, &["show", "HEAD:report.md"]),
        "Original draft"
    );
    assert_eq!(
        artifacts.freeze(&first, &completion()).await.unwrap(),
        original
    );
    assert_eq!(
        git(&original_mount.source, &["rev-parse", "HEAD"]),
        original_head
    );

    let second = InvocationKey {
        invocation: 2,
        ..first
    };
    let workspace = artifacts
        .prepare_workspace(&second, std::slice::from_ref(&original))
        .unwrap();
    fs::write(workspace.join("report.md"), "Revised draft\n").unwrap();
    let revised = freeze_node(&artifacts, &second, std::slice::from_ref(&original)).await;
    let revised_mount = artifacts
        .input_mounts(std::slice::from_ref(&revised), "run-1", "digest-1")
        .unwrap()
        .remove(0);
    let revised_head = git(&revised_mount.source, &["rev-parse", "HEAD"]);
    assert_ne!(original_head, revised_head);
    assert_eq!(
        git(&revised_mount.source, &["rev-parse", "HEAD^"]),
        original_head
    );
    assert!(
        git(&revised_mount.source, &["diff", &original_head, "HEAD"]).contains("+Revised draft")
    );
    assert_eq!(
        git(&original_mount.source, &["rev-parse", "HEAD"]),
        original_head
    );
    assert_eq!(
        git(&original_mount.source, &["rev-list", "--count", "HEAD"]),
        "1"
    );
    fs::remove_dir_all(artifacts.root.join(&original.id).join("git-view")).unwrap();
    let rebuilt = artifacts
        .input_mounts(std::slice::from_ref(&original), "run-1", "digest-1")
        .unwrap()
        .remove(0);
    assert_eq!(git(&rebuilt.source, &["rev-parse", "HEAD"]), original_head);

    // Exercise the existing business validator unchanged, using real host Git
    // identities. Python is only a test dependency for this legacy script.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let script = "import importlib.util, sys\nspec = importlib.util.spec_from_file_location('review_gate', sys.argv[1])\ngate = importlib.util.module_from_spec(spec)\nspec.loader.exec_module(gate)\nreview = {'decision':'publish', 'reviewed_commit':sys.argv[2], 'summary':'Approved', 'checks':dict.fromkeys(['facts','business','reasoning','writing'], True), 'issues':[]}\nassert gate.validate(review, {}, sys.argv[2]) == 'publish'\ntry:\n gate.validate(review, {}, sys.argv[3])\nexcept ValueError as error:\n assert 'current manuscript commit' in str(error)\nelse:\n raise AssertionError('stale review accepted')\n";
    let output = Command::new("python3")
        .args(["-c", script])
        .arg(root.join("scripts/weekly_work_report/review_gate.py"))
        .args([&original_head, &revised_head])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn agent_git_configuration_is_ignored_and_nested_business_files_survive() {
    let (temp, artifacts) = fixture();
    let first = key("write");
    let workspace = artifacts.prepare_workspace(&first, &[]).unwrap();
    fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
    let sentinel = temp.path().join("untrusted-hook-ran");
    fs::write(
        workspace.join(".git/config"),
        format!(
            "[core]\n hooksPath = {}\n",
            workspace.join(".git/hooks").display()
        ),
    )
    .unwrap();
    fs::write(
        workspace.join(".git/hooks/post-commit"),
        format!("#!/bin/sh\ntouch {}\n", sentinel.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            workspace.join(".git/hooks/post-commit"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    fs::create_dir_all(workspace.join("business/.git")).unwrap();
    fs::write(
        workspace.join("business/.git/reference.txt"),
        "business content",
    )
    .unwrap();
    fs::write(workspace.join(".gitignore"), "report.md\n").unwrap();
    fs::write(workspace.join(".gitattributes"), "*.md filter=untrusted\n").unwrap();
    fs::write(workspace.join("report.md"), "must be included").unwrap();
    let commit = freeze_node(&artifacts, &first, &[]).await;
    let files = artifacts.files_path(&commit).unwrap();
    assert!(!files.join(".git").exists());
    assert!(files.join("business/.git/reference.txt").is_file());
    let mount = artifacts
        .input_mounts(std::slice::from_ref(&commit), "run-1", "digest-1")
        .unwrap()
        .remove(0);
    assert_eq!(
        git(&mount.source, &["show", "HEAD:report.md"]),
        "must be included"
    );
    assert_eq!(
        git(&mount.source, &["show", "HEAD:business/.git/reference.txt"]),
        "business content"
    );
    assert!(!sentinel.exists());
}

#[tokio::test]
async fn git_projection_tampering_never_changes_authoritative_snapshot() {
    for corruption in ["file", "head", "config", "hook", "manifest", "half"] {
        let (_temp, artifacts) = fixture();
        let first = key("write");
        let workspace = artifacts.prepare_workspace(&first, &[]).unwrap();
        fs::write(workspace.join("report.md"), "original").unwrap();
        let commit = freeze_node(&artifacts, &first, &[]).await;
        let source = artifacts
            .input_mounts(std::slice::from_ref(&commit), "run-1", "digest-1")
            .unwrap()
            .remove(0)
            .source;
        match corruption {
            "file" => fs::write(source.join("report.md"), "tampered").unwrap(),
            "head" => fs::write(
                source.join(".git/HEAD"),
                "0000000000000000000000000000000000000000\n",
            )
            .unwrap(),
            "config" => fs::write(
                source.join(".git/config"),
                "[include]\n path = /untrusted\n",
            )
            .unwrap(),
            "hook" => {
                fs::create_dir(source.join(".git/hooks")).unwrap();
            }
            "manifest" => {
                fs::write(source.parent().unwrap().join("projection.json"), "{}").unwrap()
            }
            "half" => fs::remove_file(source.parent().unwrap().join("projection.json")).unwrap(),
            _ => unreachable!(),
        }
        assert!(
            artifacts
                .input_mounts(std::slice::from_ref(&commit), "run-1", "digest-1")
                .is_err(),
            "{corruption}"
        );
        assert_eq!(
            fs::read_to_string(artifacts.files_path(&commit).unwrap().join("report.md")).unwrap(),
            "original"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_seed_and_git_projection_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    let (temp, artifacts) = fixture();
    let first = key("write");
    let workspace = artifacts.prepare_workspace(&first, &[]).unwrap();
    fs::write(workspace.join("report.md"), "original").unwrap();
    let commit = freeze_node(&artifacts, &first, &[]).await;
    let source = artifacts
        .input_mounts(std::slice::from_ref(&commit), "run-1", "digest-1")
        .unwrap()
        .remove(0)
        .source;
    fs::remove_file(source.join("report.md")).unwrap();
    symlink(workspace.join("report.md"), source.join("report.md")).unwrap();
    assert!(
        artifacts
            .input_mounts(std::slice::from_ref(&commit), "run-1", "digest-1")
            .is_err()
    );

    let second = InvocationKey {
        invocation: 2,
        ..first
    };
    let second_workspace = artifacts.workspace_path(&second).unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(outside, second_workspace).unwrap();
    assert!(artifacts.prepare_workspace(&second, &[commit]).is_err());
}
