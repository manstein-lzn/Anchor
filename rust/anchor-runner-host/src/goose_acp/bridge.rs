use anchor_runtime::{Cancellation, ToolPort};
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::{SessionManager, local::LocalSessionManager},
    },
};
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use tokio::sync::{Mutex, Notify, oneshot};
use tokio_util::sync::CancellationToken;

use super::completion::{NativeCompletion, ToolBatch};

/// Everything one bridge invocation needs that is not the tool port itself.
struct BridgeSetup {
    routes: Vec<String>,
    plugin_instructions: String,
    cancellation: Cancellation,
    upstream: Option<super::configuration::ModelUpstream>,
    fixture: bool,
    token: String,
    observation_path: Option<PathBuf>,
    pilot: Option<Arc<super::pilot::PilotPort>>,
    outputs: Option<DeclaredOutputs>,
}

pub(super) struct NodeBridgeContext {
    pub(super) routes: Vec<String>,
    pub(super) plugin_instructions: String,
    pub(super) observation_path: Option<PathBuf>,
    pub(super) outputs: DeclaredOutputs,
}

/// What a node's definition says it writes, and where: `final_result` is
/// refused while one of them is missing, so the model can still write it.
pub(super) struct DeclaredOutputs {
    pub(super) workspace: PathBuf,
    pub(super) writes: Vec<String>,
}

impl DeclaredOutputs {
    fn validate(&self, completion: &Value) -> Result<(), String> {
        let selection = anchor_runtime::artifact::ArtifactSelection::from_output(completion)?;
        crate::artifacts::validate_selection(&self.workspace, &selection, &self.writes)
            .map_err(|error| format!("final_result refused: {error}. Correct the files or delivery list, then call final_result again. No completion was recorded."))
    }
}

/// The model proxy carries whole prompts, including embedded images, so its body
/// limit is far above the MCP tool-call limit.
const MODEL_PROXY_BODY_LIMIT: usize = 32 * 1024 * 1024;

pub(super) struct Bridge {
    pub(super) url: String,
    pub(super) token: String,
    pub(super) state: Arc<BridgeState>,
    /// Loopback address the HTTP listener actually bound.
    address: std::net::SocketAddr,
    /// UNIX socket this bridge is additionally exposed on, if any.
    socket: std::sync::Mutex<Option<PathBuf>>,
    stop: CancellationToken,
    task: Mutex<Option<tokio::task::JoinHandle<std::io::Result<()>>>>,
    sessions: Arc<LocalSessionManager>,
    handler_closed: Mutex<Option<oneshot::Receiver<()>>>,
}

pub(super) struct BridgeState {
    tools: Arc<dyn ToolPort>,
    plugin_instructions: String,
    pilot: Option<Arc<super::pilot::PilotPort>>,
    node_completion: bool,
    routes: Vec<String>,
    cancellation: Cancellation,
    upstream: Option<super::configuration::ModelUpstream>,
    /// The scripted test transport. It owns the completion-contract branches;
    /// proxying to a real provider does not change them.
    fixture: bool,
    client: reqwest::Client,
    token: String,
    pub(super) provider_calls: AtomicU64,
    call_lock: Mutex<()>,
    pub(super) calls: Mutex<Vec<Value>>,
    pub(super) dropped_calls: AtomicU64,
    observation_path: Option<PathBuf>,
    outputs: Option<DeclaredOutputs>,
    pub(super) completion: Mutex<Option<Value>>,
    pub(super) after_completion: Mutex<bool>,
    response_tools: Mutex<Vec<String>>,
    native_completion: Mutex<NativeCompletion>,
    accepting: AtomicBool,
    active_calls: AtomicUsize,
    idle: Notify,
}

struct ActiveCall(Arc<BridgeState>);

