use crate::state::BridgeState;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const REAP_TIMEOUT: Duration = Duration::from_secs(2);
type PendingResponse = oneshot::Sender<Result<Value, String>>;
type PendingRequests = Arc<Mutex<HashMap<u64, PendingResponse>>>;
type Writer = mpsc::UnboundedSender<OutgoingFrame>;
type Connection = (Writer, PendingRequests, CancellationToken);

struct OutgoingFrame {
    line: String,
    flushed: Option<oneshot::Sender<Result<(), String>>>,
}

struct LiveTransport {
    writer: Writer,
    pending: PendingRequests,
    stop: CancellationToken,
    ready: bool,
    child: Child,
    process_group: Option<rustix::process::Pid>,
    reader_task: Option<JoinHandle<()>>,
    writer_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
}

impl LiveTransport {
    fn connection(&self) -> Connection {
        (self.writer.clone(), self.pending.clone(), self.stop.clone())
    }

    async fn shutdown(&mut self) -> Result<(), String> {
        self.stop.cancel();
        if let Some(group) = self.process_group {
            match rustix::process::kill_process_group(group, rustix::process::Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => {
                    return Err(format!(
                        "cannot stop old Codex process group: {error}; writer leases retained"
                    ));
                }
            }
        }
        tokio::time::timeout(REAP_TIMEOUT, self.child.wait())
            .await
            .map_err(|_| "old Codex process did not exit; writer leases retained".to_owned())?
            .map_err(|error| {
                format!("cannot reap old Codex process: {error}; writer leases retained")
            })?;
        // Retain handles across cancellation of this future. Reconnect must join
        // the old reader's disconnected() before publishing a new transport.
        for slot in [
            &mut self.reader_task,
            &mut self.writer_task,
            &mut self.stderr_task,
        ] {
            if let Some(task) = slot.as_mut() {
                let _ = task.await;
            }
            *slot = None;
        }
        if let Some(group) = self.process_group {
            wait_process_group_exit(group, REAP_TIMEOUT).await?;
            self.process_group = None;
        }
        Ok(())
    }
}

async fn wait_process_group_exit(
    group: rustix::process::Pid,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match rustix::process::test_kill_process_group(group) {
            Err(rustix::io::Errno::SRCH) => return Ok(()),
            Err(error) => return Err(format!("cannot verify old Codex process group exit: {error}; writer leases retained")),
            Ok(()) if tokio::time::Instant::now() >= deadline => return Err("old Codex process group still exists; writer leases retained, supervised service restart required".into()),
            Ok(()) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
}

impl Drop for LiveTransport {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(group) = self.process_group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = self.child.start_kill();
    }
}

pub struct AppServerClient {
    codex_bin: PathBuf,
    codex_args: Vec<String>,
    state: Arc<BridgeState>,
    next_id: AtomicU64,
    stopping: AtomicBool,
    transport: Mutex<Option<LiveTransport>>,
}

impl AppServerClient {
    pub fn new(codex_bin: PathBuf, codex_args: Vec<String>, state: Arc<BridgeState>) -> Arc<Self> {
        Arc::new(Self {
            codex_bin,
            codex_args,
            state,
            next_id: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            transport: Mutex::new(None),
        })
    }

    pub fn codex_bin(&self) -> &Path {
        &self.codex_bin
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.request_inner(method, params, None).await
    }

    pub(crate) async fn writer_request(
        &self,
        method: &str,
        params: Value,
        id: &str,
        cwd: &Path,
        mode: &str,
    ) -> Result<Value, String> {
        self.request_inner(method, params, Some((id, cwd, mode)))
            .await
    }

    pub(crate) async fn prepare_writer(
        &self,
        id: &str,
        cwd: &Path,
        mode: &str,
    ) -> Result<Option<Arc<anchor_file_lease::ProjectLease>>, String> {
        self.ensure_connected().await?;
        self.state.acquire_writer(id, cwd, mode).await
    }

