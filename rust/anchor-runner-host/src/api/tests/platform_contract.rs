use super::*;

fn persisted_updated(state: &ApiState, run: &str) -> String {
    let modified = std::fs::metadata(state.data_root.join("runs").join(format!("{run}.json")))
        .unwrap()
        .modified()
        .unwrap();
    chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339()
}

#[tokio::test]
async fn trigger_freezes_objective_and_manual_or_schedule_source() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let manual = json!({"source":"manual","schedule":null,"scheduled_at":null});
    let cases = [
        (json!({"graph":"fixture"}), "fixture", manual.clone()),
        (
            json!({"graph":"fixture","objective":null,"trigger":null}),
            "fixture",
            manual.clone(),
        ),
        (
            json!({"graph":"fixture","objective":"","trigger":{"source":"manual"}}),
            "fixture",
            manual.clone(),
        ),
        (
            json!({"graph":"fixture","objective":"one run only","input":{"task":1},
                "trigger":{"source":"manual"}}),
            "one run only",
            manual,
        ),
        (
            json!({"graph":"fixture","objective":"scheduled objective", "trigger":{
                "source":"schedule","schedule":"daily","scheduled_at":"2026-10-05T09:30:00"}}),
            "scheduled objective",
            json!({"source":"schedule","schedule":"daily","scheduled_at":"2026-10-05T09:30:00"}),
        ),
        (
            json!({"graph":"fixture", "trigger":{
                "source":"schedule","schedule":"once","scheduled_at":"2026-10-05T09:30:00.123+08:00"}}),
            "fixture",
            json!({"source":"schedule","schedule":"once","scheduled_at":"2026-10-05T09:30:00.123+08:00"}),
        ),
    ];
    for (request, objective, trigger) in cases {
        let (status, accepted) =
            call(app.clone(), "POST", "/trigger", Some(&request.to_string())).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
        assert_eq!(accepted["graph"], "fixture");
        assert_eq!(accepted.as_object().unwrap().len(), 2);
        let run = accepted["run"].as_str().unwrap();
        wait_idle(&state).await;
        let record = FileRunStore::new(state.data_root.join("runs"))
            .load(run)
            .unwrap()
            .unwrap();
        assert_eq!(record.status, RunStatus::Completed);
        assert_eq!(record.snapshot.objective, objective);
        assert_eq!(record.graph_digest, record.snapshot.digest().unwrap());
        let metadata = state.application.metadata(run).unwrap().unwrap();
        assert_eq!(metadata.graph_digest, record.graph_digest);
        assert_eq!(metadata.trigger_source, trigger["source"].as_str().unwrap());
        assert_eq!(json!(metadata.schedule), trigger["schedule"]);
        assert_eq!(json!(metadata.scheduled_at), trigger["scheduled_at"]);
        let (_, detail) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
        assert_eq!(detail["state"]["objective"], objective);
        assert_eq!(detail["state"]["trigger"], trigger);
        assert_eq!(detail["control_requested"], Value::Null);
        assert_eq!(detail["state"]["updated"], persisted_updated(&state, run));
        let (_, listed) = call(app.clone(), "GET", "/runs", None).await;
        let listed_run = listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["run"] == run)
            .unwrap();
        assert_eq!(listed_run["objective"], objective);
        assert_eq!(listed_run["trigger"], trigger);
        assert_eq!(listed_run["updated"], detail["state"]["updated"]);
        let (_, timeline) = call(app.clone(), "GET", "/timeline", None).await;
        let timeline_run = timeline["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["run"] == run)
            .unwrap();
        assert_eq!(timeline_run, listed_run);
    }
    let (_, graph) = call(app, "GET", "/graphs/fixture", None).await;
    assert_eq!(graph["definition"]["objective"], "fixture");
}

