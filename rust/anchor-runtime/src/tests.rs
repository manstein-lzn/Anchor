use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rig_agent::{
    AgentBuilder,
    core::{
        completion::Usage,
        message::{AssistantContent, ToolCall, ToolFunction, ToolName},
        schemars::JsonSchema,
    },
    run::{AgentRunStep, ModelTurn, ModelTurnOutcome},
    test_utils::{MockAddTool, MockCompletionModel, MockStreamEvent, MockTurn},
};
use serde::Deserialize;
use serde_json::json;

use super::{
    AgentCheckpoint, CheckpointError, CheckpointStore, CompletionObserver, CompletionPort,
    ExecutionPolicy,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct AgentCompletion {
    summary: String,
    route: Option<String>,
}

#[test]
fn checkpoint_restores_a_pending_model_turn_and_completes() {
    let mut checkpoint = AgentCheckpoint::start("research", 3, "calculate", 2);
    assert!(matches!(
        checkpoint.run.next_step().expect("model step"),
        AgentRunStep::CallModel { turn: 1, .. }
    ));

    let encoded = checkpoint.encode().expect("encode checkpoint");
    let mut restored =
        AgentCheckpoint::decode(&encoded, "research", 3).expect("restore matching node invocation");
    let turn = ModelTurn::new(
        Some("response-1".to_owned()),
        vec![AssistantContent::text("42")],
        Usage::default(),
        BTreeSet::new(),
        BTreeSet::new(),
        json!({"fixture": true}),
    );
    assert!(matches!(
        restored.run.model_response(turn).expect("accept response"),
        ModelTurnOutcome::Continue { .. }
    ));
    assert!(matches!(
        restored.run.next_step().expect("terminal step"),
        AgentRunStep::Done(response) if response.output == "42"
    ));
}

#[test]
fn checkpoint_cannot_be_reused_for_a_different_invocation() {
    let checkpoint = AgentCheckpoint::start("research", 3, "calculate", 2);
    let encoded = checkpoint.encode().expect("encode checkpoint");
    assert!(matches!(
        AgentCheckpoint::decode(&encoded, "research", 4),
        Err(CheckpointError::IdentityMismatch)
    ));
}

#[test]
fn checkpoint_restores_the_same_pending_tool_call() {
    let mut checkpoint = AgentCheckpoint::start("research", 7, "use a tool", 2);
    assert!(matches!(
        checkpoint.run.next_step().expect("model step"),
        AgentRunStep::CallModel { .. }
    ));
    let mut tools = BTreeSet::new();
    tools.insert("persisted_tool".to_owned());
    checkpoint
        .run
        .model_response(ModelTurn::new(
            Some("response-1".to_owned()),
            vec![AssistantContent::ToolCall(ToolCall::from_wire(
                "call-1",
                ToolFunction::new(
                    ToolName::new("persisted_tool".to_owned()).expect("valid name"),
                    json!({"value": 42}),
                ),
            ))],
            Usage::default(),
            tools.clone(),
            tools,
            json!({"fixture": true}),
        ))
        .expect("accept tool call");

    assert!(matches!(
        checkpoint.run.next_step().expect("pending tool step"),
        AgentRunStep::CallTools { .. }
    ));
    let encoded = checkpoint.encode().expect("encode checkpoint");
    let mut restored =
        AgentCheckpoint::decode(&encoded, "research", 7).expect("restore matching invocation");
    let AgentRunStep::CallTools { calls } = restored.run.next_step().expect("replay pending step")
    else {
        panic!("Rig should restore the pending tool batch");
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool_call.id.to_string(), "call-1");
    assert_eq!(
        calls[0].tool_call.function.name.to_string(),
        "persisted_tool"
    );
}

#[tokio::test]
async fn rig_runs_a_tool_call_with_its_native_agent_driver() {
    let model = MockCompletionModel::from_turns([
        MockTurn::tool_call("call-1", "add", json!({"x": 20, "y": 22})),
        MockTurn::text("42"),
    ]);
    let agent = AgentBuilder::new(model).tool(MockAddTool).build();
    let result = agent
        .prompt("add 20 and 22")
        .max_turns(2)
        .run()
        .await
        .expect("Rig agent call");
    assert_eq!(result.output, "42");
    assert_eq!(result.completion_calls.len(), 2);
}

#[tokio::test]
async fn rig_parses_anchor_shaped_structured_completion() {
    let model = MockCompletionModel::text(r#"{"summary":"review complete","route":"next"}"#);
    let agent = AgentBuilder::new(model).build();
    let result = agent
        .prompt_typed::<AgentCompletion>("review the node")
        .await
        .expect("typed completion");
    assert_eq!(
        result.output,
        AgentCompletion {
            summary: "review complete".to_owned(),
            route: Some("next".to_owned()),
        }
    );
}

#[derive(Clone)]
struct EchoTools;

impl super::ToolPort for EchoTools {
    fn definitions(&self) -> Vec<rig_agent::core::completion::ToolDefinition> {
        vec![rig_agent::core::completion::ToolDefinition::new(
            rig_agent::core::message::ToolName::new("echo").expect("tool name"),
            "Return the supplied value",
            serde_json::json!({"type":"object"}),
        )]
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: serde_json::Value,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Vec<rig_agent::core::message::ToolResultContent>,
                        super::ToolError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if name != "echo" {
                return Err(super::ToolError::Unknown(name.to_owned()));
            }
            Ok(vec![rig_agent::core::message::ToolResultContent::text(
                arguments.to_string(),
            )])
        })
    }
}

