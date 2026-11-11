//! The shared schedule mutation adapter and its HTTP entry points.
use super::*;
use crate::application::scheduling::{
    GraphScheduleReference, ScheduleAssignmentTarget, TaskScheduleRequest, TaskScheduleUpdate,
};
use crate::task_delegation::TaskOriginSource;

/// Both HTTP and Pilot use this adapter over the same Host-owned store. The
/// adapter adds no execution grant and never starts a Run while creating a plan.
pub(in crate::api) struct ScheduleManagementAdapter {
    store: ScheduleStoreHandle,
}

impl ScheduleManagementAdapter {
    pub(in crate::api) fn new(store: ScheduleStoreHandle) -> Self {
        Self { store }
    }
}

impl crate::application::scheduling::ScheduleManagementPort for ScheduleManagementAdapter {
    fn list(&self) -> Result<Value, ApplicationError> {
        let store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        Ok(json!({"schedules": store.items.iter().map(schedule_projection).collect::<Vec<_>>()}))
    }

    fn graph_references(&self) -> Result<Vec<GraphScheduleReference>, ApplicationError> {
        let store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        store
            .items
            .iter()
            .map(|item| {
                Ok(GraphScheduleReference {
                    graph: item.graph.clone(),
                    request_id: occurrence_request_id(&item.id, &item.next_at),
                    once: rule_type(&item.rule).map_err(ApplicationError::Invalid)? == "once",
                    enabled: item.enabled,
                })
            })
            .collect()
    }

    fn check_run_deletion(&self, request_id: &str) -> Result<(), ApplicationError> {
        let store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        if store
            .items
            .iter()
            .any(|item| occurrence_request_id(&item.id, &item.next_at) == request_id)
        {
            return Err(ApplicationError::Conflict(
                "a schedule cursor still references this Run receipt; advance or cancel the plan before deleting the Run".into(),
            ));
        }
        Ok(())
    }

    fn check_task_origin_deletion(
        &self,
        matches: &dyn Fn(&TaskOrigin) -> Result<bool, ApplicationError>,
    ) -> Result<(), ApplicationError> {
        let store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        for origin in store
            .items
            .iter()
            .filter_map(|item| item.delegation.as_ref())
        {
            if matches(origin)? {
                return Err(ApplicationError::Conflict(
                    "a Graph task schedule still references this resource; cancel the plan before deleting it".into(),
                ));
            }
        }
        Ok(())
    }

    fn create(&self, graph: &str, rule: &Value, input: &Value) -> Result<Value, ApplicationError> {
        self.create_with_request(graph, rule, input, "local", None)
    }

    fn replay_creation(
        &self,
        owner: &str,
        key: &str,
        content: &Value,
    ) -> Result<Option<Value>, ApplicationError> {
        let store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        replay_creation(&store, owner, key, content)
    }

