use super::{tests::record, tests::write, *};
use crate::assistant::TurnBinding;
use anchor_runtime::graph::{
    CommitRef, NodeCompletion, ParallelActivation, ParallelBranchRecord, ParallelBranchStatus,
    RunCursor, RunResult,
};
use serde_json::json;

fn key(record: &GraphRunRecord, invocation: u64) -> InvocationKey {
    InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "agent".into(),
        invocation,
    }
}

fn turn(number: u64) -> String {
    format!("{number:08x}-0000-4000-8000-{number:012x}")
}

fn cursor(key: InvocationKey) -> RunCursor {
    RunCursor {
        node_id: key.node_id.clone(),
        key,
        input_commits: vec![],
        prepared_input: json!({}),
    }
}

fn result(key: InvocationKey) -> RunResult {
    RunResult {
        node_id: key.node_id.clone(),
        completion: NodeCompletion {
            submission: "fixture".into(),
            route: None,
            model_requests: 0,
            output: json!({"turn":turn(90), "filename":"../other"}),
        },
        interruption: None,
        commit: CommitRef {
            id: format!("fs2-{}", key_hash(&key)),
            node_id: key.node_id.clone(),
            invocation: key.invocation,
        },
        key,
        sequence: 1,
    }
}

fn binding(key: &InvocationKey, turn: &str) -> TurnBinding {
    serde_json::from_value(json!({
        "key":key, "wait_key":"../ignored", "inbound":"../ignored", "turn":turn,
    }))
    .unwrap()
}

fn binding_path(data: &Path, key: &InvocationKey) -> PathBuf {
    data.join("assistant-invocations")
        .join(format!("{}.json", key_hash(key)))
}

fn install(data: &Path, work: &Path, key: &InvocationKey, turn: &str) -> Vec<PathBuf> {
    let hash = key_hash(key);
    let node_hash = format!("{:x}", Sha256::digest(key.node_id.as_bytes()));
    let mut paths = vec![
        data.join("assistant-seeds")
            .join(format!("nf1-{hash}.json")),
        data.join("facts").join(format!("nf1-{hash}.yielded.json")),
        data.join("facts").join(format!("nf1-{hash}.json")),
        data.join("goose-acp").join(format!("{hash}.json")),
        data.join("channel-inputs")
            .join(format!("turn-{turn}/files/input.bin")),
        data.join("channel-replies")
            .join(format!("turn-{turn}.json")),
        data.join("channel-replies")
            .join(format!("turn-{turn}.tmp")),
        work.join(&key.run_id)
            .join("nodes")
            .join(node_hash)
            .join(format!("{}.txt", key.invocation)),
    ];
    for path in &paths {
        write(path, b"owned");
    }
    let path = binding_path(data, key);
    crate::assistant::save_immutable(&path, &binding(key, turn)).unwrap();
    paths.push(path);
    paths
}

fn saved_files(paths: &[PathBuf]) -> Vec<(PathBuf, Vec<u8>)> {
    paths
        .iter()
        .map(|path| (path.clone(), std::fs::read(path).unwrap()))
        .collect()
}

fn assert_saved(files: &[(PathBuf, Vec<u8>)]) {
    for (path, bytes) in files {
        assert_eq!(std::fs::read(path).unwrap(), *bytes, "{}", path.display());
    }
}

