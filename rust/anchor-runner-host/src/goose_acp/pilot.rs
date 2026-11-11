use super::{
    bridge::Bridge, configuration, elicitation::PilotElicitation, pilot_interaction, session,
    transport::AcpConnection,
};
use crate::pilot_tools::PilotTools;
use anchor_platform_session::{GooseExecution, SessionStore, TurnStatus};
use anchor_runtime::{Cancellation, ToolDefinition, ToolError, ToolPort, ToolResultContent};
use anchor_sandbox_bwrap::{BubblewrapPolicy, BubblewrapSandbox};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    future::Future,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

mod transcript;

pub(crate) struct GoosePilot {
    binary: PathBuf,
    bwrap: PathBuf,
    binary_sha256: String,
    models: configuration::ModelRegistry,
    /// How the Pilot sandbox reaches the bridge and the model proxy.
    relay: configuration::RelaySettings,
}

#[derive(Debug)]
pub(crate) enum PilotError {
    Interrupted(String),
    Provider(&'static str),
}

impl std::fmt::Display for PilotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interrupted(message) => formatter.write_str(message),
            Self::Provider(kind) => write!(
                formatter,
                "Model provider request failed ({kind}); inspect saved facts before retrying"
            ),
        }
    }
}

impl From<String> for PilotError {
    fn from(message: String) -> Self {
        Self::Interrupted(message)
    }
}

impl From<&str> for PilotError {
    fn from(message: &str) -> Self {
        Self::Interrupted(message.into())
    }
}

