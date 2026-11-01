use super::*;

fn fixture() -> (tempfile::TempDir, PilotTools) {
    let root = tempfile::tempdir().unwrap();
    let sessions = SessionStore::open(root.path().join("sessions.sqlite")).unwrap();
    let tools = PilotTools {
        application: RunApplication::new(root.path().join("state"), root.path().to_path_buf()),
        graph_name: String::new(),
        library_root: root.path().to_path_buf(),
        data_root: root.path().join("state"),
        workspace_root: root.path().join("work"),
        sessions,
        owner: "local".into(),
        session: "pilot".into(),
        turn: String::new(),
        context: None,
    };
    (root, tools)
}

#[tokio::test]
async fn discovery_and_read_use_application_bundle_validity_and_configured_alias() {
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (root, mut tools) = fixture();
    tools
        .application
        .create_graph("usable", None)
        .await
        .unwrap();
    let broken = root.path().join("broken");
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join("graph.json"), r#"{"nodes":[]}"#).unwrap();
    tools.application = tools
        .application
        .with_configured_graph("alias".into(), root.path().join("usable"));
    tools.graph_name = "alias".into();
    let listing = tools.graph_list(&json!({})).await.unwrap();
    assert_eq!(
        listing,
        json!({"graphs":[{"graph":"alias","active_runs":[]}]})
    );
    let application = tools.application.list_graphs().await.unwrap();
    assert_eq!(
        listing["graphs"][0]["graph"],
        application["graphs"][0]["graph"]
    );
    assert!(tools.application.read_graph("broken").is_err());
    assert!(tools.graph_read(&json!({"graph":"broken"})).await.is_err());
    let read = tools.graph_read(&json!({"graph":"alias"})).await.unwrap();
    assert_eq!(
        read["definition"],
        tools.application.read_graph("alias").unwrap()["definition"]
    );
    assert_eq!(read.as_object().unwrap().len(), 2);
}

#[tokio::test]
async fn validation_shares_acceptance_with_application_and_redacts_private_failures() {
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (_root, tools) = fixture();
    let valid = json!({"entry":"worker","agents":{"worker":{"model":"explicit-legacy-alias"}},"nodes":[{"id":"worker","agent":"worker"}],"edges":[]});
    let mut missing_plugin = valid.clone();
    missing_plugin["nodes"][0]["plugins"] = json!(["not-installed"]);
    for definition in [valid, missing_plugin, json!({"entry":"missing"})] {
        let application = tools.application.validate_graph(&definition);
        let pilot = tools
            .graph_validate(&json!({"definition":definition}))
            .await
            .unwrap();
        assert_eq!(pilot["valid"], application.is_ok());
        if definition["nodes"][0]["plugins"].is_array() {
            assert_eq!(pilot["error"], "invalid Graph or unavailable Plugin");
        }
    }
}

#[tokio::test]
async fn pilot_validate_create_and_update_share_host_model_reference_validation() {
    let (_root, mut tools) = fixture();
    tools
        .sessions
        .create(
            "local",
            anchor_platform_session::CreateSession {
                id: Some("pilot".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let (turn, _) = tools
        .sessions
        .create_turn("local", "pilot", "author", None)
        .unwrap();
    tools.turn = turn.id;
    let configuration = std::sync::Arc::new(
        crate::model_configuration::ModelConfiguration::from_env(&tools.data_root).unwrap(),
    );
    tools.application = tools
        .application
        .with_model_configuration(configuration.clone(), None);
    let valid = json!({"entry":"work","agents":{"worker":{"model":"models.default.flash"}},
        "nodes":[{"id":"work","agent":"worker"}],"edges":[]});
    tools
        .graph_create(&json!({"name":"saved","definition":valid}))
        .await
        .unwrap();
    for model in ["claude-sonnet-4-5", "default"] {
        let mut definition = valid.clone();
        definition["agents"]["worker"]["model"] = json!(model);
        let result = tools
            .graph_validate(&json!({"definition":definition}))
            .await
            .unwrap();
        assert_eq!(result["valid"], false);
        let error = result["error"].as_str().unwrap();
        assert!(error.contains(model) && error.contains("models.backup.pro"));
        assert!(
            tools
                .application
                .validate_graph(&definition)
                .unwrap_err()
                .to_string()
                .contains(error)
        );
        assert!(
            tools
                .graph_create(&json!({"name":"invalid","definition":definition}))
                .await
                .unwrap_err()
                .to_string()
                .contains(error)
        );
        assert!(
            tools
                .graph_update(&json!({"graph":"saved","definition":definition}))
                .await
                .unwrap_err()
                .to_string()
                .contains(error)
        );
        assert!(
            !tools
                .application
                .graph_path_checked("invalid")
                .unwrap()
                .exists()
        );
        assert_eq!(
            tools.application.read_graph("saved").unwrap()["definition"],
            valid
        );
    }
    // The spike's deterministic model name uses exactly its runtime registry.
    let fixture =
        crate::goose_acp::configuration::ModelRegistry::from_values(true, |name| match name {
            "ANCHOR_GOOSE_OPENAI_HOST" => Some("http://127.0.0.1:9999".into()),
            "ANCHOR_GOOSE_MODEL" => Some("fixture-local".into()),
            _ => None,
        })
        .unwrap();
    tools.application = tools
        .application
        .with_model_configuration(configuration, Some(fixture));
    let mut definition = valid;
    definition["agents"]["worker"]["model"] = json!("fixture-local");
    assert_eq!(
        tools
            .graph_validate(&json!({"definition":definition}))
            .await
            .unwrap()["valid"],
        true
    );
}
