use super::*;
use crate::api::tests::{finish_with_artifacts, seed_scenario};
use crate::application::run_access::fixtures;
use crate::run_metadata::{self as metadata, RunMetadata};

async fn detail_json(state: ApiState, id: &str) -> Value {
    let response = get_run(State(state), AxumPath(id.into())).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn frozen_record(state: &ApiState) -> GraphRunRecord {
    let snapshot = GraphSnapshot::from_authoring(json!({
        "objective":"frozen graph", "entry":"module",
        "agents":{"worker":{"model":"fixture","instructions":"private instructions"}},
        "graphs":{"review":{"entry":"first","exit":"last","nodes":[
            {"id":"first","agent":"worker"},{"id":"last","agent":"worker"}
        ],"edges":[{"from":"first","to":"last"}]}},
        "nodes":[{"id":"module","graph":"review"}],"edges":[]
    }))
    .unwrap();
    let record = GraphRunRecord::create_with_id(snapshot, json!({}), "frozen-run").unwrap();
    fixtures::create(&state.application, &record);
    metadata::save(
        &state.data_root,
        &RunMetadata::new(
            record.run_id.clone(),
            "fixture".into(),
            record.graph_digest.clone(),
            &state.bundle_root,
        )
        .unwrap(),
    )
    .unwrap();
    record
}

#[tokio::test]
async fn frozen_topology_survives_live_graph_changes_and_missing_definition() {
    let (_root, state) = super::super::tests::fixture();
    let record = frozen_record(&state);
    finish_with_artifacts(&state, &record.run_id).await;
    let initial = detail_json(state.clone(), &record.run_id).await;
    assert_eq!(initial["definition"]["entry"], "module/first");
    assert_eq!(initial["definition"]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        initial["definition"]["edges"],
        json!([{"from":"module/first","to":"module/last"}])
    );
    std::fs::write(
        state.bundle_root.join("graph.json"),
        "changed and invalid graph",
    )
    .unwrap();
    let changed = detail_json(state.clone(), &record.run_id).await;
    assert_eq!(changed["definition"], initial["definition"]);
    std::fs::remove_file(state.bundle_root.join("graph.json")).unwrap();
    let removed = detail_json(state, &record.run_id).await;
    assert_eq!(removed["definition"], initial["definition"]);
    assert_eq!(removed["nodes"], json!(["module/first", "module/last"]));
    assert!(!removed["definition"].to_string().contains("private"));
}

#[tokio::test]
async fn list_and_detail_share_status_control_and_attention_projection() {
    for (id, phase, waiter, outcome, resume, stop, attention) in [
        ("executing", "executing", None, None, true, true, true),
        ("child", "waiting", Some("child"), None, false, false, true),
        (
            "recovery",
            "waiting",
            Some("human"),
            None,
            true,
            false,
            true,
        ),
        ("budget", "waiting", Some("human"), None, true, false, true),
        ("done", "settled", None, Some("done"), false, false, false),
    ] {
        let (_root, state) = super::super::tests::fixture();
        let record = seed_scenario(&state, id).await;
        metadata::save(
            &state.data_root,
            &RunMetadata::new(
                id.into(),
                "fixture".into(),
                record.graph_digest.clone(),
                &state.bundle_root,
            )
            .unwrap(),
        )
        .unwrap();
        let detail = detail_json(state.clone(), id).await["projection"].clone();
        let runs = projected_runs(&state).await.unwrap();
        let listed = runs.iter().find(|run| run["run"] == id).unwrap();
        assert_eq!(detail, listed["projection"]);
        assert_eq!(detail["phase"], phase);
        assert_eq!(detail["actions"]["resume"], resume);
        assert_eq!(detail["actions"]["stop"], stop);
        assert_eq!(detail["waiting_for"].as_str(), waiter);
        assert_eq!(detail["outcome"].as_str(), outcome);
        assert_eq!(detail["actions"]["pause"], false);
        assert_eq!(
            phase == "waiting" || (phase == "executing" && detail["active"] == false),
            attention
        );
        assert!(detail.get("needs_attention").is_none());
    }
}

#[tokio::test]
async fn incomplete_run_identity_does_not_offer_inactive_controls() {
    let (_root, state) = super::super::tests::fixture();
    let record = frozen_record(&state);
    let mut metadata = state.application.metadata(&record.run_id).unwrap().unwrap();
    metadata.graph_digest = "different-frozen-graph".into();
    metadata::save(&state.data_root, &metadata).unwrap();
    let detail = detail_json(state.clone(), &record.run_id).await;
    assert_eq!(
        detail["projection"]["actions"],
        json!({"pause":false,"stop":false,"resume":false,"abandon":false})
    );
    std::fs::remove_file(
        state
            .data_root
            .join("run-metadata")
            .join(format!("{}.json", record.run_id)),
    )
    .unwrap();
    let runs = projected_runs(&state).await.unwrap();
    assert_eq!(
        runs[0]["projection"]["actions"],
        json!({"pause":false,"stop":false,"resume":false,"abandon":false})
    );
}
