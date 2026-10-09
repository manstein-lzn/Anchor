//! Explicit operator abandonment of a whole Run.
//!
//! `stop` parks a Run in `Stopped`, which stays resumable and therefore still
//! counts as unfinished when a Graph's Plugin resources are replaced. `abandon`
//! is the one-way control action for that case: it cancels in-flight node
//! execution through the ordinary stop chain, then records the Run as terminal
//! `Aborted` with an audited reason.
//!
//! Nothing is deleted. The frozen snapshot, results, recovery facts, workspace,
//! artifacts, Session Turn history and the assistant binding all stay exactly
//! as they were, so a channel instance can still be handed over to a new Run by
//! the existing (automatic or explicit) recovery path.

use super::*;
use crate::assistant::{read_json, save_immutable};
use serde::{Deserialize, Serialize};

pub(crate) const REASON_OPERATOR: &str = "operator";
pub(crate) const REASON_PLUGIN_UPDATE: &str = "plugin_update";

/// Directory of durable abandon intents, relative to the Host state root.
const ABANDON_DIR: &str = "run-abandons";
/// How long an abandon request waits for an active Run to observe cancellation
/// before reporting that the terminal transition is still in flight.
const CANCEL_POLL: Duration = Duration::from_millis(50);
const CANCEL_POLLS: usize = 600;

/// Durable, immutable record of one accepted abandon request.
///
/// Its only roles are auditability, idempotency, and telling a completion or a
/// restart that this Run must become terminal `Aborted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AbandonIntent {
    format: u32,
    pub(crate) run_id: String,
    pub(crate) reason: String,
    pub(crate) requested_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbandonAction {
    /// This Run is durably `Aborted` because of a recorded abandon request.
    Abandoned,
    /// The Run was already terminal `Aborted` without a recorded abandon
    /// request (for example a node-level recovery abort).
    AlreadyAborted,
    /// Cancellation was accepted but the Run had not released its execution
    /// slot yet; the recorded intent makes it terminal without further action.
    Abandoning,
}

impl AbandonAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Abandoned => "abandoned",
            Self::AlreadyAborted => "already_aborted",
            Self::Abandoning => "abandoning",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AbandonResult {
    pub(crate) action: AbandonAction,
    pub(crate) status: RunStatus,
    pub(crate) reason: Option<String>,
    pub(crate) requested_at: Option<String>,
}

fn parse_reason(reason: Option<&str>) -> Result<String, ApplicationError> {
    match reason {
        None => Ok(REASON_OPERATOR.into()),
        Some(value) if value == REASON_OPERATOR || value == REASON_PLUGIN_UPDATE => {
            Ok(value.into())
        }
        Some(_) => Err(ApplicationError::Invalid(format!(
            "unknown abandon reason; expected `{REASON_OPERATOR}` or `{REASON_PLUGIN_UPDATE}`"
        ))),
    }
}

impl RunApplication {
    fn abandon_intent_path(&self, run_id: &str) -> PathBuf {
        self.data_root
            .join(ABANDON_DIR)
            .join(format!("{run_id}.json"))
    }

    /// A Run id that is safe to use as one path component.
    pub(crate) fn valid_run_component(run_id: &str) -> bool {
        !run_id.is_empty()
            && run_id != "."
            && run_id != ".."
            && run_id
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    }

    pub(crate) fn abandon_intent(
        &self,
        run_id: &str,
    ) -> Result<Option<AbandonIntent>, ApplicationError> {
        if !Self::valid_run_component(run_id) {
            return Err(ApplicationError::Invalid("invalid Run id".into()));
        }
        let intent =
            read_json::<AbandonIntent>(&self.abandon_intent_path(run_id)).map_err(storage)?;
        if let Some(intent) = &intent
            && (intent.format != 1 || intent.run_id != run_id)
        {
            return Err(ApplicationError::Storage(
                "abandon intent identity changed".into(),
            ));
        }
        Ok(intent)
    }

    /// Apply a recorded abandon request once the Run holds no execution slot.
    ///
    /// `Ok(None)` means no request was ever recorded; the caller keeps the
    /// status the Runner produced.
    pub(crate) async fn finalize_recorded_abandon(
        &self,
        run_id: &str,
    ) -> Result<Option<RunStatus>, ApplicationError> {
        let Some(intent) = self.abandon_intent(run_id)? else {
            return Ok(None);
        };
        Ok(Some(
            self.finalize_abandoned_run(run_id, &intent.reason, &intent.requested_at)
                .await?,
        ))
    }

    /// Make the Run terminal `Aborted` unless it is still executing (or already
    /// terminal). Holds the execution lock so a concurrent resume cannot slip
    /// between the liveness check and the durable write.
    async fn finalize_abandoned_run(
        &self,
        run_id: &str,
        reason: &str,
        requested_at: &str,
    ) -> Result<RunStatus, ApplicationError> {
        let active = self.active.lock().await;
        if active.contains_key(run_id) {
            return Ok(self
                .store()
                .load(run_id)?
                .ok_or(ApplicationError::Missing)?
                .status);
        }
        let _lease = self.store().acquire_lease(run_id)?;
        let mut record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        if !matches!(
            record.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
        ) {
            record.status = RunStatus::Aborted;
            record.error = Some(format!(
                "Run abandoned by operator (reason {reason}, requested at {requested_at})"
            ));
            self.store().save(&record)?;
        }
        Ok(record.status)
    }

    async fn request_active_cancellation(&self, run_id: &str) {
        let active = self.active.lock().await;
        if let Some(run) = active.get(run_id) {
            run.control.cancellation.store(true, Ordering::Release);
        }
    }

