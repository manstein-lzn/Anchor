//! Narrow, explicit conversion between io-harness 0.86 and Rig 0.43.
//!
//! This crate owns data conversion only. It does not run either agent loop,
//! execute tools, persist runs, or grant host permissions. A future runtime
//! adapter must call io-harness as the sole loop owner and use this boundary for
//! model/provider calls.

use futures_util::StreamExt;
use io_harness::{
    CompletionRequest as IoRequest, CompletionResponse as IoResponse, Message as IoMessage,
    ToolCall as IoToolCall, ToolSpec as IoToolSpec,
};
use rig_core::DynModel;
use rig_core::completion::message::{
    CallId, ImageMediaType, ToolCall as RigToolCall, ToolFunction, ToolName, ToolResult,
    ToolResultContent, UserContent,
};
use rig_core::completion::{
    AssistantContent, CompletionRequest as RigRequest, CompletionResponse as RigResponse,
    Message as RigMessage, ToolDefinition as RigToolDefinition,
};
use rig_core::operation::Completion;
use rig_core::streaming::{Item, StreamEvent};
use thiserror::Error;

fn rig_provider_error(error: impl std::fmt::Display) -> io_harness::Error {
    // Rig's normalized error does not expose a lossless mapping to io-harness's
    // retry taxonomy at this boundary. Treat unknown failures as non-retryable
    // until a provider-specific mapping is proven; this avoids duplicating a
    // request whose external outcome is not known.
    io_harness::Error::provider(
        io_harness::ProviderErrorKind::Request,
        format!("Rig provider adapter: {error}"),
    )
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConversionError {
    #[error("tool name cannot be empty: {0:?}")]
    EmptyToolName(String),
    #[error("message {message} has a tool result index {call} without a preceding call")]
    ToolResultWithoutCall { message: usize, call: usize },
    #[error("io-harness media type is unsupported by Rig: {0}")]
    UnsupportedMedia(String),
    #[error("Rig response contains unsupported assistant content")]
    UnsupportedAssistantContent,
}

/// A Provider implementation that lets io-harness own the loop while Rig owns
/// the model transport. Text deltas are forwarded incrementally; reasoning and
/// tool argument deltas remain internal to Rig and are represented only by the
/// final response conversion.
#[derive(Clone)]
pub struct RigProviderAdapter {
    model: DynModel<Completion>,
    accepts_images: bool,
}

impl RigProviderAdapter {
    pub fn new(model: DynModel<Completion>, accepts_images: bool) -> Self {
        Self {
            model,
            accepts_images,
        }
    }
}

impl io_harness::Provider for RigProviderAdapter {
    async fn complete(&self, request: IoRequest) -> io_harness::Result<IoResponse> {
        let request = to_rig_request(&request)
            .map_err(|error| io_harness::Error::Config(error.to_string()))?;
        let response = self.model.call(request).await.map_err(rig_provider_error)?;
        from_rig_response(&response).map_err(|error| io_harness::Error::Config(error.to_string()))
    }

    async fn complete_streaming(
        &self,
        request: IoRequest,
        on_token: &(dyn Fn(&str) + Send + Sync),
    ) -> io_harness::Result<IoResponse> {
        let request = to_rig_request(&request)
            .map_err(|error| io_harness::Error::Config(error.to_string()))?;
        let mut stream = self.model.stream(request).map_err(rig_provider_error)?;
        while let Some(item) = stream.next().await {
            match item {
                Ok(Item::Event(StreamEvent::Text { text, .. })) => on_token(&text),
                Ok(Item::Event(_)) | Ok(Item::Unknown(_)) => {}
                Err(error) => {
                    return Err(rig_provider_error(error));
                }
            }
        }
        let response = stream.finish().await.map_err(rig_provider_error)?;
        from_rig_response(&response).map_err(|error| io_harness::Error::Config(error.to_string()))
    }

    fn name(&self) -> &str {
        self.model.name()
    }

    fn accepts_images(&self) -> bool {
        self.accepts_images
    }
}

fn tool_name(name: &str) -> Result<ToolName, ConversionError> {
    ToolName::new(name.to_owned()).map_err(|_| ConversionError::EmptyToolName(name.to_owned()))
}

fn stable_call_id(message: usize, call: usize) -> CallId {
    CallId::from_wire(format!("io-{message:08x}-{call:08x}"))
}

fn rig_media_type(media_type: &str) -> Result<ImageMediaType, ConversionError> {
    match media_type {
        "image/jpeg" => Ok(ImageMediaType::JPEG),
        "image/png" => Ok(ImageMediaType::PNG),
        "image/gif" => Ok(ImageMediaType::GIF),
        "image/webp" => Ok(ImageMediaType::WEBP),
        other => Err(ConversionError::UnsupportedMedia(other.to_owned())),
    }
}

fn rig_tools(tools: &[IoToolSpec]) -> Result<Vec<RigToolDefinition>, ConversionError> {
    tools
        .iter()
        .map(|tool| {
            Ok(RigToolDefinition::new(
                tool_name(&tool.name)?,
                tool.description.clone(),
                tool.parameters.clone(),
            ))
        })
        .collect()
}

/// Convert an io-harness request while preserving role-tagged text, tool calls,
/// positional results, schemas, and the supported image media types.
pub fn to_rig_request(request: &IoRequest) -> Result<RigRequest, ConversionError> {
    let mut history = Vec::new();
    if !request.system.is_empty() {
        history.push(RigMessage::system(request.system.clone()));
    }
    let mut last_calls: Vec<(CallId, ToolName)> = Vec::new();
    for (message_index, message) in request.messages.iter().enumerate() {
        match message {
            IoMessage::User(text) => {
                history.push(RigMessage::user(text.clone()));
                last_calls.clear();
            }
            IoMessage::Assistant { text, calls } => {
                let mut content = Vec::new();
                if let Some(text) = text {
                    content.push(AssistantContent::text(text.clone()));
                }
                last_calls.clear();
                for (call_index, call) in calls.iter().enumerate() {
                    let name = tool_name(&call.name)?;
                    let id = stable_call_id(message_index, call_index);
                    content.push(AssistantContent::ToolCall(RigToolCall::new(
                        id.clone(),
                        ToolFunction::new(name.clone(), call.arguments.clone()),
                    )));
                    last_calls.push((id, name));
                }
                if content.is_empty() {
                    return Err(ConversionError::UnsupportedAssistantContent);
                }
                history.push(RigMessage::Assistant { id: None, content });
            }
            IoMessage::Results(results) => {
                let mut converted = Vec::with_capacity(results.len());
                for result in results {
                    let Some((call, name)) = last_calls.get(result.call) else {
                        return Err(ConversionError::ToolResultWithoutCall {
                            message: message_index,
                            call: result.call,
                        });
                    };
                    converted.push(ToolResult {
                        call: call.clone(),
                        name: name.clone(),
                        content: vec![ToolResultContent::text(result.content.clone())],
                    });
                }
                history.push(RigMessage::tool_results(converted));
            }
        }
    }
    // `system` is not a user turn. When io-harness uses its legacy flat path,
    // preserve the user prompt even if a system instruction was also present.
    if request.messages.is_empty() {
        history.push(RigMessage::user(request.user.clone()));
    }
    let last = history.pop().expect("history is non-empty");
    let mut converted = RigRequest::new(last).messages(history);
    converted = converted.tools(rig_tools(&request.tools)?);
    // Keep output validation in io-harness. Some OpenAI-compatible providers
    // reject response_format; the Harness still validates the declared schema
    // locally and feeds violations back through its normal correction turn.
    if let Some(model) = &request.model {
        converted = converted.model(model.clone());
    }
    {
        let mut media = Vec::new();
        for item in &request.media {
            let kind = rig_media_type(&item.media_type)?;
            media.push(UserContent::image_base64(
                item.base64.clone(),
                Some(kind),
                None,
            ));
        }
        if !media.is_empty() {
            let target = converted.chat_history.iter_mut().rev().find_map(|message| {
                let RigMessage::User { content } = message else {
                    return None;
                };
                content
                    .iter()
                    .any(|item| matches!(item, UserContent::Text(_)))
                    .then_some(content)
            });
            if let Some(content) = target {
                content.extend(media);
            } else {
                converted
                    .chat_history
                    .push(RigMessage::User { content: media });
            }
        }
    }
    Ok(converted)
}

/// Convert the subset of a Rig response that io-harness can represent without
/// inventing provider IDs or silently flattening rich assistant content.
pub fn from_rig_response(response: &RigResponse) -> Result<IoResponse, ConversionError> {
    let mut text = String::new();
    let mut calls = Vec::new();
    for part in &response.choice {
        match part {
            AssistantContent::Text(value) => text.push_str(&value.text),
            AssistantContent::ToolCall(call) => calls.push(IoToolCall {
                name: call.function.name.to_string(),
                arguments: call.function.arguments.clone(),
            }),
            AssistantContent::Reasoning(_) | AssistantContent::Image(_) => {
                return Err(ConversionError::UnsupportedAssistantContent);
            }
        }
    }
    let usage = response.usage;
    Ok(IoResponse {
        text: (!text.is_empty()).then_some(text),
        tool_calls: calls,
        usage: usage.is_reported().then(|| io_harness::Usage {
            prompt_tokens: usage.input_tokens.unwrap_or_default(),
            completion_tokens: usage.output_tokens.unwrap_or_default(),
            total_tokens: usage.total_tokens.unwrap_or_default(),
            cache_read_tokens: usage.cached_input_tokens.unwrap_or_default(),
            reasoning_tokens: usage.reasoning_tokens.unwrap_or_default(),
            ..Default::default()
        }),
        model: response.model.clone(),
        finish_reason: response.finish_reason().map(|reason| format!("{reason:?}")),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use io_harness::{Message, ToolResult};
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
    use serde_json::json;

    #[test]
    fn request_preserves_text_tools_images_and_positional_results() {
        let request = IoRequest {
            system: "system".into(),
            user: "ignored when messages exist".into(),
            messages: vec![
                Message::User("goal".into()),
                Message::Assistant {
                    text: Some("using tool".into()),
                    calls: vec![IoToolCall {
                        name: "lookup".into(),
                        arguments: json!({"id": 7}),
                    }],
                },
                Message::Results(vec![ToolResult {
                    call: 0,
                    content: r#"{"ok":true}"#.into(),
                }]),
            ],
            tools: vec![IoToolSpec {
                name: "lookup".into(),
                description: "lookup one item".into(),
                parameters: json!({"type":"object"}),
            }],
            media: vec![io_harness::Media::image("image/png", b"png-bytes").unwrap()],
            ..Default::default()
        };
        let converted = to_rig_request(&request).unwrap();
        assert_eq!(converted.system_instructions(), Some("system"));
        assert_eq!(converted.tools[0].name, "lookup");
        assert!(matches!(converted.chat_history[3], RigMessage::User { .. }));
        let encoded = serde_json::to_string(&converted).unwrap();
        assert!(encoded.contains("io-00000001-00000000"));
        assert!(encoded.contains("cG5nLWJ5dGVz"));
    }

    #[test]
    fn invalid_result_position_and_empty_tool_name_fail_closed() {
        let bad_result = IoRequest {
            messages: vec![Message::Results(vec![ToolResult {
                call: 0,
                content: "x".into(),
            }])],
            ..Default::default()
        };
        assert!(matches!(
            to_rig_request(&bad_result),
            Err(ConversionError::ToolResultWithoutCall { .. })
        ));
        let bad_tool = IoRequest {
            tools: vec![IoToolSpec {
                name: String::new(),
                description: String::new(),
                parameters: json!({}),
            }],
            ..Default::default()
        };
        assert!(matches!(
            to_rig_request(&bad_tool),
            Err(ConversionError::EmptyToolName(_))
        ));
    }

    #[test]
    fn flat_user_prompt_is_preserved_beside_system_instruction() {
        let request = IoRequest {
            system: "system guidance".into(),
            user: "the actual task".into(),
            ..Default::default()
        };
        let converted = to_rig_request(&request).unwrap();
        assert!(converted.chat_history.iter().any(|message| {
            matches!(message, RigMessage::User { content } if content.iter().any(|item| {
                matches!(item, UserContent::Text(text) if text.text == "the actual task")
            }))
        }));
    }

    #[test]
    fn output_schema_is_kept_out_of_provider_wire_request() {
        let schema = io_harness::schema::OutputSchema::new(json!({
            "type":"object",
            "properties":{"summary":{"type":"string"}},
            "required":["summary"],
            "additionalProperties":false
        }))
        .unwrap();
        let request = IoRequest {
            user: "complete the node".into(),
            output_schema: Some(schema.clone()),
            ..Default::default()
        };

        let converted = to_rig_request(&request).unwrap();
        assert!(converted.output_schema.is_none());
    }

    #[test]
    fn response_maps_text_json_tool_calls_and_usage() {
        let name = tool_name("lookup").unwrap();
        let response = RigResponse::new(
            vec![
                AssistantContent::text("answer"),
                AssistantContent::ToolCall(RigToolCall::new(
                    CallId::from_wire("provider-call-id"),
                    ToolFunction::new(name, json!({"id": 7})),
                )),
            ],
            rig_core::completion::Usage {
                input_tokens: Some(10),
                output_tokens: Some(3),
                total_tokens: Some(13),
                ..Default::default()
            },
            "rig-test",
            json!({}),
        );
        let converted = from_rig_response(&response).unwrap();
        assert_eq!(converted.text.as_deref(), Some("answer"));
        assert_eq!(converted.tool_calls[0].name, "lookup");
        assert_eq!(converted.tool_calls[0].arguments, json!({"id": 7}));
        assert_eq!(converted.usage.unwrap().total_tokens, 13);
    }

    #[test]
    fn rich_rig_assistant_content_is_rejected() {
        let response = RigResponse::new(
            vec![AssistantContent::Image(Default::default())],
            Default::default(),
            "rig-test",
            json!({}),
        );
        assert_eq!(
            from_rig_response(&response),
            Err(ConversionError::UnsupportedAssistantContent)
        );
    }

    #[tokio::test]
    async fn rig_provider_adapter_forwards_stream_text_deltas() {
        use std::sync::{Arc, Mutex};

        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("first "),
            MockStreamEvent::text("second"),
            MockStreamEvent::final_response(Default::default()),
        ]])
        .erase();
        let adapter = RigProviderAdapter::new(model, false);
        let seen = Arc::new(Mutex::new(String::new()));
        let captured = Arc::clone(&seen);
        let response = io_harness::Provider::complete_streaming(
            &adapter,
            IoRequest {
                user: "stream".into(),
                ..Default::default()
            },
            &move |chunk| captured.lock().unwrap().push_str(chunk),
        )
        .await
        .unwrap();
        assert_eq!(*seen.lock().unwrap(), "first second");
        assert_eq!(response.text.as_deref(), Some("first second"));
    }
}
