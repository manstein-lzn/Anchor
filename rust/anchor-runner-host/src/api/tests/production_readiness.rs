use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[tokio::test]
async fn readiness_is_distinct_from_liveness_and_tracks_shutdown_state() {
    let (_root, state) = fixture();
    let readiness = Arc::new(AtomicBool::new(false));
    let app = router_with_readiness_and_web_root(state, readiness.clone(), std::env::temp_dir());

    let (status, health) = call(app.clone(), "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "ok");
    let (status, ready) = call(app.clone(), "GET", "/ready", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ready["status"], "not_ready");

    readiness.store(true, Ordering::Release);
    let (status, ready) = call(app.clone(), "GET", "/ready", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["status"], "ready");

    readiness.store(false, Ordering::Release);
    let (status, ready) = call(app, "GET", "/ready", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ready["status"], "not_ready");
}

#[tokio::test]
async fn graph_relations_projects_catalog_calls_and_schedule_counts() {
    let (_root, state) = fixture();
    let caller = json!({
        "objective":"caller",
        "entry":"invoke-node",
        "agents":{},
        "ops":{"invoke":{"call":{"graph":"fixture","mode":"detach"}}},
        "nodes":[{"id":"invoke-node","op":"invoke","plugins":[]}],
        "edges":[]
    });
    write_graph_bundle(&state.catalog_root.join("caller"), &caller).unwrap();
    state.schedules.lock().unwrap().items.push(ScheduleItem {
        id: "caller-schedule".into(),
        graph: "caller".into(),
        rule: json!({"type":"once","at":"2026-10-09T10:00:00"}),
        input: json!({}),
        created_at: "2026-10-08T10:00:00".into(),
        next_at: "2026-10-09T10:00:00".into(),
        enabled: true,
        delegation: None,
        remaining: None,
        group: None,
    });

    let (status, relations) = call(router(state), "GET", "/graph-relations", None).await;
    assert_eq!(status, StatusCode::OK, "{relations}");
    assert_eq!(
        relations,
        json!({
            "graphs":[
                {"graph":"caller","objective":"caller","schedules":1},
                {"graph":"fixture","objective":"fixture","schedules":0}
            ],
            "calls":[{
                "graph":"caller",
                "node":"invoke-node",
                "op":"invoke",
                "target":"fixture",
                "mode":"detach"
            }],
            "subscriptions":[]
        })
    );
}

#[tokio::test]
async fn graph_relations_preserves_schedule_count_when_bundle_is_missing() {
    let (_root, state) = fixture();
    std::fs::remove_dir_all(&state.bundle_root).unwrap();
    state.schedules.lock().unwrap().items.push(ScheduleItem {
        id: "missing-graph-schedule".into(),
        graph: "fixture".into(),
        rule: json!({"type":"once","at":"2026-10-09T10:00:00"}),
        input: json!({}),
        created_at: "2026-10-08T10:00:00".into(),
        next_at: "2026-10-09T10:00:00".into(),
        enabled: true,
        delegation: None,
        remaining: None,
        group: None,
    });

    let (status, relations) = call(router(state), "GET", "/graph-relations", None).await;
    assert_eq!(status, StatusCode::OK, "{relations}");
    assert_eq!(
        relations,
        json!({"graphs":[{"graph":"fixture","schedules":1}],"calls":[],"subscriptions":[]})
    );
}

#[tokio::test]
async fn graph_relations_derive_subscription_owner_at_the_http_boundary() {
    let (root, application, _channels, _sessions, session, target) =
        crate::application::graph_tasks::assignment_tests::prepared().await;
    let schedules = ScheduleStore::open(root.path().join("schedules.json")).unwrap();
    let application = application.with_schedule_management(Arc::new(
        crate::api::schedules::ScheduleManagementAdapter::new(schedules.clone()),
    ));
    let created = application
        .create_schedule_with_request(
            "producer",
            &json!({"type":"daily","time":"07:30"}),
            &target.input,
            "local",
            None,
        )
        .await
        .unwrap();
    let id = created["schedule"]["id"].as_str().unwrap();
    let mut definition = application
        .load_graph("assistant")
        .unwrap()
        .1
        .authoring_definition;
    definition["nodes"][1]["plugins"][1]["graph_tasks"] = json!({"schedules":[id]});
    application
        .update_graph("assistant", definition)
        .await
        .unwrap();
    application
        .share_schedule(
            "local",
            id,
            "Morning report",
            std::slice::from_ref(&session),
            true,
        )
        .await
        .unwrap();
    application
        .operator_subscribe_schedule(
            "local",
            id,
            &session,
            "private-instruction-marker",
            Some("09:05"),
        )
        .await
        .unwrap();
    let (_fixture_root, mut state) = fixture();
    state.application = application;
    state.schedules = schedules;
    let (status, owned) = call(router(state.clone()), "GET", "/graph-relations", None).await;
    assert_eq!(status, StatusCode::OK, "{owned}");
    assert_eq!(
        owned["subscriptions"],
        json!([{"graph":"assistant","target":"producer","schedule":id,
            "active":true,"authorized":true,"not_before":"09:05"}])
    );
    assert!(!owned.to_string().contains(&session));
    assert!(!owned.to_string().contains("private-instruction-marker"));
    state.owner = "another-owner".into();
    let (status, other) = call(router(state), "GET", "/graph-relations", None).await;
    assert_eq!(status, StatusCode::OK, "{other}");
    assert_eq!(other["subscriptions"], json!([]));
    assert_eq!(other["graphs"], owned["graphs"]);
    assert_eq!(other["calls"], owned["calls"]);
}

#[tokio::test]
async fn channel_session_projection_is_owner_scoped_and_hides_archived_sessions() {
    let (_root, state) = fixture();
    let key = "a".repeat(32);
    let owner = state.owner.clone();
    let sessions = store(&state).unwrap();
    sessions
        .create(
            &owner,
            anchor_platform_session::CreateSession {
                id: Some("visible-channel".into()),
                title: "Visible channel".into(),
                graph: "fixture".into(),
                channel: std::collections::BTreeMap::from([("source".into(), "wecom".into())]),
                ..Default::default()
            },
        )
        .unwrap();
    sessions
        .create(
            &owner,
            anchor_platform_session::CreateSession {
                id: Some("archived-channel".into()),
                graph: "fixture".into(),
                channel: std::collections::BTreeMap::from([("source".into(), "wecom".into())]),
                ..Default::default()
            },
        )
        .unwrap();
    sessions
        .set_status(
            &owner,
            "archived-channel",
            anchor_platform_session::SessionStatus::Archived,
            "",
        )
        .unwrap();
    sessions
        .create(
            &owner,
            anchor_platform_session::CreateSession {
                id: Some("unbound-session".into()),
                graph: "fixture".into(),
                ..Default::default()
            },
        )
        .unwrap();
    sessions
        .create(
            "api:owner-two",
            anchor_platform_session::CreateSession {
                id: Some("other-owner-channel".into()),
                graph: "fixture".into(),
                channel: std::collections::BTreeMap::from([("source".into(), "wecom".into())]),
                ..Default::default()
            },
        )
        .unwrap();

    let app = router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/channel-sessions")
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let projection: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        projection,
        json!({"sessions":[{
            "id":"visible-channel",
            "title":"Visible channel",
            "graph":"fixture",
            "platform":"wecom"
        }]})
    );
}

