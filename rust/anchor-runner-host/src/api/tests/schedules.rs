use super::*;
use chrono::{Local, NaiveDateTime, Timelike};
use serde_json::json;

#[tokio::test]
async fn schedules_crud_reload_and_timeline_keep_python_contract() {
    let (root, state) = fixture();
    let app = router(state.clone());
    let (status, created) = call(
        app.clone(),
        "POST",
        "/schedules",
        Some(r#"{"graph":"fixture","rule":{"type":"daily","time":"09:15"},"input":{"arg":1}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let schedule = created["schedule"].clone();
    assert_eq!(schedule["graph"], "fixture");
    assert_eq!(schedule["rule"], json!({"type":"daily","time":"09:15"}));
    assert_eq!(schedule["input"], json!({"arg":1}));
    assert_eq!(schedule["created_at"].as_str().unwrap().len(), 19);
    assert_eq!(schedule["next_at"].as_str().unwrap().len(), 19);

    let (status, listed) = call(app.clone(), "GET", "/schedules", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["schedules"][0], schedule);
    let (status, timeline) = call(app.clone(), "GET", "/timeline?days=30", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(timeline["capabilities"]["scheduling"], true);
    assert_eq!(timeline["schedules"][0], schedule);
    assert!(
        timeline["scheduled"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["schedule"] == schedule["id"] && item["status"] == "planned" })
    );

    let (status, invalid) = call(
        app.clone(),
        "POST",
        "/schedules",
        Some(r#"{"graph":"fixture","rule":{"type":"weekly","time":"09:15","weekdays":[]}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
    let identifier = schedule["id"].as_str().unwrap();
    let (status, deleted) = call(
        app.clone(),
        "DELETE",
        &format!("/schedules/{identifier}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted, json!({"schedule":identifier,"deleted":true}));
    let (status, _) = call(app, "DELETE", &format!("/schedules/{identifier}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    drop(state);
    let reloaded =
        crate::api::schedules::ScheduleStore::open(root.path().join("state/schedules.json"))
            .unwrap();
    assert!(reloaded.lock().unwrap().items.is_empty());
}

#[tokio::test]
async fn busy_due_schedule_skips_existing_admission_without_creating_a_run() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    write_graph_bundle(&state.bundle_root, &two_node_definition("true")).unwrap();
    let app = router(state.clone());
    let run = start_and_pause(&state, &app).await;
    let count_before = state.application.records().unwrap().len();
    let due = Local::now().naive_local().with_nanosecond(0).unwrap();
    {
        let mut store = state.schedules.lock().unwrap();
        store.items.push(crate::api::schedules::ScheduleItem {
            id: "busy-test".into(),
            graph: "fixture".into(),
            rule: json!({"type":"once","at":due.format("%Y-%m-%dT%H:%M:%S").to_string()}),
            input: json!({}),
            created_at: (due - chrono::Duration::seconds(1))
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string(),
            next_at: due.format("%Y-%m-%dT%H:%M:%S").to_string(),
            enabled: true,
        });
        store.persist().unwrap();
    }
    crate::api::schedules::tick_schedules(&state, due)
        .await
        .unwrap();
    assert_eq!(state.application.records().unwrap().len(), count_before);
    let store = state.schedules.lock().unwrap();
    assert!(
        !store
            .items
            .iter()
            .find(|item| item.id == "busy-test")
            .unwrap()
            .enabled
    );
    assert!(state.application.metadata(&run).unwrap().is_some());
}

#[test]
fn timeline_marks_persisted_admission_blockers_busy_across_history_pages() {
    let (_root, state) = fixture();
    let now = NaiveDateTime::parse_from_str("2026-10-06T12:00:00", "%Y-%m-%dT%H:%M:%S").unwrap();
    let start = NaiveDateTime::parse_from_str("2026-10-05T00:00:00", "%Y-%m-%dT%H:%M:%S").unwrap();
    let end = NaiveDateTime::parse_from_str("2026-10-07T00:00:00", "%Y-%m-%dT%H:%M:%S").unwrap();
    let future_end =
        NaiveDateTime::parse_from_str("2026-10-14T00:00:00", "%Y-%m-%dT%H:%M:%S").unwrap();

    let cases = [
        // This Run started before the requested timeline history page.
        (
            "long-run",
            "2026-10-06T09:00:00",
            "2026-10-04T12:00:00",
            "running",
            true,
        ),
        // A paused Run remains a durable admission blocker after its last update.
        (
            "paused-run",
            "2026-10-06T10:00:00",
            "2026-10-06T08:00:00",
            "paused",
            false,
        ),
    ];
    for (id, at, began, status, running) in cases {
        let at = NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S").unwrap();
        let began = NaiveDateTime::parse_from_str(began, "%Y-%m-%dT%H:%M:%S").unwrap();
        state.schedules.lock().unwrap().items = vec![crate::api::schedules::ScheduleItem {
            id: id.into(),
            graph: "fixture".into(),
            rule: json!({"type":"once","at":at.format("%Y-%m-%dT%H:%M:%S").to_string()}),
            input: json!({}),
            created_at: (at - chrono::Duration::days(1))
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string(),
            next_at: at.format("%Y-%m-%dT%H:%M:%S").to_string(),
            enabled: false,
        }];
        let run = json!({
            "run":"blocking-run", "graph":"fixture", "status":status,
            "running":running, "started":began.format("%Y-%m-%dT%H:%M:%S").to_string(),
            "updated":(at - chrono::Duration::minutes(10)).format("%Y-%m-%dT%H:%M:%S").to_string(),
            "trigger":{"source":"manual"}
        });
        let projected =
            crate::api::schedules::timeline_projection(&state, now, start, end, future_end, &[run])
                .unwrap();
        assert_eq!(projected[0]["status"], "missed_busy", "{id}");
    }
}

#[test]
fn reload_accepts_legacy_schedule_array_and_skips_downtime_occurrences() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state/schedules.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"[{"id":"daily","graph":"fixture","rule":{"type":"daily","time":"09:00"},"input":{"x":1},"created_at":"2026-10-01T08:00:00","next_at":"2026-10-06T09:00:00","enabled":true},{"id":"once","graph":"fixture","rule":{"type":"once","at":"2026-10-05T09:00:00"},"input":{},"created_at":"2026-10-01T08:00:00","next_at":"2026-10-05T09:00:00","enabled":true}]"#).unwrap();
    let schedules = crate::api::schedules::ScheduleStore::open(path.clone()).unwrap();
    let state = crate::api::ApiState {
        bundle_root: root.path().join("bundle"),
        catalog_root: root.path().to_path_buf(),
        application: crate::application::RunApplication::new(
            root.path().join("runs"),
            root.path().to_path_buf(),
        ),
        data_root: root.path().join("runs"),
        workspace_root: root.path().join("workspaces"),
        graph_name: "fixture".into(),
        loopback: true,
        api_keys: Vec::new(),
        schedules,
        pilots: crate::pilot_host::PilotService::default(),
        wecom: Default::default(),
        channel_event_locks: Default::default(),
        plugin_checkout: None,
    };
    let now = NaiveDateTime::parse_from_str("2026-10-06T09:01:00", "%Y-%m-%dT%H:%M:%S").unwrap();
    crate::api::schedules::skip_missed_schedules(&state, now).unwrap();
    drop(state);
    let reloaded = crate::api::schedules::ScheduleStore::open(path).unwrap();
    let store = reloaded.lock().unwrap();
    assert_eq!(
        store
            .items
            .iter()
            .find(|item| item.id == "daily")
            .unwrap()
            .next_at,
        "2026-10-07T09:00:00"
    );
    assert!(
        !store
            .items
            .iter()
            .find(|item| item.id == "once")
            .unwrap()
            .enabled
    );
}

#[test]
fn schedule_path_has_a_single_cross_process_host_owner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state/schedules.json");
    let first = crate::api::schedules::ScheduleStore::open(path.clone()).unwrap();
    assert!(crate::api::schedules::ScheduleStore::open(path.clone()).is_err());
    drop(first);
    assert!(crate::api::schedules::ScheduleStore::open(path).is_ok());
}
