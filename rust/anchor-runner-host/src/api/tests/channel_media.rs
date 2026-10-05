use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

#[tokio::test]
async fn channel_attachment_is_a_readonly_input_for_the_same_op_runner() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    unsafe {
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh,cat,printf");
    }
    write_graph_bundle(&state.bundle_root, &json!({
        "entry":"work","agents":{},
        "ops":{"work":{"run":"if printf changed > /in/channel/data.txt; then exit 9; fi; cat /in/channel/data.txt > copied.txt"}},
        "nodes":[{"id":"work","op":"work"}],"edges":[]
    })).unwrap();
    let app = router(state.clone());
    let run = "channel-00000000-0000-4000-8000-000000000099";
    let body = json!({"graph":"fixture","run":run,"session":"media-test","reply_node":"work",
        "input":{"message":"read attached file"},
        "attachments":[{"name":"data.txt","data_base64":STANDARD.encode(b"frozen authorized input"),"media_type":null}]});
    let (status, result) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{result}");
    wait_idle(&state).await;
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(run)
        .unwrap()
        .unwrap();
    assert_eq!(record.status, RunStatus::Completed, "{record:?}");
    let artifacts = HostArtifacts::new(
        state.data_root.join("artifacts"),
        state.workspace_root.clone(),
    );
    let workspace = artifacts
        .workspace_path(&record.results["work"][0].key)
        .unwrap();
    assert_eq!(
        std::fs::read(workspace.join("copied.txt")).unwrap(),
        b"frozen authorized input"
    );
    let (status, detail) = call(app, "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["attachments"][0]["name"], "data.txt");
    assert!(detail["attachments"][0].get("data_base64").is_none());
    assert!(detail["attachments"][0].get("path").is_none());
}
