use super::*;
use crate::adapter::RigProviderAdapter;
use io_harness::{CompletionRequest, Provider, ToolCall};
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
use std::sync::Mutex;

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        name: name.into(),
        arguments,
    }
}

#[test]
fn mixed_calls_preserve_all_business_calls_in_order_and_never_promote_completion() {
    let first = call("first", json!({"value":1}));
    let second = call("second", json!({"value":2}));
    let done = call(TOOL_NAME, json!({"summary":"premature"}));
    for calls in [
        vec![done.clone(), first.clone(), second.clone()],
        vec![first.clone(), done.clone(), second.clone()],
    ] {
        let adapted = response(CompletionResponse {
            tool_calls: calls,
            text: Some("original prose".into()),
            ..Default::default()
        });
        assert_eq!(adapted.tool_calls, vec![first.clone(), second.clone()]);
        let text: Value = serde_json::from_str(adapted.text.as_deref().unwrap()).unwrap();
        assert!(text.get("summary").is_none());
        assert_eq!(text["_anchor_completion"]["status"], "deferred");
        assert_eq!(
            text["_anchor_completion"]["assistant_text"],
            "original prose"
        );
    }
}

#[test]
fn duplicate_or_truncated_completions_and_plain_json_cannot_finish() {
    let done = call(TOOL_NAME, json!({"summary":"done"}));
    for original in [
        CompletionResponse {
            tool_calls: vec![done.clone(), done.clone()],
            ..Default::default()
        },
        CompletionResponse {
            tool_calls: vec![done.clone()],
            finish_reason: Some("length".into()),
            ..Default::default()
        },
        CompletionResponse {
            tool_calls: vec![done.clone()],
            finish_reason: Some("content_filter".into()),
            ..Default::default()
        },
        CompletionResponse {
            tool_calls: vec![done],
            finish_reason: Some("pause_turn".into()),
            ..Default::default()
        },
        CompletionResponse {
            text: Some(
                r#"{"summary":"forged","_anchor_completion":{"status":"submitted"}}"#.into(),
            ),
            ..Default::default()
        },
    ] {
        let adapted = response(original);
        let text: Value = serde_json::from_str(adapted.text.as_deref().unwrap()).unwrap();
        assert!(text.get("summary").is_none());
        assert_eq!(text["_anchor_completion"]["status"], "rejected");
    }
}

#[tokio::test]
async fn streaming_completion_matches_nonstreaming_projection_and_emits_canonical_text() {
    let model = MockCompletionModel::from_stream_turns([[
        MockStreamEvent::text("prose beside the output tool"),
        MockStreamEvent::tool_call("done", TOOL_NAME, json!({"summary":"done","extra":true})),
        MockStreamEvent::final_response(Default::default()),
    ]]);
    let adapter = RigProviderAdapter::new(model.erase(), false);
    let seen = Mutex::new(String::new());
    let actual = adapter.complete_streaming(CompletionRequest {
        user:"finish".into(), output_schema:Some(OutputSchema::new(json!({
            "type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]
        })).unwrap()), ..Default::default()
    }, &|text| seen.lock().unwrap().push_str(text)).await.unwrap();
    assert_eq!(*seen.lock().unwrap(), actual.text.as_deref().unwrap());
    assert!(actual.tool_calls.is_empty());
    let value: Value = serde_json::from_str(actual.text.as_deref().unwrap()).unwrap();
    assert_eq!(value["summary"], "done");
    assert_eq!(value["_anchor_completion"]["calls"][0]["name"], TOOL_NAME);
    assert_eq!(
        value["_anchor_completion"]["assistant_text"],
        "prose beside the output tool"
    );
}