    /// Abandon one whole Run: cancel its execution through the existing stop
    /// chain and record it as terminal `Aborted`.
    ///
    /// The request is idempotent: repeating it with the same reason returns the
    /// same result, and repeating it with a different reason is a conflict. A
    /// Run that already completed or failed cannot be abandoned.
    pub(crate) async fn abandon_run(
        &self,
        run_id: &str,
        reason: Option<&str>,
    ) -> Result<AbandonResult, ApplicationError> {
        if !Self::valid_run_component(run_id) {
            return Err(ApplicationError::Invalid("invalid Run id".into()));
        }
        let reason = parse_reason(reason)?;
        let record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        let recorded = self.abandon_intent(run_id)?;
        if let Some(intent) = &recorded
            && intent.reason != reason
        {
            return Err(ApplicationError::Conflict(format!(
                "Run was already abandoned for reason `{}`; requested reason `{reason}` conflicts",
                intent.reason
            )));
        }
        match record.status {
            RunStatus::Aborted => {
                // Terminal already: report the existing state and never rewrite
                // it. A recorded request answers exactly like the call that
                // recorded it, so repeating the request is idempotent.
                let action = if recorded.is_some() {
                    AbandonAction::Abandoned
                } else {
                    AbandonAction::AlreadyAborted
                };
                return Ok(AbandonResult {
                    action,
                    status: RunStatus::Aborted,
                    reason: recorded.as_ref().map(|intent| intent.reason.clone()),
                    requested_at: recorded.map(|intent| intent.requested_at),
                });
            }
            RunStatus::Completed | RunStatus::Failed => {
                return Err(ApplicationError::Conflict(
                    "terminal Run cannot be abandoned".into(),
                ));
            }
            _ => {}
        }
        // Durable before any cancellation: a Host that dies mid-request still
        // makes this Run terminal on its next start.
        let intent = match recorded {
            Some(intent) => intent,
            None => {
                let intent = AbandonIntent {
                    format: 1,
                    run_id: run_id.into(),
                    reason: reason.clone(),
                    requested_at: chrono::DateTime::<chrono::Utc>::from(
                        std::time::SystemTime::now(),
                    )
                    .to_rfc3339(),
                };
                save_immutable(&self.abandon_intent_path(run_id), &intent).map_err(storage)?;
                intent
            }
        };
        // Session calls own a delivery lifecycle: settle it through its own
        // chain first so a waiting parent is released exactly as a stop does.
        if self
            .metadata(run_id)?
            .is_some_and(|metadata| metadata.session_call.is_some())
        {
            match self.stop_session_call(run_id).await {
                Ok(()) | Err(ApplicationError::Conflict(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let mut polls = 0;
        while self.run_is_active(run_id).await {
            self.request_active_cancellation(run_id).await;
            if polls >= CANCEL_POLLS {
                let status = self
                    .store()
                    .load(run_id)?
                    .ok_or(ApplicationError::Missing)?
                    .status;
                return Ok(AbandonResult {
                    action: AbandonAction::Abandoning,
                    status,
                    reason: Some(intent.reason),
                    requested_at: Some(intent.requested_at),
                });
            }
            polls += 1;
            tokio::time::sleep(CANCEL_POLL).await;
        }
        let record = self
            .store()
            .load(run_id)?
            .ok_or(ApplicationError::Missing)?;
        if record.status == RunStatus::WaitingCall {
            // Nothing executes this Run any more, but its waiting children can.
            // Stop them exactly as `stop` does before this Run turns terminal.
            let wait_children = wait_child_ids(&record);
            self.stop_wait_children(run_id, wait_children).await?;
        }
        let status = self
            .finalize_abandoned_run(run_id, &intent.reason, &intent.requested_at)
            .await?;
        match status {
            RunStatus::Aborted => {
                self.settle_channel_run(run_id, RunStatus::Aborted)?;
                Ok(AbandonResult {
                    action: AbandonAction::Abandoned,
                    status,
                    reason: Some(intent.reason),
                    requested_at: Some(intent.requested_at),
                })
            }
            RunStatus::Completed | RunStatus::Failed => Err(ApplicationError::Conflict(
                "Run finished before it was abandoned".into(),
            )),
            // A resume that won the race before the request was recorded keeps
            // executing; cancellation will settle it into the requested state.
            _ => Ok(AbandonResult {
                action: AbandonAction::Abandoning,
                status,
                reason: Some(intent.reason),
                requested_at: Some(intent.requested_at),
            }),
        }
    }

    /// Make every recorded, still-unfinished abandon request terminal. Called at
    /// startup, before channel assistant recovery hands instances over, so an
    /// interrupted abandon cannot keep blocking Graph/Plugin replacement.
    pub(crate) async fn finalize_recorded_abandons_at_startup(
        &self,
    ) -> Result<(), ApplicationError> {
        let root = self.data_root.join(ABANDON_DIR);
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(storage(error)),
        };
        for entry in entries {
            let entry = entry.map_err(storage)?;
            let Some(run_id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .map(str::to_owned)
            else {
                continue;
            };
            let intent = match self.abandon_intent(&run_id) {
                Ok(Some(intent)) => intent,
                Ok(None) => continue,
                Err(error) => {
                    eprintln!("Run {run_id} abandon intent is unreadable: {error:?}");
                    continue;
                }
            };
            match self
                .finalize_abandoned_run(&run_id, &intent.reason, &intent.requested_at)
                .await
            {
                Ok(RunStatus::Aborted) => {
                    if let Err(error) = self.settle_channel_run(&run_id, RunStatus::Aborted) {
                        eprintln!("Run {run_id} abandon settlement failed: {error:?}");
                    }
                }
                Ok(_) => {}
                Err(error) => eprintln!("Run {run_id} abandon finalization failed: {error:?}"),
            }
        }
        Ok(())
    }
}
