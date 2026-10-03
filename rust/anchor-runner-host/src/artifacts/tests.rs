use super::*;
use serde_json::json;

pub(super) fn key(node: &str) -> InvocationKey {
    InvocationKey {
        run_id: "run-1".into(),
        graph_digest: "digest-1".into(),
        node_id: node.into(),
        invocation: 1,
    }
}
pub(super) fn completion() -> NodeCompletion {
    NodeCompletion {
        submission: "done".into(),
        route: None,
        model_requests: 0,
        output: json!({"result":"ok"}),
    }
}
pub(super) fn fixture() -> (tempfile::TempDir, HostArtifacts) {
    let temp = tempfile::tempdir().unwrap();
    let artifacts = HostArtifacts::new(
        temp.path().join("artifacts"),
        temp.path().join("workspaces"),
    );
    (temp, artifacts)
}
pub(super) fn workspace(artifacts: &HostArtifacts, key: &InvocationKey) -> PathBuf {
    let workspace = artifacts.workspace_path(key).unwrap();
    fs::create_dir_all(workspace.join("nested/empty")).unwrap();
    fs::write(workspace.join("nested/report.txt"), "original").unwrap();
    fs::write(workspace.join("report draft.txt"), "spaces are valid").unwrap();
    workspace
}

#[tokio::test]
async fn multi_buffer_file_hash_copy_and_late_chunk_corruption_are_verified() {
    use std::io::{Seek, SeekFrom};

    let (_temp, artifacts) = fixture();
    let key = key("large-output");
    let path = workspace(&artifacts, &key);
    let bytes = (0..FILE_BUFFER_BYTES * 3 + 19)
        .map(|offset| (offset % 251) as u8)
        .collect::<Vec<_>>();
    fs::write(path.join("large.bin"), &bytes).unwrap();
    let commit = artifacts.freeze(&key, &completion()).await.unwrap();
    let (published, manifest) = artifacts.load_snapshot(&commit).unwrap();
    assert_eq!(manifest.files["large.bin"].bytes, bytes.len() as u64);
    assert_eq!(
        manifest.files["large.bin"].sha256,
        format!("{:x}", Sha256::digest(&bytes))
    );
    let snapshot = published.join("files/large.bin");
    assert_eq!(fs::read(&snapshot).unwrap(), bytes);
    let mut file = fs::OpenOptions::new().write(true).open(snapshot).unwrap();
    file.seek(SeekFrom::Start((FILE_BUFFER_BYTES * 3 + 7) as u64))
        .unwrap();
    file.write_all(&[255]).unwrap();
    file.sync_all().unwrap();
    assert!(artifacts.files_path(&commit).is_err());
    assert!(artifacts.resolve(&commit).await.is_err());
    assert!(artifacts.freeze(&key, &completion()).await.is_err());
}

#[tokio::test]
async fn snapshot_is_independent_and_retry_ignores_changed_or_missing_workspace() {
    let (_temp, artifacts) = fixture();
    let key = key("work");
    let path = workspace(&artifacts, &key);
    let completion = completion();
    let commit = artifacts.freeze(&key, &completion).await.unwrap();
    assert_eq!(commit.id, format!("fs2-{}", key_hash(&key)));
    let frozen = artifacts.files_path(&commit).unwrap();
    assert_eq!(
        fs::read_to_string(frozen.join("nested/report.txt")).unwrap(),
        "original"
    );
    fs::write(path.join("nested/report.txt"), "changed").unwrap();
    assert_eq!(artifacts.freeze(&key, &completion).await.unwrap(), commit);
    fs::remove_dir_all(path).unwrap();
    assert_eq!(artifacts.freeze(&key, &completion).await.unwrap(), commit);
    assert_eq!(
        artifacts.resolve(&commit).await.unwrap(),
        serde_json::to_value(&completion).unwrap()
    );
    assert_eq!(
        fs::read_to_string(frozen.join("nested/report.txt")).unwrap(),
        "original"
    );
    let changed = NodeCompletion {
        submission: "different".into(),
        ..completion
    };
    assert!(matches!(
        artifacts.freeze(&key, &changed).await,
        Err(GraphError::RunConflict)
    ));
}

#[tokio::test]
async fn committed_file_listing_comes_from_the_verified_manifest() {
    let (_temp, artifacts) = fixture();
    let key = key("work");
    workspace(&artifacts, &key);
    let commit = artifacts.freeze(&key, &completion()).await.unwrap();

    assert_eq!(
        artifacts.list_files(&commit).unwrap(),
        vec![
            ("nested/report.txt".into(), "original".len() as u64),
            ("report draft.txt".into(), "spaces are valid".len() as u64),
        ]
    );
}

#[tokio::test]
async fn immutable_snapshot_does_not_share_inode_with_hardlinked_workspace_file() {
    let (temp, artifacts) = fixture();
    let key = key("work");
    let path = workspace(&artifacts, &key);
    fs::hard_link(
        path.join("nested/report.txt"),
        temp.path().join("other-link"),
    )
    .unwrap();
    let commit = artifacts.freeze(&key, &completion()).await.unwrap();
    fs::write(temp.path().join("other-link"), "mutated").unwrap();
    assert_eq!(
        fs::read_to_string(
            artifacts
                .files_path(&commit)
                .unwrap()
                .join("nested/report.txt")
        )
        .unwrap(),
        "original"
    );
}