#[test]
fn startup_configuration_requires_writable_roots_and_hides_sensitive_paths() {
    let (root, state) = fixture();
    let state_root = root.path().join("state");
    let workspace_root = root.path().join("workspaces");
    let catalog_root = root.path().join("catalog");
    std::fs::create_dir_all(&state_root).unwrap();
    std::fs::create_dir_all(&workspace_root).unwrap();
    std::fs::create_dir_all(&catalog_root).unwrap();
    assert!(
        validate_service_configuration(
            &state.bundle_root,
            &state_root,
            &workspace_root,
            &catalog_root,
        )
        .unwrap()
    );

    let blocked_root = root.path().join("private-secret-marker");
    std::fs::write(&blocked_root, "not a directory").unwrap();
    let error = validate_service_configuration(
        &state.bundle_root,
        &blocked_root,
        &workspace_root,
        &catalog_root,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "configured state directory is unavailable"
    );
    assert!(!error.to_string().contains("private-secret-marker"));

    let invalid_bundle = root.path().join("invalid-bundle");
    std::fs::create_dir_all(&invalid_bundle).unwrap();
    std::fs::write(invalid_bundle.join("graph.json"), "{}").unwrap();
    std::fs::write(
        invalid_bundle.join("manifest.json"),
        "credential-secret-marker",
    )
    .unwrap();
    let error = validate_service_configuration(
        &invalid_bundle,
        &state_root,
        &workspace_root,
        &catalog_root,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "configured Graph bundle is missing or invalid"
    );
    assert!(!error.to_string().contains("credential-secret-marker"));
}