    async fn request_inner(
        &self,
        method: &str,
        params: Value,
        writer_scope: Option<(&str, &Path, &str)>,
    ) -> Result<Value, String> {
        let (writer, pending, stop) = self.ensure_connected().await?;
        // Pin across the reply too: terminal events may arrive before its RPC.
        let _writer_lease = if let Some((thread, cwd, mode)) = writer_scope {
            self.state.acquire_writer(thread, cwd, mode).await?
        } else {
            None
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut map = pending.lock().await;
            if self.stopping.load(Ordering::SeqCst) || stop.is_cancelled() {
                return Err("codex app-server connection closed".into());
            }
            map.insert(id, tx);
        }
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if queue_frame(&writer, payload).is_err() {
            pending.lock().await.remove(&id);
            return Err("codex app-server stdin closed".into());
        }
        match tokio::time::timeout(RPC_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(format!(
                "codex app-server connection closed during `{method}`"
            )),
            Err(_) => {
                pending.lock().await.remove(&id);
                Err(format!("codex app-server request `{method}` timed out"))
            }
        }
    }

    pub async fn respond_to_server_request(
        &self,
        thread_id: &str,
        request_key: &str,
        rpc_id: Value,
        result: Value,
    ) -> Result<(), String> {
        // Never reconnect to answer an ID belonging to the old server. Hold the
        // transport lease through flush so reconnect cannot redirect the reply.
        let mut guard = self.transport.lock().await;
        if self.stopping.load(Ordering::SeqCst) {
            return Err("codex app-server client is shut down".into());
        }
        let live = guard.as_mut().ok_or("codex app-server connection closed")?;
        if !live.ready || live.stop.is_cancelled() || !matches!(live.child.try_wait(), Ok(None)) {
            return Err("codex app-server connection closed before sending response".into());
        }
        let thread = self
            .state
            .threads
            .read()
            .await
            .get(thread_id)
            .filter(|thread| thread.loaded)
            .cloned()
            .ok_or("server request is no longer pending; inspect again")?;
        let request = thread
            .pending_requests
            .iter()
            .find(|request| request.request_key == request_key && request.rpc_id == rpc_id)
            .ok_or("server request is no longer pending; inspect again")?;
        let grants_execution = approval_grants_execution(&request.method, &result);
        if grants_execution && thread.mode != "workspace-write" {
            // A session-scoped approval can outlive this turn. Do not silently
            // upgrade a read-only session or release its lease before that grant.
            return Err("read-only session cannot approve execution escalation; decline or interrupt, then explicitly start a workspace-write turn".into());
        }
        let _lease = if grants_execution {
            self.state
                .acquire_writer(thread_id, &thread.cwd, &thread.mode)
                .await?
        } else {
            None
        };
        flush_frame(
            &live.writer,
            &live.stop,
            json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "result": result,
            }),
        )
        .await
    }

    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let mut guard = self.transport.lock().await;
        if let Some(live) = guard.as_mut()
            && let Err(error) = live.shutdown().await
        {
            eprintln!("{error}");
            return;
        }
        // Keep outstanding guards owned by state until the service exits.
        // Production requires KillMode=control-group: Codex commands may
        // have independent groups that are not proved dead by waitpid.
        *guard = None;
    }

    async fn ensure_connected(&self) -> Result<Connection, String> {
        if self.stopping.load(Ordering::SeqCst) {
            return Err("codex app-server client is shut down".into());
        }
        let mut guard = self.transport.lock().await;
        if self.stopping.load(Ordering::SeqCst) {
            return Err("codex app-server client is shut down".into());
        }
        if let Some(live) = guard.as_mut() {
            if live.ready && !live.stop.is_cancelled() && matches!(live.child.try_wait(), Ok(None))
            {
                return Ok(live.connection());
            }
            live.shutdown().await?;
            if self.state.has_writer_leases() {
                return Err("writer execution disconnected; leases retained until supervised service restart with KillMode=control-group".into());
            }
        }
        *guard = None;
        if self.stopping.load(Ordering::SeqCst) {
            return Err("codex app-server client is shut down".into());
        }
        // Store ownership before the first handshake await. If a caller is
        // cancelled, the next caller cleans up this unready transport first.
        *guard = Some(self.spawn_transport()?);
        let live = guard.as_mut().expect("transport just installed");
        let result = self.handshake(live).await.and_then(|()| {
            if self.stopping.load(Ordering::SeqCst) {
                Err("codex app-server client is shut down".into())
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            live.shutdown()
                .await
                .map_err(|cleanup| format!("{error}; {cleanup}"))?;
            if self.state.has_writer_leases() {
                return Err(format!(
                    "{error}; writer leases retained until supervised service restart"
                ));
            }
            *guard = None;
            return Err(error);
        }
        live.ready = true;
        Ok(live.connection())
    }

    fn spawn_transport(&self) -> Result<LiveTransport, String> {
        let mut command = Command::new(&self.codex_bin);
        command
            .args(&self.codex_args)
            .env_remove(crate::config::TOKEN_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| {
            format!(
                "failed to launch `{}` {:?}: {e}",
                self.codex_bin.display(),
                self.codex_args
            )
        })?;
        let process_group =
            rustix::process::Pid::from_raw(child.id().expect("new child id") as i32)
                .expect("positive child id");
        let stdin = child.stdin.take().expect("piped codex stdin");
        let stdout = child.stdout.take().expect("piped codex stdout");
        let mut stderr = child.stderr.take().expect("piped codex stderr");
        let stop = CancellationToken::new();
        let stderr_stop = stop.clone();
        let stderr_task = tokio::spawn(async move {
            let mut sink = tokio::io::sink();
            tokio::select! {
                _ = stderr_stop.cancelled() => {}
                // Do not accumulate a potentially unbounded stderr line.
                _ = tokio::io::copy(&mut stderr, &mut sink) => {}
            }
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(write_frames(stdin, rx, stop.clone()));
        let pending = PendingRequests::default();
        let reader_pending = pending.clone();
        let reader_stop = stop.clone();
        let reader_writer = tx.clone();
        let state = self.state.clone();
        let reader_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let line = tokio::select! {
                    biased;
                    _ = reader_stop.cancelled() => break,
                    line = lines.next_line() => line,
                };
                let Ok(Some(line)) = line else {
                    break;
                };
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let has_method = message.get("method").and_then(Value::as_str).is_some();
                let has_id = message.get("id").is_some() && !message["id"].is_null();
                if has_id && !has_method {
                    if let Some(id) = message["id"].as_u64()
                        && let Some(waiter) = reader_pending.lock().await.remove(&id)
                    {
                        if let Some(error) = message.get("error") {
                            let msg = error["message"]
                                .as_str()
                                .unwrap_or("JSON-RPC error")
                                .to_owned();
                            let _ = waiter.send(Err(msg));
                        } else {
                            let _ = waiter.send(Ok(message["result"].clone()));
                        }
                    }
                } else if has_method && has_id {
                    let method = message["method"].as_str().unwrap_or_default().to_owned();
                    let rpc_id = message["id"].clone();
                    let params = message.get("params").cloned().unwrap_or(Value::Null);
                    handle_incoming_server_request(&state, &reader_writer, rpc_id, method, params)
                        .await;
                } else if let Some(method) = message.get("method").and_then(Value::as_str) {
                    let params = message.get("params").cloned().unwrap_or(Value::Null);
                    state.apply_native_notification(method, &params).await;
                }
            }
            reader_stop.cancel();
            state.disconnected().await;
            for (_, waiter) in reader_pending.lock().await.drain() {
                let _ = waiter.send(Err("codex app-server connection closed".into()));
            }
        });

        Ok(LiveTransport {
            writer: tx,
            pending,
            stop,
            ready: false,
            child,
            process_group: Some(process_group),
            reader_task: Some(reader_task),
            writer_task: Some(writer_task),
            stderr_task: Some(stderr_task),
        })
    }

    async fn handshake(&self, live: &LiveTransport) -> Result<(), String> {
        let init_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (init_tx, init_rx) = oneshot::channel();
        {
            let mut pending = live.pending.lock().await;
            if live.stop.is_cancelled() {
                return Err("codex app-server closed before initialize".into());
            }
            pending.insert(init_id, init_tx);
        }
        queue_frame(
            &live.writer,
            json!({
                "jsonrpc": "2.0",
                "id": init_id,
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "anchor-codex-bridge",
                        "title": "Anchor Codex Project Manager Bridge",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": { "experimentalApi": true }
                }
            }),
        )?;
        match tokio::time::timeout(Duration::from_secs(15), init_rx).await {
            Ok(Ok(Ok(_))) => {}
            Ok(Ok(Err(err))) => return Err(format!("codex initialize rejected: {err}")),
            Ok(Err(_)) => return Err("codex app-server closed during initialize".into()),
            Err(_) => return Err("codex initialize timed out".into()),
        }
        flush_frame(
            &live.writer,
            &live.stop,
            json!({
                "jsonrpc": "2.0",
                "method": "initialized",
            }),
        )
        .await
    }
}

