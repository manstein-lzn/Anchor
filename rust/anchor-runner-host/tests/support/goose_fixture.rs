use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    convert::Infallible,
    ffi::OsString,
    fs,
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, oneshot};

pub const MODEL: &str = "fixture-goose";
pub const GOOSE_SHA256: &str = "bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340";
pub const SUMMARY_REQUEST: &str =
    "Please summarize the conversation history provided in the system prompt.";

pub enum Reply {
    Tool(&'static str, Value),
    Text(&'static str),
    Summary(String),
    ContextLengthExceeded,
    Gated(Arc<Gate>, Box<Reply>),
}

pub struct Step {
    reply: Reply,
    feedback: Option<&'static str>,
    summary: bool,
    request_contains: Option<String>,
    usage: (u32, u32),
    preface: Option<String>,
}

impl Step {
    pub fn tool(suffix: &'static str, arguments: Value) -> Self {
        Self {
            reply: Reply::Tool(suffix, arguments),
            feedback: None,
            summary: false,
            request_contains: None,
            usage: (11, 7),
            preface: None,
        }
    }

    pub fn text(text: &'static str) -> Self {
        Self {
            reply: Reply::Text(text),
            feedback: None,
            summary: false,
            request_contains: None,
            usage: (11, 7),
            preface: None,
        }
    }

    pub fn summary(text: impl Into<String>) -> Self {
        Self {
            reply: Reply::Summary(text.into()),
            feedback: None,
            summary: true,
            request_contains: None,
            usage: (11, 7),
            preface: None,
        }
    }

    pub fn context_length_error() -> Self {
        Self {
            reply: Reply::ContextLengthExceeded,
            feedback: None,
            summary: false,
            request_contains: None,
            usage: (11, 7),
            preface: None,
        }
    }

    pub fn with_usage(mut self, input: u32, output: u32) -> Self {
        assert!(
            input
                .checked_add(output)
                .is_some_and(|total| total <= i32::MAX as u32)
        );
        self.usage = (input, output);
        self
    }

    pub fn expect_request_contains(mut self, text: impl Into<String>) -> Self {
        self.request_contains = Some(text.into());
        self
    }

    pub fn with_preface(mut self, text: impl Into<String>) -> Self {
        self.preface = Some(text.into());
        self
    }

    pub fn after(mut self, feedback: &'static str) -> Self {
        self.feedback = Some(feedback);
        self
    }

    pub fn gated(mut self, gate: &Arc<Gate>) -> Self {
        self.reply = Reply::Gated(gate.clone(), Box::new(self.reply));
        self
    }
}

pub fn command(script: &str) -> Step {
    Step::tool("anchor_run", json!({"command":["sh","-c",script]}))
}

pub fn complete(route: &str) -> Step {
    Step::tool(
        "final_result",
        json!({"summary":"fixture complete","route":route}),
    )
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
        wait_until("Goose Provider checkpoint", || {
            self.entered.load(Ordering::Acquire)
        });
    }

    pub fn open(&self) {
        self.release.add_permits(1);
    }
}

pub fn wait_until(description: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out: {description}");
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn tool_definition<'request>(
    request: &'request Value,
    suffix: &str,
) -> Result<&'request Value, String> {
    let tools = request["tools"]
        .as_array()
        .ok_or("missing actual tools schema")?;
    let matches = tools
        .iter()
        .filter(|tool| {
            tool["function"]["name"]
                .as_str()
                .is_some_and(|name| name == suffix || name.ends_with(&format!("__{suffix}")))
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "expected one actual {suffix} definition, got {}: {tools:?}",
            matches.len()
        ));
    }
    Ok(matches[0])
}

pub fn tool_feedback(request: &Value) -> Vec<Value> {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "tool")
        .cloned()
        .collect()
}

pub fn is_summary_request(request: &Value) -> bool {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|message| message["role"] == "user" && message["content"] == SUMMARY_REQUEST)
        && request
            .get("tools")
            .is_none_or(|tools| tools.is_null() || tools.as_array().is_some_and(Vec::is_empty))
}