    fn create_with_request(
        &self,
        graph: &str,
        rule: &Value,
        input: &Value,
        owner: &str,
        key: Option<&str>,
    ) -> Result<Value, ApplicationError> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        let content = json!({"graph":graph,"rule":rule,"input":input});
        if let Some(key) = key {
            crate::application::pilot_sessions::validate_request_id(key)
                .map_err(|failure| ApplicationError::Invalid(failure.to_string()))?;
            if let Some(retained) = replay_creation(&store, owner, key, &content)? {
                return Ok(retained);
            }
        }
        if graph.trim().is_empty() {
            return Err(ApplicationError::Invalid("graph is required".into()));
        }
        if !input.is_object() {
            return Err(ApplicationError::Invalid("input must be an object".into()));
        }
        let precise_now = Local::now().naive_local();
        let normalized_rule =
            normalize_rule(rule, Some(precise_now)).map_err(ApplicationError::Invalid)?;
        let next_at =
            next_after(&normalized_rule, precise_now).map_err(ApplicationError::Invalid)?;
        let created_at = precise_now
            .with_nanosecond(0)
            .expect("zero nanoseconds is valid");
        let item = ScheduleItem {
            id: unique_schedule_id(),
            graph: graph.into(),
            rule: normalized_rule,
            input: input.clone(),
            created_at: format_local_datetime(created_at),
            next_at: format_local_datetime(next_at),
            enabled: true,
            delegation: None,
            remaining: None,
            group: None,
        };
        let previous = store.items.clone();
        let previous_requests = store.creation_requests.clone();
        store.items.push(item.clone());
        if let Some(key) = key {
            store.creation_requests.push(ScheduleCreationRequest {
                owner: owner.into(),
                key: key.into(),
                content,
                schedule: item.id.clone(),
                group: None,
            });
        }
        if let Err(message) = store.persist() {
            store.items = previous;
            store.creation_requests = previous_requests;
            return Err(ApplicationError::Storage(message));
        }
        Ok(json!({"schedule": schedule_projection(&item)}))
    }

    fn update(
        &self,
        id: &str,
        graph: &str,
        rule: &Value,
        input: &Value,
        enabled: bool,
    ) -> Result<Value, ApplicationError> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        let index = store
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or(ApplicationError::Missing)?;
        let update = TaskScheduleUpdate {
            graph: graph.into(),
            rule: rule.clone(),
            input: input.clone(),
            enabled,
            remaining: store.items[index].remaining,
        };
        update_item(&mut store, index, &update, None)
    }

    fn delete(&self, id: &str) -> Result<Value, ApplicationError> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        ensure_not_dispatching(&store, id)?;
        let previous = store.items.clone();
        store.items.retain(|item| item.id != id);
        if store.items.len() == previous.len() {
            return Err(ApplicationError::Missing);
        }
        if let Err(message) = store.persist() {
            store.items = previous;
            return Err(ApplicationError::Storage(message));
        }
        Ok(json!({"schedule": id, "deleted": true}))
    }

    fn assignment_target(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<ScheduleAssignmentTarget, ApplicationError> {
        let store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        let item = store
            .items
            .iter()
            .find(|item| item.id == id)
            .ok_or(ApplicationError::Missing)?;
        if store.creation_requests.iter().any(|request| {
            request.schedule == id && request.group.is_none() && request.owner != owner
        }) || item
            .delegation
            .as_ref()
            .is_some_and(|origin| origin.owner != owner)
        {
            return Err(ApplicationError::Missing);
        }
        Ok(assignment_target(item))
    }

    fn assign(
        &self,
        id: &str,
        expected: &ScheduleAssignmentTarget,
        origin: &TaskOrigin,
        input: &Value,
    ) -> Result<Value, ApplicationError> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        let index = store
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or(ApplicationError::Missing)?;
        let previous = store.items[index].clone();
        if &assignment_target(&previous) != expected {
            return Err(ApplicationError::Conflict(
                "schedule content changed during assignment; read it again".into(),
            ));
        }
        if !input.is_object()
            || origin.session.is_none()
            || !matches!(&origin.source, TaskOriginSource::OperatorAssignment { schedule, .. } if schedule == id)
        {
            return Err(ApplicationError::Invalid(
                "assignment requires an existing Session and explicit schedule provenance".into(),
            ));
        }
        if previous.delegation.as_ref() == Some(origin) && &previous.input == input {
            return Ok(json!({"schedule":schedule_projection(&previous)}));
        }
        if previous.delegation.is_some() {
            return Err(ApplicationError::Conflict(
                "a schedule's existing task origin cannot be reassigned".into(),
            ));
        }
        ensure_not_dispatching(&store, id)?;
        if parse_local_datetime(&previous.next_at).map_err(ApplicationError::Storage)?
            <= Local::now().naive_local()
        {
            return Err(ApplicationError::Conflict("only a future occurrence can receive a new assignment; its pending cursor is unchanged".into()));
        }
        let mut assigned = previous.clone();
        assigned.input = input.clone();
        assigned.delegation = Some(origin.clone());
        store.items[index] = assigned.clone();
        if let Err(message) = store.persist() {
            store.items[index] = previous;
            return Err(ApplicationError::Storage(message));
        }
        Ok(json!({"schedule":schedule_projection(&assigned)}))
    }

    fn list_tasks(&self, origin: &TaskOrigin) -> Result<Value, ApplicationError> {
        let store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        let now = Local::now().naive_local();
        let schedules = store
            .items
            .iter()
            .filter(|item| belongs_to(item, origin))
            .map(|item| {
                let mut value = schedule_projection(item);
                value["pending"] = json!(
                    item.enabled && parse_local_datetime(&item.next_at).is_ok_and(|at| at <= now)
                );
                value
            })
            .collect::<Vec<_>>();
        Ok(json!({"schedules":schedules}))
    }

    fn replay_task_creation(
        &self,
        origin: &TaskOrigin,
        request: &TaskScheduleRequest,
    ) -> Result<Option<Value>, ApplicationError> {
        let store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        replay_tasks(&store, origin, request)
    }

    fn create_tasks(
        &self,
        origin: &TaskOrigin,
        request: &TaskScheduleRequest,
    ) -> Result<Value, ApplicationError> {
        crate::application::pilot_sessions::validate_request_id(&request.request_id)
            .map_err(|error| ApplicationError::Invalid(error.to_string()))?;
        let mut store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        if let Some(retained) = replay_tasks(&store, origin, request)? {
            return Ok(retained);
        }
        if request.graph.trim().is_empty() || !request.input.is_object() || request.rules.is_empty()
        {
            return Err(ApplicationError::Invalid(
                "graph, one or more timing rules and object input are required".into(),
            ));
        }
        if request.rules.len() > 128 || request.remaining == Some(0) {
            return Err(ApplicationError::Invalid(
                "provide at most 128 occurrences and a positive admission count".into(),
            ));
        }
        let now = Local::now().naive_local();
        let created_at = format_local_datetime(now);
        let owner = task_request_owner(origin);
        let group = task_group_id(&owner, &request.request_id);
        let mut items = Vec::with_capacity(request.rules.len());
        for rule in &request.rules {
            let rule = normalize_rule(rule, Some(now)).map_err(ApplicationError::Invalid)?;
            let once = rule_type(&rule).map_err(ApplicationError::Invalid)? == "once";
            if request.rules.len() > 1 && (!once || request.remaining.is_some()) {
                return Err(ApplicationError::Invalid(
                    "a grouped timing list accepts only once rules without a repeat count".into(),
                ));
            }
            if once && request.remaining.is_some_and(|count| count != 1) {
                return Err(ApplicationError::Invalid(
                    "a once rule can accept only one execution".into(),
                ));
            }
            let next_at =
                format_local_datetime(next_after(&rule, now).map_err(ApplicationError::Invalid)?);
            if items
                .iter()
                .any(|item: &ScheduleItem| item.next_at == next_at)
            {
                return Err(ApplicationError::Invalid(
                    "a timing list cannot contain the same occurrence twice".into(),
                ));
            }
            items.push(ScheduleItem {
                id: unique_schedule_id(),
                graph: request.graph.clone(),
                rule,
                input: request.input.clone(),
                created_at: created_at.clone(),
                next_at,
                enabled: true,
                delegation: Some(origin.clone()),
                remaining: request.remaining,
                group: Some(group.clone()),
            });
        }
        let previous = store.items.clone();
        let previous_requests = store.creation_requests.clone();
        store.creation_requests.push(ScheduleCreationRequest {
            owner,
            key: request.request_id.clone(),
            content: task_request_content(request),
            schedule: items[0].id.clone(),
            group: Some(group.clone()),
        });
        store.items.extend(items.iter().cloned());
        if let Err(message) = store.persist() {
            store.items = previous;
            store.creation_requests = previous_requests;
            return Err(ApplicationError::Storage(message));
        }
        Ok(
            json!({"schedules":items.iter().map(schedule_projection).collect::<Vec<_>>(),"group":group}),
        )
    }

    fn update_task(
        &self,
        origin: &TaskOrigin,
        id: &str,
        update: &TaskScheduleUpdate,
    ) -> Result<Value, ApplicationError> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        let index = store
            .items
            .iter()
            .position(|item| item.id == id && belongs_to(item, origin))
            .ok_or(ApplicationError::Missing)?;
        update_item(&mut store, index, update, Some(origin))
    }

    fn delete_task(&self, origin: &TaskOrigin, id: &str) -> Result<Value, ApplicationError> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        if !store
            .items
            .iter()
            .any(|item| item.id == id && belongs_to(item, origin))
        {
            return Err(ApplicationError::Missing);
        }
        ensure_not_dispatching(&store, id)?;
        let previous = store.items.clone();
        store.items.retain(|item| item.id != id);
        if let Err(message) = store.persist() {
            store.items = previous;
            return Err(ApplicationError::Storage(message));
        }
        Ok(json!({"schedule":id,"deleted":true}))
    }

    fn delete_task_group(
        &self,
        origin: &TaskOrigin,
        group: &str,
    ) -> Result<Value, ApplicationError> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| schedule_store_unavailable())?;
        let ids = store
            .items
            .iter()
            .filter(|item| item.group.as_deref() == Some(group) && belongs_to(item, origin))
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Err(ApplicationError::Missing);
        }
        for id in &ids {
            ensure_not_dispatching(&store, id)?;
        }
        let previous = store.items.clone();
        store.items.retain(|item| !ids.contains(&item.id));
        if let Err(message) = store.persist() {
            store.items = previous;
            return Err(ApplicationError::Storage(message));
        }
        Ok(json!({"group":group,"schedules":ids,"deleted":true}))
    }
}

