//! Consume an existing schedule. Production, Session queues and channel ACKs
//! retain their existing owners; only sharing and recipient relationships live here.
use super::{ApplicationError, RunApplication, RunMetadata, storage};
use crate::task_delegation::TaskOrigin;
use anchor_platform_session::{ChannelConversationKind, TaskResult};
use chrono::{Local, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

mod delivery;
mod notices;
use notices::notice_id;
#[cfg(test)]
mod tests;

/// Operator sharing grant for one schedule. Execution versions belong to each
/// admitted Run; legacy `graphs` fields on disk are ignored and can be dropped
/// once local sharing facts are rewritten.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ScheduleSharing {
    pub(crate) schedule: String,
    owner: String,
    pub(crate) graph: String,
    #[serde(default)]
    input: Value,
    #[serde(default)]
    pub(crate) revision: String,
    description: String,
    sessions: BTreeSet<String>,
    enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScheduleSubscription {
    pub(crate) id: String,
    pub(crate) schedule: String,
    #[serde(default)]
    pub(crate) sharing: String,
    pub(crate) origin: TaskOrigin,
    pub(crate) instruction: String,
    pub(crate) not_before: Option<String>,
    /// Host local occurrence boundary, not the delayed actual admission time.
    pub(crate) effective_at: String,
    pub(crate) created_at: String,
    pub(crate) active: bool,
}

fn invalid(message: impl Into<String>) -> ApplicationError {
    ApplicationError::Invalid(message.into())
}
fn digest(value: &impl Serialize) -> Result<String, ApplicationError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(storage)?)
    ))
}
fn validate_instruction(
    instruction: &str,
    not_before: Option<&str>,
) -> Result<(), ApplicationError> {
    if instruction.trim().is_empty() || instruction.len() > 8192 {
        return Err(invalid(
            "subscription instruction must contain text within 8192 bytes",
        ));
    }
    if let Some(time) = not_before {
        let parsed = NaiveTime::parse_from_str(time, "%H:%M")
            .map_err(|_| invalid("not_before must be Host-local HH:mm"))?;
        if parsed.format("%H:%M").to_string() != time {
            return Err(invalid("not_before must be Host-local HH:mm"));
        }
    }
    Ok(())
}

impl RunApplication {
    fn sharing(&self, schedule: &str) -> Result<Option<ScheduleSharing>, ApplicationError> {
        self.subscription_fact(&format!("sharing/{}", digest(&schedule)?))?
            .map(serde_json::from_value)
            .transpose()
            .map_err(storage)
    }