fn approval_grants_execution(method: &str, result: &Value) -> bool {
    if method == "item/permissions/requestApproval" {
        return result["permissions"]
            .as_object()
            .is_none_or(|permissions| !permissions.is_empty());
    }
    if method.ends_with("Approval") {
        return !matches!(
            result["decision"].as_str(),
            Some("decline" | "cancel" | "abort")
        ) && result["decision"].get("denied").is_none();
    }
    false
}

fn queue_frame(writer: &Writer, payload: Value) -> Result<(), String> {
    writer
        .send(OutgoingFrame {
            line: payload.to_string(),
            flushed: None,
        })
        .map_err(|_| "codex app-server stdin closed".to_owned())
}

async fn flush_frame(
    writer: &Writer,
    stop: &CancellationToken,
    payload: Value,
) -> Result<(), String> {
    let (tx, rx) = oneshot::channel();
    writer
        .send(OutgoingFrame {
            line: payload.to_string(),
            flushed: Some(tx),
        })
        .map_err(|_| "codex app-server stdin closed before sending response".to_owned())?;
    let result = match tokio::time::timeout(RPC_TIMEOUT, rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("codex app-server writer stopped before flushing response".into()),
        Err(_) => Err("codex app-server response flush timed out".into()),
    };
    if result.is_err() {
        stop.cancel();
    }
    result
}