#[tokio::test]
async fn digest_corruption_missing_extra_files_and_manifest_identity_are_rejected() {
    for corruption in [
        "changed",
        "missing",
        "extra",
        "directory",
        "identity",
        "escape",
    ] {
        let (_temp, artifacts) = fixture();
        let key = key("work");
        workspace(&artifacts, &key);
        let commit = artifacts.freeze(&key, &completion()).await.unwrap();
        let files = artifacts.files_path(&commit).unwrap();
        match corruption {
            "changed" => fs::write(files.join("nested/report.txt"), "corrupt").unwrap(),
            "missing" => fs::remove_file(files.join("nested/report.txt")).unwrap(),
            "extra" => fs::write(files.join("extra.txt"), "corrupt").unwrap(),
            "directory" => fs::remove_dir(files.join("nested/empty")).unwrap(),
            _ => {
                let manifest_path = artifacts.root.join(&commit.id).join("manifest.json");
                let mut manifest: Value =
                    serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
                if corruption == "identity" {
                    manifest["key"]["run_id"] = json!("another-run");
                } else {
                    manifest["files"]["../escape"] = json!({"sha256":"x","bytes":1});
                }
                fs::write(manifest_path, manifest.to_string()).unwrap();
            }
        }
        assert!(artifacts.resolve(&commit).await.is_err(), "{corruption}");
        assert!(artifacts.files_path(&commit).is_err(), "{corruption}");
        assert!(
            artifacts.freeze(&key, &completion()).await.is_err(),
            "{corruption}"
        );
    }
}

#[tokio::test]
async fn input_mounts_bind_run_digest_identity_and_reject_overlapping_nodes() {
    let (_temp, artifacts) = fixture();
    let left = key("module/left");
    let right = key("right");
    workspace(&artifacts, &left);
    workspace(&artifacts, &right);
    let first = artifacts.freeze(&left, &completion()).await.unwrap();
    let second = artifacts.freeze(&right, &completion()).await.unwrap();
    let mounts = artifacts
        .input_mounts(&[first.clone(), second], "run-1", "digest-1")
        .unwrap();
    assert_eq!(mounts[0].destination, PathBuf::from("/in/module/left"));
    assert!(
        artifacts
            .input_mounts(std::slice::from_ref(&first), "other-run", "digest-1")
            .is_err()
    );
    assert!(
        artifacts
            .input_mounts(std::slice::from_ref(&first), "run-1", "other-digest")
            .is_err()
    );
    assert!(
        artifacts
            .input_mounts(&[first.clone(), first.clone()], "run-1", "digest-1")
            .is_err()
    );
    let parent = key("module");
    workspace(&artifacts, &parent);
    let parent = artifacts.freeze(&parent, &completion()).await.unwrap();
    assert!(
        artifacts
            .input_mounts(&[first.clone(), parent], "run-1", "digest-1")
            .is_err()
    );
    let forged = CommitRef {
        node_id: "../escape".into(),
        ..first.clone()
    };
    assert!(artifacts.files_path(&forged).is_err());
    let forged = CommitRef {
        invocation: 2,
        ..first
    };
    assert!(artifacts.files_path(&forged).is_err());
}

#[tokio::test]
async fn missing_workspace_is_not_created_and_legacy_completion_has_no_files() {
    let (_temp, artifacts) = fixture();
    let key = key("work");
    let workspace = artifacts.workspace_path(&key).unwrap();
    assert!(!workspace.exists());
    assert!(artifacts.freeze(&key, &completion()).await.is_err());
    assert!(!workspace.exists());
    fs::create_dir_all(&artifacts.root).unwrap();
    let legacy = CommitRef {
        id: "legacy-id".into(),
        node_id: "work".into(),
        invocation: 1,
    };
    fs::write(
        artifacts.root.join("legacy-id.json"),
        serde_json::to_vec(&completion()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        artifacts.resolve(&legacy).await.unwrap(),
        serde_json::to_value(completion()).unwrap()
    );
    assert!(matches!(
        artifacts.files_path(&legacy),
        Err(GraphError::Unsupported(_))
    ));
    assert!(matches!(
        artifacts.input_mounts(&[legacy], "run-1", "digest-1"),
        Err(GraphError::Unsupported(_))
    ));
    let key = InvocationKey {
        run_id: "../escape".into(),
        ..key
    };
    assert!(artifacts.workspace_path(&key).is_err());
    let unsafe_commit = CommitRef {
        id: "../escape".into(),
        node_id: "work".into(),
        invocation: 1,
    };
    assert!(artifacts.resolve(&unsafe_commit).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_and_published_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    let (temp, artifacts) = fixture();
    let key = key("work");
    let path = workspace(&artifacts, &key);
    let outside = temp.path().join("outside");
    fs::write(&outside, "outside").unwrap();
    symlink(&outside, path.join("escape")).unwrap();
    assert!(artifacts.freeze(&key, &completion()).await.is_err());
    fs::remove_file(path.join("escape")).unwrap();
    let commit = artifacts.freeze(&key, &completion()).await.unwrap();
    let snapshot = artifacts.files_path(&commit).unwrap();
    fs::remove_file(snapshot.join("nested/report.txt")).unwrap();
    symlink(outside, snapshot.join("nested/report.txt")).unwrap();
    assert!(artifacts.files_path(&commit).is_err());
    assert!(artifacts.resolve(&commit).await.is_err());

    let (temp, artifacts) = fixture();
    fs::create_dir_all(temp.path().join("outside-directory")).unwrap();
    symlink(
        temp.path().join("outside-directory"),
        &artifacts.workspace_root,
    )
    .unwrap();
    assert!(artifacts.workspace_path(&key).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn special_workspace_files_are_rejected() {
    let (_temp, artifacts) = fixture();
    let key = key("work");
    let path = workspace(&artifacts, &key);
    let _socket = std::os::unix::net::UnixListener::bind(path.join("socket")).unwrap();
    assert!(artifacts.freeze(&key, &completion()).await.is_err());
}