fn observed_receipt(value: &Value) -> Option<String> {
    if let Some(receipt) = value.get("anchor_receipt").and_then(Value::as_str) {
        return Some(receipt.to_owned());
    }
    match value {
        Value::String(text) => serde_json::Deserializer::from_str(text)
            .into_iter::<Value>()
            .next()
            .and_then(Result::ok)
            .and_then(|value| observed_receipt(&value)),
        Value::Array(values) => values.iter().rev().find_map(observed_receipt),
        Value::Object(values) => values.values().rev().find_map(observed_receipt),
        _ => None,
    }
}

#[cfg(test)]
#[test]
fn receipt_header_survives_native_openai_mixed_content_flattening() {
    let flattened = json!(
        "{\"ok\":true,\"anchor_receipt\":\"media-receipt\"} before This tool result included an image after"
    );
    assert_eq!(observed_receipt(&flattened), Some("media-receipt".into()));
    assert_eq!(
        observed_receipt(&json!("plain text without a receipt")),
        None
    );
}

#[cfg(test)]
#[test]
fn summary_request_requires_the_native_prompt_and_no_tools() {
    let mut request = json!({"messages":[
        {"role":"system","content":"actual retained history"},
        {"role":"user","content":SUMMARY_REQUEST}
    ]});
    assert!(is_summary_request(&request));
    request["tools"] = json!([]);
    assert!(is_summary_request(&request));
    request["tools"] = json!([{"function":{"name":"anchor_run"}}]);
    assert!(!is_summary_request(&request));
    request["tools"] = Value::Null;
    request["messages"][1]["content"] = json!("ordinary followup");
    assert!(!is_summary_request(&request));
    request["messages"][1]["content"] = json!(SUMMARY_REQUEST);
    request["messages"][1]["role"] = json!("assistant");
    assert!(!is_summary_request(&request));
}

#[cfg(test)]
#[tokio::test]
async fn summary_fixture_checks_native_history_and_returns_scripted_usage() {
    let provider = Provider::new(
        "summary-fixture-selfcheck",
        vec![
            Step::summary("retained actual workspace marker")
                .expect_request_contains("actual workspace marker")
                .with_usage(23, 5),
        ],
    );
    let request = json!({"model":MODEL,"stream":true,"messages":[
        {"role":"system","content":"summarize actual workspace marker"},
        {"role":"user","content":SUMMARY_REQUEST}
    ]});
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{}/v1/chat/completions", provider.url))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chunks = response
        .text()
        .await
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| *line != "[DONE]")
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        chunks[1]["choices"][0]["delta"]["content"],
        "retained actual workspace marker"
    );
    assert_eq!(
        chunks.last().unwrap()["usage"],
        json!({"prompt_tokens":23,"completion_tokens":5,"total_tokens":28})
    );
    provider.assert_consumed();
    assert_eq!(provider.requests(), vec![request]);
}

#[cfg(test)]
#[tokio::test]
async fn context_length_fixture_error_is_scripted_not_an_unexpected_failure() {
    let provider = Provider::new(
        "context-length-fixture-selfcheck",
        vec![Step::context_length_error().expect_request_contains("existing task")],
    );
    let request = json!({"model":MODEL,"stream":true,"messages":[
        {"role":"user","content":"existing task"}
    ]});
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{}/v1/chat/completions", provider.url))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "context_length_exceeded"
    );
    provider.assert_consumed();
    assert_eq!(provider.requests(), vec![request]);
}

#[derive(Default)]
struct ProviderState {
    model: String,
    script: VecDeque<Step>,
    requests: Vec<Value>,
    exchanges: Vec<Value>,
    failures: Vec<String>,
    effects: Vec<Value>,
    root: PathBuf,
}

#[cfg(test)]
#[tokio::test]
async fn preface_fixture_keeps_text_separate_from_native_tool_arguments() {
    let arguments = json!({"command":["cat","evidence.txt"]});
    let provider = Provider::new(
        "preface-fixture-selfcheck",
        vec![
            Step::tool("anchor_run", arguments.clone())
                .with_preface("Inspecting the actual workspace."),
        ],
    );
    let request = json!({"model":MODEL,"stream":true,"messages":[{"role":"user","content":"inspect workspace"}],
        "tools":[{"type":"function","function":{"name":"anchor__anchor_run","parameters":{"type":"object"}}}]});
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{}/v1/chat/completions", provider.url))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chunks = response
        .text()
        .await
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| *line != "[DONE]")
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        chunks[1]["choices"][0]["delta"]["content"],
        "Inspecting the actual workspace."
    );
    let tool_arguments = chunks
        .iter()
        .filter_map(|chunk| {
            chunk["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
        })
        .collect::<String>();
    assert_eq!(
        serde_json::from_str::<Value>(&tool_arguments).unwrap(),
        arguments
    );
    provider.assert_consumed();
    assert_eq!(provider.requests(), vec![request]);
}

