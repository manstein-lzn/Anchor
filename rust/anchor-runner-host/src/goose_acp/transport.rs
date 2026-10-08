use std::{
    collections::VecDeque,
    future::{Future, pending},
    io,
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};

use anchor_runtime::Cancellation;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    task::JoinHandle,
    time::{Instant, sleep, sleep_until, timeout},
};

const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;
const MAX_NOTIFICATIONS: usize = 4096;
const MAX_NOTIFICATION_BYTES: usize = 64 * 1024 * 1024;
const CANCELLATION_POLL: Duration = Duration::from_millis(10);
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const STDERR_CLOSE_TIMEOUT: Duration = Duration::from_millis(250);

type NotificationObserver<'observer> =
    dyn Fn(&Value) -> Result<(), String> + Send + Sync + 'observer;

type PendingResponse = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;

pub(crate) trait ServerRequestHandler: Send + Sync {
    fn handle<'request>(
        &'request self,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'request>>;
}

pub(super) enum NotificationRetention {
    Strict,
    Tail,
}

pub(super) struct NotificationBuffer {
    frames: VecDeque<(Value, usize)>,
    bytes: usize,
    dropped: usize,
    retention: NotificationRetention,
}

impl NotificationBuffer {
    pub(super) fn new(retention: NotificationRetention) -> Self {
        Self {
            frames: VecDeque::new(),
            bytes: 0,
            dropped: 0,
            retention,
        }
    }

    pub(super) fn push(&mut self, message: Value, frame_bytes: usize) -> Result<(), String> {
        match self.retention {
            NotificationRetention::Strict => {
                if self.frames.len() >= MAX_NOTIFICATIONS {
                    return Err("ACP notification count exceeds 4096".into());
                }
                self.bytes = self
                    .bytes
                    .checked_add(frame_bytes)
                    .filter(|total| *total <= MAX_NOTIFICATION_BYTES)
                    .ok_or("ACP cumulative notification frames exceed 64 MiB")?;
            }
            NotificationRetention::Tail => {
                loop {
                    let marker_bytes = if self.dropped == 0 {
                        0
                    } else {
                        serde_json::to_vec(&self.truncation_marker())
                            .map_err(|_| "ACP local truncation marker serialization failed")?
                            .len()
                    };
                    if self.frames.len() + 1 + usize::from(self.dropped > 0) <= MAX_NOTIFICATIONS
                        && self.bytes + frame_bytes + marker_bytes <= MAX_NOTIFICATION_BYTES
                    {
                        break;
                    }
                    let (_, discarded_bytes) = self
                        .frames
                        .pop_front()
                        .ok_or("ACP incoming notification exceeds retention limit")?;
                    self.bytes -= discarded_bytes;
                    self.dropped = self.dropped.saturating_add(1);
                }
                self.bytes += frame_bytes;
            }
        }
        self.frames.push_back((message, frame_bytes));
        Ok(())
    }

    fn truncation_marker(&self) -> Value {
        json!({
            "source": "anchor.acp.transport",
            "kind": "notification_tail_truncated",
            "dropped_notifications": self.dropped,
        })
    }

    pub(super) fn values(&self) -> Vec<Value> {
        let mut values = self
            .frames
            .iter()
            .map(|(message, _)| message.clone())
            .collect::<Vec<_>>();
        if self.dropped > 0 {
            values.insert(0, self.truncation_marker());
        }
        values
    }

    pub(super) fn last_values(&self, count: usize) -> Vec<Value> {
        let mut values = self
            .frames
            .iter()
            .skip(self.frames.len().saturating_sub(count))
            .map(|(message, _)| message.clone())
            .collect::<Vec<_>>();
        if count > self.frames.len() {
            let mut marker = self.truncation_marker();
            marker["dropped_notifications"] = json!(count - self.frames.len());
            values.insert(0, marker);
        }
        values
    }

    fn into_values(self) -> Vec<Value> {
        let marker = (self.dropped > 0).then(|| self.truncation_marker());
        self.frames
            .into_iter()
            .map(|(message, _)| message)
            .chain(marker)
            .collect()
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(process_id: std::ffi::c_int, signal: std::ffi::c_int) -> std::ffi::c_int;
}

#[derive(Default)]
struct StderrDiagnostics {
    bytes: usize,
    authentication: bool,
    configuration: bool,
    permission: bool,
    connection: bool,
    panic: bool,
    read_failed: bool,
}

impl StderrDiagnostics {
    fn record(&mut self, bytes: &[u8]) {
        self.bytes = self.bytes.saturating_add(bytes.len());
        let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
        self.authentication |= text.contains("authentication") || text.contains("unauthorized");
        self.configuration |= text.contains("configuration");
        self.permission |= text.contains("permission denied");
        self.connection |= text.contains("connection refused");
        self.panic |= text.contains("panicked") || text.contains("panic:");
    }

