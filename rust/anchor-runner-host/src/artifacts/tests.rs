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
    completion_with_files(&["nested", "report draft.txt"])
}
pub(super) fn completion_with_files(files: &[&str]) -> NodeCompletion {
    NodeCompletion {
        submission: "done".into(),
        route: None,
        model_requests: 0,
        output: json!({"result":"ok","files":files}),
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
    let workspace = artifacts.prepare_workspace(key, &[]).unwrap();
    fs::create_dir_all(workspace.join("nested/empty")).unwrap();
    fs::write(workspace.join("nested/report.txt"), "original").unwrap();
    fs::write(workspace.join("report draft.txt"), "spaces are valid").unwrap();
    workspace
}

#[tokio::test]
async fn selected_delivery_is_independent_and_retry_ignores_changed_or_missing_workspace() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    let (temp, artifacts) = fixture();
    let invocation = key("work");
    let path = workspace(&artifacts, &invocation);
    let bytes = (0..FILE_BUFFER_BYTES * 3 + 19)
        .map(|offset| (offset % 251) as u8)
        .collect::<Vec<_>>();
    fs::write(path.join("nested/program"), &bytes).unwrap();
    let mut report = vec![0_u8; 2 * 1024 * 1024];
    fs::write(path.join("nested/iteration-report.bin"), &report).unwrap();
    fs::set_permissions(
        path.join("nested/program"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::hard_link(
        path.join("nested/report.txt"),
        temp.path().join("other-link"),
    )
    .unwrap();
    let ignored = fs::File::create(path.join("unselected-20GiB")).unwrap();
    ignored.set_len(20 * 1024 * 1024 * 1024).unwrap();
    fs::create_dir_all(path.join("node_modules/cache")).unwrap();
    symlink("/missing", path.join("node_modules/cache/link")).unwrap();
    let completion = completion();
    let commit = artifacts.freeze(&invocation, &completion).await.unwrap();
    assert_eq!(commit.id, format!("fs2-{}", key_hash(&invocation)));
    let view = artifacts.files_path(&commit).unwrap();
    assert!(!view.join("unselected-20GiB").exists());
    assert!(!view.join("node_modules").exists());
    assert!(view.join("nested/empty").is_dir());
    assert_eq!(fs::read(view.join("nested/program")).unwrap(), bytes);
    assert_ne!(
        fs::metadata(view.join("nested/program")).unwrap().ino(),
        fs::metadata(path.join("nested/program")).unwrap().ino()
    );
    assert_ne!(
        fs::metadata(view.join("nested/program"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    let descriptor = ArtifactDescriptor::read(&view).unwrap();
    assert_eq!(descriptor.artifact, commit);
    assert_eq!(
        descriptor.files["nested/program"].sha256,
        format!("{:x}", Sha256::digest(&bytes))
    );
    assert_eq!(
        descriptor.read_file(&view, "nested/report.txt").unwrap(),
        b"original"
    );
    let mut previous = commit.clone();
    for ordinal in 2..=100 {
        let current = InvocationKey {
            invocation: ordinal,
            ..invocation.clone()
        };
        assert_eq!(
            artifacts
                .prepare_workspace(&current, std::slice::from_ref(&previous))
                .unwrap(),
            path
        );
        report[0] = ordinal as u8;
        fs::write(path.join("nested/iteration-report.bin"), &report).unwrap();
        previous = artifacts
            .freeze_with_context(
                &current,
                &completion,
                &ArtifactFreezeContext {
                    kind: ArtifactKind::Node,
                    input_commits: vec![previous],
                },
            )
            .await
            .unwrap();
    }
    assert_eq!(
        fs::read_dir(artifacts.root.join("objects"))
            .unwrap()
            .count(),
        103
    );
    assert_eq!(
        descriptor.files["nested/iteration-report.bin"].bytes,
        2 * 1024 * 1024
    );
    fs::write(temp.path().join("other-link"), "changed").unwrap();
    assert_eq!(
        artifacts.freeze(&invocation, &completion).await.unwrap(),
        commit
    );
    fs::rename(path, temp.path().join("moved-workspace-for-cleanup")).unwrap();
    assert_eq!(
        artifacts.freeze(&invocation, &completion).await.unwrap(),
        commit
    );
    assert_eq!(
        descriptor.read_file(&view, "nested/report.txt").unwrap(),
        b"original"
    );
    let changed = NodeCompletion {
        output: json!({"files":[]}),
        ..completion
    };
    assert!(matches!(
        artifacts.freeze(&invocation, &changed).await,
        Err(GraphError::RunConflict)
    ));
}

#[tokio::test]
async fn metadata_listing_and_resolution_do_not_read_or_require_content() {
    use std::io::{Seek, SeekFrom};
    use std::os::unix::fs::PermissionsExt;
    let (_temp, artifacts) = fixture();
    let invocation = key("work");
    workspace(&artifacts, &invocation);
    let commit = artifacts.freeze(&invocation, &completion()).await.unwrap();
    let (_, manifest) = artifacts.load_snapshot(&commit).unwrap();
    let object = artifacts
        .object_path(&manifest.files["nested/report.txt"])
        .unwrap();
    fs::set_permissions(&object, fs::Permissions::from_mode(0o644)).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(&object).unwrap();
    file.seek(SeekFrom::Start(5)).unwrap();
    file.write_all(b"X").unwrap();
    assert_eq!(artifacts.list_files(&commit).unwrap().len(), 2);
    assert_eq!(
        artifacts.resolve(&commit).await.unwrap(),
        serde_json::to_value(completion()).unwrap()
    );
    assert!(artifacts.open_file(&commit, "nested/report.txt").is_err());
    fs::remove_file(object).unwrap();
    assert_eq!(
        artifacts.availability(&commit).unwrap(),
        ArtifactAvailability::Missing
    );
    assert_eq!(artifacts.list_files(&commit).unwrap().len(), 2);
    assert!(artifacts.files_path(&commit).is_err());
}

#[tokio::test]
async fn selected_links_special_files_descriptor_and_missing_paths_are_rejected() {
    use std::os::unix::fs::symlink;
    let (_temp, artifacts) = fixture();
    let invocation = key("work");
    let path = workspace(&artifacts, &invocation);
    symlink("nested/report.txt", path.join("link")).unwrap();
    let short = tempfile::tempdir().unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(short.path().join("s")).unwrap();
    fs::rename(short.path().join("s"), path.join("socket")).unwrap();
    fs::write(path.join(ARTIFACT_DESCRIPTOR_FILE), "collision").unwrap();
    for invalid in [
        "link",
        "socket",
        "missing",
        ARTIFACT_DESCRIPTOR_FILE,
        "../escape",
        ".",
        "nested/*",
    ] {
        assert!(
            artifacts
                .freeze(&invocation, &completion_with_files(&[invalid]))
                .await
                .is_err(),
            "{invalid}"
        );
    }
    assert!(
        validate_selection(
            &path,
            &ArtifactSelection::new(vec!["nested/report.txt".into()]).unwrap(),
            &["report draft.txt".into()]
        )
        .is_err()
    );
    let commit = artifacts
        .freeze(&invocation, &completion_with_files(&[]))
        .await
        .unwrap();
    assert!(artifacts.list_files(&commit).unwrap().is_empty());
}

#[tokio::test]
async fn objects_deduplicate_bytes_with_separate_executable_view_modes_and_safe_expiry() {
    use std::collections::BTreeSet;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let (_temp, artifacts) = fixture();
    let first_key = key("first");
    let first_ws = artifacts.prepare_workspace(&first_key, &[]).unwrap();
    fs::write(first_ws.join("program"), "same bytes").unwrap();
    let first = artifacts
        .freeze(&first_key, &completion_with_files(&["program"]))
        .await
        .unwrap();
    let second_key = key("second");
    let second_ws = artifacts.prepare_workspace(&second_key, &[]).unwrap();
    fs::write(second_ws.join("program"), "same bytes").unwrap();
    fs::set_permissions(second_ws.join("program"), fs::Permissions::from_mode(0o755)).unwrap();
    let second = artifacts
        .freeze(&second_key, &completion_with_files(&["program"]))
        .await
        .unwrap();
    assert_eq!(
        fs::read_dir(artifacts.root.join("objects"))
            .unwrap()
            .count(),
        1
    );
    let first_view = artifacts.files_path(&first).unwrap();
    let second_view = artifacts.files_path(&second).unwrap();
    assert_eq!(
        fs::metadata(first_view.join("program"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_ne!(
        fs::metadata(second_view.join("program"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_ne!(
        fs::metadata(first_view.join("program")).unwrap().ino(),
        fs::metadata(second_view.join("program")).unwrap().ino()
    );
    let protected = BTreeSet::from([second.id.clone()]);
    let report = artifacts
        .collect(&protected, &[first.id.clone(), second.id.clone()])
        .unwrap();
    assert_eq!(report.expired.as_slice(), std::slice::from_ref(&first.id));
    assert_eq!(report.removed_objects, 0);
    assert_eq!(
        artifacts.availability(&first).unwrap(),
        ArtifactAvailability::Expired
    );
    assert_eq!(
        artifacts.availability(&second).unwrap(),
        ArtifactAvailability::Available
    );
    assert_eq!(
        artifacts.list_files(&first).unwrap(),
        [("program".to_owned(), 10)]
    );
    assert!(artifacts.files_path(&first).is_err());
    assert_eq!(
        fs::read(artifacts.files_path(&second).unwrap().join("program")).unwrap(),
        b"same bytes"
    );
    let report = artifacts.collect(&BTreeSet::new(), &[second.id]).unwrap();
    assert_eq!(report.removed_objects, 1);
    assert!(report.freed_bytes >= 20);
    assert_eq!(artifacts.storage_bytes().unwrap(), report.remaining_bytes);
}

#[tokio::test]
async fn download_guard_protects_only_its_delivery_until_stream_finishes() {
    use anchor_file_lease::{FileLease, FlockOperation};
    use std::collections::BTreeSet;
    let (_temp, artifacts) = fixture();
    let invocation = key("work");
    workspace(&artifacts, &invocation);
    let commit = artifacts.freeze(&invocation, &completion()).await.unwrap();
    let read = artifacts.open_file(&commit, "nested/report.txt").unwrap();
    assert_eq!(read.bytes, 8);
    assert!(
        FileLease::acquire(
            fs::File::options()
                .write(true)
                .open(artifacts.root.join(".store.lock"))
                .unwrap(),
            FlockOperation::NonBlockingLockExclusive
        )
        .is_ok()
    );
    let pending = artifacts
        .collect(&BTreeSet::new(), std::slice::from_ref(&commit.id))
        .unwrap();
    assert!(pending.expired.is_empty());
    assert_eq!(pending.busy.as_slice(), std::slice::from_ref(&commit.id));
    let other = key("other");
    workspace(&artifacts, &other);
    let other_commit = artifacts.freeze(&other, &completion()).await.unwrap();
    assert_eq!(
        artifacts
            .collect(&BTreeSet::new(), &[other_commit.id])
            .unwrap()
            .expired
            .len(),
        1
    );
    let (file, lease) = read.into_parts();
    assert_eq!(hash_copy(file, std::io::sink()).unwrap().1, 8);
    drop(lease);
    assert_eq!(
        artifacts
            .collect(&BTreeSet::new(), &[commit.id])
            .unwrap()
            .expired
            .len(),
        1
    );
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
    let parent_key = key("module");
    workspace(&artifacts, &parent_key);
    let parent = artifacts.freeze(&parent_key, &completion()).await.unwrap();
    assert!(
        artifacts
            .input_mounts(&[first.clone(), parent], "run-1", "digest-1")
            .is_err()
    );
    assert!(
        artifacts
            .files_path(&CommitRef {
                invocation: 2,
                ..first
            })
            .is_err()
    );
}
