//! Explicit Run abandonment: `POST /runs/{run}/abandon`.
//!
//! These cases pin the operator-visible contract: the in-flight node is really
//! cancelled, the Run becomes terminal `Aborted` with an audited reason, the
//! request is idempotent, the Graph/Plugin replacement precondition flips from
//! false to true, and no binding, workspace, artifact or history is deleted.

use super::*;
use anchor_platform_session::{ChannelIdentity, ChannelInboundRequest, SessionStore, TurnStatus};
use anchor_runtime::graph::{
    InvocationKey, PendingRecovery, PluginBinding, RecoveryAttempt, RunCursor,
};

fn run_store(state: &ApiState) -> FileRunStore {
    FileRunStore::new(state.data_root.join("runs"))
}

fn record_of(state: &ApiState, run: &str) -> GraphRunRecord {
    run_store(state).load(run).unwrap().unwrap()
}

fn record_bytes(state: &ApiState, run: &str) -> Vec<u8> {
    std::fs::read(state.data_root.join("runs").join(format!("{run}.json"))).unwrap()
}

fn intent_path(state: &ApiState, run: &str) -> PathBuf {
    state
        .data_root
        .join("run-abandons")
        .join(format!("{run}.json"))
}

fn seed_run(state: &ApiState, run: &str, status: RunStatus) -> GraphRunRecord {
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let mut record = GraphRunRecord::create_with_id(bundle.snapshot, json!({}), run).unwrap();
    record.status = status;
    let store = run_store(state);
    store.save(&record).unwrap();
    metadata::save(
        &state.data_root,
        &RunMetadata::new(
            run.into(),
            state.graph_name.clone(),
            record.graph_digest.clone(),
            &state.bundle_root,
        )
        .unwrap(),
    )
    .unwrap();
    record
}