impl ProviderState {
    fn save(&self) {
        fs::write(
            self.root.join("provider.json"),
            serde_json::to_vec_pretty(&json!({
                "provider":"deterministic loopback OpenAI streaming fixture",
                "model":self.model,
                "real_model_requests":0,
                "requests":self.requests,
                "exchanges":self.exchanges,
                "failures":self.failures,
                "external_fake_effects":self.effects,
                "remaining_replies":self.script.len()
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn fail(&mut self, message: String) -> Response {
        self.failures.push(message.clone());
        self.save();
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{"message":message,"type":"fixture_failure"}})),
        )
            .into_response()
    }
}

async fn models(State(shared): State<Arc<Mutex<ProviderState>>>) -> Json<Value> {
    let mut state = shared.lock().unwrap();
    state
        .exchanges
        .push(json!({"method":"GET","path":"/v1/models"}));
    state.save();
    Json(
        json!({"object":"list","data":[{"id":state.model,"object":"model","created":1,"owned_by":"fixture"}]}),
    )
}

async fn completion(
    State(shared): State<Arc<Mutex<ProviderState>>>,
    Json(request): Json<Value>,
) -> Response {
    let (sequence, step) = {
        let mut state = shared.lock().unwrap();
        state.requests.push(request.clone());
        let sequence = state.requests.len();
        state.exchanges.push(json!({"method":"POST","path":"/v1/chat/completions","request":request,"tool_feedback":tool_feedback(&request)}));
        if request["model"] != state.model || request["stream"] != true {
            let model = state.model.clone();
            return state.fail(format!("expected {model} with stream=true: {request}"));
        }
        let Some(step) = state.script.pop_front() else {
            return state.fail("unscripted completion request".into());
        };
        if step.summary != is_summary_request(&request) {
            return state.fail(format!(
                "expected native summary request={}: {request}",
                step.summary
            ));
        }
        if let Some(expected) = &step.request_contains
            && !request["messages"].to_string().contains(expected)
        {
            return state.fail(format!(
                "actual request history must contain {expected:?}: {request}"
            ));
        }
        if let Some(expected) = step.feedback {
            let feedback = tool_feedback(&request);
            if !feedback.last().is_some_and(|message| {
                message["content"] != Value::Null
                    && message["content"].to_string().contains(expected)
            }) {
                return state.fail(format!(
                    "latest real tool feedback must contain {expected:?}: {feedback:?}"
                ));
            }
        }
        state.save();
        (sequence, step)
    };
    let mut reply = step.reply;
    while let Reply::Gated(gate, next) = reply {
        gate.entered.store(true, Ordering::Release);
        gate.release.acquire().await.unwrap().forget();
        reply = *next;
    }
    if matches!(reply, Reply::ContextLengthExceeded) {
        let response = json!({"error":{
            "message":"maximum context length exceeded",
            "type":"invalid_request_error","code":"context_length_exceeded"
        }});
        let mut state = shared.lock().unwrap();
        state.exchanges.push(json!({
            "sequence":sequence,"response_status":400,"response":response
        }));
        state.save();
        return (StatusCode::BAD_REQUEST, Json(response)).into_response();
    }
    let chunk = |delta: Value, finish: Value| {
        json!({
            "id":format!("goose-fixture-{sequence}"),"object":"chat.completion.chunk","created":1,"model":request["model"],
            "choices":[{"index":0,"delta":delta,"finish_reason":finish}]
        })
    };
    let mut chunks = vec![chunk(json!({"role":"assistant"}), Value::Null)];
    if let Some(text) = step.preface {
        chunks.push(chunk(json!({"content":text}), Value::Null));
    }
    let mut selected_tool = Value::Null;
    let finish = match reply {
        Reply::Tool(suffix, mut arguments) => {
            let definition = match tool_definition(&request, suffix) {
                Ok(definition) => definition,
                Err(message) => return shared.lock().unwrap().fail(message),
            };
            selected_tool = definition.clone();
            if suffix == "final_result"
                && definition["function"]["parameters"]["properties"]
                    .get("observed_receipt")
                    .is_some()
                && arguments.get("observed_receipt").is_none()
                && let Some(receipt) = tool_feedback(&request)
                    .iter()
                    .rev()
                    .find_map(observed_receipt)
            {
                arguments["observed_receipt"] = json!(receipt);
            }
            let name = definition["function"]["name"].as_str().unwrap();
            chunks.push(chunk(json!({"tool_calls":[{"index":0,"id":format!("call-{sequence}"),"type":"function","function":{"name":name,"arguments":""}}]}), Value::Null));
            let arguments = arguments.to_string();
            let middle = arguments
                .char_indices()
                .nth(arguments.chars().count() / 2)
                .map_or(0, |(offset, _)| offset);
            for fragment in [&arguments[..middle], &arguments[middle..]] {
                chunks.push(chunk(
                    json!({"tool_calls":[{"index":0,"function":{"arguments":fragment}}]}),
                    Value::Null,
                ));
            }
            "tool_calls"
        }
        Reply::Text(text) => {
            chunks.push(chunk(json!({"content":text}), Value::Null));
            "stop"
        }
        Reply::Summary(text) => {
            chunks.push(chunk(json!({"content":text}), Value::Null));
            "stop"
        }
        Reply::ContextLengthExceeded => unreachable!(),
        Reply::Gated(_, _) => unreachable!(),
    };
    chunks.push(chunk(json!({}), json!(finish)));
    let (input, output) = step.usage;
    let usage =
        json!({"prompt_tokens":input,"completion_tokens":output,"total_tokens":input + output});
    chunks.push(json!({"id":format!("goose-fixture-{sequence}"),"object":"chat.completion.chunk","created":1,"model":request["model"],"choices":[],"usage":usage}));
    {
        let mut state = shared.lock().unwrap();
        state.exchanges.push(json!({"sequence":sequence,"response_chunks":chunks,"usage":usage,"selected_actual_tool_schema":selected_tool}));
        state.save();
    }
    let mut frames = chunks
        .into_iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect::<Vec<_>>();
    frames.push("data: [DONE]\n\n".into());
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(futures_util::stream::iter(
            frames.into_iter().map(Ok::<_, Infallible>),
        )),
    )
        .into_response()
}