#[test]
fn startup_configuration_allows_missing_graph_but_marks_service_not_ready() {
    let (root, state) = fixture();
    std::fs::remove_dir_all(&state.bundle_root).unwrap();
    std::fs::create_dir_all(root.path().join("state")).unwrap();
    std::fs::create_dir_all(root.path().join("workspaces")).unwrap();
    std::fs::create_dir_all(root.path().join("catalog")).unwrap();
    assert!(
        !validate_service_configuration(
            &state.bundle_root,
            &root.path().join("state"),
            &root.path().join("workspaces"),
            &root.path().join("catalog"),
        )
        .unwrap()
    );
}

#[test]
fn startup_configuration_rejects_relative_and_parent_component_roots() {
    let (root, state) = fixture();
    let state_root = root.path().join("state");
    let workspace_root = root.path().join("workspaces");
    let catalog_root = root.path().join("catalog");

    for (bundle, state, workspace, catalog, message) in [
        (
            PathBuf::from("relative-bundle"),
            state_root.clone(),
            workspace_root.clone(),
            catalog_root.clone(),
            "configured bundle path must be absolute",
        ),
        (
            state.bundle_root.clone(),
            PathBuf::from("relative-state"),
            workspace_root.clone(),
            catalog_root.clone(),
            "configured state path must be absolute",
        ),
        (
            state.bundle_root.clone(),
            PathBuf::from("/tmp/parent/../state"),
            workspace_root.clone(),
            catalog_root.clone(),
            "configured state path must not contain parent components",
        ),
    ] {
        assert_eq!(
            validate_service_configuration(&bundle, &state, &workspace, &catalog)
                .unwrap_err()
                .to_string(),
            message
        );
    }
}

#[cfg(unix)]
#[test]
fn startup_configuration_rejects_symlink_paths_and_overlapping_roots() {
    use std::os::unix::fs::symlink;

    let (root, state) = fixture();
    let target = root.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let link = root.path().join("state-link");
    symlink(&target, &link).unwrap();
    assert_eq!(
        validate_service_configuration(
            &state.bundle_root,
            &link,
            &root.path().join("workspaces"),
            &root.path().join("catalog"),
        )
        .unwrap_err()
        .to_string(),
        "configured state path contains a symlink"
    );

    let shared = root.path().join("shared");
    assert_eq!(
        validate_service_configuration(
            &state.bundle_root,
            &shared,
            &shared.join("workspace"),
            &root.path().join("catalog"),
        )
        .unwrap_err()
        .to_string(),
        "configured state and workspace directories must not overlap"
    );
    assert!(!shared.exists());
}
