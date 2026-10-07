//! System Pilot turns over a native io-harness Session, not a hidden Graph.
//!
//! The host supplies a private, key-digest scope and explicitly authorized tools.
//! Store and Session stay on the calling thread; this future need not be Send.
//! Readers use public native Store APIs without taking the execution lease.
//! Store::open may perform native migrations and permissions maintenance; this
//! is a read-only product projection, not a SQLite READ_ONLY connection.

use std::collections::{BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anchor_runtime_rig::{ToolError, ToolPort};
use io_harness::{
    ApproveAll, Broadcast, EventKind, Flow, Observer, Policy, RunEvent, RunOutcome, Session, Store,
    SystemPrompt, TaskContract, Verification,
};
use rig_core::{completion::ToolDefinition, message::ToolResultContent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::adapter::RigProviderAdapter;
use crate::conversation::{Locator, unfinished_effects};
use crate::node_exec::{anchor_tool_mask, anchor_tools};

mod interactive;
pub use interactive::{
    InteractivePilotOutcome, PilotAnswer, PilotPendingQuestion, PilotQuestionRef,
    pilot_pending_question, resume_pilot_with_answer, run_interactive_pilot,
};

#[derive(Debug, Clone)]
pub struct PilotRequest {
    pub root: PathBuf,
    pub prompt: String,
    pub instructions: String,
    pub max_steps: usize,
    pub max_tokens: u64,
    pub wall_time: Duration,
}

pub trait PilotObserver: Send + Sync {
    fn event(&self, chunk: Value) -> Result<(), String>;
    fn cancelled(&self) -> bool;
    fn native_run(&self, _session: i64, _run: i64) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PilotStatus {
    Completed,
    Stopped,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotOutcome {
    pub status: PilotStatus,
    pub reply: Option<String>,
    pub error: Option<String>,
}

pub async fn run_pilot(
    request: PilotRequest,
    provider: &RigProviderAdapter,
    port: Arc<dyn ToolPort>,
    observer: Arc<dyn PilotObserver>,
) -> Result<PilotOutcome, String> {
    let max_steps = u32::try_from(request.max_steps)
        .ok()
        .filter(|steps| *steps > 0)
        .ok_or_else(|| "Pilot max_steps must be in 1..=u32::MAX".to_owned())?;
    if request.prompt.trim().is_empty() || request.max_tokens == 0 || request.wall_time.is_zero() {
        return Err("Pilot requires a prompt and nonzero token/time budgets".into());
    }
    let paths = PilotPaths::new(&request.root)?;
    paths.ensure_root()?;
    let _lease = paths.acquire()?;
    let (store, mut session) = paths.open_or_create()?;
    attach_unfinished_turn(&store, &mut session)?;
    let projection = Arc::new(Projection::new(observer, session.id()));
    let guarded: Arc<dyn ToolPort> = Arc::new(GuardedPort {
        port,
        projection: Arc::clone(&projection),
    });
    let (policy, contract) =
        pilot_contract(&request, &store, &session, &guarded, max_steps, false)?;
    let provider = provider
        .clone()
        .with_tool_catalog(guarded.definitions().into_iter().map(|tool| tool.name));
    let broadcast = Broadcast::new(
        Store::open(&paths.store).map_err(native_error)?,
        projection.as_ref(),
    );
    let result = tokio::time::timeout(
        request.wall_time,
        session.turn_bounded_observed(
            &contract,
            &provider,
            &store,
            &policy,
            &ApproveAll,
            &broadcast,
        ),
    )
    .await;
    attach_unfinished_turn(&store, &mut session)?;
    if let Some(error) = projection.error() {
        return Ok(PilotOutcome {
            status: PilotStatus::Failed,
            reply: None,
            error: Some(error),
        });
    }
    let outcome = match result {
        Err(_) => pilot_timeout(),
        Ok(Err(error)) => pilot_failure(native_error(error)),
        Ok(Ok(turn)) => pilot_outcome(turn.outcome, turn.reply),
    };
    if let Some(error) = &outcome.error {
        projection.emit(json!({"type":"error", "errorText":error}))?;
    }
    Ok(outcome)
}

fn pilot_contract(
    request: &PilotRequest,
    store: &Store,
    session: &Session,
    guarded: &Arc<dyn ToolPort>,
    max_steps: u32,
    interactive: bool,
) -> Result<(Policy, TaskContract), String> {
    let tools = anchor_tools(Arc::clone(guarded)).map_err(|error| error.to_string())?;
    let mut policy = Policy::default().layer("anchor-pilot");
    for definition in guarded.definitions() {
        policy = policy.allow_exec(&definition.name);
    }
    let mut contract = TaskContract::workspace(&request.prompt, &request.root)
        .with_verification(Verification::None)
        .with_max_steps(max_steps)
        .with_token_budget(request.max_tokens)
        .with_time_budget(request.wall_time)
        .with_tool_mask(anchor_tool_mask())
        .with_system_prompt(SystemPrompt::Replace("You are Anchor Pilot. Use only explicitly exposed Anchor tools, verify their results, and answer concisely.".into()))
        .with_tools(tools);
    if !request.instructions.trim().is_empty() {
        contract.instructions.push(request.instructions.clone());
    }
    if interactive {
        contract.tool_mask = io_harness::ToolMask::withholding(
            anchor_tool_mask()
                .names()
                .filter(|name| *name != io_harness::ASK_QUESTION_TOOL),
        );
        contract = contract.with_responder(Arc::new(io_harness::ResponderNone));
    }
    for turn in session.history(store).map_err(native_error)? {
        let facts = unfinished_effects(
            store,
            &Locator {
                scope: String::new(),
                invocation: String::new(),
                session_id: session.id(),
                run_id: turn.run_id,
                turn_id: turn.id,
            },
        )?;
        if !facts.is_empty() {
            contract.instructions.push(facts);
        }
        contract
            .instructions
            .extend(unknown_tool_facts(store, turn.run_id)?);
    }
    Ok((policy, contract))
}

fn pilot_timeout() -> PilotOutcome {
    PilotOutcome {
        status: PilotStatus::Interrupted,
        reply: None,
        error: Some("Pilot wall-time limit interrupted the native turn; unfinished tool outcomes remain unknown".into()),
    }
}

fn pilot_failure(error: String) -> PilotOutcome {
    PilotOutcome {
        status: PilotStatus::Failed,
        reply: None,
        error: Some(error),
    }
}

fn pilot_outcome(outcome: RunOutcome, reply: Option<String>) -> PilotOutcome {
    let (status, error) = match outcome {
        RunOutcome::Finished { .. } | RunOutcome::Success { .. } => (PilotStatus::Completed, None),
        RunOutcome::Cancelled { .. } => (PilotStatus::Stopped, None),
        RunOutcome::StepCapReached { .. } | RunOutcome::CostBudgetExceeded { .. } => {
            (PilotStatus::Stopped, Some(format!("{outcome:?}")))
        }
        RunOutcome::TimeBudgetExceeded { .. } => {
            (PilotStatus::Interrupted, Some(format!("{outcome:?}")))
        }
        _ => (PilotStatus::Failed, Some(format!("{outcome:?}"))),
    };
    PilotOutcome {
        status,
        reply,
        error,
    }
}

/// Project only native facts. An absent scope remains absent, and an active
/// writer keeps its lease. Native Store maintenance is allowed on existing data.
pub fn pilot_messages(root: &Path) -> Result<Vec<Value>, String> {
    let paths = PilotPaths::new(root)?;
    if !paths.root_exists()? {
        return Ok(Vec::new());
    }
    let Some((store, session)) = paths.open_existing()? else {
        return Ok(Vec::new());
    };
    let mut messages = Vec::new();
    for turn in store.session_turns(session.id()).map_err(native_error)? {
        messages.push(json!({"role":"user", "text":turn.prompt}));
        let mut last_text = None;
        let observations = store.observations(turn.run_id).map_err(native_error)?;
        let questions = store.questions(turn.run_id).map_err(native_error)?;
        for step in store.step_turns(turn.run_id).map_err(native_error)? {
            let commands = step
                .calls
                .iter()
                .map(|call| {
                    format!(
                        "{} {}",
                        interactive::external_name(&call.name, !questions.is_empty()),
                        call.arguments
                    )
                })
                .collect::<Vec<_>>();
            if step.text.is_some() || !commands.is_empty() {
                messages.push(json!({
                    "role":"assistant", "text":step.text.as_deref().unwrap_or(""),
                    "commands":commands,
                }));
                last_text = step.text;
            }
            for observation in observations.iter().filter(|observation| {
                observation.step == step.step
                    && observation.kind == io_harness::context::ObsKind::Tool
            }) {
                messages.push(json!({"role":"tool", "text":observation.text}));
            }
            for question in questions
                .iter()
                .filter(|question| question.step == step.step)
            {
                messages.push(
                    json!({"role":"assistant", "text":question.question, "toolName":"session_ask"}),
                );
                if let Some(answer) = &question.answer {
                    messages.push(json!({"role":"user", "text":answer}));
                }
            }
        }
        if let Some(reply) = turn.reply
            && last_text.as_deref() != Some(reply.as_str())
        {
            messages.push(json!({"role":"assistant", "text":reply}));
        }
        for fact in unknown_tool_facts(&store, turn.run_id)? {
            messages.push(json!({
                "role":"tool",
                "text":fact,
            }));
        }
    }
    Ok(messages)
}

fn native_error(error: io_harness::Error) -> String {
    format!("Pilot native io-harness: {error}")
}

fn unknown_tool_facts(store: &Store, run_id: i64) -> Result<Vec<String>, String> {
    let committed = store.step_turns(run_id).map_err(native_error)?;
    let mut known = BTreeSet::new();
    let mut facts = Vec::new();
    for attempt in store.open_attempts(run_id).map_err(native_error)? {
        known.insert((attempt.step, attempt.tool.clone()));
        facts.push(format!(
            "Unrecorded tool result: tool={}, step={}, attempt={}. External outcome is unknown; verify actual state before acting.",
            attempt.tool, attempt.step, attempt.id,
        ));
    }
    let mut cursor = 0;
    loop {
        let events = store
            .events_since(run_id, cursor, 512)
            .map_err(native_error)?;
        if events.is_empty() {
            break;
        }
        for (position, event) in events {
            cursor = position;
            if let EventKind::ToolCall { name, target, .. } = event.kind
                && !committed.iter().any(|step| step.step == event.step)
                && known.insert((event.step, name.clone()))
            {
                facts.push(format!(
                    "Unrecorded tool result: tool={name}, step={}, target={target}. External outcome is unknown; verify actual state before acting.",
                    event.step,
                ));
            }
        }
    }
    Ok(facts)
}

fn attach_unfinished_turn(store: &Store, session: &mut Session) -> Result<(), String> {
    let history = session.history(store).map_err(native_error)?;
    let turns = store.session_turns(session.id()).map_err(native_error)?;
    let mut unheaded = turns
        .iter()
        .filter(|turn| !history.iter().any(|previous| previous.id == turn.id));
    if let Some(turn) = unheaded.next() {
        if unheaded.next().is_some() || turn.parent_turn_id != session.head() {
            return Err("Pilot native Session has conflicting off-head turns".into());
        }
        session.branch_from(store, turn.id).map_err(native_error)?;
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionLocator {
    version: u32,
    framework: String,
    framework_version: String,
    root: PathBuf,
    session_id: i64,
}

struct PilotPaths {
    root: PathBuf,
    store: PathBuf,
    locator: PathBuf,
    lock: PathBuf,
}

impl PilotPaths {
    fn new(root: &Path) -> Result<Self, String> {
        if !root.is_absolute()
            || root
                .components()
                .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
            || !root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        {
            return Err("Pilot root must be an absolute host-generated SHA-256 scope".into());
        }
        Ok(Self {
            root: root.to_owned(),
            store: root.join("framework.sqlite3"),
            locator: root.join("session.json"),
            lock: root.join("execution.lock"),
        })
    }

    fn root_exists(&self) -> Result<bool, String> {
        let mut ancestor = PathBuf::new();
        for component in self.root.components() {
            ancestor.push(component);
            match fs::symlink_metadata(&ancestor) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => return Err(format!("unsafe Pilot directory {}", ancestor.display())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(true)
    }

    fn ensure_root(&self) -> Result<(), String> {
        if !self.root_exists()? {
            fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        }
        if !self.root_exists()? {
            return Err("Pilot scope vanished".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
                .map_err(|error| error.to_string())?;
        }
        self.validate_files()
    }

    fn validate_files(&self) -> Result<(), String> {
        for name in [
            "framework.sqlite3",
            "framework.sqlite3-wal",
            "framework.sqlite3-shm",
            "framework.sqlite3-journal",
            "session.json",
            "session.json.tmp",
            "execution.lock",
        ] {
            regular_file(&self.root.join(name))?;
        }
        Ok(())
    }

    fn acquire(&self) -> Result<ScopeLease, String> {
        regular_file(&self.lock)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&self.lock)
            .map_err(|error| error.to_string())?;
        regular_file(&self.lock)?;
        file.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => "Pilot scope is already active".into(),
            fs::TryLockError::Error(error) => error.to_string(),
        })?;
        Ok(ScopeLease(file))
    }

    fn open_existing(&self) -> Result<Option<(Store, Session)>, String> {
        self.validate_files()?;
        let has_store = regular_file(&self.store)?;
        let has_locator = regular_file(&self.locator)?;
        if !has_store && !has_locator {
            return Ok(None);
        }
        if !has_store || !has_locator {
            return Err("Pilot scope has an incomplete native Store/Session locator".into());
        }
        let locator: SessionLocator =
            serde_json::from_slice(&fs::read(&self.locator).map_err(|error| error.to_string())?)
                .map_err(|error| format!("invalid Pilot Session locator: {error}"))?;
        if locator.version != 1
            || locator.framework != "io-harness"
            || locator.framework_version != "0.86.0"
            || locator.root != self.root
            || locator.session_id <= 0
        {
            return Err("incompatible Pilot native Session locator".into());
        }
        let mut header = [0; 16];
        File::open(&self.store)
            .and_then(|mut file| file.read_exact(&mut header))
            .map_err(|error| format!("invalid Pilot native Store: {error}"))?;
        if &header != b"SQLite format 3\0" {
            return Err("invalid Pilot native Store format".into());
        }
        let store = Store::open(&self.store).map_err(native_error)?;
        let session = Session::reopen(&store, locator.session_id).map_err(native_error)?;
        if session.root() != self.root {
            return Err("Pilot native Session root does not match its scope".into());
        }
        Ok(Some((store, session)))
    }

    fn open_or_create(&self) -> Result<(Store, Session), String> {
        if let Some(existing) = self.open_existing()? {
            return Ok(existing);
        }
        let store = Store::open(&self.store).map_err(native_error)?;
        let session = Session::open(&store, &self.root).map_err(native_error)?;
        let locator = SessionLocator {
            version: 1,
            framework: "io-harness".into(),
            framework_version: "0.86.0".into(),
            root: self.root.clone(),
            session_id: session.id(),
        };
        let temporary = self.root.join("session.json.tmp");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(&serde_json::to_vec(&locator).map_err(|error| error.to_string())?)
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())?;
        fs::rename(&temporary, &self.locator).map_err(|error| error.to_string())?;
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())?;
        Ok((store, session))
    }
}

fn regular_file(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(format!("hard-linked Pilot file {}", path.display()));
                }
            }
            Ok(true)
        }
        Ok(_) => Err(format!("unsafe Pilot file {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

struct ScopeLease(File);

impl Drop for ScopeLease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Default)]
struct ProjectionState {
    run_id: i64,
    step: u32,
    ordinal: usize,
    text_id: Option<String>,
    calls: VecDeque<(String, String)>,
    error: Option<String>,
}

struct Projection {
    observer: Arc<dyn PilotObserver>,
    session_id: i64,
    state: Mutex<ProjectionState>,
    interactive: bool,
}

impl Projection {
    fn new(observer: Arc<dyn PilotObserver>, session_id: i64) -> Self {
        Self {
            observer,
            session_id,
            state: Mutex::new(ProjectionState::default()),
            interactive: false,
        }
    }

    fn error(&self) -> Option<String> {
        match self.state.lock() {
            Ok(state) => state.error.clone(),
            Err(_) => Some("Pilot observer projection lock was poisoned".into()),
        }
    }

    fn emit(&self, chunk: Value) -> Result<(), String> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        if let Err(error) = self.observer.event(chunk) {
            if let Ok(mut state) = self.state.lock() {
                state.error = Some(error.clone());
            }
            return Err(error);
        }
        Ok(())
    }

    fn cancelled(&self) -> bool {
        self.observer.cancelled() || self.error().is_some()
    }

    fn call_id(&self, name: &str) -> Result<String, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let position = state
            .calls
            .iter()
            .position(|(tool, _)| tool == name)
            .ok_or_else(|| "Anchor tool invocation has no native ToolCall event".to_owned())?;
        Ok(state.calls.remove(position).expect("position was found").1)
    }
}