impl Drop for ActiveCall {
    fn drop(&mut self) {
        if self.0.active_calls.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

#[derive(Clone)]
struct McpBridge {
    state: Arc<BridgeState>,
    _release: Arc<HandlerRelease>,
}

/// rmcp owns session workers independently of Axum's HTTP connection tasks.
/// Signal when both have released the handler, before the owner closes tools.
struct HandlerRelease(Option<oneshot::Sender<()>>);

impl Drop for HandlerRelease {
    fn drop(&mut self) {
        if let Some(closed) = self.0.take() {
            let _ = closed.send(());
        }
    }
}

impl ServerHandler for McpBridge {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(self.state.plugin_instructions.clone())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools = Vec::new();
        for definition in self.state.tools.definitions() {
            tools.push(Tool::new(
                definition.name,
                definition.description,
                Arc::new(
                    serde_json::from_value(definition.parameters).map_err(|error| {
                        ErrorData::internal_error(
                            format!("invalid Anchor tool schema: {error}"),
                            None,
                        )
                    })?,
                ),
            ));
        }
        if self.state.node_completion {
            tools.push(Tool::new(
            "final_result",
            "Complete this Anchor node after inspecting all authorized tool results. Call alone with a nonempty summary and a legal route.",
            Arc::new(serde_json::from_value(self.state.completion_schema()).unwrap()),
        ));
        }
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let started_at_ms = crate::fact_recorder::now_ms();
        self.state.active_calls.fetch_add(1, Ordering::SeqCst);
        let _active = ActiveCall(self.state.clone());
        if !self.state.accepting.load(Ordering::SeqCst) {
            return Err(ErrorData::internal_error(
                "Anchor invocation is closing",
                None,
            ));
        }
        let _guard = self.state.call_lock.lock().await;
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let resolved = crate::tool_disclosure::resolve_call(&request.name, arguments.clone());
        let (tool_name, tool_arguments) = match &resolved {
            Ok((name, arguments)) => (name.as_str(), arguments),
            Err(_) => (request.name.as_ref(), &arguments),
        };
        let epoch = self.state.provider_calls.load(Ordering::SeqCst);
        let fact = self.state.load_fact()?;
        let identity = fact
            .as_ref()
            .and_then(|fact| native_tool_identity(fact, request.meta.as_ref(), &context.meta));
        let tool_call_id = identity.as_ref().map(|identity| identity.tool_call.clone());
        let batch = ToolBatch::from_meta(request.meta.as_ref(), &context.meta);
        let native_node = self.state.node_completion && !self.state.fixture;
        let mut media = None;
        let result = if self.state.cancellation.load(Ordering::Relaxed) {
            Err("Anchor invocation was cancelled".to_owned())
        } else if native_node && (identity.is_none() || batch.is_none()) {
            Err("Anchor requires trusted Goose session, tool-call and batch metadata".to_owned())
        } else if self.state.fixture && self.state.completion.lock().await.is_some() {
            *self.state.after_completion.lock().await = true;
            Err("tools after node completion are refused".to_owned())
        } else if let Err(error) = &resolved {
            Err(error.to_string())
        } else if self.state.node_completion && request.name == "final_result" {
            let mixed_batch = if self.state.fixture {
                let response_tools = self.state.response_tools.lock().await;
                response_tools.len() != 1 || !response_tools[0].ends_with("__final_result")
            } else {
                false
            };
            let same_round = self.state.fixture
                && self.state.calls.lock().await.iter().any(|call| {
                    call["provider_request"] == epoch && call["tool"] != "final_result"
                });
            if same_round || mixed_batch {
                Err("final_result must be called alone after observing business results".to_owned())
            } else {
                let mut completion = arguments.clone();
                if !self.state.fixture {
                    completion
                        .as_object_mut()
                        .unwrap()
                        .remove("observed_receipt");
                }
                // Before the completion is frozen: a refusal here leaves the
                // model free to write the output and call final_result again.
                let completion =
                    validate_completion(&completion, &self.state.routes).and_then(|completion| {
                        match &self.state.outputs {
                            Some(outputs) => outputs.validate(&completion).map(|()| completion),
                            None => Ok(completion),
                        }
                    });
                let completion = match completion {
                    Ok(completion) if native_node => {
                        self.state.native_completion.lock().await.submit(
                            batch.as_ref().unwrap(),
                            completion,
                            arguments.get("observed_receipt"),
                            fact.as_ref()
                                .is_some_and(|fact| fact.tool_observation.is_some()),
                        )
                    }
                    completion => completion,
                };
                match completion {
                    Ok(completion) => {
                        *self.state.completion.lock().await = Some(completion.clone());
                        Ok(completion)
                    }
                    Err(error) => Err(error),
                }
            }
        } else if native_node
            && let Err(error) = self.state.begin_business(batch.as_ref().unwrap()).await
        {
            Err(error)
        } else {
            let mut observation = json!({"tool":tool_name,"arguments":tool_arguments,"result":null,"started_at":chrono::Utc::now().to_rfc3339()});
            if let Some(identity) = &identity {
                observation["native_tool_call"] = json!({
                    "session": identity.session,
                    "tool_call": identity.tool_call
                });
            }
            self.state.store_observation(&observation)?;
            let tool = async {
                match &self.state.pilot {
                    Some(pilot) => {
                        pilot
                            .call_mcp(
                                tool_name,
                                tool_arguments.clone(),
                                Some(context.peer.clone()),
                            )
                            .await
                    }
                    None => {
                        self.state
                            .tools
                            .call(tool_name, tool_arguments.clone())
                            .await
                    }
                }
            };
            let tool = crate::goose_tool_context::scope(identity, tool);
            tokio::pin!(tool);
            let result = tokio::select! {
                result = &mut tool => result,
                _ = context.ct.cancelled() => {
                    self.state.cancellation.store(true, Ordering::SeqCst);
                    tool.await
                }
            };
            let result = result
                .map_err(|error| error.to_string())
                .and_then(super::media::encode)
                .map(|output| {
                    media = output.media;
                    output.observation
                });
            let result = if self.state.node_completion && !self.state.fixture {
                let mut bytes = [0u8; 32];
                std::fs::File::open("/dev/urandom")
                    .and_then(|mut file| file.read_exact(&mut bytes))
                    .map_err(|_| ErrorData::internal_error("tool receipt unavailable", None))?;
                let receipt = bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                self.state
                    .native_completion
                    .lock()
                    .await
                    .record_receipt(receipt.clone());
                match result {
                    Ok(output) => Ok(json!({"ok":true,"output":output,"anchor_receipt":receipt})),
                    Err(error) => {
                        Err(json!({"ok":false,"error":error,"anchor_receipt":receipt}).to_string())
                    }
                }
            } else {
                result
            }
            .inspect(|output| observation["result"] = json!({"ok":true,"output":output}))
            .inspect_err(|error| observation["result"] = json!({"ok":false,"error":error}));
            self.state.store_observation(&observation)?;
            result
        };
        let evidence = match &result {
            Ok(output) => json!({"ok":true,"output":output}),
            Err(error) => json!({"ok":false,"error":error}),
        };
        self.state
            .record_call(json!({
                "provider_request":epoch,"tool":tool_name,"arguments":tool_arguments,"result":evidence,
                "tool_call_id":tool_call_id,
                "started_at_ms":started_at_ms,"ended_at_ms":crate::fact_recorder::now_ms()
            }))
            .await?;
        Ok(match result {
            Ok(output) => super::media::success(output, media),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
        })
    }
}

fn native_tool_identity(
    fact: &super::Fact,
    meta: Option<&rmcp::model::Meta>,
    context: &rmcp::model::Meta,
) -> Option<crate::goose_tool_context::GooseToolIdentity> {
    let has_identity = |meta: &rmcp::model::Meta| {
        meta.0.keys().any(|key| {
            key.eq_ignore_ascii_case("agent-session-id")
                || key.eq_ignore_ascii_case("agent-tool-call-request-id")
        })
    };
    let meta = match meta.filter(|meta| has_identity(meta)) {
        Some(meta) => {
            if has_identity(context) && meta.0 != context.0 {
                return None;
            }
            meta
        }
        None => context,
    };
    let value = |name: &str, limit: usize| {
        let mut values = meta
            .0
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name));
        let value = values.next()?.1.as_str()?;
        if values.next().is_some()
            || value.is_empty()
            || value.len() > limit
            || value.chars().any(char::is_control)
        {
            return None;
        }
        Some(value.to_owned())
    };
    let session = value("agent-session-id", 128)?;
    if fact.session_id.as_deref() != Some(session.as_str()) {
        return None;
    }
    Some(crate::goose_tool_context::GooseToolIdentity {
        key: fact.key.clone(),
        session,
        tool_call: value("agent-tool-call-request-id", 512)?,
    })
}