async fn write_frames<W: AsyncWrite + Unpin>(
    mut stdin: W,
    mut rx: mpsc::UnboundedReceiver<OutgoingFrame>,
    stop: CancellationToken,
) {
    loop {
        let frame = tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            frame = rx.recv() => frame,
        };
        let Some(mut frame) = frame else {
            break;
        };
        frame.line.push('\n');
        let result = tokio::select! {
            biased;
            _ = stop.cancelled() => Err("codex app-server connection closed before flush".to_owned()),
            result = async {
                stdin.write_all(frame.line.as_bytes()).await?;
                stdin.flush().await
            } => result.map_err(|e| format!("codex app-server stdin write/flush failed: {e}")),
        };
        let failed = result.is_err();
        if let Some(ack) = frame.flushed {
            let _ = ack.send(result);
        }
        if failed {
            break;
        }
    }
    stop.cancel();
}

async fn handle_incoming_server_request(
    state: &BridgeState,
    writer: &Writer,
    rpc_id: Value,
    method: String,
    params: Value,
) {
    if method == "currentTime/read" {
        let _ = queue_frame(
            writer,
            json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "result": { "unixSeconds": crate::state::now_ms() / 1000 }
            }),
        );
        return;
    }
    // Unknown thread-scoped methods remain pending for the authorized caller;
    // only unsupported host-wide requests or stale thread requests fail closed.
    let recorded = state
        .record_server_request(rpc_id.clone(), method.clone(), params)
        .await;
    if !recorded {
        // Unknown or stale requests fail closed rather than hanging the server.
        let _ = queue_frame(
            writer,
            json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "error": {
                    "code": -32000,
                    "message": format!("bridge has no active tracked thread for `{method}`")
                }
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::AsyncReadExt;

    struct GatedFlush {
        gate: oneshot::Receiver<bool>,
        entered: Option<oneshot::Sender<()>>,
    }

    impl AsyncWrite for GatedFlush {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            match Pin::new(&mut self.gate).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(true)) => Poll::Ready(Ok(())),
                _ => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "flush failed",
                ))),
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn response_ack_waits_for_actual_flush_and_propagates_failure() {
        for succeed in [true, false] {
            let (gate_tx, gate) = oneshot::channel();
            let (entered_tx, entered_rx) = oneshot::channel();
            let (writer, rx) = mpsc::unbounded_channel();
            let stop = CancellationToken::new();
            let task = tokio::spawn(write_frames(
                GatedFlush {
                    gate,
                    entered: Some(entered_tx),
                },
                rx,
                stop.clone(),
            ));
            let reply_stop = stop.clone();
            let reply = tokio::spawn(async move {
                flush_frame(&writer, &reply_stop, json!({"id": 7, "result": {}})).await
            });
            tokio::time::timeout(Duration::from_secs(2), entered_rx)
                .await
                .unwrap()
                .unwrap();
            assert!(!reply.is_finished(), "queue acceptance is not delivery");
            gate_tx.send(succeed).unwrap();
            assert_eq!(reply.await.unwrap().is_ok(), succeed);
            stop.cancel();
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn cancellation_closes_idle_writer_and_blocked_flush() {
        for blocked in [false, true] {
            let (stdin, mut server) = tokio::io::duplex(1);
            let (writer, rx) = mpsc::unbounded_channel();
            let stop = CancellationToken::new();
            let task = tokio::spawn(write_frames(stdin, rx, stop.clone()));
            let reply = if blocked {
                let reply_stop = stop.clone();
                let writer = writer.clone();
                let reply = tokio::spawn(async move {
                    flush_frame(&writer, &reply_stop, json!({"result": "blocked"})).await
                });
                let mut byte = [0];
                server.read_exact(&mut byte).await.unwrap();
                Some(reply)
            } else {
                None
            };
            stop.cancel();
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert!(writer.is_closed());
            if let Some(reply) = reply {
                assert!(reply.await.unwrap().is_err());
            }
        }
    }

    #[tokio::test]
    async fn unknown_thread_requests_stay_pending_but_host_requests_fail_closed() {
        let state = BridgeState::new();
        state
            .apply_notification(
                "thread/started",
                &json!({"thread": {"id": "t", "cwd": "/tmp"}}),
            )
            .await;
        let (writer, mut rx) = mpsc::unbounded_channel();
        for (index, method, status) in [
            (
                0,
                "future/request",
                crate::state::WorkerStatus::WaitingInput,
            ),
            (
                1,
                "item/commandExecution/requestApproval",
                crate::state::WorkerStatus::WaitingApproval,
            ),
            (
                2,
                "future/requestApproval",
                crate::state::WorkerStatus::WaitingApproval,
            ),
            (
                3,
                "execCommandApproval",
                crate::state::WorkerStatus::WaitingApproval,
            ),
            (
                4,
                "mcpServer/elicitation/request",
                crate::state::WorkerStatus::WaitingInput,
            ),
        ] {
            let params = json!({"threadId": "t", "payload": {"original": true}});
            handle_incoming_server_request(
                &state,
                &writer,
                json!(index),
                method.into(),
                params.clone(),
            )
            .await;
            assert!(matches!(
                rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            {
                let threads = state.threads.read().await;
                assert_eq!(threads["t"].effective_status(), status);
                assert_eq!(threads["t"].pending_requests[0].params, params);
                assert_eq!(threads["t"].pending_requests[0].method, method);
            }
            state
                .apply_notification(
                    "serverRequest/resolved",
                    &json!({"threadId": "t", "requestId": index}),
                )
                .await;
        }
        let before = state.native_events("t", None, 100).await;
        assert_eq!(before["events"].as_array().unwrap().len(), 5);
        handle_incoming_server_request(
            &state,
            &writer,
            json!("host"),
            "unsupported/host/request".into(),
            json!({"account": "host-only"}),
        )
        .await;
        let reply: Value = serde_json::from_str(&rx.try_recv().unwrap().line).unwrap();
        assert_eq!(reply["id"], "host");
        assert_eq!(reply["error"]["code"], -32000);
        assert_eq!(state.native_events("t", None, 100).await, before);
        assert!(state.threads.read().await["t"].pending_requests.is_empty());
    }

    #[tokio::test]
    async fn current_time_reply_uses_unix_seconds_without_recording_an_approval() {
        let state = BridgeState::new();
        let (writer, mut rx) = mpsc::unbounded_channel();
        handle_incoming_server_request(
            &state,
            &writer,
            json!("clock"),
            "currentTime/read".into(),
            Value::Null,
        )
        .await;
        let message: Value = serde_json::from_str(&rx.recv().await.unwrap().line).unwrap();
        assert_eq!(message["id"], "clock");
        assert!(message["result"]["unixSeconds"].is_u64());
        assert_eq!(message["result"].as_object().unwrap().len(), 1);
        assert!(state.threads.read().await.is_empty());
    }

    #[cfg(unix)]
    fn fixture(state: Arc<BridgeState>) -> Arc<AppServerClient> {
        // No provider, sleeps, files, or grandchildren: synchronization is RPC IO.
        let script = r#"
reply() {
    id=${line#*'"id":'}
    id=${id%%,*}
    printf '{"id":%s,"result":{}}\n' "$id"
}
while IFS= read -r line; do
    id=${line#*'"id":'}
    id=${id%%,*}
    case "$line" in
        *'"method":"initialized"'*) ;;
        *'"method":"initialize"'*) reply ;;
        *'"method":"begin"'*)
            printf '%s\n' '{"method":"thread/started","params":{"thread":{"id":"t","cwd":"/tmp"}}}'
            printf '{"method":"turn/started","params":{"threadId":"t","turn":{"id":"turn-%s"}}}\n' "$id"
            printf '{"id":7,"method":"item/tool/requestUserInput","params":{"threadId":"t","turnId":"turn-%s"}}\n' "$id"
            reply ;;
        *'"method":"close"'*) exit 0 ;;
        *'"method":"close-output"'*) exec 1>&- ;;
        *'"method":"hang"'*) ;;
        *'"method":'*) reply ;;
    esac