impl Observer for Projection {
    fn event(&self, event: &RunEvent) -> Flow {
        let projected = (|| {
            if matches!(event.kind, EventKind::Started { .. }) {
                self.observer.native_run(self.session_id, event.run_id)?;
            }
            let mut chunks = Vec::new();
            {
                let mut state = self.state.lock().map_err(|error| error.to_string())?;
                state.run_id = event.run_id;
                if matches!(event.kind, EventKind::Started { .. })
                    || (event.step > 0 && state.step != event.step)
                {
                    state.step = event.step;
                    chunks.push(json!({"type":"start-step"}));
                }
                match &event.kind {
                    EventKind::Token { text } => {
                        let text_id = match &state.text_id {
                            Some(id) => id.clone(),
                            None => {
                                let id = format!("pilot-{}-text-{}", event.run_id, event.step);
                                state.text_id = Some(id.clone());
                                chunks.push(json!({"type":"text-start", "id":id}));
                                id
                            }
                        };
                        chunks.push(json!({"type":"text-delta", "id":text_id, "delta":text}));
                    }
                    EventKind::ToolCall { name, .. } => {
                        state.ordinal += 1;
                        let id = format!("pilot-{}-tool-{}", event.run_id, state.ordinal);
                        state.calls.push_back((name.clone(), id.clone()));
                        chunks.push(
                            json!({"type":"tool-input-start", "toolCallId":id, "toolName":interactive::external_name(name, self.interactive)}),
                        );
                    }
                    EventKind::Step { .. } | EventKind::Finished { .. } => {
                        if let Some(id) = state.text_id.take() {
                            chunks.push(json!({"type":"text-end", "id":id}));
                        }
                        if matches!(event.kind, EventKind::Step { .. }) {
                            chunks.push(json!({"type":"finish-step"}));
                        }
                    }
                    _ => {}
                }
            }
            for chunk in chunks {
                self.emit(chunk)?;
            }
            Ok::<_, String>(())
        })();
        if let Err(error) = projected
            && let Ok(mut state) = self.state.lock()
        {
            state.error = Some(error);
        }
        if self.cancelled() {
            Flow::Cancel
        } else {
            Flow::Continue
        }
    }
}

