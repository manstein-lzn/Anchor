use super::*;
use crate::application::scheduling::{
    ScheduleManagementPort, TaskScheduleRequest, TaskScheduleUpdate,
};
use crate::task_delegation::TaskOriginSource;
use anchor_runtime::graph::{GraphTaskPolicy, InvocationKey};

fn origin() -> TaskOrigin {
    TaskOrigin {
        owner: "owner".into(),
        session: Some("session".into()),
        graph: "assistant".into(),
        source: TaskOriginSource::AgentDelegation {
            key: InvocationKey {
                run_id: "resident".into(),
                graph_digest: "source-digest".into(),
                node_id: "agent".into(),
                invocation: 1,
            },
        },
        authorization_digest: "source-digest".into(),
        policy: GraphTaskPolicy {
            graphs: vec!["task".into()],
            ..Default::default()
        },
        bindings: Default::default(),
        targets: Default::default(),
    }
}

fn agent_key_mut(origin: &mut TaskOrigin) -> &mut InvocationKey {
    match &mut origin.source {
        TaskOriginSource::AgentDelegation { key } => key,
        _ => panic!("fixture has an Agent delegation"),
    }
}

fn grouped_request() -> TaskScheduleRequest {
    let now = Local::now().naive_local();
    TaskScheduleRequest {
        graph: "task".into(),
        rules: [1, 2].map(|hours| json!({"type":"once","at":format_local_datetime(now + Duration::hours(hours))})).into(),
        input: json!({"topic":"briefing"}),
        remaining: None,
        request_id: "two-occurrences".into(),
    }
}