async fn external_effect(
    State(shared): State<Arc<Mutex<ProviderState>>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    let mut state = shared.lock().unwrap();
    state.effects.push(request.clone());
    let result = json!({"external_effect":"once","effect_count":state.effects.len()});
    state
        .exchanges
        .push(json!({"method":"POST","path":"/effects","request":request,"response":result}));
    state.save();
    Json(result)
}

async fn effect_state(State(shared): State<Arc<Mutex<ProviderState>>>) -> Json<Value> {
    let shared = shared.lock().unwrap();
    Json(json!({"effect_count":shared.effects.len(),"effects":shared.effects}))
}

pub struct Provider {
    pub url: String,
    pub root: PathBuf,
    pub model: String,
    state: Arc<Mutex<ProviderState>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<thread::JoinHandle<()>>,
}

impl Provider {
    pub fn new(scenario: &str, script: Vec<Step>) -> Self {
        Self::with_model(scenario, script, MODEL)
    }

    pub fn with_model(scenario: &str, script: Vec<Step>, model: &str) -> Self {
        let root = std::env::var_os("ANCHOR_TEST_EVIDENCE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/anchor-goose-acp-spike/evidence/integration"))
            .join(std::process::id().to_string())
            .join(scenario);
        fs::create_dir_all(&root).unwrap();
        let state = Arc::new(Mutex::new(ProviderState {
            model: model.into(),
            script: script.into(),
            root: root.clone(),
            ..Default::default()
        }));
        state.lock().unwrap().save();
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
                        .route("/v1/models", get(models))
                        .route("/effects", post(external_effect).get(effect_state))
                        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
                        .with_state(shared);
                    let server = tokio::spawn(async move {
                        axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                            .await
                            .unwrap();
                    });
                    tokio::select! { result = server => result.unwrap(), _ = stopped => {} }
                });
        });
        Self {
            url: format!("http://{address}"),
            root,
            model: model.into(),
            state,
            stop: Some(stop),
            task: Some(task),
        }
    }

    pub fn requests(&self) -> Vec<Value> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn append(&self, steps: Vec<Step>) {
        let mut state = self.state.lock().unwrap();
        state.script.extend(steps);
        state.save();
    }

    pub fn effects(&self) -> Vec<Value> {
        self.state.lock().unwrap().effects.clone()
    }

    pub fn assert_consumed(&self) {
        let state = self.state.lock().unwrap();
        assert!(
            state.failures.is_empty(),
            "{:?}; evidence {}",
            state.failures,
            self.root.display()
        );
        assert!(
            state.script.is_empty(),
            "{} unconsumed replies; evidence {}",
            state.script.len(),
            self.root.display()
        );
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

pub fn digest(path: &Path) -> String {
    let mut source = fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0; 65_536];
    loop {
        let length = source.read(&mut buffer).unwrap();
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    format!("{:x}", hash.finalize())
}

pub struct Host {
    pub base: crate::fixture::Host,
    binary: PathBuf,
    goose: PathBuf,
    started: Instant,
    allowed_commands: String,
    native: bool,
    runtime_default: bool,
    extra_environment: Vec<(OsString, OsString)>,
}

impl Host {
    pub fn new(graph: &Value) -> Self {
        let goose = PathBuf::from(std::env::var_os("ANCHOR_GOOSE_BINARY").expect("requires pinned Goose binary: set ANCHOR_GOOSE_BINARY to an absolute v1.53.0 musl binary path"));
        assert!(
            goose.is_absolute() && goose.is_file(),
            "missing absolute pinned Goose binary: {}",
            goose.display()
        );
        assert_eq!(
            digest(&goose),
            GOOSE_SHA256,
            "Goose binary differs from official pinned v1.53.0 x86_64 musl asset"
        );
        let base = crate::fixture::Host::new(graph);
        let version = Command::new(&goose)
            .arg("--version")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", base.root.path())
            .env("GOOSE_PATH_ROOT", base.root.path().join("version-probe"))
            .env("GOOSE_TELEMETRY_OFF", "1")
            .output()
            .unwrap();
        assert!(version.status.success(), "Goose version probe failed");
        assert_eq!(String::from_utf8(version.stdout).unwrap().trim(), "1.53.0");
        Self {
            binary: base.root.path().join("anchor-runner-host"),
            base,
            goose,
            started: Instant::now(),
            allowed_commands: "sh,cat,git,true".into(),
            native: false,
            runtime_default: false,
            extra_environment: Vec::new(),
        }
    }

    pub fn with_allowed_commands(mut self, commands: &str) -> Self {
        self.allowed_commands = commands.into();
        self
    }

    #[allow(dead_code)]
    pub fn with_runtime_binaries(mut self, binary: &Path, goose: &Path) -> Self {
        assert_eq!(digest(binary), digest(&self.binary));
        assert_eq!(digest(goose), GOOSE_SHA256);
        self.binary = binary.to_path_buf();
        self.goose = goose.to_path_buf();
        self
    }

    #[allow(dead_code)]
    pub fn with_extra_environment<Key, Value>(
        mut self,
        environment: impl IntoIterator<Item = (Key, Value)>,
    ) -> Self
    where
        Key: Into<OsString>,
        Value: Into<OsString>,
    {
        self.extra_environment.extend(
            environment
                .into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        self
    }

    pub fn native(mut self) -> Self {
        self.native = true;
        self
    }

    pub fn default_runtime(mut self) -> Self {
        self.native = true;
        self.runtime_default = true;
        self
    }

    fn process(&self, provider: &Provider) -> Command {
        let root = self.base.root.path();
        let mut process = Command::new(&self.binary);
        process
            .env_clear()
            .process_group(0)
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("TZ", "UTC")
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("xdg/config"))
            .env("XDG_DATA_HOME", root.join("xdg/data"))
            .env("XDG_STATE_HOME", root.join("xdg/state"))
            .env("XDG_CACHE_HOME", root.join("xdg/cache"))
            .env("ANCHOR_RUNNER_STATE_ROOT", root.join("state"))
            .env("ANCHOR_RUNNER_WORKSPACE_ROOT", root.join("work"))
            .env("ANCHOR_RUNNER_BUNDLE_ROOT", root.join("bundle"))
            .env("ANCHOR_RUNNER_CATALOG_ROOT", root)
            .env("ANCHOR_RUNNER_GRAPH_NAME", "fixture")
            .env("ANCHOR_RUNNER_SCHEDULES_PATH", root.join("schedules.json"))
            .env("ANCHOR_RUNNER_ALLOWED_COMMANDS", &self.allowed_commands)
            .env(
                "ANCHOR_RUNNER_AGENT_RUNTIME",
                if self.native {
                    "goose"
                } else {
                    "goose-acp-spike"
                },
            )
            .env("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK", "1")
            .env("ANCHOR_GOOSE_BINARY", &self.goose)
            .env("ANCHOR_GOOSE_BINARY_SHA256", digest(&self.goose))
            .env("ANCHOR_GOOSE_OPENAI_HOST", &provider.url)
            .env("ANCHOR_GOOSE_MODEL", &provider.model)
            .env("ANCHOR_MODEL_API_KEY", "fixture-only-not-a-secret")
            .env("ANCHOR_MODEL_URL", format!("{}/v1", provider.url))
            .envs(self.extra_environment.iter().cloned())
            .env("ANCHOR_MODEL_WIRE_API", "chat")
            .env("ANCHOR_MODEL_NAME", &provider.model)
            .env(
                "ANCHOR_MODEL_ALIASES",
                json!({"models.worker":provider.model}).to_string(),
            )
            .current_dir(root);
        if self.runtime_default && !cfg!(feature = "legacy-regression") {
            process.env_remove("ANCHOR_RUNNER_AGENT_RUNTIME");
        }
        process
    }

    pub fn run(&self, provider: &Provider) -> Value {
        self.run_process(provider, self.process(provider))
    }

    pub fn run_without_env(&self, provider: &Provider, variable: &str) -> Value {
        let mut process = self.process(provider);
        process.env_remove(variable);
        self.run_process(provider, process)
    }

    fn run_process(&self, provider: &Provider, mut process: Command) -> Value {
        let mut child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let request = serde_json::to_vec(&json!({"op":"start_bundle","version":1,"request_id":"goose-fixture","run_id":"fixture","input":{}})).unwrap();
        let mut input = child.stdin.take().unwrap();
        input
            .write_all(&(request.len() as u32).to_be_bytes())
            .unwrap();
        input.write_all(&request).unwrap();
        drop(input);
        let read = |mut pipe: Box<dyn Read + Send>| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                pipe.read_to_end(&mut bytes).unwrap();
                bytes
            })
        };
        let output = read(Box::new(child.stdout.take().unwrap()));
        let errors = read(Box::new(child.stderr.take().unwrap()));
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                kill_group(&mut child);
                break None;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let output = output.join().unwrap();
        let errors = errors.join().unwrap();
        fs::write(provider.root.join("host-stdout.bin"), &output).unwrap();
        fs::write(provider.root.join("host-stderr.txt"), &errors).unwrap();
        assert!(
            status.is_some_and(|status| status.success()),
            "Host failed or timed out: {}; evidence {}",
            String::from_utf8_lossy(&errors),
            provider.root.display()
        );
        assert!(output.len() >= 4, "missing Host framed response");
        let length = u32::from_be_bytes(output[..4].try_into().unwrap()) as usize;
        assert_eq!(output.len(), length + 4, "unexpected Host frames");
        let response = serde_json::from_slice(&output[4..]).unwrap();
        fs::write(
            provider.root.join("host-response.json"),
            serde_json::to_vec_pretty(&response).unwrap(),
        )
        .unwrap();
        response
    }

    pub fn record(&self, run: &str) -> Value {
        self.base.record_for(run)
    }

    pub fn serve(&self, provider: &Provider) -> HttpHost {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let log = provider.root.join("host-http.log");
        let child = self
            .process(provider)
            .arg("serve")
            .env("ANCHOR_RUNNER_LISTEN", address.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let mut server = HttpHost {
            child,
            killed: false,
            url: format!("http://{address}"),
            log,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        wait_until("Goose HTTP Host startup", || {
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

    pub fn evidence(&self, provider: &Provider, run: &str, checks: Value) {
        provider.assert_consumed();
        let root = self.base.root.path();
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let case_source = checks
            .get("case_source")
            .and_then(Value::as_str)
            .unwrap_or("tests/goose_acp.rs");
        let metadata = root.join("state/run-metadata").join(format!("{run}.json"));
        let graph = if metadata.is_file() {
            let metadata = crate::fixture::read_json(metadata);
            crate::fixture::read_json(
                Path::new(metadata["bundle_source"].as_str().unwrap()).join("graph.json"),
            )
        } else {
            crate::fixture::read_json(root.join("bundle/graph.json"))
        };
        fs::write(provider.root.join("evidence.json"), serde_json::to_vec_pretty(&json!({
            "status":"passed", "runtime":"real Goose native loop over ACP", "real_model_requests":0,
            "goose_version":"1.53.0", "goose_binary":self.goose, "goose_binary_sha256":digest(&self.goose),
            "goose_release_asset_sha256":"4124f3b56dcebf1f396ddddaa66d68cf710318dcf947d8c81de6eb5866af11d7",
            "host_binary_sha256":digest(&self.binary),
            "test_binary_sha256":digest(&std::env::current_exe().unwrap()),
            "test_source_sha256":digest(&source_root.join(case_source)),
            "fixture_source_sha256":digest(&source_root.join("tests/support/goose_fixture.rs")),
            "graph":graph,
            "run":self.record(run), "provider_requests":provider.requests(), "checks":checks,
            "native_goose_conversation":self.native_conversation(run, "worker", 1),
            "external_fake_effects":provider.effects(),
            "workspace_files":file_inventory(&root.join("work")),
            "state_files":file_inventory(&root.join("state")),
            "elapsed_ms":self.started.elapsed().as_millis(),
            "production_data_used":false,"dotenv_loaded":false
        })).unwrap()).unwrap();
        println!(
            "evidence: {}",
            provider.root.join("evidence.json").display()
        );
    }

    pub fn native_record(&self, run: &str, node: &str, invocation: u64) -> (Value, Value, PathBuf) {
        let (fact, process) = self.invocation_paths(run, node, invocation);
        (
            crate::fixture::read_json(fact.with_extension("json")),
            crate::fixture::read_json(fact.with_extension("evidence.json")),
            process,
        )
    }

    fn invocation_paths(&self, run: &str, node: &str, invocation: u64) -> (PathBuf, PathBuf) {
        let saved = self.record(run);
        let key = anchor_runtime_rig::graph::InvocationKey {
            run_id: run.into(),
            graph_digest: saved["graph_digest"].as_str().unwrap().into(),
            node_id: node.into(),
            invocation,
        };
        let stem = format!("{:x}", Sha256::digest(key.durable_key().as_bytes()));
        let facts = self.base.root.path().join(if self.native {
            "state/goose-acp"
        } else {
            "state/goose-acp-spike"
        });
        let fact = facts.join(&stem);
        let process_root = self.base.root.path().join("work/.goose-process");
        let saved_fact = crate::fixture::read_json(fact.with_extension("json"));
        let process = match saved_fact.get("conversation_scope") {
            None | Some(Value::Null) => process_root.join(stem),
            Some(Value::String(scope)) => {
                assert!(
                    scope.strip_prefix("gc1-").is_some_and(|digest| {
                        digest.len() == 64
                            && digest.bytes().all(|character| {
                                character.is_ascii_digit() || (b'a'..=b'f').contains(&character)
                            })
                    }),
                    "unsafe Goose conversation scope"
                );
                process_root
                    .join("conversations")
                    .join(scope)
                    .join("process")
            }
            Some(_) => panic!("invalid Goose conversation scope"),
        };
        (fact, process)
    }

    pub fn native_fact(&self, run: &str, node: &str, invocation: u64) -> Value {
        let (fact, _) = self.invocation_paths(run, node, invocation);
        crate::fixture::read_json(fact.with_extension("json"))
    }

    pub fn native_conversation(&self, run: &str, node: &str, invocation: u64) -> Value {
        let (fact, process) = self.invocation_paths(run, node, invocation);
        let fact = crate::fixture::read_json(fact.with_extension("json"));
        let session = fact["session_id"]
            .as_str()
            .expect("missing real Goose session identity");
        self.session_conversation(&process, session)
    }

    pub fn session_conversation(&self, process: &Path, session: &str) -> Value {
        assert!(
            !session.is_empty()
                && session.len() <= 128
                && session.bytes().all(
                    |character| character.is_ascii_alphanumeric() || b"_-".contains(&character)
                ),
            "unsafe Goose session id"
        );
        let database = process.join("data/sessions/sessions.db");
        assert!(
            database.is_file(),
            "missing real Goose native session database"
        );
        let query = format!(
            "SELECT json_group_array(json_object('id',id,'role',role,'content_json',content_json,'metadata_json',metadata_json,'created_timestamp',created_timestamp)) FROM (SELECT id,role,content_json,metadata_json,created_timestamp FROM messages WHERE session_id='{session}' ORDER BY id);"
        );
        let output = Command::new("/usr/bin/sqlite3")
            .args(["-readonly", "-batch", "-noheader"])
            .arg(&database)
            .arg(query)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", process.join("home"))
            .output()
            .expect("requires installed sqlite3 CLI to inspect native Goose history");
        assert!(
            output.status.success(),
            "native SQLite history read failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut rows = if output.stdout.is_empty() {
            json!([])
        } else {
            serde_json::from_slice::<Value>(&output.stdout).unwrap()
        };
        for row in rows.as_array_mut().unwrap() {
            row["content"] = serde_json::from_str(row["content_json"].as_str().unwrap()).unwrap();
            row.as_object_mut().unwrap().remove("content_json");
            if let Some(metadata) = row["metadata_json"].as_str() {
                row["metadata"] = serde_json::from_str(metadata).unwrap();
            }
            row.as_object_mut().unwrap().remove("metadata_json");
        }
        rows
    }
}

pub fn file_inventory(root: &Path) -> Vec<Value> {
    fn visit(root: &Path, path: &Path, files: &mut Vec<Value>) {
        if !path.exists() {
            return;
        }
        let mut entries = fs::read_dir(path)
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                visit(root, &path, files);
            } else if kind.is_file() {
                let bytes = fs::read(&path).unwrap();
                let mut item = json!({"path":path.strip_prefix(root).unwrap(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"bytes":bytes.len()});
                if bytes.len() <= 131_072
                    && let Ok(text) = String::from_utf8(bytes)
                {
                    item["text"] = json!(text);
                }
                files.push(item);
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files
}

fn kill_group(child: &mut Child) {
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

pub struct HttpHost {
    child: Child,
    killed: bool,
    url: String,
    log: PathBuf,
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}

impl HttpHost {
    pub fn kill(&mut self) {
        kill_group(&mut self.child);
        self.killed = true;
    }
    fn try_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), reqwest::Error> {
        self.runtime.block_on(async {
            let mut request = self
                .client
                .request(method.parse().unwrap(), format!("{}{path}", self.url));
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await?;
            let status = response.status().as_u16();
            let bytes = response.bytes().await?;
            Ok((
                status,
                if bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap()
                },
            ))
        })
    }

    pub fn request(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        self.try_request(method, path, body).unwrap()
    }

    #[allow(dead_code)]
    pub fn events(&self, path: &str, after: u64) -> String {
        self.runtime.block_on(async {
            self.client
                .get(format!("{}{path}", self.url))
                .header("Last-Event-ID", after.to_string())
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .text()
                .await
                .unwrap()
        })
    }

    pub fn trigger(&self) -> String {
        let (status, accepted) =
            self.request("POST", "/trigger", Some(&json!({"graph":"fixture"})));
        assert_eq!(status, 202, "{accepted}");
        accepted["run"].as_str().unwrap().into()
    }

    pub fn wait_status(&self, run: &str, expected: &str) -> Value {
        let mut detail = Value::Null;
        wait_until(expected, || {
            let (status, saved) = self.request("GET", &format!("/runs/{run}"), None);
            assert_eq!(status, 200, "{saved}");
            detail = saved;
            assert!(
                detail["state"]["status"] != "budget_stopped"
                    && (expected == "failed" || detail["state"]["status"] != "failed"),
                "unexpected Run failure: {detail}"
            );
            detail["state"]["status"] == expected && detail["active"] == false
        });
        detail
    }
}

impl Drop for HttpHost {
    fn drop(&mut self) {
        if !self.killed {
            kill_group(&mut self.child);
        }
    }
}
