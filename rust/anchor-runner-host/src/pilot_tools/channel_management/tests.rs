use super::*;
use crate::application::channel_management::fake::FakeChannels;
use anchor_platform_session::{CreateSession, TurnStatus};
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, PilotTools, Arc<FakeChannels>) {
    let root = tempfile::tempdir().unwrap();
    let graph = root.path().join("catalog/task");
    std::fs::create_dir_all(&graph).unwrap();
    crate::application::channel_management::fake::install_channel_plugin(
        &root.path().join("catalog"),
        "gateway",
    );
    std::fs::write(graph.join("graph.json"),json!({"entry":"wait","agents":{"reply":{"model":"fixture","instructions":"fixture"}},"ops":{"wait":{"host":{"operation":"session.wait_input"}},"deliver":{"host":{"operation":"session.reply"}}},"nodes":[{"id":"wait","op":"wait"},{"id":"reply","agent":"reply","plugins":["gateway"]},{"id":"deliver","op":"deliver"}],"edges":[{"from":"wait","to":"reply"},{"from":"reply","to":"deliver"},{"from":"deliver","to":"wait"}]}).to_string()).unwrap();
    std::fs::write(
        graph.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[{"id":"gateway"}]}).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join("state/platform")).unwrap();
    let sessions = anchor_platform_session::SessionStore::open(
        root.path().join("state/platform/sessions.sqlite"),
    )
    .unwrap();
    sessions
        .create(
            "local",
            CreateSession {
                id: Some("pilot".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let port = Arc::new(FakeChannels::new());
    let tools = PilotTools {
        application: crate::application::RunApplication::new(
            root.path().join("state"),
            root.path().join("catalog"),
        )
        .with_channel_management(port.clone()),
        graph_name: String::new(),
        library_root: root.path().join("catalog"),
        data_root: root.path().join("state"),
        workspace_root: root.path().join("workspaces"),
        sessions,
        owner: "local".into(),
        session: "pilot".into(),
        turn: String::new(),
        context: None,
    };
    (root, tools, port)
}

fn arguments() -> Value {
    json!({"graph":"task","users":["alice"],"send_users":["alice"]})
}

#[tokio::test]
async fn channel_mutations_require_an_owned_active_turn_and_leave_session_execution_grants_unchanged()
 {
    // The bot plan resolves the Library root from the process environment.
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (_root, mut tools, port) = fixture();
    let (graph, members) = configuration_arguments(&arguments()).unwrap();
    assert!(
        tools
            .configure_channel_approved(&graph, members.clone())
            .await
            .is_err()
    );
    let (turn, _) = tools
        .sessions
        .create_turn("local", "pilot", "initialize", None)
        .unwrap();
    tools = tools.for_turn(&turn.id);
    tools.owner = "other".into();
    assert!(
        tools
            .configure_channel_approved(&graph, members.clone())
            .await
            .is_err()
    );
    tools.owner = "local".into();
    let before = tools.sessions.get("local", "pilot").unwrap();
    tools
        .configure_channel_approved(&graph, members.clone())
        .await
        .unwrap();
    // The confirmation carries the saved binding, Plugin included.
    let binding = port.get("task").unwrap().binding;
    assert_eq!(binding.plugin, "gateway");
    tools
        .enable_channel_approved(&graph, true, &binding)
        .await
        .unwrap();
    assert!(port.get("task").unwrap().enabled);
    let after = tools.sessions.get("local", "pilot").unwrap();
    assert_eq!(before.network_allowed, after.network_allowed);
    assert_eq!(before.plugin_mounts, after.plugin_mounts);
    assert!(tools.application.records().unwrap().is_empty());
    tools
        .sessions
        .finish_turn("local", "pilot", &turn.id, TurnStatus::Completed, None)
        .unwrap();
    assert!(
        tools
            .enable_channel_approved(&graph, false, &binding)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn malformed_bindings_unknown_graphs_and_model_credentials_or_approval_are_rejected() {
    // The bot plan resolves the Library root from the process environment.
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let (_root, tools, port) = fixture();
    for field in [
        "secret",
        "WECOM_BOT_SECRET",
        "credential",
        "approval",
        "confirm",
        "network",
        "owner",
        "platform",
        "reply_node",
        "plugin",
    ] {
        let mut request = arguments();
        request[field] = json!("not-a-model-parameter");
        assert!(configuration_arguments(&request).is_err(), "{field}");
        assert!(enable_arguments(&json!({"graph":"task","enabled":true,field:true})).is_err());
    }
    let mut request = arguments();
    request["send_users"] = json!([]);
    assert!(configuration_arguments(&request).is_err());
    let mut request = arguments();
    request["private_only"] = json!("yes");
    assert!(configuration_arguments(&request).is_err());
    request["private_only"] = json!(true);
    assert!(configuration_arguments(&request).unwrap().1.private_only);
    let (_, members) = configuration_arguments(&arguments()).unwrap();
    assert!(!members.private_only);
    for graph in ["missing", "../task", ""] {
        assert!(
            tools
                .application
                .configure_channel("local", graph, members.clone())
                .await
                .is_err(),
            "{graph}"
        );
    }
    assert!(port.get("task").is_none());
    assert!(
        tools
            .application
            .set_channel_enabled("local", "task", true)
            .await
            .is_err()
    );
}
