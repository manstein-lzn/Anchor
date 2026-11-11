use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{GatewayError, MAX_TEXT_BYTES, media::InboundMedia};

const MAX_MIXED_ITEMS: usize = 32;
const MAX_MEDIA_ITEMS: usize = 16;

pub use anchor_channel_transport::{ChannelEvent, ConversationScope, digest};

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
    let chat_type = match body.get("chattype") {
        None => "single",
        Some(Value::String(value)) if matches!(value.as_str(), "single" | "group") => value,
        _ => return Err(GatewayError::Invalid("unsupported callback chat type")),
    };
    let conversation = match body
        .get("chatid")
        .filter(|value| value.as_str() != Some(""))
    {
        Some(value) => identity(Some(value), 200)?,
        None if chat_type == "single" => sender.clone(),
        None => return Err(GatewayError::Invalid("group callback has no chatid")),
    };
    let message_type = body
        .get("msgtype")
        .and_then(Value::as_str)
        .ok_or(GatewayError::Invalid("missing callback message type"))?;
    // The official long-connection contract only exposes standalone image,
    // file and voice callbacks in private chats. Group media arrives in mixed.
    if chat_type == "group" && matches!(message_type, "image" | "file" | "voice") {
        return Err(GatewayError::UnsupportedMedia);
    }
    if ["video", "audio", "media", "attachments"]
        .iter()
        .any(|key| body.contains_key(*key))
    {
        return Err(GatewayError::UnsupportedMedia);
    }
    // Media payloads stay as descriptors: the transport downloads and decrypts
    // them itself, so the temporary URL never leaves this process.
    let (text, attachments) = match message_type {
        "text" => {
            if body.contains_key("mixed")
                || ["image", "file", "voice"]
                    .iter()
                    .any(|key| body.contains_key(*key))
            {
                return Err(GatewayError::UnsupportedMedia);
            }
            (text_content(body.get("text"))?, Vec::new())
        }
        "mixed" => mixed_content(body)?,
        "image" | "file" | "voice" => {
            if body.contains_key("mixed") {
                return Err(GatewayError::UnsupportedMedia);
            }
            let payload = body
                .get(message_type)
                .and_then(Value::as_object)
                .ok_or(GatewayError::Invalid("media callback has no payload"))?;
            let media = InboundMedia::from_payload(message_type, payload)?;
            (String::new(), vec![media.descriptor()])
        }
        _ => return Err(GatewayError::UnsupportedMedia),
    };
    if text.len() > MAX_TEXT_BYTES {
        return Err(GatewayError::Invalid(
            "callback text exceeds platform limits",
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
            ("chat_type".into(), json!(chat_type)),
            ("request_id".into(), json!(request_id)),
        ]),
        attachments,
    }))
}

/// A mixed callback may interleave text and media items. Text-only mixed
/// callbacks keep their original meaning: joined text and no attachments.
fn mixed_content(body: &Map<String, Value>) -> Result<(String, Vec<Value>), GatewayError> {
    let items = body
        .get("mixed")
        .and_then(|mixed| mixed.get("msg_item"))
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty() && items.len() <= MAX_MIXED_ITEMS)
        .ok_or(GatewayError::Invalid("invalid mixed callback"))?;
    let mut texts = Vec::with_capacity(items.len());
    let mut attachments = Vec::new();
    for item in items {
        let object = item
            .as_object()
            .ok_or(GatewayError::Invalid("invalid mixed item"))?;
        let kind = object
            .get("msgtype")
            .and_then(Value::as_str)
            .ok_or(GatewayError::UnsupportedMedia)?;
        if kind == "text" {
            if object.keys().any(|key| key != "text" && key != "msgtype") {
                return Err(GatewayError::UnsupportedMedia);
            }
            texts.push(text_content(object.get("text"))?);
            continue;
        }
        if object.keys().any(|key| key != kind && key != "msgtype") {
            return Err(GatewayError::UnsupportedMedia);
        }
        let payload = object
            .get(kind)
            .and_then(Value::as_object)
            .ok_or(GatewayError::UnsupportedMedia)?;
        let media = InboundMedia::from_payload(kind, payload)?;
        if attachments.len() >= MAX_MEDIA_ITEMS {
            return Err(GatewayError::Invalid("too many inbound attachments"));
        }
        attachments.push(media.descriptor());
    }
    Ok((texts.join("\n"), attachments))
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