    fn summary(&self) -> String {
        let categories = [
            (self.authentication, "authentication"),
            (self.configuration, "configuration"),
            (self.permission, "permission"),
            (self.connection, "connection"),
            (self.panic, "panic"),
            (self.read_failed, "stderr read failure"),
        ]
        .into_iter()
        .filter_map(|(present, category)| present.then_some(category))
        .collect::<Vec<_>>();
        format!(
            "stderr bytes={}, categories=[{}]; raw stderr withheld",
            self.bytes,
            categories.join(", ")
        )
    }
}

pub(crate) struct AcpConnection {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr_task: Option<JoinHandle<()>>,
    diagnostics: Arc<Mutex<StderrDiagnostics>>,
    next_id: u64,
    in_flight: bool,
    poisoned: bool,
    closed: bool,
    elicitation: Option<Arc<dyn ServerRequestHandler>>,
    #[cfg(unix)]
    process_group: Option<std::ffi::c_int>,
}

impl AcpConnection {
    pub(crate) async fn spawn(mut command: Command) -> Result<Self, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| format!("ACP process spawn failed: {error}"))?;
        let stdin = child.stdin.take().ok_or("ACP child stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("ACP child stdout unavailable")?;
        let mut stderr = child.stderr.take().ok_or("ACP child stderr unavailable")?;
        let diagnostics = Arc::new(Mutex::new(StderrDiagnostics::default()));
        let drain_diagnostics = Arc::clone(&diagnostics);
        let stderr_task = tokio::spawn(async move {
            let mut buffer = [0_u8; 4096];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(length) => {
                        if let Ok(mut diagnostics) = drain_diagnostics.lock() {
                            diagnostics.record(&buffer[..length]);
                        }
                    }
                    Err(_) => {
                        if let Ok(mut diagnostics) = drain_diagnostics.lock() {
                            diagnostics.read_failed = true;
                        }
                        break;
                    }
                }
            }
        });
        Ok(Self {
            #[cfg(unix)]
            process_group: child.id().and_then(|process_id| process_id.try_into().ok()),
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr_task: Some(stderr_task),
            diagnostics,
            next_id: 1,
            in_flight: false,
            poisoned: false,
            closed: false,
            elicitation: None,
        })
    }

    pub(crate) fn set_elicitation_handler(&mut self, handler: Arc<dyn ServerRequestHandler>) {
        self.elicitation = Some(handler);
    }

    pub(crate) async fn request(
        &mut self,
        method: &str,
        params: Value,
        cancellation: &Cancellation,
        deadline: Instant,
    ) -> Result<(Value, Vec<Value>), String> {
        self.request_with_retention(
            method,
            params,
            cancellation,
            Some(deadline),
            NotificationRetention::Strict,
            None,
        )
        .await
    }

    pub(crate) async fn request_with_deadline(
        &mut self,
        method: &str,
        params: Value,
        cancellation: &Cancellation,
        deadline: Option<Instant>,
    ) -> Result<(Value, Vec<Value>), String> {
        self.request_observed(method, params, cancellation, deadline, &|_| Ok(()))
            .await
    }

    pub(crate) async fn request_observed(
        &mut self,
        method: &str,
        params: Value,
        cancellation: &Cancellation,
        deadline: Option<Instant>,
        observer: &(dyn Fn(&Value) -> Result<(), String> + Send + Sync),
    ) -> Result<(Value, Vec<Value>), String> {
        self.request_with_retention(
            method,
            params,
            cancellation,
            deadline,
            NotificationRetention::Tail,
            Some(observer),
        )
        .await
    }

    async fn request_with_retention(
        &mut self,
        method: &str,
        params: Value,
        cancellation: &Cancellation,
        deadline: Option<Instant>,
        retention: NotificationRetention,
        observer: Option<&NotificationObserver<'_>>,
    ) -> Result<(Value, Vec<Value>), String> {
        self.ensure_ready()?;
        validate_call(method, &params)?;
        self.in_flight = true;
        let result = bounded(
            self.request_inner(method, params, cancellation, deadline, retention, observer),
            cancellation,
            deadline,
        )
        .await;
        self.in_flight = false;
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                self.poisoned = true;
                let cleanup = self.terminate();
                Err(self.diagnose_cleanup(error, cleanup))
            }
        }
    }

    async fn request_inner(
        &mut self,
        method: &str,
        params: Value,
        cancellation: &Cancellation,
        deadline: Option<Instant>,
        retention: NotificationRetention,
        observer: Option<&NotificationObserver<'_>>,
    ) -> Result<(Value, Vec<Value>), String> {
        let request_id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("ACP request id exhausted")?;
        self.write_frame(&json!({
            "jsonrpc": "2.0", "id": request_id, "method": method, "params": params,
        }))
        .await?;
        let mut notifications = NotificationBuffer::new(retention);
        let mut elicitation_response: Option<PendingResponse> = None;
        let mut incoming_frame = Vec::with_capacity(4096);
        loop {
            check_interruption(cancellation, deadline)?;
            let frame = tokio::select! {
                response = async {
                    match elicitation_response.as_mut() {
                        Some(response) => response.await,
                        None => pending().await,
                    }
                } => {
                    elicitation_response = None;
                    self.write_frame(&response?).await?;
                    continue;
                },
                frame = self.read_frame(&mut incoming_frame) => frame?,
            };
            let message: Value = serde_json::from_slice(&frame).map_err(|error| {
                format!(
                    "ACP invalid JSON frame at line {}, column {}",
                    error.line(),
                    error.column()
                )
            })?;
            let object = message
                .as_object()
                .ok_or("ACP JSON-RPC frame must be an object; batches are unsupported")?;
            if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return Err("ACP invalid or missing JSON-RPC version".into());
            }
            if let Some(method) = object.get("method") {
                let method = method.as_str().ok_or("ACP invalid JSON-RPC method")?;
                if object.contains_key("result") || object.contains_key("error") {
                    return Err("ACP JSON-RPC call also contains a response".into());
                }
                if let Some(params) = object.get("params") {
                    validate_params(params)?;
                }
                if let Some(agent_id) = object.get("id") {
                    validate_id(agent_id)?;
                    let response = if method == "elicitation/create" {
                        match &self.elicitation {
                            Some(handler) => {
                                if elicitation_response.is_some() {
                                    return Err(
                                        "ACP parallel elicitation requests are unsupported".into(),
                                    );
                                }
                                let handler = handler.clone();
                                let agent_id = agent_id.clone();
                                let params = object.get("params").cloned().unwrap_or(Value::Null);
                                elicitation_response = Some(Box::pin(async move {
                                    let result = handler.handle(params).await?;
                                    Ok(json!({"jsonrpc":"2.0","id":agent_id,"result":result}))
                                }));
                                continue;
                            }
                            None => {
                                json!({"jsonrpc":"2.0","id":agent_id,"result":{"action":"cancel"}})
                            }
                        }
                    } else if method == "session/request_permission" {
                        json!({
                            "jsonrpc": "2.0", "id": agent_id,
                            "result": {"outcome": {"outcome": "cancelled"}},
                        })
                    } else {
                        json!({
                            "jsonrpc": "2.0", "id": agent_id,
                            "error": {"code": -32601, "message": "Method not found"},
                        })
                    };
                    self.write_frame(&response).await?;
                } else {
                    if let Some(observer) = observer {
                        observer(&message)?;
                    }
                    notifications.push(message, frame.len())?;
                }
                continue;
            }
            let response_id = object.get("id").ok_or("ACP response is missing its id")?;
            if elicitation_response.is_some() {
                return Err("ACP prompt ended with a pending elicitation request".into());
            }
            validate_id(response_id)?;
            if response_id.as_u64() != Some(request_id) {
                return Err("ACP response id does not match the outstanding request".into());
            }
            match (object.get("result"), object.get("error")) {
                (Some(result), None) => return Ok((result.clone(), notifications.into_values())),
                (None, Some(error)) => {
                    let code = error
                        .get("code")
                        .and_then(Value::as_i64)
                        .ok_or("ACP malformed JSON-RPC error code")?;
                    if error.get("message").and_then(Value::as_str).is_none() {
                        return Err("ACP malformed JSON-RPC error message".into());
                    }
                    return Err(format!(
                        "ACP JSON-RPC error code {code}; agent details withheld"
                    ));
                }
                _ => return Err("ACP response must contain exactly one of result or error".into()),
            }
        }
    }

    pub(crate) async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.ensure_ready()?;
        validate_call(method, &params)?;
        self.in_flight = true;
        let result = timeout(
            NOTIFY_TIMEOUT,
            self.write_frame(&json!({"jsonrpc": "2.0", "method": method, "params": params})),
        )
        .await
        .unwrap_or_else(|_| Err("ACP notification write deadline exceeded".into()));
        self.in_flight = false;
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.poisoned = true;
                let cleanup = self.terminate();
                Err(self.diagnose_cleanup(error, cleanup))
            }
        }
    }

    async fn write_frame(&mut self, message: &Value) -> Result<(), String> {
        let mut frame = serde_json::to_vec(message)
            .map_err(|_| "ACP outgoing JSON serialization failed".to_owned())?;
        if frame.len() > MAX_LINE_BYTES {
            return Err("ACP outgoing frame exceeds 32 MiB".into());
        }
        frame.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or("ACP stdin is closed")?;
        stdin
            .write_all(&frame)
            .await
            .map_err(|error| format!("ACP stdin write failed: {error}"))?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("ACP stdin flush failed: {error}"))
    }

    async fn read_frame(&mut self, frame: &mut Vec<u8>) -> Result<Vec<u8>, String> {
        loop {
            let available = self
                .stdout
                .fill_buf()
                .await
                .map_err(|error| format!("ACP stdout read failed: {error}"))?;
            if available.is_empty() {
                return Err(if frame.is_empty() {
                    "ACP connection lost: stdout EOF".into()
                } else {
                    "ACP connection lost: unterminated JSON frame at stdout EOF".into()
                });
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let length = newline.unwrap_or(available.len());
            if length > MAX_LINE_BYTES - frame.len() {
                return Err("ACP incoming frame exceeds 32 MiB".into());
            }
            frame.extend_from_slice(&available[..length]);
            self.stdout.consume(length + usize::from(newline.is_some()));
            if newline.is_some() {
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                return Ok(std::mem::take(frame));
            }
        }
    }

    pub(crate) async fn close(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        self.poisoned = true;
        let termination = self.terminate();
        let waited = timeout(CLOSE_TIMEOUT, self.child.wait())
            .await
            .map_err(|_| "ACP process exit deadline exceeded".to_owned())
            .and_then(|result| result.map_err(|error| format!("ACP process wait failed: {error}")));
        let mut drain_error = None;
        if let Some(mut stderr_task) = self.stderr_task.take() {
            match timeout(STDERR_CLOSE_TIMEOUT, &mut stderr_task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => drain_error = Some("ACP stderr drain task failed".to_owned()),
                Err(_) => {
                    stderr_task.abort();
                    drain_error = Some("ACP stderr drain exit deadline exceeded".to_owned());
                }
            }
        }
        self.closed = termination.is_ok() && waited.is_ok();
        let errors = termination
            .err()
            .into_iter()
            .chain(waited.err())
            .chain(drain_error)
            .collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(self.diagnose(errors.join("; ")))
        }
    }

    fn ensure_ready(&mut self) -> Result<(), String> {
        if self.in_flight {
            self.poisoned = true;
            let cleanup = self.terminate();
            return Err(self.diagnose_cleanup(
                "ACP previous operation was abandoned; connection cannot be reused".into(),
                cleanup,
            ));
        }
        if self.poisoned || self.closed {
            return Err(self.diagnose("ACP connection is closed or unusable".into()));
        }
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.stdin.take();
        let mut errors = Vec::new();
        #[cfg(unix)]
        if let Some(process_group) = self.process_group {
            if unsafe { kill(-process_group, 9) } == 0 {
                self.process_group = None;
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(3) {
                    self.process_group = None;
                } else {
                    errors.push(format!("ACP process group termination failed: {error}"));
                }
            }
        }
        if let Err(error) = self.child.start_kill() {
            errors.push(format!("ACP process termination failed: {error}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn diagnose_cleanup(&self, error: String, cleanup: Result<(), String>) -> String {
        self.diagnose(match cleanup {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; {cleanup}"),
        })
    }

    fn diagnose(&self, error: String) -> String {
        match self.diagnostics.lock() {
            Ok(diagnostics) => format!("{error}; {}", diagnostics.summary()),
            Err(_) => format!("{error}; stderr diagnostics unavailable"),
        }
    }
}

impl Drop for AcpConnection {
    fn drop(&mut self) {
        let _ = self.terminate();
        if let Some(stderr_task) = self.stderr_task.take() {
            stderr_task.abort();
        }
    }
}

fn validate_call(method: &str, params: &Value) -> Result<(), String> {
    if method.is_empty() {
        return Err("ACP outgoing method must not be empty".into());
    }
    validate_params(params)
}

fn validate_params(params: &Value) -> Result<(), String> {
    if !params.is_object() && !params.is_array() {
        return Err("ACP JSON-RPC params must be an object or array".into());
    }
    Ok(())
}

fn validate_id(request_id: &Value) -> Result<(), String> {
    if request_id.is_null()
        || request_id.is_string()
        || request_id.as_i64().is_some()
        || request_id.as_u64().is_some()
    {
        Ok(())
    } else {
        Err("ACP JSON-RPC id must be a string, integer or null".into())
    }
}

fn check_interruption(
    cancellation: &Cancellation,
    deadline: Option<Instant>,
) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        return Err("ACP request cancelled".into());
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err("ACP request deadline exceeded".into());
    }
    Ok(())
}

