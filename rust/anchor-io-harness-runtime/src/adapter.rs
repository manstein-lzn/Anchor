//! Narrow, explicit conversion between io-harness 0.86 and Rig 0.43.
//!
//! This crate owns data conversion only. It does not run either agent loop,
//! execute tools, persist runs, or grant host permissions. The host calls
//! io-harness as the sole loop owner and uses this boundary for model/provider
//! calls.

use futures_util::StreamExt;
use io_harness::{
    CompletionRequest as IoRequest, CompletionResponse as IoResponse, Message as IoMessage,
    ToolCall as IoToolCall, ToolSpec as IoToolSpec,
};
use rig_core::DynModel;
use rig_core::completion::message::{
    CallId, ImageMediaType, Reasoning, ReasoningContent, ToolCall as RigToolCall, ToolFunction,
    ToolName, ToolResult, ToolResultContent, UserContent,
};
use rig_core::completion::{
    AssistantContent, CompletionRequest as RigRequest, CompletionResponse as RigResponse,
    FinishReason, Message as RigMessage, ToolDefinition as RigToolDefinition,
};
use rig_core::operation::Completion;
use rig_core::streaming::{Item, StreamEvent};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;

fn rig_provider_error(error: rig_core::error::ProviderError) -> io_harness::Error {
    // Preserve Rig's explicit retry verdict. In particular, transport/body
    // decode failures are retryable in Rig 0.43; mapping them to Request makes
    // io-harness escalate without using its native bounded retry policy.
    let kind = if error.is_retryable() {
        io_harness::ProviderErrorKind::Transport
    } else {
        io_harness::ProviderErrorKind::Request
    };
    io_harness::Error::provider(kind, format!("Rig provider adapter: {error}"))
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConversionError {
    #[error("tool name `final_result` is reserved for Anchor node completion")]
    ReservedCompletionTool,
    #[error("tool name cannot be empty: {0:?}")]
    EmptyToolName(String),
    #[error("message {message} has a tool result index {call} without a preceding call")]
    ToolResultWithoutCall { message: usize, call: usize },
    #[error("io-harness media type is unsupported by Rig: {0}")]
    UnsupportedMedia(String),
    #[error("Rig response contains unsupported assistant content")]
    UnsupportedAssistantContent,
    #[error("Rig reasoning content has no representable text (encrypted/redacted only)")]
    UnsupportedReasoning,
}

/// A Provider implementation that lets io-harness own the loop while Rig owns
/// the model transport. Plain requests forward text deltas incrementally. Anchor
/// completion requests buffer the stream and emit the canonical projection once
/// the full tool call arrives; partial arguments and prose cannot signal completion.
#[derive(Clone)]
pub struct RigProviderAdapter {
    model: DynModel<Completion>,
    accepts_images: bool,
    recording_root: Option<std::path::PathBuf>,
    call_ids: Arc<Mutex<CallIdLedger>>,
}

/// io-harness intentionally models tool calls without provider IDs. Responses
/// APIs require the provider's `call_id` to survive into the next request, so
/// this small ledger bridges that representation gap. The ordinal is the
/// position of a call in the flattened io-harness transcript; it is stable
/// while the native Harness run grows its history.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct CallIdLedger {
    assigned: BTreeMap<usize, String>,
    pending: Vec<PendingCallId>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PendingCallId {
    name: String,
    arguments: serde_json::Value,
    id: String,
}

impl CallIdLedger {
    fn load(root: &std::path::Path) -> Self {
        std::fs::read(crate::recording::call_ids_path(root))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, root: Option<&std::path::Path>) {
        let Some(root) = root else { return };
        let Ok(bytes) = serde_json::to_vec_pretty(self) else {
            return;
        };
        let path = crate::recording::call_ids_path(root);
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        if std::fs::write(&temporary, bytes).is_ok() {
            let _ = std::fs::rename(temporary, path);
        }
    }

    fn id_for(
        &mut self,
        ordinal: usize,
        message: usize,
        call: usize,
        name: &ToolName,
        arguments: &serde_json::Value,
    ) -> CallId {
        if let Some(id) = self.assigned.get(&ordinal) {
            return CallId::from_wire(id.clone());
        }
        if let Some(pending) = self
            .pending
            .first()
            .filter(|pending| pending.name == name.as_ref() && pending.arguments == *arguments)
            .cloned()
        {
            self.pending.remove(0);
            self.assigned.insert(ordinal, pending.id.clone());
            return CallId::from_wire(pending.id);
        }
        let id = stable_call_id(message, call);
        self.assigned.insert(ordinal, id.wire().into_owned());
        id
    }

    fn observe(&mut self, response: &RigResponse, completion: bool) {
        self.pending = response
            .choice
            .iter()
            .filter_map(|part| {
                let AssistantContent::ToolCall(call) = part else {
                    return None;
                };
                if completion && call.function.name.as_ref() == crate::completion::TOOL_NAME {
                    return None;
                }
                Some(PendingCallId {
                    name: call.function.name.to_string(),
                    arguments: call.function.arguments.clone(),
                    id: call.id.wire().into_owned(),
                })
            })
            .collect();
    }
}

impl RigProviderAdapter {
    pub fn new(model: DynModel<Completion>, accepts_images: bool) -> Self {
        Self {
            model,
            accepts_images,
            recording_root: None,
            call_ids: Arc::new(Mutex::new(CallIdLedger::default())),
        }
    }

    /// Bind private append-only observations to this invocation. The recorder
    /// stores typed requests/responses, never provider credentials or HTTP bytes.
    pub fn with_recording(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        let root = root.into();
        let ledger = CallIdLedger::load(&root);
        self.recording_root = Some(root);
        self.call_ids = Arc::new(Mutex::new(ledger));
        self
    }

    async fn exchange(
        &self,
        request: IoRequest,
        on_token: Option<&(dyn Fn(&str) + Send + Sync)>,
    ) -> io_harness::Result<IoResponse> {
        let completion = request.output_schema.is_some();
        let attempt = self
            .recording_root
            .as_ref()
            .map(|root| crate::recording::Attempt::begin(root, &request))
            .transpose()?;
        // Record sees the response before Anchor's completion projection. Its
        // native exchange retains final_result calls instead of the internal value.
        let transport = RigExchange {
            adapter: self,
            attempt: attempt.as_ref(),
            on_token,
            completion,
        };
        let result = if let Some(attempt) = &attempt {
            let provider = io_harness::provider::Record::new(transport);
            let result = io_harness::Provider::complete(&provider, request).await;
            match &result {
                Ok(_) => {
                    // Recording failures after a response cannot change the
                    // executed turn or cause Harness to retry its effects.
                    let saved = attempt.save(&provider);
                    let _ = attempt.outcome(
                        if saved.is_ok() {
                            "succeeded"
                        } else {
                            "recording_incomplete"
                        },
                        saved.as_ref().err(),
                    );
                }
                Err(error) => {
                    let _ = attempt.outcome("failed", Some(error));
                }
            }
            result
        } else {
            io_harness::Provider::complete(&transport, request).await
        };
        let response = result?;
        if completion {
            let response = crate::completion::response(response);
            if let Some(on_token) = on_token
                && let Some(text) = &response.text
            {
                on_token(text);
            }
            Ok(response)
        } else {
            Ok(response)
        }
    }
}

impl io_harness::Provider for RigProviderAdapter {
    async fn complete(&self, request: IoRequest) -> io_harness::Result<IoResponse> {
        self.exchange(request, None).await
    }

    async fn complete_streaming(
        &self,
        request: IoRequest,
        on_token: &(dyn Fn(&str) + Send + Sync),
    ) -> io_harness::Result<IoResponse> {
        self.exchange(request, Some(on_token)).await
    }

    fn name(&self) -> &str {
        self.model.name()
    }

    fn accepts_images(&self) -> bool {
        self.accepts_images
    }
}

struct RigExchange<'a> {
    adapter: &'a RigProviderAdapter,
    attempt: Option<&'a crate::recording::Attempt>,
    on_token: Option<&'a (dyn Fn(&str) + Send + Sync)>,
    completion: bool,
}

