use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::pilot_tools::PilotTools;
use anchor_platform_session::{
    Session, SessionError, SessionStatus, SessionStore, Turn, TurnStatus,
};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

#[derive(Clone, Default)]
pub(crate) struct PilotService {
    gate: Arc<tokio::sync::Mutex<()>>,
    active: Arc<Mutex<HashMap<PathBuf, ActivePilot>>>,
}

struct ActivePilot {
    turn: String,
    cancellation: Arc<AtomicBool>,
}

/// A web policy change shares admission/settlement's boundary. Acquire this
/// before catalog -> Library guards; Session deletion also takes Pilot first.
pub(crate) struct PluginMountGuard {
    _gate: tokio::sync::OwnedMutexGuard<()>,
    active: Arc<Mutex<HashMap<PathBuf, ActivePilot>>>,
}

impl PluginMountGuard {
    pub(crate) fn effect(
        &self,
        sessions: &SessionStore,
        owner: &str,
        session: &Session,
        data_root: &std::path::Path,
        changed: bool,
    ) -> Result<(serde_json::Value, serde_json::Value), SessionError> {
        let source = self
            .active
            .lock()
            .map_err(|_| SessionError::Storage("Pilot control failed".into()))?
            .get(&scope(data_root, owner, session))
            .filter(|active| !active.cancellation.load(Ordering::Acquire))
            .map(|active| active.turn.clone());
        if !changed {
            return Ok((
                serde_json::json!({"status":"unchanged"}),
                serde_json::json!({"status":"none"}),
            ));
        }
        let turns = sessions.list_turns(owner, &session.id)?;
        let source = source.filter(|source| {
            session.graph.is_empty()
                && session.channel.is_empty()
                && session.status != SessionStatus::Archived
                && turns
                    .first()
                    .is_some_and(|turn| turn.id == *source && turn.status == TurnStatus::Running)
        });
        let Some(source) = source else {
            return Ok((
                serde_json::json!({"status":"next_turn"}),
                serde_json::json!({"status":"none"}),
            ));
        };
        if continuation_request(sessions, owner, &session.id, &source)?.is_none() {
            sessions.append_event(
                owner,
                &session.id,
                "pilot.continuation.requested",
                std::collections::BTreeMap::from([
                    ("turn".into(), serde_json::json!(source)),
                    (
                        "request_id".into(),
                        serde_json::json!(format!("continuation:{source}")),
                    ),
                    (
                        "reason".into(),
                        serde_json::json!("Session Plugin mounts changed from the web"),
                    ),
                ]),
            )?;
        }
        Ok((
            serde_json::json!({"status":"after_current_turn","source_turn":source}),
            continuation_status(sessions, owner, &session.id, &source)?,
        ))
    }
}

pub(crate) fn scope(data_root: &std::path::Path, owner: &str, session: &Session) -> PathBuf {
    let identity = serde_json::to_vec(&(owner, &session.id, session.created_at))
        .expect("Session identity is JSON");
    std::path::absolute(data_root)
        .unwrap_or_else(|_| data_root.to_path_buf())
        .join("platform/pilot")
        .join(format!("{:x}", Sha256::digest(identity)))
}

pub(crate) struct PilotAdmission {
    pub(crate) sessions: SessionStore,
    pub(crate) owner: String,
    pub(crate) session: Session,
    pub(crate) request_id: String,
    pub(crate) prompt: Option<String>,
    pub(crate) attachments: crate::attachments::PreparedAttachments,
    pub(crate) input_source: Option<String>,
    pub(crate) root: PathBuf,
    pub(crate) tools: PilotTools,
}

impl PilotAdmission {
    fn for_continuation(mut self, request_id: String) -> Self {
        self.input_source = request_id.strip_prefix("continuation:").map(str::to_owned);
        self.request_id = request_id;
        self.prompt = Some(CONTINUATION_PROMPT.into());
        self
    }
}

