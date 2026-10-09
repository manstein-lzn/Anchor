use super::*;
use anchor_runtime::graph::{PendingRecovery, RecoveryAttempt, RunCursor};
use std::time::Duration;

fn request(serial: u64, session: &str, previous: Option<&str>) -> Value {
    json!({
        "graph":"fixture",
        "run":format!("channel-00000000-0000-4000-8000-{serial:012x}"),
        "session":session,
        "reply_node":"work",
        "input":{"message":format!("message {serial}")},
        "previous_run":previous,
    })
}

fn gate_definition() -> Value {
    json!({
        "objective":"conversation admission fixture","entry":"work","agents":{},
        "input":{"default_input":"frozen"},
        "ops":{
            "work":{"run":"printf ready > gate.ready; while [ ! -f gate.release ]; do sleep 0.02; done; printf once > reply.txt",
                    "writes":["reply.txt"],"wall_time_limit_seconds":10},
            "finish":{"run":"true"},
        },
        "nodes":[{"id":"work","op":"work"},{"id":"finish","op":"finish"}],
        "edges":[{"from":"work","to":"finish"}],
    })
}

fn configure_gate(root: &std::path::Path, state: &ApiState) {
    set_host_env(root, &state.data_root);
    unsafe {
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh,cat,sleep,printf");
    }
    write_graph_bundle(&state.bundle_root, &gate_definition()).unwrap();
}