struct FailingTools;

impl super::ToolPort for FailingTools {
    fn definitions(&self) -> Vec<rig_agent::core::completion::ToolDefinition> {
        EchoTools.definitions()
    }

    fn call<'a>(
        &'a self,
        _name: &'a str,
        _arguments: serde_json::Value,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Vec<rig_agent::core::message::ToolResultContent>,
                        super::ToolError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async { Err(super::ToolError::Failed("fixture tool failure".to_owned())) })
    }
}

#[derive(Default)]
struct RecordingObserver {
    events: Mutex<Vec<String>>,
}

impl CompletionObserver for RecordingObserver {
    fn request_started(&self, provider: &str) {
        self.events
            .lock()
            .expect("observer lock")
            .push(format!("start:{provider}"));
    }

    fn request_finished(
        &self,
        provider: &str,
        _response: &rig_agent::core::completion::CompletionResponse,
    ) {
        self.events
            .lock()
            .expect("observer lock")
            .push(format!("finish:{provider}"));
    }

    fn request_failed(&self, provider: &str, _error: &rig_agent::core::error::ProviderError) {
        self.events
            .lock()
            .expect("observer lock")
            .push(format!("fail:{provider}"));
    }
}

struct SlowCompletion;

impl CompletionPort for SlowCompletion {
    fn capabilities(&self) -> rig_agent::core::completion::ProviderCapabilities {
        rig_agent::core::completion::ProviderCapabilities::default()
    }

    fn complete<'a>(
        &'a self,
        _request: rig_agent::core::completion::CompletionRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        rig_agent::core::completion::CompletionResponse,
                        rig_agent::core::error::ProviderError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            Err(rig_agent::core::error::ProviderError::request(
                "late fixture",
            ))
        })
    }
}