fn belongs_to(item: &ScheduleItem, origin: &TaskOrigin) -> bool {
    item.delegation
        .as_ref()
        .is_some_and(|saved| saved.same_scope(origin))
}

fn assignment_target(item: &ScheduleItem) -> ScheduleAssignmentTarget {
    ScheduleAssignmentTarget {
        graph: item.graph.clone(),
        input: item.input.clone(),
        delegation: item.delegation.clone(),
    }
}

fn ensure_not_dispatching(store: &ScheduleStore, id: &str) -> Result<(), ApplicationError> {
    if store.in_flight.contains(id) {
        return Err(ApplicationError::Conflict(
            "schedule admission is in progress; inspect it before editing or deleting".into(),
        ));
    }
    Ok(())
}

fn update_item(
    store: &mut ScheduleStore,
    index: usize,
    update: &TaskScheduleUpdate,
    origin: Option<&TaskOrigin>,
) -> Result<Value, ApplicationError> {
    let previous = store.items[index].clone();
    ensure_not_dispatching(store, &previous.id)?;
    if update.graph.trim().is_empty()
        || !update.input.is_object()
        || (update.enabled && update.remaining == Some(0))
    {
        return Err(ApplicationError::Invalid(
            "graph, object input and positive remaining admissions are required".into(),
        ));
    }
    let mut updated = previous.clone();
    updated.graph = update.graph.clone();
    updated.rule = normalize_rule(&update.rule, None).map_err(ApplicationError::Invalid)?;
    updated.input = update.input.clone();
    updated.enabled = update.enabled;
    updated.remaining = update.remaining;
    let content_changed = previous.graph != updated.graph
        || previous.rule != updated.rule
        || previous.input != updated.input;
    if content_changed
        && parse_local_datetime(&previous.next_at).is_ok_and(|at| at <= Local::now().naive_local())
    {
        return Err(ApplicationError::Conflict("a pending occurrence keeps its original content; cancel it and create a new schedule to replace it".into()));
    }
    if update.remaining.is_some_and(|count| count > 1)
        && rule_type(&updated.rule).map_err(ApplicationError::Invalid)? == "once"
    {
        return Err(ApplicationError::Invalid(
            "a once rule can accept only one execution".into(),
        ));
    }
    // An explicit timing edit starts a new future sequence. Scoped pause/resume
    // retains its pending occurrence; operator re-enabling keeps its old contract.
    if update.enabled && (previous.rule != updated.rule || (origin.is_none() && !previous.enabled))
    {
        let now = Local::now().naive_local();
        updated.rule =
            normalize_rule(&updated.rule, Some(now)).map_err(ApplicationError::Invalid)?;
        updated.next_at = format_local_datetime(
            next_after(&updated.rule, now).map_err(ApplicationError::Invalid)?,
        );
    }
    if let Some(origin) = origin.filter(|_| content_changed) {
        updated.delegation = Some(origin.clone());
    }
    store.items[index] = updated.clone();
    if let Err(message) = store.persist() {
        store.items[index] = previous;
        return Err(ApplicationError::Storage(message));
    }
    Ok(json!({"schedule":schedule_projection(&updated)}))
}

