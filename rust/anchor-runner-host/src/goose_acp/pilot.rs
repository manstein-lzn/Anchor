use super::{
    bridge::Bridge, configuration, elicitation::PilotElicitation, pilot_interaction, session,
    transport::AcpConnection,
};
use crate::pilot_tools::PilotTools;
use anchor_platform_session::{GooseExecution, SessionStore, TurnStatus};
use anchor_runtime_rig::{Cancellation, ToolDefinition, ToolError, ToolPort, ToolResultContent};
use anchor_sandbox_bwrap::{BubblewrapPolicy, BubblewrapSandbox};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    future::Future,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

pub(crate) struct GoosePilot {
    binary: PathBuf,
    binary_sha256: String,
    models: configuration::ModelRegistry,
}

const MAX_FACT_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) struct PilotTurn {
    pub(crate) root: PathBuf,
    pub(crate) sessions: SessionStore,
    pub(crate) owner: String,
    pub(crate) session: String,
    pub(crate) turn: String,
    pub(crate) prompt: String,
    pub(crate) instructions: String,
    pub(crate) cancellation: Cancellation,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PilotFact {
    version: u32,
    binary_sha256: String,
    model_binding: String,
    session_id: Option<String>,
    tool_observation: Option<Value>,
}

fn save(root: &Path, fact: &PilotFact) -> Result<(), String> {
    let temporary = root.join("goose.json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&temporary)
        .map_err(|_| "Goose Pilot fact could not be opened safely")?;
    file.write_all(&serde_json::to_vec(fact).map_err(|error| error.to_string())?)
        .and_then(|_| file.sync_all())
        .map_err(|_| "Goose Pilot fact could not be persisted")?;
    fs::rename(temporary, root.join("goose.json")).map_err(|error| error.to_string())?;
    File::open(root)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

fn load(root: &Path) -> Result<Option<PilotFact>, String> {
    if let Ok(metadata) = fs::symlink_metadata(root.join("goose.json"))
        && (!metadata.is_file() || metadata.len() > MAX_FACT_BYTES)
    {
        return Err("Goose Pilot fact must be a bounded regular file".into());
    }
    match fs::read(root.join("goose.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "Goose Pilot fact is malformed".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("Goose Pilot fact is unavailable".into()),
    }
}

pub(super) fn directory(root: &Path) -> Result<(), String> {
    use rustix::fs::{Mode, OFlags, fsync, mkdirat, open, openat};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut parent = open("/", flags, Mode::empty()).map_err(|_| "Pilot directory unavailable")?;
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                match mkdirat(&parent, name, Mode::RWXU) {
                    Ok(()) => {
                        fsync(&parent).map_err(|_| "Pilot directory could not be persisted")?
                    }
                    Err(error) if error == rustix::io::Errno::EXIST => {}
                    Err(_) => return Err("Pilot directory unavailable".into()),
                }
                parent = openat(&parent, name, flags, Mode::empty())
                    .map_err(|_| "Pilot directory must not contain symlinks")?;
            }
            _ => return Err("Pilot directory must be an absolute host path".into()),
        }
    }
    Ok(())
}

impl GoosePilot {
    pub(crate) fn from_env(root: &Path, retained: Option<&GooseExecution>) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err("Pilot root must be absolute".into());
        }
        if root.join("framework.sqlite3").exists() || root.join("session.json").exists() {
            return Err("legacy Pilot history cannot silently become a Goose Session".into());
        }
        let (binary, binary_sha256) = configuration::binary()?;
        let models = configuration::ModelRegistry::from_env(false)?;
        let binding = models.resolve(None)?;
        let fact = load(root)?;
        validate_binding(fact.as_ref(), retained, &binary_sha256, &binding.identity)?;
        Ok(Self {
            binary,
            binary_sha256,
            models,
        })
    }

    pub(crate) async fn run(
        self,
        request: PilotTurn,
        tools: Arc<PilotTools>,
    ) -> Result<TurnStatus, String> {
        directory(&request.root)?;
        fs::set_permissions(&request.root, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(request.root.join("execution.lock"))
            .map_err(|_| "Pilot execution lease unavailable")?;
        rustix::fs::flock(&lease, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| "Pilot Session is already executing")?;
        let binding = self.models.resolve(None)?;
        let fact = load(&request.root)?.unwrap_or(PilotFact {
            version: 1,
            binary_sha256: self.binary_sha256.clone(),
            model_binding: binding.identity.clone(),
            session_id: None,
            tool_observation: None,
        });
        save(&request.root, &fact)?;
        let fact = Arc::new(Mutex::new(fact));
        let request = Arc::new(request);
        let guarded = Arc::new(PilotPort {
            tools,
            request: request.clone(),
            fact: fact.clone(),
            sequence: AtomicU64::new(0),
        });
        let mut random = [0u8; 32];
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(|_| "Pilot MCP authentication unavailable")?;
        let token = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let tool_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let bridge = Bridge::start_pilot(guarded, tool_cancel, token).await?;
        let result = self.execute(&request, &fact, &bridge, &binding).await;
        let close = bridge.close().await;
        drop(lease);
        close?;
        if request.cancellation.load(Ordering::Acquire) {
            return Ok(TurnStatus::Stopped);
        }
        result
    }

    async fn execute(
        &self,
        request: &Arc<PilotTurn>,
        fact: &Mutex<PilotFact>,
        bridge: &Bridge,
        binding: &configuration::ModelBinding,
    ) -> Result<TurnStatus, String> {
        let directory = request.root.join("process");
        self::directory(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let sandbox = BubblewrapSandbox::new(
            BubblewrapPolicy::new(
                std::env::var_os("ANCHOR_BWRAP")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| "bwrap".into()),
                ["goose"],
            )
            .authorize_workspace_root(&directory)
            .allow_network(),
        )
        .map_err(|error| error.to_string())?;
        let command = configuration::command(
            &sandbox,
            &directory,
            &self.binary,
            self.models.environment(binding, &bridge.url, &bridge.token),
            request.cancellation.clone(),
        )?;
        let mut connection = AcpConnection::spawn(command).await?;
        let result = self.prompt(request, fact, bridge, &mut connection).await;
        if result.is_err() {
            let session = fact
                .lock()
                .map_err(|_| "Pilot fact lock unavailable")?
                .session_id
                .clone();
            if let Some(session) = session {
                let _ = connection
                    .notify("session/cancel", json!({"sessionId":session}))
                    .await;
            }
        }
        let close = connection.close().await;
        close?;
        if request.cancellation.load(Ordering::Acquire) {
            return Ok(TurnStatus::Stopped);
        }
        result
    }

    async fn prompt(
        &self,
        request: &Arc<PilotTurn>,
        fact: &Mutex<PilotFact>,
        bridge: &Bridge,
        connection: &mut AcpConnection,
    ) -> Result<TurnStatus, String> {
        let restored = fact
            .lock()
            .map_err(|_| "Pilot fact lock unavailable")?
            .session_id
            .clone();
        let opened = session::open(
            connection,
            bridge,
            restored.as_deref(),
            &request.cancellation,
            None,
            true,
            true,
        )
        .await?;
        let observation = {
            let mut fact = fact.lock().map_err(|_| "Pilot fact lock unavailable")?;
            fact.session_id = Some(opened.id.clone());
            save(&request.root, &fact)?;
            fact.tool_observation.clone()
        };
        let scope = request
            .root
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("Pilot scope identity unavailable")?;
        request
            .sessions
            .bind_goose(
                &request.owner,
                &request.session,
                &request.turn,
                GooseExecution {
                    scope: scope.into(),
                    session: opened.id.clone(),
                },
            )
            .map_err(|_| "Pilot Goose association persistence failed")?;
        connection.set_elicitation_handler(Arc::new(PilotElicitation {
            request: request.clone(),
            native_session: opened.id.clone(),
        }));
        emit(request, json!({"type":"text-start","id":request.turn}))?;
        let callback = |frame: &Value| {
            if frame["method"] == "session/update" {
                if frame["params"]["sessionId"] != opened.id {
                    return Err("ACP update belongs to another Pilot Session".into());
                }
                let update = &frame["params"]["update"];
                if update["sessionUpdate"] == "agent_message_chunk"
                    && let Some(text) = update["content"]["text"].as_str()
                {
                    emit(
                        request,
                        json!({"type":"text-delta","id":request.turn,"delta":text}),
                    )?;
                }
            }
            Ok(())
        };
        let prompt = format!(
            "{}\nOnly use the authorized anchor MCP tools; no node final_result is available. Continue the same native Session. Missing tool results do not prove an operation did not occur; inspect saved facts and actual state before repeating any uncertain action. Latest durable Anchor tool observation: {}\nUser message: {}",
            request.instructions,
            json!(observation),
            request.prompt
        );
        let (response, _) = connection
            .request_observed(
                "session/prompt",
                json!({"sessionId":opened.id,"prompt":[{"type":"text","text":prompt}]}),
                &request.cancellation,
                None,
                &callback,
            )
            .await?;
        if response["stopReason"] != "end_turn" {
            return Err("Goose Pilot did not finish the Turn".into());
        }
        emit(request, json!({"type":"text-end","id":request.turn}))?;
        Ok(TurnStatus::Completed)
    }
}

