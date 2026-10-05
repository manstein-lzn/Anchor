//! Native output-tool protocol at the provider boundary. Harness still owns
//! validation, correction turns, completion persistence and recovery.

use io_harness::{CompletionResponse, schema::OutputSchema};
use rig_core::{completion::ToolDefinition, completion::message::ToolName};
use serde_json::{Map, Value, json};

pub(crate) const TOOL_NAME: &str = "final_result";

#[cfg(test)]
mod tests;

pub(crate) fn definition(schema: &OutputSchema) -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new(TOOL_NAME.to_owned()).expect("static valid completion tool name"),
        "Submit the completed node with a summary and, when required, one allowed route. Call this tool alone after inspecting all business tool results. Extra fields are ignored.",
        schema.as_value().clone(),
    )
}

/// A completion call has no business effect to dispatch. A mixed response
/// retains every business call; its premature completion is recorded as
/// deferred, never carried forward as a pending successful result.
pub(crate) fn response(mut response: CompletionResponse) -> CompletionResponse {
    let mut completions = Vec::new();
    response.tool_calls.retain(|call| {
        if call.name == TOOL_NAME {
            completions.push(call.clone());
            false
        } else {
            true
        }
    });
    if completions.is_empty() && !response.tool_calls.is_empty() {
        return response;
    }

    let original_text = response.text.take();
    let mut output = Map::new();
    let status = if !response.tool_calls.is_empty() {
        "deferred"
    } else if completions.len() == 1
        && !matches!(
            response.finish_reason.as_deref(),
            Some("length" | "content_filter" | "pause_turn")
        )
    {
        if let Some(arguments) = completions[0].arguments.as_object() {
            for name in ["summary", "route"] {
                if let Some(value) = arguments.get(name) {
                    // Python permits an omitted/null route and ignores extra
                    // arguments. Blank summaries must reach Harness correction.
                    if name == "route" && value.is_null()
                        || name == "summary" && value.as_str().is_some_and(|s| s.trim().is_empty())
                    {
                        continue;
                    }
                    output.insert(name.into(), value.clone());
                }
            }
        }
        "submitted"
    } else {
        "rejected"
    };

    // This is a projection in the existing Harness completion/turn text, not
    // a new journal. Keep received calls for inspection and live acceptance.
    output.insert(
        "_anchor_completion".into(),
        json!({
            "status": status, "calls": completions, "assistant_text": original_text,
        }),
    );
    if status != "submitted" {
        output.insert("instruction".into(), json!(
            "Completion was not accepted. Inspect all business tool results, then call final_result exactly once and alone. Plain text, including JSON text, is not a completion call."
        ));
    }
    // Only a genuine lone completion call can supply summary/route. For all
    // other quiet turns the missing summary invokes Harness's existing retry.
    response.text = Some(Value::Object(output).to_string());
    response
}
