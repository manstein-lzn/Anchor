//! Operator task origins use the existing private channel Session and approved
//! assistant publication; consumption never grants Graph execution authority.
use super::*;
use crate::{
    application::{channel_management, scheduling::ScheduleAssignmentTarget},
    channel_platform::ChannelPlatform,
};
use anchor_platform_session::{ChannelConversationKind, ChannelIdentity};

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
        let (origin, identity) = self.operator_channel_origin(owner, session, |node_id| {
            TaskOriginSource::OperatorAssignment {
                schedule: schedule.into(),
                node_id,
                instruction: instruction.into(),
            }
        })?;
        if identity.source != ChannelPlatform::Wecom.as_str() {
            return Err(invalid(
                "schedule assignment requires an existing private WeCom Session",
            ));
        }
        if target.input.get("bot_graph").and_then(Value::as_str) != Some(origin.graph.as_str())
            || target.input.get("userid").and_then(Value::as_str)
                != Some(identity.sender_id.as_str())
        {
            return Err(ApplicationError::Conflict(
                "schedule recipient does not match the selected assistant Session".into(),
            ));
        }
        for (field, expected) in [
            ("bot_graph", origin.graph.as_str()),
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
        if !origin.policy.graphs.contains(&target.graph) {
            return Err(invalid(
                "the schedule Graph is not selected in the assistant's task policy",
            ));
        }
        self.authorize_delegated_graph(&origin, &target.graph)?;
        Ok(origin)
    }

    pub(in crate::application) fn operator_subscription_origin(
        &self,
        owner: &str,
        session: &str,
        schedule: &str,
        instruction: &str,
    ) -> Result<TaskOrigin, ApplicationError> {
        let (origin, _) = self.operator_channel_origin(owner, session, |node_id| {
            TaskOriginSource::OperatorSubscription {
                schedule: schedule.into(),
                node_id,
                instruction: instruction.into(),
            }
        })?;
        if !origin.policy.schedules.iter().any(|id| id == schedule) {
            return Err(invalid(
                "the schedule is not selected in the assistant's subscription policy",
            ));
        }
        Ok(origin)
    }

    fn operator_channel_origin(
        &self,
        owner: &str,
        session: &str,
        make_source: impl FnOnce(String) -> TaskOriginSource,
    ) -> Result<(TaskOrigin, ChannelIdentity), ApplicationError> {
        let (saved, identity, plan) = self.task_channel_identity(owner, session)?;
        let sessions = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(super::super::assistants::assistant_session_error)?;
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
        // binding only; origin creation never admits or restarts the assistant Run.
        self.ensure_current_assistant(&metadata)?;
        let (path, bundle) = self.load_graph(&saved.graph).map_err(invalid)?;
        let reference = declaration(&bundle.snapshot, &plan.work_node)
            .ok_or_else(|| invalid("the assistant work node does not mount graph-tasks"))?;
        let policy = reference
            .graph_tasks
            .clone()
            .ok_or_else(|| invalid("the assistant work node has no Graph task policy"))?;
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
            source: make_source(plan.work_node),
            authorization_digest: digest,
            policy,
            bindings: receipt.bindings,
            targets: receipt.task_graphs,
        };
        self.current_task_grant(&origin)?;
        Ok((origin, identity))
    }
    pub(in crate::application) fn task_channel_identity(
        &self,
        owner: &str,
        session: &str,
    ) -> Result<
        (
            anchor_platform_session::Session,
            ChannelIdentity,
            channel_management::ChannelPlan,
        ),
        ApplicationError,
    > {
        let sessions = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(super::super::assistants::assistant_session_error)?;
        let identity = sessions
            .channel_identity(owner, session)
            .map_err(super::super::assistants::assistant_session_error)?;
        let saved = sessions
            .get(owner, session)
            .map_err(super::super::assistants::assistant_session_error)?;
        if ChannelPlatform::parse(&identity.source).is_none()
            || identity.conversation_kind != ChannelConversationKind::Single
            || identity.sender_id.is_empty()
            || identity.conversation_id.is_empty()
        {
            return Err(invalid(
                "operator task origins require an existing private channel Session",
            ));
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
        Ok((saved, identity, plan))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn operator_subscription_requires_consumption_policy_without_execution_grants() {
        let (_root, application, channels, sessions, session, target) =
            super::super::assignment_tests::prepared().await;
        let subscribe = || {
            application.operator_subscription_origin(
                "local",
                &session,
                "shared-plan",
                "Deliver the published result here",
            )
        };
        assert!(subscribe().is_err());
        let mut definition = application
            .load_graph("assistant")
            .unwrap()
            .1
            .authoring_definition;
        definition["nodes"][1]["plugins"][1]["graph_tasks"] = json!({"schedules":["shared-plan"]});
        application
            .update_graph("assistant", definition)
            .await
            .unwrap();
        let before = sessions.get("local", &session).unwrap();
        let origin = subscribe().unwrap();
        assert_eq!(origin.owner, "local");
        assert_eq!(origin.session.as_deref(), Some(session.as_str()));
        assert_eq!(origin.graph, "assistant");
        assert_eq!(origin.node_id(), "work");
        assert!(origin.agent_key().is_none());
        assert!(origin.policy.graphs.is_empty());
        assert!(origin.targets.is_empty());
        assert!(matches!(
            &origin.source,
            TaskOriginSource::OperatorSubscription { schedule, instruction, .. }
                if schedule == "shared-plan" && instruction == "Deliver the published result here"
        ));
        assert!(
            application
                .authorize_delegated_graph(&origin, &target.graph)
                .is_err()
        );
        assert!(
            application
                .operator_subscription_origin("local", &session, "another-plan", "Deliver here")
                .is_err()
        );
        assert!(
            application
                .operator_subscription_origin(
                    "another-owner",
                    &session,
                    "shared-plan",
                    "Deliver here"
                )
                .is_err()
        );
        assert_eq!(sessions.get("local", &session).unwrap(), before);
        assert!(application.active_runs(None).await.is_empty());
        assert_eq!(application.records().unwrap().len(), 1);
        sessions
            .set_plugin_mount("local", &session, "graph-tasks", false, "operator", "test")
            .unwrap();
        assert!(subscribe().is_err());
        sessions
            .set_plugin_mount("local", &session, "graph-tasks", true, "operator", "test")
            .unwrap();
        assert!(subscribe().is_ok());
        channels.update("assistant", |bot| bot.binding.send_users.clear());
        assert!(subscribe().is_err());
    }
}