fn validate_binding(
    fact: Option<&PilotFact>,
    retained: Option<&GooseExecution>,
    binary_sha256: &str,
    model_binding: &str,
) -> Result<(), String> {
    if fact.is_some_and(|fact| {
        fact.version != 1
            || fact.binary_sha256 != binary_sha256
            || fact.model_binding != model_binding
    }) {
        return Err("Goose Pilot binary or model binding changed".into());
    }
    if retained.is_some_and(|retained| {
        fact.and_then(|fact| fact.session_id.as_deref()) != Some(retained.session.as_str())
    }) {
        return Err("retained Goose Pilot identity has no matching durable Session fact".into());
    }
    Ok(())
}

fn emit(request: &PilotTurn, event: Value) -> Result<(), String> {
    request
        .sessions
        .append_turn_event(&request.owner, &request.session, &request.turn, event)
        .map(|_| ())
        .map_err(|_| "Pilot event persistence failed".into())
}

pub(super) struct PilotPort {
    tools: Arc<PilotTools>,
    request: Arc<PilotTurn>,
    fact: Arc<Mutex<PilotFact>>,
    sequence: AtomicU64,
}

impl ToolPort for PilotPort {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.tools.definitions();
        definitions.extend(pilot_interaction::definitions());
        definitions
    }
    fn is_read_only(&self, name: &str) -> bool {
        self.tools.is_read_only(name)
    }
    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(self.call_mcp(name, arguments, None))
    }
}