impl io_harness::Provider for RigExchange<'_> {
    async fn complete(&self, request: IoRequest) -> io_harness::Result<IoResponse> {
        let request = to_rig_request_with_ids(&request, &self.adapter.call_ids)
            .map_err(|error| io_harness::Error::Config(error.to_string()))?;
        if let Some(attempt) = self.attempt {
            attempt.rig_request(&request)?;
        }
        let response = if let Some(on_token) = self.on_token {
            let mut stream = self
                .adapter
                .model
                .stream(request)
                .map_err(rig_provider_error)?;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(Item::Event(StreamEvent::Text { text, .. })) if !self.completion => {
                        on_token(&text)
                    }
                    Ok(Item::Event(_)) | Ok(Item::Unknown(_)) => {}
                    Err(error) => return Err(rig_provider_error(error)),
                }
            }
            stream.finish().await.map_err(rig_provider_error)?
        } else {
            self.adapter
                .model
                .call(request)
                .await
                .map_err(rig_provider_error)?
        };
        if let Some(attempt) = self.attempt {
            let _ = attempt.rig_response(&response);
        }
        let converted = from_rig_response(&response)
            .map_err(|error| io_harness::Error::Config(error.to_string()))?;
        if let Ok(mut ledger) = self.adapter.call_ids.lock() {
            ledger.observe(&response, self.completion);
            ledger.save(self.adapter.recording_root.as_deref());
        }
        Ok(converted)
    }

    fn name(&self) -> &str {
        self.adapter.model.name()
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
    to_rig_request_with_ids(request, &Arc::new(Mutex::new(CallIdLedger::default())))
}