impl BridgeState {
    async fn begin_business(&self, batch: &ToolBatch) -> Result<(), String> {
        self.native_completion.lock().await.begin_business(batch)
    }

    fn load_fact(&self) -> Result<Option<super::Fact>, ErrorData> {
        self.observation_path
            .as_ref()
            .map(|path| {
                super::read_fact(path).ok().flatten().ok_or_else(|| {
                    ErrorData::internal_error("Anchor invocation fact unavailable", None)
                })
            })
            .transpose()
    }

    async fn record_call(&self, call: Value) -> Result<(), ErrorData> {
        let mut calls = self.calls.lock().await;
        if calls.len() >= 256 {
            if self.fixture {
                self.cancellation.store(true, Ordering::SeqCst);
                return Err(ErrorData::internal_error(
                    "Goose spike tool call limit exceeded",
                    None,
                ));
            }
            calls.remove(0);
            self.dropped_calls.fetch_add(1, Ordering::SeqCst);
        }
        calls.push(call);
        Ok(())
    }

    fn store_observation(&self, observation: &Value) -> Result<(), ErrorData> {
        let Some(path) = &self.observation_path else {
            return Ok(());
        };
        let mut fact = self
            .load_fact()?
            .ok_or_else(|| ErrorData::internal_error("Anchor invocation fact unavailable", None))?;
        fact.tool_observation = Some(observation.clone());
        super::store_fact(path, &fact).map_err(|_| {
            ErrorData::internal_error("Anchor tool observation could not be saved", None)
        })
    }
}

impl BridgeState {
    fn completion_schema(&self) -> Value {
        let mut schema = completion_schema(&self.routes);
        if !self.fixture {
            schema["properties"]["observed_receipt"] = json!({
                "type":"string", "description":"Copy an anchor_receipt from any result in the most recent authorized non-completion tool batch, including discovery and read-only tools. Required after any such tool; call final_result alone in a later batch. Do not guess a receipt or repeat business operations merely to obtain one."
            });
        }
        schema
    }
}

pub(super) fn completion_schema(routes: &[String]) -> Value {
    let route = if routes.is_empty() {
        json!({"type":"null"})
    } else {
        json!({"type":"string","enum":routes})
    };
    let required = if routes.len() > 1 {
        json!(["summary", "files", "route"])
    } else {
        json!(["summary", "files"])
    };
    json!({"type":"object","properties":{"summary":{"type":"string","minLength":1},"route":route,
        "files":{"type":"array","items":{"type":"string"},"description":"Complete list of workspace-relative files or directories to deliver. Directories export recursively. Use [] for text only; include every declared writes output. The remaining workspace stays private and is not archived."}},
        "required":required,"additionalProperties":false})
}

pub(super) fn validate_completion(value: &Value, routes: &[String]) -> Result<Value, String> {
    let object = value.as_object().ok_or("completion must be an object")?;
    if object
        .keys()
        .any(|key| key != "summary" && key != "route" && key != "files")
    {
        return Err("unknown completion field".into());
    }
    let summary = value["summary"]
        .as_str()
        .filter(|summary| !summary.trim().is_empty())
        .ok_or("completion requires a nonempty summary")?;
    let route = match value.get("route") {
        None | Some(Value::Null) if routes.len() < 2 => routes.first().cloned(),
        Some(Value::String(route)) if routes.contains(route) => Some(route.clone()),
        _ => return Err("completion route is not an allowed outgoing route".into()),
    };
    let files = anchor_runtime::artifact::ArtifactSelection::from_output(value)?.paths;
    Ok(json!({"summary":summary,"route":route,"files":files}))
}

async fn authorize(
    State(state): State<Arc<BridgeState>>,
    request: Request,
    next: Next,
) -> Response {
    if request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some(state.token.as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

async fn completion(
    State(state): State<Arc<BridgeState>>,
    Json(mut body): Json<Value>,
) -> Response {
    if state.cancellation.load(Ordering::Relaxed) {
        return StatusCode::CONFLICT.into_response();
    }
    state.provider_calls.fetch_add(1, Ordering::SeqCst);
    body["parallel_tool_calls"] = json!(false);
    let Some(upstream) = state.upstream.as_ref() else {
        return (StatusCode::NOT_FOUND, "model transport is not proxied").into_response();
    };
    let response = state
        .client
        .post(upstream.url.clone())
        .bearer_auth(&upstream.api_key)
        .json(&body)
        .send()
        .await;
    match response {
        Ok(mut response) => {
            let status = response.status();
            if !state.fixture {
                // Stream a real provider response straight through: buffering it
                // would delay every token and cap the response at the fixture bound.
                let mut builder = Response::builder().status(status);
                if let Some(content_type) = response.headers().get("content-type") {
                    builder = builder.header("content-type", content_type);
                }
                return builder
                    .body(Body::from_stream(response.bytes_stream()))
                    .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
            }
            let content_type = response.headers().get("content-type").cloned();
            let mut bytes = Vec::new();
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) if bytes.len() + chunk.len() <= 16 * 1024 * 1024 => {
                        bytes.extend_from_slice(&chunk);
                    }
                    Ok(None) => break,
                    _ => {
                        return (
                            StatusCode::BAD_GATEWAY,
                            "local fixture response exceeded bounds or failed",
                        )
                            .into_response();
                    }
                }
            }
            if status.is_success() {
                match response_tool_names(&bytes) {
                    Ok(names) => *state.response_tools.lock().await = names,
                    Err(_) => {
                        return (
                            StatusCode::BAD_GATEWAY,
                            "local fixture tool response was malformed",
                        )
                            .into_response();
                    }
                }
            }
            let mut output = Response::new(Body::from(bytes));
            *output.status_mut() = status;
            if let Some(content_type) = content_type {
                output.headers_mut().insert("content-type", content_type);
            }
            output
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "local fixture provider failed").into_response(),
    }
}

/// Forward UNIX-socket connections to the bridge's loopback listener.
async fn forward_unix(
    listener: tokio::net::UnixListener,
    address: std::net::SocketAddr,
    stop: CancellationToken,
) {
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = stop.cancelled() => return,
        };
        let Ok((mut inbound, _)) = accepted else {
            return;
        };
        tokio::spawn(async move {
            let Ok(mut outbound) = tokio::net::TcpStream::connect(address).await else {
                return;
            };
            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
        });
    }
}

