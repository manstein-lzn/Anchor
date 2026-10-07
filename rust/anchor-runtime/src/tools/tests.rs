use serde_json::{Value, json};

use super::{ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent};

#[test]
fn definition_preserves_the_rig_043_wire_shape() {
    let definition = ToolDefinition::new(
        ToolName::new("anchor_echo").unwrap(),
        "Return JSON unchanged",
        json!({"type": "object", "properties": {"value": {}}}),
    );
    let encoded = json!({
        "name": "anchor_echo",
        "description": "Return JSON unchanged",
        "parameters": {"type": "object", "properties": {"value": {}}}
    });
    assert_eq!(serde_json::to_value(&definition).unwrap(), encoded);
    assert_eq!(
        serde_json::from_value::<ToolDefinition>(encoded).unwrap(),
        definition
    );
}

#[test]
fn text_results_preserve_the_rig_043_wire_shape_and_literal_content() {
    for text in ["", "plain text", "{\"value\":true}", "多行\n\"text\""] {
        let content = ToolResultContent::text(text);
        let encoded = json!({"type": "text", "text": text});
        assert_eq!(serde_json::to_value(&content).unwrap(), encoded);
        let decoded = serde_json::from_value::<ToolResultContent>(encoded).unwrap();
        assert_eq!(decoded, content);
        assert_eq!(decoded.as_text(), Some(text));
        assert_eq!(decoded.as_json(), None);
    }
}

#[test]
fn json_results_preserve_the_rig_043_wire_shape_and_value_types() {
    for value in [
        Value::Null,
        json!(true),
        json!(42),
        json!(3.25),
        json!("literal text"),
        json!([null, false, {"nested": "value"}]),
        json!({"type": "text", "text": "not a text result"}),
    ] {
        let content = ToolResultContent::json(value.clone());
        let encoded = json!({"type": "json", "value": value});
        assert_eq!(serde_json::to_value(&content).unwrap(), encoded);
        let decoded = serde_json::from_value::<ToolResultContent>(encoded).unwrap();
        assert_eq!(decoded, content);
        assert_eq!(decoded.as_json(), Some(&value));
        assert_eq!(decoded.as_text(), None);
    }
}

#[test]
fn typed_results_require_their_canonical_fields() {
    for encoded in [
        json!({"type": "unknown", "text": "result"}),
        json!({"type": "text"}),
        json!({"type": "text", "text": {}}),
        json!({"text": "result"}),
        json!({"type": "json", "value": {}, "text": "ignored"}),
    ] {
        if encoded["type"] == "json" {
            let content = serde_json::from_value::<ToolResultContent>(encoded).unwrap();
            assert_eq!(content.as_json(), Some(&json!({})));
        } else {
            assert!(serde_json::from_value::<ToolResultContent>(encoded).is_err());
        }
    }
}

#[test]
fn tool_names_match_rig_nonempty_validation_and_string_serialization() {
    assert!(ToolName::new("").is_err());
    assert!(serde_json::from_value::<ToolName>(json!("")).is_err());
    for name in ["anchor_echo", "namespace.tool", " ", "工具"] {
        let tool_name = ToolName::new(name).unwrap();
        assert_eq!(tool_name.as_str(), name);
        assert_eq!(tool_name.to_string(), name);
        assert_eq!(serde_json::to_value(&tool_name).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<ToolName>(json!(name)).unwrap(),
            tool_name
        );
    }
}

struct EchoTools;

impl ToolPort for EchoTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::new(
            ToolName::new("anchor_echo").unwrap(),
            "Return JSON unchanged",
            json!({"type": "object"}),
        )]
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a,
        >,
    > {
        Box::pin(async move {
            if name != "anchor_echo" {
                return Err(ToolError::Unknown(name.to_owned()));
            }
            Ok(vec![ToolResultContent::json(arguments)])
        })
    }
}

#[tokio::test]
async fn stable_tool_port_preserves_dispatch_results_and_conservative_replay() {
    let tools: Box<dyn crate::ToolPort> = Box::new(EchoTools);
    assert!(!tools.is_read_only("anchor_echo"));
    assert_eq!(tools.definitions()[0].name, "anchor_echo");
    assert_eq!(
        tools
            .call("anchor_echo", json!({"value": 7}))
            .await
            .unwrap(),
        vec![ToolResultContent::json(json!({"value": 7}))]
    );
    assert!(matches!(
        tools.call("unknown", Value::Null).await,
        Err(ToolError::Unknown(name)) if name == "unknown"
    ));
    assert_eq!(
        ToolError::Failed("denied".into()).to_string(),
        "tool failed: denied"
    );
}

#[cfg(not(feature = "rig-legacy"))]
#[test]
fn native_results_do_not_accept_unsupported_media() {
    assert!(
        serde_json::from_value::<ToolResultContent>(json!({
            "type": "image",
            "data": "encoded"
        }))
        .is_err()
    );
    for mime_type in ["image/png", "image/jpeg", "image/webp"] {
        let content = ToolResultContent::image("AAEC", mime_type);
        let encoded = json!({"type":"image","data":"AAEC","mime_type":mime_type});
        assert_eq!(serde_json::to_value(&content).unwrap(), encoded);
        assert_eq!(
            serde_json::from_value::<ToolResultContent>(encoded).unwrap(),
            content
        );
        assert_eq!(content.as_text(), None);
        assert_eq!(content.as_json(), None);
    }
}