const CONTINUATION_PROMPT: &str = "Host 已应用能力或授权配置变化。能力变化不是新的业务请求。以原生会话中最近一次用户要求为准，仅继续尚未完成且已获授权的部分；已完成的结果和副作用不得因挂载、凭据或环境变化而重做，结果未知时先核查现场。若最近用户只讨论或确认能力，只说明配置与能力状态，不执行业务操作或改写已有产物。若无需继续，简短告知配置已生效。使用本轮重新冻结的能力，必要时核查 platform_status。";

pub(crate) enum AdmissionError {
    Session(SessionError),
    Provider,
}

impl From<SessionError> for AdmissionError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl PilotService {
    pub(crate) async fn plugin_mount_guard(&self) -> PluginMountGuard {
        PluginMountGuard {
            _gate: self.gate.clone().lock_owned().await,
            active: self.active.clone(),
        }
    }
    pub(crate) async fn ready_to_continue(
        &self,
        mut admission: PilotAdmission,
        source: &str,
        reason: &str,
    ) -> Result<(), AdmissionError> {
        let guard = self.gate.lock().await;
        let current = admission
            .sessions
            .get(&admission.owner, &admission.session.id)?;
        if current.created_at != admission.session.created_at
            || current.status == SessionStatus::Archived
        {
            return Err(
                SessionError::Conflict("continuation Session identity changed".into()).into(),
            );
        }
        let turn = admission
            .sessions
            .get_turn(&admission.owner, &current.id, source)?;
        if turn.status == TurnStatus::Stopped
            || continuation_cancelled(&admission.sessions, &admission.owner, &current.id, source)?
            || self
                .active
                .lock()
                .map_err(|_| SessionError::Storage("Pilot control failed".into()))?
                .get(&admission.root)
                .is_some_and(|active| active.cancellation.load(Ordering::Acquire))
            || admission
                .sessions
                .list_turns(&admission.owner, &current.id)?
                .first()
                .is_none_or(|latest| latest.id != source)
        {
            return Err(SessionError::Conflict(
                "continuation was stopped or superseded by a newer user Turn".into(),
            )
            .into());
        }
        let request_id = format!("continuation:{source}");
        admission.sessions.append_event(
            &admission.owner,
            &current.id,
            "pilot.continuation.requested",
            std::collections::BTreeMap::from([
                ("turn".into(), serde_json::json!(source)),
                ("request_id".into(), serde_json::json!(request_id)),
                ("reason".into(), serde_json::json!(reason)),
            ]),
        )?;
        drop(guard);
        if turn.status != TurnStatus::Completed {
            return Ok(());
        }
        admission = admission.for_continuation(request_id);
        let sessions = admission.sessions.clone();
        let owner = admission.owner.clone();
        let session = admission.session.id.clone();
        let result = self.admit(admission).await;
        if result.is_err() {
            let _ = sessions.append_event(
                &owner,
                &session,
                "pilot.continuation.failed",
                std::collections::BTreeMap::from([("turn".into(), serde_json::json!(source))]),
            );
        }
        result.map(|_| ())
    }
    pub(crate) async fn credential_submission_guard(
        &self,
        root: &std::path::Path,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, SessionError> {
        let guard = self.gate.clone().lock_owned().await;
        if self
            .active
            .lock()
            .map_err(|_| SessionError::Storage("Pilot control failed".into()))?
            .get(root)
            .is_some_and(|active| active.cancellation.load(Ordering::Acquire))
        {
            return Err(SessionError::Conflict(
                "Pilot is stopping; credential input is no longer active".into(),
            ));
        }
        Ok(guard)
    }

    pub(crate) fn admit(
        &self,
        mut admission: PilotAdmission,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Turn, AdmissionError>> + Send + '_>,
    > {
        Box::pin(async move {
            let _gate = self.gate.lock().await;
            let current = admission
                .sessions
                .get(&admission.owner, &admission.session.id)?;
            if current.created_at != admission.session.created_at
                || admission.root != scope(&admission.tools.data_root, &admission.owner, &current)
            {
                return Err(SessionError::Conflict(
                    "Pilot Session identity changed before admission".into(),
                )
                .into());
            }
            admission.session = current;
            let sessions = admission.sessions.clone();
            let owner = admission.owner.clone();
            let session = admission.session.id.clone();
            let request_id = admission.request_id.clone();
            let prompt = admission.prompt.clone();
            let existing = tokio::task::spawn_blocking(move || {
                sessions
                    .list_turns(&owner, &session)
                    .map(|turns| turns.into_iter().any(|turn| turn.request_id == request_id))
            })
            .await
            .map_err(|_| SessionError::Storage("Pilot admission failed".into()))??;
            if !existing && let Some(source) = admission.request_id.strip_prefix("continuation:") {
                let latest = admission
                    .sessions
                    .list_turns(&admission.owner, &admission.session.id)?
                    .into_iter()
                    .next();
                if latest
                    .is_none_or(|turn| turn.id != source || turn.status != TurnStatus::Completed)
                    || continuation_request(
                        &admission.sessions,
                        &admission.owner,
                        &admission.session.id,
                        source,
                    )?
                    .as_deref()
                        != Some(&admission.request_id)
                {
                    return Err(SessionError::Conflict(
                        "automatic continuation source is no longer the latest completed Turn"
                            .into(),
                    )
                    .into());
                }
            }
            if !existing && admission.session.status == SessionStatus::Archived {
                return Err(
                    SessionError::Conflict("archived sessions cannot accept turns".into()).into(),
                );
            }
            if !existing
                && self
                    .active
                    .lock()
                    .map_err(|_| SessionError::Storage("Pilot control failed".into()))?
                    .contains_key(&admission.root)
            {
                return Err(SessionError::Conflict(
                    "Pilot Session still has active execution".into(),
                )
                .into());
            }
            let goose = if existing {
                None
            } else {
                let turns = admission
                    .sessions
                    .list_turns(&admission.owner, &admission.session.id)?;
                if crate::goose_acp::runtime_mode() != "goose" {
                    eprintln!(
                        "anchor-runner-host: Pilot provider unavailable: runtime mode is not Goose"
                    );
                    return Err(AdmissionError::Provider);
                }
                if turns.iter().any(|turn| turn.native.is_some()) {
                    return Err(SessionError::Conflict(
                        "legacy Pilot turns cannot silently switch to Goose".into(),
                    )
                    .into());
                }
                Some(
                    crate::goose_acp::pilot::GoosePilot::from_state(
                        &admission.root,
                        turns.iter().find_map(|turn| turn.goose.as_ref()),
                        &admission.tools.data_root,
                        admission.tools.application.bwrap.clone(),
                    )
                    .map_err(|reason| {
                        eprintln!("anchor-runner-host: Pilot provider unavailable: {reason}");
                        AdmissionError::Provider
                    })?,
                )
            };
            // Resume and capability continuation inherit only the exact trusted
            // source Turn. A fresh user message receives only its own uploads.
            if admission.input_source.is_none()
                && admission.prompt.is_none()
                && admission.attachments.0.is_empty()
            {
                let turns = admission
                    .sessions
                    .list_turns(&admission.owner, &admission.session.id)?;
                if let Some(retained) = turns
                    .iter()
                    .find(|turn| turn.request_id == admission.request_id)
                {
                    admission.input_source = admission
                        .sessions
                        .turn_events(&admission.owner, &admission.session.id, &retained.id, 0)?
                        .into_iter()
                        .find(|event| event.data["type"] == "input-attachments")
                        .and_then(|event| {
                            event.data["manifest"]["source_turn"]
                                .as_str()
                                .map(str::to_owned)
                        });
                } else {
                    admission.input_source = turns.first().map(|turn| turn.id.clone());
                }
            }
            let mut trusted_source = None;
            if let Some(source) = admission.input_source.as_deref() {
                let source_turn =
                    admission
                        .sessions
                        .get_turn(&admission.owner, &admission.session.id, source)?;
                let (prepared, reference) = crate::pilot_attachments::source_inputs(
                    &admission.tools.data_root,
                    &admission.sessions,
                    &admission.owner,
                    &admission.session,
                    &source_turn,
                )
                .map_err(SessionError::Storage)?;
                admission.attachments = prepared;
                trusted_source = reference;
            }
            if !existing {
                crate::pilot_resources::register(
                    &admission.tools.data_root,
                    &admission.tools.workspace_root,
                    &admission.owner,
                    &admission.session,
                )
                .map_err(SessionError::Storage)?;
            }
            let identity =
                crate::pilot_attachments::identity(&admission.attachments, trusted_source.as_ref());
            let data = admission.tools.data_root.clone();
            let snapshot = admission.session.clone();
            let source = trusted_source;
            let prepared = std::mem::take(&mut admission.attachments);
            let sessions = admission.sessions.clone();
            let owner = admission.owner.clone();
            let session = admission.session.id.clone();
            let request_id = admission.request_id.clone();
            let (turn, created) = tokio::task::spawn_blocking(move || {
                sessions.create_turn_with_attachments(
                    &owner,
                    &session,
                    &request_id,
                    prompt.as_deref(),
                    &identity,
                    |turn| {
                        crate::pilot_attachments::freeze_inputs(
                            &data,
                            &owner,
                            &snapshot,
                            turn,
                            source.as_ref(),
                            &prepared,
                        )
                        .map_err(SessionError::Storage)
                    },
                )
            })
            .await
            .map_err(|_| SessionError::Storage("Pilot admission failed".into()))??;
            if !created {
                return Ok(turn);
            }
            let goose = goose
                .ok_or_else(|| SessionError::Storage("Pilot admission identity changed".into()))?;
            // Persist the mount policy before acknowledging this Turn. The MCP
            // connections below reuse this snapshot, even if a human changes the
            // Session switches while startup is waiting for a remote server.
            let frozen = crate::plugin_mounts::SessionMounts::open(&admission.tools.data_root)
                .and_then(|source| {
                    crate::node_host::freeze_session_switches(
                        &admission.tools.data_root,
                        &admission.session.id,
                        &turn.id,
                        &source,
                    )
                });
            match frozen {
                Ok(_) => {}
                Err(reason) => {
                    admission.sessions.finish_turn(
                        &admission.owner,
                        &admission.session.id,
                        &turn.id,
                        TurnStatus::Interrupted,
                        Some(&reason),
                    )?;
                    return Err(SessionError::Storage(reason).into());
                }
            }
            let cancellation = Arc::new(AtomicBool::new(false));
            match self.active.lock() {
                Ok(mut active) => {
                    active.insert(
                        admission.root.clone(),
                        ActivePilot {
                            turn: turn.id.clone(),
                            cancellation: cancellation.clone(),
                        },
                    );
                }
                Err(_) => {
                    admission.sessions.finish_turn(
                        &admission.owner,
                        &admission.session.id,
                        &turn.id,
                        TurnStatus::Interrupted,
                        Some("Pilot control failed"),
                    )?;
                    return Err(SessionError::Storage("Pilot control failed".into()).into());
                }
            }
            let turn_id = turn.id.clone();
            let request = crate::goose_acp::pilot::PilotTurn {
                root: admission.root.clone(),
                sessions: admission.sessions.clone(),
                owner: admission.owner.clone(),
                session: admission.session.id.clone(),
                turn: turn_id.clone(),
                data_root: admission.tools.data_root.clone(),
                prompt: admission.prompt.take().unwrap_or_else(|| {
                    if crate::pilot_attachments::input_references(
                        &admission.sessions,
                        &admission.owner,
                        &admission.session.id,
                        &turn_id,
                    )
                    .is_ok_and(|refs| !refs.is_empty())
                        && admission.input_source.is_none()
                    {
                        String::new()
                    } else {
                        "继续这个会话。先核查中断前的事实和现场，不要重放结果未知的操作。".into()
                    }
                }),
                instructions: INSTRUCTIONS.into(),
                cancellation,
            };
            let tools = admission.tools.clone().for_turn(&turn_id);
            let source_turn = turn_id.clone();
            let continuation = PilotAdmission {
                sessions: admission.sessions.clone(),
                owner: admission.owner.clone(),
                session: admission.session.clone(),
                request_id: String::new(),
                prompt: None,
                attachments: Default::default(),
                input_source: None,
                root: admission.root.clone(),
                tools: admission.tools.clone(),
            };
            let service = self.clone();
            tokio::spawn(async move {
                let root = admission.root.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| "Pilot executor could not start".to_owned())?;
                    runtime.block_on(execute_turn(goose, request, tools))
                })
                .await;
                let (status, failure) = match result {
                Ok(Ok(status)) => (status, None),
                Ok(Err(error @ crate::goose_acp::pilot::PilotError::Provider(_))) => {
                    (TurnStatus::Failed, Some(error.to_string()))
                }
                Ok(Err(message)) => {
                    eprintln!("Pilot executor failed: {message}");
                    (TurnStatus::Interrupted, Some("Pilot execution interrupted; continue with a new message to inspect the saved facts".to_owned()))
                }
                _ => (TurnStatus::Interrupted, Some("Pilot execution interrupted; continue with a new message to inspect the saved facts".to_owned())),
            };
                let saved = service.settle(admission, turn_id, status, failure).await;
                if saved.is_err() {
                    eprintln!("Pilot terminal state could not be persisted");
                }
                if saved.is_ok_and(|turn| turn.status == TurnStatus::Completed) {
                    let pending = continuation_request(
                        &continuation.sessions,
                        &continuation.owner,
                        &continuation.session.id,
                        &source_turn,
                    );
                    if let Ok(Some(request_id)) = pending {
                        let next = PilotAdmission {
                            sessions: continuation.sessions.clone(),
                            owner: continuation.owner.clone(),
                            session: continuation.session.clone(),
                            request_id: String::new(),
                            prompt: None,
                            attachments: Default::default(),
                            input_source: None,
                            root,
                            tools: continuation.tools,
                        }
                        .for_continuation(request_id);
                        if let Err(error) = service.admit(next).await {
                            let reason = match error {
                                AdmissionError::Session(error) => error.to_string(),
                                AdmissionError::Provider => "Pilot provider unavailable".into(),
                            };
                            let _ = continuation.sessions.append_event(
                                &continuation.owner,
                                &continuation.session.id,
                                "pilot.continuation.failed",
                                std::collections::BTreeMap::from([(
                                    "turn".into(),
                                    serde_json::json!(source_turn),
                                )]),
                            );
                            eprintln!(
                                "Pilot automatic continuation could not be admitted: {reason}"
                            );
                        }
                    }
                }
            });
            Ok(turn)
        })
    }

    async fn settle(
        &self,
        admission: PilotAdmission,
        turn: String,
        mut status: TurnStatus,
        mut failure: Option<String>,
    ) -> Result<Turn, SessionError> {
        let _gate = self.gate.lock().await;
        let root = admission.root.clone();
        // Stop may win the gate after execution has returned success. Its
        // cancellation still owns the terminal fact and forbids continuation.
        if self
            .active
            .lock()
            .map_err(|_| SessionError::Storage("Pilot control failed".into()))?
            .get(&root)
            .is_some_and(|active| {
                active.turn == turn && active.cancellation.load(Ordering::Acquire)
            })
        {
            status = TurnStatus::Stopped;
            failure = None;
        }
        let finished = turn.clone();
        let saved = tokio::task::spawn_blocking(move || {
            admission.sessions.finish_turn(
                &admission.owner,
                &admission.session.id,
                &finished,
                status,
                failure.as_deref(),
            )
        })
        .await
        .map_err(|_| SessionError::Storage("Pilot settlement failed".into()));
        if let Ok(mut active) = self.active.lock()
            && active.get(&root).is_some_and(|active| active.turn == turn)
        {
            active.remove(&root);
        }
        saved?
    }

    pub(crate) async fn stop_session(
        &self,
        sessions: SessionStore,
        data_root: PathBuf,
        owner: String,
        session: Session,
    ) -> Result<bool, SessionError> {
        let _gate = self.gate.lock().await;
        let current = sessions.get(&owner, &session.id)?;
        if current.created_at != session.created_at {
            return Err(SessionError::Conflict(
                "Pilot Session identity changed before stop".into(),
            ));
        }
        let root = scope(&data_root, &owner, &current);
        let stopping = self.cancel_active(&root);
        let latest = sessions.list_turns(&owner, &current.id)?.into_iter().next();
        let mut cancelled = false;
        if let Some(source) = latest
            && !continuation_cancelled(&sessions, &owner, &current.id, &source.id)?
        {
            // Bind stop to the latest source even if an OAuth callback has not
            // requested continuation yet. Keep the source's settled history.
            sessions.append_event(
                &owner,
                &current.id,
                "pilot.continuation.cancelled",
                std::collections::BTreeMap::from([
                    ("turn".into(), serde_json::json!(source.id)),
                    (
                        "request_id".into(),
                        serde_json::json!(format!("continuation:{}", source.id)),
                    ),
                ]),
            )?;
            cancelled = true;
        }
        Ok(stopping || cancelled)
    }

    fn cancel_active(&self, root: &std::path::Path) -> bool {
        if let Ok(active) = self.active.lock()
            && let Some(active) = active.get(root)
        {
            active.cancellation.store(true, Ordering::Release);
            return true;
        }
        false
    }

    pub(crate) async fn delete_session(
        &self,
        sessions: SessionStore,
        data: PathBuf,
        workspaces: PathBuf,
        library: PathBuf,
        owner: String,
        session: Session,
    ) -> Result<(), SessionError> {
        // Admission, finish and cleanup share one gate. Reload under it because
        // the API's earlier ownership lookup may precede another Turn.
        let _gate = self.gate.lock().await;
        let current = sessions.get(&owner, &session.id)?;
        if current.created_at != session.created_at {
            return Err(SessionError::Conflict(
                "Pilot Session identity changed before deletion".into(),
            ));
        }
        let root = scope(&data, &owner, &current);
        if self
            .active
            .lock()
            .map_err(|_| SessionError::Storage("Pilot control failed".into()))?
            .contains_key(&root)
        {
            return Err(SessionError::Conflict(
                "Pilot Session still has active execution".into(),
            ));
        }
        tokio::task::spawn_blocking(move || {
            crate::pilot_resources::release_development_scope(&library, &root)
                .map_err(SessionError::Conflict)?;
            crate::credential_store::delete_requests(&data, &owner, &current)
                .map_err(SessionError::Storage)?;
            crate::oauth_links::delete_session(&data, &owner, &current)
                .map_err(SessionError::Storage)?;
            crate::pilot_resources::delete(&sessions, &data, &workspaces, &owner, &current)
        })
        .await
        .map_err(|_| SessionError::Storage("Pilot cleanup task failed".into()))?
    }
}