async fn wait_gate(state: &ApiState, run: &str) -> PathBuf {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = FileRunStore::new(state.data_root.join("runs"))
                .load(run)
                .unwrap()
                .unwrap();
            assert!(
                !matches!(record.status, RunStatus::Failed | RunStatus::Completed),
                "{record:?}"
            );
            if let Some(cursor) = record.cursor {
                let workspace = HostArtifacts::new(
                    state.data_root.join("artifacts"),
                    state.workspace_root.clone(),
                )
                .workspace_path(&cursor.key)
                .unwrap();
                if workspace.join("gate.ready").is_file() {
                    return workspace;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real Op did not enter its fixture gate")
}

async fn wait_run_status(state: &ApiState, run: &str, expected: RunStatus) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = FileRunStore::new(state.data_root.join("runs"))
                .load(run)
                .unwrap()
                .unwrap();
            if record.status == expected
                && !state
                    .application
                    .active_runs(None)
                    .await
                    .iter()
                    .any(|id| id == run)
            {
                return;
            }
            assert_ne!(record.status, RunStatus::Failed, "{record:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real Run did not settle")
}

async fn stop_run(app: &Router, state: &ApiState, run: &str) {
    let (status, value) = call(app.clone(), "POST", &format!("/runs/{run}/stop"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    wait_run_status(state, run, RunStatus::Stopped).await;
}

#[tokio::test]
async fn conversation_runs_allow_same_graph_two_sessions_and_protect_previous_runs() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    configure_gate(root.path(), &state);
    let app = router(state.clone());
    let alice = request(1, "alice", None);
    let bob = request(2, "bob", None);
    let alice_body = alice.to_string();
    let bob_body = bob.to_string();
    let ((alice_status, alice_value), (bob_status, bob_value)) = tokio::join!(
        call(app.clone(), "POST", "/conversation-runs", Some(&alice_body)),
        call(app.clone(), "POST", "/conversation-runs", Some(&bob_body)),
    );
    assert_eq!(alice_status, StatusCode::ACCEPTED, "{alice_value}");
    assert_eq!(bob_status, StatusCode::ACCEPTED, "{bob_value}");
    let alice_run = alice["run"].as_str().unwrap();
    let bob_run = bob["run"].as_str().unwrap();
    wait_gate(&state, alice_run).await;
    let bob_workspace = wait_gate(&state, bob_run).await;
    assert_eq!(
        state.application.active_runs(Some("fixture")).await.len(),
        2
    );

    let next = request(3, "alice", Some(alice_run));
    let (status, rejected) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&next.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(state.application.records().unwrap().len(), 2);
    let (status, rejected) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    let (status, repeated) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&alice.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{repeated}");
    assert_eq!(repeated["run"], alice["run"]);
    let mut different = alice.clone();
    different["input"]["message"] = json!("different");
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&different.to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    stop_run(&app, &state, alice_run).await;
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&next.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let next_run = next["run"].as_str().unwrap();
    wait_gate(&state, next_run).await;
    let original = std::fs::read(
        state
            .data_root
            .join("runs")
            .join(format!("{alice_run}.json")),
    )
    .unwrap();
    for (method, path, body) in [
        ("POST", format!("/runs/{alice_run}/resume"), None),
        ("DELETE", format!("/runs/{alice_run}"), None),
        (
            "POST",
            format!("/runs/{alice_run}/recovery"),
            Some(r#"{"node_id":"work","invocation":1,"attempt_id":1,"decision":"retry"}"#),
        ),
    ] {
        let (status, rejected) = call(app.clone(), method, &path, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert!(
            rejected["error"]
                .as_str()
                .unwrap()
                .contains("previous turn")
        );
    }
    assert_eq!(
        std::fs::read(
            state
                .data_root
                .join("runs")
                .join(format!("{alice_run}.json"))
        )
        .unwrap(),
        original
    );
    let (_, detail) = call(app.clone(), "GET", &format!("/runs/{next_run}"), None).await;
    assert_eq!(detail["state"]["trigger"]["session"], "alice");
    assert_eq!(detail["state"]["trigger"]["reply_node"], "work");
    assert_eq!(detail["state"]["trigger"]["previous_run"], alice_run);
    let (_, listed) = call(app.clone(), "GET", "/runs", None).await;
    assert_eq!(
        listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["run"] == next_run)
            .unwrap()["trigger"],
        detail["state"]["trigger"]
    );

    stop_run(&app, &state, next_run).await;
    std::fs::write(bob_workspace.join("gate.release"), "release").unwrap();
    wait_run_status(&state, bob_run, RunStatus::Completed).await;
    let shared = state.data_root.join("io-harness/sessions/node.sqlite3");
    std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
    std::fs::write(&shared, "Session-owned record").unwrap();
    // A completed Session turn with nothing newer may be deleted, and that
    // cleanup must still leave Session-owned shared records alone.
    let (status, deleted) = call(app.clone(), "DELETE", &format!("/runs/{bob_run}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "Session-owned record"
    );
    assert!(
        crate::run_deletions::load(&state.data_root, bob_run)
            .unwrap()
            .is_some()
    );
    assert!(
        !state
            .data_root
            .join("runs")
            .join(format!("{bob_run}.json"))
            .exists()
    );
    let (status, deleted) = call(app, "DELETE", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    assert!(state.application.records().unwrap().is_empty());
}

/// A conversation Run is one link of a Session's lineage. A link may be deleted
/// once nothing newer still reads it, in any order: its tombstone keeps the place
/// it held, so the surviving lineage stays a single chain.
#[tokio::test]
async fn conversation_lineage_deletes_any_link_once_nothing_newer_reads_it() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    configure_gate(root.path(), &state);
    let app = router(state.clone());

    // first → second → third, each turn completed before the next is admitted.
    let mut previous: Option<String> = None;
    let mut runs = Vec::new();
    for serial in 20..23 {
        let body = request(serial, "alice", previous.as_deref());
        let (status, accepted) = call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
        let run = body["run"].as_str().unwrap().to_owned();
        let workspace = wait_gate(&state, &run).await;
        // The Run this turn is still reading must stay, and so must the one that
        // is currently executing.
        if let Some(previous) = previous.as_deref() {
            let (status, rejected) =
                call(app.clone(), "DELETE", &format!("/runs/{previous}"), None).await;
            assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
            assert!(
                rejected["error"]
                    .as_str()
                    .unwrap()
                    .contains("previous turn")
            );
        }
        let (status, rejected) = call(app.clone(), "DELETE", &format!("/runs/{run}"), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert!(
            rejected["error"]
                .as_str()
                .unwrap()
                .contains("still running")
        );
        std::fs::write(workspace.join("gate.release"), "release").unwrap();
        wait_run_status(&state, &run, RunStatus::Completed).await;
        previous = Some(run.clone());
        runs.push(run);
    }
    let (first_run, second_run, third_run) = (&runs[0], &runs[1], &runs[2]);

    // A middle link goes once its successor is settled, and the surviving chain
    // still resolves past the tombstone onto the older link that survives.
    let (status, deleted) = call(app.clone(), "DELETE", &format!("/runs/{second_run}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    assert!(
        crate::run_deletions::load(&state.data_root, second_run)
            .unwrap()
            .unwrap()
            .previous_run()
            .is_some()
    );
    let fourth = request(23, "alice", Some(third_run));
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&fourth.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let fourth_run = fourth["run"].as_str().unwrap();
    let workspace = wait_gate(&state, fourth_run).await;
    std::fs::write(workspace.join("gate.release"), "release").unwrap();
    wait_run_status(&state, fourth_run, RunStatus::Completed).await;

    // The newest link may go while older ones survive: every surviving link is
    // then the predecessor of a deleted one, so the Session has no live chain
    // and the next accepted turn starts a fresh one.
    let (status, deleted) = call(app.clone(), "DELETE", &format!("/runs/{fourth_run}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    let fifth = request(24, "alice", None);
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&fifth.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let fifth_run = fifth["run"].as_str().unwrap();
    let workspace = wait_gate(&state, fifth_run).await;
    std::fs::write(workspace.join("gate.release"), "release").unwrap();
    wait_run_status(&state, fifth_run, RunStatus::Completed).await;

    // The inert older links can still be cleaned up, in any order.
    for run in [third_run, first_run] {
        let (status, deleted) = call(app.clone(), "DELETE", &format!("/runs/{run}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{run}: {deleted}");
    }
}

#[tokio::test]
async fn conversation_identity_and_lineage_reject_cross_session_graph_or_reply_changes() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let mut graph = gate_definition();
    graph["ops"]["work"]["run"] = json!("true");
    write_graph_bundle(&state.bundle_root, &graph).unwrap();
    let other = state.catalog_root.join("other");
    write_graph_bundle(&other, &graph).unwrap();
    let app = router(state.clone());
    let first = request(10, "alice", None);
    let foreign = request(11, "bob", None);
    for body in [&first, &foreign] {
        let (status, accepted) = call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
        wait_run_status(&state, body["run"].as_str().unwrap(), RunStatus::Completed).await;
    }
    let first_run = first["run"].as_str().unwrap();
    let valid_next = request(12, "alice", Some(first_run));
    let mut wrong_graph = valid_next.clone();
    wrong_graph["graph"] = json!("other");
    let mut wrong_reply = valid_next.clone();
    wrong_reply["reply_node"] = json!("finish");
    let invalid = [
        request(12, "alice", None),
        request(12, "alice", Some(foreign["run"].as_str().unwrap())),
        request(12, "alice", Some("missing-run")),
        request(12, "new-session", Some(first_run)),
        wrong_graph,
        wrong_reply,
    ];
    for body in invalid {
        let (status, rejected) = call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}: {rejected}");
        assert_eq!(state.application.records().unwrap().len(), 2);
    }
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&valid_next.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    wait_run_status(
        &state,
        valid_next["run"].as_str().unwrap(),
        RunStatus::Completed,
    )
    .await;
    let stale = request(13, "alice", Some(first_run));
    assert_eq!(
        call(app, "POST", "/conversation-runs", Some(&stale.to_string()))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn conversation_resume_is_not_blocked_by_another_session_on_the_same_graph() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    configure_gate(root.path(), &state);
    let app = router(state.clone());
    let alice = request(6, "alice", None);
    let bob = request(7, "bob", None);
    for body in [&alice, &bob] {
        assert_eq!(
            call(
                app.clone(),
                "POST",
                "/conversation-runs",
                Some(&body.to_string())
            )
            .await
            .0,
            StatusCode::ACCEPTED
        );
    }
    let alice_run = alice["run"].as_str().unwrap();
    let bob_run = bob["run"].as_str().unwrap();
    wait_gate(&state, alice_run).await;
    let workspace = wait_gate(&state, bob_run).await;
    assert_eq!(
        call(app.clone(), "POST", &format!("/runs/{bob_run}/pause"), None)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    std::fs::write(
        workspace.join("gate.release"),
        "release after pause request",
    )
    .unwrap();
    wait_run_status(&state, bob_run, RunStatus::Paused).await;
    let bob_next = request(8, "bob", Some(bob_run));
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&bob_next.to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, resumed) = call(
        app.clone(),
        "POST",
        &format!("/runs/{bob_run}/resume"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
    wait_run_status(&state, bob_run, RunStatus::Completed).await;
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(bob_run)
        .unwrap()
        .unwrap();
    assert_eq!(record.results["work"].len(), 1);
    assert_eq!(record.invocations["work"], 1);
    assert_eq!(state.application.active_runs(None).await, vec![alice_run]);
    stop_run(&app, &state, alice_run).await;
}

#[tokio::test]
async fn superseded_conversation_cannot_resume_or_recover_its_wait_child() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    configure_gate(root.path(), &state);
    let child_path = state.catalog_root.join("child");
    write_graph_bundle(&child_path, &gate_definition()).unwrap();
    let parent = json!({
        "objective":"conversation with wait child","entry":"work","agents":{},
        "ops":{"work":{"call":{"graph":"child","mode":"wait"}}},
        "nodes":[{"id":"work","op":"work"}],"edges":[],
    });
    write_graph_bundle(&state.bundle_root, &parent).unwrap();
    let app = router(state.clone());
    let first = request(60, "alice", None);
    let first_run = first["run"].as_str().unwrap();
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&first.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let store = FileRunStore::new(state.data_root.join("runs"));
    let child_run = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = store.load(first_run).unwrap().unwrap();
            if let Some(child) = state
                .application
                .child_metadata(first_run)
                .unwrap()
                .first()
                .map(|metadata| metadata.run_id.clone())
            {
                break child;
            }
            assert_ne!(record.status, RunStatus::Failed, "{record:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wait child was not admitted");
    let workspace = wait_gate(&state, &child_run).await;
    assert_eq!(
        call(
            app.clone(),
            "POST",
            &format!("/runs/{child_run}/pause"),
            None
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    std::fs::write(
        workspace.join("gate.release"),
        "release after child pause request",
    )
    .unwrap();
    wait_run_status(&state, &child_run, RunStatus::Paused).await;
    wait_run_status(&state, first_run, RunStatus::WaitingCall).await;
    stop_run(&app, &state, first_run).await;
    wait_run_status(&state, &child_run, RunStatus::Stopped).await;

    // A fresh message uses the current catalog; the original parent and child
    // retain their frozen Graphs and must become read-only once it is accepted.
    write_graph_bundle(
        &state.bundle_root,
        &json!({
            "objective":"next message","entry":"work","agents":{},
            "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[],
        }),
    )
    .unwrap();
    let next = request(61, "alice", Some(first_run));
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&next.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_run_status(&state, next["run"].as_str().unwrap(), RunStatus::Completed).await;
    let parent_before = std::fs::read(
        state
            .data_root
            .join("runs")
            .join(format!("{first_run}.json")),
    )
    .unwrap();
    let child_before = std::fs::read(
        state
            .data_root
            .join("runs")
            .join(format!("{child_run}.json")),
    )
    .unwrap();
    for (path, body) in [
        (format!("/runs/{child_run}/resume"), None),
        (
            format!("/runs/{child_run}/recovery"),
            Some(r#"{"node_id":"work","invocation":1,"attempt_id":1,"decision":"retry"}"#),
        ),
    ] {
        let (status, rejected) = call(app.clone(), "POST", &path, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert!(
            rejected["error"]
                .as_str()
                .unwrap()
                .contains("previous turn")
        );
    }
    assert!(state.application.active_runs(None).await.is_empty());
    assert_eq!(
        std::fs::read(
            state
                .data_root
                .join("runs")
                .join(format!("{first_run}.json"))
        )
        .unwrap(),
        parent_before
    );
    assert_eq!(
        std::fs::read(
            state
                .data_root
                .join("runs")
                .join(format!("{child_run}.json"))
        )
        .unwrap(),
        child_before
    );
    assert_eq!(
        store.load(&child_run).unwrap().unwrap().results["work"].len(),
        1
    );
    assert!(
        !store
            .load(&child_run)
            .unwrap()
            .unwrap()
            .results
            .contains_key("finish")
    );
    let (status, detail) = call(app, "GET", &format!("/runs/{child_run}"), None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["state"]["status"], "stopped");
}

#[tokio::test]
async fn superseded_conversation_keeps_detached_child_independent() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    configure_gate(root.path(), &state);
    write_graph_bundle(&state.catalog_root.join("child"), &gate_definition()).unwrap();
    write_graph_bundle(
        &state.bundle_root,
        &json!({
            "objective":"conversation with detached child","entry":"work","agents":{},
            "ops":{"work":{"call":{"graph":"child","mode":"detach"}}},
            "nodes":[{"id":"work","op":"work"}],"edges":[],
        }),
    )
    .unwrap();
    let app = router(state.clone());
    let first = request(62, "alice", None);
    let first_run = first["run"].as_str().unwrap();
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&first.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_run_status(&state, first_run, RunStatus::Completed).await;
    let child_run = state.application.child_metadata(first_run).unwrap()[0]
        .run_id
        .clone();
    let workspace = wait_gate(&state, &child_run).await;
    assert_eq!(
        call(
            app.clone(),
            "POST",
            &format!("/runs/{child_run}/pause"),
            None
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    std::fs::write(
        workspace.join("gate.release"),
        "release after detached child pause request",
    )
    .unwrap();
    wait_run_status(&state, &child_run, RunStatus::Paused).await;
    stop_run(&app, &state, &child_run).await;
    write_graph_bundle(
        &state.bundle_root,
        &json!({
            "objective":"next message","entry":"work","agents":{},
            "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[],
        }),
    )
    .unwrap();
    let next = request(63, "alice", Some(first_run));
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&next.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_run_status(&state, next["run"].as_str().unwrap(), RunStatus::Completed).await;
    let (status, resumed) = call(app, "POST", &format!("/runs/{child_run}/resume"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
    wait_run_status(&state, &child_run, RunStatus::Completed).await;
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(&child_run)
        .unwrap()
        .unwrap();
    assert_eq!(record.results["work"].len(), 1);
    assert_eq!(record.results["finish"].len(), 1);
}

fn seed_call_fixture(
    state: &ApiState,
    graph: &str,
    run: &str,
    parent: &str,
    root: &str,
    mode: &str,
) {
    let store = FileRunStore::new(state.data_root.join("runs"));
    let parent_record = store.load(parent).unwrap().unwrap();
    let parent_metadata = state.application.metadata(parent).unwrap().unwrap();
    let path = state.application.graph_bundle_path(graph);
    let bundle = FileGraphBundleLoader::new(&path).load().unwrap();
    let mut record =
        anchor_runtime::graph::GraphRunRecord::create_with_id(bundle.snapshot, Value::Null, run)
            .unwrap();
    record.status = RunStatus::Stopped;
    record.plugin_bindings = bundle
        .plugins
        .into_iter()
        .map(|plugin| (plugin.id.clone(), plugin))
        .collect();
    record.plugin_bindings_initialized = true;
    let metadata = RunMetadata::graph_call_child(
        run.into(),
        graph.into(),
        record.graph_digest.clone(),
        &path,
        crate::application::metadata::GraphCallSource {
            parent_run: parent.into(),
            parent_graph: parent_metadata.graph,
            parent_graph_digest: parent_record.graph_digest,
            node: "work".into(),
            invocation: 1,
            mode: mode.into(),
            root_run: root.into(),
        },
    )
    .unwrap();
    metadata::save(&state.data_root, &metadata).unwrap();
    store.save(&record).unwrap();
}

#[tokio::test]
async fn superseded_wait_ancestry_stops_at_a_detach_boundary() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let first = request(64, "alice", None);
    let first_run = first["run"].as_str().unwrap();
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&first.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_run_status(&state, first_run, RunStatus::Completed).await;
    write_graph_bundle(
        &state.catalog_root.join("child"),
        &json!({"entry":"work","objective":"child","agents":{},
        "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]}),
    )
    .unwrap();
    seed_call_fixture(&state, "child", "wait-child", first_run, first_run, "wait");
    seed_call_fixture(
        &state,
        "child",
        "wait-grandchild",
        "wait-child",
        first_run,
        "wait",
    );
    seed_call_fixture(
        &state,
        "child",
        "detached-child",
        first_run,
        first_run,
        "detach",
    );
    seed_call_fixture(
        &state,
        "child",
        "detached-wait-grandchild",
        "detached-child",
        first_run,
        "wait",
    );
    let next = request(65, "alice", Some(first_run));
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&next.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_run_status(&state, next["run"].as_str().unwrap(), RunStatus::Completed).await;
    for run in ["wait-child", "wait-grandchild"] {
        let before =
            std::fs::read(state.data_root.join("runs").join(format!("{run}.json"))).unwrap();
        for (path, body) in [
            (format!("/runs/{run}/resume"), None),
            (
                format!("/runs/{run}/recovery"),
                Some(r#"{"node_id":"work","invocation":1,"attempt_id":1,"decision":"retry"}"#),
            ),
        ] {
            let (status, value) = call(app.clone(), "POST", &path, body).await;
            assert_eq!(status, StatusCode::CONFLICT, "{value}");
            assert!(value["error"].as_str().unwrap().contains("previous turn"));
        }
        assert_eq!(
            std::fs::read(state.data_root.join("runs").join(format!("{run}.json"))).unwrap(),
            before
        );
    }
    for run in ["detached-child", "detached-wait-grandchild"] {
        let (status, value) = call(app.clone(), "POST", &format!("/runs/{run}/resume"), None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{value}");
        wait_run_status(&state, run, RunStatus::Completed).await;
    }
}

#[tokio::test]
async fn sealed_plugin_conversation_history_does_not_permanently_block_graph_updates() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let plugin = state.catalog_root.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills/example")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Demo","skills":"skills/"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("skills/example/SKILL.md"), "demo skill").unwrap();
    let definition = json!({"objective":"plugin history","entry":"work",
        "agents":{"worker":{"model":"fixture","instructions":"fixture"}},"ops":{},
        "nodes":[{"id":"work","agent":"worker","plugins":["demo"]}],"edges":[]});
    let app = router(state.clone());
    assert_eq!(
        call(
            app.clone(),
            "PUT",
            "/graphs/fixture",
            Some(&json!({"definition":definition}).to_string())
        )
        .await
        .0,
        StatusCode::OK
    );
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let store = FileRunStore::new(state.data_root.join("runs"));
    let mut first = anchor_runtime::graph::GraphRunRecord::create_with_id(
        bundle.snapshot.clone(),
        Value::Null,
        "plugin-first",
    )
    .unwrap();
    first.status = RunStatus::Stopped;
    first.plugin_bindings = bundle
        .plugins
        .into_iter()
        .map(|plugin| (plugin.id.clone(), plugin))
        .collect();
    first.plugin_bindings_initialized = true;
    let mut first_metadata = RunMetadata::new(
        first.run_id.clone(),
        "fixture".into(),
        first.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    first_metadata.conversation = Some(crate::application::ConversationSource {
        session: "alice".into(),
        reply_node: "work".into(),
        previous_run: None,
    });
    metadata::save(&state.data_root, &first_metadata).unwrap();
    store.save(&first).unwrap();
    let update = json!({"definition":definition}).to_string();
    assert_eq!(
        call(app.clone(), "PUT", "/graphs/fixture", Some(&update))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut next = anchor_runtime::graph::GraphRunRecord::create_with_id(
        first.snapshot.clone(),
        Value::Null,
        "plugin-next",
    )
    .unwrap();
    next.status = RunStatus::Completed;
    next.plugin_bindings = first.plugin_bindings.clone();
    next.plugin_bindings_initialized = true;
    let mut next_metadata = RunMetadata::new(
        next.run_id.clone(),
        "fixture".into(),
        next.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    next_metadata.conversation = Some(crate::application::ConversationSource {
        session: "alice".into(),
        reply_node: "work".into(),
        previous_run: Some(first.run_id.clone()),
    });
    metadata::save(&state.data_root, &next_metadata).unwrap();
    store.save(&next).unwrap();
    let (status, value) = call(app.clone(), "PUT", "/graphs/fixture", Some(&update)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    seed_call_fixture(
        &state,
        "fixture",
        "plugin-wait-child",
        &first.run_id,
        &first.run_id,
        "wait",
    );
    assert_eq!(
        call(app.clone(), "PUT", "/graphs/fixture", Some(&update))
            .await
            .0,
        StatusCode::OK
    );
    let mut pending = store.load("plugin-wait-child").unwrap().unwrap();
    pending.status = RunStatus::Running;
    store.save(&pending).unwrap();
    assert_eq!(
        call(app.clone(), "PUT", "/graphs/fixture", Some(&update))
            .await
            .0,
        StatusCode::CONFLICT
    );
    pending.status = RunStatus::Stopped;
    store.save(&pending).unwrap();
    seed_call_fixture(
        &state,
        "fixture",
        "plugin-detached-child",
        &first.run_id,
        &first.run_id,
        "detach",
    );
    assert_eq!(
        call(app.clone(), "PUT", "/graphs/fixture", Some(&update))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut detached = store.load("plugin-detached-child").unwrap().unwrap();
    detached.status = RunStatus::Completed;
    store.save(&detached).unwrap();
    next.status = RunStatus::Stopped;
    store.save(&next).unwrap();
    assert_eq!(
        call(app, "PUT", "/graphs/fixture", Some(&update)).await.0,
        StatusCode::CONFLICT
    );
}

fn seed_legacy_scope(state: &ApiState, run: &str, node: &str) -> (PathBuf, PathBuf, PathBuf) {
    let metadata = state.application.metadata(run).unwrap().unwrap();
    let hint = crate::node_host::conversation_hint_for(&metadata, node).unwrap();
    let scope = format!("nc1-{:x}", Sha256::digest(hint.key.as_bytes()));
    let io_root = state.data_root.join("io-harness/store");
    let directory = io_root.join("conversations").join(&scope);
    let framework_root = io_root.join("conversation-roots").join(&scope);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::create_dir_all(&framework_root).unwrap();
    let (session_id, native_run, turn) = {
        std::fs::write(
            directory.join("framework.sqlite3"),
            b"opaque legacy cleanup fixture",
        )
        .unwrap();
        (1, 1, 1)
    };
    std::fs::write(
        directory.join("session.json"),
        json!({"version":1,"session_id":session_id}).to_string(),
    )
    .unwrap();
    let key = anchor_runtime::graph::InvocationKey {
        run_id: run.into(),
        graph_digest: metadata.graph_digest,
        node_id: node.into(),
        invocation: 1,
    };
    let stem = format!("np1-{:x}", Sha256::digest(key.durable_key().as_bytes()));
    let pointer = io_root.join(format!("{stem}.conversation.json"));
    std::fs::write(
        &pointer,
        json!({"version":1,"scope":scope,"invocation":key.durable_key(),
        "session_id":session_id,"run_id":native_run,"turn_id":turn})
        .to_string(),
    )
    .unwrap();
    (directory, framework_root, pointer)
}

#[tokio::test]
async fn graph_delete_cleans_legacy_scopes_and_preserves_other_graph_history() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    write_graph_bundle(&state.bundle_root, &two_node_definition("true")).unwrap();
    let app = router(state.clone());
    let mut first = request(66, "alice", None);
    first["reply_node"] = json!("first");
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&first.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_run_status(&state, first["run"].as_str().unwrap(), RunStatus::Completed).await;
    let new_definition = json!({"objective":"new graph","entry":"first","agents":{},
        "ops":{"first":{"run":"true"}},"nodes":[{"id":"first","op":"first"}],"edges":[]});
    assert_eq!(
        call(
            app.clone(),
            "PUT",
            "/graphs/fixture",
            Some(&json!({"definition":new_definition}).to_string())
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut next = request(67, "alice", first["run"].as_str());
    next["reply_node"] = json!("first");
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&next.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_run_status(&state, next["run"].as_str().unwrap(), RunStatus::Completed).await;
    write_graph_bundle(&state.catalog_root.join("other"), &new_definition).unwrap();
    let mut other = request(68, "bob", None);
    other["graph"] = json!("other");
    other["reply_node"] = json!("first");
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&other.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_run_status(&state, other["run"].as_str().unwrap(), RunStatus::Completed).await;
    let first_scope = seed_legacy_scope(&state, first["run"].as_str().unwrap(), "first");
    let same_scope = seed_legacy_scope(&state, next["run"].as_str().unwrap(), "first");
    let old_node_scope = seed_legacy_scope(&state, first["run"].as_str().unwrap(), "second");
    let other_scope = seed_legacy_scope(&state, other["run"].as_str().unwrap(), "first");
    assert_eq!(first_scope.0, same_scope.0);
    seed_call_fixture(
        &state,
        "other",
        "orphan-wait-child",
        first["run"].as_str().unwrap(),
        first["run"].as_str().unwrap(),
        "wait",
    );
    let run_store = FileRunStore::new(state.data_root.join("runs"));
    let mut orphan = run_store.load("orphan-wait-child").unwrap().unwrap();
    orphan.status = RunStatus::Running;
    run_store.save(&orphan).unwrap();
    let (status, value) = call(app.clone(), "DELETE", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert!(first_scope.0.join("framework.sqlite3").is_file());
    stop_run(&app, &state, "orphan-wait-child").await;
    // A single conversation Run may now be deleted, and that cleanup must still
    // leave the Session-owned legacy scopes to the Graph cascade.
    let (status, value) = call(
        app.clone(),
        "DELETE",
        &format!("/runs/{}", next["run"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{value}");
    assert!(first_scope.0.exists());
    let (status, value) = call(app.clone(), "DELETE", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{value}");
    for (directory, framework_root, pointer) in [first_scope, same_scope, old_node_scope] {
        assert!(!directory.exists());
        assert!(!framework_root.exists());
        assert!(!pointer.exists());
    }
    assert!(other_scope.0.join("framework.sqlite3").is_file());
    assert!(other_scope.1.is_dir());
    assert!(other_scope.2.is_file());
    assert_eq!(state.application.records().unwrap().len(), 2);
    assert_eq!(
        state.application.records().unwrap()[0].0,
        other["run"].as_str().unwrap()
    );
    let (status, value) = call(
        app.clone(),
        "POST",
        "/graphs",
        Some(&json!({"name":"fixture","definition":new_definition}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    let mut fresh = request(69, "alice", None);
    fresh["reply_node"] = json!("first");
    let (status, value) = call(app, "POST", "/conversation-runs", Some(&fresh.to_string())).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    wait_run_status(&state, fresh["run"].as_str().unwrap(), RunStatus::Completed).await;
    let native = seed_legacy_scope(&state, fresh["run"].as_str().unwrap(), "first");
    assert_eq!(
        std::fs::read(native.0.join("framework.sqlite3")).unwrap(),
        b"opaque legacy cleanup fixture"
    );
}

#[tokio::test]
async fn graph_delete_allows_a_completed_caller_after_its_callee_was_deleted() {
    let _env = PROCESS_ENV.lock().await;
    for conversation in [false, true] {
        let (root, state) = fixture();
        set_host_env(root.path(), &state.data_root);
        let completed = json!({"objective":"completed node","entry":"work","agents":{},
            "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]});
        write_graph_bundle(&state.catalog_root.join("child"), &completed).unwrap();
        write_graph_bundle(
            &state.bundle_root,
            &json!({"objective":"completed caller","entry":"work","agents":{},
            "ops":{"work":{"call":{"graph":"child","mode":"wait"}}},
            "nodes":[{"id":"work","op":"work"}],"edges":[]}),
        )
        .unwrap();
        let app = router(state.clone());
        let (endpoint, body) = if conversation {
            ("/conversation-runs", request(70, "alice", None))
        } else {
            ("/trigger", json!({"graph":"fixture"}))
        };
        let (status, value) = call(app.clone(), "POST", endpoint, Some(&body.to_string())).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{value}");
        let run = value["run"].as_str().unwrap();
        wait_run_status(&state, run, RunStatus::Completed).await;
        let scope = conversation.then(|| seed_legacy_scope(&state, run, "work"));
        assert_eq!(
            call(
                app.clone(),
                "PUT",
                "/graphs/fixture",
                Some(&json!({"definition":completed}).to_string())
            )
            .await
            .0,
            StatusCode::OK
        );
        let (status, value) = call(app.clone(), "DELETE", "/graphs/child", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{value}");
        let (status, value) = call(app, "DELETE", "/graphs/fixture", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{value}");
        assert!(state.application.records().unwrap().is_empty());
        if let Some((directory, framework_root, pointer)) = scope {
            assert!(!directory.exists());
            assert!(!framework_root.exists());
            assert!(!pointer.exists());
        }
    }
}

#[tokio::test]
async fn conversation_retry_uses_frozen_input_after_catalog_change_and_never_dispatches() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let mut original = json!({"entry":"work","objective":"original","input":{"default":7},"agents":{},
        "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]});
    write_graph_bundle(&state.bundle_root, &original).unwrap();
    let app = router(state.clone());
    let body = request(20, "alice", None);
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&body.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let run = body["run"].as_str().unwrap();
    wait_run_status(&state, run, RunStatus::Completed).await;
    let before = std::fs::read(state.data_root.join("runs").join(format!("{run}.json"))).unwrap();
    original["input"] = json!({"default":99});
    original["ops"]["work"]["run"] = json!("not-an-authorized-command");
    write_graph_bundle(&state.bundle_root, &original).unwrap();
    let mut restarted = state.clone();
    restarted.application =
        RunApplication::new(state.data_root.clone(), state.catalog_root.clone())
            .with_configured_graph("fixture".into(), state.bundle_root.clone());
    let app = router(restarted.clone());
    let mut equivalent = body.clone();
    equivalent["input"]["default"] = json!(7);
    for body in [&body, &equivalent] {
        let (status, repeated) = call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{repeated}");
        assert_eq!(repeated["run"], run);
    }
    let mut changed = equivalent;
    changed["input"]["default"] = json!(99);
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&changed.to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(restarted.application.active_runs(None).await.is_empty());
    assert_eq!(state.application.records().unwrap().len(), 1);
    assert_eq!(
        std::fs::read(state.data_root.join("runs").join(format!("{run}.json"))).unwrap(),
        before
    );
    std::fs::remove_file(state.bundle_root.join("graph.json")).unwrap();
    assert_eq!(
        call(app, "POST", "/conversation-runs", Some(&body.to_string()))
            .await
            .0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn conversation_orphan_running_requires_explicit_stop_before_a_new_turn() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let first = request(30, "alice", None);
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&first.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let run = first["run"].as_str().unwrap();
    wait_run_status(&state, run, RunStatus::Completed).await;
    let store = FileRunStore::new(state.data_root.join("runs"));
    let mut orphan = store.load(run).unwrap().unwrap();
    orphan.status = RunStatus::Running;
    store.save(&orphan).unwrap();
    let mut restarted = state.clone();
    restarted.application =
        RunApplication::new(state.data_root.clone(), state.catalog_root.clone())
            .with_configured_graph("fixture".into(), state.bundle_root.clone());
    let app = router(restarted.clone());
    let next = request(31, "alice", Some(run));
    let (_, detail) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(detail["state"]["status"], "running");
    assert_eq!(detail["active"], false);
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&next.to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&first.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert!(restarted.application.active_runs(None).await.is_empty());
    // A durable Running orphan is never silently replayed; the trusted host must
    // settle it explicitly before admitting the next conversation turn.
    stop_run(&app, &restarted, run).await;
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&next.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_run_status(
        &restarted,
        next["run"].as_str().unwrap(),
        RunStatus::Completed,
    )
    .await;
}

#[tokio::test]
async fn conversation_manual_run_conflict_and_request_validation_leave_no_facts() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    configure_gate(root.path(), &state);
    let app = router(state.clone());
    let (status, manual) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{manual}");
    let run = manual["run"].as_str().unwrap();
    wait_gate(&state, run).await;
    let conversation = request(40, "alice", None);
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&conversation.to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut invalid = conversation.clone();
    invalid["run"] = json!("../escape");
    let mut unknown = conversation.clone();
    unknown["permissions"] = json!({"all":true});
    let mut invalid_input = conversation.clone();
    invalid_input["input"] = json!([]);
    for body in [invalid, unknown, invalid_input] {
        assert_eq!(
            call(
                app.clone(),
                "POST",
                "/conversation-runs",
                Some(&body.to_string())
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(state.application.records().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_dir(state.data_root.join("run-metadata"))
            .unwrap()
            .count(),
        1
    );
    stop_run(&app, &state, run).await;
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&conversation.to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(app.clone(), "DELETE", &format!("/runs/{run}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn conversation_waiting_recovery_stop_keeps_unknown_attempts_without_execution() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let snapshot = anchor_runtime::graph::GraphSnapshot::admit(json!({
        "objective":"unknown-effect fixture","entry":"work",
        "agents":{"worker":{"model":"fixture","instructions":"fixture"}},
        "ops":{},"nodes":[{"id":"work","agent":"worker"}],"edges":[],
    }))
    .unwrap();
    let mut record = anchor_runtime::graph::GraphRunRecord::create_with_id(
        snapshot,
        Value::Null,
        "channel-00000000-0000-4000-8000-000000000032",
    )
    .unwrap();
    let key = anchor_runtime::graph::InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    };
    record.invocations.insert("work".into(), 1);
    record.passes.insert("work".into(), 1);
    record.cursor = Some(RunCursor {
        node_id: "work".into(),
        key: key.clone(),
        input_commits: vec![],
        prepared_input: json!({"input":null,"committed_inputs":[]}),
    });
    record.status = RunStatus::WaitingRecovery;
    record.recovery = vec![PendingRecovery {
        key,
        attempt: RecoveryAttempt {
            attempt_id: 17,
            step: 1,
            tool: "unknown-effect-fixture".into(),
            started_at: "fixture".into(),
        },
    }];
    let store = FileRunStore::new(state.data_root.join("runs"));
    store.save(&record).unwrap();
    let mut metadata = RunMetadata::new(
        record.run_id.clone(),
        "fixture".into(),
        record.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    metadata.trigger_source = "channel".into();
    metadata.conversation = Some(crate::application::ConversationSource {
        session: "alice".into(),
        reply_node: "work".into(),
        previous_run: None,
    });
    metadata::save(&state.data_root, &metadata).unwrap();
    let app = router(state.clone());
    let body = request(51, "alice", Some(&record.run_id));
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&body.to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    stop_run(&app, &state, &record.run_id).await;
    let stopped = store.load(&record.run_id).unwrap().unwrap();
    assert_eq!(stopped.recovery, record.recovery);
    assert_eq!(stopped.cursor, record.cursor);
    assert!(stopped.results.is_empty());
    assert!(!state.data_root.join("io-harness").exists());
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&body.to_string())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_run_status(&state, body["run"].as_str().unwrap(), RunStatus::Completed).await;
    assert_eq!(store.load(&record.run_id).unwrap().unwrap(), stopped);
}

#[tokio::test]
async fn graph_node_plugins_project_expanded_nodes_and_legacy_metadata_remains_readable() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let plugin = state.catalog_root.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills/example")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Demo","skills":"skills/"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("skills/example/SKILL.md"), "demo skill").unwrap();
    let authoring = json!({"entry":"stage","objective":"projection","agents":{"worker":{"model":"fixture","instructions":"fixture"}},"ops":{"work":{"run":"true"}},
        "nodes":[{"id":"stage","graph":"inner"}],"edges":[],
        "graphs":{"inner":{"entry":"inside","exit":"inside","nodes":[{"id":"inside","agent":"worker","plugins":["demo"]}],"edges":[]}}});
    let app = router(state.clone());
    let (status, changed) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":authoring}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    let (status, graph) = call(app, "GET", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(graph["definition"], authoring);
    assert_eq!(graph["node_plugins"], json!({"stage/inside":["demo"]}));
    let metadata = RunMetadata::new(
        "legacy-run".into(),
        "fixture".into(),
        "digest".into(),
        &state.bundle_root,
    )
    .unwrap();
    let mut old = serde_json::to_value(&metadata).unwrap();
    old.as_object_mut().unwrap().remove("conversation");
    for format in [1, 2] {
        old["format"] = json!(format);
        let directory = state.data_root.join("run-metadata");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("legacy-run.json"), old.to_string()).unwrap();
        assert!(
            metadata::load(&state.data_root, "legacy-run")
                .unwrap()
                .unwrap()
                .conversation
                .is_none()
        );
    }
}