#[test]
fn cleanup_enumerates_cursor_results_and_uncommitted_bindings_without_crossing_runs() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let work = root.path().join("work");
    let mut target = record("target");
    let other = record("other");
    target.cursor = Some(cursor(key(&target, 3)));
    target
        .results
        .insert("agent".into(), vec![result(key(&target, 5))]);
    target.parallel = Some(ParallelActivation {
        activation_id: "fixture".into(),
        fanout_node: "agent".into(),
        join_node: "agent".into(),
        fanout_invocation: 1,
        branches: vec![ParallelBranchRecord {
            branch_id: "branch".into(),
            entry: "agent".into(),
            nodes: vec!["agent".into()],
            next_index: 0,
            status: ParallelBranchStatus::Running,
            cursor: Some(cursor(key(&target, 7))),
            completed: vec![],
            error: None,
        }],
    });
    let artifacts = crate::HostArtifacts::new(data.join("artifacts"), work.clone());
    artifacts
        .bind_node_workspace(&target.run_id, &target.graph_digest, "agent")
        .unwrap();
    let mut owned = Vec::new();
    for invocation in [1, 3, 5, 7, 9] {
        owned.extend(install(
            &data,
            &work,
            &key(&target, invocation),
            &turn(invocation),
        ));
    }
    let other_paths = install(&data, &work, &key(&other, 1), &turn(90));
    let other_saved = saved_files(&other_paths);
    let own_artifact = data
        .join("artifacts")
        .join(&target.results["agent"][0].commit.id);
    write(&own_artifact.join("files/output.txt"), b"artifact");
    write(&own_artifact.with_extension("json"), b"artifact");
    let foreign_artifact = data.join("artifacts/foreign/files/output.txt");
    write(&foreign_artifact, b"foreign");

    delete_run_data(&target, &data, &work).unwrap();
    delete_run_data(&target, &data, &work).unwrap();

    assert!(owned.iter().all(|path| !path.exists()));
    assert!(!work.join(&target.run_id).exists());
    assert!(!own_artifact.exists());
    assert!(!own_artifact.with_extension("json").exists());
    assert_saved(&other_saved);
    assert_eq!(std::fs::read(foreign_artifact).unwrap(), b"foreign");
}

#[test]
fn pending_inbounds_and_model_values_do_not_authorize_other_turn_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let work = root.path().join("work");
    let mut target = record("target");
    let other = record("other");
    let target_key = key(&target, 1);
    let other_paths = install(&data, &work, &key(&other, 1), &turn(90));
    let mut target_paths = install(&data, &work, &target_key, &turn(1));
    let copied_key = key(&target, 2);
    target_paths.extend(install(&data, &work, &copied_key, &turn(1)));
    let pending_input = data.join("channel-inputs/other-pending/files/input.bin");
    write(&pending_input, b"pending");
    let mut fact = binding(&target_key, &turn(1));
    fact.pending_inbounds = vec![turn(90), "other-pending".into(), "../outside".into()];
    write(
        &binding_path(&data, &target_key),
        &serde_json::to_vec(&fact).unwrap(),
    );
    fact.key = copied_key.clone();
    write(
        &binding_path(&data, &copied_key),
        &serde_json::to_vec(&fact).unwrap(),
    );
    target.input = json!({"turn":turn(90), "pending_inbounds":[turn(90)]});
    target
        .results
        .insert("agent".into(), vec![result(target_key)]);
    let other_saved = saved_files(&other_paths);

    delete_run_data(&target, &data, &work).unwrap();
    delete_run_data(&target, &data, &work).unwrap();

    assert!(target_paths.iter().all(|path| !path.exists()));
    assert_saved(&other_saved);
    assert_eq!(std::fs::read(pending_input).unwrap(), b"pending");
}

#[test]
fn partial_delete_keeps_binding_authority_until_cleanup_can_be_replayed() {
    for blocked in ["reply", "seed", "yielded", "fact", "workspace"] {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let work = root.path().join("work");
        let mut target = record("target");
        target.invocations.clear();
        let bound_key = key(&target, 9);
        let paths = install(&data, &work, &bound_key, &turn(9));
        let other_paths = install(&data, &work, &key(&record("other"), 1), &turn(90));
        let other_saved = saved_files(&other_paths);
        let blocker = match blocked {
            "reply" => data
                .join("channel-replies")
                .join(format!("turn-{}.json", turn(9))),
            "seed" => data
                .join("assistant-seeds")
                .join(format!("nf1-{}.json", key_hash(&bound_key))),
            "yielded" => data
                .join("facts")
                .join(format!("nf1-{}.yielded.json", key_hash(&bound_key))),
            "fact" => data
                .join("facts")
                .join(format!("nf1-{}.json", key_hash(&bound_key))),
            "workspace" => work.join(&target.run_id),
            _ => unreachable!(),
        };
        if blocked == "workspace" {
            std::fs::remove_dir_all(&blocker).unwrap();
            write(&blocker, b"blocker");
        } else {
            std::fs::remove_file(&blocker).unwrap();
            std::fs::create_dir(&blocker).unwrap();
        }

        assert!(delete_run_data(&target, &data, &work).is_err(), "{blocked}");
        assert!(binding_path(&data, &bound_key).exists(), "{blocked}");
        assert!(
            !data
                .join("channel-inputs")
                .join(format!("turn-{}", turn(9)))
                .exists()
        );
        assert_saved(&other_saved);

        if blocked == "workspace" {
            std::fs::remove_file(blocker).unwrap();
        } else {
            std::fs::remove_dir(blocker).unwrap();
        }
        delete_run_data(&target, &data, &work).unwrap();
        delete_run_data(&target, &data, &work).unwrap();
        assert!(paths.iter().all(|path| !path.exists()), "{blocked}");
        assert_saved(&other_saved);
    }
}