impl PilotPort {
    pub(super) async fn call_mcp(
        &self,
        name: &str,
        arguments: Value,
        peer: Option<rmcp::Peer<rmcp::RoleServer>>,
    ) -> Result<Vec<ToolResultContent>, ToolError> {
        let failed = |reason: String| ToolError::Failed(reason);
        if self.request.cancellation.load(Ordering::Acquire) {
            return Err(failed("Pilot was cancelled".into()));
        }
        if !self
            .definitions()
            .iter()
            .any(|definition| definition.name == name)
        {
            return Err(ToolError::Unknown(name.into()));
        }
        let id = format!(
            "{}-{}",
            self.request.turn,
            self.sequence.fetch_add(1, Ordering::Relaxed)
        );
        let mut observation = json!({"tool":name,"arguments":arguments,"result":null});
        {
            let mut fact = self
                .fact
                .lock()
                .map_err(|_| failed("Pilot fact lock unavailable".into()))?;
            fact.tool_observation = Some(observation.clone());
            save(&self.request.root, &fact).map_err(failed)?;
        }
        emit(
            &self.request,
            json!({"type":"tool-input-available","toolCallId":id,
                "toolName":name,"input":arguments}),
        )
        .map_err(failed)?;
        let result = if matches!(name, "ask_user" | "graph_delete") {
            match peer {
                Some(peer) => {
                    pilot_interaction::call(
                        &self.tools,
                        name,
                        arguments,
                        peer,
                        &self.request.cancellation,
                    )
                    .await
                }
                None => Err(failed(
                    "interactive tools require native MCP elicitation".into(),
                )),
            }
        } else {
            self.tools.call(name, arguments).await
        };
        let event = match &result {
            Ok(output) => {
                observation["result"] = json!({"ok":true,"output":output});
                json!({"type":"tool-output-available","toolCallId":id,"output":output})
            }
            Err(error) => {
                observation["result"] = json!({"ok":false,"error":error.to_string()});
                json!({"type":"tool-output-error","toolCallId":id,"errorText":error.to_string()})
            }
        };
        {
            let mut fact = self
                .fact
                .lock()
                .map_err(|_| failed("Pilot fact lock unavailable".into()))?;
            fact.tool_observation = Some(observation);
            save(&self.request.root, &fact).map_err(failed)?;
        }
        emit(&self.request, event).map_err(failed)?;
        result
    }
}

