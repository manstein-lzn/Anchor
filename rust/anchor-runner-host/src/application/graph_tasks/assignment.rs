//! Operator transfer of an already-addressed schedule to its real assistant
//! Session. The grant uses the same publication and admission as native tasks.
use super::*;
use crate::application::{channel_management, scheduling::ScheduleAssignmentTarget};
use anchor_platform_session::ChannelConversationKind;

impl RunApplication {
    pub(in crate::application) fn operator_assignment_origin(
        &self,
        owner: &str,
        session: &str,
        schedule: &str,
        instruction: &str,
        target: &ScheduleAssignmentTarget,
        input: &Value,
    ) -> Result<TaskOrigin, ApplicationError> {
        let sessions = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(super::super::assistants::assistant_session_error)?;
        let identity = sessions
            .channel_identity(owner, session)
            .map_err(super::super::assistants::assistant_session_error)?;
        let saved = sessions
            .get(owner, session)
            .map_err(super::super::assistants::assistant_session_error)?;
        if identity.source != channel_management::PLATFORM
            || identity.conversation_kind != ChannelConversationKind::Single
            || identity.sender_id.is_empty()
            || identity.conversation_id.is_empty()
        {
            return Err(invalid(
                "schedule assignment requires an existing private WeCom Session",
            ));
        }
        if target.input.get("bot_graph").and_then(Value::as_str) != Some(saved.graph.as_str())
            || target.input.get("userid").and_then(Value::as_str)
                != Some(identity.sender_id.as_str())
        {
            return Err(ApplicationError::Conflict(
                "schedule recipient does not match the selected assistant Session".into(),
            ));
        }
        for (field, expected) in [
            ("bot_graph", saved.graph.as_str()),
            ("userid", identity.sender_id.as_str()),
        ] {
            if input
                .get(field)
                .is_some_and(|value| value.as_str() != Some(expected))
            {
                return Err(invalid(
                    "assignment input cannot change the schedule's bot or recipient",
                ));
            }
        }
        self.ensure_channel_session_eligible(owner, session)?;
        let current = self
            .channel_management()?
            .snapshot(&saved.graph)?
            .ok_or(ApplicationError::Missing)?;
        let plan = self.channel_bot_plan(&saved.graph)?;
        if current.owner != owner
            || !current.enabled
            || current.binding.plugin != plan.plugin
            || !current.binding.allows_inbound(&identity.sender_id)
            || !current.binding.allows_outbound(&identity.sender_id)
        {
            return Err(invalid(
                "the Session recipient is not authorized by this enabled bot",
            ));
        }
        let binding = sessions
            .get_channel_assistant(owner, session)
            .map_err(super::super::assistants::assistant_session_error)?
            .ok_or_else(|| invalid("the selected Session has no existing assistant instance"))?;
        let metadata = self
            .metadata(&binding.run_id)?
            .ok_or(ApplicationError::Missing)?;
        self.read_run(&binding.run_id)?
            .ok_or(ApplicationError::Missing)?;
        let source = metadata
            .assistant
            .as_ref()
            .ok_or(ApplicationError::Missing)?;
        if source.owner != owner
            || source.session != session
            || metadata.graph != saved.graph
            || source.work_node != plan.work_node
        {
            return Err(invalid(
                "the Session's assistant identity does not match this bot",
            ));
        }
        // A stopped instance is still a valid destination. This check reads its
        // binding only; assignment never admits or restarts the assistant Run.
        self.ensure_current_assistant(&metadata)?;
        let (path, bundle) = self.load_graph(&saved.graph).map_err(invalid)?;
        let reference = declaration(&bundle.snapshot, &plan.work_node)
            .ok_or_else(|| invalid("the assistant work node does not mount graph-tasks"))?;
        let policy = reference
            .graph_tasks
            .clone()
            .ok_or_else(|| invalid("the assistant work node has no Graph task policy"))?;
        if !policy.graphs.contains(&target.graph) {
            return Err(invalid(
                "the schedule Graph is not selected in the assistant's task policy",
            ));
        }
        let digest = bundle.snapshot.digest()?;
        let receipt = assistant_plugins::publication(&self.data_root, &path, &digest)
            .map_err(invalid)?
            .ok_or_else(|| {
                invalid("the assistant's current task permissions need operator approval")
            })?;
        let origin = TaskOrigin {
            owner: owner.into(),
            session: Some(session.into()),
            graph: saved.graph,
            source: TaskOriginSource::OperatorAssignment {
                schedule: schedule.into(),
                node_id: plan.work_node,
                instruction: instruction.into(),
            },
            authorization_digest: digest,
            policy,
            bindings: receipt.bindings,
            targets: receipt.task_graphs,
        };
        self.authorize_delegated_graph(&origin, &target.graph)?;
        Ok(origin)
    }
}
