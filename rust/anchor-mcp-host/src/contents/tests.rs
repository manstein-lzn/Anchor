use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Value, json};

use super::{McpHostError, result_contents};

fn response(content: Vec<Value>, structured: Option<Value>) -> CallToolResult {
    serde_json::from_value(json!({
        "content": content,
        "structuredContent": structured,
        "isError": false
    }))
    .unwrap()
}

fn result_wire(result: &CallToolResult) -> Value {
    serde_json::to_value(result_contents(result).unwrap()).unwrap()
}

fn preserved_blocks() -> Vec<Value> {
    vec![
        json!({
            "type": "resource",
            "resource": {"uri": "file:///input.txt", "mimeType": "text/plain", "text": "body", "_meta": {"inner": true}},
            "_meta": {"outer": true},
            "annotations": {"audience": ["user"], "priority": 0.5}
        }),
        json!({
            "type": "resource",
            "resource": {"uri": "file:///input.bin", "mimeType": "application/octet-stream", "blob": "AAEC"}
        }),
        json!({
            "type": "resource_link",
            "uri": "file:///data",
            "name": "data",
            "description": "linked data",
            "mimeType": "application/json",
            "size": 19,
            "_meta": {"kept": true}
        }),
        json!({"type": "audio", "data": "AAEC", "mimeType": "audio/wav", "_meta": {"kept": true}}),
        json!({"type": "resource", "resource": {"uri": "file:///untyped", "blob": "AAEC"}}),
    ]
}

#[test]
fn literal_text_is_not_reparsed_as_json() {
    let result = CallToolResult::success(vec![
        ContentBlock::text("{\"value\":true}"),
        ContentBlock::text("多行\ntext"),
    ]);
    assert_eq!(
        result_wire(&result),
        json!([
            {"type": "text", "text": "{\"value\":true}"},
            {"type": "text", "text": "多行\ntext"}
        ])
    );
}

#[test]
fn structured_results_replace_only_first_canonical_compatibility_text_in_place() {
    let structured = json!({"value": 7});
    let result = response(
        vec![
            json!({"type": "text", "text": "before"}),
            json!({"type": "text", "text": structured.to_string()}),
            json!({"type": "text", "text": structured.to_string()}),
            json!({"type": "text", "text": "after"}),
        ],
        Some(structured.clone()),
    );
    assert_eq!(
        result_wire(&result),
        json!([
            {"type": "text", "text": "before"},
            {"type": "json", "value": structured},
            {"type": "text", "text": structured.to_string()},
            {"type": "text", "text": "after"}
        ])
    );
}

#[test]
fn structured_results_prepend_typed_value_without_replacing_genuine_text() {
    let structured = json!({"value": 7});
    let genuine_text = serde_json::to_string_pretty(&structured).unwrap();
    let result = response(
        vec![json!({"type": "text", "text": genuine_text})],
        Some(structured.clone()),
    );
    assert_eq!(
        result_wire(&result),
        json!([
            {"type": "json", "value": structured},
            {"type": "text", "text": genuine_text}
        ])
    );
}

#[test]
fn structured_constructor_and_nonobject_values_keep_their_types() {
    for value in [
        json!({"value": 7}),
        json!([1, false]),
        json!(true),
        json!("text"),
    ] {
        assert_eq!(
            result_wire(&CallToolResult::structured(value.clone())),
            json!([{ "type": "json", "value": value }])
        );
    }
}

#[test]
fn audio_resources_and_links_preserve_complete_blocks() {
    let blocks = preserved_blocks();
    let expected: Vec<_> = blocks
        .iter()
        .map(|block| json!({"type": "json", "value": block}))
        .collect();
    assert_eq!(result_wire(&response(blocks, None)), Value::Array(expected));
}

#[test]
fn empty_results_match_original_sendable_text_normalization() {
    assert_eq!(
        result_wire(&CallToolResult::success(vec![])),
        json!([{ "type": "text", "text": "" }])
    );
    assert_eq!(
        result_wire(&CallToolResult::error(vec![])),
        json!([{ "type": "text", "text": "the MCP tool reported an error" }])
    );
}

#[test]
fn invalid_typed_images_and_image_resources_fail_without_partial_output() {
    for mime_type in [
        "image/jpeg",
        "image/png",
        "image/gif",
        "image/webp",
        "image/heic",
        "image/heif",
        "image/svg+xml",
        "image/unknown",
    ] {
        for block in [
            json!({"type": "image", "data": "AAEC", "mimeType": mime_type}),
            json!({"type": "resource", "resource": {"uri": "file:///image", "mimeType": mime_type, "blob": "AAEC"}}),
        ] {
            let result = response(
                vec![
                    json!({"type": "text", "text": "not a partial success"}),
                    block,
                ],
                Some(json!({"also": "not a partial success"})),
            );
            assert!(matches!(
                result_contents(&result),
                Err(McpHostError::Encode(_))
            ));
        }
    }
}

#[test]
fn native_images_and_blob_resources_remain_typed_in_mixed_order() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let data = STANDARD.encode(bytes.into_inner());
    let structured = json!({"caption":"test image"});
    let result = response(
        vec![
            json!({"type":"text","text":"before"}),
            json!({"type":"image","mimeType":"image/png","data":data}),
            json!({"type":"text","text":structured.to_string()}),
            json!({"type":"resource","resource":{"uri":"file:///never-dereference","mimeType":"image/png","blob":data}}),
            json!({"type":"text","text":"after"}),
        ],
        Some(structured.clone()),
    );
    assert_eq!(
        result_wire(&result),
        json!([
            {"type":"text","text":"before"},
            {"type":"image","mime_type":"image/png","data":data},
            {"type":"json","value":structured},
            {"type":"image","mime_type":"image/png","data":data},
            {"type":"text","text":"after"}
        ])
    );
}
