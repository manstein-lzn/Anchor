//! Opt-in real-provider smoke for the A70 NodeRequest boundary.
//!
//! This binary uses Rig for the provider transport and io-harness for the only
//! model/tool loop. It writes no Anchor Graph/Run state.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anchor_io_harness_runtime::adapter::RigProviderAdapter;
use anchor_io_harness_runtime::node_exec::{IoHarnessNodeExecution, fixture_policy};
use anchor_runtime_rig::{NodeRequest, ToolError, ToolPort};
use rig_core::completion::ToolDefinition;
use rig_core::message::ToolResultContent;
use serde_json::json;

struct EchoPort {
    calls: AtomicUsize,
}

impl ToolPort for EchoPort {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "anchor_echo".into(),
            description:
                "Return the supplied value as JSON. Call this exactly once before answering.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
        }]
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if name != "anchor_echo" {
                return Err(ToolError::Unknown(name.to_owned()));
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![ToolResultContent::json(json!({
                "echo": arguments.get("value").cloned().unwrap_or(serde_json::Value::Null),
            }))])
        })
    }
}

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    std::env::var(name).map_err(|_| format!("missing {name}").into())
}

fn endpoint_host(base: &str) -> &str {
    base.split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base)
        .split('/')
        .next()
        .unwrap_or(base)
}

fn evidence_path() -> Option<PathBuf> {
    std::env::var_os("ANCHOR_IO_NODE_EXEC_EVIDENCE").map(PathBuf::from)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = required("ANCHOR_MODEL_API_KEY")?;
    let base_url = required("ANCHOR_MODEL_URL")?;
    let model_name = required("ANCHOR_MODEL_NAME")?;
    let wire_api = std::env::var("ANCHOR_MODEL_WIRE_API").unwrap_or_else(|_| "chat".into());
    let model = match wire_api.as_str() {
        "chat" => rig_core::providers::openai::OpenAIConfig::new(api_key)
            .with_base_url(base_url.clone())
            .client()
            .chat(&model_name)
            .erase(),
        "responses" => rig_core::providers::openai::OpenAIConfig::new(api_key)
            .with_base_url(base_url.clone())
            .client()
            .responses(&model_name)
            .erase(),
        other => return Err(format!("unsupported ANCHOR_MODEL_WIRE_API={other}").into()),
    };
    let provider = RigProviderAdapter::new(model, false);
    let workspace =
        std::env::temp_dir().join(format!("anchor-io-node-exec-live-{}", std::process::id()));
    std::fs::create_dir_all(&workspace)?;
    let store_path = workspace.join("io-harness.sqlite3");
    let cancellation = Arc::new(AtomicBool::new(false));
    let port = Arc::new(EchoPort {
        calls: AtomicUsize::new(0),
    });
    let request = NodeRequest {
        execution_id: "a70-live".into(),
        task: "Call anchor_echo exactly once with value a70-live, then report the echoed value.".into(),
        instructions: "Use the Anchor tool before answering. Return only JSON with summary and route=done. The summary must mention the echo result.".into(),
        routes: vec!["done".into()],
        max_turns: 6,
        workspace: workspace.clone(),
        cancellation: Arc::clone(&cancellation),
    };
    let policy = fixture_policy().allow_net(endpoint_host(&base_url));
    let execution = IoHarnessNodeExecution::new(store_path, policy);
    let outcome = execution.start(&request, &provider, port.clone()).await?;
    let evidence = json!({
        "status": "passed",
        "boundary": "NodeRequest -> Rig provider -> io-harness loop -> Anchor ToolPort -> NodeOutcome",
        "provider": "real configured OpenAI-compatible endpoint",
        "wire_api": wire_api,
        "model": model_name,
        "tool_calls": port.calls.load(Ordering::SeqCst),
        "node_status": format!("{:?}", outcome.status),
        "summary": outcome.submission,
        "route": outcome.route,
        "model_requests": outcome.model_requests,
    });
    if let Some(path) = evidence_path() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&evidence)?)?;
        println!("evidence={}", path.display());
    }
    println!("{}", serde_json::to_string_pretty(&evidence)?);
    let _ = std::fs::remove_dir_all(workspace);
    Ok(())
}
