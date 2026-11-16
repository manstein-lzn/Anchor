use crate::{
    client::AppServerClient,
    config::BridgeConfig,
    git,
    state::{BridgeState, PendingServerRequest, TrackedThread, WorkerStatus, busy, executing},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
mod native;
mod session;

pub struct Manager {
    pub config: BridgeConfig,
    pub state: Arc<BridgeState>,
    pub client: Arc<AppServerClient>,
    // Only admission/control is serialized. Background turns and waits remain parallel.
    operations: Mutex<()>,
    stopped: AtomicBool,
    protocol: tokio::sync::OnceCell<crate::protocol::ProtocolCatalog>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    #[default]
    ReadOnly,
    WorkspaceWrite,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Approval {
    #[default]
    OnRequest,
    Never,
}
impl Approval {
    fn name(self) -> &'static str {
        match self {
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spawn {
    project: String,
    prompt: String,
    title: Option<String>,
    worktree: Option<String>,
    base_ref: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    #[serde(default)]
    mode: Mode,
    #[serde(default)]
    approval_policy: Approval,
    #[serde(default)]
    network_access: bool,
    #[serde(default = "default_wait")]
    wait_seconds: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThreadArg {
    thread_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Btw {
    thread_id: String,
    prompt: String,
    #[serde(default = "default_wait")]
    wait_seconds: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Board {
    project: Option<String>,
    cursor: Option<String>,
    #[serde(default)]
    archived: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Continue {
    thread_id: String,
    prompt: Option<String>,
    request_key: Option<String>,
    approval_decision: Option<String>,
    answers: Option<std::collections::BTreeMap<String, Vec<String>>>,
    elicitation_content: Option<Value>,
    mode: Option<Mode>,
    #[serde(default)]
    approval_policy: Approval,
    #[serde(default)]
    network_access: bool,
    #[serde(default = "default_wait")]
    wait_seconds: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    thread_id: String,
    action: String,
    #[serde(default)]
    force: bool,
}
fn default_wait() -> u64 {
    12
}
fn parse<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))
}
fn validate_wait(wait: u64) -> Result<(), String> {
    if wait > 30 {
        Err("wait_seconds must be between 0 and 30".into())
    } else {
        Ok(())
    }
}
fn validate_prompt(prompt: &str) -> Result<(), String> {
    if prompt.trim().is_empty() || prompt.len() > 100_000 {
        Err("prompt must be nonempty and at most 100000 bytes".into())
    } else {
        Ok(())
    }
}

impl Manager {
    #[cfg(test)]
    pub(crate) fn install_writer_fixture(&self) {
        self.protocol
            .set(crate::protocol::ProtocolCatalog::writer_fixture())
            .unwrap();
    }

    pub fn new(config: BridgeConfig) -> Arc<Self> {
        let state = BridgeState::new();
        let client = AppServerClient::new(
            config.codex_bin.clone(),
            config.codex_args.clone(),
            state.clone(),
        );
        Arc::new(Self {
            config,
            state,
            client,
            operations: Mutex::new(()),
            stopped: AtomicBool::new(false),
            protocol: tokio::sync::OnceCell::new(),
        })
    }

    pub async fn dispatch(
        &self,
        name: &str,
        args: Value,
        scope: Option<&str>,
    ) -> Result<Value, String> {
        self.check_open()?;
        let roots = self.config.effective_roots(scope)?;
        match name {
            "native_discover" => self.native_discover(parse(args)?).await,
            "native_events" => self.native_events(parse(args)?, &roots).await,
            "native_call" | "native_respond" => {
                let _guard = self.operations.lock().await;
                self.check_open()?;
                if name == "native_call" {
                    self.native_call(parse(args)?, &roots).await
                } else {
                    self.native_respond(parse(args)?, &roots).await
                }
            }
            "projects" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Empty {}
                let _: Empty = parse(args)?;
                let projects = git::discover_projects(&roots, &self.config.worktree_root).await;
                let threads = self.state.threads.read().await;
                let projects: Vec<_> = projects
                    .into_iter()
                    .map(|p| {
                        let active = threads
                            .values()
                            .filter(|t| {
                                busy(t)
                                    && (t.project_path.as_deref() == Some(Path::new(&p.path))
                                        || t.cwd == Path::new(&p.path))
                            })
                            .count();
                        let mut value = json!(p);
                        value["active_sessions"] = json!(active);
                        value
                    })
                    .collect();
                Ok(json!({"projects": projects, "roots": roots, "discovery_depth": 3}))
            }
            "board" => self.board(parse(args)?, &roots).await,
            "models" => self.models(parse(args)?).await,
            "history" => self.history(parse(args)?, &roots).await,
            "session" => {
                let _guard = self.operations.lock().await;
                self.check_open()?;
                self.session(parse(args)?, &roots).await
            }
            "inspect" => {
                let a: ThreadArg = parse(args)?;
                let t = self.thread(&a.thread_id, &roots, true).await?;
                let git = git::git_workspace_snapshot(&t.cwd).await;
                Ok(t.inspect_detail(git))
            }
            "spawn" => {
                let a: Spawn = parse(args)?;
                validate_wait(a.wait_seconds)?;
                validate_prompt(&a.prompt)?;
                if a.reasoning_effort
                    .as_ref()
                    .is_some_and(|s| s.trim().is_empty())
                {
                    return Err(
                        "reasoning_effort must be nonempty; omit it to inherit local configuration"
                            .into(),
                    );
                }
                let wait = a.wait_seconds;
                let id = {
                    let _guard = self.operations.lock().await;
                    self.check_open()?;
                    self.spawn(a, &roots).await?
                };
                self.wait_summary(&id, wait).await
            }
            "btw" => {
                let a: Btw = parse(args)?;
                validate_wait(a.wait_seconds)?;
                validate_prompt(&a.prompt)?;
                let id = {
                    let _guard = self.operations.lock().await;
                    self.check_open()?;
                    self.btw(&a, &roots).await?
                };
                self.wait_summary(&id, a.wait_seconds).await?;
                let threads = self.state.threads.read().await;
                let side = threads.get(&id).ok_or("side conversation disappeared")?;
                Ok(side.inspect_detail(None))
            }
            "steer_or_resume" => {
                let a: Continue = parse(args)?;
                validate_wait(a.wait_seconds)?;
                let id = a.thread_id.clone();
                let wait = a.wait_seconds;
                let action = {
                    let _guard = self.operations.lock().await;
                    self.check_open()?;
                    self.continue_thread(a, &roots).await?
                };
                let mut value = self.wait_summary(&id, wait).await?;
                value["action"] = json!(action);
                Ok(value)
            }
            "control" => {
                let a: Control = parse(args)?;
                let _guard = self.operations.lock().await;
                self.check_open()?;
                self.control(a, &roots).await
            }
            _ => Err("unknown Codex bridge tool".into()),
        }
    }

    fn check_open(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::Acquire) {
            Err("bridge is shutting down".into())
        } else {
            Ok(())
        }
    }

    pub async fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        self.client.shutdown().await;
        // Detached HTTP calls may be inside a bounded Git operation. Drain that
        // admission before returning; queued mutations observe stopped above.
        let _guard = self.operations.lock().await;
    }

    async fn spawn(&self, a: Spawn, roots: &[PathBuf]) -> Result<String, String> {
        let project = self.config.resolve_project(&a.project, roots)?;
        if a.base_ref.is_some() && a.worktree.is_none() {
            return Err("base_ref requires worktree".into());
        }
        let prepared = match &a.worktree {
            Some(slug) => Some(
                git::ensure_worktree(
                    &self.config.worktree_root,
                    &project,
                    slug,
                    a.base_ref.as_deref(),
                )
                .await?,
            ),
            None => None,
        };
        let cwd = prepared
            .as_ref()
            .map_or_else(|| project.clone(), |w| w.path.clone());
        let managed = git::managed_worktree_info(&self.config.worktree_root, &cwd).await?;
        self.check_writer(&cwd, a.mode.name(), None).await?;
        // Probe before creating a thread; actual execution acquires its owned guard.
        let _admission = if a.mode.name() == "workspace-write" {
            Some(
                anchor_file_lease::ProjectLease::acquire(&cwd)
                    .map_err(|e| format!("project writer lease unavailable: {e}"))?,
            )
        } else {
            None
        };
        let mut params = json!({"cwd": cwd, "sandbox": a.mode.name(), "approvalPolicy": a.approval_policy.name(), "approvalsReviewer": "user"});
        if let Some(model) = a.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = a.reasoning_effort {
            params["config"] = json!({"model_reasoning_effort": effort});
        }
        let response = self.client.request("thread/start", params).await?;
        let id = response["thread"]["id"]
            .as_str()
            .ok_or("thread/start returned no thread id")?
            .to_owned();
        {
            let mut threads = self.state.threads.write().await;
            let t = threads.entry(id.clone()).or_insert_with(|| {
                TrackedThread::new(
                    id.clone(),
                    a.title.clone(),
                    Some(project.clone()),
                    cwd.clone(),
                    a.worktree.clone(),
                    a.mode.name().into(),
                )
            });
            t.project_path = Some(project);
            t.cwd = cwd;
            t.worktree_slug = a.worktree;
            t.git_common_dir = prepared.map(|w| w.git_common_dir);
            if let Some((repo, slug, common)) = managed {
                t.project_path = Some(repo);
                t.worktree_slug = Some(slug);
                t.git_common_dir = Some(common);
            }
            t.mode = a.mode.name().into();
            t.network_access = a.network_access;
            t.loaded = true;
            t.model = response["model"].as_str().map(str::to_owned);
            t.reasoning_effort = response["reasoningEffort"].as_str().map(str::to_owned);
        }
        let setup = async {
            if let Some(title) = a.title {
                self.client
                    .request("thread/name/set", json!({"threadId": id, "name": title}))
                    .await?;
                if let Some(t) = self.state.threads.write().await.get_mut(&id) {
                    t.title = Some(title);
                }
            }
            drop(_admission);
            self.start_turn(&id, &a.prompt, a.approval_policy).await
        }
        .await;
        if let Err(e) = setup {
            if let Some(t) = self.state.threads.write().await.get_mut(&id) {
                t.last_error = Some(e.clone());
            }
            return Err(format!(
                "thread {id} created; turn admission uncertain: {e}. Inspect before retrying; do not spawn a duplicate."
            ));
        }
        Ok(id)
    }

    async fn btw(&self, a: &Btw, roots: &[PathBuf]) -> Result<String, String> {
        let parent = self.thread(&a.thread_id, roots, false).await?;
        if parent.archived || parent.parent_thread_id.is_some() {
            return Err("btw requires an unarchived main session; use session unarchive for archived history".into());
        }
        let id = self
            .fork_thread(
                &parent,
                &session::Fork {
                    title: Some(format!(
                        "btw: {}",
                        parent.title.as_deref().unwrap_or(&parent.thread_id)
                    )),
                    ephemeral: true,
                    ..Default::default()
                },
            )
            .await?;
        // Same native ephemeral-fork boundary as the CLI side chat, never a steer of the parent.
        let prompt = format!(
            "Side conversation boundary. All inherited history is reference context only, not your current task. \
             Answer only the question below; do not continue inherited tasks, invoke tools or subagents, \
             modify files, or change the main session. The main task continues independently. \
             Your context is a fork-time snapshot, not a live feed.\nMain session snapshot: {}\nQuestion: {}",
            parent.board_summary(),
            a.prompt
        );
        if let Err(error) = self.start_turn(&id, &prompt, Approval::Never).await {
            return Err(format!(
                "side conversation {id} created: {error}. Inspect it before retrying; main session was not steered or interrupted."
            ));
        }
        Ok(id)
    }

    async fn start_turn(&self, id: &str, prompt: &str, approval: Approval) -> Result<(), String> {
        self.start_native_turn(
            id,
            json!({"input": [{"type":"text","text":prompt}]}),
            approval,
        )
        .await
        .map(|_| ())
    }

    async fn start_native_turn(
        &self,
        id: &str,
        mut params: Value,
        approval: Approval,
    ) -> Result<Value, String> {
        let current = self
            .state
            .threads
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or("thread disappeared")?;
        self.check_writer(&current.cwd, &current.mode, Some(id))
            .await?;
        let _lease = self
            .client
            .prepare_writer(id, &current.cwd, &current.mode)
            .await?;
        let t = {
            let mut threads = self.state.threads.write().await;
            let current = threads.get_mut(id).ok_or("thread disappeared")?;
            current.admission_epoch += 1;
            current.status = WorkerStatus::Unknown;
            current.clone()
        };
        let sandbox = native::native_sandbox(&t);
        params["threadId"] = json!(id);
        params["cwd"] = json!(t.cwd);
        params["approvalPolicy"] = json!(approval.name());
        params["approvalsReviewer"] = json!("user");
        params["sandboxPolicy"] = sandbox;
        let response = self
            .client
            .writer_request("turn/start", params, id, &t.cwd, &t.mode)
            .await;
        let response = match response {
            Ok(response) if response["turn"]["id"].is_string() => response,
            outcome => {
                let reason = outcome
                    .err()
                    .unwrap_or_else(|| "turn/start returned no turn id".into());
                if let Some(current) = self.state.threads.write().await.get_mut(id)
                    && current.loaded
                    && current.admission_epoch == t.admission_epoch
                    && current.last_turn_id == t.last_turn_id
                    && current.active_turn_id.is_none()
                {
                    current.status = WorkerStatus::Unknown;
                    current.last_error = Some(format!(
                        "Turn admission is uncertain: {reason}. Inspect before retrying."
                    ));
                }
                return Err(reason);
            }
        };
        let turn_id = response["turn"]["id"].as_str().expect("turn id checked");
        // Notifications can arrive before the RPC response, including completion.
        if self
            .state
            .threads
            .read()
            .await
            .get(id)
            .is_some_and(|t| t.last_turn_id.as_deref() != Some(turn_id))
        {
            self.state
                .apply_notification(
                    "turn/started",
                    &json!({"threadId": id, "turn": response["turn"]}),
                )
                .await;
        }
        Ok(response)
    }

    async fn check_writer(
        &self,
        cwd: &Path,
        mode: &str,
        except: Option<&str>,
    ) -> Result<(), String> {
        if mode != "workspace-write" {
            return Ok(());
        }
        let queued: Vec<_> = self
            .state
            .threads
            .read()
            .await
            .values()
            .filter(|t| t.loaded && t.native_queue_pending && Some(t.thread_id.as_str()) != except)
            .map(|t| t.thread_id.clone())
            .collect();
        for id in queued {
            self.refresh_native_queue(&id).await?;
        }
        if self.state.threads.read().await.values().any(|t| {
            Some(t.thread_id.as_str()) != except
                && busy(t)
                && t.mode == "workspace-write"
                && (t.cwd.starts_with(cwd) || cwd.starts_with(&t.cwd))
        }) {
            return Err(
                "a writer is already active in this workspace; use a different worktree".into(),
            );
        }
        Ok(())
    }

    async fn thread(
        &self,
        id: &str,
        roots: &[PathBuf],
        detail: bool,
    ) -> Result<TrackedThread, String> {
        let cached = self.state.threads.read().await.get(id).cloned();
        if cached.as_ref().is_some_and(|t| !t.loaded || !t.subscribed) {
            let result = self
                .client
                .request("thread/read", json!({"threadId": id}))
                .await?;
            let actual = PathBuf::from(
                result["thread"]["cwd"]
                    .as_str()
                    .ok_or("history has no cwd")?,
            );
            if !self.config.is_authorized_cwd(&actual, roots) {
                return Err("thread is outside authorized project scope".into());
            }
            if let Some(current) = self.state.threads.write().await.get_mut(id)
                && !current.subscribed
            {
                current.loaded = result["thread"]["status"]["type"]
                    .as_str()
                    .is_some_and(|s| s != "notLoaded");
                current.model = result["thread"]["model"].as_str().map(str::to_owned);
                current.reasoning_effort = result["thread"]["reasoningEffort"]
                    .as_str()
                    .map(str::to_owned);
            }
            if cached.as_ref().is_some_and(|t| t.cwd != actual) {
                let mut threads = self.state.threads.write().await;
                if threads.get(id).is_some_and(|t| !t.loaded || !t.subscribed) {
                    threads.remove(id);
                }
                drop(threads);
                self.import_thread(&result["thread"], roots).await?;
            }
        }
        let cached = self.state.threads.read().await.get(id).cloned();
        let mut t = match cached {
            Some(t) => t,
            None => {
                let result = self
                    .client
                    .request("thread/read", json!({"threadId": id}))
                    .await?;
                self.import_thread(&result["thread"], roots).await?
            }
        };
        if !self.config.is_authorized_cwd(&t.cwd, roots) {
            return Err("thread is outside authorized project scope".into());
        }
        if detail && t.status == WorkerStatus::Unknown {
            self.refresh_native_goal(id).await?;
            self.refresh_native_queue(id).await?;
            let result = self
                .client
                .request("thread/read", json!({"threadId": id}))
                .await?;
            let runtime = result["thread"]["status"]["type"]
                .as_str()
                .unwrap_or("unknown");
            let turns = self
                .client
                .request(
                    "thread/turns/list",
                    json!({"threadId": id, "limit": 1, "sortDirection": "desc"}),
                )
                .await?;
            let last = turns["data"].as_array().and_then(|data| data.first());
            let mut threads = self.state.threads.write().await;
            if let Some(current) = threads.get_mut(id) {
                if current.status == WorkerStatus::Unknown
                    && current.admission_epoch == t.admission_epoch
                    && !self.state.writer_admission_in_flight(id)
                {
                    match runtime {
                        "active" => {
                            if let Some(turn) = last.filter(|turn| turn["status"] == "inProgress")
                                && let Some(turn_id) = turn["id"].as_str()
                            {
                                current.active_turn_id = Some(turn_id.into());
                                current.last_turn_id = Some(turn_id.into());
                                current.status = WorkerStatus::Running;
                            }
                        }
                        "idle" | "notLoaded" => {
                            current.loaded = runtime == "idle";
                            if !current.loaded {
                                current.subscribed = false;
                            }
                            current.status = WorkerStatus::Idle;
                            current.last_error = None;
                            self.state.release_idle_writer_locked(id, current);
                        }
                        _ => {}
                    }
                }
                t = current.clone();
            }
        }
        if detail && (!t.loaded || !t.subscribed) {
            let result = self
                .client
                .request(
                    "thread/turns/list",
                    json!({"threadId": id, "limit": 1, "sortDirection": "desc"}),
                )
                .await?;
            if let Some(turn) = result["data"].as_array().and_then(|a| a.first()) {
                t.last_turn_id = turn["id"].as_str().map(str::to_owned);
                t.status = match turn["status"].as_str() {
                    Some("completed") => WorkerStatus::Completed,
                    Some("failed") => WorkerStatus::Failed,
                    Some("interrupted") => WorkerStatus::Interrupted,
                    _ => WorkerStatus::Idle,
                };
                t.last_error = turn["error"]["message"].as_str().map(str::to_owned);
                if turn["status"] == "inProgress" {
                    t.last_error = Some("Unfinished stored turn; live execution is unknown. Verify the original CLI is stopped before resuming.".into());
                }
                t.latest_agent_message.clear();
                t.latest_command_output.clear();
                let items = self.client.request("thread/items/list", json!({"threadId": id, "turnId": t.last_turn_id, "limit": 30, "sortDirection": "desc"})).await?;
                if let Some(items) = items["data"].as_array() {
                    for entry in items {
                        let item = &entry["item"];
                        if item["type"] == "agentMessage" && t.latest_agent_message.is_empty() {
                            t.latest_agent_message = git::truncate_chars(
                                item["text"].as_str().unwrap_or_default(),
                                24000,
                            );
                        }
                        if item["type"] == "commandExecution" && t.latest_command_output.is_empty()
                        {
                            t.latest_command_output = git::truncate_chars(
                                item["aggregatedOutput"].as_str().unwrap_or_default(),
                                8000,
                            );
                        }
                    }
                }
                // Do not replace a thread concurrently resumed by another call.
                let mut threads = self.state.threads.write().await;
                if let Some(current) = threads.get_mut(id) {
                    if (!current.loaded || !current.subscribed)
                        && current.updated_at_ms == t.updated_at_ms
                        && current.admission_epoch == t.admission_epoch
                    {
                        current.last_turn_id = t.last_turn_id;
                        current.status = t.status;
                        current.last_error = t.last_error;
                        current.latest_agent_message = t.latest_agent_message;
                        current.latest_command_output = t.latest_command_output;
                        current.streaming_message_buffer.clear();
                    }
                    t = current.clone();
                }
            }
        }
        Ok(t)
    }

    async fn import_thread(
        &self,
        value: &Value,
        roots: &[PathBuf],
    ) -> Result<TrackedThread, String> {
        let id = value["id"].as_str().ok_or("history has no thread id")?;
        let cwd = PathBuf::from(value["cwd"].as_str().ok_or("history has no cwd")?);
        if !self.config.is_authorized_cwd(&cwd, roots) {
            let mut threads = self.state.threads.write().await;
            if threads.get(id).is_some_and(|thread| !thread.loaded) {
                threads.remove(id);
            }
            return Err("thread is outside authorized project scope".into());
        }
        let mut t = TrackedThread::new(
            id.into(),
            value["name"].as_str().map(str::to_owned),
            None,
            cwd.clone(),
            None,
            "read-only".into(),
        );
        t.loaded = false;
        t.subscribed = false;
        t.model = value["model"].as_str().map(str::to_owned);
        t.reasoning_effort = value["reasoningEffort"].as_str().map(str::to_owned);
        t.updated_at_ms = value["updatedAt"]
            .as_u64()
            .unwrap_or_default()
            .saturating_mul(1000);
        if let Some((repo, slug, common)) =
            git::managed_worktree_info(&self.config.worktree_root, &cwd).await?
        {
            t.project_path = Some(repo);
            t.worktree_slug = Some(slug);
            t.git_common_dir = Some(common);
        } else {
            t.project_path = Some(cwd);
        }
        let mut threads = self.state.threads.write().await;
        if threads.get(id).is_some_and(|current| {
            !current.loaded && (current.cwd != t.cwd || current.updated_at_ms < t.updated_at_ms)
        }) {
            threads.insert(id.into(), t.clone());
        }
        Ok(threads.entry(id.into()).or_insert(t).clone())
    }

    async fn board(&self, a: Board, roots: &[PathBuf]) -> Result<Value, String> {
        let project = a
            .project
            .as_deref()
            .map(|p| self.config.resolve_project(p, roots))
            .transpose()?;
        let result = self
            .client
            .request(
                "thread/list",
                json!({"limit": 100, "cursor": a.cursor, "archived": a.archived,
                    "modelProviders": [], "sortKey": "updated_at",
                    "sourceKinds": ["cli", "vscode", "exec", "appServer", "subAgent",
                        "subAgentReview", "subAgentCompact", "subAgentThreadSpawn", "subAgentOther", "unknown"]}),
            )
            .await;
        let mut warning = None;
        let mut cursor = Value::Null;
        let mut page_ids = std::collections::BTreeSet::new();
        match result {
            Ok(result) => {
                cursor = result["nextCursor"].clone();
                if let Some(data) = result["data"].as_array() {
                    for value in data {
                        if let Ok(t) = self.import_thread(value, roots).await {
                            page_ids.insert(t.thread_id.clone());
                            if !t.loaded
                                && let Some(t) =
                                    self.state.threads.write().await.get_mut(&t.thread_id)
                            {
                                t.archived = a.archived;
                            }
                        }
                    }
                }
            }
            Err(error) => warning = Some(error),
        }
        let threads = self.state.threads.read().await;
        let mut visible: Vec<_> = threads
            .values()
            .filter(|t| {
                t.archived == a.archived
                    && (t.loaded || page_ids.contains(&t.thread_id))
                    && self.config.is_authorized_cwd(&t.cwd, roots)
                    && project
                        .as_ref()
                        .is_none_or(|p| t.project_path.as_ref() == Some(p) || t.cwd == *p)
            })
            .collect();
        visible.sort_by_key(|t| {
            (
                std::cmp::Reverse(busy(t)),
                std::cmp::Reverse(t.updated_at_ms),
            )
        });
        let count = visible.len();
        let summaries: Vec<_> = visible
            .into_iter()
            .take(200)
            .map(TrackedThread::board_summary)
            .collect();
        Ok(
            json!({"threads": summaries, "history_next_cursor": cursor, "cached_total": count,
            "truncated": count > 200, "warning": warning}),
        )
    }

    async fn continue_thread(
        &self,
        a: Continue,
        roots: &[PathBuf],
    ) -> Result<&'static str, String> {
        let t = self.thread(&a.thread_id, roots, false).await?;
        if t.status == WorkerStatus::Unknown {
            return Err("turn admission is uncertain; inspect before resuming or steering".into());
        }
        if let Some(key) = &a.request_key {
            if a.prompt.is_some() || a.mode.is_some() {
                return Err(
                    "answer a pending request separately from starting or steering a turn".into(),
                );
            }
            let request = t
                .pending_requests
                .iter()
                .find(|r| &r.request_key == key)
                .ok_or("pending request not found; inspect again")?;
            let response = answer_request(request, &a)?;
            self.client
                .respond_to_server_request(&a.thread_id, key, request.rpc_id.clone(), response)
                .await?;
            if let Some(t) = self.state.threads.write().await.get_mut(&a.thread_id) {
                t.pending_requests.retain(|r| &r.request_key != key);
            }
            return Ok("answered");
        }
        if a.approval_decision.is_some() || a.answers.is_some() || a.elicitation_content.is_some() {
            return Err("request_key is required for a pending request response".into());
        }
        let prompt = a.prompt.as_deref().ok_or("prompt is required")?;
        validate_prompt(prompt)?;
        if t.parent_thread_id.is_some() && (a.mode.is_some() || a.network_access) {
            return Err("btw conversations cannot change execution permissions".into());
        }
        if !t.pending_requests.is_empty() {
            return Err("answer pending requests before steering".into());
        }
        if let Some(turn) = &t.active_turn_id {
            if a.mode.is_some() || a.network_access {
                return Err("cannot change execution permissions while steering".into());
            }
            self.client
                .request(
                    "turn/steer",
                    json!({"threadId": t.thread_id, "expectedTurnId": turn,
                "input": [{"type": "text", "text": prompt}]}),
                )
                .await?;
            return Ok("steered");
        }
        if t.archived {
            return Err("archived thread cannot be resumed by this bridge".into());
        }
        let mode = a
            .mode
            .map_or_else(|| t.mode.clone(), |mode| mode.name().to_owned());
        self.check_writer(&t.cwd, &mode, Some(&t.thread_id)).await?;
        self.load_thread(&t, &mode, a.approval_policy).await?;
        if let Some(t) = self.state.threads.write().await.get_mut(&a.thread_id) {
            t.mode = mode;
            t.network_access = a.network_access;
            t.loaded = true;
        }
        let approval = if t.parent_thread_id.is_some() {
            Approval::Never
        } else {
            a.approval_policy
        };
        self.start_turn(&a.thread_id, prompt, approval).await?;
        Ok("resumed")
    }

    async fn control(&self, a: Control, roots: &[PathBuf]) -> Result<Value, String> {
        let t = self.thread(&a.thread_id, roots, false).await?;
        match a.action.as_str() {
            "close" => {
                if t.parent_thread_id.is_none() {
                    return Err("close is only for temporary btw conversations".into());
                }
                if busy(&t) {
                    return Err("interrupt the side conversation before closing it; do not interrupt its parent".into());
                }
                self.client
                    .request("thread/unsubscribe", json!({"threadId": t.thread_id}))
                    .await?;
                self.state.threads.write().await.remove(&t.thread_id);
                Ok(json!({"thread_id": t.thread_id, "action": "closed"}))
            }
            "interrupt" => {
                let turn = t
                    .active_turn_id
                    .as_deref()
                    .ok_or("thread has no active turn")?;
                self.client
                    .request(
                        "turn/interrupt",
                        json!({"threadId": t.thread_id, "turnId": turn}),
                    )
                    .await?;
                Ok(json!({"thread_id": t.thread_id, "action": "interrupt_requested"}))
            }
            "archive" => {
                if t.parent_thread_id.is_some() {
                    return Err("use close for a temporary btw conversation".into());
                }
                if busy(&t) {
                    return Err("interrupt active turn before archiving".into());
                }
                self.client
                    .request("thread/archive", json!({"threadId": t.thread_id}))
                    .await?;
                if let Some(t) = self.state.threads.write().await.get_mut(&a.thread_id) {
                    t.archived = true;
                }
                Ok(json!({"thread_id": t.thread_id, "action": "archived"}))
            }
            "remove_worktree" => {
                if t.parent_thread_id.is_some() {
                    return Err("btw conversations do not own the parent's worktree".into());
                }
                let repo = t
                    .project_path
                    .as_deref()
                    .ok_or("thread has no managed project")?;
                let slug = t
                    .worktree_slug
                    .as_deref()
                    .ok_or("thread is not in a managed worktree")?;
                let root = git::list_managed_worktrees(&self.config.worktree_root, repo)
                    .into_iter()
                    .find(|worktree| worktree.slug == slug)
                    .map(|worktree| PathBuf::from(worktree.path))
                    .ok_or("managed worktree unavailable")?;
                if self
                    .state
                    .threads
                    .read()
                    .await
                    .values()
                    .any(|other| busy(other) && other.cwd.starts_with(&root))
                {
                    return Err("worktree has active sessions; interrupt them first".into());
                }
                let removed =
                    git::remove_managed_worktree(&self.config.worktree_root, repo, slug, a.force)
                        .await?;
                Ok(
                    json!({"thread_id": t.thread_id, "action": "worktree_removed", "path": removed, "branch_retained": true}),
                )
            }
            _ => Err("action must be interrupt, archive, remove_worktree or close".into()),
        }
    }

    async fn wait_summary(&self, id: &str, seconds: u64) -> Result<Value, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
        loop {
            let change = self.state.change_notify.notified();
            tokio::pin!(change);
            change.as_mut().enable();
            let t = self
                .state
                .threads
                .read()
                .await
                .get(id)
                .cloned()
                .ok_or("thread disappeared")?;
            if t.effective_status() != WorkerStatus::Running
                || tokio::time::Instant::now() >= deadline
            {
                return Ok(t.board_summary());
            }
            let _ = tokio::time::timeout_at(deadline, change).await;
        }
    }
}