fn to_rig_request_with_ids(
    request: &IoRequest,
    call_ids: &Arc<Mutex<CallIdLedger>>,
) -> Result<RigRequest, ConversionError> {
    let mut history = Vec::new();
    if !request.system.is_empty() {
        history.push(RigMessage::system(request.system.clone()));
    }
    if request.output_schema.is_some() {
        history.push(RigMessage::system(
            "Anchor completion protocol: submit your final result by calling final_result alone. Any feedback asking for a JSON document or an output shape refers to the arguments of final_result, never to plain assistant text. Business tool calls remain separate; inspect their results before submitting completion."
        ));
    }
    let mut last_calls: Vec<(CallId, ToolName)> = Vec::new();
    let mut call_ordinal = 0;
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
                    let id = call_ids
                        .lock()
                        .map_err(|_| ConversionError::UnsupportedAssistantContent)?
                        .id_for(
                            call_ordinal,
                            message_index,
                            call_index,
                            &name,
                            &call.arguments,
                        );
                    call_ordinal += 1;
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
    let mut tools = rig_tools(&request.tools)?;
    if let Some(schema) = &request.output_schema {
        if request
            .tools
            .iter()
            .any(|tool| tool.name == crate::completion::TOOL_NAME)
        {
            return Err(ConversionError::ReservedCompletionTool);
        }
        tools.push(crate::completion::definition(schema));
    }
    converted = converted.tools(tools);
    // Output travels as a native tool invocation. Harness validates the adapter's
    // canonical result locally; providers need no response_format support.
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
    // io-harness carries provider thinking in `CompletionResponse::reasoning`:
    // a display string delivered to the observer and deliberately never
    // persisted or replayed (a step's durable turn keeps only text and tool
    // calls). Rig's `Text`/`Summary` reasoning blocks are that thinking and map
    // onto it faithfully. `Encrypted`/`Redacted` blocks are opaque provider
    // replay payloads, not thinking; io-harness has no field for them, and
    // because this boundary never replays reasoning they are neither sent nor
    // flattened into the display text. A reasoning item with no displayable
    // text carries nothing representable and must fail closed, not be dropped.
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    for part in &response.choice {
        match part {
            AssistantContent::Text(value) => text.push_str(&value.text),
            AssistantContent::ToolCall(call) => calls.push(IoToolCall {
                name: call.function.name.to_string(),
                arguments: call.function.arguments.clone(),
            }),
            AssistantContent::Reasoning(sealed) => {
                // A sealed value opens for its own issuer, which is always
                // present on the value.
                let opened = sealed
                    .open(sealed.issuer())
                    .ok_or(ConversionError::UnsupportedReasoning)?;
                let value = rig_reasoning_display_text(opened);
                if value.is_empty() {
                    return Err(ConversionError::UnsupportedReasoning);
                }
                if !reasoning.is_empty() {
                    reasoning.push('\n');
                }
                reasoning.push_str(&value);
            }
            AssistantContent::Image(_) => {
                return Err(ConversionError::UnsupportedAssistantContent);
            }
        }
    }
    let usage = response.usage;
    Ok(IoResponse {
        text: (!text.is_empty()).then_some(text),
        reasoning: (!reasoning.is_empty()).then_some(reasoning),
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
        finish_reason: response.finish_reason().map(|reason| match reason {
            FinishReason::Stop => "stop".into(),
            FinishReason::Length => "length".into(),
            FinishReason::ToolCalls => "tool_calls".into(),
            FinishReason::ContentFilter => "content_filter".into(),
            FinishReason::Other(value) => value,
        }),
        ..Default::default()
    })
}

