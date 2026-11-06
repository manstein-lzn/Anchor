//! Schedule management owns durable timing facts through a Host adapter. Graph
//! execution and permissions stay in the existing admission/ticker path.
use super::{ApplicationError, RunApplication};
use crate::task_delegation::{TaskOrigin, TaskOriginSource};
use serde_json::Value;

pub(crate) struct TaskScheduleRequest {
    pub(crate) graph: String,
    pub(crate) rules: Vec<Value>,
    pub(crate) input: Value,
    pub(crate) remaining: Option<u32>,
    pub(crate) request_id: String,
}

pub(crate) struct TaskScheduleUpdate {
    pub(crate) graph: String,
    pub(crate) rule: Value,
    pub(crate) input: Value,
    pub(crate) enabled: bool,
    pub(crate) remaining: Option<u32>,
}

#[derive(Clone, PartialEq)]
pub(crate) struct ScheduleAssignmentTarget {
    pub(crate) graph: String,
    pub(crate) input: Value,
    pub(crate) delegation: Option<TaskOrigin>,
}

pub(crate) trait ScheduleManagementPort: Send + Sync {
    fn list(&self) -> Result<Value, ApplicationError>;
    fn check_run_deletion(&self, _request_id: &str) -> Result<(), ApplicationError> {
        Ok(())
    }
    fn check_task_origin_deletion(
        &self,
        _matches: &dyn Fn(&TaskOrigin) -> Result<bool, ApplicationError>,
    ) -> Result<(), ApplicationError> {
        Ok(())
    }
    fn create(&self, graph: &str, rule: &Value, input: &Value) -> Result<Value, ApplicationError>;
    fn replay_creation(
        &self,
        _owner: &str,
        _key: &str,
        _content: &Value,
    ) -> Result<Option<Value>, ApplicationError> {
        Err(ApplicationError::Invalid(
            "keyed schedule creation is not supported by this Host".into(),
        ))
    }
    fn create_with_request(
        &self,
        graph: &str,
        rule: &Value,
        input: &Value,
        _owner: &str,
        key: Option<&str>,
    ) -> Result<Value, ApplicationError> {
        if key.is_some() {
            return Err(ApplicationError::Invalid(
                "keyed schedule creation is not supported by this Host".into(),
            ));
        }
        self.create(graph, rule, input)
    }
    fn update(
        &self,
        id: &str,
        graph: &str,
        rule: &Value,
        input: &Value,
        enabled: bool,
    ) -> Result<Value, ApplicationError>;
    fn delete(&self, id: &str) -> Result<Value, ApplicationError>;
    fn assignment_target(
        &self,
        _owner: &str,
        _id: &str,
    ) -> Result<ScheduleAssignmentTarget, ApplicationError> {
        Err(scoped_scheduling_unavailable())
    }
    fn assign(
        &self,
        _id: &str,
        _expected: &ScheduleAssignmentTarget,
        _origin: &TaskOrigin,
        _input: &Value,
    ) -> Result<Value, ApplicationError> {
        Err(scoped_scheduling_unavailable())
    }
    fn list_tasks(&self, _origin: &TaskOrigin) -> Result<Value, ApplicationError> {
        Err(scoped_scheduling_unavailable())
    }
    fn replay_task_creation(
        &self,
        _origin: &TaskOrigin,
        _request: &TaskScheduleRequest,
    ) -> Result<Option<Value>, ApplicationError> {
        Err(scoped_scheduling_unavailable())
    }
    fn create_tasks(
        &self,
        _origin: &TaskOrigin,
        _request: &TaskScheduleRequest,
    ) -> Result<Value, ApplicationError> {
        Err(scoped_scheduling_unavailable())
    }
    fn update_task(
        &self,
        _origin: &TaskOrigin,
        _id: &str,
        _update: &TaskScheduleUpdate,
    ) -> Result<Value, ApplicationError> {
        Err(scoped_scheduling_unavailable())
    }
    fn delete_task(&self, _origin: &TaskOrigin, _id: &str) -> Result<Value, ApplicationError> {
        Err(scoped_scheduling_unavailable())
    }
    fn delete_task_group(
        &self,
        _origin: &TaskOrigin,
        _group: &str,
    ) -> Result<Value, ApplicationError> {
        Err(scoped_scheduling_unavailable())
    }
}

fn scoped_scheduling_unavailable() -> ApplicationError {
    ApplicationError::Invalid("this Host does not provide scoped schedule management".into())
}

impl RunApplication {
    pub(crate) fn check_scheduled_run_deletion(
        &self,
        request_id: &str,
    ) -> Result<(), ApplicationError> {
        match self.schedules.as_deref() {
            Some(port) => port.check_run_deletion(request_id),
            None => Ok(()),
        }
    }

    fn schedule_management(&self) -> Result<&dyn ScheduleManagementPort, ApplicationError> {
        self.schedules.as_deref().ok_or_else(|| {
            ApplicationError::Invalid("this Host does not provide schedule management".into())
        })
    }

    pub(crate) fn list_schedules(&self) -> Result<Value, ApplicationError> {
        self.schedule_management()?.list()
    }