const MAX_FACT_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) struct PilotTurn {
    pub(crate) root: PathBuf,
    pub(crate) data_root: PathBuf,
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

pub(crate) fn execution_lease(root: &Path) -> Result<anchor_file_lease::FileLease, String> {
    crate::host_fs::directory(root)?;
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(root.join("execution.lock"))
        .map_err(|_| "Pilot execution lease unavailable")?;
    if !lease
        .metadata()
        .map_err(|_| "Pilot execution lease unavailable")?
        .is_file()
    {
        return Err("Pilot execution lease is not a regular file".into());
    }
    let lease = anchor_file_lease::FileLease::acquire(
        lease,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .map_err(|_| "Pilot Session is already executing")?;
    Ok(lease)
}

impl GoosePilot {
    pub(crate) fn from_state(
        root: &Path,
        retained: Option<&GooseExecution>,
        state: &Path,
        bwrap: PathBuf,
    ) -> Result<Self, String> {
        let models = crate::model_configuration::ModelConfiguration::from_env(state)?
            .execution_snapshot(None, None)?;
        Self::from_registry(root, retained, models, bwrap)
    }

    fn from_registry(
        root: &Path,
        retained: Option<&GooseExecution>,
        models: configuration::ModelRegistry,
        bwrap: PathBuf,
    ) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err("Pilot root must be absolute".into());
        }
        if root.join("framework.sqlite3").exists() || root.join("session.json").exists() {
            return Err("legacy Pilot history cannot silently become a Goose Session".into());
        }
        let (binary, binary_sha256) = configuration::binary()?;
        let fact = load(root)?;
        validate_binding(fact.as_ref(), retained, &binary_sha256)?;
        let relay = configuration::RelaySettings::from_env(&binary)?;
        Ok(Self {
            bwrap,
            binary,
            binary_sha256,
            models,
            relay,
        })
    }

    pub(crate) async fn run(
        self,
        request: PilotTurn,
        tools: Arc<PilotTools>,
        // The Plugin tools this Turn froze when it was admitted. Empty is the
        // normal case for a Session that mounted nothing.
        plugin_tools: Vec<Arc<dyn ToolPort>>,
        mut catalog: crate::plugin_disclosure::PluginDisclosureCatalog,
    ) -> Result<TurnStatus, PilotError> {
        crate::host_fs::directory(&request.root)?;
        fs::set_permissions(&request.root, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let lease = execution_lease(&request.root)?;
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
        let mut builtins = tools.definitions();
        builtins.extend(pilot_interaction::definitions());
        let visible = builtins
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        catalog.owners.retain(|name, _| !visible.contains(name));
        let definitions = plugin_definitions(builtins, &plugin_tools);
        let read_only = definitions
            .iter()
            .map(|definition| {
                let name = &definition.name;
                let readonly = tools.is_read_only(name)
                    || plugin_port(&plugin_tools, name).is_some_and(|port| port.is_read_only(name));
                (name.clone(), readonly)
            })
            .collect();
        let disclosure = crate::tool_disclosure::ToolDisclosure::new(
            definitions,
            read_only,
            crate::tool_disclosure::DisclosurePolicy::new(visible),
            catalog,
        );
        let guarded = Arc::new(PilotPort {
            tools,
            plugin_tools,
            request: request.clone(),
            fact: fact.clone(),
            sequence: AtomicU64::new(0),
            disclosure,
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
        let bridge = Bridge::start_pilot(
            guarded,
            tool_cancel,
            self.models.model_upstream(),
            self.models.fixture(),
            token,
        )
        .await?;
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
    ) -> Result<TurnStatus, PilotError> {
        let directory = request.root.join("process");
        crate::host_fs::directory(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        // Keep the socket path short: the kernel caps a UNIX socket path at `sun_path`.
        let socket = std::env::temp_dir().join(format!(
            "anchor-pilot-bridge-{}.sock",
            &format!("{:x}", Sha256::digest(directory.display().to_string()))[..16]
        ));
        bridge
            .expose_on_unix_socket(&socket)
            .map_err(|error| error.to_string())?;
        let transport = self.relay.transport(socket);
        let endpoint = transport.endpoint(&bridge.url);
        let sandbox = BubblewrapSandbox::new(
            BubblewrapPolicy::new(self.bwrap.clone(), ["goose"])
                .authorize_workspace_root(&directory)
                .allow_network(),
        )
        .map_err(|error| error.to_string())?;
        let mut environment = self.models.environment(binding, &endpoint, &bridge.token);
        environment.push(anchor_runtime::SandboxEnvironment::new(
            "GOOSE_MOIM_MESSAGE_TEXT",
            configuration::boundary_text(&configuration::BoundaryFacts {
                isolated: self.relay.is_isolated(),
                wall_clock: None,
                disclosure: crate::tool_disclosure::forced(),
            }),
        ));
        let command = configuration::command(
            &sandbox,
            &directory,
            &self.binary,
            environment,
            request.cancellation.clone(),
            &transport,
        )?;
        let mut connection = AcpConnection::spawn(command).await?;
        let result = self
            .prompt(request, fact, bridge, &endpoint, &mut connection)
            .await
            .map_err(|error| match connection.provider_failure() {
                Some(kind) => PilotError::Provider(kind),
                None => PilotError::Interrupted(error),
            });
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
        endpoint: &str,
        connection: &mut AcpConnection,
    ) -> Result<TurnStatus, String> {
        let restored = fact
            .lock()
            .map_err(|_| "Pilot fact lock unavailable")?
            .session_id
            .clone();
        let binding = self.models.resolve(None)?;
        let opened = session::open(
            connection,
            bridge,
            endpoint,
            restored.as_deref(),
            &request.instructions,
            &binding,
            &request.cancellation,
            None,
            session::ClientCapabilities {
                custom_notifications: true,
                form_elicitation: true,
            },
        )
        .await?;
        let observation = {
            let mut fact = fact.lock().map_err(|_| "Pilot fact lock unavailable")?;
            // A Goose binary upgrade may keep the same native Session.  We only
            // update the durable runtime binding after `session/load` has
            // succeeded above; a failed load therefore cannot silently replace
            // the retained Session.
            fact.binary_sha256 = self.binary_sha256.clone();
            fact.model_binding = binding.identity;
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
                    && update["content"]["type"] == "image"
                    && let (Some(data), Some(mime)) = (
                        update["content"]["data"].as_str(),
                        update["content"]["mimeType"].as_str(),
                    )
                {
                    let uploads = [crate::attachments::UploadedAttachment {
                        name: format!(
                            "assistant-{}.{}",
                            &format!("{:x}", Sha256::digest(data))[..16],
                            image_extension(mime)?
                        ),
                        data_base64: data.into(),
                        media_type: Some(mime.into()),
                    }];
                    let prepared = crate::attachments::prepare(&uploads)?;
                    crate::pilot_attachments::publish(
                        &request.data_root,
                        &request.sessions,
                        &request.owner,
                        &request.session,
                        &request.turn,
                        prepared,
                    )?;
                }
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
            "Only use the authorized anchor MCP tools; no node final_result is available. Continue the same native Session. Missing tool results do not prove an operation did not occur; inspect saved facts and actual state before repeating any uncertain action. Latest durable Anchor tool observation: {}\nUser message: {}",
            json!(observation),
            request.prompt
        );
        let session = request
            .sessions
            .get(&request.owner, &request.session)
            .map_err(|e| e.to_string())?;
        let turn = request
            .sessions
            .get_turn(&request.owner, &request.session, &request.turn)
            .map_err(|e| e.to_string())?;
        let inputs = crate::pilot_attachments::prepared_inputs(
            &request.data_root,
            &request.sessions,
            &request.owner,
            &session,
            &turn,
        )?;
        let refs = crate::pilot_attachments::input_references(
            &request.sessions,
            &request.owner,
            &request.session,
            &request.turn,
        )?;
        let prompt=format!("{prompt}\nCurrent Turn read-only attachments in /in/pilot (only these files are authorized for this Turn): {}. Read document bytes with anchor_read or anchor_run; use publish_file to deliver workspace files. Prior Session files are not automatically mounted.",json!(refs.iter().map(|item|json!({"name":item.name,"size":item.size,"sha256":item.sha256,"media_type":item.media_type})).collect::<Vec<_>>()));
        let mut blocks = vec![json!({"type":"text","text":prompt})];
        for (item, bytes) in inputs.0 {
            if let Some(mime) = item
                .media_type
                .as_deref()
                .filter(|mime| mime.starts_with("image/"))
            {
                blocks.push(super::media::image_prompt(&bytes, mime)?);
            }
        }
        let (response, _) = connection
            .request_observed(
                "session/prompt",
                json!({"sessionId":opened.id,"prompt":blocks}),
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
) -> Result<(), String> {
    if fact.is_some_and(|fact| fact.version != 1 || fact.model_binding.is_empty()) {
        return Err("Goose Pilot fact or model binding is invalid".into());
    }
    if retained.is_some_and(|retained| {
        fact.and_then(|fact| fact.session_id.as_deref()) != Some(retained.session.as_str())
    }) {
        return Err("retained Goose Pilot identity has no matching durable Session fact".into());
    }
    // The binary digest is an audit fact, while the opaque Goose Session is the
    // recovery cursor.  A deployment may replace a compatible Goose binary; the
    // successful `session/load` above rebinds the fact.  A fact without a
    // retained Session still cannot be used to invent a replacement cursor.
    if fact.is_some_and(|fact| fact.binary_sha256 != binary_sha256) && retained.is_none() {
        return Err("Goose Pilot binary changed without a retained Session identity".into());
    }
    Ok(())
}

fn image_extension(mime: &str) -> Result<&'static str, String> {
    match mime {
        "image/png" => Ok("png"),
        "image/jpeg" => Ok("jpg"),
        "image/webp" => Ok("webp"),
        _ => Err("unsupported Pilot image MIME".into()),
    }
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
    /// The Plugin tool ports this Turn froze when it was admitted. They are part
    /// of the Pilot's tool list, never rebuilt from live Session state: a click
    /// made during this Turn must not change what this Turn can call.
    plugin_tools: Vec<Arc<dyn ToolPort>>,
    request: Arc<PilotTurn>,
    fact: Arc<Mutex<PilotFact>>,
    sequence: AtomicU64,
    disclosure: crate::tool_disclosure::ToolDisclosure,
}

/// The Pilot's tool list: the built-in tools, then the Plugin tools this Turn
/// froze. A Plugin tool may never shadow a built-in one, so the built-ins win.
pub(crate) fn plugin_definitions(
    mut definitions: Vec<ToolDefinition>,
    plugin_tools: &[Arc<dyn ToolPort>],
) -> Vec<ToolDefinition> {
    for port in plugin_tools {
        for definition in port.definitions() {
            if !definitions
                .iter()
                .any(|existing| existing.name == definition.name)
            {
                definitions.push(definition);
            }
        }
    }
    definitions
}

/// The frozen Plugin port that owns this tool name, if any.
pub(crate) fn plugin_port<'ports>(
    plugin_tools: &'ports [Arc<dyn ToolPort>],
    name: &str,
) -> Option<&'ports Arc<dyn ToolPort>> {
    plugin_tools.iter().find(|port| {
        port.definitions()
            .iter()
            .any(|definition| definition.name == name)
    })
}

impl ToolPort for PilotPort {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.disclosure.definitions()
    }
    fn is_read_only(&self, name: &str) -> bool {
        self.disclosure.is_read_only(name)
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
    pub(super) fn instructions(&self) -> String {
        self.disclosure.instructions()
    }

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
        let (name, arguments) = self.disclosure.resolve(name, arguments)?;
        let name = name.as_str();
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
        // One authority table decides, so a new confirmed tool cannot be added to
        // the definition list and forgotten here.
        let result = if crate::tool_disclosure::is_meta(name) {
            self.disclosure.inspect(name, &arguments)
        } else if crate::pilot_tools::authority(name)
            == Some(crate::pilot_tools::ToolAuthority::Confirm)
        {
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
        } else if let Some(port) = plugin_port(&self.plugin_tools, name) {
            // A frozen Plugin tool: the human's mount is its authorization, so it
            // is called like the Pilot's own Mutate tools.
            port.call(name, arguments).await
        } else {
            self.tools.call(name, arguments).await
        };
        let mut display = Value::Null;
        let result = result.and_then(|output| {
            let encoded = super::media::encode(output.clone()).map_err(failed)?;
            display = encoded.observation;
            for (index, (bytes, mime)) in encoded.images.into_iter().enumerate() {
                let prepared = crate::attachments::from_bytes(
                    format!(
                        "tool-{}-{index}.{}",
                        id,
                        image_extension(&mime).map_err(failed)?
                    ),
                    bytes,
                    Some(&mime),
                )
                .map_err(failed)?;
                crate::pilot_attachments::publish(
                    &self.request.data_root,
                    &self.request.sessions,
                    &self.request.owner,
                    &self.request.session,
                    &self.request.turn,
                    prepared,
                )
                .map_err(failed)?;
            }
            Ok(output)
        });
        let event = match &result {
            Ok(_) => {
                observation["result"] = json!({"ok":true,"output":display});
                json!({"type":"tool-output-available","toolCallId":id,"output":display})
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
    transcript::messages(sessions, owner, session)
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
        let mut turns = Vec::new();
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
            turns.push(turn.id);
        }
        assert_eq!(
            messages(&sessions, "local", "pilot").unwrap(),
            vec![
                json!({"role":"user","text":"question-1","turn":turns[0]}),
                json!({"role":"assistant","text":"answer-1","turn":turns[0]}),
                json!({"role":"user","text":"question-2","turn":turns[1]}),
                json!({"role":"assistant","text":"answer-2","turn":turns[1]}),
                json!({"role":"user","text":"question-3","turn":turns[2]}),
                json!({"role":"assistant","text":"answer-3","turn":turns[2]}),
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
        assert!(crate::host_fs::directory(&root.path().join("link/child")).is_err());
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
        assert!(validate_binding(Some(&fact()), Some(&retained), "binary").is_ok());
        assert!(validate_binding(Some(&fact()), Some(&retained), "new-binary").is_ok());
        assert!(validate_binding(None, Some(&retained), "binary").is_err());
        let mut changed = fact();
        changed.session_id = Some("replacement-session".into());
        assert!(validate_binding(Some(&changed), Some(&retained), "binary").is_err());
        changed = fact();
        changed.model_binding.clear();
        assert!(validate_binding(Some(&changed), Some(&retained), "binary").is_err());
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
