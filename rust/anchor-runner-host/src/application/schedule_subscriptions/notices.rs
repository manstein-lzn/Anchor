//! Bounded status notices use the same Session queue, never producer diagnostics.
use super::*;

pub(super) fn notice_id(
    subscription: &str,
    metadata: &RunMetadata,
    event: &str,
) -> Result<String, ApplicationError> {
    Ok(format!(
        "subscription-status-{}",
        digest(&(
            subscription,
            &metadata.schedule,
            &metadata.scheduled_at,
            event
        ))?
    ))
}

impl RunApplication {
    pub(super) fn subscription_notice_metadata(
        &self,
        schedule: &str,
        at: &str,
    ) -> Result<Option<RunMetadata>, ApplicationError> {
        let Some(share) = self.sharing(schedule)? else {
            return Ok(None);
        };
        if !self.sharing_current(&share)? {
            return Ok(None);
        }
        // Only a local eligibility carrier, never saved or presented as a Run.
        let path = self.graph_bundle_path(&share.graph);
        let mut metadata = RunMetadata::new(String::new(), share.graph, String::new(), &path)?;
        metadata.schedule = Some(schedule.into());
        metadata.scheduled_at = Some(at.into());
        metadata.schedule_publication = Some(if share.revision.is_empty() {
            schedule.to_owned()
        } else {
            share.revision
        });
        Ok(Some(metadata))
    }

    pub(crate) async fn record_subscription_schedule_blocked(
        &self,
        schedule: &str,
        at: &str,
    ) -> Result<usize, ApplicationError> {
        let _guard = self.graph_catalog_mutation_guard().await;
        match self.subscription_notice_metadata(schedule, at)? {
            Some(metadata) => self.subscription_notice(&metadata, "schedule.blocked"),
            None => Ok(0),
        }
    }

    pub(super) fn subscription_notice(
        &self,
        metadata: &RunMetadata,
        event: &str,
    ) -> Result<usize, ApplicationError> {
        let sessions = crate::assistant::sessions(&self.data_root).map_err(storage)?;
        let mut count = 0;
        let mut failure = None;
        for subscription in self.subscriptions()? {
            let attempt = || -> Result<bool, ApplicationError> {
                if !self.subscription_eligible(&subscription, metadata)?
                    || !self.subscription_ready(&subscription, metadata)?
                {
                    return Ok(false);
                }
                let session = subscription
                    .origin
                    .session
                    .as_deref()
                    .ok_or(ApplicationError::Missing)?;
                if event == "result.blocked"
                    && sessions
                        .task_result(
                            &subscription.origin.owner,
                            session,
                            &super::delivery::result_id(&metadata.run_id, &subscription.id)?,
                        )
                        .map_err(storage)?
                        .is_some()
                {
                    return Ok(false);
                }
                let id = notice_id(&subscription.id, metadata, event)?;
                if sessions
                    .task_result(&subscription.origin.owner, session, &id)
                    .map_err(storage)?
                    .is_some()
                {
                    return Ok(false);
                }
                let result = TaskResult {
                    id,
                    source_run: (event != "schedule.blocked").then(|| metadata.run_id.clone()),
                    outcome: "blocked".into(),
                    files: vec![],
                    content: json!({
                        "event":event,"graph":metadata.graph,"schedule":metadata.schedule,"scheduled_at":metadata.scheduled_at,
                        "subscription":{"id":subscription.id,"schedule":subscription.schedule,"sharing":subscription.sharing,"created_at":subscription.created_at},
                        "request":{"instruction":subscription.instruction},
                        "reason":"The scheduled publication is not ready. Its retained occurrence requires operator inspection; no final files are available.",
                        "admission_confirmed":event != "schedule.blocked",
                        "retry":"Do not start another producer or infer that no admission facts exist. The original occurrence and receipts remain authoritative."
                    }),
                };
                sessions
                    .record_task_result(&subscription.origin.owner, session, &result)
                    .map_err(storage)?;
                crate::assistant::signal(session).notify_waiters();
                Ok(true)
            };
            match attempt() {
                Ok(true) => count += 1,
                Ok(false) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(count),
        }
    }
}
