//! Experimental Anchor runtime kernel using Rig's Rust agent runtime.
//!
//! This crate is a feasibility slice, not yet the authoritative Anchor runtime.

use rig_agent::{core::completion::Message, run::AgentRun};
use serde::{Deserialize, Serialize};

const CHECKPOINT_FORMAT: u32 = 1;

/// Durable identity and Rig state for one experimental AgentNode attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCheckpoint {
    format: u32,
    pub node_id: String,
    pub invocation: u32,
    pub run: AgentRun,
}

impl AgentCheckpoint {
    /// Start a Rig run bound to an Anchor node invocation.
    pub fn start(
        node_id: impl Into<String>,
        invocation: u32,
        task: impl Into<String>,
        max_turns: usize,
    ) -> Self {
        Self {
            format: CHECKPOINT_FORMAT,
            node_id: node_id.into(),
            invocation,
            run: AgentRun::new(Message::user(task.into())).max_turns(max_turns),
        }
    }

    /// Serialize the checkpoint using Rig's own versioned AgentRun envelope.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Restore a checkpoint only for the exact Anchor node invocation.
    pub fn decode(
        bytes: &[u8],
        expected_node: &str,
        expected_invocation: u32,
    ) -> Result<Self, CheckpointError> {
        let checkpoint: Self = serde_json::from_slice(bytes)?;
        if checkpoint.format != CHECKPOINT_FORMAT {
            return Err(CheckpointError::UnsupportedFormat(checkpoint.format));
        }
        if checkpoint.node_id != expected_node || checkpoint.invocation != expected_invocation {
            return Err(CheckpointError::IdentityMismatch);
        }
        Ok(checkpoint)
    }
}

/// A checkpoint could not be parsed or did not belong to the requested node attempt.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    /// The saved run did not use this wrapper's format.
    #[error("unsupported Anchor checkpoint format: {0}")]
    UnsupportedFormat(u32),
    /// The checkpoint was saved for a different node or invocation.
    #[error("checkpoint identity does not match the requested node invocation")]
    IdentityMismatch,
    /// JSON or Rig's own run-state format could not be decoded.
    #[error(transparent)]
    Decode(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

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

    use super::{AgentCheckpoint, CheckpointError};

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
        let mut restored = AgentCheckpoint::decode(&encoded, "research", 3)
            .expect("restore matching node invocation");
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
        let AgentRunStep::CallTools { calls } =
            restored.run.next_step().expect("replay pending step")
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
}
