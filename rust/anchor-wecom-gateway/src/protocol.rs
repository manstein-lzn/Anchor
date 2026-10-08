use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{GatewayError, MAX_TEXT_BYTES};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelEvent {
    pub source: String,
    pub event_id: String,
    pub sender_id: String,
    pub conversation_id: String,
    pub text: String,
    pub reply_target: String,
    pub message_type: String,
    pub metadata: BTreeMap<String, Value>,
    pub attachments: Vec<Value>,
}

pub fn normalize_message(frame: &Value) -> Result<Option<ChannelEvent>, GatewayError> {
    if frame.get("cmd").and_then(Value::as_str) != Some("aibot_msg_callback") {
        return Ok(None);
    }
    let body = frame
        .get("body")
        .and_then(Value::as_object)
        .ok_or(GatewayError::Invalid("invalid callback body"))?;
    let sender = identity(frame.pointer("/body/from/userid"), 200)?;
    let request_id = identity(frame.pointer("/headers/req_id"), 500)?;
    let event_id = match body.get("msgid").filter(|value| value.as_str() != Some("")) {
        Some(value) => identity(Some(value), 500)?,
        None => request_id.clone(),
    };
    let conversation = match body
        .get("chatid")
        .filter(|value| value.as_str() != Some(""))
    {
        Some(value) => identity(Some(value), 200)?,
        None => sender.clone(),
    };
    let message_type = body
        .get("msgtype")
        .and_then(Value::as_str)
        .ok_or(GatewayError::Invalid("missing callback message type"))?;
    if [
        "image",
        "file",
        "voice",
        "video",
        "audio",
        "media",
        "msg_item",
        "attachments",
    ]
    .iter()
    .any(|key| body.contains_key(*key))
    {
        return Err(GatewayError::UnsupportedMedia);
    }
    if message_type == "text" && body.contains_key("mixed") {
        return Err(GatewayError::UnsupportedMedia);
    }
    let text = match message_type {
        "text" => text_content(body.get("text"))?,
        "mixed" => {
            let items = body
                .get("mixed")
                .and_then(|mixed| mixed.get("msg_item"))
                .and_then(Value::as_array)
                .filter(|items| !items.is_empty() && items.len() <= 32)
                .ok_or(GatewayError::Invalid("invalid mixed callback"))?;
            let mut texts = Vec::with_capacity(items.len());
            for item in items {
                let object = item
                    .as_object()
                    .ok_or(GatewayError::Invalid("invalid mixed item"))?;
                if object.get("msgtype").and_then(Value::as_str) != Some("text")
                    || object.keys().any(|key| key != "text" && key != "msgtype")
                {
                    return Err(GatewayError::UnsupportedMedia);
                }
                texts.push(text_content(object.get("text"))?);
            }
            texts.join("\n")
        }
        _ => return Err(GatewayError::UnsupportedMedia),
    };
    if text.len() > MAX_TEXT_BYTES {
        return Err(GatewayError::Invalid(
            "callback text exceeds platform limits",
        ));
    }
    let chat_type = body.get("chattype").cloned().unwrap_or(json!("single"));
    if chat_type.as_str() != Some("single") {
        return Err(GatewayError::Invalid(
            "only private WeCom callbacks are supported",
        ));
    }
    Ok(Some(ChannelEvent {
        source: "wecom".into(),
        event_id,
        sender_id: sender,
        conversation_id: conversation.clone(),
        reply_target: conversation,
        text,
        message_type: message_type.into(),
        metadata: BTreeMap::from([
            ("chat_type".into(), chat_type),
            ("request_id".into(), json!(request_id)),
        ]),
        attachments: Vec::new(),
    }))
}

fn text_content(value: Option<&Value>) -> Result<String, GatewayError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or(GatewayError::Invalid("invalid callback text"))?;
    if object.keys().any(|key| key != "content") {
        return Err(GatewayError::UnsupportedMedia);
    }
    object
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(GatewayError::Invalid("invalid callback text"))
}

pub(crate) fn identity(value: Option<&Value>, limit: usize) -> Result<String, GatewayError> {
    value
        .and_then(Value::as_str)
        .filter(|value| {
            !value.trim().is_empty()
                && value.chars().count() <= limit
                && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or(GatewayError::Invalid("missing or invalid message identity"))
}

pub fn stream_id(event_id: &str) -> String {
    format!("anchor-{:x}", Sha256::digest(event_id.as_bytes()))[..39].into()
}

pub(crate) fn frame(cmd: &str, request_id: &str, body: Value) -> Value {
    json!({"cmd": cmd, "headers": {"req_id": request_id}, "body": body})
}

pub(crate) fn event_digest(event: &ChannelEvent) -> String {
    let mut bound = event.clone();
    bound.metadata.remove("request_id");
    digest(&serde_json::to_vec(&bound).expect("serializable channel event"))
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