/// A Run parked at an unresolved external effect: the ordinary recovery entry
/// point applies to it until it is abandoned.
fn seed_waiting_recovery(state: &ApiState, run: &str) -> GraphRunRecord {
    let snapshot = GraphSnapshot::admit(json!({
        "objective":"unknown-effect fixture","entry":"work",
        "agents":{"worker":{"model":"fixture","instructions":"fixture"}},
        "ops":{},"nodes":[{"id":"work","agent":"worker"}],"edges":[],
    }))
    .unwrap();
    let mut record = GraphRunRecord::create_with_id(snapshot, Value::Null, run).unwrap();
    let key = InvocationKey {
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
    run_store(state).save(&record).unwrap();
    metadata::save(
        &state.data_root,
        &RunMetadata::new(
            run.into(),
            state.graph_name.clone(),
            record.graph_digest.clone(),
            &state.bundle_root,
        )
        .unwrap(),
    )
    .unwrap();
    record
}

#[tokio::test]
async fn abandon_cancels_the_running_node_and_makes_the_run_terminal_aborted() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    write_graph_bundle(
        &state.bundle_root,
        &json!({
            "objective":"abandon fixture","entry":"work","agents":{},
            "ops":{"work":{"run":"sh -c 'sleep 1; printf late > never.txt'"}},
            "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[],
        }),
    )
    .unwrap();
    let app = router(state.clone());
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let run = accepted["run"].as_str().unwrap().to_owned();
    for _ in 0..400 {
        if record_of(&state, &run).cursor.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        record_of(&state, &run).cursor.is_some(),
        "node never started"
    );
    assert!(state.application.run_is_active(&run).await);

    let (status, body) = call(
        app.clone(),
        "POST",
        &format!("/runs/{run}/abandon"),
        Some(r#"{"reason":"plugin_update"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["action"], "abandoned");
    assert_eq!(body["abandoned"], true);
    assert_eq!(body["status"], "aborted");
    assert_eq!(body["reason"], "plugin_update");
    assert!(body["requested_at"].as_str().is_some());

    let record = record_of(&state, &run);
    assert_eq!(record.status, RunStatus::Aborted);
    let error = record.error.clone().unwrap();
    assert!(error.contains("abandoned"), "{error}");
    assert!(error.contains("plugin_update"), "{error}");
    assert!(!state.application.run_is_active(&run).await);
    assert!(state.application.active_runs(None).await.is_empty());
    assert!(intent_path(&state, &run).is_file());

    // The sandbox process was really cancelled: had it survived, its second
    // shell statement would have written this file by now.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let (status, missing) = call(
        app.clone(),
        "GET",
        &format!("/runs/{run}/files/work/never.txt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    assert_eq!(record_of(&state, &run), record);

    // One-way: a resumed Run would continue the round the operator gave up.
    let (status, rejected) = call(app.clone(), "POST", &format!("/runs/{run}/resume"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert!(
        rejected["error"].as_str().unwrap().contains("abandoned"),
        "{rejected}"
    );

    // Idempotent: the repeat returns the identical body and rewrites nothing.
    let (status, repeated) = call(
        app.clone(),
        "POST",
        &format!("/runs/{run}/abandon"),
        Some(r#"{"reason":"plugin_update"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated, body);
    assert_eq!(record_of(&state, &run), record);

    // A different reason for an already abandoned Run is a conflict.
    let (status, conflicted) = call(
        app.clone(),
        "POST",
        &format!("/runs/{run}/abandon"),
        Some(r#"{"reason":"operator"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflicted}");
    assert!(conflicted["error"].as_str().unwrap().contains("operator"));
}

#[tokio::test]
async fn abandon_is_idempotent_and_rejects_terminal_unknown_or_invalid_requests() {
    let (_root, state) = fixture();
    let app = router(state.clone());

    // A body is optional; the default reason is `operator`.
    let stopped = seed_run(&state, "abandon-stopped", RunStatus::Stopped);
    assert_eq!(stopped.status, RunStatus::Stopped);
    let (status, defaulted) =
        call(app.clone(), "POST", "/runs/abandon-stopped/abandon", None).await;
    assert_eq!(status, StatusCode::OK, "{defaulted}");
    assert_eq!(defaulted["reason"], "operator");
    assert_eq!(defaulted["status"], "aborted");
    assert_eq!(
        record_of(&state, "abandon-stopped").status,
        RunStatus::Aborted
    );
    let (status, repeated) = call(app.clone(), "POST", "/runs/abandon-stopped/abandon", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated, defaulted);

    // An empty reason object behaves like an absent body.
    let stopped = seed_run(&state, "abandon-blank", RunStatus::Stopped);
    assert_eq!(stopped.status, RunStatus::Stopped);
    let (status, blank) = call(
        app.clone(),
        "POST",
        "/runs/abandon-blank/abandon",
        Some(r#"{"reason":"   "}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{blank}");
    assert_eq!(blank["reason"], "operator");

    // Invalid input is rejected before anything durable changes.
    for (body, expected) in [
        (
            r#"{"reason":"not-a-reason"}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (r#"{"reason":123}"#, StatusCode::BAD_REQUEST),
        (r#"{"unexpected":true}"#, StatusCode::BAD_REQUEST),
        ("not json", StatusCode::BAD_REQUEST),
    ] {
        let before = record_bytes(&state, "abandon-stopped");
        let (status, rejected) = call(
            app.clone(),
            "POST",
            "/runs/abandon-stopped/abandon",
            Some(body),
        )
        .await;
        assert_eq!(status, expected, "{body}: {rejected}");
        assert_eq!(record_bytes(&state, "abandon-stopped"), before);
    }

    // Completed and failed Runs cannot be abandoned; missing Runs are 404.
    seed_run(&state, "abandon-completed", RunStatus::Completed);
    seed_run(&state, "abandon-failed", RunStatus::Failed);
    for run in ["abandon-completed", "abandon-failed"] {
        let before = record_bytes(&state, run);
        let (status, rejected) = call(
            app.clone(),
            "POST",
            &format!("/runs/{run}/abandon"),
            Some(r#"{"reason":"plugin_update"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert!(
            rejected["error"]
                .as_str()
                .unwrap()
                .contains("terminal Run cannot be abandoned"),
            "{rejected}"
        );
        assert_eq!(record_bytes(&state, run), before);
        assert!(!intent_path(&state, run).exists());
    }
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/runs/abandon-missing/abandon",
            Some(r#"{"reason":"plugin_update"}"#)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // A Run that was already terminal `Aborted` for another reason reports that
    // existing state and records no new abandon request.
    seed_run(&state, "abandon-already", RunStatus::Aborted);
    let (status, existing) = call(
        app.clone(),
        "POST",
        "/runs/abandon-already/abandon",
        Some(r#"{"reason":"plugin_update"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{existing}");
    assert_eq!(existing["action"], "already_aborted");
    assert_eq!(existing["abandoned"], false);
    assert_eq!(existing["status"], "aborted");
    assert!(existing["reason"].is_null());
    assert!(!intent_path(&state, "abandon-already").exists());

    // A Run id that is not one safe path component is refused outright.
    assert!(matches!(
        state.application.abandon_run("../escape", None).await,
        Err(crate::application::ApplicationError::Invalid(_))
    ));
}

#[tokio::test]
async fn abandon_unblocks_plugin_resource_replacement_and_leaves_stopped_runs_blocking() {
    let (_root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(_root.path(), &state.data_root);
    let plugin = state.catalog_root.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills/example")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Demo","skills":"skills/"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("skills/example/SKILL.md"), "demo skill").unwrap();
    let app = router(state.clone());
    let definition = |objective: &str| {
        json!({
            "objective":objective,
            "agents":{"worker":{"model":"fixture","instructions":"work"}},
            "ops":{},
            "nodes":[{"id":"work","agent":"worker","plugins":["demo"]}],
            "edges":[]
        })
    };
    let (status, created) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition("plugin authoring graph")}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert!(state.bundle_root.join("plugins/demo/plugin.json").is_file());

    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let mut record =
        GraphRunRecord::create_with_id(bundle.snapshot, json!({}), "abandon-plugin").unwrap();
    record.status = RunStatus::Stopped;
    record.plugin_bindings_initialized = true;
    record.plugin_bindings.insert(
        "demo".into(),
        PluginBinding {
            id: "demo".into(),
            digest: "a".repeat(64),
            resources: vec!["skills/example/SKILL.md".into()],
            mcp_servers: Vec::new(),
        },
    );
    run_store(&state).save(&record).unwrap();
    metadata::save(
        &state.data_root,
        &RunMetadata::new(
            record.run_id.clone(),
            state.graph_name.clone(),
            record.graph_digest.clone(),
            &state.bundle_root,
        )
        .unwrap(),
    )
    .unwrap();

    // Stopped is unfinished: it still blocks Plugin replacement.
    assert!(
        state
            .application
            .has_unfinished_plugin_run(&state.bundle_root)
            .await
            .unwrap()
    );
    let (status, blocked) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition("plugin update one")}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
    let graph_before = std::fs::read(state.bundle_root.join("graph.json")).unwrap();

    let (status, abandoned) = call(
        app.clone(),
        "POST",
        "/runs/abandon-plugin/abandon",
        Some(r#"{"reason":"plugin_update"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{abandoned}");
    assert_eq!(abandoned["abandoned"], true);
    assert!(
        !state
            .application
            .has_unfinished_plugin_run(&state.bundle_root)
            .await
            .unwrap()
    );
    // The abandoned Run keeps its frozen Plugin bindings and its history.
    assert_eq!(
        record_of(&state, "abandon-plugin").plugin_bindings,
        record.plugin_bindings
    );
    let (status, replaced) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition("plugin update two")}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    assert_eq!(replaced["definition"]["objective"], "plugin update two");
    assert_ne!(
        std::fs::read(state.bundle_root.join("graph.json")).unwrap(),
        graph_before
    );
    assert_eq!(
        record_of(&state, "abandon-plugin").status,
        RunStatus::Aborted
    );

    // The predicate itself is unchanged: another stopped Run of the same Graph
    // still blocks the next replacement.
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let mut next =
        GraphRunRecord::create_with_id(bundle.snapshot, json!({}), "abandon-plugin-two").unwrap();
    next.status = RunStatus::Stopped;
    next.plugin_bindings = record.plugin_bindings.clone();
    next.plugin_bindings_initialized = true;
    run_store(&state).save(&next).unwrap();
    metadata::save(
        &state.data_root,
        &RunMetadata::new(
            next.run_id.clone(),
            state.graph_name.clone(),
            next.graph_digest.clone(),
            &state.bundle_root,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        state
            .application
            .has_unfinished_plugin_run(&state.bundle_root)
            .await
            .unwrap()
    );
    let (status, blocked) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition("plugin update three")}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
}

/// The persistent assistant fixture: one bound Session instance whose Run is
/// persisted without an execution slot (the state a restart leaves behind).
fn assistant_fixture(state: &ApiState) -> (SessionStore, String, String) {
    let mut definition: Value = serde_json::from_str(include_str!(
        "../../../../../examples/graphs/wecom-persistent-assistant.json"
    ))
    .unwrap();
    definition["nodes"][1]["plugins"] = json!([]);
    write_graph_bundle(&state.bundle_root, &definition).unwrap();
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let admission = sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: "abandon-assistant-input".into(),
                identity: ChannelIdentity {
                    source: "wecom".into(),
                    account: Some("corp-a".into()),
                    conversation_id: "conversation-a".into(),
                    sender_id: "user-a".into(),
                },
                graph: "fixture".into(),
                reply_node: "assistant".into(),
                text: Some("hello".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: true,
            },
        )
        .unwrap();
    let session = admission.session.id;
    let run = "abandon-assistant-run";
    sessions
        .bind_channel_assistant("local", &session, run, "wait_input", "assistant", "reply")
        .unwrap();
    let in_flight = sessions
        .claim_channel_assistant_input("local", &session, run, "abandon-wait-key")
        .unwrap()
        .unwrap();
    assert_eq!(in_flight.relation.turn_id, admission.turn.id);
    let mut record = GraphRunRecord::create_with_id(
        GraphSnapshot::admit(definition).unwrap(),
        json!({"session":session}),
        run,
    )
    .unwrap();
    // A crash can leave the record Running with nothing executing it.
    record.status = RunStatus::Running;
    record.plugin_bindings_initialized = true;
    run_store(state).save(&record).unwrap();
    let mut saved = RunMetadata::new(
        run.into(),
        "fixture".into(),
        record.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    saved.conversation = Some(metadata::ConversationSource {
        session: session.clone(),
        reply_node: "assistant".into(),
        previous_run: None,
    });
    saved.assistant = Some(metadata::AssistantSource {
        owner: "local".into(),
        session: session.clone(),
        wait_node: "wait_input".into(),
        work_node: "assistant".into(),
        reply_node: "reply".into(),
    });
    metadata::save(&state.data_root, &saved).unwrap();
    (sessions, session, admission.turn.id)
}

#[tokio::test]
async fn abandon_keeps_the_assistant_binding_workspace_and_history_for_handover() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let (sessions, session, turn) = assistant_fixture(&state);
    let run = "abandon-assistant-run";
    let before = record_of(&state, run);

    // A real workspace scene the instance owns.
    let artifacts = HostArtifacts::new(
        state.data_root.join("artifacts"),
        state.workspace_root.clone(),
    );
    let key = InvocationKey {
        run_id: run.into(),
        graph_digest: before.graph_digest.clone(),
        node_id: "assistant".into(),
        invocation: 1,
    };
    artifacts
        .bind_node_workspace(run, &before.graph_digest, "assistant")
        .unwrap();
    let workspace = artifacts.workspace_path(&key).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("draft.txt"), "uncommitted work").unwrap();
    // An existing artifact store entry the abandon must not retire or delete.
    let artifacts_root = state.data_root.join("artifacts");
    std::fs::create_dir_all(&artifacts_root).unwrap();
    let existing_artifact = artifacts_root.join("commit-existing.json");
    std::fs::write(&existing_artifact, "{}").unwrap();
    let artifact_entries = std::fs::read_dir(&artifacts_root).unwrap().count();

    let app = router(state.clone());
    assert_eq!(
        call(
            app.clone(),
            "GET",
            &format!("/channel-sessions/{session}/assistant"),
            None
        )
        .await
        .1["needs_recovery"],
        json!(true)
    );
    let (status, abandoned) = call(
        app.clone(),
        "POST",
        &format!("/runs/{run}/abandon"),
        Some(r#"{"reason":"plugin_update"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{abandoned}");
    assert_eq!(abandoned["status"], "aborted");

    let after = record_of(&state, run);
    assert_eq!(after.status, RunStatus::Aborted);
    // Only the status and the audit reason changed.
    assert_eq!(after.snapshot, before.snapshot);
    assert_eq!(after.input, before.input);
    assert_eq!(after.cursor, before.cursor);
    assert_eq!(after.results, before.results);
    assert_eq!(after.recovery, before.recovery);
    assert_eq!(after.plugin_bindings, before.plugin_bindings);

    // Nothing was retired or deleted, and the in-flight round was settled with
    // the Run instead of being handed on as still running.
    let binding = sessions
        .get_channel_assistant("local", &session)
        .unwrap()
        .unwrap();
    assert_eq!(binding.run_id, run);
    assert!(
        !sessions
            .retired_channel_assistant("local", &session, run)
            .unwrap()
    );
    assert!(workspace.join("draft.txt").is_file());
    assert_eq!(
        std::fs::read_dir(&artifacts_root).unwrap().count(),
        artifact_entries
    );
    assert!(existing_artifact.is_file());
    let settled = sessions
        .list_turns("local", &session)
        .unwrap()
        .into_iter()
        .find(|entry| entry.id == turn)
        .unwrap();
    assert_eq!(settled.status, TurnStatus::Failed);
    assert!(
        sessions
            .get_channel_inbound("local", &session, "abandon-assistant-input")
            .is_ok()
    );
    assert!(
        sessions
            .get_channel_assistant_input("local", &session, run, "abandon-wait-key")
            .unwrap()
            .is_some()
    );

    // The existing recovery path still takes the instance over: the old Run
    // stays terminal and a new Run is handed the same Session and workspace.
    crate::api::assistant_recovery::resume_channel_assistants(&state)
        .await
        .unwrap();
    let handed_over = sessions
        .get_channel_assistant("local", &session)
        .unwrap()
        .unwrap();
    assert_ne!(handed_over.run_id, run);
    assert!(handed_over.run_id.starts_with("assistant-resume-"));
    assert_eq!(record_of(&state, run).status, RunStatus::Aborted);
    assert!(workspace.join("draft.txt").is_file());
    assert!(
        sessions
            .get_channel_assistant_input("local", &session, run, "abandon-wait-key")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn abandon_leaves_other_runs_and_their_recovery_entry_points_unchanged() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    // The ordinary paused Run is started first: a *new* Run of the same Graph
    // cannot be admitted while any unfinished Run of that Graph exists, and the
    // seeded `WaitingRecovery` siblings are unfinished by design. Resuming this
    // existing Run afterwards exercises the entry point that the abandon must
    // leave untouched.
    write_graph_bundle(
        &state.bundle_root,
        &two_node_definition("sh -c 'cat /in/first/count.txt > frozen.txt'"),
    )
    .unwrap();
    let app = router(state.clone());
    let paused = start_and_pause(&state, &app).await;
    let abandoned = seed_waiting_recovery(&state, "abandon-recovery-target");
    let sibling = seed_waiting_recovery(&state, "abandon-recovery-sibling");

    // The ordinary recovery entry point answers the sibling identically before
    // and after another Run of the same Host is abandoned.
    let probe = (
        "POST",
        "/runs/abandon-recovery-sibling/recovery",
        r#"{"node_id":"work","invocation":1,"attempt_id":999,"decision":"retry"}"#,
    );
    let (status_before, body_before) = call(app.clone(), probe.0, probe.1, Some(probe.2)).await;
    assert_eq!(status_before, StatusCode::CONFLICT, "{body_before}");
    let sibling_before = record_bytes(&state, "abandon-recovery-sibling");

    let (status, abandoned_body) = call(
        app.clone(),
        "POST",
        "/runs/abandon-recovery-target/abandon",
        Some(r#"{"reason":"operator"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{abandoned_body}");
    let durable = record_of(&state, "abandon-recovery-target");
    assert_eq!(durable.status, RunStatus::Aborted);
    // Unresolved attempts stay as read-only facts, exactly like a stop.
    assert_eq!(durable.recovery, abandoned.recovery);
    assert_eq!(durable.cursor, abandoned.cursor);

    // Its own entry points now refuse, because it is terminal.
    for (path, body) in [
        ("/runs/abandon-recovery-target/resume".to_owned(), None),
        (
            "/runs/abandon-recovery-target/recovery".to_owned(),
            Some(r#"{"node_id":"work","invocation":1,"attempt_id":17,"decision":"retry"}"#),
        ),
    ] {
        let (status, rejected) = call(app.clone(), "POST", &path, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert!(
            rejected["error"].as_str().unwrap().contains("abandoned"),
            "{rejected}"
        );
    }

    // The sibling's entry point still answers exactly as it did before.
    let (status_after, body_after) = call(app.clone(), probe.0, probe.1, Some(probe.2)).await;
    assert_eq!(status_after, status_before);
    assert_eq!(body_after, body_before);
    assert_eq!(
        record_bytes(&state, "abandon-recovery-sibling"),
        sibling_before
    );
    assert_eq!(
        record_of(&state, "abandon-recovery-sibling").status,
        RunStatus::WaitingRecovery
    );
    assert_eq!(
        record_of(&state, "abandon-recovery-sibling").recovery,
        sibling.recovery
    );
    assert!(state.application.active_runs(None).await.is_empty());

    // The paused ordinary Run still resumes on the ordinary frozen snapshot path.
    let (status, _) = call(app.clone(), "POST", &format!("/runs/{paused}/resume"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    wait_idle(&state).await;
    assert_eq!(record_of(&state, &paused).status, RunStatus::Completed);
    assert_eq!(
        record_of(&state, "abandon-recovery-target").status,
        RunStatus::Aborted
    );
}
