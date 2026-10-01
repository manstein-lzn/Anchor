use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use rig_agent::{
    AgentBuilder,
    core::{
        completion::Usage,
        message::{AssistantContent, ToolCall, ToolFunction, ToolName},
        schemars::JsonSchema,
    },
    run::{AgentRunStep, ModelTurn, ModelTurnOutcome},
    test_utils::{MockAddTool, MockCompletionModel, MockTurn},
};
use serde::Deserialize;
use serde_json::json;

use super::{AgentCheckpoint, CheckpointError, CheckpointStore, CompletionPort, ExecutionPolicy};

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
