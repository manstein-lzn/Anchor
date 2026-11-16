use crate::git::truncate_chars;
use anchor_file_lease::ProjectLease;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Notify, RwLock};

const MAX_RECENT_ACTIVITIES: usize = 16;
const MAX_MESSAGE_CHARS: usize = 24_000;
const MAX_COMMAND_OUTPUT_CHARS: usize = 8_000;
const MAX_DIFF_CHARS: usize = 12_000;
const CONNECTION_LOST_ERROR: &str = "connection lost; inspect history before resume";
const MAX_NATIVE_EVENTS: usize = 256;
const MAX_NATIVE_EVENT_BYTES: usize = 64 * 1024;
const MAX_NATIVE_BUFFER_BYTES: usize = 1024 * 1024;
static NEXT_STREAM_EPOCH: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
struct NativeEvent<'a> {
    cursor: u64,
    method: &'a str,
    params: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpc_id: Option<&'a Value>,
}

// Store serialized frames, not cloned Value trees, so the byte cap bounds the
// retained payload and a large native event never needs a second full copy.
#[derive(Default)]
struct NativeEventBytes(Vec<u8>);

impl Write for NativeEventBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_NATIVE_EVENT_BYTES - self.0.len() {
            return Err(io::Error::other("native event exceeds byte limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct NativeEventBuffer {
    events: VecDeque<(u64, String)>,
    bytes: usize,
    last_cursor: u64,
}

impl NativeEventBuffer {
    fn push(&mut self, method: &str, params: &Value, request: Option<&PendingServerRequest>) {
        let Some(cursor) = self.last_cursor.checked_add(1) else {
            return;
        };
        let event = NativeEvent {
            cursor,
            method,
            params,
            request_key: request.map(|r| r.request_key.as_str()),
            rpc_id: request.map(|r| &r.rpc_id),
        };
        let mut encoded = NativeEventBytes::default();
        let raw = if serde_json::to_writer(&mut encoded, &event).is_ok() {
            String::from_utf8(encoded.0).expect("JSON serializer emits UTF-8")
        } else {
            json!({
                "cursor": cursor,
                "method": truncate_chars(method, 256),
                "params": null,
                "truncated": true,
                "reason": "native_event_exceeds_byte_limit",
                "max_event_bytes": MAX_NATIVE_EVENT_BYTES,
            })
            .to_string()
        };
        while self.events.len() >= MAX_NATIVE_EVENTS
            || self.bytes + raw.len() > MAX_NATIVE_BUFFER_BYTES
        {
            let (_, removed) = self.events.pop_front().expect("event fits empty buffer");
            self.bytes -= removed.len();
        }
        self.bytes += raw.len();
        self.events.push_back((cursor, raw));
        self.last_cursor = cursor;
    }
}

struct NativeEventStreams {
    epoch: String,
    threads: BTreeMap<String, NativeEventBuffer>,
}

impl Default for NativeEventStreams {
    fn default() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            epoch: format!(
                "{}-{nanos}-{}",
                std::process::id(),
                NEXT_STREAM_EPOCH.fetch_add(1, Ordering::Relaxed)
            ),
            threads: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Idle,
    Unknown,
    Running,
    WaitingInput,
    WaitingApproval,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanStep {
    pub step: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActivityEntry {
    pub kind: String,
    pub summary: String,
    pub status: String,
    pub timestamp_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PendingServerRequest {
    #[serde(skip_serializing)]
    pub rpc_id: Value,
    pub request_key: String,
    pub method: String,
    pub params: Value,
    pub received_at_ms: u64,
}

#[derive(Clone, Debug)]
pub struct TrackedThread {
    pub thread_id: String,
    pub parent_thread_id: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub title: Option<String>,
    pub project_path: Option<PathBuf>,
    pub cwd: PathBuf,
    pub worktree_slug: Option<String>,
    pub git_common_dir: Option<PathBuf>,
    pub mode: String,
    pub network_access: bool,
    pub loaded: bool,
    pub subscribed: bool,
    pub archived: bool,
    pub status: WorkerStatus,
    pub active_turn_id: Option<String>,
    pub admission_epoch: u64,
    pub last_turn_id: Option<String>,
    pub turn_started_at_ms: Option<u64>,
    pub updated_at_ms: u64,
    pub last_error: Option<String>,
    pub plan_explanation: Option<String>,
    pub plan: Vec<PlanStep>,
    pub latest_agent_message: String,
    pub streaming_message_buffer: String,
    pub latest_command_output: String,
    pub latest_diff: String,
    pub recent_activities: VecDeque<ActivityEntry>,
    pub pending_requests: Vec<PendingServerRequest>,
    pub token_usage: Option<Value>,
    /// Reconstructible thread-scoped goal, retained until native reconciliation.
    pub native_goal: Option<Value>,
    pub native_queue_pending: bool,
    pub native_queue_epoch: u64,
}

impl TrackedThread {
    pub fn new(
        thread_id: String,
        title: Option<String>,
        project_path: Option<PathBuf>,
        cwd: PathBuf,
        worktree_slug: Option<String>,
        mode: String,
    ) -> Self {
        Self {
            thread_id,
            parent_thread_id: None,
            model: None,
            reasoning_effort: None,
            title,
            project_path,
            cwd,
            worktree_slug,
            git_common_dir: None,
            mode,
            network_access: false,
            loaded: true,
            subscribed: true,
            archived: false,
            status: WorkerStatus::Idle,
            active_turn_id: None,
            admission_epoch: 0,
            last_turn_id: None,
            turn_started_at_ms: None,
            updated_at_ms: now_ms(),
            last_error: None,
            plan_explanation: None,
            plan: Vec::new(),
            latest_agent_message: String::new(),
            streaming_message_buffer: String::new(),
            latest_command_output: String::new(),
            latest_diff: String::new(),
            recent_activities: VecDeque::new(),
            pending_requests: Vec::new(),
            token_usage: None,
            native_goal: None,
            native_queue_pending: false,
            native_queue_epoch: 0,
        }
    }

    pub fn effective_status(&self) -> WorkerStatus {
        if let Some(req) = self.pending_requests.first() {
            if req.method.ends_with("/requestApproval")
                || matches!(
                    req.method.as_str(),
                    "execCommandApproval" | "applyPatchApproval"
                )
            {
                return WorkerStatus::WaitingApproval;
            }
            return WorkerStatus::WaitingInput;
        }
        self.status
    }

    pub fn push_activity(
        &mut self,
        kind: impl Into<String>,
        summary: String,
        status: impl Into<String>,
    ) {
        let ts = now_ms();
        self.updated_at_ms = ts;
        let summary = truncate_chars(summary.trim(), 240);
        if let Some(back) = self.recent_activities.back_mut()
            && back.summary == summary
        {
            back.status = status.into();
            back.timestamp_ms = ts;
            return;
        }
        if self.recent_activities.len() >= MAX_RECENT_ACTIVITIES {
            self.recent_activities.pop_front();
        }
        self.recent_activities.push_back(ActivityEntry {
            kind: kind.into(),
            summary,
            status: status.into(),
            timestamp_ms: ts,
        });
    }

    pub fn board_summary(&self) -> Value {
        let elapsed_seconds = self
            .turn_started_at_ms
            .filter(|_| {
                matches!(
                    self.effective_status(),
                    WorkerStatus::Unknown
                        | WorkerStatus::Running
                        | WorkerStatus::WaitingInput
                        | WorkerStatus::WaitingApproval
                )
            })
            .map(|started| now_ms().saturating_sub(started) / 1000);
        let latest_reply_preview = truncate_chars(self.latest_message().trim(), 300);
        json!({
            "thread_id": self.thread_id,
            "parent_thread_id": self.parent_thread_id,
            "model": self.model,
            "reasoning_effort": self.reasoning_effort,
            "title": self.title,
            "project": self.project_path.as_ref().map(|p| p.to_string_lossy()),
            "cwd": self.cwd.to_string_lossy(),
            "worktree": self.worktree_slug,
            "mode": self.mode,
            "loaded": self.loaded,
            "subscribed": self.subscribed,
            "archived": self.archived,
            "status": self.effective_status(),
            "active_turn_id": self.active_turn_id,
            "last_turn_id": self.last_turn_id,
            "elapsed_seconds": elapsed_seconds,
            "updated_at_ms": self.updated_at_ms,
            "plan": self.plan,
            "recent_activity": self.recent_activities.back(),
            "pending_requests": self.pending_requests.iter().map(|r| json!({
                "request_key": r.request_key,
                "method": r.method,
                "received_at_ms": r.received_at_ms,
            })).collect::<Vec<_>>(),
            "last_error": self.last_error,
            "latest_reply_preview": latest_reply_preview,
            "token_usage": self.token_usage,
            "native_goal": self.native_goal,
            "native_queue_pending": self.native_queue_pending,
        })
    }

    pub fn inspect_detail(&self, git_snapshot: Option<Value>) -> Value {
        let latest_message = self.latest_message();
        json!({
            "thread_id": self.thread_id,
            "parent_thread_id": self.parent_thread_id,
            "model": self.model,
            "reasoning_effort": self.reasoning_effort,
            "title": self.title,
            "project": self.project_path.as_ref().map(|p| p.to_string_lossy()),
            "cwd": self.cwd.to_string_lossy(),
            "worktree": self.worktree_slug,
            "mode": self.mode,
            "loaded": self.loaded,
            "archived": self.archived,
            "subscribed": self.subscribed,
            "status": self.effective_status(),
            "active_turn_id": self.active_turn_id,
            "last_turn_id": self.last_turn_id,
            "updated_at_ms": self.updated_at_ms,
            "last_error": self.last_error,
            "plan_explanation": self.plan_explanation,
            "plan": self.plan,
            "latest_agent_message": tail_chars(latest_message.trim(), MAX_MESSAGE_CHARS),
            "latest_command_output": tail_chars(self.latest_command_output.trim(), MAX_COMMAND_OUTPUT_CHARS),
            "latest_diff": tail_chars(self.latest_diff.trim(), MAX_DIFF_CHARS),
            "recent_activities": self.recent_activities,
            "pending_requests": self.pending_requests,
            "token_usage": self.token_usage,
            "native_goal": self.native_goal,
            "native_queue_pending": self.native_queue_pending,
            "git": git_snapshot,
        })
    }

    fn latest_message(&self) -> &str {
        if self.streaming_message_buffer.is_empty() {
            &self.latest_agent_message
        } else {
            &self.streaming_message_buffer
        }
    }
}

struct WriterLease {
    guard: Arc<ProjectLease>,
    // Native child turns may resume without a Bridge RPC. Until we can prove
    // their complete lifecycle, only reaping the transport releases this family.
    delegated: bool,
}

#[derive(Default)]
pub struct BridgeState {
    pub threads: RwLock<BTreeMap<String, TrackedThread>>,
    // Guards are execution ownership, never part of a cloned UI/history snapshot.
    // Always take threads before this mutex when both are needed.
    writer_leases: std::sync::Mutex<BTreeMap<String, WriterLease>>,
    pub change_notify: Notify,
    next_request_nonce: AtomicU64,
    native_streams: RwLock<NativeEventStreams>,
}

impl BridgeState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) async fn acquire_writer(
        &self,
        id: &str,
        cwd: &std::path::Path,
        mode: &str,
    ) -> Result<Option<Arc<ProjectLease>>, String> {
        if mode != "workspace-write" {
            return Ok(None);
        }
        let mut threads = self.threads.write().await;
        let thread = threads.get_mut(id).ok_or("thread disappeared")?;
        let mut leases = self.writer_leases.lock().unwrap();
        if let Some(lease) = leases.get(id)
            && lease.guard.path() != cwd.canonicalize().map_err(|e| e.to_string())?
        {
            return Err("project directory changed while its writer lease is held".into());
        }
        if !leases.contains_key(id) {
            let lease = ProjectLease::acquire(cwd).map_err(|e| {
                format!(
                    "project writer lease unavailable for {}: {e}",
                    cwd.display()
                )
            })?;
            leases.insert(
                id.to_owned(),
                WriterLease {
                    guard: Arc::new(lease),
                    delegated: false,
                },
            );
        }
        thread.mode = mode.to_owned();
        if !executing(thread) {
            thread.admission_epoch += 1;
            thread.status = WorkerStatus::Unknown;
        }
        Ok(leases.get(id).map(|lease| lease.guard.clone()))
    }

    pub(crate) fn writer_admission_in_flight(&self, id: &str) -> bool {
        // The map owns one reference; request scopes pin any in-flight admission.
        self.writer_leases
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|lease| Arc::strong_count(&lease.guard) > 1)
    }

    pub(crate) async fn release_idle_writer(&self, id: &str) {
        let threads = self.threads.read().await;
        if let Some(thread) = threads.get(id) {
            self.release_idle_writer_locked(id, thread);
        }
    }

    pub(crate) fn release_idle_writer_locked(&self, id: &str, thread: &TrackedThread) {
        if !busy(thread) && thread.last_error.as_deref() != Some(CONNECTION_LOST_ERROR) {
            let mut leases = self.writer_leases.lock().unwrap();
            if leases.get(id).is_some_and(|lease| !lease.delegated) {
                leases.remove(id);
            }
        }
    }

    fn retain_native_children(
        &self,
        method: &str,
        params: &Value,
        threads: &BTreeMap<String, TrackedThread>,
        thread_id: &str,
    ) {
        let item = &params["item"];
        let child = &params["thread"];
        let collab = matches!(method, "item/started" | "item/completed")
            && item["type"] == "collabAgentToolCall";
        let parent = child["parentThreadId"].as_str().or_else(|| {
            child
                .pointer("/source/subAgent/thread_spawn/parent_thread_id")
                .and_then(Value::as_str)
        });
        let derived = method == "thread/started"
            && (parent.is_some() || child["source"].get("subAgent").is_some());
        let native_root =
            method == "thread/started" && child["source"] == "appServer" && parent.is_none();
        let untracked = !native_root
            && !threads.contains_key(thread_id)
            && matches!(
                method,
                "thread/started"
                    | "turn/started"
                    | "item/started"
                    | "item/completed"
                    | "thread/status/changed"
            );
        if !collab && !derived && !untracked {
            return;
        }
        let owner = if collab {
            item["senderThreadId"].as_str()
        } else {
            parent
        };
        let mut leases = self.writer_leases.lock().unwrap();
        if let Some(lease) = owner.and_then(|id| leases.get_mut(id)) {
            lease.delegated = true;
        } else {
            // No trustworthy parent/receiver mapping: pin every possible owner.
            // Child completed/errored/notFound and thread/closed are not proof
            // that its descendants or independently scheduled work have ended.
            for lease in leases.values_mut() {
                lease.delegated = true;
            }
        }
    }

    pub(crate) fn has_writer_leases(&self) -> bool {
        !self.writer_leases.lock().unwrap().is_empty()
    }

    /// Read an authorized thread's in-memory wire stream, not a recovery log.
    /// The caller must authorize thread/project access before invoking this API.
    /// Keep (thread_id, stream_epoch, next_cursor) together. After an epoch change
    /// (Bridge restart/new state), discard the cursor and read with after=None.
    /// App-server reconnects retain the epoch and cursors. after is exclusive;
    /// limit is capped at 256 (zero reads metadata only). Empty streams start at 0.
    /// gap means an explicit cursor was evicted or is ahead of this stream.
    /// A gap requires history/inspect reconciliation, not automatic replay.
    /// Each thread retains at most 256 events / 1 MiB of serialized payload.
    /// Events over 64 KiB become valid JSON markers with truncated=true and
    /// params=null; request params remain exact in the pending request table.
    /// Non-truncated server-request events also carry request_key and typed rpc_id.
    pub async fn native_events(&self, thread_id: &str, after: Option<u64>, limit: usize) -> Value {
        let streams = self.native_streams.read().await;
        let buffer = streams.threads.get(thread_id);
        let last_cursor = buffer.map_or(0, |b| b.last_cursor);
        let oldest_cursor = buffer.and_then(|b| b.events.front().map(|(cursor, _)| *cursor));
        let gap = after.is_some_and(|cursor| {
            cursor > last_cursor || oldest_cursor.is_some_and(|first| first > 1 && cursor < first)
        });
        let events: Vec<Value> = buffer
            .into_iter()
            .flat_map(|b| &b.events)
            .filter(|(cursor, _)| *cursor > after.unwrap_or(0))
            .take(limit.min(MAX_NATIVE_EVENTS))
            .map(|(_, raw)| serde_json::from_str(raw).expect("stored events are valid JSON"))
            .collect();
        let next_cursor = events
            .last()
            .and_then(|event| event["cursor"].as_u64())
            .unwrap_or(after.unwrap_or(0).min(last_cursor));
        json!({
            "thread_id": thread_id,
            "stream_epoch": streams.epoch,
            "events": events,
            "next_cursor": next_cursor,
            "gap": gap,
            "last_cursor": last_cursor,
            "oldest_cursor": oldest_cursor,
        })
    }

    // Only the transport uses this entrypoint: manager-synthesized reducer
    // updates must not masquerade as raw native notifications.
    pub(crate) async fn apply_native_notification(&self, method: &str, params: &Value) {
        if let Some(thread_id) = extract_thread_id(method, params) {
            self.native_streams
                .write()
                .await
                .threads
                .entry(thread_id)
                .or_default()
                .push(method, params, None);
            self.change_notify.notify_waiters();
        }
        self.apply_notification(method, params).await;
    }

    pub async fn disconnected(&self) {
        let mut threads = self.threads.write().await;
        for thread in threads.values_mut() {
            let was_running = thread.active_turn_id.is_some()
                || matches!(
                    thread.effective_status(),
                    WorkerStatus::Unknown
                        | WorkerStatus::Running
                        | WorkerStatus::WaitingInput
                        | WorkerStatus::WaitingApproval
                );
            thread.loaded = false;
            thread.subscribed = false;
            thread.active_turn_id = None;
            thread.pending_requests.clear();
            if was_running {
                thread.status = WorkerStatus::Failed;
                thread.last_error = Some(CONNECTION_LOST_ERROR.into());
            }
            thread.updated_at_ms = now_ms();
        }
        drop(threads);
        self.change_notify.notify_waiters();
    }

    pub async fn apply_notification(&self, method: &str, params: &Value) {
        let Some(thread_id) = extract_thread_id(method, params) else {
            return;
        };
        let mut threads = self.threads.write().await;
        self.retain_native_children(method, params, &threads, &thread_id);
        if method == "thread/started"
            && !threads.contains_key(&thread_id)
            && let Some(cwd) = params["thread"]["cwd"].as_str()
        {
            let title = params["thread"]["name"].as_str().map(str::to_owned);
            threads.insert(
                thread_id.clone(),
                TrackedThread::new(
                    thread_id.clone(),
                    title,
                    None,
                    PathBuf::from(cwd),
                    None,
                    "read-only".into(),
                ),
            );
        }
        let Some(thread) = threads.get_mut(&thread_id) else {
            return;
        };
        let turn_id = extract_turn_id(params);
        // Goal updates are thread-scoped; their optional turnId is attribution.
        if !matches!(
            method,
            "turn/started" | "thread/goal/updated" | "thread/queue/changed"
        ) && let Some(id) = turn_id
            && let Some(current) = thread
                .active_turn_id
                .as_deref()
                .or(thread.last_turn_id.as_deref())
            && id != current
        {
            return;
        }
        thread.updated_at_ms = now_ms();
        match method {
            "thread/queue/changed" => {
                thread.native_queue_pending = true;
                thread.native_queue_epoch += 1;
            }
            "thread/goal/updated" => {
                if let Some(goal) = params.get("goal").filter(|goal| goal.is_object()) {
                    thread.native_goal = Some(goal.clone());
                }
            }
            "thread/goal/cleared" => {
                thread.native_goal = None;
            }
            "thread/settings/updated" => {
                thread.model = params["threadSettings"]["model"]
                    .as_str()
                    .map(str::to_owned);
                thread.reasoning_effort = params["threadSettings"]["effort"]
                    .as_str()
                    .map(str::to_owned);
            }
            "thread/started" => {
                thread.loaded = true;
                thread.subscribed = true;
            }
            "thread/closed" => {
                thread.loaded = false;
                thread.subscribed = false;
            }
            "thread/name/updated" => {
                if let Some(name) = params["threadName"]
                    .as_str()
                    .or_else(|| params["name"].as_str())
                {
                    thread.title = Some(name.to_owned());
                }
            }
            "thread/archived" => {
                thread.archived = true;
            }
            "thread/unarchived" => {
                thread.archived = false;
            }
            "thread/status/changed" => {
                if params["status"]["type"].as_str() == Some("notLoaded") {
                    thread.loaded = false;
                    thread.subscribed = false;
                } else if params["status"]["type"].as_str() == Some("systemError") {
                    thread.status = WorkerStatus::Failed;
                }
            }
            "turn/started" => {
                let Some(turn_id) = turn_id else {
                    return;
                };
                // The turn/start reply can arrive after its completion notification.
                if thread.last_turn_id.as_deref() == Some(turn_id)
                    || thread.active_turn_id.is_some()
                {
                    return;
                }
                thread.active_turn_id = Some(turn_id.to_owned());
                thread.last_turn_id = Some(turn_id.to_owned());
                thread.status = WorkerStatus::Running;
                thread.turn_started_at_ms = Some(now_ms());
                if thread.last_error.as_deref() != Some(CONNECTION_LOST_ERROR) {
                    thread.last_error = None;
                }
                thread.plan_explanation = None;
                thread.plan.clear();
                thread.latest_diff.clear();
                thread.latest_command_output.clear();
                thread.pending_requests.clear();
                thread.latest_agent_message.clear();
                thread.streaming_message_buffer.clear();
                thread.push_activity("turn", "Turn started".into(), "in_progress");
            }
            "turn/completed" => {
                let Some(id) = turn_id else {
                    return;
                };
                let turn = &params["turn"];
                if !matches!(
                    turn["status"].as_str(),
                    Some("completed" | "interrupted" | "failed")
                ) {
                    return;
                }
                if thread.active_turn_id.is_none() && thread.last_turn_id.as_deref() == turn_id {
                    return;
                }
                thread.last_turn_id = Some(id.to_owned());
                thread.active_turn_id = None;
                thread.pending_requests.clear();
                if !thread.streaming_message_buffer.is_empty() {
                    thread.latest_agent_message =
                        std::mem::take(&mut thread.streaming_message_buffer);
                }
                match turn["status"].as_str() {
                    Some("completed") => {
                        thread.status = WorkerStatus::Completed;
                        thread.push_activity("turn", "Turn completed".into(), "completed");
                    }
                    Some("interrupted") => {
                        thread.status = WorkerStatus::Interrupted;
                        thread.push_activity("turn", "Turn interrupted".into(), "interrupted");
                    }
                    Some("failed") => {
                        thread.status = WorkerStatus::Failed;
                        let reason = turn["error"]["message"]
                            .as_str()
                            .unwrap_or("turn failed")
                            .to_owned();
                        thread.last_error = Some(reason.clone());
                        thread.push_activity("turn", format!("Failed: {reason}"), "failed");
                    }
                    _ => {
                        thread.status = WorkerStatus::Idle;
                    }
                }
            }
            "turn/plan/updated" => {
                thread.plan_explanation = params["explanation"].as_str().map(str::to_owned);
                if let Some(steps) = params["plan"].as_array() {
                    thread.plan = steps
                        .iter()
                        .filter_map(|item| {
                            Some(PlanStep {
                                step: item["step"].as_str()?.to_owned(),
                                status: item["status"].as_str().unwrap_or("pending").to_owned(),
                            })
                        })
                        .collect();
                    thread.push_activity(
                        "plan",
                        format!("Updated plan ({} steps)", thread.plan.len()),
                        "updated",
                    );
                }
            }
            "turn/diff/updated" => {
                if let Some(diff) = params["diff"].as_str() {
                    thread.latest_diff = tail_chars(diff, MAX_DIFF_CHARS).to_owned();
                }
            }
            "item/agentMessage/delta" => {
                if let Some(delta) = params["delta"].as_str() {
                    append_tail(
                        &mut thread.streaming_message_buffer,
                        delta,
                        MAX_MESSAGE_CHARS,
                    );
                }
            }
            "item/commandExecution/outputDelta" => {
                if let Some(delta) = params["delta"].as_str() {
                    append_tail(
                        &mut thread.latest_command_output,
                        delta,
                        MAX_COMMAND_OUTPUT_CHARS,
                    );
                }
            }
            "item/started" => {
                let item = &params["item"];
                match item["type"].as_str() {
                    Some("commandExecution") => {
                        let cmd = item["command"].as_str().unwrap_or("command");
                        thread.latest_command_output.clear();
                        thread.push_activity("command", format!("Running `{cmd}`"), "in_progress");
                    }
                    Some("fileChange") => {
                        thread.push_activity(
                            "file_change",
                            "Applying file changes".into(),
                            "in_progress",
                        );
                    }
                    Some("mcpToolCall") => {
                        let tool = item["tool"].as_str().unwrap_or("mcp_tool");
                        thread.push_activity("mcp", format!("Calling MCP `{tool}`"), "in_progress");
                    }
                    Some("agentMessage") => {
                        thread.streaming_message_buffer.clear();
                    }
                    _ => {}
                }
            }
            "item/completed" => {
                let item = &params["item"];
                match item["type"].as_str() {
                    Some("agentMessage") => {
                        if let Some(text) = item["text"].as_str()
                            && !text.trim().is_empty()
                        {
                            thread.latest_agent_message =
                                tail_chars(text, MAX_MESSAGE_CHARS).to_owned();
                            thread.streaming_message_buffer.clear();
                        }
                    }
                    Some("commandExecution") => {
                        let cmd = item["command"].as_str().unwrap_or("command");
                        let exit = item["exitCode"].as_i64();
                        if let Some(out) = item["aggregatedOutput"].as_str()
                            && !out.is_empty()
                        {
                            thread.latest_command_output =
                                tail_chars(out, MAX_COMMAND_OUTPUT_CHARS).to_owned();
                        }
                        thread.push_activity(
                            "command",
                            format!("Finished `{cmd}` (exit={exit:?})"),
                            if exit == Some(0) {
                                "completed"
                            } else {
                                "failed"
                            },
                        );
                    }
                    Some("fileChange") => {
                        let count = item["changes"].as_array().map(Vec::len).unwrap_or(0);
                        thread.push_activity(
                            "file_change",
                            format!("Updated {count} file(s)"),
                            "completed",
                        );
                    }
                    _ => {}
                }
            }
            "thread/tokenUsage/updated" => {
                thread.token_usage = Some(params["tokenUsage"].clone());
            }
            "serverRequest/resolved" => {
                let req_id = &params["requestId"];
                thread.pending_requests.retain(|r| &r.rpc_id != req_id);
            }
            "error" => {
                if let Some(msg) = params["error"]["message"]
                    .as_str()
                    .or_else(|| params["message"].as_str())
                {
                    thread.last_error = Some(msg.to_owned());
                }
            }
            _ => {}
        }
        if matches!(
            method,
            "turn/completed" | "thread/goal/cleared" | "thread/goal/updated"
        ) && !busy(thread)
        {
            self.release_idle_writer_locked(&thread_id, thread);
        }
        drop(threads);
        self.change_notify.notify_waiters();
    }

    pub async fn record_server_request(
        &self,
        rpc_id: Value,
        method: String,
        params: Value,
    ) -> bool {
        if !rpc_id.is_string() && !rpc_id.is_i64() && !rpc_id.is_u64() {
            return false;
        }
        let Some(thread_id) = extract_thread_id(&method, &params) else {
            return false;
        };
        let mut threads = self.threads.write().await;
        let Some(thread) = threads.get_mut(&thread_id) else {
            return false;
        };
        if !thread.loaded
            || (extract_turn_id(&params).is_some()
                && extract_turn_id(&params) != thread.active_turn_id.as_deref())
        {
            return false;
        }
        if thread.pending_requests.iter().any(|r| r.rpc_id == rpc_id) {
            return true;
        }
        // Never reset on disconnect: a cached manager response must not answer
        // a reused server ID. JSON spelling also distinguishes 1 from "1".
        let Ok(nonce) =
            self.next_request_nonce
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        else {
            return false;
        };
        let request_key = format!("{nonce}:{rpc_id}");
        thread.push_activity(
            "server_request",
            format!("Awaiting decision for `{method}`"),
            "waiting",
        );
        let request = PendingServerRequest {
            rpc_id,
            request_key,
            method,
            params,
            received_at_ms: now_ms(),
        };
        self.native_streams
            .write()
            .await
            .threads
            .entry(thread_id)
            .or_default()
            .push(&request.method, &request.params, Some(&request));
        thread.pending_requests.push(request);
        drop(threads);
        self.change_notify.notify_waiters();
        true
    }
}

pub(crate) fn busy(t: &TrackedThread) -> bool {
    (t.loaded
        && (t.native_queue_pending
            || t.native_goal
                .as_ref()
                .is_some_and(|g| g["status"] == "active")))
        || executing(t)
}

pub(crate) fn executing(t: &TrackedThread) -> bool {
    t.active_turn_id.is_some()
        || matches!(
            t.effective_status(),
            WorkerStatus::Unknown
                | WorkerStatus::Running
                | WorkerStatus::WaitingInput
                | WorkerStatus::WaitingApproval
        )
}

fn extract_thread_id(method: &str, params: &Value) -> Option<String> {
    if method == "thread/started" {
        return params["thread"]["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
    }
    params["threadId"]
        .as_str()
        .or_else(|| params["conversationId"].as_str())
        .or_else(|| params["conversation_id"].as_str())
        .or_else(|| params["thread_id"].as_str())
        .or_else(|| params["thread"]["id"].as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn extract_turn_id(params: &Value) -> Option<&str> {
    params["turn"]["id"]
        .as_str()
        .or_else(|| params["turnId"].as_str())
}

fn tail_chars(input: &str, limit: usize) -> &str {
    let start = input
        .char_indices()
        .rev()
        .nth(limit)
        .map_or(0, |(offset, ch)| offset + ch.len_utf8());
    &input[start..]
}

fn append_tail(buffer: &mut String, delta: &str, limit: usize) {
    buffer.push_str(tail_chars(delta, limit));
    let keep = tail_chars(buffer, limit).len();
    buffer.drain(..buffer.len() - keep);
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delegated_and_unknown_children_keep_parent_lease_past_terminal() {
        for source in [
            json!({"parentThreadId":"writer","source":"appServer"}),
            json!({"source":{"subAgent":{"thread_spawn":{"parent_thread_id":"writer","depth":1}}}}),
            json!({"source":{"subAgent":"review"}}),
            json!({"source":"unknown"}),
        ] {
            let root = tempfile::tempdir().unwrap();
            let state = BridgeState::new();
            state.threads.write().await.insert(
                "writer".into(),
                TrackedThread::new(
                    "writer".into(),
                    None,
                    None,
                    root.path().to_owned(),
                    None,
                    "workspace-write".into(),
                ),
            );
            drop(
                state
                    .acquire_writer("writer", root.path(), "workspace-write")
                    .await
                    .unwrap(),
            );
            state
                .apply_notification(
                    "turn/started",
                    &json!({"threadId":"writer","turn":{"id":"parent-turn"}}),
                )
                .await;
            let mut child = source;
            child["id"] = json!("child");
            child["cwd"] = json!(root.path());
            state
                .apply_native_notification("thread/started", &json!({"thread":child}))
                .await;
            state
                .apply_native_notification(
                    "turn/started",
                    &json!({"threadId":"child","turn":{"id":"child-turn"}}),
                )
                .await;
            state
                .apply_notification(
                    "turn/completed",
                    &json!({"threadId":"writer","turn":{"id":"parent-turn","status":"completed"}}),
                )
                .await;
            state.release_idle_writer("writer").await;
            assert!(!state.writer_admission_in_flight("writer"));
            assert!(ProjectLease::acquire(root.path()).is_err());
            for id in ["stale", "child-turn"] {
                state
                    .apply_native_notification(
                        "turn/completed",
                        &json!({"threadId":"child","turn":{"id":id,"status":"completed"}}),
                    )
                    .await;
                assert!(ProjectLease::acquire(root.path()).is_err());
            }
            state
                .apply_native_notification("thread/closed", &json!({"threadId":"child"}))
                .await;
            state.disconnected().await;
            state.release_idle_writer("writer").await;
            assert!(ProjectLease::acquire(root.path()).is_err());
            drop(state);
            ProjectLease::acquire(root.path()).unwrap();
        }
    }

    #[tokio::test]
    async fn collab_without_child_registration_pins_only_known_writer_or_all_if_unknown() {
        for sender in ["writer", "untracked-child"] {
            let root = tempfile::tempdir().unwrap();
            let state = BridgeState::new();
            for id in ["writer", "sibling"] {
                let cwd = root.path().join(id);
                std::fs::create_dir(&cwd).unwrap();
                state.threads.write().await.insert(
                    id.into(),
                    TrackedThread::new(
                        id.into(),
                        None,
                        None,
                        cwd.clone(),
                        None,
                        "workspace-write".into(),
                    ),
                );
                drop(
                    state
                        .acquire_writer(id, &cwd, "workspace-write")
                        .await
                        .unwrap(),
                );
                state
                    .apply_notification(
                        "turn/started",
                        &json!({"threadId":id,"turn":{"id":"turn"}}),
                    )
                    .await;
            }
            state.apply_native_notification("item/started", &json!({"threadId":sender,"turnId":"turn","item":{
                "id":"spawn","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress",
                "senderThreadId":sender,"receiverThreadIds":[],"agentsStates":{}
            }})).await;
            for id in ["writer", "sibling"] {
                state
                    .apply_notification(
                        "turn/completed",
                        &json!({"threadId":id,"turn":{"id":"turn","status":"completed"}}),
                    )
                    .await;
            }
            assert!(ProjectLease::acquire(&root.path().join("writer")).is_err());
            assert_eq!(
                ProjectLease::acquire(&root.path().join("sibling")).is_err(),
                sender == "untracked-child"
            );
            drop(state);
            ProjectLease::acquire(root.path()).unwrap();
        }
    }

    #[tokio::test]
    async fn project_guard_survives_pending_queue_disconnect_and_stale_completion() {
        let root = tempfile::tempdir().unwrap();
        let state = BridgeState::new();
        state.threads.write().await.insert(
            "writer".into(),
            TrackedThread::new(
                "writer".into(),
                None,
                None,
                root.path().to_owned(),
                None,
                "workspace-write".into(),
            ),
        );
        state
            .acquire_writer("writer", root.path(), "workspace-write")
            .await
            .unwrap();
        state
            .apply_notification(
                "turn/started",
                &json!({"threadId":"writer","turn":{"id":"live"}}),
            )
            .await;
        state
            .record_server_request(
                json!(1),
                "item/tool/requestUserInput".into(),
                json!({"threadId":"writer","turnId":"live"}),
            )
            .await;
        state
            .apply_notification(
                "turn/completed",
                &json!({"threadId":"writer","turn":{"id":"stale","status":"completed"}}),
            )
            .await;
        assert!(ProjectLease::acquire(root.path()).is_err());
        state
            .apply_notification("thread/queue/changed", &json!({"threadId":"writer"}))
            .await;
        state
            .apply_notification(
                "turn/completed",
                &json!({"threadId":"writer","turn":{"id":"live","status":"completed"}}),
            )
            .await;
        assert!(ProjectLease::acquire(root.path()).is_err());
        {
            let mut threads = state.threads.write().await;
            let thread = threads.get_mut("writer").unwrap();
            thread.native_queue_pending = false;
            thread.native_goal = Some(json!({"status":"active"}));
        }
        state.release_idle_writer("writer").await;
        assert!(ProjectLease::acquire(root.path()).is_err());
        state
            .apply_notification("thread/goal/cleared", &json!({"threadId":"writer"}))
            .await;
        ProjectLease::acquire(root.path()).unwrap();
        let pin = state
            .acquire_writer("writer", root.path(), "workspace-write")
            .await
            .unwrap();
        assert!(state.writer_admission_in_flight("writer"));
        state
            .apply_notification(
                "turn/completed",
                &json!({"threadId":"writer","turn":{"id":"unknown-new","status":"unexpected"}}),
            )
            .await;
        state
            .apply_notification(
                "turn/completed",
                &json!({"threadId":"writer","turn":{"id":"live","status":"completed"}}),
            )
            .await;
        drop(pin);
        assert!(ProjectLease::acquire(root.path()).is_err());
        state.disconnected().await;
        state.release_idle_writer("writer").await;
        assert!(ProjectLease::acquire(root.path()).is_err());
        drop(state);
        ProjectLease::acquire(root.path()).unwrap();
    }

    #[tokio::test]
    async fn native_goal_projection_survives_turns_and_disconnect_until_explicit_clear() {
        let state = state().await;
        assert!(state.threads.read().await["t"].native_goal.is_none());
        let mut goal = json!({
            "threadId": "t", "objective": "Verify recovery", "status": "active",
            "tokenBudget": null, "tokensUsed": 12, "timeUsedSeconds": 3,
            "createdAt": 1, "updatedAt": 2
        });
        let updated = json!({"threadId": "t", "turnId": null, "goal": goal});
        state
            .apply_native_notification("thread/goal/updated", &updated)
            .await;
        start(&state, "first").await;
        complete(&state, "first").await;
        assert_eq!(
            state.threads.read().await["t"].native_goal,
            Some(goal.clone())
        );
        start(&state, "second").await;
        assert_eq!(
            state.threads.read().await["t"].native_goal,
            Some(goal.clone())
        );
        // A thread-level update can be attributed to an already-finished turn.
        goal["tokensUsed"] = json!(24);
        state
            .apply_native_notification(
                "thread/goal/updated",
                &json!({"threadId": "t", "turnId": "first", "goal": goal}),
            )
            .await;
        assert_eq!(
            state.threads.read().await["t"].native_goal,
            Some(goal.clone())
        );
        for status in ["idle", "notLoaded", "systemError"] {
            state
                .apply_notification(
                    "thread/status/changed",
                    &json!({"threadId": "t", "status": {"type": status}}),
                )
                .await;
            assert_eq!(
                state.threads.read().await["t"].native_goal,
                Some(goal.clone())
            );
        }
        state
            .apply_notification("thread/closed", &json!({"threadId": "t"}))
            .await;
        state.disconnected().await;
        {
            let threads = state.threads.read().await;
            let thread = &threads["t"];
            assert!(!thread.loaded && !thread.subscribed);
            assert_eq!(thread.native_goal, Some(goal.clone()));
            assert_eq!(thread.board_summary()["native_goal"], goal);
            assert_eq!(thread.inspect_detail(None)["native_goal"], goal);
            assert_eq!(thread.last_error.as_deref(), Some(CONNECTION_LOST_ERROR));
        }
        // Completion updates retain the exact goal rather than erasing it.
        goal["status"] = json!("complete");
        state
            .apply_native_notification(
                "thread/goal/updated",
                &json!({"threadId": "t", "goal": goal}),
            )
            .await;
        for params in [
            json!({"threadId": "t"}),
            json!({"threadId": "t", "goal": null}),
        ] {
            state
                .apply_notification("thread/goal/updated", &params)
                .await;
        }
        assert_eq!(state.threads.read().await["t"].native_goal, Some(goal));
        let cleared = json!({"threadId": "t"});
        state
            .apply_native_notification("thread/goal/cleared", &cleared)
            .await;
        {
            let threads = state.threads.read().await;
            let thread = &threads["t"];
            assert!(thread.native_goal.is_none());
            assert_eq!(thread.board_summary()["native_goal"], Value::Null);
            assert_eq!(thread.inspect_detail(None)["native_goal"], Value::Null);
        }
        let events = state.native_events("t", None, 10).await;
        assert_eq!(events["events"][0]["params"], updated);
        assert_eq!(events["events"][3]["method"], "thread/goal/cleared");
        assert_eq!(events["events"][3]["params"], cleared);
    }

    #[tokio::test]
    async fn native_events_capture_wire_before_reducer_filters_without_global_disclosure() {
        let state = state().await;
        start(&state, "current").await;
        // Synthetic manager updates must not be presented as native wire events.
        assert_eq!(state.native_events("t", None, 10).await["last_cursor"], 0);
        let stale = json!({"threadId": "t", "turn": {"id": "old", "status": "completed"}});
        state
            .apply_native_notification("turn/completed", &stale)
            .await;
        let future = json!({"threadId": "t", "unrecognized": {"array": [null, true, 3], "text": "native\nvalue"}});
        state
            .apply_native_notification("future/native/event", &future)
            .await;
        state
            .apply_native_notification(
                "future/native/event",
                &json!({"thread": {"id": "not-yet-tracked"}, "data": 7}),
            )
            .await;
        for params in [
            json!({"account": "host-wide"}),
            json!({"threadId": ""}),
            json!({"data": {"threadId": "t"}}),
        ] {
            state
                .apply_native_notification("account/updated", &params)
                .await;
        }
        let page = state.native_events("t", None, 10).await;
        assert_eq!(
            page["events"],
            json!([
                {"cursor": 1, "method": "turn/completed", "params": stale},
                {"cursor": 2, "method": "future/native/event", "params": future},
            ])
        );
        assert_eq!(page["next_cursor"], 2);
        assert_eq!(page["gap"], false);
        assert_eq!(
            state.threads.read().await["t"].active_turn_id.as_deref(),
            Some("current")
        );
        assert!(!state.threads.read().await.contains_key("not-yet-tracked"));
        assert_eq!(
            state.native_events("not-yet-tracked", None, 10).await["events"][0]["params"]["data"],
            7
        );
        assert_eq!(state.native_events("", None, 10).await["last_cursor"], 0);
        assert_eq!(
            state.native_events("absent", None, 10).await["events"],
            json!([])
        );
    }

    #[tokio::test]
    async fn native_events_paginate_exclusively_and_report_evicted_or_ahead_cursors() {
        let state = BridgeState::new();
        for i in 1..=MAX_NATIVE_EVENTS + 5 {
            state
                .apply_native_notification("future/event", &json!({"threadId": "t", "i": i}))
                .await;
        }
        let first = state.native_events("t", None, 2).await;
        assert_eq!(first["oldest_cursor"], 6);
        assert_eq!(first["last_cursor"], 261);
        assert_eq!(first["next_cursor"], 7);
        assert_eq!(first["gap"], false);
        assert_eq!(first["events"][0]["params"]["i"], 6);
        let next = state.native_events("t", Some(7), 2).await;
        assert_eq!(next["events"][0]["cursor"], 8);
        assert_eq!(next["next_cursor"], 9);
        assert_eq!(next["gap"], false);
        let evicted = state.native_events("t", Some(5), usize::MAX).await;
        assert_eq!(evicted["gap"], true);
        assert_eq!(
            evicted["events"].as_array().unwrap().len(),
            MAX_NATIVE_EVENTS
        );
        assert_eq!(evicted["next_cursor"], 261);
        let zero = state.native_events("t", Some(10), 0).await;
        assert_eq!(zero["events"], json!([]));
        assert_eq!(zero["next_cursor"], 10);
        let caught_up = state.native_events("t", Some(261), 10).await;
        assert_eq!(caught_up["events"], json!([]));
        assert_eq!(caught_up["next_cursor"], 261);
        assert_eq!(caught_up["gap"], false);
        let ahead = state.native_events("t", Some(u64::MAX), 10).await;
        assert_eq!(ahead["gap"], true);
        assert_eq!(ahead["events"], json!([]));
        assert_eq!(ahead["next_cursor"], 261);
        assert_eq!(state.native_events("empty", Some(1), 10).await["gap"], true);
        let empty = state.native_events("empty", None, 0).await;
        assert_eq!(empty["next_cursor"], 0);
        assert_eq!(empty["last_cursor"], 0);
        assert_eq!(empty["oldest_cursor"], Value::Null);
    }

    #[tokio::test]
    async fn native_events_byte_eviction_and_oversized_markers_keep_valid_bounded_json() {
        let state = BridgeState::new();
        let params = json!({"threadId": "t", "text": "\"\n\u{754c}".repeat(8_000)});
        for _ in 0..30 {
            state
                .apply_native_notification("future/event", &params)
                .await;
        }
        {
            let streams = state.native_streams.read().await;
            let buffer = &streams.threads["t"];
            assert!(buffer.bytes <= MAX_NATIVE_BUFFER_BYTES);
            assert!(buffer.events.len() < 30);
            assert_eq!(
                buffer.bytes,
                buffer
                    .events
                    .iter()
                    .map(|(_, raw)| raw.len())
                    .sum::<usize>()
            );
        }
        let page = state.native_events("t", Some(0), MAX_NATIVE_EVENTS).await;
        assert_eq!(page["gap"], true);
        assert_eq!(page["events"][0]["params"], params);
        let huge = json!({"threadId": "t", "text": "\u{754c}".repeat(MAX_NATIVE_EVENT_BYTES)});
        state.apply_native_notification("future/large", &huge).await;
        state
            .apply_native_notification(
                &"\u{754c}".repeat(MAX_NATIVE_EVENT_BYTES),
                &json!({"threadId": "t"}),
            )
            .await;
        let markers = state.native_events("t", Some(30), 10).await;
        assert_eq!(markers["events"].as_array().unwrap().len(), 2);
        for marker in markers["events"].as_array().unwrap() {
            assert_eq!(marker["truncated"], true);
            assert_eq!(marker["params"], Value::Null);
            assert_eq!(marker["reason"], "native_event_exceeds_byte_limit");
            let encoded = serde_json::to_string(marker).unwrap();
            assert!(encoded.len() <= MAX_NATIVE_EVENT_BYTES);
            assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), *marker);
        }
        assert_eq!(markers["events"][0]["method"], "future/large");
        assert_eq!(markers["next_cursor"], 32);
    }

    #[tokio::test]
    async fn native_events_epoch_and_cursors_survive_disconnect_and_snapshot_replacement() {
        let state = state().await;
        state
            .apply_native_notification("future/event", &json!({"threadId": "t"}))
            .await;
        let before = state.native_events("t", None, 10).await;
        state.disconnected().await;
        state.threads.write().await.insert(
            "t".into(),
            TrackedThread::new(
                "t".into(),
                None,
                None,
                PathBuf::from("/tmp"),
                None,
                "read-only".into(),
            ),
        );
        state
            .apply_native_notification("future/event", &json!({"threadId": "t"}))
            .await;
        let after = state.native_events("t", Some(1), 10).await;
        assert_eq!(before["stream_epoch"], after["stream_epoch"]);
        assert_eq!(after["next_cursor"], 2);
        assert_eq!(
            state.native_events("other", None, 10).await["stream_epoch"],
            after["stream_epoch"]
        );
        let restarted = BridgeState::new().native_events("t", None, 10).await;
        assert_ne!(restarted["stream_epoch"], after["stream_epoch"]);
        assert_eq!(restarted["last_cursor"], 0);
    }

    #[tokio::test]
    async fn native_request_events_correlate_without_modifying_pending_params() {
        let state = state().await;
        for (id, params) in [
            (
                json!(1),
                json!({"threadId": "t", "arguments": {"key": [1, null]}}),
            ),
            (
                json!("1"),
                json!({"threadId": "t", "text": "x".repeat(MAX_NATIVE_EVENT_BYTES)}),
            ),
        ] {
            assert!(
                state
                    .record_server_request(id.clone(), "future/request".into(), params.clone())
                    .await
            );
            // A duplicate pending wire ID does not create a second actionable request.
            assert!(
                state
                    .record_server_request(id.clone(), "future/request".into(), params.clone())
                    .await
            );
            let threads = state.threads.read().await;
            let request = threads["t"]
                .pending_requests
                .iter()
                .find(|r| r.rpc_id == id)
                .unwrap();
            assert_eq!(request.params, params);
            assert_eq!(threads["t"].effective_status(), WorkerStatus::WaitingInput);
        }
        let page = state.native_events("t", None, 10).await;
        assert_eq!(page["events"].as_array().unwrap().len(), 2);
        let threads = state.threads.read().await;
        let first = &threads["t"].pending_requests[0];
        assert_eq!(page["events"][0]["rpc_id"], first.rpc_id);
        assert_eq!(page["events"][0]["request_key"], first.request_key);
        assert_eq!(page["events"][0]["params"], first.params);
        assert_eq!(page["events"][1]["truncated"], true);
        assert_eq!(
            threads["t"].pending_requests[1].params["text"]
                .as_str()
                .unwrap()
                .len(),
            MAX_NATIVE_EVENT_BYTES
        );
    }

    #[tokio::test]
    async fn native_events_wake_waiters_even_when_reducer_does_not_track_thread() {
        let state = BridgeState::new();
        let notified = state.change_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        state
            .apply_native_notification("future/event", &json!({"thread_id": "untracked"}))
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(1), notified)
            .await
            .unwrap();
        assert_eq!(
            state.native_events("untracked", None, 10).await["last_cursor"],
            1
        );
        assert!(state.threads.read().await.is_empty());
    }

    async fn state() -> Arc<BridgeState> {
        let state = BridgeState::new();
        state
            .apply_notification(
                "thread/started",
                &json!({
                    "thread": {"id": "t", "cwd": "/tmp/project"}
                }),
            )
            .await;
        state
    }

    async fn start(state: &BridgeState, id: &str) {
        state
            .apply_notification(
                "turn/started",
                &json!({
                    "threadId": "t", "turn": {"id": id}
                }),
            )
            .await;
    }

    async fn complete(state: &BridgeState, id: &str) {
        state
            .apply_notification(
                "turn/completed",
                &json!({
                    "threadId": "t", "turn": {"id": id, "status": "completed"}
                }),
            )
            .await;
    }

    #[tokio::test]
    async fn disconnect_clears_live_state_and_wakes_waiters_without_erasing_history() {
        let state = state().await;
        start(&state, "old").await;
        assert!(
            state
                .record_server_request(
                    json!(7),
                    "item/tool/requestUserInput".into(),
                    json!({"threadId": "t", "turnId": "old"})
                )
                .await
        );
        {
            let mut threads = state.threads.write().await;
            threads.get_mut("t").unwrap().latest_agent_message = "retained history".into();
            let mut done = TrackedThread::new(
                "done".into(),
                None,
                None,
                PathBuf::from("/tmp"),
                None,
                "read-only".into(),
            );
            done.status = WorkerStatus::Completed;
            done.last_error = Some("retained error".into());
            threads.insert("done".into(), done);
        }
        let notified = state.change_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        state.disconnected().await;
        tokio::time::timeout(std::time::Duration::from_secs(1), notified)
            .await
            .unwrap();
        {
            let threads = state.threads.read().await;
            assert!(
                threads.values().all(|t| !t.loaded
                    && t.active_turn_id.is_none()
                    && t.pending_requests.is_empty())
            );
            let t = &threads["t"];
            assert_eq!(t.status, WorkerStatus::Failed);
            assert_eq!(t.last_error.as_deref(), Some(CONNECTION_LOST_ERROR));
            assert_eq!(t.last_turn_id.as_deref(), Some("old"));
            assert_eq!(t.latest_agent_message, "retained history");
            assert_eq!(threads["done"].status, WorkerStatus::Completed);
            assert_eq!(
                threads["done"].last_error.as_deref(),
                Some("retained error")
            );
        }
        state.disconnected().await;
        complete(&state, "old").await;
        start(&state, "old").await;
        assert_eq!(state.threads.read().await["t"].status, WorkerStatus::Failed);
        state
            .apply_notification(
                "thread/started",
                &json!({"thread": {"id": "t", "cwd": "/tmp/project"}}),
            )
            .await;
        assert_eq!(
            state.threads.read().await["t"].last_error.as_deref(),
            Some(CONNECTION_LOST_ERROR)
        );
    }

    #[tokio::test]
    async fn new_turn_resets_projection_and_ignores_late_completion_and_duplicate_start() {
        let state = state().await;
        start(&state, "old").await;
        complete(&state, "old").await;
        {
            let mut threads = state.threads.write().await;
            let t = threads.get_mut("t").unwrap();
            t.plan_explanation = Some("old plan".into());
            t.plan.push(PlanStep {
                step: "old".into(),
                status: "completed".into(),
            });
            t.latest_diff = "old diff".into();
            t.latest_command_output = "old output".into();
            t.latest_agent_message = "old answer".into();
            t.streaming_message_buffer = "old stream".into();
        }
        start(&state, "new").await;
        {
            let threads = state.threads.read().await;
            let t = &threads["t"];
            assert!(t.plan.is_empty() && t.plan_explanation.is_none());
            assert!(t.latest_diff.is_empty() && t.latest_command_output.is_empty());
            assert!(t.latest_agent_message.is_empty() && t.streaming_message_buffer.is_empty());
        }
        state
            .apply_notification(
                "item/agentMessage/delta",
                &json!({"threadId": "t", "turnId": "new", "delta": "current"}),
            )
            .await;
        assert!(
            state
                .record_server_request(
                    json!(9),
                    "item/tool/requestUserInput".into(),
                    json!({"threadId": "t", "turnId": "new"})
                )
                .await
        );
        for id in ["old", "new"] {
            start(&state, id).await;
        }
        complete(&state, "old").await;
        state
            .apply_notification(
                "turn/completed",
                &json!({"threadId": "t", "turn": {"status": "completed"}}),
            )
            .await;
        state
            .apply_notification(
                "item/agentMessage/delta",
                &json!({"threadId": "t", "turnId": "old", "delta": "stale"}),
            )
            .await;
        {
            let threads = state.threads.read().await;
            let t = &threads["t"];
            assert_eq!(t.active_turn_id.as_deref(), Some("new"));
            assert_eq!(t.streaming_message_buffer, "current");
            assert_eq!(t.pending_requests.len(), 1);
        }
        complete(&state, "new").await;
        start(&state, "new").await;
        let threads = state.threads.read().await;
        let t = &threads["t"];
        assert_eq!(t.status, WorkerStatus::Completed);
        assert!(t.active_turn_id.is_none());
        assert!(t.pending_requests.is_empty());
        assert_eq!(t.latest_agent_message, "current");
    }

    #[tokio::test]
    async fn completion_before_synthetic_start_is_terminal() {
        let state = state().await;
        complete(&state, "fast").await;
        start(&state, "fast").await;
        let threads = state.threads.read().await;
        assert_eq!(threads["t"].status, WorkerStatus::Completed);
        assert_eq!(threads["t"].last_turn_id.as_deref(), Some("fast"));
        assert!(threads["t"].active_turn_id.is_none());
    }

    #[tokio::test]
    async fn streaming_message_wins_over_previous_commentary_through_completion() {
        let state = state().await;
        start(&state, "turn").await;
        state
            .apply_notification(
                "item/completed",
                &json!({"threadId": "t", "item": {"type": "agentMessage", "text": "commentary"}}),
            )
            .await;
        state
            .apply_notification(
                "item/started",
                &json!({"threadId": "t", "item": {"type": "agentMessage"}}),
            )
            .await;
        state
            .apply_notification(
                "item/agentMessage/delta",
                &json!({"threadId": "t", "delta": "current answer"}),
            )
            .await;
        {
            let threads = state.threads.read().await;
            assert_eq!(
                threads["t"].board_summary()["latest_reply_preview"],
                "current answer"
            );
            assert_eq!(
                threads["t"].inspect_detail(None)["latest_agent_message"],
                "current answer"
            );
        }
        complete(&state, "turn").await;
        assert_eq!(
            state.threads.read().await["t"].latest_agent_message,
            "current answer"
        );
    }

    #[tokio::test]
    async fn projection_buffers_keep_exact_unicode_tails() {
        let state = state().await;
        let message = format!("{}END", "界".repeat(MAX_MESSAGE_CHARS + 20));
        let output = format!("{}TAIL", "é".repeat(MAX_COMMAND_OUTPUT_CHARS + 20));
        let diff = format!("{}LAST", "界".repeat(MAX_DIFF_CHARS + 20));
        state
            .apply_notification(
                "item/agentMessage/delta",
                &json!({"threadId": "t", "delta": message}),
            )
            .await;
        state
            .apply_notification(
                "item/commandExecution/outputDelta",
                &json!({"threadId": "t", "delta": output}),
            )
            .await;
        state
            .apply_notification("turn/diff/updated", &json!({"threadId": "t", "diff": diff}))
            .await;
        {
            let threads = state.threads.read().await;
            let t = &threads["t"];
            assert_eq!(
                t.streaming_message_buffer,
                tail_chars(&message, MAX_MESSAGE_CHARS)
            );
            assert_eq!(
                t.latest_command_output,
                tail_chars(&output, MAX_COMMAND_OUTPUT_CHARS)
            );
            assert_eq!(t.latest_diff, tail_chars(&diff, MAX_DIFF_CHARS));
        }
        state.apply_notification("item/completed", &json!({"threadId": "t", "item": {"type": "commandExecution", "aggregatedOutput": output, "exitCode": 0}})).await;
        state
            .apply_notification(
                "item/completed",
                &json!({"threadId": "t", "item": {"type": "agentMessage", "text": message}}),
            )
            .await;
        let threads = state.threads.read().await;
        let t = &threads["t"];
        assert_eq!(
            t.latest_agent_message,
            tail_chars(&message, MAX_MESSAGE_CHARS)
        );
        assert_eq!(
            t.latest_command_output,
            tail_chars(&output, MAX_COMMAND_OUTPUT_CHARS)
        );
        assert_eq!(t.latest_agent_message.chars().count(), MAX_MESSAGE_CHARS);
        assert_eq!(
            t.latest_command_output.chars().count(),
            MAX_COMMAND_OUTPUT_CHARS
        );
        assert_eq!(tail_chars("abc", 0), "");
        let mut chunks = String::new();
        for chunk in ["a界", "é", "tail"] {
            append_tail(&mut chunks, chunk, 4);
        }
        assert_eq!(chunks, "tail");
    }

    #[tokio::test]
    async fn request_keys_preserve_wire_types_and_legacy_conversation_names() {
        let state = state().await;
        for (id, method, params) in [
            (
                json!(1),
                "execCommandApproval",
                json!({"conversationId": "t"}),
            ),
            (
                json!("1"),
                "applyPatchApproval",
                json!({"conversation_id": "t"}),
            ),
        ] {
            assert!(
                state
                    .record_server_request(id.clone(), method.into(), params.clone())
                    .await
            );
            assert!(state.record_server_request(id, method.into(), params).await);
        }
        {
            let threads = state.threads.read().await;
            let t = &threads["t"];
            assert_eq!(t.pending_requests.len(), 2);
            assert_eq!(t.pending_requests[0].request_key, "0:1");
            assert_eq!(t.pending_requests[1].request_key, "1:\"1\"");
            assert_eq!(t.effective_status(), WorkerStatus::WaitingApproval);
            assert_eq!(t.mode, "read-only");
            assert!(!t.network_access);
        }
        state
            .apply_notification(
                "serverRequest/resolved",
                &json!({"threadId": "t", "requestId": 1}),
            )
            .await;
        assert_eq!(
            state.threads.read().await["t"].pending_requests[0].rpc_id,
            json!("1")
        );
        assert!(
            !state
                .record_server_request(
                    Value::Null,
                    "execCommandApproval".into(),
                    json!({"conversationId": "t"})
                )
                .await
        );
        state.disconnected().await;
        assert!(
            !state
                .record_server_request(
                    json!(2),
                    "execCommandApproval".into(),
                    json!({"conversationId": "t"})
                )
                .await
        );
    }
}