fn response_tool_names(bytes: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let mut names = std::collections::BTreeMap::<u64, String>::new();
    if let Ok(body) = serde_json::from_str::<Value>(text) {
        if let Some(calls) = body["choices"][0]["message"]["tool_calls"].as_array() {
            for (index, call) in calls.iter().enumerate() {
                names.insert(
                    index as u64,
                    call["function"]["name"]
                        .as_str()
                        .ok_or("missing tool name")?
                        .to_owned(),
                );
            }
        }
    } else {
        for line in text.lines() {
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data == "[DONE]" {
                continue;
            }
            let body: Value = serde_json::from_str(data).map_err(|error| error.to_string())?;
            if let Some(calls) = body["choices"][0]["delta"]["tool_calls"].as_array() {
                for call in calls {
                    let index = call["index"]
                        .as_u64()
                        .ok_or("missing streaming tool index")?;
                    let name = names.entry(index).or_default();
                    if let Some(fragment) = call["function"]["name"].as_str() {
                        name.push_str(fragment);
                    }
                }
            }
        }
    }
    if names.len() > 64 || names.values().any(|name| name.is_empty()) {
        return Err("invalid fixture tool names".into());
    }
    Ok(names.into_values().collect())
}

impl Bridge {
    pub(super) async fn start(
        tools: Arc<dyn ToolPort>,
        context: NodeBridgeContext,
        cancellation: Cancellation,
        upstream: Option<super::configuration::ModelUpstream>,
        fixture: bool,
        token: String,
    ) -> Result<Self, String> {
        Self::start_inner(
            tools,
            BridgeSetup {
                routes: context.routes,
                plugin_instructions: context.plugin_instructions,
                cancellation,
                upstream,
                fixture,
                token,
                observation_path: context.observation_path,
                pilot: None,
                outputs: Some(context.outputs),
            },
        )
        .await
    }

    pub(super) async fn start_pilot(
        tools: Arc<super::pilot::PilotPort>,
        cancellation: Cancellation,
        upstream: Option<super::configuration::ModelUpstream>,
        fixture: bool,
        token: String,
    ) -> Result<Self, String> {
        Self::start_inner(
            tools.clone(),
            BridgeSetup {
                routes: Vec::new(),
                plugin_instructions: tools.instructions(),
                cancellation,
                upstream,
                fixture,
                token,
                observation_path: None,
                pilot: Some(tools),
                outputs: None,
            },
        )
        .await
    }