    pub(in crate::application) fn subscriptions(
        &self,
    ) -> Result<Vec<ScheduleSubscription>, ApplicationError> {
        self.subscription_facts("recipients/")?
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(storage))
            .collect()
    }

    fn subscription(&self, id: &str) -> Result<ScheduleSubscription, ApplicationError> {
        if !anchor_runtime::identifier::is_name(id) {
            return Err(ApplicationError::Missing);
        }
        self.subscription_fact(&format!("recipients/{id}"))?
            .ok_or(ApplicationError::Missing)
            .and_then(|value| serde_json::from_value(value).map_err(storage))
    }

    fn store_subscription(
        &self,
        subscription: &ScheduleSubscription,
    ) -> Result<(), ApplicationError> {
        self.put_subscription_fact(
            &format!("recipients/{}", subscription.id),
            json!(subscription),
        )
    }

    fn sharing_current(&self, share: &ScheduleSharing) -> Result<bool, ApplicationError> {
        if !share.enabled {
            return Ok(false);
        }
        let target = match self
            .schedule_management()?
            .assignment_target(&share.owner, &share.schedule)
        {
            Ok(target) => target,
            Err(ApplicationError::Missing) => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(target.graph == share.graph)
    }

    /// Explicit operator audience. Same installation owner is never a sharing grant.
    pub(crate) async fn share_schedule(
        &self,
        owner: &str,
        id: &str,
        description: &str,
        sessions: &[String],
        enabled: bool,
    ) -> Result<Value, ApplicationError> {
        if description.trim().is_empty()
            || description.len() > 2048
            || (enabled && sessions.is_empty())
        {
            return Err(invalid(
                "sharing requires a description and an explicit Session audience",
            ));
        }
        let _guard = self.graph_catalog_mutation_guard().await;
        let target = self.schedule_management()?.assignment_target(owner, id)?;
        let store = crate::assistant::sessions(&self.data_root).map_err(storage)?;
        for session in sessions {
            let identity = store.channel_identity(owner, session).map_err(storage)?;
            if !matches!(identity.source.as_str(), "wecom" | "feishu")
                || identity.conversation_kind != ChannelConversationKind::Single
            {
                return Err(invalid(
                    "sharing audience requires existing private channel Sessions",
                ));
            }
        }
        let mut share = ScheduleSharing {
            schedule: id.into(),
            owner: owner.into(),
            graph: target.graph.clone(),
            input: target.input,
            revision: String::new(),
            description: description.into(),
            sessions: sessions.iter().cloned().collect(),
            enabled,
        };
        if let Some(previous) = self.sharing(id)? {
            if previous.owner == share.owner
                && previous.graph == share.graph
                && previous.sessions == share.sessions
                && previous.enabled == share.enabled
                && !previous.revision.is_empty()
            {
                share.revision = previous.revision.clone();
                if previous == share {
                    return Ok(json!({"sharing":share}));
                }
            } else if previous.owner == share.owner
                && previous.graph == share.graph
                && previous.description == share.description
                && previous.sessions == share.sessions
                && previous.enabled == share.enabled
            {
                share.revision = previous.revision;
                return Ok(json!({"sharing":share}));
            }
        }
        if share.revision.is_empty() {
            share.revision = digest(&(&share, Utc::now().to_rfc3339()))?;
        }
        self.put_subscription_fact(&format!("sharing/{}", digest(&id)?), json!(share))?;
        // Persist revocation before cancelling queued inputs. Reconciliation
        // repeats this after a crash; channel sends always recheck this fact.
        self.cancel_ineligible_subscription_results()?;
        Ok(json!({"sharing":share}))
    }

    fn authorize_subscription(
        &self,
        origin: &TaskOrigin,
        share: &ScheduleSharing,
    ) -> Result<(), ApplicationError> {
        let session = origin
            .session
            .as_deref()
            .ok_or_else(|| invalid("subscriptions require a private assistant Session"))?;
        if origin.owner != share.owner
            || !share.sessions.contains(session)
            || !origin.policy.schedules.contains(&share.schedule)
        {
            return Err(ApplicationError::Missing);
        }
        let (saved, _, plan) = self.task_channel_identity(&origin.owner, session)?;
        if saved.graph != origin.graph || plan.work_node != origin.node_id() {
            return Err(ApplicationError::Missing);
        }
        let (policy, receipt) = self.current_task_grant(origin)?;
        if !policy.schedules.contains(&share.schedule) {
            return Err(ApplicationError::Missing);
        }
        let original = origin
            .bindings
            .get("graph-tasks")
            .ok_or(ApplicationError::Missing)?;
        if !receipt
            .bindings
            .get("graph-tasks")
            .is_some_and(|current| original.same_authorization(current))
        {
            return Err(invalid("graph-tasks authorization changed"));
        }
        if !self.sharing_current(share)? {
            return Err(ApplicationError::Conflict(
                "shared schedule contract changed or was withdrawn".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn discover_task_schedules(
        &self,
        origin: &TaskOrigin,
    ) -> Result<Value, ApplicationError> {
        let schedules = self.schedule_management()?.list()?;
        let subscriptions = self.subscriptions()?;
        let mut visible = Vec::new();
        for id in &origin.policy.schedules {
            let Some(share) = self.sharing(id)? else {
                continue;
            };
            match self.authorize_subscription(origin, &share) {
                Ok(()) => {}
                Err(ApplicationError::Storage(error)) => {
                    return Err(ApplicationError::Storage(error));
                }
                Err(_) => continue,
            }
            let Some(schedule) = schedules["schedules"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["id"] == *id))
            else {
                continue;
            };
            visible.push(json!({"id":id,"description":share.description,"graph":share.graph,"rule":schedule["rule"],
                "next_at":schedule["next_at"],"enabled":schedule["enabled"],"output":"committed terminal files and terminal submissions only",
                "subscribed":subscriptions.iter().any(|sub| sub.active && sub.schedule == *id && sub.origin.same_scope(origin))}));
        }
        Ok(json!({"schedules":visible}))
    }

    fn subscription_projection(
        &self,
        subscription: &ScheduleSubscription,
    ) -> Result<Value, ApplicationError> {
        let authorized = self.subscription_authorized(subscription)?;
        let session = subscription
            .origin
            .session
            .as_deref()
            .ok_or(ApplicationError::Missing)?;
        let pending = crate::assistant::sessions(&self.data_root)
            .map_err(storage)?
            .pending_task_results(&subscription.origin.owner, session)
            .map_err(storage)?;
        let results = pending
            .iter()
            .filter(|result| result.content["subscription"]["id"] == subscription.id)
            .map(|result| json!({"id":result.id,"run":result.source_run,"outcome":result.outcome}))
            .collect::<Vec<_>>();
        Ok(
            json!({"id":subscription.id,"schedule":subscription.schedule,"active":subscription.active,"authorized":authorized,
            "effective_at":subscription.effective_at,"not_before":subscription.not_before,"instruction":subscription.instruction,"pending_results":results}),
        )
    }

    pub(crate) fn list_task_subscriptions(
        &self,
        origin: &TaskOrigin,
    ) -> Result<Value, ApplicationError> {
        let items = self
            .subscriptions()?
            .iter()
            .filter(|subscription| subscription.origin.same_scope(origin))
            .map(|subscription| self.subscription_projection(subscription))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({"subscriptions":items}))
    }

    pub(crate) fn operator_list_subscriptions(
        &self,
        owner: &str,
    ) -> Result<Value, ApplicationError> {
        let items = self
            .subscriptions()?
            .iter()
            .filter(|subscription| subscription.origin.owner == owner)
            .map(|subscription| {
                let mut value = self.subscription_projection(subscription)?;
                value["session"] = json!(subscription.origin.session);
                Ok(value)
            })
            .collect::<Result<Vec<_>, ApplicationError>>()?;
        Ok(json!({"subscriptions":items}))
    }

    pub(in crate::application) fn graph_subscription_relations(
        &self,
        owner: &str,
    ) -> Result<Vec<Value>, ApplicationError> {
        let mut relations = Vec::new();
        for subscription in self
            .subscriptions()?
            .iter()
            .filter(|subscription| subscription.origin.owner == owner)
        {
            let Some(share) = self.sharing(&subscription.schedule)? else {
                continue;
            };
            if share.owner != owner || share.schedule != subscription.schedule {
                continue;
            }
            if ![
                subscription.origin.graph.as_str(),
                share.graph.as_str(),
                subscription.schedule.as_str(),
            ]
            .into_iter()
            .all(anchor_runtime::identifier::is_name)
                || validate_instruction(
                    &subscription.instruction,
                    subscription.not_before.as_deref(),
                )
                .is_err()
            {
                return Err(storage("invalid schedule subscription relation"));
            }
            relations.push(json!({
                "graph": subscription.origin.graph,
                "target": share.graph,
                "schedule": subscription.schedule,
                "active": subscription.active,
                "authorized": self.subscription_authorized(subscription)?,
                "not_before": subscription.not_before,
            }));
        }
        Ok(relations)
    }

    pub(crate) async fn subscribe_task_schedule(
        &self,
        origin: &TaskOrigin,
        schedule: &str,
        instruction: &str,
        not_before: Option<&str>,
    ) -> Result<Value, ApplicationError> {
        let _guard = self.graph_catalog_mutation_guard().await;
        self.subscribe_schedule(origin, schedule, instruction, not_before)
    }

    fn subscribe_schedule(
        &self,
        origin: &TaskOrigin,
        schedule: &str,
        instruction: &str,
        not_before: Option<&str>,
    ) -> Result<Value, ApplicationError> {
        validate_instruction(instruction, not_before)?;
        let share = self.sharing(schedule)?.ok_or(ApplicationError::Missing)?;
        self.authorize_subscription(origin, &share)?;
        let id = digest(&(origin.scope_key(), schedule))?;
        match self.subscription(&id) {
            Ok(previous) if previous.active => {
                if previous.instruction == instruction
                    && previous.not_before.as_deref() == not_before
                {
                    return Ok(json!({"subscription":self.subscription_projection(&previous)?}));
                }
                return Err(ApplicationError::Conflict(
                    "unsubscribe before changing an existing subscription contract".into(),
                ));
            }
            Ok(_) | Err(ApplicationError::Missing) => {}
            Err(error) => return Err(error),
        }
        let subscription = ScheduleSubscription {
            id,
            schedule: schedule.into(),
            sharing: share.revision,
            origin: origin.clone(),
            instruction: instruction.into(),
            not_before: not_before.map(str::to_owned),
            effective_at: Local::now()
                .naive_local()
                .format("%Y-%m-%dT%H:%M:%S%.f")
                .to_string(),
            created_at: Utc::now().to_rfc3339(),
            active: true,
        };
        self.store_subscription(&subscription)?;
        Ok(json!({"subscription":self.subscription_projection(&subscription)?}))
    }

    pub(crate) async fn operator_subscribe_schedule(
        &self,
        owner: &str,
        id: &str,
        session: &str,
        instruction: &str,
        not_before: Option<&str>,
    ) -> Result<Value, ApplicationError> {
        let _guard = self.graph_catalog_mutation_guard().await;
        let origin = self.operator_subscription_origin(owner, session, id, instruction)?;
        self.subscribe_schedule(&origin, id, instruction, not_before)
    }

    pub(crate) async fn unsubscribe_task_schedule(
        &self,
        origin: &TaskOrigin,
        id: &str,
    ) -> Result<Value, ApplicationError> {
        let _guard = self.graph_catalog_mutation_guard().await;
        let subscription = self.subscription(id)?;
        if !subscription.origin.same_scope(origin) {
            return Err(ApplicationError::Missing);
        }
        self.unsubscribe(subscription)
    }

    pub(crate) async fn operator_unsubscribe_schedule(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<Value, ApplicationError> {
        let _guard = self.graph_catalog_mutation_guard().await;
        let subscription = self.subscription(id)?;
        if subscription.origin.owner != owner {
            return Err(ApplicationError::Missing);
        }
        self.unsubscribe(subscription)
    }

    fn unsubscribe(
        &self,
        mut subscription: ScheduleSubscription,
    ) -> Result<Value, ApplicationError> {
        subscription.active = false;
        self.store_subscription(&subscription)?;
        self.cancel_ineligible_subscription_results()?;
        Ok(json!({"id":subscription.id,"active":false}))
    }

    fn subscription_authorized(
        &self,
        subscription: &ScheduleSubscription,
    ) -> Result<bool, ApplicationError> {
        if !subscription.active {
            return Ok(false);
        }
        let Some(share) = self.sharing(&subscription.schedule)? else {
            return Ok(false);
        };
        match self.authorize_subscription(&subscription.origin, &share) {
            Ok(()) => Ok(true),
            Err(ApplicationError::Storage(error)) => Err(ApplicationError::Storage(error)),
            Err(_) => Ok(false),
        }
    }

    /// Count enabled shared schedules and active subscriptions referencing `graph`.
    pub(in crate::application) fn shared_schedule_impact(
        &self,
        graph: &str,
    ) -> Result<(usize, usize), ApplicationError> {
        let mut schedules = BTreeSet::new();
        for state in self.subscription_facts("sharing/")? {
            let share: ScheduleSharing = serde_json::from_value(state).map_err(storage)?;
            if share.enabled && share.graph == graph {
                schedules.insert(share.schedule);
            }
        }
        if schedules.is_empty() {
            return Ok((0, 0));
        }
        let subscriptions = self
            .subscriptions()?
            .into_iter()
            .filter(|sub| sub.active && schedules.contains(&sub.schedule))
            .count();
        Ok((schedules.len(), subscriptions))
    }
}