fn answer_request(request: &PendingServerRequest, a: &Continue) -> Result<Value, String> {
    let decision = a.approval_decision.as_deref();
    match request.method.as_str() {
        "item/tool/requestUserInput" => {
            let answers = a.answers.as_ref().ok_or("answers map is required")?;
            let questions = request.params["questions"]
                .as_array()
                .ok_or("invalid pending questions")?;
            if questions.len() != answers.len()
                || questions
                    .iter()
                    .any(|q| q["id"].as_str().is_none_or(|id| !answers.contains_key(id)))
            {
                return Err("answers must match all pending question ids".into());
            }
            let answers: serde_json::Map<_, _> = answers
                .iter()
                .map(|(id, values)| (id.clone(), json!({"answers": values})))
                .collect();
            Ok(json!({"answers": answers}))
        }
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            let decision = decision.ok_or("approval_decision is required")?;
            if !["accept", "acceptForSession", "decline", "cancel"].contains(&decision) {
                return Err("invalid approval decision".into());
            }
            Ok(json!({"decision": decision}))
        }
        "execCommandApproval" | "applyPatchApproval" => {
            let decision = match decision {
                Some("accept") => json!("approved"),
                Some("acceptForSession") => json!("approved_for_session"),
                Some("decline") => json!({"denied": {"rejection": "Declined by project manager"}}),
                Some("cancel") => json!("abort"),
                _ => return Err("invalid approval decision".into()),
            };
            Ok(json!({"decision": decision}))
        }
        "item/permissions/requestApproval" => match decision {
            Some("accept") => {
                Ok(json!({"permissions": request.params["permissions"], "scope": "turn"}))
            }
            Some("decline") | Some("cancel") => Ok(json!({"permissions": {}, "scope": "turn"})),
            _ => Err(
                "permissions approval accepts accept, decline or cancel (turn scope only)".into(),
            ),
        },
        "mcpServer/elicitation/request" => match decision {
            Some(action @ ("accept" | "decline" | "cancel")) => {
                Ok(json!({"action": action, "content": a.elicitation_content}))
            }
            _ => Err("elicitation requires accept, decline or cancel".into()),
        },
        _ => Err("unsupported server request; interrupt rather than guess a response".into()),
    }
}