fn task_request_owner(origin: &TaskOrigin) -> String {
    format!("graph-task:{}", origin.scope_key())
}

fn task_request_content(request: &TaskScheduleRequest) -> Value {
    json!({"graph":request.graph,"rules":request.rules,"input":request.input,"remaining":request.remaining})
}

fn task_group_id(owner: &str, key: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"anchor-schedule-group-v1\0");
    digest
        .update(serde_json::to_vec(&(owner, key)).expect("task request identity is serializable"));
    format!("schedule-group-{:x}", digest.finalize())
}

fn replay_tasks(
    store: &ScheduleStore,
    origin: &TaskOrigin,
    request: &TaskScheduleRequest,
) -> Result<Option<Value>, ApplicationError> {
    let owner = task_request_owner(origin);
    let Some(saved) = store
        .creation_requests
        .iter()
        .find(|saved| saved.owner == owner && saved.key == request.request_id)
    else {
        return Ok(None);
    };
    if saved.content != task_request_content(request) {
        return Err(ApplicationError::Conflict(
            "operation key was already used for different content".into(),
        ));
    }
    let group = saved.group.as_deref().ok_or_else(|| {
        ApplicationError::Storage("task schedule creation has no group identity".into())
    })?;
    let items = store
        .items
        .iter()
        .filter(|item| item.group.as_deref() == Some(group) && belongs_to(item, origin))
        .collect::<Vec<_>>();
    if items.len() != request.rules.len() || !items.iter().any(|item| item.id == saved.schedule) {
        return Err(ApplicationError::Conflict(
            "a schedule from the created group has been deleted".into(),
        ));
    }
    Ok(Some(
        json!({"schedules":items.into_iter().map(schedule_projection).collect::<Vec<_>>(),"group":group}),
    ))
}