    async fn start_inner(tools: Arc<dyn ToolPort>, setup: BridgeSetup) -> Result<Self, String> {
        let BridgeSetup {
            routes,
            plugin_instructions,
            cancellation,
            upstream,
            fixture,
            token,
            observation_path,
            pilot,
            outputs,
        } = setup;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let state = Arc::new(BridgeState {
            tools,
            plugin_instructions,
            node_completion: pilot.is_none(),
            pilot,
            routes,
            cancellation,
            upstream,
            fixture,
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(5))
                // A total timeout would cut a long streamed model response short;
                // a stalled connection still fails after the read timeout.
                .read_timeout(std::time::Duration::from_secs(60))
                .build()
                .map_err(|error| error.to_string())?,
            token: format!("Bearer {token}"),
            provider_calls: AtomicU64::new(0),
            call_lock: Mutex::new(()),
            calls: Mutex::new(Vec::new()),
            dropped_calls: AtomicU64::new(0),
            observation_path,
            outputs,
            completion: Mutex::new(None),
            after_completion: Mutex::new(false),
            response_tools: Mutex::new(Vec::new()),
            native_completion: Mutex::new(NativeCompletion::default()),
            accepting: AtomicBool::new(true),
            active_calls: AtomicUsize::new(0),
            idle: Notify::new(),
        });
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let (handler_released, handler_closed) = oneshot::channel();
        let handler = McpBridge {
            state: state.clone(),
            _release: Arc::new(HandlerRelease(Some(handler_released))),
        };
        let sessions = Arc::new(LocalSessionManager::default());
        let service: StreamableHttpService<McpBridge, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(handler.clone()),
                sessions.clone(),
                StreamableHttpServerConfig::default()
                    .with_sse_keep_alive(None)
                    .with_cancellation_token(stop.child_token()),
            );
        let app = Router::new()
            .nest_service("/mcp", service)
            .route(
                "/v1/chat/completions",
                post(completion)
                    .layer(axum::extract::DefaultBodyLimit::max(MODEL_PROXY_BODY_LIMIT)),
            )
            .route(
                "/anchor/v1/chat/completions",
                post(completion)
                    .layer(axum::extract::DefaultBodyLimit::max(MODEL_PROXY_BODY_LIMIT)),
            )
            .route(
                "/v1/responses",
                post(completion)
                    .layer(axum::extract::DefaultBodyLimit::max(MODEL_PROXY_BODY_LIMIT)),
            )
            .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
            .layer(middleware::from_fn_with_state(state.clone(), authorize))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
        });
        Ok(Self {
            url: format!("http://{address}"),
            token,
            state,
            address,
            socket: std::sync::Mutex::new(None),
            stop,
            task: Mutex::new(Some(task)),
            sessions,
            handler_closed: Mutex::new(Some(handler_closed)),
        })
    }

    /// Additionally expose the bridge on a UNIX socket.
    ///
    /// A node sandbox that shares no network with the host reaches the bridge
    /// through an in-sandbox relay: the relay listens on that sandbox's own
    /// loopback and forwards to this socket, which the host mounts read-only.
    pub(super) fn expose_on_unix_socket(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let _ = std::fs::remove_file(path);
        let listener = tokio::net::UnixListener::bind(path).map_err(|error| error.to_string())?;
        *self
            .socket
            .lock()
            .map_err(|_| "bridge socket lock poisoned".to_owned())? = Some(path.to_path_buf());
        let address = self.address;
        let stop = self.stop.clone();
        tokio::spawn(forward_unix(listener, address, stop));
        Ok(())
    }

    /// Remove the extra UNIX socket, if one was bound.
    fn remove_socket(&self) {
        if let Ok(mut socket) = self.socket.lock()
            && let Some(path) = socket.take()
        {
            let _ = std::fs::remove_file(path);
        }
    }

    fn cancel(&self) {
        self.remove_socket();
        self.state.accepting.store(false, Ordering::SeqCst);
        self.state.cancellation.store(true, Ordering::SeqCst);
        self.stop.cancel();
    }

    pub(super) async fn close(&self) -> Result<(), String> {
        self.remove_socket();
        self.state.accepting.store(false, Ordering::SeqCst);
        self.state.cancellation.store(true, Ordering::SeqCst);
        let settle = async {
            loop {
                let idle = self.state.idle.notified();
                if self.state.active_calls.load(Ordering::SeqCst) == 0 {
                    break;
                }
                idle.await;
            }
        };
        let settled = if self.state.fixture {
            tokio::time::timeout(std::time::Duration::from_secs(3), settle)
                .await
                .map_err(|_| {
                    "MCP tools did not settle; external results remain uncertain".to_owned()
                })
        } else {
            settle.await;
            Ok(())
        };
        self.cancel();
        let timeout = std::time::Duration::from_secs(if self.state.fixture { 3 } else { 10 });
        // Axum joins its own HTTP connections during graceful shutdown. Aborting
        // only the serve task would leave keepalive/SSE handlers owning tools.
        let http_closed = {
            let mut task = self.task.lock().await;
            let result = if let Some(task) = task.as_mut() {
                match tokio::time::timeout(timeout, &mut *task).await {
                    Ok(Ok(Ok(()))) => Ok(()),
                    Ok(Ok(Err(error))) => Err(format!("MCP bridge HTTP shutdown failed: {error}")),
                    Ok(Err(error)) => Err(format!("MCP bridge task failed: {error}")),
                    Err(_) => {
                        task.abort();
                        let _ = task.await;
                        Err("MCP bridge HTTP connections did not close; tool ownership remains uncertain".into())
                    }
                }
            } else {
                Ok(())
            };
            task.take();
            result
        };
        // The fixed rmcp release cancels SSE streams through the token above,
        // but native session workers must also receive its close_session call.
        let session_ids = self
            .sessions
            .sessions
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let sessions_closed = async {
            for id in session_ids {
                self.sessions
                    .close_session(&id)
                    .await
                    .map_err(|error| format!("MCP session shutdown failed: {error}"))?;
            }
            Ok::<_, String>(())
        };
        let sessions_closed = tokio::time::timeout(timeout, sessions_closed)
            .await
            .unwrap_or_else(|_| {
                Err("MCP sessions did not close; tool ownership remains uncertain".into())
            });
        let handlers_closed = {
            let mut closed = self.handler_closed.lock().await;
            if let Some(receiver) = closed.as_mut() {
                match tokio::time::timeout(timeout, receiver).await {
                    Ok(Ok(())) => {
                        closed.take();
                        Ok(())
                    }
                    Ok(Err(error)) => Err(format!("MCP handler shutdown unavailable: {error}")),
                    Err(_) => Err(
                        "MCP handlers did not release tools; tool ownership remains uncertain"
                            .into(),
                    ),
                }
            } else {
                Ok(())
            }
        };
        settled
            .and(http_closed)
            .and(sessions_closed)
            .and(handlers_closed)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.cancel();
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_runtime::ToolError;
    use anchor_runtime::graph::InvocationKey;
    use rmcp::{
        ServiceExt,
        transport::{
            StreamableHttpClientTransport,
            streamable_http_client::StreamableHttpClientTransportConfig,
        },
    };

    struct SlowTools {
        cancellation: Cancellation,
        entered: Notify,
        finished: AtomicBool,
    }

    fn native_fact() -> super::super::Fact {
        super::super::Fact {
            version: 2,
            key: InvocationKey {
                run_id: "run".into(),
                graph_digest: "graph".into(),
                node_id: "worker".into(),
                invocation: 1,
            },
            binary_sha256: "binary".into(),
            model_binding: Some("model".into()),
            session_id: Some("native-session".into()),
            completion: None,
            reason: None,
            tool_observation: None,
            conversation_scope: None,
            provider_failure: None,
        }
    }

    fn native_request(
        name: &'static str,
        arguments: Value,
        batch: &str,
        size: u64,
    ) -> CallToolRequestParams {
        let mut request = CallToolRequestParams::new(name)
            .with_arguments(serde_json::from_value(arguments).unwrap());
        request.meta = Some(
            serde_json::from_value(json!({
                "agent-session-id":"native-session",
                "agent-tool-call-request-id":format!("{batch}-{name}"),
                "agent-tool-call-batch-id":batch,
                "agent-tool-call-batch-size":size
            }))
            .unwrap(),
        );
        request
    }

    #[test]
    fn native_identity_requires_bound_session_and_unique_unspoofed_metadata() {
        let fact = native_fact();
        let meta: rmcp::model::Meta = serde_json::from_value(
            json!({"agent-session-id":"native-session","agent-tool-call-request-id":"native-call"}),
        )
        .unwrap();
        let empty = rmcp::model::Meta::default();
        let identity = native_tool_identity(&fact, Some(&meta), &empty).unwrap();
        assert_eq!(identity.key, fact.key);
        assert_eq!(identity.tool_call, "native-call");
        assert!(native_tool_identity(&fact, None, &meta).is_some());
        assert!(native_tool_identity(&fact, None, &empty).is_none());
        for invalid in [
            json!({"agent-session-id":"other","agent-tool-call-request-id":"native-call"}),
            json!({"agent-session-id":"native-session"}),
            json!({"agent-session-id":"native-session","agent-tool-call-request-id":"native-call","Agent-Tool-Call-Request-Id":"spoofed"}),
            json!({"agent-session-id":"native-session","agent-tool-call-request-id":"call\n"}),
            json!({"agent-session-id":"native-session","agent-tool-call-request-id":17}),
        ] {
            let invalid = serde_json::from_value(invalid).unwrap();
            assert!(native_tool_identity(&fact, Some(&invalid), &empty).is_none());
            assert!(native_tool_identity(&fact, Some(&invalid), &meta).is_none());
        }
    }

    impl ToolPort for SlowTools {
        fn definitions(&self) -> Vec<anchor_runtime::ToolDefinition> {
            vec![anchor_runtime::ToolDefinition::new(
                anchor_runtime::ToolName::new("wait").unwrap(),
                "Wait for controlled cancellation",
                json!({"type":"object"}),
            )]
        }

        fn call<'a>(
            &'a self,
            _: &'a str,
            _: Value,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<Vec<anchor_runtime::ToolResultContent>, ToolError>,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.entered.notify_waiters();
                while !self.cancellation.load(Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                self.finished.store(true, Ordering::SeqCst);
                Err(ToolError::Failed("cancelled tool settled".into()))
            })
        }
    }

    #[tokio::test]
    async fn unix_socket_exposes_the_loopback_listener() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (bridge, _tools) = slow_bridge(false).await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bridge.sock");
        bridge.expose_on_unix_socket(&path).unwrap();
        let mut stream = tokio::net::UnixStream::connect(&path).await.unwrap();
        let request = format!(
            "GET /unknown HTTP/1.1\r\nhost: bridge\r\nauthorization: Bearer {}\r\nconnection: close\r\n\r\n",
            bridge.token
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 404"), "{text}");
    }

    async fn slow_bridge(fixture: bool) -> (Bridge, Arc<SlowTools>) {
        slow_bridge_at(fixture, None).await
    }

    async fn slow_bridge_at(
        fixture: bool,
        observation_path: Option<PathBuf>,
    ) -> (Bridge, Arc<SlowTools>) {
        anchor_mcp_host::initialize_crypto_provider();
        let cancellation = Cancellation::default();
        let tools = Arc::new(SlowTools {
            cancellation: cancellation.clone(),
            entered: Notify::new(),
            finished: AtomicBool::new(false),
        });
        let (disclosed, plugin_instructions) = crate::tool_disclosure::maybe_wrap(
            tools.clone(),
            ["wait"],
            crate::plugin_disclosure::PluginDisclosureCatalog::default(),
        );
        let bridge = Bridge::start(
            disclosed,
            NodeBridgeContext {
                routes: Vec::new(),
                plugin_instructions,
                observation_path,
                outputs: DeclaredOutputs {
                    workspace: std::env::temp_dir(),
                    writes: Vec::new(),
                },
            },
            cancellation,
            Some(super::super::configuration::ModelUpstream {
                url: reqwest::Url::parse("http://127.0.0.1:9/v1/chat/completions").unwrap(),
                api_key: "fixture-only-not-a-secret".into(),
            }),
            fixture,
            "test-only-token".into(),
        )
        .await
        .unwrap();
        (bridge, tools)
    }

    #[tokio::test]
    async fn close_releases_tools_while_http_keepalive_and_mcp_clients_remain_open() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (bridge, tools) = slow_bridge(false).await;
        let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", bridge.url))
            .auth_header(bridge.token.clone());
        let client = ().serve(StreamableHttpClientTransport::from_config(config)).await.unwrap();
        client.list_all_tools().await.unwrap();

        let mut stream = tokio::net::TcpStream::connect(bridge.address)
            .await
            .unwrap();
        stream.write_all(format!(
            "GET /unknown HTTP/1.1\r\nhost: bridge\r\nauthorization: Bearer {}\r\nconnection: keep-alive\r\n\r\n",
            bridge.token
        ).as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !response.ends_with(b"\r\n\r\n") {
                let byte = stream.read_u8().await.unwrap();
                response.push(byte);
            }
        })
        .await
        .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 404"));

        bridge.close().await.unwrap();
        assert_eq!(
            Arc::strong_count(&bridge.state),
            1,
            "server tasks still own the tools"
        );
        drop(bridge);
        assert_eq!(
            Arc::strong_count(&tools),
            1,
            "tool shutdown cannot obtain exclusive ownership"
        );
        let mut remaining = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            stream.read_to_end(&mut remaining),
        )
        .await
        .unwrap()
        .unwrap();
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn mcp_initialize_carries_persistent_discovery_instructions_even_without_plugins() {
        let (bridge, _) = slow_bridge(false).await;
        let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", bridge.url))
            .auth_header(bridge.token.clone());
        let client = ().serve(StreamableHttpClientTransport::from_config(config)).await.unwrap();
        let peer = client.peer_info().unwrap();
        let instructions = peer.instructions.as_deref().unwrap();
        assert_eq!(instructions, bridge.state.plugin_instructions);
        assert!(
            instructions.starts_with("Current Anchor Plugin capabilities for this invocation.")
        );
        assert!(instructions.contains("anchor_plugins"));
        let footer: Value = serde_json::from_str(instructions.lines().last().unwrap()).unwrap();
        assert_eq!(footer["total"], 0);
        assert_eq!(footer["omitted"], 0);
        client.cancel().await.unwrap();
        bridge.close().await.unwrap();
    }

    #[tokio::test]
    async fn native_tool_failure_retains_mcp_error_and_observed_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let fact_path = directory.path().join("invocation.json");
        super::super::store_fact(&fact_path, &native_fact()).unwrap();
        let (bridge, tools) = slow_bridge_at(false, Some(fact_path)).await;
        let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", bridge.url))
            .auth_header(bridge.token.clone());
        let client = Arc::new(
            ().serve(StreamableHttpClientTransport::from_config(config))
                .await
                .unwrap(),
        );
        let entered = tools.entered.notified();
        let called = client.clone();
        let call = tokio::spawn(async move {
            called
                .call_tool(native_request(
                    "anchor_tools_call",
                    json!({"name":"wait","arguments":{}}),
                    "failed-business",
                    1,
                ))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered)
            .await
            .unwrap();
        tools.cancellation.store(true, Ordering::SeqCst);
        let response = tokio::time::timeout(std::time::Duration::from_secs(3), call)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let response = serde_json::to_value(response).unwrap();
        assert_eq!(response["isError"], true);
        let feedback: Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(feedback["ok"], false);
        assert!(
            feedback["error"]
                .as_str()
                .unwrap()
                .contains("cancelled tool settled")
        );
        assert_eq!(feedback["anchor_receipt"].as_str().unwrap().len(), 64);
        assert_eq!(bridge.state.calls.lock().await[0]["result"]["ok"], false);
        assert_eq!(bridge.state.calls.lock().await[0]["tool"], "wait");
        tools.cancellation.store(false, Ordering::SeqCst);
        let discovery = client
            .call_tool(native_request(
                "anchor_plugins",
                json!({}),
                "latest-discovery",
                1,
            ))
            .await
            .unwrap();
        let discovery = serde_json::to_value(discovery).unwrap();
        assert_ne!(discovery["isError"], true);
        let discovered: Value =
            serde_json::from_str(discovery["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(discovered["anchor_receipt"].as_str().unwrap().len(), 64);
        assert_ne!(discovered["anchor_receipt"], feedback["anchor_receipt"]);
        for rejected_receipt in [json!("invalid-receipt"), feedback["anchor_receipt"].clone()] {
            let rejected = client
                .call_tool(native_request(
                    "final_result",
                    json!({
                        "summary":"This receipt must not complete the node.",
                        "files":[],
                        "observed_receipt":rejected_receipt
                    }),
                    "rejected-final",
                    1,
                ))
                .await
                .unwrap();
            let rejected = serde_json::to_value(rejected).unwrap();
            assert_eq!(rejected["isError"], true);
            let diagnostic = rejected["content"][0]["text"].as_str().unwrap();
            assert!(diagnostic.contains("final_result.observed_receipt must match"));
            assert!(diagnostic.contains("including discovery and read-only tools"));
            assert!(!diagnostic.contains("must be called alone"));
            assert!(bridge.state.completion.lock().await.is_none());
        }
        let accepted = client
            .call_tool(native_request(
                "final_result",
                json!({
                    "summary":"Observed the failed business result and latest discovery result.",
                    "files":[],
                    "observed_receipt":discovered["anchor_receipt"]
                }),
                "accepted-final",
                1,
            ))
            .await
            .unwrap();
        assert_ne!(accepted.is_error, Some(true));
        assert!(bridge.state.completion.lock().await.is_some());
        assert_eq!(
            bridge
                .state
                .calls
                .lock()
                .await
                .iter()
                .filter(|call| call["tool"] == "wait")
                .count(),
            1
        );
        let client = Arc::try_unwrap(client).ok().unwrap();
        client.cancel().await.unwrap();
        bridge.close().await.unwrap();
    }

    #[tokio::test]
    async fn native_completion_freezes_candidate_without_publishing_and_cancellation_still_wins() {
        let directory = tempfile::tempdir().unwrap();
        let fact_path = directory.path().join("invocation.json");
        super::super::store_fact(&fact_path, &native_fact()).unwrap();
        let (bridge, tools) = slow_bridge_at(false, Some(fact_path.clone())).await;
        let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", bridge.url))
            .auth_header(bridge.token.clone());
        let client = ().serve(StreamableHttpClientTransport::from_config(config)).await.unwrap();

        let missing_meta = client
            .call_tool(CallToolRequestParams::new("anchor_plugins"))
            .await
            .unwrap();
        assert_eq!(missing_meta.is_error, Some(true));
        assert!(
            bridge
                .state
                .load_fact()
                .unwrap()
                .unwrap()
                .tool_observation
                .is_none()
        );
        assert!(!tools.finished.load(Ordering::SeqCst));

        let submit = json!({"summary":"accepted candidate","files":[]});
        let accepted = client
            .call_tool(native_request("final_result", submit.clone(), "final", 1))
            .await
            .unwrap();
        assert_ne!(accepted.is_error, Some(true));
        let candidate = bridge.state.completion.lock().await.clone().unwrap();
        // The candidate alone is never a durable completion. The invocation
        // owner still has to observe end_turn and finish both cleanup paths.
        assert!(
            super::super::read_fact(&fact_path)
                .unwrap()
                .unwrap()
                .completion
                .is_none()
        );
        let duplicate = client
            .call_tool(native_request(
                "final_result",
                submit.clone(),
                "duplicate",
                1,
            ))
            .await
            .unwrap();
        assert_ne!(duplicate.is_error, Some(true));
        for request in [
            native_request(
                "final_result",
                json!({"summary":"conflicting candidate","files":[]}),
                "conflict",
                1,
            ),
            native_request("anchor_plugins", json!({}), "later-business", 1),
        ] {
            assert_eq!(
                client.call_tool(request).await.unwrap().is_error,
                Some(true)
            );
            assert_eq!(
                bridge.state.completion.lock().await.as_ref(),
                Some(&candidate)
            );
            assert!(!*bridge.state.after_completion.lock().await);
        }
        bridge.state.cancellation.store(true, Ordering::SeqCst);
        let cancelled = client
            .call_tool(native_request("final_result", submit, "cancelled", 1))
            .await
            .unwrap();
        assert_eq!(cancelled.is_error, Some(true));
        assert_eq!(
            bridge.state.completion.lock().await.as_ref(),
            Some(&candidate)
        );
        assert!(
            super::super::read_fact(&fact_path)
                .unwrap()
                .unwrap()
                .completion
                .is_none()
        );
        client.cancel().await.unwrap();
        bridge.close().await.unwrap();
    }

    #[tokio::test]
    async fn native_restored_observation_needs_a_current_authorized_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let fact_path = directory.path().join("invocation.json");
        let mut fact = native_fact();
        fact.tool_observation = Some(json!({"tool":"previous","result":{"anchor_receipt":"old"}}));
        super::super::store_fact(&fact_path, &fact).unwrap();
        let (bridge, _) = slow_bridge_at(false, Some(fact_path)).await;
        let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", bridge.url))
            .auth_header(bridge.token.clone());
        let client = ().serve(StreamableHttpClientTransport::from_config(config)).await.unwrap();
        for submit in [
            json!({"summary":"missing","files":[]}),
            json!({"summary":"old","observed_receipt":"old","files":[]}),
        ] {
            assert_eq!(
                client
                    .call_tool(native_request("final_result", submit, "restored-final", 1))
                    .await
                    .unwrap()
                    .is_error,
                Some(true)
            );
        }
        let discovery = client
            .call_tool(native_request("anchor_plugins", json!({}), "inspection", 1))
            .await
            .unwrap();
        let discovery = serde_json::to_value(discovery).unwrap();
        let output: Value =
            serde_json::from_str(discovery["content"][0]["text"].as_str().unwrap()).unwrap();
        let accepted = client
            .call_tool(native_request(
                "final_result",
                json!({"summary":"inspected", "observed_receipt":output["anchor_receipt"],"files":[]}),
                "final",
                1,
            ))
            .await
            .unwrap();
        assert_ne!(accepted.is_error, Some(true));
        client.cancel().await.unwrap();
        bridge.close().await.unwrap();
    }

    #[tokio::test]
    async fn native_trace_tail_does_not_stop_long_tasks_and_spike_still_has_a_limit() {
        let (native, _) = slow_bridge(false).await;
        for sequence in 0..260 {
            native
                .state
                .record_call(json!({"sequence":sequence}))
                .await
                .unwrap();
        }
        assert_eq!(native.state.calls.lock().await.len(), 256);
        assert_eq!(native.state.calls.lock().await[0]["sequence"], 4);
        assert_eq!(native.state.calls.lock().await[255]["sequence"], 259);
        assert_eq!(native.state.dropped_calls.load(Ordering::SeqCst), 4);
        assert!(!native.state.cancellation.load(Ordering::SeqCst));
        native.close().await.unwrap();
        let (spike, _) = slow_bridge(true).await;
        for sequence in 0..256 {
            spike
                .state
                .record_call(json!({"sequence":sequence}))
                .await
                .unwrap();
        }
        assert!(
            spike
                .state
                .record_call(json!({"sequence":256}))
                .await
                .is_err()
        );
        assert!(spike.state.cancellation.load(Ordering::SeqCst));
        assert_eq!(spike.state.dropped_calls.load(Ordering::SeqCst), 0);
        spike.close().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_waits_for_the_actual_mcp_tool_future_to_settle() {
        let (bridge, tools) = slow_bridge(true).await;
        let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", bridge.url))
            .auth_header(bridge.token.clone());
        let transport = StreamableHttpClientTransport::from_config(config);
        let client = Arc::new(().serve(transport).await.unwrap());
        let entered = tools.entered.notified();
        let called = client.clone();
        let call = tokio::spawn(async move {
            called
                .call_tool(
                    CallToolRequestParams::new("anchor_tools_call").with_arguments(
                        serde_json::from_value(json!({"name":"wait","arguments":{}})).unwrap(),
                    ),
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered)
            .await
            .unwrap();
        bridge.close().await.unwrap();
        assert!(tools.finished.load(Ordering::SeqCst));
        assert_eq!(bridge.state.active_calls.load(Ordering::SeqCst), 0);
        assert_eq!(bridge.state.calls.lock().await.len(), 1);
        assert!(!bridge.state.accepting.load(Ordering::SeqCst));
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), call)
            .await
            .unwrap();
        let client = Arc::try_unwrap(client).ok().unwrap();
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn native_close_keeps_invocation_owned_until_actual_tools_settle() {
        let (bridge, _) = slow_bridge(false).await;
        bridge.state.active_calls.fetch_add(1, Ordering::SeqCst);
        let active = ActiveCall(bridge.state.clone());
        let close = bridge.close();
        tokio::pin!(close);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(3100), &mut close)
                .await
                .is_err()
        );
        assert_eq!(bridge.state.active_calls.load(Ordering::SeqCst), 1);
        assert!(!bridge.state.accepting.load(Ordering::SeqCst));
        drop(active);
        tokio::time::timeout(std::time::Duration::from_secs(1), close)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bridge.state.active_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn shutdown_timeout_keeps_unsettled_tools_uncertain() {
        let (bridge, _) = slow_bridge(true).await;
        bridge.state.active_calls.fetch_add(1, Ordering::SeqCst);
        let active = ActiveCall(bridge.state.clone());
        assert!(bridge.close().await.unwrap_err().contains("uncertain"));
        assert_eq!(bridge.state.active_calls.load(Ordering::SeqCst), 1);
        drop(active);
    }

    #[test]
    fn completion_rejects_text_bad_routes_and_extra_fields() {
        let routes = vec!["ok".to_owned(), "retry".to_owned()];
        for value in [
            json!("done"),
            json!({"summary":" "}),
            json!({"summary":"done","route":"other"}),
            json!({"summary":"done","route":"ok","unsafe":true}),
        ] {
            assert!(validate_completion(&value, &routes).is_err());
        }
        assert_eq!(
            validate_completion(
                &json!({"summary":"done","route":"retry","files":[]}),
                &routes
            )
            .unwrap(),
            json!({"summary":"done","route":"retry","files":[]})
        );
    }

    #[test]
    fn completion_waits_for_every_declared_output_file_or_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let outputs = DeclaredOutputs {
            workspace: workspace.path().to_path_buf(),
            writes: vec!["review.json".into(), "data".into()],
        };
        // The blind-03 reviewer wrote into a subdirectory.
        std::fs::create_dir(workspace.path().join("review")).unwrap();
        std::fs::write(workspace.path().join("review/review.json"), "{}").unwrap();
        let completion = json!({"files":["review.json","data"]});
        let refused = outputs.validate(&completion).unwrap_err();
        assert!(refused.starts_with("final_result refused:"), "{refused}");
        assert!(refused.contains("No completion was recorded"), "{refused}");
        std::fs::write(workspace.path().join("review.json"), "{}").unwrap();
        std::fs::create_dir(workspace.path().join("data")).unwrap();
        assert_eq!(outputs.validate(&completion), Ok(()));
        assert!(outputs.validate(&json!({"files":[]})).is_err());
    }

    #[test]
    fn no_route_and_single_route_are_explicit() {
        assert_eq!(
            validate_completion(&json!({"summary":"done","files":[]}), &[]).unwrap(),
            json!({"summary":"done","route":null,"files":[]})
        );
        assert!(validate_completion(&json!({"summary":"done","route":"bad"}), &[]).is_err());
        assert_eq!(
            validate_completion(&json!({"summary":"done","files":[]}), &["ok".into()]).unwrap(),
            json!({"summary":"done","route":"ok","files":[]})
        );
    }

    #[test]
    fn streamed_tool_names_preserve_batch_boundaries_and_fragmented_names() {
        let stream = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"anchor__\"}},{\"index\":1,\"function\":{\"name\":\"anchor__anchor_run\"}}]}}]}\n\ndata: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"final_result\"}}]}}]}\n\ndata: [DONE]\n\n";
        assert_eq!(
            response_tool_names(stream).unwrap(),
            vec!["anchor__final_result", "anchor__anchor_run"]
        );
        assert!(response_tool_names(b"data: invalid\n").is_err());
        assert_eq!(response_tool_names(br#"{"choices":[{"message":{"tool_calls":[{"function":{"name":"anchor__final_result"}}]}}]}"#).unwrap(), vec!["anchor__final_result"]);
    }
}
