use super::fixtures;
use super::*;
use fixtures::{scenarios, work_record as record};
use serde_json::json;

#[test]
fn projected_schedule_rule_recognizes_only_public_phases() {
    for (phase, unfinished) in [
        ("executing", true),
        ("waiting", true),
        ("settled", false),
        ("ready", false),
        ("running", false),
        ("paused", false),
        ("budget_stopped", false),
        ("waiting_call", false),
        ("waiting_recovery", false),
        ("stopped", false),
        ("completed", false),
        ("failed", false),
        ("aborted", false),
        ("stopping", false),
        ("pausing", false),
        ("waiting_resume", false),
        ("", false),
        ("future", false),
    ] {
        assert_eq!(
            projected_run_is_unfinished(&json!({"projection":{"phase":phase}})),
            unfinished,
            "{phase}"
        );
    }
    for run in [
        json!({}),
        json!({"status":"running"}),
        json!({"projection":null}),
        json!({"projection":[]}),
        json!({"projection":{"phase":null}}),
        json!({"projection":{"phase":5}}),
        json!({"projection":{"phase":[]}}),
        json!({"projection":{"phase":{"executing":null}}}),
    ] {
        assert!(!projected_run_is_unfinished(&run));
    }
}

#[tokio::test]
async fn intent_controls_preserve_missing_and_unknown_operation_errors() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
    assert!(matches!(
        application.stop("missing").await,
        Err(ApplicationError::Missing)
    ));
    assert!(matches!(
        application.pause("missing").await,
        Err(ApplicationError::Missing)
    ));
    assert!(matches!(
        application.resume("missing").await,
        Err(ApplicationError::Missing)
    ));
    assert!(matches!(application.control("missing", "unknown").await,
        Err(ApplicationError::Invalid(message)) if message == "unknown Run control"));
}

#[test]
fn read_contract_exposes_timestamp_and_reserves_unmapped_outcomes() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
    fixtures::create(&application, &record("timestamp"));
    let view = fixtures::view(&application, "timestamp");
    assert!(view.updated().is_some());
    for (wire, expected) in [
        ("escalated", RunOutcome::Escalated),
        ("unknown", RunOutcome::Unknown),
    ] {
        assert_eq!(
            serde_json::from_value::<RunOutcome>(json!(wire)).unwrap(),
            expected
        );
    }
}

#[test]
fn dispatch_success_and_stopped_execution_rules_cover_kernel_facts() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
    for id in scenarios(&application) {
        let view = fixtures::view(&application, id);
        assert_eq!(view.can_dispatch(), id == "executing", "{id}");
        assert_eq!(
            view.has_stopped_execution(),
            matches!(id, "stopped" | "done" | "failed" | "aborted"),
            "{id}"
        );
        assert_eq!(view.has_succeeded(), id == "done", "{id}");
        assert_eq!(
            view.occupies_conversation_execution(),
            id == "executing",
            "{id}"
        );
        assert!(
            view.with_active(true).occupies_conversation_execution(),
            "{id}"
        );
    }
}

#[test]
fn pilot_session_retained_records_keep_progress_bytes_order_and_filtering() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
    for id in scenarios(&application) {
        let record = fixtures::record(&application, id);
        let expected = format!(
            r#"[{{"run":"{id}","record":{}}}]"#,
            serde_json::to_string(&record).unwrap()
        );
        assert_eq!(
            serde_json::to_vec(
                &application
                    .pilot_session_runs(&["missing".into(), id.into()])
                    .unwrap()
            )
            .unwrap(),
            expected.as_bytes(),
            "{id}"
        );
    }
    assert!(application.pilot_session_runs(&[]).unwrap().is_empty());
}