struct GuardedPort {
    port: Arc<dyn ToolPort>,
    projection: Arc<Projection>,
}

impl ToolPort for GuardedPort {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.port.definitions()
    }

    fn is_read_only(&self, name: &str) -> bool {
        self.port.is_read_only(name)
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if self.projection.cancelled() {
                return Err(ToolError::Failed(
                    "Pilot cancelled before Anchor tool dispatch".into(),
                ));
            }
            let id = self.projection.call_id(name).map_err(ToolError::Failed)?;
            self.projection
                .emit(json!({
                    "type":"tool-input-available", "toolCallId":id,
                    "toolName":name, "input":arguments,
                }))
                .map_err(ToolError::Failed)?;
            if self.projection.cancelled() {
                return Err(ToolError::Failed(
                    "Pilot cancelled before Anchor tool dispatch".into(),
                ));
            }
            let result = self.port.call(name, arguments).await;
            let chunk = match &result {
                Ok(results) => json!({
                    "type":"tool-output-available", "toolCallId":id,
                    "output":serde_json::to_value(results).map_err(|error| ToolError::Failed(error.to_string()))?,
                }),
                Err(error) => {
                    json!({"type":"tool-output-error", "toolCallId":id, "errorText":error.to_string()})
                }
            };
            let _ = self.projection.emit(chunk);
            result
        })
    }
}

#[cfg(test)]
mod tests;
