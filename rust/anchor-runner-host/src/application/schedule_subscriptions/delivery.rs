use super::*;
use anchor_platform_session::SessionError;
use std::path::Path;

fn local_time(value: &str) -> Result<NaiveDateTime, ApplicationError> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").map_err(storage)
}

pub(super) fn result_id(run: &str, subscription: &str) -> Result<String, ApplicationError> {
    Ok(format!("subscription-{}", digest(&(run, subscription))?))
}

impl RunApplication {
    pub(in crate::application) fn schedule_publication_at_admission(
        &self,
        metadata: &RunMetadata,
    ) -> Result<Option<String>, ApplicationError> {
        let Some(schedule) = metadata.schedule.as_deref() else {
            return Ok(None);
        };
        let Some(share) = self.sharing(schedule)? else {
            return Ok(None);
        };
        let owner = crate::credential_store::binding_owner(&share.owner);
        if !share.enabled
            || metadata.graph != share.graph
            || metadata.oauth_owner.as_deref() != Some(owner.as_str())
        {
            return Ok(None);
        }
        Ok(Some(if share.revision.is_empty() {
            schedule.to_owned()
        } else {
            share.revision
        }))
    }

    pub(super) fn subscription_eligible(
        &self,
        subscription: &ScheduleSubscription,
        metadata: &RunMetadata,
    ) -> Result<bool, ApplicationError> {
        if metadata.schedule.as_deref() != Some(subscription.schedule.as_str())
            || metadata.schedule_publication.is_none()
        {
            return Ok(false);
        }
        let Some(at) = metadata.scheduled_at.as_deref() else {
            return Ok(false);
        };
        if local_time(at)? <= local_time(&subscription.effective_at)? {
            return Ok(false);
        }
        self.subscription_authorized(subscription)
    }

    pub(super) fn subscription_ready(
        &self,
        subscription: &ScheduleSubscription,
        metadata: &RunMetadata,
    ) -> Result<bool, ApplicationError> {
        let Some(time) = subscription.not_before.as_deref() else {
            return Ok(true);
        };
        let occurrence = local_time(
            metadata
                .scheduled_at
                .as_deref()
                .ok_or(ApplicationError::Missing)?,
        )?;
        let floor = occurrence
            .date()
            .and_time(NaiveTime::parse_from_str(time, "%H:%M").map_err(storage)?);
        Ok(Local::now().naive_local() >= floor)
    }

    fn validate_shared_execution(&self, metadata: &RunMetadata) -> Result<(), ApplicationError> {
        let view = self.read_run(&metadata.run_id)?.ok_or(ApplicationError::Missing)?;
        if view.progress().graph_digest != metadata.graph_digest {
            return Err(invalid("shared execution Graph identity changed"));
        }
        Ok(())
    }