/// The displayable thinking text of a Rig reasoning item: `Text` and `Summary`
/// blocks, in order. `Encrypted` and `Redacted` blocks are opaque provider
/// replay payloads and contribute nothing here; a caller can tell an item that
/// carried only those apart by the empty result.
fn rig_reasoning_display_text(reasoning: &Reasoning) -> String {
    let mut text = String::new();
    for block in &reasoning.content {
        match block {
            ReasoningContent::Text { text: value, .. } => text.push_str(value),
            ReasoningContent::Summary(value) => text.push_str(value),
            ReasoningContent::Encrypted(_) | ReasoningContent::Redacted { .. } => {}
        }
    }
    text
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
    fn provider_error_mapping_preserves_rigs_retry_verdict() {
        let transient = rig_provider_error(rig_core::error::ProviderError::from_transport_error(
            rig_core::http_client::Error::StreamEnded,
        ));
        assert!(matches!(
            transient,
            io_harness::Error::Provider {
                kind: io_harness::ProviderErrorKind::Transport,
                ..
            }
        ));

        let permanent = rig_provider_error(rig_core::error::ProviderError::request(
            "request cannot be built",
        ));
        assert!(matches!(
            permanent,
            io_harness::Error::Provider {
                kind: io_harness::ProviderErrorKind::Request,
                ..
            }
        ));
    }

    #[test]
    fn responses_wire_preserves_parallel_tool_results_and_call_ids() {
        let request = IoRequest {
            user: "run both checks".into(),
            messages: vec![
                Message::User("run both checks".into()),
                Message::Assistant {
                    text: None,
                    calls: vec![
                        IoToolCall {
                            name: "check_one".into(),
                            arguments: json!({"item": 1}),
                        },
                        IoToolCall {
                            name: "check_two".into(),
                            arguments: json!({"item": 2}),
                        },
                    ],
                },
                Message::Results(vec![
                    ToolResult {
                        call: 0,
                        content: "sandbox refusal".into(),
                    },
                    ToolResult {
                        call: 1,
                        content: "success".into(),
                    },
                ]),
            ],
            tools: vec![
                IoToolSpec {
                    name: "check_one".into(),
                    description: "first check".into(),
                    parameters: json!({"type": "object"}),
                },
                IoToolSpec {
                    name: "check_two".into(),
                    description: "second check".into(),
                    parameters: json!({"type": "object"}),
                },
            ],
            ..Default::default()
        };

        let rig_request = to_rig_request(&request).unwrap();
        let responses = rig_core::providers::openai::responses_api::CompletionRequest::try_from((
            "test-model".to_owned(),
            rig_request,
        ))
        .unwrap();
        let encoded = serde_json::to_value(responses).unwrap();
        let input = encoded["input"].as_array().unwrap();
        let calls: Vec<_> = input
            .iter()
            .filter(|item| item["type"] == "function_call")
            .collect();
        let outputs: Vec<_> = input
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .collect();
        assert_eq!(calls.len(), 2, "encoded input: {encoded}");
        assert_eq!(outputs.len(), 2, "encoded input: {encoded}");
        assert_eq!(
            calls
                .iter()
                .map(|item| item["call_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["io-00000001-00000000", "io-00000001-00000001"]
        );
        assert_eq!(
            outputs
                .iter()
                .map(|item| item["call_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["io-00000001-00000000", "io-00000001-00000001"]
        );
        assert_eq!(outputs[0]["output"], "sandbox refusal");
        assert_eq!(outputs[1]["output"], "success");
    }

    #[test]
    fn provider_call_ids_survive_io_harness_round_trip() {
        let first_response = RigResponse::new(
            vec![
                AssistantContent::ToolCall(RigToolCall::new(
                    CallId::from_wire("call_provider_a"),
                    ToolFunction::new(tool_name("check_one").unwrap(), json!({"item": 1})),
                )),
                AssistantContent::ToolCall(RigToolCall::new(
                    CallId::from_wire("call_provider_b"),
                    ToolFunction::new(tool_name("check_two").unwrap(), json!({"item": 2})),
                )),
            ],
            Default::default(),
            "test-model",
            json!({}),
        );
        let io_response = from_rig_response(&first_response).unwrap();
        assert_eq!(io_response.tool_calls.len(), 2);

        let ledger = Arc::new(Mutex::new(CallIdLedger::default()));
        ledger.lock().unwrap().observe(&first_response, false);
        let next_request = IoRequest {
            messages: vec![
                Message::User("run both checks".into()),
                Message::Assistant {
                    text: None,
                    calls: io_response.tool_calls,
                },
                Message::Results(vec![
                    ToolResult {
                        call: 0,
                        content: "first".into(),
                    },
                    ToolResult {
                        call: 1,
                        content: "second".into(),
                    },
                ]),
            ],
            ..Default::default()
        };
        let rig_request = to_rig_request_with_ids(&next_request, &ledger).unwrap();
        let responses = rig_core::providers::openai::responses_api::CompletionRequest::try_from((
            "test-model".to_owned(),
            rig_request,
        ))
        .unwrap();
        let encoded = serde_json::to_value(responses).unwrap();
        let input = encoded["input"].as_array().unwrap();
        let calls: Vec<_> = input
            .iter()
            .filter(|item| item["type"] == "function_call")
            .collect();
        let outputs: Vec<_> = input
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .collect();
        assert_eq!(
            calls
                .iter()
                .map(|item| item["call_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["call_provider_a", "call_provider_b"]
        );
        assert_eq!(
            outputs
                .iter()
                .map(|item| item["call_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["call_provider_a", "call_provider_b"]
        );
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
    fn output_schema_becomes_native_completion_tool_without_response_format() {
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
        assert_eq!(converted.tools.len(), 1);
        assert_eq!(converted.tools[0].name, "final_result");
        assert_eq!(converted.tools[0].parameters, *schema.as_value());
        let mut collision = request;
        collision.tools.push(IoToolSpec {
            name: "final_result".into(),
            description: "business tool".into(),
            parameters: json!({"type":"object"}),
        });
        assert!(matches!(
            to_rig_request(&collision),
            Err(ConversionError::ReservedCompletionTool)
        ));
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

    fn sealed_reasoning(reasoning: Reasoning) -> rig_core::completion::message::Sealed<Reasoning> {
        reasoning.sealed(rig_core::completion::message::Issuer::from_static("openai"))
    }

    #[test]
    fn reasoning_text_is_carried_into_io_reasoning_without_dropping_text_or_tools() {
        let name = tool_name("lookup").unwrap();
        let response = RigResponse::new(
            vec![
                AssistantContent::Reasoning(sealed_reasoning(Reasoning::new("thinking hard"))),
                AssistantContent::text("answer"),
                AssistantContent::ToolCall(RigToolCall::new(
                    CallId::from_wire("call-1"),
                    ToolFunction::new(name, json!({"id": 7})),
                )),
            ],
            Default::default(),
            "rig-test",
            json!({}),
        );
        let converted = from_rig_response(&response).unwrap();
        assert_eq!(converted.reasoning.as_deref(), Some("thinking hard"));
        assert_eq!(converted.text.as_deref(), Some("answer"));
        assert_eq!(converted.tool_calls[0].name, "lookup");
    }

    #[test]
    fn mixed_reasoning_keeps_text_and_does_not_flatten_opaque_bytes() {
        // DeepSeek's Responses wire returns reasoning_text beside an opaque
        // encrypted payload. The thinking text is carried; the opaque replay
        // payload has no io-harness field and must not be flattened into the
        // output.
        let opaque = "ENC:v1:opaque-replay-bytes";
        let response = RigResponse::new(
            vec![
                AssistantContent::Reasoning(sealed_reasoning(Reasoning {
                    id: Some("reasoning-1".into()),
                    content: vec![
                        ReasoningContent::Text {
                            text: "plan: help the user".into(),
                            signature: None,
                        },
                        ReasoningContent::Encrypted(opaque.into()),
                    ],
                })),
                AssistantContent::text("Hi"),
            ],
            Default::default(),
            "rig-test",
            json!({}),
        );
        let converted = from_rig_response(&response).unwrap();
        let reasoning = converted.reasoning.as_deref().unwrap();
        assert_eq!(reasoning, "plan: help the user");
        assert!(!reasoning.contains(opaque));
        assert_eq!(converted.text.as_deref(), Some("Hi"));
    }

    #[test]
    fn opaque_only_reasoning_fails_closed() {
        for block in [
            ReasoningContent::Encrypted("encrypted-only".into()),
            ReasoningContent::Redacted {
                data: "redacted-only".into(),
            },
        ] {
            let response = RigResponse::new(
                vec![AssistantContent::Reasoning(sealed_reasoning(Reasoning {
                    id: None,
                    content: vec![block],
                }))],
                Default::default(),
                "rig-test",
                json!({}),
            );
            assert_eq!(
                from_rig_response(&response),
                Err(ConversionError::UnsupportedReasoning)
            );
        }
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

    fn summary_request() -> IoRequest {
        IoRequest {
            user: "finish the node".into(),
            output_schema: Some(
                io_harness::schema::OutputSchema::new(json!({
                    "type": "object",
                    "properties": {"summary": {"type": "string"}},
                    "required": ["summary"]
                }))
                .unwrap(),
            ),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn completion_stream_is_atomic_and_fails_closed_before_final_call() {
        use std::sync::{Arc, Mutex};

        // Rig exposes argument fragments before it exposes the completed
        // tool call (and its name). The adapter must not publish a guessed
        // summary from those fragments. The sole callback is the canonical
        // projection after the final_result End event.
        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::tool_call_name_delta("done", "final_result"),
            MockStreamEvent::tool_call_arguments_delta("done", "{\"summary\":\"first"),
            MockStreamEvent::tool_call_arguments_delta("done", " growth\"}"),
            MockStreamEvent::tool_call_end("done"),
            MockStreamEvent::final_response_with_default_usage(),
        ]]);
        let adapter = RigProviderAdapter::new(model.erase(), false);
        let chunks = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = Arc::clone(&chunks);
        let response =
            io_harness::Provider::complete_streaming(&adapter, summary_request(), &move |chunk| {
                captured.lock().unwrap().push(chunk.to_owned())
            })
            .await
            .unwrap();
        let chunks = chunks.lock().unwrap();
        assert_eq!(
            chunks.len(),
            1,
            "partial final_result arguments must not stream"
        );
        let value: serde_json::Value = serde_json::from_str(&chunks[0]).unwrap();
        assert_eq!(value["summary"], "first growth");
        assert_eq!(value["_anchor_completion"]["status"], "submitted");
        assert_eq!(response.text.as_deref(), Some(chunks[0].as_str()));
    }

    #[tokio::test]
    async fn mixed_and_truncated_completion_streams_never_publish_summary() {
        use std::sync::{Arc, Mutex};

        let mixed_model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::tool_call("business", "lookup", json!({"id": 7})),
            MockStreamEvent::tool_call_name_delta("done", "final_result"),
            MockStreamEvent::tool_call_arguments_delta("done", "{\"summary\":\"premature\"}"),
            MockStreamEvent::tool_call_end("done"),
            MockStreamEvent::final_response_with_default_usage(),
        ]]);
        let mixed_adapter = RigProviderAdapter::new(mixed_model.erase(), false);
        let mixed_chunks = Arc::new(Mutex::new(Vec::<String>::new()));
        let mixed_captured = Arc::clone(&mixed_chunks);
        io_harness::Provider::complete_streaming(
            &mixed_adapter,
            summary_request(),
            &move |chunk| mixed_captured.lock().unwrap().push(chunk.to_owned()),
        )
        .await
        .unwrap();
        {
            let mixed_chunks = mixed_chunks.lock().unwrap();
            assert_eq!(mixed_chunks.len(), 1);
            let mixed: serde_json::Value = serde_json::from_str(&mixed_chunks[0]).unwrap();
            assert_eq!(mixed["_anchor_completion"]["status"], "deferred");
            assert!(mixed.get("summary").is_none());
        }

        // A stream that reaches the provider finish without a completed tool
        // call is rejected; its partial arguments are never a completion fact.
        let truncated_model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::tool_call_name_delta("done", "final_result"),
            MockStreamEvent::tool_call_arguments_delta("done", "{\"summary\":\"cut off"),
            MockStreamEvent::final_response_with_default_usage(),
        ]]);
        let truncated_adapter = RigProviderAdapter::new(truncated_model.erase(), false);
        let truncated_chunks = Arc::new(Mutex::new(Vec::<String>::new()));
        let truncated_captured = Arc::clone(&truncated_chunks);
        io_harness::Provider::complete_streaming(
            &truncated_adapter,
            summary_request(),
            &move |chunk| truncated_captured.lock().unwrap().push(chunk.to_owned()),
        )
        .await
        .unwrap();
        let truncated_chunks = truncated_chunks.lock().unwrap();
        assert_eq!(truncated_chunks.len(), 1);
        let truncated: serde_json::Value = serde_json::from_str(&truncated_chunks[0]).unwrap();
        assert_eq!(truncated["_anchor_completion"]["status"], "rejected");
        assert!(truncated.get("summary").is_none());
    }
}