#[tokio::test]
async fn node_executor_drives_tools_and_validates_route() {
    let model = MockCompletionModel::from_turns([
        MockTurn::tool_call("echo-1", "echo", json!({"value":"fixture"})),
        MockTurn::text(r#"{"summary":"tool complete","route":"next"}"#),
    ]);
    let port = super::RigCompletionPort::new(model.erase());
    let mut checkpoint = AgentCheckpoint::start("review", 1, "use echo", 3);
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let outcome = super::NodeExecutor::execute(
        &mut checkpoint,
        &port,
        &EchoTools,
        &cancellation,
        &["next".to_owned()],
    )
    .await
    .expect("node execution");
    assert_eq!(outcome.status, super::NodeStatus::Completed);
    assert_eq!(outcome.submission, "tool complete");
    assert_eq!(outcome.route.as_deref(), Some("next"));
    assert_eq!(outcome.model_requests, 2);
}

#[tokio::test]
async fn node_executor_rejects_unknown_route() {
    let port = super::RigCompletionPort::new(
        MockCompletionModel::text(r#"{"summary":"done","route":"forbidden"}"#).erase(),
    );
    let mut checkpoint = AgentCheckpoint::start("review", 2, "finish", 1);
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error = super::NodeExecutor::execute(
        &mut checkpoint,
        &port,
        &EchoTools,
        &cancellation,
        &["next".to_owned()],
    )
    .await
    .expect_err("route must be checked");
    assert!(error.to_string().contains("not allowed"));
}

#[tokio::test]
async fn node_executor_requires_a_route_when_multiple_routes_are_available() {
    let port =
        super::RigCompletionPort::new(MockCompletionModel::text(r#"{"summary":"done"}"#).erase());
    let mut checkpoint = AgentCheckpoint::start("review", 4, "finish", 1);
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error = super::NodeExecutor::execute(
        &mut checkpoint,
        &port,
        &EchoTools,
        &cancellation,
        &["accept".to_owned(), "revise".to_owned()],
    )
    .await
    .expect_err("a multi-exit node must select one route");
    assert!(error.to_string().contains("choose exactly one route"));
    assert!(error.to_string().contains("accept, revise"));
}

#[tokio::test]
async fn node_executor_keeps_route_optional_with_zero_or_one_exit() {
    for routes in [vec![], vec!["next".to_owned()]] {
        let port = super::RigCompletionPort::new(
            MockCompletionModel::text(r#"{"summary":"done"}"#).erase(),
        );
        let mut checkpoint = AgentCheckpoint::start("review", 5, "finish", 1);
        let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let outcome = super::NodeExecutor::execute(
            &mut checkpoint,
            &port,
            &EchoTools,
            &cancellation,
            &routes,
        )
        .await
        .expect("route remains optional with at most one exit");
        assert_eq!(outcome.route, None);
        assert_eq!(outcome.submission, "done");
    }
}

#[tokio::test]
async fn node_executor_honors_cancellation_before_io() {
    let port = super::RigCompletionPort::new(
        MockCompletionModel::text(r#"{"summary":"unreachable"}"#).erase(),
    );
    let mut checkpoint = AgentCheckpoint::start("review", 3, "cancel", 1);
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let outcome =
        super::NodeExecutor::execute(&mut checkpoint, &port, &EchoTools, &cancellation, &[])
            .await
            .expect("cancellation is a normal outcome");
    assert_eq!(outcome.status, super::NodeStatus::Cancelled);
    assert_eq!(outcome.model_requests, 0);
}

#[tokio::test]
async fn node_executor_resumes_a_persisted_tool_boundary() {
    let mut checkpoint = AgentCheckpoint::start("review", 4, "use echo", 3);
    assert!(matches!(
        checkpoint.run.next_step().expect("model step"),
        AgentRunStep::CallModel { turn: 1, .. }
    ));
    let mut advertised = BTreeSet::new();
    advertised.insert("echo".to_owned());
    checkpoint
        .run
        .model_response(ModelTurn::new(
            Some("response-1".to_owned()),
            vec![AssistantContent::ToolCall(ToolCall::from_wire(
                "echo-1",
                ToolFunction::new(
                    ToolName::new("echo").expect("tool name"),
                    json!({"value":"fixture"}),
                ),
            ))],
            Usage::default(),
            advertised.clone(),
            advertised,
            json!({"fixture": true}),
        ))
        .expect("record pending tool");
    assert!(matches!(
        checkpoint.run.next_step().expect("tool step"),
        AgentRunStep::CallTools { .. }
    ));
    let bytes = checkpoint.encode().expect("encode");
    let mut restored = AgentCheckpoint::decode(&bytes, "review", 4).expect("decode");
    let port = super::RigCompletionPort::new(
        MockCompletionModel::text(r#"{"summary":"resumed","route":"next"}"#).erase(),
    );
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let outcome = super::NodeExecutor::execute(
        &mut restored,
        &port,
        &EchoTools,
        &cancellation,
        &["next".to_owned()],
    )
    .await
    .expect("resume execution");
    assert_eq!(outcome.submission, "resumed");
    assert_eq!(outcome.model_requests, 1);
}

#[test]
fn checkpoint_can_be_created_from_node_request() {
    let request = super::NodeRequest {
        execution_id: "exec-1".to_owned(),
        task: "review".to_owned(),
        instructions: "be concise".to_owned(),
        routes: vec!["done".to_owned()],
        max_turns: 2,
        workspace: std::path::PathBuf::from("/tmp/workspace"),
        cancellation: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let checkpoint = AgentCheckpoint::from_request("review", 5, &request);
    assert!(checkpoint.run.initial_prompt().is_some());
    assert_eq!(checkpoint.run_spec.max_turns, Some(2));
}

fn structured_node_request(routes: &[&str]) -> super::NodeRequest {
    super::NodeRequest {
        execution_id: "structured-fixture".into(),
        task: "perform the work and finish".into(),
        instructions: "preserve the result".into(),
        routes: routes.iter().map(|route| (*route).into()).collect(),
        max_turns: 8,
        workspace: "/tmp/structured-fixture".into(),
        cancellation: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

struct StructuredTools {
    name: &'static str,
    calls: Mutex<usize>,
}

impl super::ToolPort for StructuredTools {
    fn definitions(&self) -> Vec<rig_agent::core::completion::ToolDefinition> {
        vec![rig_agent::core::completion::ToolDefinition::new(
            ToolName::new(self.name).unwrap(),
            "fixture business tool",
            json!({"type":"object"}),
        )]
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: serde_json::Value,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Vec<rig_agent::core::message::ToolResultContent>,
                        super::ToolError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if name != self.name {
                return Err(super::ToolError::Unknown(name.into()));
            }
            *self.calls.lock().unwrap() += 1;
            Ok(vec![rig_agent::core::message::ToolResultContent::json(
                arguments,
            )])
        })
    }
}

struct RecordingCompletion {
    inner: super::RigCompletionPort,
    requests: Mutex<Vec<rig_agent::core::completion::CompletionRequest>>,
}

impl CompletionPort for RecordingCompletion {
    fn capabilities(&self) -> rig_agent::core::completion::ProviderCapabilities {
        self.inner.capabilities()
    }

    fn complete<'a>(
        &'a self,
        request: rig_agent::core::completion::CompletionRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        rig_agent::core::completion::CompletionResponse,
                        rig_agent::core::error::ProviderError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.complete(request)
    }
}

#[test]
fn structured_node_schema_requires_a_route_only_for_multiple_exits() {
    for routes in [vec![], vec!["next"], vec!["accept", "revise"]] {
        let checkpoint =
            AgentCheckpoint::from_request("work", 1, &structured_node_request(&routes));
        let schema = checkpoint.run_spec.output_schema.as_ref().unwrap();
        assert_eq!(checkpoint.run_spec.output_mode, super::OutputMode::Tool);
        assert_eq!(schema["properties"]["summary"]["type"], "string");
        assert_eq!(
            schema["required"],
            if routes.len() > 1 {
                json!(["summary", "route"])
            } else {
                json!(["summary"])
            }
        );
        if routes.len() > 1 {
            assert_eq!(schema["properties"]["route"]["enum"], json!(routes));
        }
    }
    assert!(
        AgentCheckpoint::start("work", 1, "low-level", 2)
            .run_spec
            .output_schema
            .is_none()
    );
}

#[tokio::test]
async fn structured_node_corrects_prose_with_rig_feedback_without_replaying_business_tools() {
    let model = MockCompletionModel::from_turns([
        MockTurn::tool_call("business", "echo", json!({"report":"already-written"})),
        MockTurn::text("Finished the report.\n```json\n{\"summary\":\"done\"}\n```"),
        MockTurn::tool_call(
            "answer",
            "final_result",
            json!({"summary":"done","route":"next"}),
        ),
    ]);
    let completion = RecordingCompletion {
        inner: super::RigCompletionPort::new(model.erase()),
        requests: Mutex::new(Vec::new()),
    };
    let tools = StructuredTools {
        name: "echo",
        calls: Mutex::new(0),
    };
    let request = structured_node_request(&["next"]);
    let mut checkpoint = AgentCheckpoint::from_request("work", 1, &request);
    let outcome = super::NodeExecutor::execute(
        &mut checkpoint,
        &completion,
        &tools,
        &request.cancellation,
        &request.routes,
    )
    .await
    .unwrap();
    assert_eq!(outcome.submission, "done");
    assert_eq!(outcome.route.as_deref(), Some("next"));
    assert_eq!(outcome.model_requests, 3);
    assert_eq!(*tools.calls.lock().unwrap(), 1);
    let requests = completion.requests.lock().unwrap();
    assert!(
        serde_json::to_string(&requests[2].chat_history)
            .unwrap()
            .contains("not as plain text")
    );
    assert!(
        serde_json::to_string(&requests[2].chat_history)
            .unwrap()
            .contains("already-written")
    );
    for request in requests.iter() {
        assert_eq!(
            request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["echo", "final_result"]
        );
    }
    assert_eq!(checkpoint.run.output_tool_name(), Some("final_result"));
}

#[tokio::test]
async fn structured_output_tool_name_is_saved_before_provider_io_and_pinned_on_reload() {
    let root = std::env::temp_dir().join(format!(
        "anchor-output-name-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = super::FileCheckpointStore::new(&root);
    let request = structured_node_request(&[]);
    let mut checkpoint = AgentCheckpoint::from_request("work", 1, &request);
    let tools = StructuredTools {
        name: "final_result",
        calls: Mutex::new(0),
    };
    let failed = super::RigCompletionPort::new(
        MockCompletionModel::from_turns([MockTurn::error("interrupt")]).erase(),
    );
    assert!(
        super::NodeExecutor::execute_with_store(
            &mut checkpoint,
            &failed,
            &tools,
            &store,
            "output-name",
            &request.cancellation,
            &request.routes
        )
        .await
        .is_err()
    );
    let mut restored = store.load("output-name", "work", 1).unwrap().unwrap();
    assert_eq!(restored.run.output_tool_name(), Some("final_result_1"));
    assert_eq!(
        restored
            .run
            .advertised_tools()
            .unwrap()
            .definitions
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["final_result", "final_result_1"]
    );
    let completion = RecordingCompletion {
        inner: super::RigCompletionPort::new(
            MockCompletionModel::from_turns([MockTurn::tool_call(
                "answer",
                "final_result_1",
                json!({"summary":"recovered"}),
            )])
            .erase(),
        ),
        requests: Mutex::new(Vec::new()),
    };
    let changed_tools = StructuredTools {
        name: "echo",
        calls: Mutex::new(0),
    };
    let outcome = super::NodeExecutor::execute_with_store(
        &mut restored,
        &completion,
        &changed_tools,
        &store,
        "output-name",
        &request.cancellation,
        &request.routes,
    )
    .await
    .unwrap();
    assert_eq!(outcome.submission, "recovered");
    assert_eq!(
        completion.requests.lock().unwrap()[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["echo", "final_result_1"]
    );
    assert_eq!(*changed_tools.calls.lock().unwrap(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn structured_output_still_rejects_an_illegal_route() {
    let request = structured_node_request(&["accept", "revise"]);
    let mut checkpoint = AgentCheckpoint::from_request("work", 1, &request);
    let completion = super::RigCompletionPort::new(
        MockCompletionModel::from_turns([MockTurn::tool_call(
            "answer",
            "final_result",
            json!({"summary":"done","route":"unauthorized"}),
        )])
        .erase(),
    );
    let error = super::NodeExecutor::execute(
        &mut checkpoint,
        &completion,
        &EchoTools,
        &request.cancellation,
        &request.routes,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, super::NodeError::InvalidResult(reason) if reason.contains("not allowed"))
    );
}

#[test]
fn file_checkpoint_store_round_trips_and_rejects_path_escape() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("anchor-rig-checkpoint-{unique}"));
    let store = super::FileCheckpointStore::new(&root);
    let checkpoint = AgentCheckpoint::start("review", 9, "persist", 2);
    store
        .save("execution-9", &checkpoint)
        .expect("save checkpoint");
    let restored = store
        .load("execution-9", "review", 9)
        .expect("load checkpoint")
        .expect("checkpoint exists");
    assert_eq!(restored.node_id, "review");
    assert!(
        store
            .load("missing", "review", 9)
            .expect("missing is normal")
            .is_none()
    );
    assert!(matches!(
        store.save("../escape", &checkpoint),
        Err(super::CheckpointStoreError::InvalidKey(_))
    ));
    store.delete("execution-9").expect("delete checkpoint");
    assert!(
        store
            .load("execution-9", "review", 9)
            .expect("deleted is missing")
            .is_none()
    );
    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[tokio::test]
async fn node_executor_persists_pending_model_before_provider_failure() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("anchor-rig-provider-failure-{unique}"));
    let store = super::FileCheckpointStore::new(&root);
    let model = MockCompletionModel::from_turns([MockTurn::error("provider fixture failure")]);
    let port = super::RigCompletionPort::new(model.erase());
    let mut checkpoint = AgentCheckpoint::start("review", 10, "fail", 1);
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error = super::NodeExecutor::execute_with_store(
        &mut checkpoint,
        &port,
        &EchoTools,
        &store,
        "execution-10",
        &cancellation,
        &[],
    )
    .await
    .expect_err("provider failure should be returned");
    assert!(error.to_string().contains("provider"));
    let mut restored = store
        .load("execution-10", "review", 10)
        .expect("load persisted pending model")
        .expect("pending checkpoint exists");
    assert!(matches!(
        restored.pending_step(),
        Some(AgentRunStep::CallModel { turn: 1, .. })
    ));
    let recovery_port = super::RigCompletionPort::new(
        MockCompletionModel::text(r#"{"summary":"recovered"}"#).erase(),
    );
    let outcome = super::NodeExecutor::execute_with_store(
        &mut restored,
        &recovery_port,
        &EchoTools,
        &store,
        "execution-10",
        &cancellation,
        &[],
    )
    .await
    .expect("resume pending model");
    assert_eq!(outcome.submission, "recovered");
    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[tokio::test]
async fn node_executor_persists_pending_tools_after_tool_failure() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("anchor-rig-tool-failure-{unique}"));
    let store = super::FileCheckpointStore::new(&root);
    let model = MockCompletionModel::from_turns([MockTurn::tool_call(
        "echo-1",
        "echo",
        json!({"value":"fixture"}),
    )]);
    let port = super::RigCompletionPort::new(model.erase());
    let mut checkpoint = AgentCheckpoint::start("review", 11, "use tool", 2);
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error = super::NodeExecutor::execute_with_store(
        &mut checkpoint,
        &port,
        &FailingTools,
        &store,
        "execution-11",
        &cancellation,
        &[],
    )
    .await
    .expect_err("tool failure should be returned");
    assert!(error.to_string().contains("tool"));
    let mut restored = store
        .load("execution-11", "review", 11)
        .expect("load pending tools")
        .expect("pending tool checkpoint exists");
    assert!(matches!(
        restored.pending_step(),
        Some(AgentRunStep::CallTools { calls }) if calls.len() == 1
    ));
    let recovery_port = super::RigCompletionPort::new(
        MockCompletionModel::text(r#"{"summary":"tool recovered"}"#).erase(),
    );
    let outcome = super::NodeExecutor::execute_with_store(
        &mut restored,
        &recovery_port,
        &EchoTools,
        &store,
        "execution-11",
        &cancellation,
        &[],
    )
    .await
    .expect("resume pending tools");
    assert_eq!(outcome.submission, "tool recovered");
    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[tokio::test]
async fn node_executor_timeout_keeps_pending_model_step() {
    let mut checkpoint = AgentCheckpoint::start("review", 12, "slow", 1);
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error = super::NodeExecutor::execute_with_policy(
        &mut checkpoint,
        &SlowCompletion,
        &EchoTools,
        ExecutionPolicy {
            model_timeout: Some(std::time::Duration::from_millis(1)),
            tool_timeout: None,
        },
        &cancellation,
        &[],
    )
    .await
    .expect_err("slow model should time out");
    assert!(error.to_string().contains("model request timed out"));
    assert!(matches!(
        checkpoint.pending_step(),
        Some(AgentRunStep::CallModel { turn: 1, .. })
    ));
}

#[tokio::test]
async fn node_executor_recovers_persisted_model_timeout_with_new_provider() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("anchor-rig-timeout-recovery-{unique}"));
    let store = super::FileCheckpointStore::new(&root);
    let mut checkpoint = AgentCheckpoint::start("review", 13, "slow provider", 1);
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error = super::NodeExecutor::execute_with_store_and_policy(
        &mut checkpoint,
        &SlowCompletion,
        &EchoTools,
        &store,
        "execution-13",
        ExecutionPolicy {
            model_timeout: Some(std::time::Duration::from_millis(1)),
            tool_timeout: None,
        },
        &cancellation,
        &[],
    )
    .await
    .expect_err("slow model should time out");
    assert!(error.to_string().contains("model request timed out"));

    let mut restored = store
        .load("execution-13", "review", 13)
        .expect("load checkpoint")
        .expect("persisted checkpoint exists");
    assert!(matches!(
        restored.pending_step(),
        Some(AgentRunStep::CallModel { turn: 1, .. })
    ));

    let recovery_port = super::RigCompletionPort::new(
        MockCompletionModel::text(r#"{"summary":"recovered after timeout"}"#).erase(),
    );
    let outcome = super::NodeExecutor::execute(
        &mut restored,
        &recovery_port,
        &EchoTools,
        &cancellation,
        &[],
    )
    .await
    .expect("resume pending model request with a newly bound provider");
    assert_eq!(outcome.submission, "recovered after timeout");
    assert!(restored.pending_step().is_none());
    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[tokio::test]
async fn streaming_port_emits_events_and_returns_complete_response() {
    let model = MockCompletionModel::from_stream_turns([[
        MockStreamEvent::text("{"),
        MockStreamEvent::text("\"summary\":\"streamed\"}"),
        MockStreamEvent::final_response_with_default_usage(),
    ]]);
    let port = super::RigCompletionPort::new(model.erase());
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut chunks = Vec::new();
    let response = super::stream_completion(
        &port,
        rig_agent::core::completion::CompletionRequest::new("stream"),
        &cancellation,
        None,
        |event| {
            if let rig_agent::core::streaming::StreamEvent::Text { text, .. } = event {
                chunks.push(text.clone());
            }
        },
    )
    .await
    .expect("stream should finish");
    assert_eq!(chunks.concat(), "{\"summary\":\"streamed\"}");
    assert_eq!(response.choice.len(), 1);
}

#[tokio::test]
async fn streaming_port_honors_cancellation_without_committing_response() {
    let model = MockCompletionModel::from_stream_turns([[
        MockStreamEvent::text("partial"),
        MockStreamEvent::text("never committed"),
    ]]);
    let port = super::RigCompletionPort::new(model.erase());
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let error = super::stream_completion(
        &port,
        rig_agent::core::completion::CompletionRequest::new("stream"),
        &cancellation,
        None,
        |_| {},
    )
    .await
    .expect_err("cancelled stream");
    assert!(matches!(error, super::StreamingError::Cancelled));
}

#[tokio::test]
async fn interrupted_stream_keeps_checkpoint_pending_for_provider_recovery() {
    let mut checkpoint = AgentCheckpoint::start("review", 14, "stream then recover", 1);
    let step = checkpoint.run.next_step().expect("pending model step");
    let AgentRunStep::CallModel {
        prompt, turn: 1, ..
    } = &step
    else {
        panic!("first Rig step should call the model");
    };
    let prompt = prompt.clone();
    checkpoint.pending_step = Some(step);
    let encoded = checkpoint.encode().expect("encode interrupted checkpoint");
    let mut restored = AgentCheckpoint::decode(&encoded, "review", 14).expect("reload");

    let model = MockCompletionModel::from_stream_turns([[
        MockStreamEvent::text("partial response"),
        MockStreamEvent::text(" must not be committed"),
        MockStreamEvent::final_response_with_default_usage(),
    ]]);
    let port = super::RigCompletionPort::new(model.erase());
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut observed = Vec::new();
    let error = super::stream_completion(
        &port,
        rig_agent::core::completion::CompletionRequest::new(prompt),
        &cancellation,
        None,
        |event| {
            if let rig_agent::core::streaming::StreamEvent::Text { text, .. } = event {
                observed.push(text.clone());
                cancellation.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        },
    )
    .await
    .expect_err("host cancellation should discard a partial provider response");
    assert!(matches!(error, super::StreamingError::Cancelled));
    assert_eq!(observed.concat(), "partial response");
    assert!(matches!(
        restored.pending_step(),
        Some(AgentRunStep::CallModel { turn: 1, .. })
    ));

    let recovery_port = super::RigCompletionPort::new(
        MockCompletionModel::text(r#"{"summary":"recovered after stream interruption"}"#).erase(),
    );
    let recovery_cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let outcome = super::NodeExecutor::execute(
        &mut restored,
        &recovery_port,
        &EchoTools,
        &recovery_cancellation,
        &[],
    )
    .await
    .expect("resume interrupted model step through a fresh provider binding");
    assert_eq!(outcome.submission, "recovered after stream interruption");
    assert!(restored.pending_step().is_none());
}

#[tokio::test]
async fn observed_completion_port_records_only_request_outcomes() {
    let model = MockCompletionModel::text(r#"{"summary":"observed"}"#);
    let base = super::RigCompletionPort::new(model.erase());
    let observer = std::sync::Arc::new(RecordingObserver::default());
    let port = super::ObservedCompletionPort::new(base, "fixture-provider", observer.clone());
    let response = port
        .complete(rig_agent::core::completion::CompletionRequest::new(
            "observe",
        ))
        .await
        .expect("completion");
    assert_eq!(response.choice.len(), 1);
    assert_eq!(
        observer.events.lock().expect("observer lock").as_slice(),
        ["start:fixture-provider", "finish:fixture-provider"]
    );
}