    pub(in crate::application) async fn deliver_subscription_results(
        &self,
        run: &str,
        workspace: &Path,
    ) -> Result<usize, ApplicationError> {
        let Some(metadata) = self.metadata(run)? else {
            return Ok(0);
        };
        if metadata.schedule_publication.is_none() {
            return Ok(0);
        }
        let _guard = self.graph_catalog_mutation_guard().await;
        let Some(view) = self.read_run(run)? else {
            return Ok(0);
        };
        if !view.is_settled() {
            if matches!(
                view.phase(),
                super::super::run_access::RunPhase::Waiting {
                    for_what: super::super::run_access::RunWait::Human
                        | super::super::run_access::RunWait::Effect,
                    ..
                }
            ) {
                return self.subscription_notice(&metadata, "run.blocked");
            }
            return Ok(0);
        }
        if view.active() {
            return Ok(0);
        }
        let sessions = crate::assistant::sessions(&self.data_root).map_err(storage)?;
        let mut prepared = None;
        let mut delivered = 0;
        let mut failure = None;
        for subscription in self.subscriptions()? {
            let attempt = async {
                if !self.subscription_eligible(&subscription, &metadata)? || !self.subscription_ready(&subscription, &metadata)? { return Ok(false); }
                let session = subscription.origin.session.as_deref().ok_or(ApplicationError::Missing)?;
                let id = result_id(run, &subscription.id)?;
                if sessions.task_result(&subscription.origin.owner, session, &id).map_err(storage)?.is_some() { return Ok(false); }
                if prepared.is_none() {
                    self.validate_shared_execution(&metadata)?;
                    prepared = Some(self.task_completion(&metadata, &view, workspace, true).await?);
                }
                let mut result: TaskResult = prepared.as_ref().expect("prepared shared completion").clone();
                result.id = id;
                result.content["subscription"] = json!({"id":subscription.id,"schedule":subscription.schedule,"sharing":subscription.sharing,"created_at":subscription.created_at});
                result.content["request"] = json!({"instruction":subscription.instruction});
                sessions.record_task_result(&subscription.origin.owner, session, &result).map_err(storage)?;
                crate::assistant::signal(session).notify_waiters();
                Ok::<_, ApplicationError>(true)
            }.await;
            match attempt {
                Ok(true) => delivered += 1,
                Ok(false) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        if failure.is_some()
            && let Err(error) = self.subscription_notice(&metadata, "result.blocked")
        {
            failure.get_or_insert(error);
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(delivered),
        }
    }

    /// Called by the channel adapter immediately before using a subscription
    /// result. Cancellation never rewrites confirmed/unknown gateway facts.
    pub(crate) fn authorize_subscription_result(
        &self,
        owner: &str,
        session: &str,
        result: &TaskResult,
    ) -> Result<(), ApplicationError> {
        let Some(envelope) = result.content.get("subscription") else {
            return Ok(());
        };
        let subscription =
            self.subscription(envelope["id"].as_str().ok_or(ApplicationError::Missing)?)?;
        if subscription.origin.owner != owner
            || subscription.origin.session.as_deref() != Some(session)
            || envelope["created_at"] != subscription.created_at
        {
            return Err(ApplicationError::Missing);
        }
        let metadata = match result.source_run.as_deref() {
            Some(run) => self.metadata(run)?.ok_or(ApplicationError::Missing)?,
            None if result.content["event"] == "schedule.blocked" => self
                .subscription_notice_metadata(
                    &subscription.schedule,
                    result.content["scheduled_at"]
                        .as_str()
                        .ok_or(ApplicationError::Missing)?,
                )?
                .ok_or(ApplicationError::Missing)?,
            None => return Err(ApplicationError::Missing),
        };
        let expected = if result.outcome == "blocked" {
            let event = result.content["event"]
                .as_str()
                .ok_or(ApplicationError::Missing)?;
            if !matches!(event, "schedule.blocked" | "run.blocked" | "result.blocked")
                || !result.files.is_empty()
            {
                return Err(ApplicationError::Missing);
            }
            notice_id(&subscription.id, &metadata, event)?
        } else {
            result_id(
                result
                    .source_run
                    .as_deref()
                    .ok_or(ApplicationError::Missing)?,
                &subscription.id,
            )?
        };
        if result.id != expected {
            return Err(ApplicationError::Missing);
        }
        let sessions = crate::assistant::sessions(&self.data_root).map_err(storage)?;
        if !self.subscription_eligible(&subscription, &metadata)?
            || !self.subscription_ready(&subscription, &metadata)?
            || sessions
                .task_result_cancelled(owner, session, &result.id)
                .map_err(storage)?
        {
            return Err(invalid(
                "subscription was withdrawn or is outside its delivery window",
            ));
        }
        Ok(())
    }

    pub(in crate::application) fn cancel_ineligible_subscription_results(
        &self,
    ) -> Result<(), ApplicationError> {
        let subscriptions = self.subscriptions()?;
        if subscriptions.is_empty() {
            return Ok(());
        }
        let sessions = crate::assistant::sessions(&self.data_root).map_err(storage)?;
        let mut failure = None;
        for subscription in subscriptions {
            let Some(session) = subscription.origin.session.as_deref() else {
                continue;
            };
            let pending = match sessions.pending_task_results(&subscription.origin.owner, session) {
                Ok(pending) => pending,
                Err(SessionError::Missing) => continue,
                Err(error) => {
                    failure.get_or_insert(storage(error));
                    continue;
                }
            };
            for result in pending
                .iter()
                .filter(|result| result.content["subscription"]["id"] == subscription.id)
            {
                match self.authorize_subscription_result(
                    &subscription.origin.owner,
                    session,
                    result,
                ) {
                    Ok(()) => {}
                    Err(ApplicationError::Storage(error)) => {
                        failure.get_or_insert(ApplicationError::Storage(error));
                    }
                    Err(_) => {
                        if let Err(error) = sessions.cancel_task_result(
                            &subscription.origin.owner,
                            session,
                            &result.id,
                            "subscription authority withdrawn",
                        ) {
                            failure.get_or_insert(storage(error));
                        }
                    }
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(in crate::application) fn subscription_result_pending(
        &self,
        metadata: &RunMetadata,
    ) -> Result<bool, ApplicationError> {
        if metadata.schedule_publication.is_none() {
            return Ok(false);
        }
        let sessions = crate::assistant::sessions(&self.data_root).map_err(storage)?;
        for subscription in self.subscriptions()? {
            if metadata.schedule.as_deref() != Some(subscription.schedule.as_str()) {
                continue;
            }
            let Some(session) = subscription.origin.session.as_deref() else {
                continue;
            };
            let owner = &subscription.origin.owner;
            let protected = match sessions.protected_task_results(owner, session) {
                Ok(protected) => protected,
                Err(SessionError::Missing) => continue,
                Err(error) => return Err(storage(error)),
            };
            if protected
                .iter()
                .any(|result| result.source_run.as_deref() == Some(metadata.run_id.as_str()))
            {
                return Ok(true);
            }
            // This obligation exists before enqueue, including partial fan-out
            // and the not-before wait. Session receipts are the only ACK store.
            if self.subscription_eligible(&subscription, metadata)?
                && sessions
                    .task_result(
                        owner,
                        session,
                        &result_id(&metadata.run_id, &subscription.id)?,
                    )
                    .map_err(storage)?
                    .is_none()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