done
"#;
        AppServerClient::new(
            PathBuf::from("/bin/sh"),
            vec!["-c".into(), script.into()],
            state,
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn approval_replies_cannot_upgrade_readonly_or_bypass_project_lease() {
        for (method, accept, decline) in [
            (
                "item/commandExecution/requestApproval",
                json!({"decision":"accept"}),
                json!({"decision":"decline"}),
            ),
            (
                "item/fileChange/requestApproval",
                json!({"decision":"acceptForSession"}),
                json!({"decision":"cancel"}),
            ),
            (
                "execCommandApproval",
                json!({"decision":"approved"}),
                json!({"decision":{"denied":{"rejection":"No"}}}),
            ),
            (
                "applyPatchApproval",
                json!({"decision":"approved_for_session"}),
                json!({"decision":"abort"}),
            ),
            (
                "item/permissions/requestApproval",
                json!({"permissions":{"fileSystem":{"write":["project"]}},"scope":"turn"}),
                json!({"permissions":{},"scope":"turn"}),
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let state = BridgeState::new();
            let client = fixture(state.clone());
            client.request("begin", Value::Null).await.unwrap();
            let (key, turn) = {
                let mut threads = state.threads.write().await;
                let thread = threads.get_mut("t").unwrap();
                thread.cwd = root.path().to_owned();
                thread.pending_requests[0].method = method.into();
                (
                    thread.pending_requests[0].request_key.clone(),
                    thread.active_turn_id.clone().unwrap(),
                )
            };
            let host = anchor_file_lease::ProjectLease::acquire(root.path()).unwrap();
            assert!(
                client
                    .respond_to_server_request("t", &key, json!(7), accept.clone())
                    .await
                    .unwrap_err()
                    .contains("read-only")
            );
            client
                .respond_to_server_request("t", &key, json!(7), decline)
                .await
                .unwrap();
            assert_eq!(state.threads.read().await["t"].mode, "read-only");
            state.threads.write().await.get_mut("t").unwrap().mode = "workspace-write".into();
            assert!(
                client
                    .respond_to_server_request("t", &key, json!(7), accept.clone())
                    .await
                    .unwrap_err()
                    .contains("writer lease")
            );
            drop(host);
            client
                .respond_to_server_request("t", &key, json!(7), accept)
                .await
                .unwrap();
            assert!(anchor_file_lease::ProjectLease::acquire(root.path()).is_err());
            state
                .apply_notification(
                    "turn/completed",
                    &json!({"threadId":"t","turn":{"id":turn,"status":"completed"}}),
                )
                .await;
            anchor_file_lease::ProjectLease::acquire(root.path()).unwrap();
            client.shutdown().await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn eof_clears_state_before_reconnect_and_stale_response_does_not_reconnect() {
        let state = BridgeState::new();
        let client = fixture(state.clone());
        client.request("begin", Value::Null).await.unwrap();
        let old_key = state.threads.read().await["t"].pending_requests[0]
            .request_key
            .clone();
        let before = state.native_events("t", None, 10).await;
        assert_eq!(
            before["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "thread/started",
                "turn/started",
                "item/tool/requestUserInput"
            ]
        );
        assert_eq!(before["events"][2]["request_key"], old_key);
        client
            .respond_to_server_request("t", &old_key, json!(7), json!({}))
            .await
            .unwrap();
        let (writer, pending, stop) = client.ensure_connected().await.unwrap();
        assert!(client.request("close", Value::Null).await.is_err());
        {
            let threads = state.threads.read().await;
            assert!(!threads["t"].loaded);
            assert_eq!(
                threads["t"].last_error.as_deref(),
                Some("connection lost; inspect history before resume")
            );
            assert!(threads["t"].active_turn_id.is_none());
            assert!(threads["t"].pending_requests.is_empty());
        }
        assert!(stop.is_cancelled());
        assert!(pending.lock().await.is_empty());
        let next_id = client.next_id.load(Ordering::Relaxed);
        assert!(
            client
                .respond_to_server_request("t", &old_key, json!(7), json!({}))
                .await
                .is_err()
        );
        assert_eq!(client.next_id.load(Ordering::Relaxed), next_id);
        client.request("begin", Value::Null).await.unwrap();
        let new_key = state.threads.read().await["t"].pending_requests[0]
            .request_key
            .clone();
        let after = state.native_events("t", Some(3), 10).await;
        assert_eq!(after["stream_epoch"], before["stream_epoch"]);
        assert_eq!(after["next_cursor"], 6);
        assert_eq!(after["events"][2]["request_key"], new_key);
        assert_ne!(old_key, new_key);
        assert!(writer.is_closed());
        assert!(
            client
                .respond_to_server_request("t", &old_key, json!(7), json!({}))
                .await
                .is_err()
        );
        assert!(
            client
                .respond_to_server_request("wrong-thread", &new_key, json!(7), json!({}))
                .await
                .is_err()
        );
        assert!(
            client
                .respond_to_server_request("t", &new_key, json!("7"), json!({}))
                .await
                .is_err()
        );
        client
            .respond_to_server_request("t", &new_key, json!(7), json!({}))
            .await
            .unwrap();
        client.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reconnect_joins_old_reader_before_new_state_can_be_published() {
        let state = BridgeState::new();
        let client = fixture(state.clone());
        client.request("begin", Value::Null).await.unwrap();
        let (_, _, stop) = client.ensure_connected().await.unwrap();
        let threads = state.threads.write().await;
        stop.cancel();
        let (entered_tx, entered_rx) = oneshot::channel();
        let next_client = client.clone();
        let reconnect = tokio::spawn(async move {
            let _ = entered_tx.send(());
            next_client.ensure_connected().await
        });
        entered_rx.await.unwrap();
        // The old disconnected() is deliberately blocked by this write lock.
        assert!(!reconnect.is_finished());
        drop(threads);
        tokio::time::timeout(Duration::from_secs(2), reconnect)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        state.threads.write().await.get_mut("t").unwrap().loaded = true;
        client.request("barrier", Value::Null).await.unwrap();
        assert!(state.threads.read().await["t"].loaded);
        client.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_reaps_child_joins_all_io_and_drains_pending_requests() {
        let state = BridgeState::new();
        let client = fixture(state.clone());
        client.request("begin", Value::Null).await.unwrap();
        let mut live = client.transport.lock().await.take().unwrap();
        let (pending_tx, pending_rx) = oneshot::channel();
        live.pending.lock().await.insert(999, pending_tx);
        tokio::time::timeout(Duration::from_secs(2), live.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert!(live.child.id().is_none());
        assert!(live.child.try_wait().unwrap().is_some());
        assert!(
            live.reader_task.is_none() && live.writer_task.is_none() && live.stderr_task.is_none()
        );
        assert!(live.writer.is_closed());
        assert!(pending_rx.await.unwrap().is_err());
        assert!(live.pending.lock().await.is_empty());
        assert!(!state.threads.read().await["t"].loaded);
        live.shutdown().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_process_group_timeout_retains_project_lease() {
        let root = tempfile::tempdir().unwrap();
        let state = BridgeState::new();
        let client = fixture(state.clone());
        client.request("begin", Value::Null).await.unwrap();
        state.threads.write().await.get_mut("t").unwrap().cwd = root.path().to_owned();
        drop(
            client
                .prepare_writer("t", root.path(), "workspace-write")
                .await
                .unwrap(),
        );
        let group = client
            .transport
            .lock()
            .await
            .as_ref()
            .unwrap()
            .process_group
            .unwrap();
        assert!(
            wait_process_group_exit(group, Duration::ZERO)
                .await
                .unwrap_err()
                .contains("still exists")
        );
        state.disconnected().await;
        state.release_idle_writer("t").await;
        assert!(anchor_file_lease::ProjectLease::acquire(root.path()).is_err());
        client.shutdown().await;
        wait_process_group_exit(group, Duration::ZERO)
            .await
            .unwrap();
        assert!(anchor_file_lease::ProjectLease::acquire(root.path()).is_err());
        drop(client);
        drop(state);
        anchor_file_lease::ProjectLease::acquire(root.path()).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdout_eof_blocks_reconnect_and_keeps_lease_until_service_teardown() {
        let root = tempfile::tempdir().unwrap();
        let state = BridgeState::new();
        let client = fixture(state.clone());
        client.request("begin", Value::Null).await.unwrap();
        state.threads.write().await.get_mut("t").unwrap().cwd = root.path().to_owned();
        drop(
            client
                .prepare_writer("t", root.path(), "workspace-write")
                .await
                .unwrap(),
        );
        assert!(client.request("close-output", Value::Null).await.is_err());
        assert!(
            client
                .transport
                .lock()
                .await
                .as_mut()
                .unwrap()
                .child
                .try_wait()
                .unwrap()
                .is_none()
        );
        assert_eq!(
            anchor_file_lease::ProjectLease::acquire(root.path())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(
            client
                .ensure_connected()
                .await
                .err()
                .unwrap()
                .contains("supervised service restart")
        );
        assert!(anchor_file_lease::ProjectLease::acquire(root.path()).is_err());
        client.shutdown().await;
        assert!(anchor_file_lease::ProjectLease::acquire(root.path()).is_err());
        drop(client);
        drop(state);
        anchor_file_lease::ProjectLease::acquire(root.path()).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_shutdown_is_terminal_even_for_waiting_connection_admission() {
        let state = BridgeState::new();
        let client = fixture(state);
        client.request("barrier", Value::Null).await.unwrap();
        let guard = client.transport.lock().await;
        let shutdown_client = client.clone();
        let shutdown = tokio::spawn(async move { shutdown_client.shutdown().await });
        while !client.stopping.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        let next_id = client.next_id.load(Ordering::Relaxed);
        assert!(client.ensure_connected().await.is_err());
        drop(guard);
        tokio::time::timeout(Duration::from_secs(2), shutdown)
            .await
            .unwrap()
            .unwrap();
        assert!(client.request("barrier", Value::Null).await.is_err());
        assert!(
            client
                .respond_to_server_request("t", "0:7", json!(7), Value::Null)
                .await
                .is_err()
        );
        assert_eq!(client.next_id.load(Ordering::Relaxed), next_id);
        assert!(client.transport.lock().await.is_none());
        client.shutdown().await;

        let never_started = fixture(BridgeState::new());
        never_started.shutdown().await;
        assert!(never_started.ensure_connected().await.is_err());
        assert_eq!(never_started.next_id.load(Ordering::Relaxed), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_handshake_leaves_no_transport_and_clears_loaded_threads() {
        let state = BridgeState::new();
        state
            .apply_notification(
                "thread/started",
                &json!({"thread": {"id": "t", "cwd": "/tmp"}}),
            )
            .await;
        let client = AppServerClient::new(
            PathBuf::from("/bin/sh"),
            vec!["-c".into(), "exit 0".into()],
            state.clone(),
        );
        assert!(client.ensure_connected().await.is_err());
        assert!(client.transport.lock().await.is_none());
        assert!(!state.threads.read().await["t"].loaded);
    }
}
