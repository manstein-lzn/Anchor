use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::pilot_tools::PilotTools;
#[cfg(feature = "legacy-regression")]
use anchor_io_harness_runtime::{
    adapter::RigProviderAdapter,
    pilot::{PilotObserver, PilotRequest, PilotStatus, run_pilot},
};
#[cfg(feature = "legacy-regression")]
use anchor_platform_session::NativeExecution;
use anchor_platform_session::{
    Session, SessionError, SessionStatus, SessionStore, Turn, TurnStatus,
};
#[cfg(feature = "legacy-regression")]
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(feature = "legacy-regression")]
use std::time::Duration;

enum PilotBackend {
    #[cfg(feature = "legacy-regression")]
    Legacy(RigProviderAdapter),
    Goose(crate::goose_acp::pilot::GoosePilot),
}

#[derive(Clone, Default)]
pub(crate) struct PilotService {
    gate: Arc<tokio::sync::Mutex<()>>,
    active: Arc<Mutex<HashMap<PathBuf, Arc<AtomicBool>>>>,
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
    pub(crate) root: PathBuf,
    pub(crate) tools: PilotTools,
}

pub(crate) enum AdmissionError {
    Session(SessionError),
    Provider,
}

impl From<SessionError> for AdmissionError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

#[cfg(feature = "legacy-regression")]
struct DeliveryObserver {
    sessions: SessionStore,
    owner: String,
    session: String,
    turn: String,
    scope: String,
    cancellation: Arc<AtomicBool>,
}

#[cfg(feature = "legacy-regression")]
impl PilotObserver for DeliveryObserver {
    fn event(&self, chunk: Value) -> Result<(), String> {
        self.sessions
            .append_turn_event(&self.owner, &self.session, &self.turn, chunk)
            .map(|_| ())
            .map_err(|_| "Pilot event persistence failed".into())
    }

    fn cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }

    fn native_run(&self, session: i64, run: i64) -> Result<(), String> {
        self.sessions
            .bind_native(
                &self.owner,
                &self.session,
                &self.turn,
                NativeExecution {
                    scope: self.scope.clone(),
                    session,
                    run,
                },
            )
            .map(|_| ())
            .map_err(|_| "Pilot native association persistence failed".into())
    }
}

