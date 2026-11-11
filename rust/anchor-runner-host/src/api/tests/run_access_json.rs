use super::*;

#[tokio::test]
async fn run_status_http_json_bytes_match_the_phase_contract() {
    for (scenario, phase_fields, reason, resume, stop, abandon, executed) in [
        (
            "executing",
            r#""phase":"executing""#,
            "null",
            true,
            true,
            true,
            "[]",
        ),
        (
            "paused",
            r#""phase":"waiting","waiting_for":"human""#,
            r#""paused""#,
            true,
            false,
            true,
            "[]",
        ),
        (
            "budget",
            r#""phase":"waiting","waiting_for":"human""#,
            r#""budget""#,
            true,
            false,
            true,
            "[]",
        ),
        (
            "child",
            r#""phase":"waiting","waiting_for":"child""#,
            "null",
            false,
            false,
            true,
            "[]",
        ),
        (
            "recovery",
            r#""phase":"waiting","waiting_for":"human""#,
            r#""unknown external tool effects""#,
            true,
            false,
            true,
            "[]",
        ),
        (
            "done",
            r#""phase":"settled","outcome":"done""#,
            "null",
            false,
            false,
            false,
            r#"["worker"]"#,
        ),
        (
            "failed",
            r#""phase":"settled","outcome":"failed""#,
            r#""worker failed: recorded reason""#,
            false,
            false,
            false,
            "[]",
        ),
        (
            "aborted",
            r#""phase":"settled","outcome":"aborted""#,
            r#""recorded reason""#,
            false,
            false,
            false,
            "[]",
        ),
        (
            "stopped",
            r#""phase":"waiting","waiting_for":"human""#,
            r#""stopped""#,
            true,
            false,
            true,
            "[]",
        ),
    ] {
        let (_root, state) = fixture();
        let record = seed_scenario(&state, scenario).await;
        let mut metadata = RunMetadata::new(
            scenario.into(),
            "fixture".into(),
            record.graph_digest.clone(),
            &state.bundle_root,
        )
        .unwrap();
        metadata.created = "2026-10-19T00:00:00+00:00".into();
        metadata::save(&state.data_root, &metadata).unwrap();
        let view = state.application.read_run(scenario).unwrap().unwrap();
        let updated = serde_json::to_string(&view.updated()).unwrap();
        let request = Request::builder().uri("/runs").body(Body::empty()).unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        let row = parsed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["run"] == scenario)
            .unwrap();
        let expected = format!(
            r#"{{"session_call":null,"run":"{scenario}","graph":"fixture","running":false,"projection":{{{phase_fields},"reason":{reason},"active":false,"actions":{{"pause":false,"stop":{stop},"resume":{resume},"abandon":{abandon}}},"updated":{updated}}},"started":"2026-10-19T00:00:00+00:00","updated":{updated},"executed":{executed},"objective":"test","trigger":{{"source":"manual","schedule":null,"scheduled_at":null}}}}"#,
        );
        assert_eq!(
            serde_json::to_vec(row).unwrap(),
            expected.as_bytes(),
            "{scenario}"
        );
    }
}
