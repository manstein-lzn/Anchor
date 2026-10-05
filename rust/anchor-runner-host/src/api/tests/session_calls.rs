use super::*;
use crate::application::{
    ConversationSource,
    metadata::GraphCallSource,
    session_calls::{SessionCall, SessionContext},
};
use anchor_runtime_rig::graph::GraphCallOutcome;
use std::time::Duration;

fn seed_session_call(state: &ApiState, run: &str, mode: &str, bound: bool) {
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let mut record = GraphRunRecord::create_with_id(
        bundle.snapshot,
        json!({"session":"alice", "channel":{"source":"wecom", "sender_id":"alice-user"}}),
        run,
    )
    .unwrap();
    record.plugin_bindings_initialized = true;
    let mut child = RunMetadata::graph_call_child(
        run.into(),
        "fixture".into(),
        record.graph_digest.clone(),
        &state.bundle_root,
        GraphCallSource {
            parent_run: "absent-parent".into(),
            parent_graph: "source".into(),
            parent_graph_digest: "source-digest".into(),
            node: "invoke".into(),
            invocation: 1,
            mode: mode.into(),
            root_run: "absent-parent".into(),
        },
    )
    .unwrap();
    child.session_call = Some(SessionCall {
        context: SessionContext {
            session: "alice".into(),
            reply_node: "work".into(),
            conversation_id: "alice-conversation".into(),
            channel: record.input["channel"].clone(),
        },
        status: "pending".into(),
        error: String::new(),
    });
    if bound {
        child.conversation = Some(ConversationSource {
            session: "alice".into(),
            reply_node: "work".into(),
            previous_run: None,
        });
    }
    metadata::save(&state.data_root, &child).unwrap();
    FileRunStore::new(state.data_root.join("runs"))
        .save(&record)
        .unwrap();
}

fn foreground(serial: u64, session: &str, previous: Option<&str>) -> Value {
    json!({
        "graph":"fixture", "run":format!("channel-00000000-0000-4000-8000-{serial:012x}"),
        "session":session, "reply_node":"work",
        "input":{"message":format!("message {serial}")}, "previous_run":previous,
    })
}

async fn execute(
    app: &Router,
    run: &str,
    session: &str,
    previous: Option<&str>,
) -> (StatusCode, Value) {
    call(
        app.clone(),
        "POST",
        &format!("/runs/{run}/session-execution"),
        Some(&json!({"session":session, "previous_run":previous}).to_string()),
    )
    .await
}

fn record_bytes(state: &ApiState, run: &str) -> Vec<u8> {
    std::fs::read(state.data_root.join("runs").join(format!("{run}.json"))).unwrap()
}

fn metadata_bytes(state: &ApiState, run: &str) -> Vec<u8> {
    std::fs::read(
        state
            .data_root
            .join("run-metadata")
            .join(format!("{run}.json")),
    )
    .unwrap()
}

async fn completed_session_call(state: &ApiState, app: &Router, run: &str) {
    seed_session_call(state, run, "wait", false);
    let (status, accepted) = execute(app, run, "alice", None).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    wait_idle(state).await;
    assert_eq!(
        FileRunStore::new(state.data_root.join("runs"))
            .load(run)
            .unwrap()
            .unwrap()
            .status,
        RunStatus::Completed,
    );
}

#[tokio::test]
async fn session_execution_rejects_other_session_and_wrong_previous_identity() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let alice = foreground(100, "alice", None);
    let bob = foreground(101, "bob", None);
    for body in [&alice, &bob] {
        let (status, accepted) = call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
        wait_idle(&state).await;
    }
    let run = "session-identity";
    seed_session_call(&state, run, "wait", false);
    let original_record = record_bytes(&state, run);
    let original_metadata = metadata_bytes(&state, run);
    for (session, previous) in [
        ("bob", Some(alice["run"].as_str().unwrap())),
        ("alice", None),
        ("alice", Some(bob["run"].as_str().unwrap())),
        ("alice", Some("missing-run")),
    ] {
        let (status, rejected) = execute(&app, run, session, previous).await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert_eq!(record_bytes(&state, run), original_record);
        assert_eq!(metadata_bytes(&state, run), original_metadata);
        assert!(state.application.active_runs(None).await.is_empty());
    }
    let previous = alice["run"].as_str().unwrap();
    let (status, accepted) = execute(&app, run, "alice", Some(previous)).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    wait_idle(&state).await;
    let original_record = record_bytes(&state, run);
    let original_metadata = metadata_bytes(&state, run);
    let (status, rejected) = execute(&app, run, "alice", Some(bob["run"].as_str().unwrap())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(record_bytes(&state, run), original_record);
    assert_eq!(metadata_bytes(&state, run), original_metadata);
}