#[test]
fn invalid_known_bindings_fail_before_any_owned_or_foreign_files_are_deleted() {
    for invalid in ["run", "digest", "turn-path", "turn-name"] {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let work = root.path().join("work");
        let target = record("target");
        let target_key = key(&target, 1);
        let target_paths = install(&data, &work, &target_key, &turn(1));
        let other_paths = install(&data, &work, &key(&record("other"), 1), &turn(90));
        let mut fact = binding(&target_key, &turn(1));
        match invalid {
            "run" => fact.key.run_id = "other".into(),
            "digest" => fact.key.graph_digest = "f".repeat(64),
            "turn-path" => fact.turn = "../other".into(),
            "turn-name" => fact.turn = "other".into(),
            _ => unreachable!(),
        }
        write(
            &binding_path(&data, &target_key),
            &serde_json::to_vec(&fact).unwrap(),
        );
        let target_saved = saved_files(&target_paths);
        let other_saved = saved_files(&other_paths);

        assert!(delete_run_data(&target, &data, &work).is_err(), "{invalid}");
        assert_saved(&target_saved);
        assert_saved(&other_saved);
    }
}

#[test]
fn discovery_ignores_unowned_or_misnamed_facts_and_filters_foreign_record_keys() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let work = root.path().join("work");
    let mut target = record("target");
    let target_paths = install(&data, &work, &key(&target, 1), &turn(1));
    let foreign_key = key(&record("other"), 1);
    target.cursor = Some(cursor(foreign_key.clone()));
    target
        .results
        .insert("agent".into(), vec![result(foreign_key.clone())]);
    let mut preserved = install(&data, &work, &foreign_key, &turn(90));
    let mut wrong_digest = key(&target, 11);
    wrong_digest.graph_digest = "f".repeat(64);
    let mut missing_node = key(&target, 12);
    missing_node.node_id = "not-in-snapshot".into();
    for unowned in [wrong_digest, missing_node] {
        let path = binding_path(&data, &unowned);
        write(
            &path,
            &serde_json::to_vec(&binding(&unowned, &turn(90))).unwrap(),
        );
        preserved.push(path);
        let fact = data
            .join("facts")
            .join(format!("nf1-{}.yielded.json", key_hash(&unowned)));
        write(&fact, b"unowned");
        preserved.push(fact);
    }
    for name in [
        format!("{}.json", "a".repeat(64)),
        "model-filename.json".into(),
        format!("{}.json", "b".repeat(64)),
    ] {
        let path = data.join("assistant-invocations").join(name);
        let bytes = if path.file_name().unwrap() == format!("{}.json", "b".repeat(64)).as_str() {
            b"invalid json".to_vec()
        } else {
            serde_json::to_vec(&binding(&key(&target, 99), &turn(90))).unwrap()
        };
        write(&path, &bytes);
        preserved.push(path);
    }
    let saved = saved_files(&preserved);

    assert_eq!(invocation_keys(&target), vec![key(&target, 1)]);
    delete_run_data(&target, &data, &work).unwrap();
    delete_run_data(&target, &data, &work).unwrap();

    assert!(target_paths.iter().all(|path| !path.exists()));
    assert_saved(&saved);
}

#[cfg(unix)]
#[test]
fn assistant_cleanup_does_not_follow_symlinked_fact_or_reply_directories() {
    for directory in [
        "assistant-invocations",
        "assistant-seeds",
        "channel-replies",
    ] {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let work = root.path().join("work");
        let target = record("target");
        let target_key = key(&target, 1);
        install(&data, &work, &target_key, &turn(1));
        let source = data.join(directory);
        let outside = root.path().join("outside");
        std::fs::rename(&source, &outside).unwrap();
        let paths = std::fs::read_dir(&outside)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        let saved = saved_files(&paths);
        std::os::unix::fs::symlink(&outside, &source).unwrap();

        assert!(
            delete_run_data(&target, &data, &work).is_err(),
            "{directory}"
        );
        assert_saved(&saved);
        assert!(binding_path(&data, &target_key).exists());
    }
}
