use super::*;
use anchor_platform_session::{CreateSession, SessionStore, TurnStatus};

fn sessions(state: &ApiState) -> SessionStore {
    SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap()
}

fn network_plugin(state: &ApiState, valid: bool) {
    if valid {
        crate::test_network_plugin(&state.catalog_root);
        return;
    }
    let directory = state.catalog_root.join("plugins/network");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("plugin.json"),
        json!({"name":"Network","network":valid}).to_string(),
    )
    .unwrap();
}

async fn freeze(
    state: &ApiState,
    session: &str,
    turn: &str,
) -> crate::pilot_tools::PreparedPilotPlugins {
    crate::pilot_tools::frozen_turn_tools(
        &state.application.bwrap,
        state.application.library_root_override.as_deref(),
        &state.data_root,
        &state.catalog_root,
        &state.workspace_root,
        "local",
        session,
        turn,
        anchor_runtime::Cancellation::default(),
        None,
        &[],
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn new_pilot_policy_is_durable_and_explicit_disable_survives_restart_and_next_turn() {
    let _guard = PROCESS_ENV.lock().await;
    let (_root, state) = fixture();
    network_plugin(&state, true);
    let app = router(state.clone());
    let (status, created) = call(
        app.clone(),
        "POST",
        "/sessions",
        Some(r#"{"id":"default-online"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["session"]["plugin_mounts"], json!({"network":true}));
    assert_eq!(created["session"]["network_allowed"], true);
    let store = sessions(&state);
    let events = store.events("local", "default-online", 0).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "session.created");
    assert!(freeze(&state, "default-online", "first").await.network);

    let (status, denied) = call(
        app.clone(),
        "POST",
        "/sessions/default-online/network",
        Some(r#"{"mode":"denied"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{denied}");
    assert_eq!(
        sessions(&state)
            .get("local", "default-online")
            .unwrap()
            .plugin_mounts
            .get("network"),
        Some(&false)
    );
    let events = sessions(&state)
        .events("local", "default-online", 0)
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.network.changed")
            .count(),
        1
    );
    let (status, disabled) = call(
        app.clone(),
        "POST",
        "/sessions/default-online/plugins/network",
        Some(r#"{"on":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    drop(app);
    drop(store);
    let app = router(state.clone());
    let (status, restored) = call(app.clone(), "GET", "/sessions/default-online", None).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(
        restored["session"]["plugin_mounts"],
        json!({"network":false})
    );
    assert_eq!(restored["session"]["network_allowed"], false);
    // An already admitted Turn keeps its policy; a later Turn sees the disable.
    assert!(freeze(&state, "default-online", "first").await.network);
    assert!(!freeze(&state, "default-online", "next").await.network);
    let (status, allowed) = call(
        app,
        "POST",
        "/sessions/default-online/network",
        Some(r#"{"allowed":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{allowed}");
    assert_eq!(
        sessions(&state)
            .get("local", "default-online")
            .unwrap()
            .plugin_mounts
            .get("network"),
        Some(&false)
    );
    assert!(!freeze(&state, "default-online", "still-off").await.network);
}

#[tokio::test]
async fn responses_default_and_continuation_preserve_mount_disable_and_network_denial() {
    let _guard = PROCESS_ENV.lock().await;
    let (_root, state) = fixture();
    network_plugin(&state, true);
    for (mounted, permission) in [(false, true), (true, false)] {
        let app = router(state.clone());
        let (status, first) = call(
            app.clone(),
            "POST",
            "/v1/responses",
            Some(r#"{"input":"first"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let store = sessions(&state);
        let session = store
            .list("local")
            .unwrap()
            .into_iter()
            .find(|session| {
                store
                    .list_turns("local", &session.id)
                    .unwrap()
                    .iter()
                    .any(|turn| first["id"] == format!("resp_{}", turn.id))
            })
            .unwrap();
        assert!(session.graph.is_empty() && session.channel.is_empty());
        assert_eq!(
            session.plugin_mounts,
            BTreeMap::from([("network".into(), true)])
        );
        assert!(session.network_allowed);
        assert!(freeze(&state, &session.id, "initial-policy").await.network);
        store
            .set_plugin_mount(
                "local",
                &session.id,
                "network",
                mounted,
                "operator",
                "explicit choice",
            )
            .unwrap();
        store
            .set_network_allowed("local", &session.id, permission, "operator")
            .unwrap();
        let before = store.get("local", &session.id).unwrap();
        drop(store);
        drop(app);
        let app = router(state.clone());
        let (status, continued) = call(
            app,
            "POST",
            "/v1/responses",
            Some(&json!({"input":"continue","previous_response_id":first["id"]}).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{continued}");
        let after = sessions(&state).get("local", &session.id).unwrap();
        assert_eq!(after.plugin_mounts, before.plugin_mounts);
        assert_eq!(after.network_allowed, before.network_allowed);
        assert_eq!(
            sessions(&state)
                .list_turns("local", &session.id)
                .unwrap()
                .len(),
            2
        );
        assert!(
            !freeze(&state, &session.id, "continued-policy")
                .await
                .network
        );
    }
}

#[tokio::test]
async fn a_default_mount_requires_an_installed_valid_network_plugin_and_api_cannot_inject_policy() {
    let _guard = PROCESS_ENV.lock().await;
    for invalid in [false, true] {
        let (root, state) = fixture();
        if invalid {
            network_plugin(&state, false);
        }
        let app = router(state.clone());
        let (status, created) = call(
            app.clone(),
            "POST",
            "/sessions",
            Some(r#"{"id":"no-capability"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["session"]["plugin_mounts"], json!({"network":true}));
        let prepared = freeze(&state, "no-capability", "first").await;
        assert!(!prepared.network);
        assert!(prepared.ports.is_empty());
        assert!(!prepared.reasons.is_empty());
        if !invalid {
            assert!(!root.path().join("plugins/network").exists());
        }
        for forbidden in [
            json!({"plugin_mounts":{"other":true}}),
            json!({"network_allowed":false}),
            json!({"graph":"business"}),
            json!({"channel":{"source":"wecom"}}),
        ] {
            let (status, body) = call(
                app.clone(),
                "POST",
                "/sessions",
                Some(&forbidden.to_string()),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        }
        let (status, body) = call(
            app,
            "POST",
            "/v1/responses",
            Some(r#"{"input":"forged","plugin_mounts":{"other":true}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(sessions(&state).list("local").unwrap().len(), 1);
    }
}

#[tokio::test]
async fn legacy_pilot_read_and_responses_continuation_do_not_backfill_empty_mounts() {
    let _guard = PROCESS_ENV.lock().await;
    let (_root, state) = fixture();
    network_plugin(&state, true);
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let store = sessions(&state);
    store
        .create(
            "local",
            CreateSession {
                id: Some("responses-legacy".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let (turn, _) = store
        .create_turn("local", "responses-legacy", "old", Some("old input"))
        .unwrap();
    store
        .finish_turn(
            "local",
            "responses-legacy",
            &turn.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    drop(store);
    let app = router(state.clone());
    let (_, read) = call(app.clone(), "GET", "/sessions/responses-legacy", None).await;
    assert_eq!(read["session"]["plugin_mounts"], json!({}));
    let (status, response) = call(
        app,
        "POST",
        "/v1/responses",
        Some(
            &json!({"input":"continue","previous_response_id":format!("resp_{}", turn.id)})
                .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let retained = sessions(&state).get("local", "responses-legacy").unwrap();
    assert!(retained.plugin_mounts.is_empty());
    assert!(!freeze(&state, "responses-legacy", "new-turn").await.network);
}

#[tokio::test]
async fn network_modes_validate_before_publication_and_preserve_context_ownership() {
    let _guard = PROCESS_ENV.lock().await;
    let (_root, state) = fixture();
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let store = sessions(&state);
    store
        .create(
            "local",
            CreateSession {
                id: Some("policy".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let original = store.get("local", "policy").unwrap();
    let app = router(state.clone());
    for (body, expected) in [
        (json!({"mode":"invalid"}), StatusCode::BAD_REQUEST),
        (json!({"mode":true}), StatusCode::BAD_REQUEST),
        (
            json!({"mode":"enabled","allowed":true}),
            StatusCode::BAD_REQUEST,
        ),
        (json!({"mode":"enabled"}), StatusCode::CONFLICT),
    ] {
        let (status, result) = call(
            app.clone(),
            "POST",
            "/sessions/policy/network",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, expected, "{result}");
        assert_eq!(store.get("local", "policy").unwrap(), original);
        assert_eq!(store.events("local", "policy", 0).unwrap().len(), 1);
        assert!(
            !state.application.library_root().join("plugins").exists(),
            "a refused request created a Library"
        );
    }
    // Offline policy is useful even when the provider is absent or damaged.
    for (mode, allowed) in [("denied", false), ("auto", true)] {
        let (status, result) = call(
            app.clone(),
            "POST",
            "/sessions/policy/network",
            Some(&json!({"mode":mode}).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let saved = store.get("local", "policy").unwrap();
        assert_eq!(saved.network_allowed, allowed);
        assert_eq!(saved.plugin_mounts.get("network"), Some(&false));
        assert!(
            !state
                .application
                .library_root()
                .join("plugins/network")
                .exists()
        );
    }
    network_plugin(&state, true);
    let (status, result) = call(
        app.clone(),
        "POST",
        "/sessions/policy/network",
        Some(r#"{"mode":"enabled"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let network = result["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "network")
        .unwrap();
    assert_eq!(network["networkMode"], "enabled");
    assert_eq!(network["networkAllowed"], true);
    for (id, graph, channel) in [
        ("graph-policy", "workflow", BTreeMap::new()),
        (
            "channel-policy",
            "",
            BTreeMap::from([("source".into(), "feishu".into())]),
        ),
    ] {
        let original = store
            .create(
                "local",
                CreateSession {
                    id: Some(id.into()),
                    graph: graph.into(),
                    channel,
                    ..Default::default()
                },
            )
            .unwrap();
        let (status, result) = call(
            app.clone(),
            "POST",
            &format!("/sessions/{id}/network"),
            Some(r#"{"mode":"enabled"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{result}");
        assert_eq!(store.get("local", id).unwrap(), original);
        assert_eq!(store.events("local", id, 0).unwrap().len(), 1);
    }
}