fn continuation_request(
    sessions: &SessionStore,
    owner: &str,
    session: &str,
    turn: &str,
) -> Result<Option<String>, SessionError> {
    let events = sessions.events(owner, session, 0)?;
    if events
        .iter()
        .any(|event| is_continuation_cancel(event, turn))
    {
        return Ok(None);
    }
    let request = events
        .into_iter()
        .rev()
        .find(|event| {
            event.kind == "pilot.continuation.requested"
                && event.data.get("turn").and_then(serde_json::Value::as_str) == Some(turn)
        })
        .and_then(|event| {
            event
                .data
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    let Some(request) = request else {
        return Ok(None);
    };
    if sessions
        .list_turns(owner, session)?
        .iter()
        .any(|turn| turn.request_id == request)
    {
        return Ok(None);
    }
    Ok(Some(request))
}

fn is_continuation_cancel(event: &anchor_platform_session::SessionEvent, turn: &str) -> bool {
    event.kind == "pilot.continuation.cancelled"
        && event.data.get("turn").and_then(serde_json::Value::as_str) == Some(turn)
}

pub(crate) fn continuation_cancelled(
    sessions: &SessionStore,
    owner: &str,
    session: &str,
    turn: &str,
) -> Result<bool, SessionError> {
    Ok(sessions
        .events(owner, session, 0)?
        .iter()
        .any(|event| is_continuation_cancel(event, turn)))
}

pub(crate) fn continuation_status(
    sessions: &SessionStore,
    owner: &str,
    session: &str,
    turn: &str,
) -> Result<serde_json::Value, SessionError> {
    let request_id = format!("continuation:{turn}");
    let turns = sessions.list_turns(owner, session)?;
    if let Some(next) = turns.iter().find(|next| next.request_id == request_id) {
        return Ok(serde_json::json!({"status":"admitted","turn":next.id}));
    }
    let events = sessions.events(owner, session, 0)?;
    if events
        .iter()
        .any(|event| is_continuation_cancel(event, turn))
    {
        return Ok(serde_json::json!({"status":"cancelled","request_id":request_id}));
    }
    let requested = events.iter().any(|event| {
        event.kind == "pilot.continuation.requested"
            && event.data.get("turn").and_then(serde_json::Value::as_str) == Some(turn)
    });
    if !requested {
        return Ok(serde_json::json!({"status":"none"}));
    }
    let current = sessions.get_turn(owner, session, turn)?;
    let failed = events.iter().any(|event| {
        event.kind == "pilot.continuation.failed"
            && event.data.get("turn").and_then(serde_json::Value::as_str) == Some(turn)
    });
    let status = if current.status == TurnStatus::Stopped
        || sessions.get(owner, session)?.status == SessionStatus::Archived
    {
        "cancelled"
    } else if turns.first().is_none_or(|latest| latest.id != turn) {
        "superseded"
    } else if matches!(current.status, TurnStatus::Interrupted | TurnStatus::Failed) || failed {
        "pending"
    } else {
        "scheduled"
    };
    Ok(serde_json::json!({"status":status,"request_id":request_id}))
}

async fn execute_turn(
    goose: crate::goose_acp::pilot::GoosePilot,
    request: crate::goose_acp::pilot::PilotTurn,
    mut tools: PilotTools,
) -> Result<TurnStatus, crate::goose_acp::pilot::PilotError> {
    if request.cancellation.load(Ordering::Acquire) {
        return Ok(TurnStatus::Stopped);
    }
    let session = tools
        .sessions
        .get(&request.owner, &request.session)
        .map_err(|e| e.to_string())?;
    let turn = tools
        .sessions
        .get_turn(&request.owner, &request.session, &request.turn)
        .map_err(|e| e.to_string())?;
    let readonly = crate::pilot_attachments::readonly_inputs(
        &tools.data_root,
        &tools.sessions,
        &request.owner,
        &session,
        &turn,
    )?;
    let mut context = crate::pilot_tools::PilotTurnContext::new(
        &tools.application.bwrap,
        tools.application.library_root_override.as_deref(),
        &tools.workspace_root,
        &request.owner,
        &request.session,
        false,
        request.cancellation.clone(),
    )?;
    // Startup belongs to the registered execution, not admission: no global
    // gate is held while OAuth or MCP is waiting.
    let runtime = tools
        .application
        .runtime_snapshot()
        .map_err(|error| format!("{error:?}"))?;
    if let Some(snapshot) = &runtime {
        tools.application = tools
            .application
            .clone()
            .with_frozen_runtime_snapshot(snapshot.clone());
    }
    let (plugin_tools, network, disclosure) = match crate::pilot_tools::frozen_turn_tools(
        &tools.application.bwrap,
        tools.application.library_root_override.as_deref(),
        &tools.data_root,
        &tools.library_root,
        &tools.workspace_root,
        &request.owner,
        &request.session,
        &request.turn,
        request.cancellation.clone(),
        runtime.as_ref(),
        &readonly,
    )
    .await
    {
        Ok(prepared) => {
            for reason in prepared.reasons {
                eprintln!(
                    "anchor-runner-host: Pilot Turn {} Plugin: {reason}",
                    request.turn
                );
            }
            (prepared.ports, prepared.network, prepared.disclosure)
        }
        Err(reason) => {
            if request.cancellation.load(Ordering::Acquire) {
                return if reason == "Pilot tool startup was cancelled" {
                    Ok(TurnStatus::Stopped)
                } else {
                    Err(reason.into())
                };
            }
            eprintln!(
                "anchor-runner-host: Pilot Turn {} has no Plugin tools: {reason}",
                request.turn
            );
            (
                Vec::new(),
                false,
                crate::plugin_disclosure::PluginDisclosureCatalog::default(),
            )
        }
    };
    let outcome = if request.cancellation.load(Ordering::Acquire) {
        Ok(TurnStatus::Stopped)
    } else {
        context.set_readonly_inputs(readonly)?;
        context.freeze_network(network);
        if let Some(snapshot) = runtime.as_ref() {
            context.configure(snapshot, &tools.library_root, &request.root)?;
        }
        tools.context = Some(Arc::new(context));
        let execution_plugins = plugin_tools
            .iter()
            .cloned()
            .map(|port| port as Arc<dyn anchor_runtime::ToolPort>)
            .collect();
        goose
            .run(request, Arc::new(tools), execution_plugins, disclosure)
            .await
    };
    // Goose has closed its bridge and released the dispatch references. Reap
    // every admitted Plugin before finish can release this Session for another
    // Turn or deletion, including cancellation after startup returned its ports.
    let mut cleanup = Vec::new();
    for port in plugin_tools {
        match Arc::try_unwrap(port) {
            Ok(port) => {
                if let Err(reason) = port.shutdown().await {
                    cleanup.push(reason);
                }
            }
            Err(_) => cleanup.push("MCP port still has an active reference".into()),
        }
    }
    if cleanup.is_empty() {
        outcome
    } else {
        Err(format!("Pilot Plugin cleanup failed: {}", cleanup.join("; ")).into())
    }
}

const INSTRUCTIONS: &str = "你是 Anchor Pilot，帮助用户理解和管理 Graph、Plugin、Run 与产物。先回答实际问题，只读取当前问题相关资源；讨论能力不等于授权操作，只有用户明确要求才创建/更新 Graph、启动或控制 Run。\n\n按需发现指导：先从当前能力摘要和 anchor_plugins 找已挂载 Plugin，用 anchor_plugin_read 读取其 instructions 或 Skill 目录，再选择当前任务需要的 Skill；不一次读取全部正文。工具定义以实际披露的 schema 为准。开发指导未挂载时，用 plugin_list/plugin_read 查询已安装 Library，建议用户启用相关 Plugin，或按用户要求用 plugin_mount 发起既有原生确认；管理读取不授予执行权限。Plugin 安装与挂载分开，不暗中启用其他 Plugin。纯 Skill 开关只控制指导披露，不代表撤销命令授权、清空工作区或改变 runtime。无指导时也不得虚构工具或权限。\n\n宿主底线：anchor_read/anchor_edit 的 path 与候选工具的 directory 相对本 Session /workspace，跨 Turn 保留；anchor_run 只执行宿主已授权命令。空环境可用 platform_status 核查，以实际工具返回为准。不能修改/删除定义自己的 Graph，也不能安装替换为自己提供指令的 Plugin，交操作员维护。\n\n权限与能力在当前 Turn 冻结。需要联网用 session_network，安装用 plugin_install，挂载用 plugin_mount，均保留原生确认，模型不能传确认值，取消不执行；候选声明不能给自己授权。环境、挂载或授权变化后结束当前 Turn，由 Host 在同一 Session 自动续接，再使用重新冻结的能力。续接不是新业务请求，不能重做已完成操作，结果未知先核查现场。\n\n凭据只走 credential_request 的专用控件，不经 ask_user、聊天、命令或文件索取、读取或回显值。OAuth 先 oauth_status，再按 oauth_authorize 返回链接引导浏览器授权；Host 保存配置并自动续接，只根据状态继续。渠道先 channel_status；用户要求时才用 channel_configure/channel_enable 的原生确认，所需凭据也用专用控件。\n\n保存 Graph 不等于启动，graph_run 返回 Run ID 仅表示接纳；通过 run_status 核查完成。暂停和停止在安全边界生效，停止仍可恢复；永久收束走 run_abandon 的原生确认，保留历史、workspace 与产物。删除只用 graph_delete，确认绑定精确 Graph 内容；取消不删除，内容变化需重新确认。需要补充信息用 ask_user 原生表单。未提供的操作（包括对外发布）不得声称完成。\n\n事实与恢复：只有真实工具结果证明动作，区分准备、接纳、执行和业务验证；无法获取的事实如实说明。恢复或续聊先核查已保存事实及 session_wait/run_status/graph_read，不重放未知结果、旧确认或已完成副作用。准确用 #anchor/graph/<id>、#anchor/run/<id>、#anchor/artifact/<run>/<node>/<path> 引用实际对象，不编造 ID。";
