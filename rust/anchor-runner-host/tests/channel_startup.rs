#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;

use fixture::{Host, wait_until};
use serde_json::json;
use std::fs;

#[test]
fn a_bot_configured_on_its_graph_starts_its_gateway_and_the_graph_stays_editable() {
    let mut graph = json!({
        "objective":"editable channel", "entry":"wait_input",
        "agents":{"worker":{"model":"models.default.flash"}},
        "ops":{
            "wait_input":{"host":{"operation":"session.wait_input"}},
            "reply":{"host":{"operation":"session.reply"}}
        },
        "nodes":[
            {"id":"wait_input","op":"wait_input"},
            {"id":"assistant","agent":"worker","plugins":["wecom",{"id":"network","network":"denied"}]},
            {"id":"reply","op":"reply"}
        ],
        "edges":[
            {"from":"wait_input","to":"assistant"},
            {"from":"assistant","to":"reply"},
            {"from":"reply","to":"wait_input"}
        ]
    });
    let host = Host::new(&graph);
    let root = host.root.path();
    fs::rename(root.join("bundle"), root.join("assistant")).unwrap();
    fs::write(
        root.join("assistant/manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[{"id":"wecom"},{"id":"network"}]})
            .to_string(),
    )
    .unwrap();
    fixture::install_system_plugin(root, anchor_graph_host::SystemPlugin::Wecom);

    let server = host.serve_configured_without_model(|process| {
        process
            .env("ANCHOR_WECOM_WS_URL", "ws://127.0.0.1:1")
            .env_remove("ANCHOR_RUNNER_GRAPH_NAME")
            .env_remove("ANCHOR_RUNNER_BUNDLE_ROOT");
    });
    let (status, configured) = server.request(
        "PUT",
        "/graphs/assistant/channel",
        Some(&json!({"users":["fixture-member"],"send_users":["fixture-member"]})),
    );
    assert_eq!(status, 200, "{configured}");
    let (status, credentials) =
        server.request("GET", "/graphs/assistant/channel/credentials", None);
    assert_eq!(status, 200, "{credentials}");
    for (field, value) in [
        ("WECOM_BOT_ID", "fixture-bot"),
        ("WECOM_BOT_SECRET", "fixture-secret"),
    ] {
        let (status, saved) = server.request("PUT", &format!("/graphs/assistant/channel/credentials/{field}"),
            Some(&json!({"authorization_digest":credentials["targets"][0]["authorization_digest"],"value":value})));
        assert_eq!(status, 200, "{saved}");
    }
    let (status, enabled) = server.request(
        "POST",
        "/graphs/assistant/channel/enabled",
        Some(&json!({"enabled":true,"expected_binding":configured["binding"]})),
    );
    assert_eq!(status, 200, "{enabled}");
    wait_until("the Graph's gateway process", || {
        host.state_root()
            // The gateway state directory is named by a digest of the Graph.
            .join("channels/graphs/a39a7ffad4a3013f/control.json")
            .is_file()
    });
    let (status, channel) = server.request("GET", "/graphs/assistant/channel", None);
    assert_eq!(status, 200, "{channel}");
    assert_eq!(channel["enabled"], true);
    assert_eq!(channel["reply_node"], "assistant");
    assert_eq!(channel["binding"]["plugin"], "wecom");
    let (status, listed) = server.request("GET", "/channels", None);
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["channels"][0]["graph"], "assistant");
    graph["objective"] = json!("updated channel definition");
    let (status, updated) = server.request(
        "PUT",
        "/graphs/assistant",
        Some(&json!({"definition":graph})),
    );
    assert_eq!(status, 200, "{updated}");
    assert_eq!(updated["definition"], graph);
    assert!(!root.join("dev").exists());
}