    pub(crate) async fn assign_schedule(
        &self,
        owner: &str,
        id: &str,
        session: &str,
        instruction: &str,
        input: Option<&Value>,
    ) -> Result<Value, ApplicationError> {
        if instruction.trim().is_empty() || instruction.len() > 8192 {
            return Err(ApplicationError::Invalid(
                "assignment instruction must contain text within 8192 bytes".into(),
            ));
        }
        if input.is_some_and(|input| !input.is_object()) {
            return Err(ApplicationError::Invalid("input must be an object".into()));
        }
        let port = self.schedule_management()?;
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        let target = port.assignment_target(owner, id)?;
        let input = input.unwrap_or(&target.input);
        if let Some(origin) = &target.delegation {
            if origin.owner == owner
                && origin.session.as_deref() == Some(session)
                && matches!(&origin.source, TaskOriginSource::OperatorAssignment { schedule, instruction: saved, .. } if schedule == id && saved == instruction)
                && input == &target.input
            {
                // A retry reads the existing assignment; it cannot refresh its
                // grant or alter an occurrence that has since become due.
                return port.assign(id, &target, origin, input);
            }
            return Err(ApplicationError::Conflict(
                "this schedule already has a task origin; cancel it before assigning another"
                    .into(),
            ));
        }
        let _library_lease = self.plugin_reference_lease().await?;
        let origin =
            self.operator_assignment_origin(owner, session, id, instruction, &target, input)?;
        port.assign(id, &target, &origin, input)
    }

    pub(crate) fn list_task_schedules(
        &self,
        origin: &TaskOrigin,
    ) -> Result<Value, ApplicationError> {
        self.schedule_management()?.list_tasks(origin)
    }

    pub(crate) async fn create_task_schedules(
        &self,
        origin: &TaskOrigin,
        request: &TaskScheduleRequest,
    ) -> Result<Value, ApplicationError> {
        super::pilot_sessions::validate_request_id(&request.request_id)
            .map_err(|failure| ApplicationError::Invalid(failure.to_string()))?;
        let port = self.schedule_management()?;
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        if let Some(retained) = port.replay_task_creation(origin, request)? {
            return Ok(retained);
        }
        self.authorize_delegated_graph(origin, &request.graph)?;
        self.load_graph(&request.graph)
            .map_err(|_| ApplicationError::Missing)?;
        port.create_tasks(origin, request)
    }

    pub(crate) async fn update_task_schedule(
        &self,
        origin: &TaskOrigin,
        id: &str,
        update: &TaskScheduleUpdate,
    ) -> Result<Value, ApplicationError> {
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        if update.enabled {
            self.authorize_delegated_graph(origin, &update.graph)?;
            self.load_graph(&update.graph)
                .map_err(|_| ApplicationError::Missing)?;
        }
        self.schedule_management()?.update_task(origin, id, update)
    }

    pub(crate) fn delete_task_schedule(
        &self,
        origin: &TaskOrigin,
        id: &str,
    ) -> Result<Value, ApplicationError> {
        self.schedule_management()?.delete_task(origin, id)
    }

    pub(crate) fn delete_task_schedule_group(
        &self,
        origin: &TaskOrigin,
        group: &str,
    ) -> Result<Value, ApplicationError> {
        self.schedule_management()?.delete_task_group(origin, group)
    }

    #[cfg(test)]
    pub(crate) async fn create_schedule(
        &self,
        graph: &str,
        rule: &Value,
        input: &Value,
    ) -> Result<Value, ApplicationError> {
        self.create_schedule_with_request(graph, rule, input, "local", None)
            .await
    }

    pub(crate) async fn create_schedule_with_request(
        &self,
        graph: &str,
        rule: &Value,
        input: &Value,
        owner: &str,
        key: Option<&str>,
    ) -> Result<Value, ApplicationError> {
        let port = self.schedule_management()?;
        if graph.trim().is_empty() {
            return Err(ApplicationError::Invalid("graph is required".into()));
        }
        if !rule.is_object() || !input.is_object() {
            return Err(ApplicationError::Invalid(
                "rule and input must be objects".into(),
            ));
        }
        // Graph deletion/publication and scheduling share the same guard; a
        // schedule cannot be created against a graph removed during validation.
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        if let Some(key) = key {
            super::pilot_sessions::validate_request_id(key)
                .map_err(|failure| ApplicationError::Invalid(failure.to_string()))?;
            if let Some(retained) = port.replay_creation(
                owner,
                key,
                &serde_json::json!({"graph":graph,"rule":rule,"input":input}),
            )? {
                return Ok(retained);
            }
        }
        self.load_graph(graph)
            .map_err(|_| ApplicationError::Missing)?;
        port.create_with_request(graph, rule, input, owner, key)
    }

    pub(crate) fn delete_schedule(&self, id: &str) -> Result<Value, ApplicationError> {
        self.schedule_management()?.delete(id)
    }

    pub(crate) async fn update_schedule(
        &self,
        id: &str,
        graph: &str,
        rule: &Value,
        input: &Value,
        enabled: bool,
    ) -> Result<Value, ApplicationError> {
        let port = self.schedule_management()?;
        if graph.trim().is_empty() || !rule.is_object() || !input.is_object() {
            return Err(ApplicationError::Invalid(
                "graph, object rule and object input are required".into(),
            ));
        }
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        self.load_graph(graph)
            .map_err(|_| ApplicationError::Missing)?;
        port.update(id, graph, rule, input, enabled)
    }
}
