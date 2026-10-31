use super::*;

fn key(run: &str, invocation: u64) -> InvocationKey {
    InvocationKey {
        run_id: run.into(),
        graph_digest: "graph-digest".into(),
        node_id: "module/work".into(),
        invocation,
    }
}

#[test]
fn every_node_continues_its_scene_and_new_runs_are_isolated() {
    let root = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new(root.path().into());
    let first = key("first", 1);
    let path = workspaces.prepare(&first).unwrap();
    let lease = workspaces.acquire(&first).unwrap();
    fs::create_dir(path.join("dependencies")).unwrap();
    fs::write(path.join("dependencies/cache"), b"unselected cache").unwrap();
    std::os::unix::fs::symlink("/nonexistent", path.join("local-link")).unwrap();
    lease.finish_published().unwrap();
    let second = key("first", 2);
    assert_eq!(workspaces.prepare(&second).unwrap(), path);
    let lease = workspaces.acquire(&second).unwrap();
    assert_eq!(
        fs::read(path.join("dependencies/cache")).unwrap(),
        b"unselected cache"
    );
    assert!(
        fs::symlink_metadata(path.join("local-link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let isolated = key("other", 1);
    let other = workspaces.prepare(&isolated).unwrap();
    assert_ne!(path, other);
    let independent = workspaces.acquire(&isolated).unwrap();
    independent.finish_interrupted().unwrap();
    lease.finish_interrupted().unwrap();
}

#[test]
fn execution_and_unpublished_candidates_block_transfer_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new(root.path().into());
    let first = key("first", 1);
    let path = workspaces.prepare(&first).unwrap();
    let execution = workspaces.acquire(&first).unwrap();
    fs::write(path.join("draft"), b"pending work").unwrap();
    let resource = workspaces.resource(&first).unwrap();
    let target = key("target", 1);
    assert!(matches!(
        workspaces.acquire(&first),
        Err(GraphError::RunConflict)
    ));
    assert!(matches!(
        workspaces.bind_resource(&target, &resource),
        Err(GraphError::RunConflict)
    ));
    drop(execution); // A dropped process lock does not release the candidate.
    let restarted = Workspaces::new(root.path().into());
    assert!(matches!(
        restarted.acquire(&key("first", 2)),
        Err(GraphError::RunConflict)
    ));
    assert!(matches!(
        restarted.bind_resource(&target, &resource),
        Err(GraphError::RunConflict)
    ));
    let resumed = restarted.acquire(&first).unwrap();
    assert_eq!(fs::read(path.join("draft")).unwrap(), b"pending work");
    resumed.finish_interrupted().unwrap();
    restarted.bind_resource(&target, &resource).unwrap();
    assert_eq!(restarted.prepare(&target).unwrap(), path);
    let target_execution = restarted.acquire(&target).unwrap();
    assert!(matches!(
        restarted.acquire(&key("first", 2)),
        Err(GraphError::RunConflict)
    ));
    target_execution.finish_published().unwrap();
}

#[test]
fn explicit_binding_keeps_the_same_live_directory_through_run_deletion() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new(root.path().into());
    let first = key("first", 1);
    let source = workspaces.prepare(&first).unwrap();
    let lease = workspaces.acquire(&first).unwrap();
    fs::write(source.join("draft"), b"current work").unwrap();
    let inode = fs::metadata(&source).unwrap().ino();
    lease.finish_published().unwrap();
    let resource = workspaces.resource(&first).unwrap();
    let target = key("target", 1);
    workspaces.prepare(&target).unwrap(); // Admission may prepare an empty default.
    workspaces.bind_resource(&target, &resource).unwrap();
    workspaces.bind_resource(&target, &resource).unwrap();
    let path = workspaces.prepare(&target).unwrap();
    assert_eq!(path, source);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    let lease = workspaces.acquire(&target).unwrap();
    workspaces.release_run("first").unwrap();
    assert_eq!(fs::read(path.join("draft")).unwrap(), b"current work");
    assert!(matches!(
        workspaces.release_resource(&resource),
        Err(GraphError::RunConflict)
    ));
    lease.finish_interrupted().unwrap();
    workspaces.release_run("target").unwrap();
    assert!(path.is_dir());
    workspaces.release_resource(&resource).unwrap();
    assert!(!path.exists());
}

#[test]
fn binding_never_replaces_existing_work_or_accepts_a_host_path() {
    let root = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new(root.path().into());
    let source = key("source", 1);
    workspaces.prepare(&source).unwrap();
    let resource = workspaces.resource(&source).unwrap();
    let target = key("target", 1);
    let target_path = workspaces.prepare(&target).unwrap();
    fs::write(target_path.join("local-work"), b"keep").unwrap();
    assert!(matches!(
        workspaces.bind_resource(&target, &resource),
        Err(GraphError::RunConflict)
    ));
    assert!(workspaces.bind_resource(&target, "/tmp/scene").is_err());
    assert_eq!(fs::read(target_path.join("local-work")).unwrap(), b"keep");
    assert_eq!(workspaces.bindings().unwrap().len(), 2);
    assert_eq!(workspaces.resources().unwrap().len(), 2);
}