#[tokio::test]
async fn queued_session_call_allows_foreground_and_is_not_dispatched_at_startup() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    seed_session_call(&state, "queued-session-call", "detach", false);
    state
        .application
        .recover_detached_at_startup()
        .await
        .unwrap();
    assert!(state.application.active_runs(None).await.is_empty());
    let app = router(state.clone());
    let body = foreground(110, "alice", None);
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    wait_idle(&state).await;
    let store = FileRunStore::new(state.data_root.join("runs"));
    assert_eq!(
        store.load("queued-session-call").unwrap().unwrap().status,
        RunStatus::Ready
    );
    assert_eq!(
        store
            .load(body["run"].as_str().unwrap())
            .unwrap()
            .unwrap()
            .status,
        RunStatus::Completed
    );
    let context = state
        .application
        .metadata("queued-session-call")
        .unwrap()
        .unwrap();
    assert_eq!(context.session_call.unwrap().status, "pending");
    assert!(context.conversation.is_none());
}

#[tokio::test]
async fn bound_session_call_yields_and_continues_same_run_after_foreground_and_restart() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let run = "bound-session-call";
    // This is the durable boundary after binding and before dispatch on a prior host.
    seed_session_call(&state, run, "wait", true);
    let frozen = FileRunStore::new(state.data_root.join("runs"))
        .load(run)
        .unwrap()
        .unwrap();
    let app = router(state.clone());
    let (status, yielded) = call(
        app.clone(),
        "POST",
        &format!("/runs/{run}/session-yield"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{yielded}");
    let stopped = FileRunStore::new(state.data_root.join("runs"))
        .load(run)
        .unwrap()
        .unwrap();
    assert_eq!(stopped.status, RunStatus::Stopped);
    assert_eq!(stopped.input, frozen.input);
    let next = foreground(120, "alice", Some(run));
    let (status, accepted) = call(app, "POST", "/conversation-runs", Some(&next.to_string())).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    wait_idle(&state).await;

    let mut restarted = state.clone();
    restarted.application =
        RunApplication::new(state.data_root.clone(), state.catalog_root.clone())
            .with_configured_graph("fixture".into(), state.bundle_root.clone());
    let app = router(restarted.clone());
    let (status, rejected) = execute(&app, run, "alice", Some(next["run"].as_str().unwrap())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    let (status, accepted) = execute(&app, run, "alice", None).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    wait_idle(&restarted).await;
    let resumed = FileRunStore::new(state.data_root.join("runs"))
        .load(run)
        .unwrap()
        .unwrap();
    assert_eq!(resumed.status, RunStatus::Completed, "{resumed:?}");
    assert_eq!(resumed.run_id, run);
    assert_eq!(resumed.snapshot, frozen.snapshot);
    assert_eq!(resumed.input, frozen.input);
    assert_eq!(resumed.passes["work"], 1);
    assert_eq!(state.application.records().unwrap().len(), 2);
    let binding = state.application.metadata(run).unwrap().unwrap();
    assert_eq!(binding.session_call.unwrap().status, "pending");
    assert_eq!(binding.conversation.unwrap().previous_run, None);
}

#[tokio::test]
async fn active_session_call_yield_releases_execution_without_settling_delivery() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    unsafe {
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh,cat,sleep,printf");
    }
    write_graph_bundle(&state.bundle_root, &json!({
        "objective":"session gate", "entry":"work", "agents":{},
        "ops":{"work":{"run":"printf ready > gate.ready; while [ ! -f gate.release ]; do sleep 0.02; done", "wall_time_limit_seconds":10}},
        "nodes":[{"id":"work","op":"work"}], "edges":[],
    })).unwrap();
    let run = "active-session-call";
    seed_session_call(&state, run, "wait", false);
    let app = router(state.clone());
    let (status, accepted) = execute(&app, run, "alice", None).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
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
            if let Some(cursor) = record.cursor.as_ref() {
                let workspace = HostArtifacts::new(
                    state.data_root.join("artifacts"),
                    state.workspace_root.clone(),
                )
                .workspace_path(&cursor.key)
                .unwrap();
                if workspace.join("gate.ready").is_file() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the Op did not reach its local fixture gate");
    let (status, yielded) = call(
        app.clone(),
        "POST",
        &format!("/runs/{run}/session-yield"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{yielded}");
    wait_idle(&state).await;
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(run)
        .unwrap()
        .unwrap();
    assert_eq!(record.status, RunStatus::Stopped);
    assert!(record.cursor.is_some());
    assert_eq!(
        state
            .application
            .metadata(run)
            .unwrap()
            .unwrap()
            .session_call
            .unwrap()
            .status,
        "pending"
    );
    let next = foreground(130, "alice", Some(run));
    let (status, accepted) = call(app, "POST", "/conversation-runs", Some(&next.to_string())).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    // Release only the new foreground Op; the interrupted child remains its original Run.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let next_run = next["run"].as_str().unwrap();
            let record = FileRunStore::new(state.data_root.join("runs"))
                .load(next_run)
                .unwrap()
                .unwrap();
            if let Some(cursor) = record.cursor.as_ref() {
                let workspace = HostArtifacts::new(
                    state.data_root.join("artifacts"),
                    state.workspace_root.clone(),
                )
                .workspace_path(&cursor.key)
                .unwrap();
                if workspace.join("gate.ready").is_file() {
                    std::fs::write(workspace.join("gate.release"), "release").unwrap();
                    break;
                }
            }
            assert!(
                !matches!(record.status, RunStatus::Failed | RunStatus::Completed),
                "{record:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the foreground Op did not reach its local fixture gate");
    wait_idle(&state).await;
    assert_eq!(
        FileRunStore::new(state.data_root.join("runs"))
            .load(run)
            .unwrap()
            .unwrap(),
        record
    );
}

#[tokio::test]
async fn completed_session_execution_stays_waiting_until_delivery_settlement() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let run = "session-delivery";
    completed_session_call(&state, &app, run).await;
    let original_record = record_bytes(&state, run);
    assert_eq!(
        state.application.session_call_outcome(run, "wait").unwrap(),
        Some(GraphCallOutcome::Waiting {
            child_run_id: run.into()
        }),
    );
    let (_, detail) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(detail["state"]["status"], "completed");
    assert_eq!(detail["session_call"]["status"], "pending");
    for _ in 0..2 {
        let (status, settled) = call(
            app.clone(),
            "POST",
            &format!("/runs/{run}/session-settlement"),
            Some(r#"{"status":"delivered"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{settled}");
    }
    assert_eq!(
        state.application.session_call_outcome(run, "wait").unwrap(),
        None
    );
    assert_eq!(record_bytes(&state, run), original_record);
    let (status, rejected) = call(
        app,
        "POST",
        &format!("/runs/{run}/session-settlement"),
        Some(r#"{"status":"failed","error":"late failure"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(
        state
            .application
            .metadata(run)
            .unwrap()
            .unwrap()
            .session_call
            .unwrap()
            .status,
        "delivered"
    );
}

#[tokio::test]
async fn stopping_completed_pending_session_call_preserves_completed_run() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let run = "session-stop-delivery";
    completed_session_call(&state, &app, run).await;
    let original_record = record_bytes(&state, run);
    let (status, stopped) = call(app.clone(), "POST", &format!("/runs/{run}/stop"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{stopped}");
    assert_eq!(record_bytes(&state, run), original_record);
    let call_state = state
        .application
        .metadata(run)
        .unwrap()
        .unwrap()
        .session_call
        .unwrap();
    assert_eq!(call_state.status, "failed");
    assert_eq!(
        state.application.session_call_outcome(run, "wait").unwrap(),
        Some(GraphCallOutcome::Failed {
            child_run_id: Some(run.into()),
            reason: call_state.error
        }),
    );
    let (status, rejected) = execute(&app, run, "alice", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
}

#[tokio::test]
async fn generic_session_resume_pause_and_recovery_cannot_bypass_host_arbitration() {
    let (_root, state) = fixture();
    let run = "session-controls";
    seed_session_call(&state, run, "wait", false);
    let original_record = record_bytes(&state, run);
    let original_metadata = metadata_bytes(&state, run);
    let app = router(state.clone());
    for (operation, body) in [
        ("resume", None),
        ("pause", None),
        (
            "recovery",
            Some(r#"{"node_id":"work","invocation":1,"attempt_id":1,"decision":"retry"}"#),
        ),
    ] {
        let (status, rejected) = call(
            app.clone(),
            "POST",
            &format!("/runs/{run}/{operation}"),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert_eq!(record_bytes(&state, run), original_record);
        assert_eq!(metadata_bytes(&state, run), original_metadata);
        assert!(state.application.active_runs(None).await.is_empty());
    }
}

#[tokio::test]
async fn pending_session_delivery_blocks_graph_mutation_and_deletion() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let run = "session-retained-delivery";
    completed_session_call(&state, &app, run).await;
    let graph = std::fs::read(state.bundle_root.join("graph.json")).unwrap();
    let original_record = record_bytes(&state, run);
    let original_metadata = metadata_bytes(&state, run);
    let update = json!({"definition":json!({"objective":"updated", "entry":"work", "agents":{}, "ops":{"work":{"run":"true"}}, "nodes":[{"id":"work","op":"work"}], "edges":[]})}).to_string();
    for (method, uri, body) in [
        ("PUT", "/graphs/fixture".to_owned(), Some(update.as_str())),
        ("DELETE", "/graphs/fixture".to_owned(), None),
        ("DELETE", format!("/runs/{run}"), None),
    ] {
        let (status, rejected) = call(app.clone(), method, &uri, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert_eq!(
            std::fs::read(state.bundle_root.join("graph.json")).unwrap(),
            graph
        );
        assert_eq!(record_bytes(&state, run), original_record);
        assert_eq!(metadata_bytes(&state, run), original_metadata);
    }
    assert_eq!(
        call(
            app.clone(),
            "POST",
            &format!("/runs/{run}/session-settlement"),
            Some(r#"{"status":"delivered"}"#)
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, updated) = call(app.clone(), "PUT", "/graphs/fixture", Some(&update)).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let (status, deleted) = call(app, "DELETE", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    assert!(state.application.records().unwrap().is_empty());
}
