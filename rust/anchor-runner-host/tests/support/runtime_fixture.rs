//! Isolated production Host and HTTP Provider fixtures. No dotenv or inherited credentials.
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, oneshot};

pub enum Reply {
    Tool(&'static str, Value),
    Text(String),
    Unavailable,
    BadRequest,
    Gated(Arc<Gate>, Box<Reply>),
}

pub struct Gate {
    entered: AtomicBool,
    release: Semaphore,
}

impl Gate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: AtomicBool::new(false),
            release: Semaphore::new(0),
        })
    }

    pub fn wait_entered(&self) {
        wait_until("Provider checkpoint", || {
            self.entered.load(Ordering::Acquire)
        });
    }

    pub fn open(&self) {
        self.release.add_permits(1);
    }
}

pub fn wait_until(description: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if condition() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Default)]
struct ProviderState {
    scripts: BTreeMap<String, VecDeque<Reply>>,
    requests: Vec<Value>,
    failures: Vec<String>,
}

pub struct Provider {
    pub url: String,
    state: Arc<Mutex<ProviderState>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<thread::JoinHandle<()>>,
}

async fn completion(
    State(state): State<Arc<Mutex<ProviderState>>>,
    Json(request): Json<Value>,
) -> Response {
    let model = request["model"].as_str().unwrap_or_default().to_owned();
    let stream = request["stream"] == true;
    let (sequence, mut reply) = {
        let mut state = state.lock().unwrap();
        state.requests.push(request);
        let sequence = state.requests.len();
        let reply = state.scripts.get_mut(&model).and_then(VecDeque::pop_front);
        (sequence, reply)
    };
    while let Some(Reply::Gated(gate, next)) = reply {
        gate.entered.store(true, Ordering::Release);
        gate.release.acquire().await.unwrap().forget();
        reply = Some(*next);
    }
    match reply {
        Some(Reply::Unavailable | Reply::BadRequest) => (
            if matches!(reply, Some(Reply::Unavailable)) { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::BAD_REQUEST },
            Json(json!({"error":{"message":"deterministic transport failure","type":"server_error"}})),
        ).into_response(),
        Some(Reply::Gated(_, _)) => unreachable!(),
        Some(Reply::Text(text)) if stream => {
            let delta = json!({"id":format!("fixture-{sequence}"),"object":"chat.completion.chunk","created":1,"model":model,
                "choices":[{"index":0,"finish_reason":null,"delta":{"role":"assistant","content":text}}]});
            let finished = json!({"id":format!("fixture-{sequence}"),"object":"chat.completion.chunk","created":1,"model":model,
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}});
            ([(axum::http::header::CONTENT_TYPE,"text/event-stream")], format!("data: {delta}\n\ndata: {finished}\n\ndata: [DONE]\n\n")).into_response()
        }
        Some(Reply::Text(text)) => (StatusCode::OK, Json(json!({"id":format!("fixture-{sequence}"),"object":"chat.completion","created":1,"model":model,
            "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}],
            "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}))).into_response(),
        Some(Reply::Tool(name, arguments)) if stream => {
            let call = json!({
                "id":format!("fixture-{sequence}"),"object":"chat.completion.chunk","created":1,"model":model,
                "choices":[{"index":0,"finish_reason":null,"delta":{
                    "role":"assistant","tool_calls":[{"index":0,"id":format!("call-{sequence}"),"type":"function",
                        "function":{"name":name,"arguments":arguments.to_string()}}]
                }}]
            });
            let finished = json!({
                "id":format!("fixture-{sequence}"),"object":"chat.completion.chunk","created":1,"model":model,
                "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
            });
            ([(axum::http::header::CONTENT_TYPE,"text/event-stream")], format!("data: {call}\n\ndata: {finished}\n\ndata: [DONE]\n\n")).into_response()
        }
        Some(Reply::Tool(name, arguments)) => (
            StatusCode::OK,
            Json(json!({
                "id":format!("fixture-{sequence}"), "object":"chat.completion", "created":1,
                "model":model,
                "choices":[{"index":0,"finish_reason":"tool_calls","message":{
                    "role":"assistant","content":null,
                    "tool_calls":[{"id":format!("call-{sequence}"),"type":"function",
                        "function":{"name":name,"arguments":arguments.to_string()}}]
                }}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
            })),
        ).into_response(),
        None => {
            state.lock().unwrap().failures.push(format!("unexpected request {sequence} for {model}"));
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":{"message":"fixture script exhausted","type":"fixture_error"}})),
            ).into_response()
        }
    }
}

impl Provider {
    pub fn new(scripts: impl IntoIterator<Item = (&'static str, Vec<Reply>)>) -> Self {
        let state = Arc::new(Mutex::new(ProviderState {
            scripts: scripts
                .into_iter()
                .map(|(model, replies)| (model.into(), replies.into()))
                .collect(),
            ..Default::default()
        }));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (stop, stopped) = oneshot::channel();
        let shared = state.clone();
        let task = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let app = Router::new()
                        .route("/v1/chat/completions", post(completion))
                        .with_state(shared);
                    let server = tokio::spawn(async move {
                        axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                            .await
                            .unwrap();
                    });
                    tokio::select! {
                        result = server => { result.unwrap(); }
                        _ = stopped => {}
                    }
                });
        });
        Self {
            url: format!("http://{address}/v1"),
            state,
            stop: Some(stop),
            task: Some(task),
        }
    }

    pub fn requests(&self) -> Vec<Value> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn append_replies(&self, model: &str, replies: Vec<Reply>) {
        self.state
            .lock()
            .unwrap()
            .scripts
            .entry(model.into())
            .or_default()
            .extend(replies);
    }

    pub fn assert_consumed(&self) {
        let state = self.state.lock().unwrap();
        assert!(state.failures.is_empty(), "{:?}", state.failures);
        for (model, script) in &state.scripts {
            assert!(script.is_empty(), "unconsumed replies for {model}");
        }
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

pub fn command(script: &str) -> Reply {
    Reply::Tool("anchor_run", json!({"command":["sh","-c",script]}))
}

pub fn complete(route: Option<&str>) -> Reply {
    Reply::Tool(
        "final_result",
        json!({"summary":"fixture completed","route":route}),
    )
}

pub struct Host {
    pub root: tempfile::TempDir,
    binary: PathBuf,
    started: Instant,
    allowed_commands: String,
}

impl Host {
    pub fn new(graph: &Value) -> Self {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("bundle");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("graph.json"), graph.to_string()).unwrap();
        fs::write(
            bundle.join("manifest.json"),
            r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
        )
        .unwrap();
        let binary = root.path().join("anchor-runner-host");
        if fs::hard_link(env!("CARGO_BIN_EXE_anchor-runner-host"), &binary).is_err() {
            fs::copy(env!("CARGO_BIN_EXE_anchor-runner-host"), &binary).unwrap();
        }
        Self {
            root,
            binary,
            started: Instant::now(),
            allowed_commands: "sh,cat,git,true".into(),
        }
    }

    #[allow(dead_code)]
    pub fn with_allowed_commands(mut self, commands: &str) -> Self {
        self.allowed_commands = commands.to_owned();
        self
    }

    fn process(&self, provider: Option<&Provider>) -> Command {
        let mut process = Command::new(&self.binary);
        process
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("TZ", "UTC")
            .env("ANCHOR_RUNNER_STATE_ROOT", self.root.path().join("state"))
            .env(
                "ANCHOR_RUNNER_WORKSPACE_ROOT",
                self.root.path().join("work"),
            )
            .env("ANCHOR_RUNNER_BUNDLE_ROOT", self.root.path().join("bundle"))
            .env("ANCHOR_RUNNER_CATALOG_ROOT", self.root.path())
            .env("ANCHOR_RUNNER_GRAPH_NAME", "fixture")
            .env(
                "ANCHOR_RUNNER_SCHEDULES_PATH",
                self.root.path().join("schedules.json"),
            )
            .env("ANCHOR_RUNNER_ALLOWED_COMMANDS", &self.allowed_commands)
            .current_dir(self.root.path());
        if let Some(provider) = provider {
            process
            .env("ANCHOR_MODEL_API_KEY", "fixture-only-not-a-secret")
            .env("ANCHOR_MODEL_URL", &provider.url)
            .env("ANCHOR_MODEL_WIRE_API", "chat")
            .env("ANCHOR_MODEL_NAME", "fixture-default")
            .env("ANCHOR_MODEL_IMAGE_MODELS", r#"["fixture-worker"]"#)
            .env("ANCHOR_MODEL_ALIASES", r#"{"models.worker":"fixture-worker","models.left":"fixture-left","models.right":"fixture-right"}"#);
        }
        process
    }

    pub fn run(&self, provider: &Provider) -> Value {
        let mut child = self
            .process(Some(provider))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let request = serde_json::to_vec(&json!({
            "op":"start_bundle","version":1,"request_id":"fixture","run_id":"fixture","input":{}
        }))
        .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(&(request.len() as u32).to_be_bytes())
            .unwrap();
        stdin.write_all(&request).unwrap();
        drop(stdin);

        // Drain both pipes concurrently and enforce a deadline without leaving a Host behind.
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let read = |mut pipe: Box<dyn std::io::Read + Send>| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                pipe.read_to_end(&mut bytes).unwrap();
                bytes
            })
        };
        let out = read(Box::new(stdout));
        let err = read(Box::new(stderr));
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                break None;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let stdout = out.join().unwrap();
        let stderr = err.join().unwrap();
        assert!(
            status.is_some_and(|status| status.success()),
            "Host failed or timed out: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert!(stdout.len() >= 4, "missing framed response");
        let length = u32::from_be_bytes(stdout[..4].try_into().unwrap()) as usize;
        assert_eq!(stdout.len(), length + 4, "unexpected Host frames");
        serde_json::from_slice(&stdout[4..]).unwrap()
    }

    pub fn record(&self) -> Value {
        self.record_for("fixture")
    }

    pub fn record_for(&self, run: &str) -> Value {
        read_json(
            self.root
                .path()
                .join("state/runs")
                .join(format!("{run}.json")),
        )
    }

    pub fn serve(&self, provider: &Provider) -> HttpHost {
        self.serve_process(self.process(Some(provider)))
    }

    pub fn serve_without_model(&self) -> HttpHost {
        self.serve_process(self.process(None))
    }

    fn serve_process(&self, mut process: Command) -> HttpHost {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        drop(socket);
        let log = self.root.path().join("http-host.log");
        let child = process
            .arg("serve")
            .env("ANCHOR_RUNNER_LISTEN", address.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let mut server = HttpHost {
            child,
            url: format!("http://{address}"),
            log,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        wait_until("HTTP Host startup", || {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "Host startup failed: {}",
                fs::read_to_string(&server.log).unwrap()
            );
            server
                .try_request("GET", "/health", None)
                .is_ok_and(|(status, _)| status == 200)
        });
        server
    }

    pub fn artifact(&self, record: &Value, node: &str) -> PathBuf {
        self.root
            .path()
            .join("state/artifacts")
            .join(record["results"][node][0]["commit"]["id"].as_str().unwrap())
    }

    pub fn file(&self, record: &Value, node: &str, name: &str) -> Vec<u8> {
        fs::read(self.artifact(record, node).join("files").join(name)).unwrap()
    }

    pub fn workspace_files(&self, run: &str, name: &str) -> Vec<Vec<u8>> {
        let root = self.root.path().join("work").join(run);
        let mut files = Vec::new();
        visit_files(&root, &root, &mut |path, _| {
            if path.file_name().is_some_and(|file| file == name) {
                files.push(fs::read(path).unwrap());
            }
        });
        files
    }

    #[cfg(feature = "legacy-regression")]
    pub fn history(&self, run: &str, node: &str, invocation: u64) -> Vec<Value> {
        let record = self.record_for(run);
        let key = anchor_runtime_rig::graph::InvocationKey {
            run_id: run.to_owned(),
            graph_digest: record["graph_digest"].as_str().unwrap().to_owned(),
            node_id: node.to_owned(),
            invocation,
        };
        anchor_io_harness_runtime::node_port::trace_messages(
            &self.root.path().join("state/io-harness/store"),
            &key,
        )
        .unwrap()
    }
}

pub struct HttpHost {
    child: Child,
    url: String,
    log: PathBuf,
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}

impl Drop for Host {
    fn drop(&mut self) {
        if thread::panicking() {
            self.root.disable_cleanup(true);
            eprintln!(
                "failed Runtime fixture retained: {}",
                self.root.path().display()
            );
        }
    }
}

impl HttpHost {
    fn try_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), reqwest::Error> {
        self.try_request_with_headers(method, path, body, &[])
    }

    fn try_request_with_headers(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> Result<(u16, Value), reqwest::Error> {
        self.runtime.block_on(async {
            let mut request = self
                .client
                .request(method.parse().unwrap(), format!("{}{path}", self.url));
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await?;
            let status = response.status().as_u16();
            let bytes = response.bytes().await?;
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            };
            Ok((status, value))
        })
    }

    pub fn request(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        self.try_request(method, path, body).unwrap()
    }

    pub fn request_with_headers(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (u16, Value) {
        self.try_request_with_headers(method, path, None, headers)
            .unwrap()
    }

    pub fn events(&self, path: &str, last_event: Option<u64>) -> (u16, String) {
        self.runtime.block_on(async {
            let mut request = self.client.get(format!("{}{path}", self.url));
            if let Some(last_event) = last_event {
                request = request.header("Last-Event-ID", last_event);
            }
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            (status, response.text().await.unwrap())
        })
    }

    pub fn trigger(&self) -> String {
        let (status, accepted) =
            self.request("POST", "/trigger", Some(&json!({"graph":"fixture"})));
        assert_eq!(status, 202, "{accepted}");
        accepted["run"].as_str().unwrap().to_owned()
    }

    pub fn wait_status(&self, run: &str, expected: &str) -> Value {
        let mut detail = Value::Null;
        wait_until(expected, || {
            let (status, saved) = self.request("GET", &format!("/runs/{run}"), None);
            assert_eq!(status, 200, "{saved}");
            detail = saved;
            assert!(
                !matches!(
                    detail["state"]["status"].as_str(),
                    Some("failed" | "budget_stopped")
                ),
                "unexpected Run failure: {detail}"
            );
            detail["state"]["status"] == expected && detail["active"] == false
        });
        detail
    }
}

impl Drop for HttpHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn read_json(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[cfg(feature = "legacy-regression")]
pub fn evidence(name: &str, host: &Host, provider: &Provider, checks: Value) {
    evidence_run(name, host, provider, "fixture", checks);
}

#[cfg(feature = "legacy-regression")]
pub fn evidence_run(name: &str, host: &Host, provider: &Provider, run: &str, checks: Value) {
    write_evidence(name, host, provider, Some(host.record_for(run)), checks);
}

#[cfg(feature = "legacy-regression")]
pub fn evidence_rejection(
    name: &str,
    host: &Host,
    provider: &Provider,
    response: Value,
    checks: Value,
) {
    assert!(!host.root.path().join("state/runs/fixture.json").exists());
    write_evidence(
        name,
        host,
        provider,
        None,
        json!({"response":response,"assertions":checks}),
    );
}

#[cfg(feature = "legacy-regression")]
fn write_evidence(name: &str, host: &Host, provider: &Provider, run: Option<Value>, checks: Value) {
    let root = std::env::var_os("ANCHOR_TEST_EVIDENCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/runtime-contract")
                .join(std::process::id().to_string())
        });
    fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{name}.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "status":"passed", "scenario":name,
            "provider":"deterministic loopback fixture", "wire":"chat",
            "run":run, "provider_requests":provider.requests(), "checks":checks,
            "host_binary_sha256":file_digest(&host.binary),
            "test_binary_sha256":&*TEST_BINARY_DIGEST,
            "graph_sha256":file_digest(&host.root.path().join("bundle/graph.json")),
            "bundle_manifest_sha256":file_digest(&host.root.path().join("bundle/manifest.json")),
            "elapsed_ms":host.started.elapsed().as_millis(),
            "native_history":native_history(host),
            "artifacts":artifact_evidence(&host.root.path().join("state/artifacts")),
            "workspaces":workspace_evidence(&host.root.path().join("work")),
            "production_data_used":false, "dotenv_loaded":false
        }))
        .unwrap(),
    )
    .unwrap();
    println!("evidence: {}", path.display());
}