#[test]
fn phase_reads_kernel_controls_node_completion_and_call_binding() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
    for id in scenarios(&application) {
        let view = fixtures::view(&application, id);
        match id {
            "executing" => assert_eq!(view.phase(), RunPhase::Executing),
            "paused" | "stopped" | "budget" => assert_eq!(
                view.phase(),
                RunPhase::Waiting {
                    for_what: RunWait::Human,
                    reason: Some(id)
                }
            ),
            "recovery" => assert_eq!(
                view.phase(),
                RunPhase::Waiting {
                    for_what: RunWait::Human,
                    reason: Some("unknown external tool effects")
                }
            ),
            "child" => assert!(matches!(
                view.phase(),
                RunPhase::Waiting {
                    for_what: RunWait::Child,
                    ..
                }
            )),
            "done" => assert_eq!(
                view.phase(),
                RunPhase::Settled {
                    outcome: RunOutcome::Done,
                    reason: None
                }
            ),
            "failed" => assert_eq!(
                view.phase(),
                RunPhase::Settled {
                    outcome: RunOutcome::Failed,
                    reason: Some("worker failed: recorded reason")
                }
            ),
            "aborted" => assert_eq!(
                view.phase(),
                RunPhase::Settled {
                    outcome: RunOutcome::Aborted,
                    reason: Some("recorded reason")
                }
            ),
            _ => unreachable!(),
        }
        if id != "child" {
            assert_eq!(view.progress().snapshot.objective, "test");
        }
        let executed = if id == "done" {
            vec!["worker".to_string()]
        } else {
            Vec::new()
        };
        assert_eq!(view.progress().executed_nodes(), executed);
    }
}

#[test]
fn predicates_follow_public_phase_waiter_and_outcome_distinctions() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
    for id in scenarios(&application) {
        let view = fixtures::view(&application, id);
        let settled = matches!(id, "done" | "failed" | "aborted");
        assert_eq!(view.is_settled(), settled, "{id}");
        assert_eq!(
            view.is_quiescent(),
            !matches!(id, "executing" | "child" | "recovery"),
            "{id}"
        );
        assert_eq!(view.needs_attention(), id != "done", "{id}");
        assert_eq!(view.can_resume(), !settled && id != "child", "{id}");
        assert_eq!(view.can_stop(), id == "executing", "{id}");
        assert_eq!(view.can_abandon(), !settled, "{id}");
        assert!(!view.can_pause(), "{id}");
    }
}

#[test]
fn active_and_session_observations_do_not_reinterpret_durable_state() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
    for id in scenarios(&application) {
        let durable = fixtures::view(&application, id);
        for coordinated in [false, true] {
            let mut observed = durable.clone().with_active(true);
            observed.session_coordinated = coordinated;
            assert!(observed.active());
            assert!(!observed.is_quiescent(), "{id}");
            assert!(!observed.can_resume(), "{id}");
            assert_eq!(observed.can_stop(), id == "executing", "{id}");
            assert_eq!(
                observed.can_pause(),
                id == "executing" && !coordinated,
                "{id}"
            );
            assert_eq!(observed.can_abandon(), !durable.is_settled(), "{id}");
            assert_eq!(observed.phase(), durable.phase());
        }
    }
}

#[test]
fn inactive_controls_are_decided_by_kernel_and_recovery_can_request_inspection() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
    scenarios(&application);
    for id in ["done", "failed", "aborted"] {
        let runs = application.kernel_owner(id).unwrap().unwrap();
        assert!(matches!(
            runs.control(id, "stop", None),
            Err(ApplicationError::Conflict(_))
        ));
        assert!(matches!(
            runs.control(id, "resume", None),
            Err(ApplicationError::Conflict(_))
        ));
    }
    assert!(fixtures::view(&application, "recovery").has_pending_recovery());
    assert!(fixtures::view(&application, "recovery").can_resume());
    assert!(
        application
            .kernel_owner("budget")
            .unwrap()
            .unwrap()
            .control("budget", "resume", None)
            .is_ok()
    );
    assert_eq!(
        fixtures::view(&application, "budget").phase(),
        RunPhase::Executing
    );
}

#[tokio::test]
async fn pilot_read_tools_keep_phase_json_bytes_and_progress_field_order() {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
    fixtures::create(&application, &record("facade-test"));
    let updated =
        serde_json::to_string(&fixtures::view(&application, "facade-test").updated()).unwrap();
    let projection = format!(
        r#"{{"phase":"executing","reason":null,"active":false,"actions":{{"pause":false,"stop":false,"resume":false,"abandon":false}},"updated":{updated}}}"#
    );
    let expected_list =
        format!(r#"{{"runs":[{{"run":"facade-test","projection":{projection}}}]}}"#);
    assert_eq!(
        serde_json::to_vec(&application.pilot_run_list().await.unwrap()).unwrap(),
        expected_list.as_bytes()
    );
    let progress = serde_json::to_string(&fixtures::record(&application, "facade-test")).unwrap();
    let expected_status =
        format!(r#"{{"run":"facade-test","record":{progress},"projection":{projection}}}"#);
    assert_eq!(
        serde_json::to_vec(&application.pilot_run_status("facade-test").await.unwrap()).unwrap(),
        expected_status.as_bytes()
    );
}
