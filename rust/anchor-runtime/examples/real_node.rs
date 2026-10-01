//! Minimal real-provider smoke test for the Rust AgentNode boundary.
//!
//! Required environment: ANCHOR_MODEL_API_KEY, ANCHOR_MODEL_URL,
//! ANCHOR_MODEL_NAME. Optional ANCHOR_MODEL_WIRE_API defaults to chat.

use std::{
    env,
    sync::{Arc, atomic::AtomicBool},
};

use anchor_runtime_rig::{
    AgentCheckpoint, Cancellation, NodeExecutor, NodeRequest, RigCompletionPort, ToolPort,
};

struct FixtureTool;
impl ToolPort for FixtureTool {
    fn definitions(&self) -> Vec<rig_agent::core::completion::ToolDefinition> {
        vec![rig_agent::core::completion::ToolDefinition::new(
            rig_agent::core::message::ToolName::new("fixture_tool").expect("tool name"),
            "Return the deterministic fixture string",
            serde_json::json!({"type":"object"}),
        )]
    }
    fn call<'a>(
        &'a self,
        name: &'a str,
        _arguments: serde_json::Value,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Vec<rig_agent::core::message::ToolResultContent>,
                        anchor_runtime_rig::ToolError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if name == "fixture_tool" {
                Ok(vec![rig_agent::core::message::ToolResultContent::text(
                    "fixture-ok",
                )])
            } else {
                Err(anchor_runtime_rig::ToolError::Unknown(name.to_owned()))
            }
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = env::var("ANCHOR_MODEL_API_KEY")?;
    let base_url = env::var("ANCHOR_MODEL_URL")?;
    let model = env::var("ANCHOR_MODEL_NAME")?;
    let wire_api = env::var("ANCHOR_MODEL_WIRE_API").unwrap_or_else(|_| "chat".to_owned());
    let request = NodeRequest {
        execution_id: "rust-smoke".to_owned(),
        task: "Call fixture_tool, then return a concise health check".to_owned(),
        instructions:
            "Use the fixture_tool before answering. Return exactly JSON with summary and no route"
                .to_owned(),
        routes: Vec::new(),
        max_turns: 2,
        workspace: env::current_dir()?,
        cancellation: Arc::new(AtomicBool::new(false)),
    };
    let cancellation: Cancellation = request.cancellation.clone();
    let mut checkpoint = AgentCheckpoint::from_request("rust-smoke", 1, &request);
    let provider = RigCompletionPort::openai_compatible(api_key, base_url, model, &wire_api)?;
    let outcome =
        NodeExecutor::execute(&mut checkpoint, &provider, &FixtureTool, &cancellation, &[]).await?;
    println!(
        "status={:?} requests={} summary={}",
        outcome.status, outcome.model_requests, outcome.submission
    );
    Ok(())
}