#[tokio::test]
async fn invalid_trigger_is_rejected_before_creating_a_run() {
    let (_root, state) = fixture();
    let app = router(state.clone());
    let invalid = [
        json!({"objective":12}),
        json!({"objective":{}}),
        json!({"input":[]}),
        json!({"trigger":"schedule"}),
        json!({"trigger":{}}),
        json!({"trigger":{"source":"webhook"}}),
        json!({"trigger":{"source":"graph_call"}}),
        json!({"trigger":{"source":"manual","unexpected":true}}),
        json!({"trigger":{"source":"manual","schedule":"daily"}}),
        json!({"trigger":{"source":"manual","scheduled_at":"2026-10-05T09:30:00"}}),
        json!({"trigger":{"source":"schedule"}}),
        json!({"trigger":{"source":"schedule","schedule":12,"scheduled_at":"2026-10-05T09:30:00"}}),
        json!({"trigger":{"source":"schedule","schedule":" ","scheduled_at":"2026-10-05T09:30:00"}}),
        json!({"trigger":{"source":"schedule","schedule":"daily"}}),
        json!({"trigger":{"source":"schedule","schedule":"daily","scheduled_at":null}}),
        json!({"trigger":{"source":"schedule","schedule":"daily","scheduled_at":""}}),
        json!({"trigger":{"source":"schedule","schedule":"daily","scheduled_at":"2026-10-05"}}),
        json!({"trigger":{"source":"schedule","schedule":"daily","scheduled_at":"2026-02-30T09:30:00"}}),
    ];
    for mut request in invalid {
        request["graph"] = json!("fixture");
        let (status, rejected) =
            call(app.clone(), "POST", "/trigger", Some(&request.to_string())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{request}: {rejected}");
        assert!(rejected["error"].is_string());
        assert!(state.application.records().unwrap().is_empty());
        assert!(!state.data_root.join("run-metadata").exists());
        assert!(state.application.active_runs(None).await.is_empty());
    }
}

#[tokio::test]
async fn busy_schedule_creates_no_run_or_source_metadata() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    write_graph_bundle(&state.bundle_root, &two_node_definition("true")).unwrap();
    let app = router(state.clone());
    let run = start_and_pause(&state, &app).await;
    let metadata_before = std::fs::read(
        state
            .data_root
            .join("run-metadata")
            .join(format!("{run}.json")),
    )
    .unwrap();
    let request = json!({"graph":"fixture","objective":"must not replace", "trigger":{
        "source":"schedule","schedule":"busy","scheduled_at":"2026-10-05T09:30:00"}});
    let (status, rejected) = call(app, "POST", "/trigger", Some(&request.to_string())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(state.application.records().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_dir(state.data_root.join("run-metadata"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read(
            state
                .data_root
                .join("run-metadata")
                .join(format!("{run}.json"))
        )
        .unwrap(),
        metadata_before
    );
}

#[tokio::test]
async fn updated_tracks_persisted_pause_stop_and_resume_after_restart() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    write_graph_bundle(&state.bundle_root, &two_node_definition("true")).unwrap();
    let app = router(state.clone());
    let original_trigger = json!({"source":"schedule","schedule":"resume-schedule",
        "scheduled_at":"2026-10-05T09:30:00"});
    let request = json!({"graph":"fixture","objective":"frozen run objective",
        "trigger":original_trigger,"input":{"original":true}})
    .to_string();
    let run = start_and_pause_request(&state, &app, &request).await;
    let (_, paused) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(paused["state"]["status"], "paused");
    assert_eq!(paused["state"]["objective"], "frozen run objective");
    assert_eq!(paused["state"]["trigger"], original_trigger);
    assert_eq!(paused["state"]["updated"], persisted_updated(&state, &run));
    let paused_at =
        chrono::DateTime::parse_from_rfc3339(paused["state"]["updated"].as_str().unwrap()).unwrap();
    let started_at =
        chrono::DateTime::parse_from_rfc3339(paused["state"]["started"].as_str().unwrap()).unwrap();
    assert!(paused_at > started_at);
    let mut replacement = two_node_definition("sh -c 'exit 9'");
    replacement["objective"] = json!("new catalog objective");
    let (status, changed) = call(
        app,
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":replacement}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    let mut restarted = state.clone();
    restarted.application =
        RunApplication::new(state.data_root.clone(), state.catalog_root.clone())
            .with_configured_graph(state.graph_name.clone(), state.bundle_root.clone());
    let restarted_app = router(restarted.clone());
    let (_, reopened) = call(restarted_app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(reopened["state"]["updated"], paused["state"]["updated"]);
    assert_eq!(reopened["control_requested"], Value::Null);
    assert_eq!(
        call(
            restarted_app.clone(),
            "POST",
            &format!("/runs/{run}/stop"),
            None
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let (_, stopped) = call(restarted_app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(stopped["state"]["status"], "stopped");
    assert_eq!(stopped["state"]["updated"], persisted_updated(&state, &run));
    let stopped_at =
        chrono::DateTime::parse_from_rfc3339(stopped["state"]["updated"].as_str().unwrap())
            .unwrap();
    assert!(stopped_at > paused_at);
    assert_eq!(
        call(
            restarted_app.clone(),
            "POST",
            &format!("/runs/{run}/resume"),
            None
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_idle(&restarted).await;
    let (_, completed) = call(restarted_app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(completed["state"]["status"], "completed");
    assert_eq!(completed["state"]["objective"], "frozen run objective");
    assert_eq!(completed["state"]["trigger"], original_trigger);
    assert_eq!(completed["state"]["started"], paused["state"]["started"]);
    assert_eq!(
        completed["state"]["updated"],
        persisted_updated(&state, &run)
    );
    assert!(
        chrono::DateTime::parse_from_rfc3339(completed["state"]["updated"].as_str().unwrap())
            .unwrap()
            > stopped_at
    );
    let (_, listed) = call(restarted_app, "GET", "/runs", None).await;
    assert_eq!(listed["runs"][0]["updated"], completed["state"]["updated"]);
}

#[tokio::test]
async fn legacy_metadata_without_schedule_fields_remains_readable() {
    let (_root, state) = fixture();
    let snapshot = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap()
        .snapshot;
    let store = FileRunStore::new(state.data_root.join("runs"));
    std::fs::create_dir_all(state.data_root.join("run-metadata")).unwrap();
    for format in [1, 2] {
        let id = format!("legacy-format-{format}");
        let mut record =
            GraphRunRecord::create_with_id(snapshot.clone(), json!({}), id.clone()).unwrap();
        record.status = RunStatus::Failed;
        store.save(&record).unwrap();
        std::fs::write(
            state
                .data_root
                .join("run-metadata")
                .join(format!("{id}.json")),
            json!({
                "format":format,"run_id":id,"graph":"fixture","graph_digest":record.graph_digest,
                "bundle_source":state.bundle_root.canonicalize().unwrap(),
                "created":"2026-10-04T00:00:00Z","trigger_source":"manual"
            })
            .to_string(),
        )
        .unwrap();
    }
    let app = router(state.clone());
    let (status, listed) = call(app.clone(), "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["runs"].as_array().unwrap().len(), 2);
    for item in listed["runs"].as_array().unwrap() {
        let run = item["run"].as_str().unwrap();
        let (status, detail) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            item["trigger"],
            json!({"source":"manual","schedule":null,"scheduled_at":null})
        );
        assert_eq!(detail["state"]["trigger"], item["trigger"]);
        assert_eq!(detail["state"]["updated"], item["updated"]);
    }
}

#[tokio::test]
async fn create_graph_publishes_initial_authoring_definition_and_plugins() {
    let (_root, state) = fixture();
    let plugin = state.catalog_root.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills/example")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Demo","skills":"skills/"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("skills/example/SKILL.md"), "initial skill").unwrap();
    let definition = json!({
        "objective":"initial definition","agents":{"worker":{"model":"fixture","instructions":"work"}},
        "ops":{},"nodes":[{"id":"work","agent":"worker","plugins":["demo"]}],"edges":[],
        "layout":{"positions":{"work":{"x":10,"y":20}}}
    });
    let app = router(state.clone());
    let body = json!({"name":"initial","definition":definition}).to_string();
    let (status, created) = call(app.clone(), "POST", "/graphs", Some(&body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created, json!({"graph":"initial","definition":definition}));
    let (status, fetched) = call(app.clone(), "GET", "/graphs/initial", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["graph"], "initial");
    assert_eq!(fetched["definition"], definition);
    assert_eq!(fetched["node_plugins"]["work"], json!(["demo"]));
    let bundle = FileGraphBundleLoader::new(state.catalog_root.join("initial"))
        .load()
        .unwrap();
    assert_eq!(bundle.authoring_definition, definition);
    assert_eq!(bundle.plugins[0].id, "demo");
    assert_eq!(
        std::fs::read_to_string(
            state
                .catalog_root
                .join("initial/plugins/demo/skills/example/SKILL.md")
        )
        .unwrap(),
        "initial skill"
    );
    let (status, _) = call(app, "POST", "/graphs", Some(&body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn failed_graph_create_leaves_no_target_or_staging_graph() {
    let (_root, state) = fixture();
    let app = router(state.clone());
    let invalid = [
        Value::Null,
        json!({"objective":"broken","nodes":[]}),
        json!({"objective":"missing plugin","agents":{"worker":{}},"ops":{},
            "nodes":[{"id":"work","agent":"worker","plugins":["missing"]}],"edges":[]}),
    ];
    for definition in invalid {
        let request = json!({"name":"retryable","definition":definition}).to_string();
        let (status, rejected) = call(app.clone(), "POST", "/graphs", Some(&request)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
        assert!(!state.catalog_root.join("retryable").exists());
        assert!(
            !std::fs::read_dir(&state.catalog_root)
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".graph-create-"))
        );
        assert_eq!(
            call(app.clone(), "GET", "/graphs/retryable", None).await.0,
            StatusCode::NOT_FOUND
        );
        let (_, listed) = call(app.clone(), "GET", "/graphs", None).await;
        assert_eq!(listed["graphs"].as_array().unwrap().len(), 1);
    }
    let (status, created) = call(app, "POST", "/graphs", Some(r#"{"name":"retryable"}"#)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
}
