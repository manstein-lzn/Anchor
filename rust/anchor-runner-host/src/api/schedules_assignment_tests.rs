use super::*;
use crate::{application::scheduling::ScheduleManagementPort, task_delegation::TaskOriginSource};
use anchor_runtime::graph::GraphTaskPolicy;

#[test]
fn assignment_body_requires_instruction_and_does_not_accept_model_supplied_authority() {
    for body in [
        json!({"session":"session"}),
        json!({"session":"session","instruction":"Deliver here","graph":"another-graph"}),
        json!({"session":"session","instruction":"Deliver here","owner":"another-owner"}),
        json!({"session":"session","instruction":"Deliver here","key":{"run_id":"fake"}}),
    ] {
        assert!(serde_json::from_value::<ScheduleAssignment>(body).is_err());
    }
}

#[tokio::test]
async fn operator_schedule_assignment_is_atomic_scoped_and_idempotent_through_the_application() {
    let (root, application, _channels, _sessions, session, target) =
        crate::application::graph_tasks::assignment_tests::prepared().await;
    let store = ScheduleStore::open(root.path().join("schedules.json")).unwrap();
    let application = application
        .with_schedule_management(Arc::new(ScheduleManagementAdapter::new(store.clone())));
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
    for invalid in ["".to_string(), " ".into(), "x".repeat(8193)] {
        assert!(matches!(
            application
                .assign_schedule("local", id, &session, &invalid, None)
                .await,
            Err(ApplicationError::Invalid(_))
        ));
    }
    let updated_input = json!({"send_not_before":"09:05"});
    let assigned = application
        .assign_schedule(
            "local",
            id,
            &session,
            "Deliver the result here",
            Some(&updated_input),
        )
        .await
        .unwrap();
    assert_eq!(
        assigned["schedule"]["next_at"],
        created["schedule"]["next_at"]
    );
    assert_eq!(
        assigned["schedule"]["created_at"],
        created["schedule"]["created_at"]
    );
    assert_eq!(assigned["schedule"]["rule"], created["schedule"]["rule"]);
    assert_eq!(assigned["schedule"]["graph"], "producer");
    assert_eq!(
        assigned["schedule"]["assignment"]["instruction"],
        "Deliver the result here"
    );
    assert_eq!(
        application
            .assign_schedule(
                "local",
                id,
                &session,
                "Deliver the result here",
                Some(&updated_input)
            )
            .await
            .unwrap(),
        assigned
    );
    assert!(matches!(
        application
            .assign_schedule(
                "local",
                id,
                &session,
                "A different instruction",
                Some(&updated_input)
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    assert!(matches!(
        application
            .assign_schedule(
                "local",
                id,
                "another-session",
                "Deliver the result here",
                Some(&updated_input)
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    assert!(matches!(
        application
            .assign_schedule(
                "local",
                id,
                &session,
                "Deliver the result here",
                Some(&json!({}))
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    assert!(matches!(
        application
            .assign_schedule(
                "another-owner",
                id,
                &session,
                "Deliver the result here",
                Some(&updated_input)
            )
            .await,
        Err(ApplicationError::Missing)
    ));
    assert_eq!(
        application.records().unwrap().len(),
        1,
        "assignment must not admit a task or restart the stopped assistant"
    );
    assert!(application.active_runs(None).await.is_empty());
}

fn assignment(id: &str) -> TaskOrigin {
    TaskOrigin {
        owner: "local".into(),
        session: Some("existing-session".into()),
        graph: "assistant".into(),
        source: TaskOriginSource::OperatorAssignment {
            schedule: id.into(),
            node_id: "work".into(),
            instruction: "Deliver results here".into(),
        },
        authorization_digest: "approved-source".into(),
        policy: GraphTaskPolicy::default(),
        bindings: Default::default(),
        targets: Default::default(),
    }
}

#[test]
fn operator_assignment_preserves_timing_and_retries_without_replaying_an_occurrence() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("schedules.json");
    let store = ScheduleStore::open(path.clone()).unwrap();
    let adapter = ScheduleManagementAdapter::new(store.clone());
    let created = adapter
        .create_with_request(
            "producer",
            &json!({"type":"daily","time":"07:30"}),
            &json!({"bot_graph":"assistant","userid":"recipient"}),
            "local",
            Some("original-plan"),
        )
        .unwrap();
    let id = created["schedule"]["id"].as_str().unwrap();
    {
        let mut locked = store.lock().unwrap();
        locked.items[0].enabled = false;
        locked.items[0].remaining = Some(3);
        locked.items[0].group = Some("retained-group".into());
        locked.persist().unwrap();
    }
    let before = store.lock().unwrap().items[0].clone();
    let expected = adapter.assignment_target("local", id).unwrap();
    let origin = assignment(id);
    let result = adapter
        .assign(id, &expected, &origin, &json!({"producer":true}))
        .unwrap();
    let mut after = store.lock().unwrap().items[0].clone();
    assert_eq!(after.input, json!({"producer":true}));
    assert_eq!(after.delegation.as_ref(), Some(&origin));
    assert_eq!(
        result["schedule"]["assignment"]["session"],
        "existing-session"
    );
    assert!(result["schedule"].get("delegation").is_none());
    after.input = before.input.clone();
    after.delegation = None;
    assert_eq!(after, before);
    let mut next_turn = origin.clone();
    next_turn.source = TaskOriginSource::AgentDelegation {
        key: anchor_runtime::graph::InvocationKey {
            run_id: "real-assistant-run".into(),
            graph_digest: "approved-source".into(),
            node_id: "work".into(),
            invocation: 1,
        },
    };
    assert_eq!(
        adapter.list_tasks(&next_turn).unwrap()["schedules"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    drop(adapter);
    drop(store);
    let store = ScheduleStore::open(path).unwrap();
    let adapter = ScheduleManagementAdapter::new(store.clone());
    assert_eq!(
        adapter
            .assign(
                id,
                &adapter.assignment_target("local", id).unwrap(),
                &origin,
                &json!({"producer":true})
            )
            .unwrap(),
        result
    );
    let due = format_local_datetime(Local::now().naive_local() - Duration::days(1));
    {
        let mut locked = store.lock().unwrap();
        locked.items[0].next_at = due.clone();
        locked.in_flight.insert(id.into());
    }
    let result = adapter
        .assign(
            id,
            &adapter.assignment_target("local", id).unwrap(),
            &origin,
            &json!({"producer":true}),
        )
        .unwrap();
    assert_eq!(result["schedule"]["next_at"], due);
    let mut foreign = origin.clone();
    foreign.session = Some("another-session".into());
    assert!(matches!(
        adapter.assign(
            id,
            &adapter.assignment_target("local", id).unwrap(),
            &foreign,
            &json!({"producer":true})
        ),
        Err(ApplicationError::Conflict(_))
    ));
}

#[test]
fn assignment_rejects_foreign_pending_busy_or_changed_plans_and_rolls_back_storage_failures() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("schedules.json");
    let store = ScheduleStore::open(path.clone()).unwrap();
    let adapter = ScheduleManagementAdapter::new(store.clone());
    let created = adapter
        .create_with_request(
            "producer",
            &json!({"type":"interval","seconds":60}),
            &json!({}),
            "local",
            Some("owned-plan"),
        )
        .unwrap();
    let id = created["schedule"]["id"].as_str().unwrap();
    assert!(matches!(
        adapter.assignment_target("another-owner", id),
        Err(ApplicationError::Missing)
    ));
    let expected = adapter.assignment_target("local", id).unwrap();
    let before = store.lock().unwrap().items[0].clone();
    let origin = assignment(id);
    store.lock().unwrap().in_flight.insert(id.into());
    assert!(matches!(
        adapter.assign(id, &expected, &origin, &json!({})),
        Err(ApplicationError::Conflict(_))
    ));
    {
        let mut locked = store.lock().unwrap();
        locked.in_flight.clear();
        locked.items[0].next_at =
            format_local_datetime(Local::now().naive_local() - Duration::days(1));
    }
    assert!(matches!(
        adapter.assign(id, &expected, &origin, &json!({})),
        Err(ApplicationError::Conflict(_))
    ));
    {
        let mut locked = store.lock().unwrap();
        locked.items[0] = before.clone();
        locked.items[0].input = json!({"changed":true});
    }
    assert!(matches!(
        adapter.assign(id, &expected, &origin, &json!({})),
        Err(ApplicationError::Conflict(_))
    ));
    let blocker = root.path().join("not-directory");
    std::fs::write(&blocker, "file").unwrap();
    {
        let mut locked = store.lock().unwrap();
        locked.items[0] = before.clone();
        locked.path = blocker.join("schedules.json");
    }
    assert!(matches!(
        adapter.assign(id, &expected, &origin, &json!({})),
        Err(ApplicationError::Storage(_))
    ));
    assert_eq!(store.lock().unwrap().items[0], before);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap()["schedules"][0]
            .get("delegation"),
        None
    );
}