pub(super) async fn bounded<Output>(
    operation: impl Future<Output = Result<Output, String>>,
    cancellation: &Cancellation,
    deadline: Option<Instant>,
) -> Result<Output, String> {
    check_interruption(cancellation, deadline)?;
    let cancelled = async {
        loop {
            if cancellation.load(Ordering::Acquire) {
                return;
            }
            sleep(CANCELLATION_POLL).await;
        }
    };
    let expired = async {
        match deadline {
            Some(deadline) => sleep_until(deadline).await,
            None => pending::<()>().await,
        }
    };
    tokio::select! {
        biased;
        _ = cancelled => Err("ACP request cancelled".into()),
        _ = expired => Err("ACP request deadline exceeded".into()),
        result = operation => result,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use super::*;

    fn cancellation() -> Cancellation {
        Arc::new(AtomicBool::new(false))
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    async fn peer(script: &str) -> AcpConnection {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script).process_group(0);
        AcpConnection::spawn(command).await.unwrap()
    }

    fn response() -> &'static str {
        r#"{"jsonrpc":"2.0","id":1,"result":{"stopReason":"end_turn"}}"#
    }

    struct ElicitationHandler {
        answer: Value,
        waiting: bool,
    }

    impl ServerRequestHandler for ElicitationHandler {
        fn handle<'request>(
            &'request self,
            params: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'request>> {
            Box::pin(async move {
                if self.waiting {
                    pending::<()>().await;
                }
                if params["sessionId"] != "native-session" {
                    return Err("elicitation Session mismatch".into());
                }
                Ok(self.answer.clone())
            })
        }
    }

    #[tokio::test]
    async fn native_elicitation_response_uses_the_agent_request_id() {
        for answer in [
            json!({"action":"accept","content":{"confirm":true}}),
            json!({"action":"decline"}),
            json!({"action":"cancel"}),
        ] {
            let root = tempfile::tempdir().unwrap();
            let returned = root.path().join("response.json");
            let request = json!({"jsonrpc":"2.0","id":"goose-question","method":"elicitation/create","params":{"sessionId":"native-session"}});
            let mut connection = peer(&format!(
                "IFS= read -r request\nprintf '%s\\n' '{request}'\nIFS= read -r answer\nprintf '%s' \"$answer\" > '{}'\nprintf '%s\\n' '{}'\n", returned.display(), response(),
            )).await;
            connection.set_elicitation_handler(Arc::new(ElicitationHandler {
                answer: answer.clone(),
                waiting: false,
            }));
            let (result, _) = connection
                .request("session/prompt", json!({}), &cancellation(), deadline())
                .await
                .unwrap();
            assert_eq!(result["stopReason"], "end_turn");
            let saved: Value = serde_json::from_slice(&std::fs::read(returned).unwrap()).unwrap();
            assert_eq!(
                saved,
                json!({"jsonrpc":"2.0","id":"goose-question","result":answer})
            );
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_pending_native_question() {
        let request = json!({"jsonrpc":"2.0","id":7,"method":"elicitation/create","params":{"sessionId":"native-session"}});
        let mut connection = peer(&format!(
            "IFS= read -r request\nprintf '%s\\n' '{request}'\nsleep 30\n",
        ))
        .await;
        connection.set_elicitation_handler(Arc::new(ElicitationHandler {
            answer: Value::Null,
            waiting: true,
        }));
        let cancellation = cancellation();
        let cancel = cancellation.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(30)).await;
            cancel.store(true, Ordering::Release);
        });
        let result = timeout(
            Duration::from_secs(2),
            connection.request("session/prompt", json!({}), &cancellation, deadline()),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(connection.poisoned);
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn pending_native_question_detects_agent_exit_without_waiting_for_an_answer() {
        let request = json!({"jsonrpc":"2.0","id":7,"method":"elicitation/create","params":{"sessionId":"native-session"}});
        let mut connection = peer(&format!(
            "IFS= read -r request\nprintf '%s\\n' '{request}'\nsleep 0.1\n"
        ))
        .await;
        connection.set_elicitation_handler(Arc::new(ElicitationHandler {
            answer: Value::Null,
            waiting: true,
        }));
        let result = timeout(
            Duration::from_secs(2),
            connection.request_with_deadline("session/prompt", json!({}), &cancellation(), None),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().contains("EOF"));
        connection.close().await.unwrap();
    }

    struct DelayedElicitationHandler;

    impl ServerRequestHandler for DelayedElicitationHandler {
        fn handle<'request>(
            &'request self,
            _: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'request>> {
            Box::pin(async {
                sleep(Duration::from_millis(100)).await;
                Ok(json!({"action":"decline"}))
            })
        }
    }

    #[tokio::test]
    async fn native_answer_does_not_discard_a_partially_received_notification() {
        let request = json!({"jsonrpc":"2.0","id":7,"method":"elicitation/create","params":{}});
        let notification = json!({"jsonrpc":"2.0","method":"session/update","params":{"update":{"text":"during question"}}});
        let encoded = notification.to_string();
        let middle = encoded.len() / 2;
        let mut connection = peer(&format!(
            "IFS= read -r request\nprintf '%s\\n' '{request}'\nprintf '%s' '{}'\nIFS= read -r answer\nprintf '%s\\n' '{}'\nprintf '%s\\n' '{}'\n",
            &encoded[..middle], &encoded[middle..], response(),
        )).await;
        connection.set_elicitation_handler(Arc::new(DelayedElicitationHandler));
        let (result, notifications) = connection
            .request_with_deadline(
                "session/prompt",
                json!({}),
                &cancellation(),
                Some(deadline()),
            )
            .await
            .unwrap();
        assert_eq!(result["stopReason"], "end_turn");
        assert_eq!(notifications, vec![notification]);
        connection.close().await.unwrap();
    }

    fn assert_truncation_marker(marker: &Value, dropped: usize) {
        assert_eq!(
            marker,
            &json!({
                "source": "anchor.acp.transport",
                "kind": "notification_tail_truncated",
                "dropped_notifications": dropped,
            })
        );
        assert!(marker.get("jsonrpc").is_none());
        assert!(marker.get("method").is_none());
    }

    #[tokio::test]
    async fn observed_notification_is_persisted_before_prompt_response() {
        let directory = tempfile::tempdir().unwrap();
        let observation_path = directory.path().join("notification.json");
        let release_path = directory.path().join("release-response");
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"update": {"text": "first chunk"}},
        });
        let mut connection = peer(&format!(
            "IFS= read -r request || exit 10\nprintf '%s\\n' '{notification}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf '%s\\n' '{}'\n",
            release_path.display(),
            response(),
        ))
        .await;
        let observed = AtomicUsize::new(0);
        let observer = |message: &Value| {
            observed.fetch_add(1, Ordering::AcqRel);
            let bytes = serde_json::to_vec(message).map_err(|error| error.to_string())?;
            std::fs::write(&observation_path, bytes).map_err(|error| error.to_string())
        };
        let cancellation = cancellation();
        {
            let request = connection.request_observed(
                "session/prompt",
                json!({}),
                &cancellation,
                Some(deadline()),
                &observer,
            );
            tokio::pin!(request);
            tokio::select! {
                result = &mut request => panic!("prompt completed before response release: {result:?}"),
                result = timeout(Duration::from_secs(2), async {
                    while !observation_path.exists() {
                        sleep(Duration::from_millis(10)).await;
                    }
                }) => result.unwrap(),
            }
            let persisted: Value =
                serde_json::from_slice(&std::fs::read(&observation_path).unwrap()).unwrap();
            assert_eq!(persisted, notification);
            assert_eq!(observed.load(Ordering::Acquire), 1);
            assert!(
                timeout(Duration::from_millis(30), &mut request)
                    .await
                    .is_err()
            );
            std::fs::write(&release_path, b"ready").unwrap();
            let (result, notifications) = timeout(Duration::from_secs(2), request)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result, json!({"stopReason": "end_turn"}));
            assert_eq!(notifications, vec![notification]);
        }
        assert_eq!(observed.load(Ordering::Acquire), 1);
        assert!(!connection.poisoned);
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn observed_persistence_error_is_preserved_and_poisons_connection() {
        let directory = tempfile::tempdir().unwrap();
        let mut connection = peer(&format!(
            "IFS= read -r request\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{\"text\":\"first\"}}}}' '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{\"text\":\"second\"}}}}' '{}'\nsleep 20",
            response(),
        ))
        .await;
        let observed = AtomicUsize::new(0);
        let persistence_error = Mutex::new(None);
        let observer = |message: &Value| {
            observed.fetch_add(1, Ordering::AcqRel);
            let bytes = serde_json::to_vec(message).map_err(|error| error.to_string())?;
            std::fs::write(directory.path(), bytes).map_err(|error| {
                let error = format!("notification persistence failed: {error}");
                *persistence_error.lock().unwrap() = Some(error.clone());
                error
            })
        };
        let cancellation = cancellation();
        let error = connection
            .request_observed("session/prompt", json!({}), &cancellation, None, &observer)
            .await
            .unwrap_err();
        let expected = persistence_error.lock().unwrap().clone().unwrap();
        assert!(error.starts_with(&expected), "{error}");
        assert_eq!(observed.load(Ordering::Acquire), 1);
        assert!(connection.poisoned);
        assert!(!connection.in_flight);
        assert_eq!(connection.next_id, 2);
        let error = connection
            .request_observed("session/prompt", json!({}), &cancellation, None, &observer)
            .await
            .unwrap_err();
        assert!(error.contains("closed or unusable"), "{error}");
        assert_eq!(observed.load(Ordering::Acquire), 1);
        assert_eq!(connection.next_id, 2);
        assert!(
            connection
                .request("session/new", json!({}), &cancellation, deadline())
                .await
                .unwrap_err()
                .contains("closed or unusable")
        );
        assert!(
            connection
                .notify("session/cancel", json!({}))
                .await
                .is_err()
        );
        connection.close().await.unwrap();
        assert!(connection.child.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn observed_request_without_deadline_can_be_cancelled_after_a_chunk() {
        let mut connection = peer(
            "IFS= read -r request\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{}}'\nsleep 20",
        )
        .await;
        let cancellation = cancellation();
        let observed = AtomicUsize::new(0);
        let observer = |_message: &Value| {
            observed.fetch_add(1, Ordering::AcqRel);
            cancellation.store(true, Ordering::Release);
            Ok(())
        };
        let error = timeout(
            Duration::from_secs(2),
            connection.request_observed(
                "session/prompt",
                json!({}),
                &cancellation,
                None,
                &observer,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.contains("cancelled"), "{error}");
        assert_eq!(observed.load(Ordering::Acquire), 1);
        assert!(connection.poisoned);
        assert!(!connection.in_flight);
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn optional_deadline_none_keeps_pending_io_until_cancellation() {
        for (script, params) in [
            ("IFS= read -r request; sleep 20", json!({})),
            ("IFS= read -r request; printf '%s' '{'; sleep 20", json!({})),
            ("sleep 20", json!({"text": "x".repeat(900_000)})),
        ] {
            let mut connection = peer(script).await;
            let cancellation = cancellation();
            {
                let request =
                    connection.request_with_deadline("session/prompt", params, &cancellation, None);
                tokio::pin!(request);
                assert!(
                    timeout(Duration::from_millis(80), &mut request)
                        .await
                        .is_err()
                );
                cancellation.store(true, Ordering::Release);
                let error = timeout(Duration::from_secs(2), request)
                    .await
                    .unwrap()
                    .unwrap_err();
                assert!(error.contains("cancelled"), "{error}");
                assert!(!error.contains("deadline"), "{error}");
            }
            assert!(connection.poisoned);
            assert_eq!(connection.next_id, 2);
            assert!(!connection.in_flight);
            connection.close().await.unwrap();
            assert!(connection.child.try_wait().unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn optional_deadline_some_bounds_pending_io() {
        for (script, params) in [
            ("IFS= read -r request; sleep 20", json!({})),
            ("IFS= read -r request; printf '%s' '{'; sleep 20", json!({})),
            ("sleep 20", json!({"text": "x".repeat(900_000)})),
        ] {
            let mut connection = peer(script).await;
            let started = Instant::now();
            let error = connection
                .request_with_deadline(
                    "session/prompt",
                    params,
                    &cancellation(),
                    Some(started + Duration::from_millis(80)),
                )
                .await
                .unwrap_err();
            assert!(error.contains("deadline exceeded"), "{error}");
            assert!(started.elapsed() < Duration::from_secs(2));
            assert!(connection.poisoned);
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn optional_deadline_pre_cancelled_or_expired_request_is_not_sent() {
        for (pre_cancelled, deadline) in [(true, None), (false, Some(Instant::now()))] {
            let mut connection = peer("IFS= read -r request; sleep 20").await;
            let cancellation = cancellation();
            cancellation.store(pre_cancelled, Ordering::Release);
            let error = connection
                .request_with_deadline("session/prompt", json!({}), &cancellation, deadline)
                .await
                .unwrap_err();
            assert!(error.contains(if pre_cancelled {
                "cancelled"
            } else {
                "deadline"
            }));
            assert_eq!(connection.next_id, 1);
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn notification_tail_keeps_latest_updates_and_rejects_agent_requests() {
        let count = MAX_NOTIFICATIONS * 3;
        let mut script = format!(
            "IFS= read -r request\ncounter=0\nwhile [ \"$counter\" -lt {count} ]; do\nprintf '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{\"sequence\":%s}}}}\\n' \"$counter\"\ncounter=$((counter + 1))\ndone\n"
        );
        for (index, method) in [
            "session/request_permission",
            "fs/read_text_file",
            "fs/write_text_file",
            "terminal/create",
            "terminal/output",
            "terminal/wait_for_exit",
            "terminal/kill",
            "terminal/release",
            "unknown/extension",
        ]
        .into_iter()
        .enumerate()
        {
            let agent_id = match index {
                0 => json!(1),
                1 => Value::Null,
                _ => json!(format!("agent-{index}")),
            };
            let request = json!({
                "jsonrpc": "2.0", "id": agent_id, "method": method,
                "params": {"path": "/no/host/access", "command": "exit 99"},
            });
            let expected = if index == 0 {
                json!({
                    "jsonrpc": "2.0", "id": agent_id,
                    "result": {"outcome": {"outcome": "cancelled"}},
                })
            } else {
                json!({
                    "jsonrpc": "2.0", "id": agent_id,
                    "error": {"code": -32601, "message": "Method not found"},
                })
            };
            script.push_str(&format!(
                "printf '%s\\n' '{request}'\nIFS= read -r reply || exit 11\n[ \"$reply\" = '{expected}' ] || exit 12\n"
            ));
        }
        script.push_str(&format!(
            "printf '%s\\n' '{}'\nIFS= read -r request || exit 13\ncase \"$request\" in *'\"id\":2'*) ;; *) exit 14 ;; esac\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":null}}'\n",
            response()
        ));
        for observe in [false, true] {
            let mut connection = peer(&script).await;
            let cancellation = cancellation();
            let observed = AtomicUsize::new(0);
            let observer = |message: &Value| {
                let sequence = observed.fetch_add(1, Ordering::AcqRel);
                assert_eq!(message["method"], "session/update");
                assert_eq!(message["params"]["sequence"], sequence);
                Ok(())
            };
            let (result, notifications) = if observe {
                connection
                    .request_observed(
                        "session/prompt",
                        json!({}),
                        &cancellation,
                        Some(deadline()),
                        &observer,
                    )
                    .await
            } else {
                connection
                    .request_with_deadline(
                        "session/prompt",
                        json!({}),
                        &cancellation,
                        Some(deadline()),
                    )
                    .await
            }
            .unwrap();
            assert_eq!(
                observed.load(Ordering::Acquire),
                if observe { count } else { 0 }
            );
            assert_eq!(result, json!({"stopReason": "end_turn"}));
            assert_eq!(notifications.len(), MAX_NOTIFICATIONS);
            let retained = MAX_NOTIFICATIONS - 1;
            for (index, notification) in notifications[..retained].iter().enumerate() {
                assert_eq!(notification["method"], "session/update");
                assert_eq!(notification["params"]["sequence"], count - retained + index);
            }
            assert_truncation_marker(notifications.last().unwrap(), count - retained);
            let bytes: usize = notifications
                .iter()
                .map(|notification| serde_json::to_vec(notification).unwrap().len())
                .sum();
            assert!(bytes <= MAX_NOTIFICATION_BYTES);
            let (result, notifications) = connection
                .request("session/new", json!({}), &cancellation, deadline())
                .await
                .unwrap();
            assert!(result.is_null());
            assert!(notifications.is_empty());
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn notification_tail_is_bounded_in_bytes_and_preserves_the_final_update() {
        let capacity = MAX_NOTIFICATION_BYTES / MAX_LINE_BYTES;
        let count = capacity + 1;
        let mut script = padded_notifications(count);
        script = script.replace(
            &format!("printf '%s\\n' '{}'", response()),
            &format!(
                "printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{\"text\":\"final\"}}}}'\nprintf '%s\\n' '{}'",
                response()
            ),
        );
        for observe in [false, true] {
            let mut connection = peer(&script).await;
            let cancellation = cancellation();
            let observed = AtomicUsize::new(0);
            let observer = |message: &Value| {
                assert_eq!(message["method"], "session/update");
                observed.fetch_add(1, Ordering::AcqRel);
                Ok(())
            };
            let (result, notifications) = if observe {
                connection
                    .request_observed(
                        "session/prompt",
                        json!({}),
                        &cancellation,
                        Some(deadline()),
                        &observer,
                    )
                    .await
            } else {
                connection
                    .request_with_deadline(
                        "session/prompt",
                        json!({}),
                        &cancellation,
                        Some(deadline()),
                    )
                    .await
            }
            .unwrap();
            assert_eq!(
                observed.load(Ordering::Acquire),
                if observe { count + 1 } else { 0 }
            );
            assert_eq!(result, json!({"stopReason": "end_turn"}));
            assert_eq!(notifications.len(), capacity + 1);
            assert_eq!(notifications[capacity - 1]["params"]["text"], "final");
            assert_truncation_marker(notifications.last().unwrap(), count - (capacity - 1));
            let bytes: usize = notifications
                .iter()
                .map(|notification| serde_json::to_vec(notification).unwrap().len())
                .sum();
            assert!(bytes <= MAX_NOTIFICATION_BYTES);
            assert!(!connection.poisoned);
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn notification_tail_accepts_exact_limits_without_a_truncation_marker() {
        let capacity = MAX_NOTIFICATION_BYTES / MAX_LINE_BYTES;
        let mut scripts = vec![padded_notifications(capacity)];
        scripts.push(format!(
            "IFS= read -r request\ncounter=0\nwhile [ \"$counter\" -lt {MAX_NOTIFICATIONS} ]; do\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{}}}}'\ncounter=$((counter + 1))\ndone\nprintf '%s\\n' '{}'\n",
            response()
        ));
        for (script, count) in scripts.into_iter().zip([capacity, MAX_NOTIFICATIONS]) {
            let mut connection = peer(&script).await;
            let (_, notifications) = connection
                .request_with_deadline("session/prompt", json!({}), &cancellation(), None)
                .await
                .unwrap();
            assert_eq!(notifications.len(), count);
            assert!(
                notifications
                    .iter()
                    .all(|notification| notification["method"] == "session/update")
            );
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn notification_buffer_keeps_both_limits_after_each_frame() {
        let mut notifications = NotificationBuffer::new(NotificationRetention::Tail);
        let mut total = 0;
        for frame_bytes in std::iter::repeat_n(MAX_LINE_BYTES, 40)
            .chain(std::iter::repeat_n(100, MAX_NOTIFICATIONS * 2))
            .chain(std::iter::repeat_n(MAX_LINE_BYTES, 20))
        {
            notifications
                .push(json!({"sequence": total}), frame_bytes)
                .unwrap();
            total += 1;
            let marker_bytes = if notifications.dropped == 0 {
                0
            } else {
                serde_json::to_vec(&notifications.truncation_marker())
                    .unwrap()
                    .len()
            };
            assert!(
                notifications.frames.len() + usize::from(notifications.dropped > 0)
                    <= MAX_NOTIFICATIONS
            );
            assert!(notifications.bytes + marker_bytes <= MAX_NOTIFICATION_BYTES);
            assert_eq!(notifications.frames.len() + notifications.dropped, total);
            assert_eq!(
                notifications.frames.back().unwrap().0["sequence"],
                total - 1
            );
        }
        let retained = notifications.frames.len();
        let values = notifications.into_values();
        assert_truncation_marker(values.last().unwrap(), total - retained);
    }

    #[tokio::test]
    async fn optional_deadline_still_rejects_oversized_frames() {
        for deadline in [None, Some(deadline())] {
            let mut connection = peer(&format!(
                "IFS= read -r request; head -c {} /dev/zero | tr '\\000' x; sleep 20",
                MAX_LINE_BYTES + 1
            ))
            .await;
            let error = connection
                .request_with_deadline("session/prompt", json!({}), &cancellation(), deadline)
                .await
                .unwrap_err();
            assert!(error.contains("incoming frame exceeds 32 MiB"), "{error}");
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn requests_notifications_and_notify_use_stdout_and_matching_ids() {
        let mut connection = peer(
            r#"
                IFS= read -r request || exit 10
                case "$request" in *'"id":1'*'"method":"session/prompt"'*) ;; *) exit 11 ;; esac
                printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"local","update":{"text":"hello"}}}'
                printf '%s\r\n' '{"jsonrpc":"2.0","id":1,"result":{"stopReason":"end_turn"}}'
                IFS= read -r notification || exit 12
                case "$notification" in *'"method":"session/cancel"'*) ;; *) exit 13 ;; esac
                case "$notification" in *'"id":'*) exit 14 ;; esac
                IFS= read -r request || exit 15
                case "$request" in *'"id":2'*) ;; *) exit 16 ;; esac
                printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":null}'
            "#,
        )
        .await;
        let (result, notifications) = connection
            .request("session/prompt", json!({}), &cancellation(), deadline())
            .await
            .unwrap();
        assert_eq!(result, json!({"stopReason": "end_turn"}));
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0]["method"], "session/update");
        assert_eq!(notifications[0]["params"]["update"]["text"], "hello");
        connection
            .notify("session/cancel", json!({"sessionId": "local"}))
            .await
            .unwrap();
        let (result, notifications) = connection
            .request("session/new", json!({}), &cancellation(), deadline())
            .await
            .unwrap();
        assert!(result.is_null());
        assert!(notifications.is_empty());
        connection.close().await.unwrap();
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn permissions_are_cancelled_and_other_agent_requests_fail_closed() {
        let mut script = "IFS= read -r request || exit 10\n".to_owned();
        let methods = [
            "session/request_permission",
            "fs/read_text_file",
            "fs/write_text_file",
            "terminal/create",
            "terminal/output",
            "terminal/wait_for_exit",
            "terminal/kill",
            "terminal/release",
            "unknown/extension",
        ];
        for (index, method) in methods.into_iter().enumerate() {
            let agent_id = if index == 0 {
                json!(1)
            } else if index == 1 {
                Value::Null
            } else {
                json!(format!("agent-{index}"))
            };
            let request = json!({
                "jsonrpc": "2.0", "id": agent_id, "method": method,
                "params": {"path": "/no/host/access", "command": "exit 99"},
            });
            let expected = if index == 0 {
                json!({
                    "jsonrpc": "2.0", "id": agent_id,
                    "result": {"outcome": {"outcome": "cancelled"}},
                })
            } else {
                json!({
                    "jsonrpc": "2.0", "id": agent_id,
                    "error": {"code": -32601, "message": "Method not found"},
                })
            };
            script.push_str(&format!(
                "printf '%s\\n' '{request}'\nIFS= read -r reply || exit 11\n[ \"$reply\" = '{expected}' ] || exit 12\n"
            ));
        }
        script.push_str(&format!("printf '%s\\n' '{}'\n", response()));
        for observe in [false, true] {
            let mut connection = peer(&script).await;
            let cancellation = cancellation();
            let observer =
                |_message: &Value| Err("server requests and results must not be observed".into());
            let (_, notifications) = if observe {
                connection
                    .request_observed(
                        "session/prompt",
                        json!({}),
                        &cancellation,
                        Some(deadline()),
                        &observer,
                    )
                    .await
            } else {
                connection
                    .request("session/prompt", json!({}), &cancellation, deadline())
                    .await
            }
            .unwrap();
            assert!(notifications.is_empty());
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn malformed_frames_and_unmatched_ids_are_rejected() {
        let frames = [
            ("not-json", "invalid JSON"),
            ("", "invalid JSON"),
            ("[]", "must be an object"),
            (r#"{"id":1,"result":{}}"#, "version"),
            (r#"{"jsonrpc":"1.0","id":1,"result":{}}"#, "version"),
            (r#"{"jsonrpc":"2.0","id":2,"result":{}}"#, "does not match"),
            (
                r#"{"jsonrpc":"2.0","id":"1","result":{}}"#,
                "does not match",
            ),
            (r#"{"jsonrpc":"2.0","id":true,"result":{}}"#, "id must be"),
            (r#"{"jsonrpc":"2.0","id":1.0,"result":{}}"#, "id must be"),
            (r#"{"jsonrpc":"2.0","result":{}}"#, "missing its id"),
            (r#"{"jsonrpc":"2.0","id":1}"#, "exactly one"),
            (
                r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{}}"#,
                "exactly one",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":"bad","message":"bad"}}"#,
                "error code",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1}}"#,
                "error message",
            ),
            (r#"{"jsonrpc":"2.0","method":3}"#, "invalid JSON-RPC method"),
            (
                r#"{"jsonrpc":"2.0","method":"fs/read_text_file","params":null}"#,
                "params must be",
            ),
            (
                r#"{"jsonrpc":"2.0","method":"fs/read_text_file","id":[],"params":{}}"#,
                "id must be",
            ),
            (
                r#"{"jsonrpc":"2.0","method":"session/update","result":{}}"#,
                "also contains a response",
            ),
        ];
        for (frame, expected) in frames {
            for observe in [false, true] {
                let mut connection = peer(&format!(
                    "IFS= read -r request; printf '%s\\n' '{frame}'; sleep 20"
                ))
                .await;
                let cancellation = cancellation();
                let observer = |_message: &Value| Err("invalid frames must not be observed".into());
                let error = if observe {
                    connection
                        .request_observed(
                            "session/prompt",
                            json!({}),
                            &cancellation,
                            Some(deadline()),
                            &observer,
                        )
                        .await
                } else {
                    connection
                        .request("session/prompt", json!({}), &cancellation, deadline())
                        .await
                }
                .unwrap_err();
                assert!(error.contains(expected), "{expected}: {error}");
                assert!(connection.poisoned);
                connection.close().await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn oversized_incoming_line_is_rejected_without_needing_a_newline() {
        let mut connection = peer(&format!(
            "IFS= read -r request; head -c {} /dev/zero | tr '\\000' x; sleep 20",
            MAX_LINE_BYTES + 1
        ))
        .await;
        let error = connection
            .request("session/prompt", json!({}), &cancellation(), deadline())
            .await
            .unwrap_err();
        assert!(error.contains("incoming frame exceeds 32 MiB"), "{error}");
        connection.close().await.unwrap();
    }

    fn padded_notifications(count: usize) -> String {
        let prefix = r#"{"jsonrpc":"2.0","method":"session/update","params":{"text":""#;
        let suffix = r#""}}"#;
        let padding = MAX_LINE_BYTES - prefix.len() - suffix.len();
        format!(
            "IFS= read -r request\npadding=$(head -c {padding} /dev/zero | tr '\\000' x)\ncounter=0\nwhile [ \"$counter\" -lt {count} ]; do\nprintf '%s%s%s\\n' '{prefix}' \"$padding\" '{suffix}'\ncounter=$((counter + 1))\ndone\nprintf '%s\\n' '{}'\n",
            response()
        )
    }

    #[tokio::test]
    async fn exact_line_and_cumulative_byte_limits_are_accepted() {
        let capacity = MAX_NOTIFICATION_BYTES / MAX_LINE_BYTES;
        let mut connection = peer(&padded_notifications(capacity)).await;
        let (_, notifications) = connection
            .request("session/prompt", json!({}), &cancellation(), deadline())
            .await
            .unwrap();
        assert_eq!(notifications.len(), capacity);
        for notification in notifications {
            assert_eq!(
                serde_json::to_vec(&notification).unwrap().len(),
                MAX_LINE_BYTES
            );
        }
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn cumulative_notification_byte_limit_is_enforced() {
        let mut connection = peer(&padded_notifications(
            MAX_NOTIFICATION_BYTES / MAX_LINE_BYTES + 1,
        ))
        .await;
        let error = connection
            .request("session/prompt", json!({}), &cancellation(), deadline())
            .await
            .unwrap_err();
        assert!(error.contains("exceed 64 MiB"), "{error}");
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn notification_count_limit_is_inclusive_and_enforced() {
        for (count, accepted) in [(4096, true), (4097, false)] {
            let mut connection = peer(&format!(
                "IFS= read -r request\ncounter=0\nwhile [ \"$counter\" -lt {count} ]; do\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{}}}}'\ncounter=$((counter + 1))\ndone\nprintf '%s\\n' '{}'\n",
                response()
            ))
            .await;
            let result = connection
                .request("session/prompt", json!({}), &cancellation(), deadline())
                .await;
            if accepted {
                assert_eq!(result.unwrap().1.len(), count);
            } else {
                assert!(result.unwrap_err().contains("count exceeds 4096"));
            }
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn deadline_bounds_stalled_and_partial_reads_and_blocked_writes() {
        for (script, params) in [
            ("IFS= read -r request; sleep 20", json!({})),
            ("IFS= read -r request; printf '%s' '{'; sleep 20", json!({})),
            ("sleep 20", json!({"text": "x".repeat(900_000)})),
        ] {
            let mut connection = peer(script).await;
            let started = Instant::now();
            let error = connection
                .request(
                    "session/prompt",
                    params,
                    &cancellation(),
                    started + Duration::from_millis(80),
                )
                .await
                .unwrap_err();
            assert!(error.contains("deadline exceeded"), "{error}");
            assert!(started.elapsed() < Duration::from_secs(2));
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_pending_read() {
        let mut connection = peer("IFS= read -r request; sleep 20").await;
        let cancellation = cancellation();
        let trigger = Arc::clone(&cancellation);
        let cancellation_task = tokio::spawn(async move {
            sleep(Duration::from_millis(30)).await;
            trigger.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let error = connection
            .request("session/prompt", json!({}), &cancellation, deadline())
            .await
            .unwrap_err();
        assert!(error.contains("cancelled"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        cancellation_task.await.unwrap();
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn pre_cancelled_or_expired_request_is_not_sent() {
        for pre_cancelled in [true, false] {
            let mut connection = peer("IFS= read -r request; sleep 20").await;
            let cancellation = cancellation();
            cancellation.store(pre_cancelled, Ordering::Release);
            let deadline = if pre_cancelled {
                deadline()
            } else {
                Instant::now()
            };
            let error = connection
                .request("session/prompt", json!({}), &cancellation, deadline)
                .await
                .unwrap_err();
            assert!(error.contains(if pre_cancelled {
                "cancelled"
            } else {
                "deadline"
            }));
            assert_eq!(connection.next_id, 1);
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn abandoned_request_is_never_retried_or_reused() {
        let mut connection = peer("IFS= read -r request; sleep 20").await;
        let cancellation = cancellation();
        assert!(
            timeout(
                Duration::from_millis(40),
                connection.request("session/prompt", json!({}), &cancellation, deadline())
            )
            .await
            .is_err()
        );
        let error = connection
            .request("session/prompt", json!({}), &cancellation, deadline())
            .await
            .unwrap_err();
        assert!(error.contains("abandoned"));
        assert_eq!(connection.next_id, 2);
        assert!(
            connection
                .notify("session/cancel", json!({}))
                .await
                .is_err()
        );
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn outgoing_frame_limit_is_enforced() {
        for notification in [false, true] {
            let mut connection = peer("IFS= read -r request; sleep 20").await;
            let params = json!({"text": "x".repeat(MAX_LINE_BYTES)});
            let error = if notification {
                connection
                    .notify("session/cancel", params)
                    .await
                    .unwrap_err()
            } else {
                connection
                    .request("session/prompt", params, &cancellation(), deadline())
                    .await
                    .unwrap_err()
            };
            assert!(error.contains("outgoing frame exceeds 32 MiB"));
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn child_exit_and_unterminated_frame_are_explicit_errors() {
        for (script, expected) in [
            ("IFS= read -r request; exit 7", "stdout EOF"),
            (
                "IFS= read -r request; printf '%s' '{'; exit 7",
                "unterminated",
            ),
        ] {
            let mut connection = peer(script).await;
            let error = connection
                .request("session/prompt", json!({}), &cancellation(), deadline())
                .await
                .unwrap_err();
            assert!(error.contains(expected), "{error}");
            connection.close().await.unwrap();
            assert!(connection.child.try_wait().unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn stderr_is_drained_but_never_used_as_protocol_or_echoed_as_diagnostics() {
        let mut connection = peer(&format!(
            "IFS= read -r request\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"stderr is not RPC\"}}' >&2\nhead -c 1048576 /dev/zero >&2\nprintf '%s\\n' 'authentication failed credential=host-private-secret' >&2\nsleep 0.02\nprintf '%s\\n' '{}'\nIFS= read -r request\nexit 7\n",
            response()
        ))
        .await;
        let (result, _) = connection
            .request("session/prompt", json!({}), &cancellation(), deadline())
            .await
            .unwrap();
        assert_eq!(result, json!({"stopReason": "end_turn"}));
        let error = connection
            .request("session/new", json!({}), &cancellation(), deadline())
            .await
            .unwrap_err();
        assert!(error.contains("stdout EOF"));
        assert!(error.contains("authentication"), "{error}");
        assert!(!error.contains("host-private-secret"));
        assert!(!error.contains("stderr is not RPC"));
        assert!(connection.diagnostics.lock().unwrap().bytes >= 1048576);
        assert!(error.len() < 1024);
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn agent_error_is_not_retried_and_does_not_echo_secret_details() {
        let mut connection = peer(
            r#"IFS= read -r request; printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"host-private-secret","data":{"token":"other-secret"}}}'; sleep 20"#,
        )
        .await;
        let error = connection
            .request("session/prompt", json!({}), &cancellation(), deadline())
            .await
            .unwrap_err();
        assert!(error.contains("error code -32000"));
        assert!(!error.contains("host-private-secret"));
        assert!(!error.contains("other-secret"));
        assert!(
            connection
                .request("session/prompt", json!({}), &cancellation(), deadline())
                .await
                .is_err()
        );
        assert_eq!(connection.next_id, 2);
        connection.close().await.unwrap();
    }

    #[cfg(target_os = "linux")]
    async fn assert_not_running(process_id: u32) {
        timeout(Duration::from_secs(2), async {
            loop {
                match tokio::fs::read_to_string(format!("/proc/{process_id}/stat")).await {
                    Ok(stat) => {
                        let state = stat.rsplit_once(") ").unwrap().1.chars().next().unwrap();
                        if state == 'Z' || state == 'X' {
                            return;
                        }
                    }
                    Err(error)
                        if error.kind() == io::ErrorKind::NotFound
                            || error.raw_os_error()
                                == Some(rustix::io::Errno::SRCH.raw_os_error()) =>
                    {
                        return;
                    }
                    Err(error) => panic!("process state read failed: {error}"),
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("ACP descendant remained running");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn close_kills_descendants_even_after_the_group_leader_has_exited() {
        let mut connection = peer(
            r#"IFS= read -r request; sleep 20 >/dev/null 2>/dev/null & descendant=$!; printf '{"jsonrpc":"2.0","id":1,"result":{"pid":%s}}\n' "$descendant"; exit 0"#,
        )
        .await;
        let (result, _) = connection
            .request("session/prompt", json!({}), &cancellation(), deadline())
            .await
            .unwrap();
        let descendant = result["pid"].as_u64().unwrap() as u32;
        timeout(CLOSE_TIMEOUT, connection.child.wait())
            .await
            .unwrap()
            .unwrap();
        connection.close().await.unwrap();
        assert_not_running(descendant).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn drop_kills_the_process_group() {
        let mut connection = peer(
            r#"IFS= read -r request; sleep 20 & descendant=$!; printf '{"jsonrpc":"2.0","id":1,"result":{"pid":%s}}\n' "$descendant"; wait"#,
        )
        .await;
        let leader = connection.child.id().unwrap();
        let (result, _) = connection
            .request("session/prompt", json!({}), &cancellation(), deadline())
            .await
            .unwrap();
        let descendant = result["pid"].as_u64().unwrap() as u32;
        drop(connection);
        assert_not_running(leader).await;
        assert_not_running(descendant).await;
    }
}