fn replay_creation(
    store: &ScheduleStore,
    owner: &str,
    key: &str,
    content: &Value,
) -> Result<Option<Value>, ApplicationError> {
    let Some(request) = store
        .creation_requests
        .iter()
        .find(|request| request.owner == owner && request.key == key)
    else {
        return Ok(None);
    };
    if &request.content != content {
        return Err(ApplicationError::Conflict(
            "operation key was already used for different content".into(),
        ));
    }
    let item = store
        .items
        .iter()
        .find(|item| item.id == request.schedule)
        .ok_or_else(|| ApplicationError::Conflict("created schedule has been deleted".into()))?;
    Ok(Some(json!({"schedule":schedule_projection(item)})))
}

fn schedule_store_unavailable() -> ApplicationError {
    ApplicationError::Storage("schedule store unavailable".into())
}

fn management_error(failure: ApplicationError, missing: &str) -> HttpResponse {
    match failure {
        ApplicationError::Missing => error(StatusCode::NOT_FOUND, missing),
        ApplicationError::Invalid(message) => error(StatusCode::BAD_REQUEST, message),
        ApplicationError::Conflict(message) => error(StatusCode::CONFLICT, message),
        ApplicationError::Storage(message) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

pub(in crate::api) async fn list_schedules(
    State(state): State<ApiState>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .list_schedules()
        .map(Json)
        .map_err(|failure| management_error(failure, "no such schedule"))
}

pub(in crate::api) async fn create_schedule(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let Some(object) = body.as_object() else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "provide graph, rule, and optional object input",
        ));
    };
    if object
        .keys()
        .any(|key| !["graph", "rule", "input"].contains(&key.as_str()))
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "unsupported schedule fields",
        ));
    }
    let graph = object
        .get("graph")
        .and_then(Value::as_str)
        .filter(|graph| !graph.trim().is_empty())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "graph is required"))?;
    let input = object.get("input").cloned().unwrap_or_else(empty_object);
    if !input.is_object() {
        return Err(error(StatusCode::BAD_REQUEST, "input must be an object"));
    }
    let rule = object
        .get("rule")
        .filter(|rule| rule.is_object())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "rule must be an object"))?;

    let created = state
        .application
        .create_schedule_with_request(
            graph,
            rule,
            &input,
            &private_owner(&state, &headers),
            idempotency_key(&headers)?.as_deref(),
        )
        .await
        .map_err(|failure| management_error(failure, &format!("no such graph: {graph}")))?;
    Ok((StatusCode::CREATED, Json(created)))
}

pub(in crate::api) async fn delete_schedule(
    State(state): State<ApiState>,
    AxumPath(identifier): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .delete_schedule(&identifier)
        .map(Json)
        .map_err(|failure| management_error(failure, "no such schedule"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct ScheduleUpdate {
    graph: String,
    rule: Value,
    #[serde(default = "empty_object")]
    input: Value,
    enabled: bool,
}

pub(in crate::api) async fn update_schedule(
    State(state): State<ApiState>,
    AxumPath(identifier): AxumPath<String>,
    Json(body): Json<ScheduleUpdate>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .update_schedule(
            &identifier,
            &body.graph,
            &body.rule,
            &body.input,
            body.enabled,
        )
        .await
        .map(Json)
        .map_err(|failure| management_error(failure, "no such Graph or schedule"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct ScheduleAssignment {
    session: String,
    instruction: String,
    input: Option<Value>,
}

pub(in crate::api) async fn assign_schedule(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(identifier): AxumPath<String>,
    Json(body): Json<ScheduleAssignment>,
) -> Result<Json<Value>, HttpResponse> {
    state
        .application
        .assign_schedule(
            &private_owner(&state, &headers),
            &identifier,
            &body.session,
            &body.instruction,
            body.input.as_ref(),
        )
        .await
        .map(Json)
        .map_err(|failure| management_error(failure, "no such schedule or channel Session"))
}