#[test]
fn task_schedule_groups_are_durable_idempotent_and_scoped_to_the_source_agent() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("schedules.json");
    let store = ScheduleStore::open(path.clone()).unwrap();
    let adapter = ScheduleManagementAdapter::new(store.clone());
    let origin = origin();
    let request = grouped_request();
    let created = adapter.create_tasks(&origin, &request).unwrap();
    assert_eq!(created["schedules"].as_array().unwrap().len(), 2);
    assert!(created["schedules"][0].get("delegation").is_none());
    let id = created["schedules"][0]["id"].as_str().unwrap();
    let group = created["group"].as_str().unwrap();
    let mut next_turn = origin.clone();
    agent_key_mut(&mut next_turn).invocation += 1;
    agent_key_mut(&mut next_turn).run_id = "replacement-resident".into();
    assert_eq!(adapter.create_tasks(&next_turn, &request).unwrap(), created);
    let update = TaskScheduleUpdate {
        graph: "task".into(),
        rule: request.rules[0].clone(),
        input: request.input.clone(),
        enabled: false,
        remaining: None,
    };
    let mut foreign_scopes: [TaskOrigin; 5] = std::array::from_fn(|_| origin.clone());
    foreign_scopes[0].owner = "other-owner".into();
    foreign_scopes[1].session = Some("other-session".into());
    foreign_scopes[2].graph = "other-assistant".into();
    agent_key_mut(&mut foreign_scopes[3]).node_id = "other-agent".into();
    foreign_scopes[4].session = None;
    for foreign in foreign_scopes {
        assert_eq!(
            adapter.list_tasks(&foreign).unwrap(),
            json!({"schedules":[]})
        );
        assert!(matches!(
            adapter.update_task(&foreign, id, &update),
            Err(ApplicationError::Missing)
        ));
        assert!(matches!(
            adapter.delete_task(&foreign, id),
            Err(ApplicationError::Missing)
        ));
        assert!(matches!(
            adapter.delete_task_group(&foreign, group),
            Err(ApplicationError::Missing)
        ));
    }
    let mut other_session = origin.clone();
    other_session.session = Some("other-session".into());
    let other = adapter.create_tasks(&other_session, &request).unwrap();
    assert_ne!(other["group"], created["group"]);
    adapter
        .create("task", &json!({"type":"daily","time":"09:00"}), &json!({}))
        .unwrap();
    assert_eq!(
        adapter.list_tasks(&origin).unwrap()["schedules"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    drop(adapter);
    drop(store);
    let store = ScheduleStore::open(path).unwrap();
    let adapter = ScheduleManagementAdapter::new(store.clone());
    assert_eq!(adapter.create_tasks(&next_turn, &request).unwrap(), created);
    adapter.delete_task(&next_turn, id).unwrap();
    assert!(matches!(
        adapter.create_tasks(&origin, &request),
        Err(ApplicationError::Conflict(_))
    ));
    adapter.delete_task_group(&next_turn, group).unwrap();
    assert_eq!(
        adapter.list_tasks(&origin).unwrap(),
        json!({"schedules":[]})
    );
    assert_eq!(
        adapter.list_tasks(&other_session).unwrap()["schedules"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(matches!(
        adapter.create_tasks(&origin, &request),
        Err(ApplicationError::Conflict(_))
    ));
    let mut unbound = origin;
    unbound.session = None;
    adapter.create_tasks(&unbound, &request).unwrap();
    agent_key_mut(&mut unbound).run_id = "another-run".into();
    assert_eq!(
        adapter.list_tasks(&unbound).unwrap(),
        json!({"schedules":[]})
    );
}

#[test]
fn task_schedule_batch_validation_and_storage_errors_leave_no_partial_group() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("schedules.json");
    let store = ScheduleStore::open(path.clone()).unwrap();
    let adapter = ScheduleManagementAdapter::new(store.clone());
    let origin = origin();
    let mut request = grouped_request();
    let valid_rules = request.rules.clone();
    for rules in [
        vec![
            valid_rules[0].clone(),
            json!({"type":"weekly","weekdays":[]}),
        ],
        vec![valid_rules[0].clone(), valid_rules[0].clone()],
        vec![
            json!({"type":"interval","seconds":60}),
            valid_rules[0].clone(),
        ],
    ] {
        request.rules = rules;
        assert!(matches!(
            adapter.create_tasks(&origin, &request),
            Err(ApplicationError::Invalid(_))
        ));
        assert_eq!(adapter.list().unwrap(), json!({"schedules":[]}));
    }
    for (rules, remaining) in [
        (valid_rules.clone(), Some(0)),
        (vec![valid_rules[0].clone()], Some(2)),
        (valid_rules.clone(), Some(1)),
    ] {
        request.rules = rules;
        request.remaining = remaining;
        assert!(matches!(
            adapter.create_tasks(&origin, &request),
            Err(ApplicationError::Invalid(_))
        ));
        assert_eq!(adapter.list().unwrap(), json!({"schedules":[]}));
    }
    request.rules = valid_rules;
    request.remaining = None;
    let blocker = root.path().join("not-a-directory");
    std::fs::write(&blocker, "file").unwrap();
    store.lock().unwrap().path = blocker.join("schedules.json");
    assert!(matches!(
        adapter.create_tasks(&origin, &request),
        Err(ApplicationError::Storage(_))
    ));
    assert_eq!(adapter.list().unwrap(), json!({"schedules":[]}));
    store.lock().unwrap().path = path.clone();
    assert!(
        adapter
            .replay_task_creation(&origin, &request)
            .unwrap()
            .is_none()
    );
    let created = adapter.create_tasks(&origin, &request).unwrap();
    let before = adapter.list_tasks(&origin).unwrap();
    store.lock().unwrap().path = blocker.join("schedules.json");
    assert!(matches!(
        adapter.delete_task_group(&origin, created["group"].as_str().unwrap()),
        Err(ApplicationError::Storage(_))
    ));
    assert_eq!(adapter.list_tasks(&origin).unwrap(), before);
    store.lock().unwrap().path = path;
    let mut changed = request;
    changed.input = json!({"topic":"changed"});
    assert!(matches!(
        adapter.create_tasks(&origin, &changed),
        Err(ApplicationError::Conflict(_))
    ));
}

#[test]
fn scoped_pause_resume_keeps_pending_occurrences_and_pending_content_cannot_be_rewritten() {
    let root = tempfile::tempdir().unwrap();
    let store = ScheduleStore::open(root.path().join("schedules.json")).unwrap();
    let adapter = ScheduleManagementAdapter::new(store.clone());
    let origin = origin();
    let request = TaskScheduleRequest {
        graph: "task".into(),
        rules: vec![json!({"type":"interval","seconds":60})],
        input: json!({"original":true}),
        remaining: Some(3),
        request_id: "repeat".into(),
    };
    let created = adapter.create_tasks(&origin, &request).unwrap();
    let id = created["schedules"][0]["id"].as_str().unwrap();
    let due = format_local_datetime(Local::now().naive_local() - Duration::days(1));
    {
        let mut store = store.lock().unwrap();
        store.items[0].next_at = due.clone();
        store.persist().unwrap();
    }
    assert_eq!(
        adapter.list_tasks(&origin).unwrap()["schedules"][0]["pending"],
        true
    );
    let mut update = TaskScheduleUpdate {
        graph: request.graph.clone(),
        rule: request.rules[0].clone(),
        input: request.input.clone(),
        enabled: false,
        remaining: request.remaining,
    };
    adapter.update_task(&origin, id, &update).unwrap();
    let paused = adapter.list_tasks(&origin).unwrap();
    assert_eq!(paused["schedules"][0]["pending"], false);
    update.input = json!({"rewritten":true});
    assert!(matches!(
        adapter.update_task(&origin, id, &update),
        Err(ApplicationError::Conflict(_))
    ));
    assert_eq!(adapter.list_tasks(&origin).unwrap(), paused);
    update.input = request.input;
    update.enabled = true;
    let mut later_origin = origin;
    agent_key_mut(&mut later_origin).invocation += 1;
    later_origin.policy.graphs.clear();
    let resumed = adapter.update_task(&later_origin, id, &update).unwrap();
    assert_eq!(resumed["schedule"]["next_at"], due);
    assert_eq!(resumed["schedule"]["remaining"], 3);
}

#[test]
fn schedule_cursor_protects_run_receipts_until_advanced_or_cancelled_even_while_paused() {
    let root = tempfile::tempdir().unwrap();
    let store = ScheduleStore::open(root.path().join("schedules.json")).unwrap();
    let adapter = ScheduleManagementAdapter::new(store.clone());
    let created = adapter
        .create("task", &json!({"type":"interval","seconds":60}), &json!({}))
        .unwrap();
    let id = created["schedule"]["id"].as_str().unwrap();
    let at = created["schedule"]["next_at"].as_str().unwrap();
    let request_id = occurrence_request_id(id, at);
    let references = adapter.graph_references().unwrap();
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].graph, "task");
    assert_eq!(references[0].request_id, request_id);
    assert!(!references[0].once);
    assert!(references[0].enabled);
    assert!(matches!(
        adapter.check_run_deletion(&request_id),
        Err(ApplicationError::Conflict(_))
    ));
    {
        let mut store = store.lock().unwrap();
        store.items[0].enabled = false;
        store.persist().unwrap();
    }
    assert!(!adapter.graph_references().unwrap()[0].enabled);
    assert!(matches!(
        adapter.check_run_deletion(&request_id),
        Err(ApplicationError::Conflict(_))
    ));
    let next = format_local_datetime(parse_local_datetime(at).unwrap() + Duration::seconds(60));
    {
        let mut store = store.lock().unwrap();
        store.items[0].next_at = next.clone();
        store.persist().unwrap();
    }
    adapter.check_run_deletion(&request_id).unwrap();
    let next_request = occurrence_request_id(id, &next);
    assert!(matches!(
        adapter.check_run_deletion(&next_request),
        Err(ApplicationError::Conflict(_))
    ));
    adapter.delete(id).unwrap();
    adapter.check_run_deletion(&next_request).unwrap();
    assert!(adapter.graph_references().unwrap().is_empty());
    let once = adapter.create("task", &json!({"type":"once", "at":format_local_datetime(Local::now().naive_local() + Duration::hours(1))}), &json!({})).unwrap();
    let references = adapter.graph_references().unwrap();
    assert!(references[0].once);
    assert_eq!(
        references[0].request_id,
        occurrence_request_id(
            once["schedule"]["id"].as_str().unwrap(),
            once["schedule"]["next_at"].as_str().unwrap()
        )
    );
}