impl PilotService {
    pub(crate) async fn admit(&self, admission: PilotAdmission) -> Result<Turn, AdmissionError> {
        let _gate = self.gate.lock().await;
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
        if !existing && admission.session.status == SessionStatus::Archived {
            return Err(
                SessionError::Conflict("archived sessions cannot accept turns".into()).into(),
            );
        }
        let backend = if existing {
            None
        } else {
            let turns = admission
                .sessions
                .list_turns(&admission.owner, &admission.session.id)?;
            let runtime = crate::goose_acp::runtime_mode();
            Some(match runtime.as_str() {
                "goose" => {
                    if turns.iter().any(|turn| turn.native.is_some()) {
                        return Err(SessionError::Conflict(
                            "legacy Pilot turns cannot silently switch to Goose".into(),
                        )
                        .into());
                    }
                    PilotBackend::Goose(
                        crate::goose_acp::pilot::GoosePilot::from_env(
                            &admission.root,
                            turns.iter().find_map(|turn| turn.goose.as_ref()),
                        )
                        .map_err(|_| AdmissionError::Provider)?,
                    )
                }
                #[cfg(feature = "legacy-regression")]
                "io-harness" => {
                    if admission.root.join("goose.json").exists()
                        || turns.iter().any(|turn| turn.goose.is_some())
                    {
                        return Err(SessionError::Conflict(
                            "Goose Pilot turns cannot silently switch to legacy".into(),
                        )
                        .into());
                    }
                    let registry = crate::model_registry::from_env()
                        .map_err(|_| AdmissionError::Provider)?
                        .ok_or(AdmissionError::Provider)?;
                    PilotBackend::Legacy(RigProviderAdapter::new(
                        registry.model(None),
                        registry.accepts_images(None),
                    ))
                }
                _ => return Err(AdmissionError::Provider),
            })
        };
        let sessions = admission.sessions.clone();
        let owner = admission.owner.clone();
        let session = admission.session.id.clone();
        let request_id = admission.request_id.clone();
        let (turn, created) = tokio::task::spawn_blocking(move || {
            sessions.create_turn(&owner, &session, &request_id, prompt.as_deref())
        })
        .await
        .map_err(|_| SessionError::Storage("Pilot admission failed".into()))??;
        if !created {
            return Ok(turn);
        }
        let backend = backend
            .ok_or_else(|| SessionError::Storage("Pilot admission identity changed".into()))?;
        let cancellation = Arc::new(AtomicBool::new(false));
        self.active
            .lock()
            .map_err(|_| SessionError::Storage("Pilot control failed".into()))?
            .insert(admission.root.clone(), cancellation.clone());
        #[cfg(feature = "legacy-regression")]
        let observer = Arc::new(DeliveryObserver {
            sessions: admission.sessions.clone(),
            owner: admission.owner.clone(),
            session: admission.session.id.clone(),
            turn: turn.id.clone(),
            scope: admission
                .root
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| SessionError::Storage("Pilot scope identity is invalid".into()))?
                .into(),
            cancellation: cancellation.clone(),
        });
        let turn_id = turn.id.clone();
        let execution_turn = turn_id.clone();
        let execution_sessions = admission.sessions.clone();
        let execution_owner = admission.owner.clone();
        let execution_session = admission.session.id.clone();
        let tools = Arc::new(admission.tools.for_turn(&turn_id));
        let service = self.clone();
        tokio::spawn(async move {
            let root = admission.root.clone();
            let result = tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
                    .map_err(|_| "Pilot executor could not start".to_owned())?;
                runtime.block_on(async move { match backend {
                    PilotBackend::Goose(goose) => goose.run(crate::goose_acp::pilot::PilotTurn {
                        root:admission.root,
                        sessions:execution_sessions,owner:execution_owner,
                        session:execution_session,turn:execution_turn.clone(),
                        prompt:admission.prompt.unwrap_or_else(|| "继续这个会话。先核查中断前的事实和现场，不要重放结果未知的操作。".into()),
                        instructions:INSTRUCTIONS.into(),cancellation,
                    },tools).await.map(|status| (status,None::<String>)),
                    #[cfg(feature = "legacy-regression")]
                    PilotBackend::Legacy(provider) => {
                        let provider = provider.with_recording(admission.root.join("providers").join(&execution_turn).join("provider.recordings"));
                        run_pilot(PilotRequest {
                    root: admission.root,
                    prompt: admission.prompt.unwrap_or_else(|| "继续这个会话。先核查中断前的事实和现场，不要重放结果未知的操作；必要时询问用户。".into()),
                    instructions: INSTRUCTIONS.into(),
                    max_steps: 12,
                    max_tokens: 24_000,
                    wall_time: Duration::from_secs(90),
                        }, &provider, tools, observer).await.map(|outcome| (match outcome.status {
                            PilotStatus::Completed => TurnStatus::Completed,
                            PilotStatus::Stopped => TurnStatus::Stopped,
                            PilotStatus::Interrupted => TurnStatus::Interrupted,
                            PilotStatus::Failed => TurnStatus::Failed,
                        }, outcome.error))
                    }
                }})
            }).await;
            let (status, failure) = match result {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(message)) => {
                    eprintln!("Pilot executor failed: {message}");
                    (TurnStatus::Interrupted, Some("Pilot execution interrupted; continue with a new message to inspect the saved facts".into()))
                }
                _ => (TurnStatus::Interrupted, Some("Pilot execution interrupted; continue with a new message to inspect the saved facts".into())),
            };
            let _gate = service.gate.lock().await;
            let saved = tokio::task::spawn_blocking(move || {
                admission.sessions.finish_turn(
                    &admission.owner,
                    &admission.session.id,
                    &turn_id,
                    status,
                    failure.as_deref(),
                )
            })
            .await;
            if !matches!(saved, Ok(Ok(_))) {
                eprintln!("Pilot terminal state could not be persisted");
            }
            if let Ok(mut active) = service.active.lock() {
                active.remove(&root);
            }
        });
        Ok(turn)
    }

    pub(crate) async fn stop(&self, root: &std::path::Path) -> bool {
        let _gate = self.gate.lock().await;
        if let Ok(active) = self.active.lock()
            && let Some(cancellation) = active.get(root)
        {
            cancellation.store(true, Ordering::Release);
            return true;
        }
        false
    }
}

const INSTRUCTIONS: &str = "你是 Anchor Pilot，帮助用户理解、管理 Graph、Plugin、Run 和产物。先回答用户实际问题，讨论能力不等于授权操作；只有用户明确要求才创建/更新 Graph、启动或控制 Run。只读取当前问题直接相关的资源，不搜索论文或遍历整个项目。保存 Graph 不等于启动 Graph；graph_run 仅接纳并返回 Run ID，不等于执行完成。暂停和停止是安全边界上的请求，应通过 run_status 核查最终状态。需要补充信息时使用 ask_user 原生表单提问，等待用户回答；删除只用 graph_delete，它会再次请求用户确认绑定的精确 Graph 内容。用户拒绝或取消不删除，内容变化需重新确认。本切片尚不提供对外发布，不得声称完成未提供的操作。只有真实工具结果才证明动作；无法获取的事实如实说明。恢复或续聊先核查保存的事实和 session_wait/run_status/graph_read，不盲目重放未知结果或旧确认。使用 #anchor/graph/<id>、#anchor/run/<id>、#anchor/artifact/<run>/<node>/<path> 链接引用对象。";