static TEST_BINARY_DIGEST: LazyLock<String> =
    LazyLock::new(|| file_digest(&std::env::current_exe().unwrap()));

fn file_digest(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = [0; 65_536];
    loop {
        let bytes = file.read(&mut buffer).unwrap();
        if bytes == 0 {
            break;
        }
        digest.update(&buffer[..bytes]);
    }
    format!("{:x}", digest.finalize())
}

#[cfg(feature = "legacy-regression")]
fn native_history(host: &Host) -> BTreeMap<String, Value> {
    let mut history = BTreeMap::new();
    let root = host.root.path().join("state/runs");
    visit_files(&root, &root, &mut |path, _| {
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let record = read_json(path);
            let run = record["run_id"].as_str().unwrap();
            for (node, count) in record["invocations"].as_object().unwrap() {
                for invocation in 1..=count.as_u64().unwrap() {
                    let messages = host.history(run, node, invocation);
                    if !messages.is_empty() {
                        history.insert(format!("{run}/{node}/{invocation}"), json!(messages));
                    }
                }
            }
        }
    });
    history
}

fn workspace_evidence(root: &Path) -> BTreeMap<String, Value> {
    let mut workspaces = BTreeMap::new();
    visit_files(root, root, &mut |path, relative| {
        let bytes = fs::read(path).unwrap();
        workspaces.insert(
            relative.to_owned(),
            json!({
                "sha256":format!("{:x}", Sha256::digest(&bytes)), "bytes":bytes.len(),
                "text":if bytes.len() <= 16_384 { std::str::from_utf8(&bytes).ok() } else { None }
            }),
        );
    });
    workspaces
}

pub(super) fn artifact_evidence(root: &Path) -> BTreeMap<String, Value> {
    let mut artifacts = BTreeMap::new();
    visit_files(root, root, &mut |path, relative| {
        if path.file_name().is_some_and(|name| name == "manifest.json") {
            artifacts.insert(relative.to_owned(), read_json(path));
        } else if relative.split('/').nth(1) == Some("files") {
            let bytes = fs::read(path).unwrap();
            artifacts.insert(relative.to_owned(), json!({
                "sha256":format!("{:x}", Sha256::digest(&bytes)), "bytes":bytes.len(),
                "text":if bytes.len() <= 16_384 { std::str::from_utf8(&bytes).ok() } else { None }
            }));
        }
    });
    artifacts
}

fn visit_files(root: &Path, directory: &Path, visitor: &mut impl FnMut(&Path, &str)) {
    if !directory.exists() {
        return;
    }
    let mut entries = fs::read_dir(directory)
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "git-view" && entry.file_name() != ".git" {
                visit_files(root, &path, visitor);
            }
        } else if entry.file_type().unwrap().is_file() {
            visitor(&path, path.strip_prefix(root).unwrap().to_str().unwrap());
        }
    }
}