pub(crate) fn messages(
    sessions: &SessionStore,
    owner: &str,
    session: &str,
) -> Result<Vec<Value>, String> {
    let turns = sessions
        .list_turns(owner, session)
        .map_err(|_| "Pilot history unavailable")?;
    let mut messages = Vec::new();
    for turn in turns.into_iter().rev() {
        if let Some(prompt) = turn.prompt {
            messages.push(json!({"role":"user","text":prompt}));
        }
        let events = sessions
            .turn_events(owner, session, &turn.id, 0)
            .map_err(|_| "Pilot history unavailable")?;
        let text = events
            .iter()
            .filter(|event| event.data["type"] == "text-delta")
            .filter_map(|event| event.data["delta"].as_str())
            .collect::<String>();
        if !text.is_empty() {
            messages.push(json!({"role":"assistant","text":text}));
        }
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projected_messages_remain_chronological_across_multiple_turns() {
        let root = tempfile::tempdir().unwrap();
        let sessions = SessionStore::open(root.path().join("sessions.sqlite")).unwrap();
        sessions
            .create(
                "local",
                anchor_platform_session::CreateSession {
                    id: Some("pilot".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        for index in 1..=3 {
            let (turn, _) = sessions
                .create_turn(
                    "local",
                    "pilot",
                    &format!("request-{index}"),
                    Some(&format!("question-{index}")),
                )
                .unwrap();
            sessions
                .append_turn_event(
                    "local",
                    "pilot",
                    &turn.id,
                    json!({"type":"text-delta","delta":format!("answer-{index}")}),
                )
                .unwrap();
            sessions
                .finish_turn("local", "pilot", &turn.id, TurnStatus::Completed, None)
                .unwrap();
        }
        assert_eq!(
            messages(&sessions, "local", "pilot").unwrap(),
            vec![
                json!({"role":"user","text":"question-1"}),
                json!({"role":"assistant","text":"answer-1"}),
                json!({"role":"user","text":"question-2"}),
                json!({"role":"assistant","text":"answer-2"}),
                json!({"role":"user","text":"question-3"}),
                json!({"role":"assistant","text":"answer-3"}),
            ]
        );
    }

    fn fact() -> PilotFact {
        PilotFact {
            version: 1,
            binary_sha256: "binary".into(),
            model_binding: "model".into(),
            session_id: Some("native-session".into()),
            tool_observation: None,
        }
    }

    #[test]
    fn private_fact_is_durable_and_symlink_replacement_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        save(root.path(), &fact()).unwrap();
        assert_eq!(
            load(root.path()).unwrap().unwrap().session_id,
            Some("native-session".into())
        );
        assert_eq!(
            fs::metadata(root.path().join("goose.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let target = root.path().join("untouched");
        fs::write(&target, "untouched").unwrap();
        std::os::unix::fs::symlink(&target, root.path().join("goose.json.tmp")).unwrap();
        assert!(save(root.path(), &fact()).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
    }

    #[test]
    fn bounded_fact_and_directory_traversal_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.path().join("link")).unwrap();
        assert!(directory(&root.path().join("link/child")).is_err());
        assert!(!outside.join("child").exists());
        let oversized = File::create(root.path().join("goose.json")).unwrap();
        oversized.set_len(MAX_FACT_BYTES + 1).unwrap();
        assert!(load(root.path()).is_err());
        fs::remove_file(root.path().join("goose.json")).unwrap();
        std::os::unix::fs::symlink(oversized_path(root.path()), root.path().join("goose.json"))
            .unwrap();
        assert!(load(root.path()).is_err());
    }

    fn oversized_path(root: &Path) -> PathBuf {
        let path = root.join("other.json");
        fs::write(&path, "{}").unwrap();
        path
    }

    #[test]
    fn retained_session_cannot_be_replaced_after_its_fact_disappears() {
        let retained = GooseExecution {
            scope: "a".repeat(64),
            session: "native-session".into(),
        };
        assert!(validate_binding(Some(&fact()), Some(&retained), "binary", "model").is_ok());
        assert!(validate_binding(None, Some(&retained), "binary", "model").is_err());
        let mut changed = fact();
        changed.session_id = Some("replacement-session".into());
        assert!(validate_binding(Some(&changed), Some(&retained), "binary", "model").is_err());
        assert!(
            validate_binding(Some(&fact()), Some(&retained), "binary", "changed-model").is_err()
        );
    }

    #[test]
    fn full_artifact_preview_survives_json_escaping_in_latest_tool_fact() {
        let root = tempfile::tempdir().unwrap();
        let mut fact = fact();
        fact.tool_observation = Some(json!({"result":"\u{0001}".repeat(1024*1024)}));
        save(root.path(), &fact).unwrap();
        assert_eq!(
            load(root.path()).unwrap().unwrap().tool_observation,
            fact.tool_observation
        );
    }
}
