//! Feishu's official Go SDK `ws/pbbp2.pb.go`, `model.go` and
//! `client_message.go` define this binary envelope and acknowledgement shape.
//! Protobuf owns wire encoding; this module only supplies bounds and admission.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use prost::Message;
use serde_json::{Value, json};

use crate::media::InboundResource;
use crate::{ChannelEvent, GatewayError, MAX_REQUEST_BYTES, MAX_TEXT_BYTES};

pub const MAX_FRAME_BYTES: usize = 256 * 1024;
const MAX_PARTIAL_MESSAGES: usize = 32;
const MAX_PARTS: usize = 64;
const MAX_PARTIAL_BYTES: usize = 2 * 1024 * 1024;
const PARTIAL_TTL: Duration = Duration::from_secs(5);

#[derive(Clone, PartialEq, Message)]
pub struct Header {
    #[prost(string, required, tag = "1")]
    pub key: String,
    #[prost(string, required, tag = "2")]
    pub value: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Frame {
    #[prost(uint64, required, tag = "1")]
    pub seq_id: u64,
    #[prost(uint64, required, tag = "2")]
    pub log_id: u64,
    #[prost(int32, required, tag = "3")]
    pub service: i32,
    #[prost(int32, required, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<Header>,
    #[prost(string, tag = "6")]
    pub payload_encoding: String,
    #[prost(string, tag = "7")]
    pub payload_type: String,
    #[prost(bytes = "vec", tag = "8")]
    pub payload: Vec<u8>,
    #[prost(string, tag = "9")]
    pub log_id_new: String,
}

impl Frame {
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.key == key)
            .map(|header| header.value.as_str())
    }

    pub fn decode_binary(bytes: &[u8]) -> Result<Self, GatewayError> {
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(GatewayError::Invalid("Feishu frame exceeds size limit"));
        }
        let frame = Self::decode(bytes)
            .map_err(|_| GatewayError::Invalid("invalid Feishu protobuf frame"))?;
        let mut keys = std::collections::BTreeSet::new();
        if frame.headers.len() > 32
            || frame.headers.iter().any(|header| {
                header.key.len() > 128 || header.value.len() > 1024 || !keys.insert(&header.key)
            })
        {
            return Err(GatewayError::Invalid("invalid Feishu frame headers"));
        }
        if !matches!(frame.payload_encoding.as_str(), "" | "json") {
            return Err(GatewayError::Invalid("unsupported Feishu payload encoding"));
        }
        Ok(frame)
    }

    pub fn encode_binary(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    pub fn ping(service: i32) -> Self {
        Self {
            service,
            headers: vec![Header {
                key: "type".into(),
                value: "ping".into(),
            }],
            ..Self::default()
        }
    }

    pub(crate) fn acknowledge(mut self, code: u16, elapsed_millis: u128) -> Self {
        self.headers.retain(|header| header.key != "biz_rt");
        self.headers.push(Header {
            key: "biz_rt".into(),
            value: elapsed_millis.to_string(),
        });
        self.payload = serde_json::to_vec(&json!({"code":code,"headers":null,"data":null}))
            .expect("serializable response");
        self
    }
}

struct PartialMessage {
    parts: Vec<Option<Vec<u8>>>,
    bytes: usize,
    expires: Instant,
    service: i32,
}

#[derive(Default)]
pub(crate) struct Reassembler {
    partial: BTreeMap<String, PartialMessage>,
}

impl Reassembler {
    pub fn accept(&mut self, frame: &Frame) -> Result<Option<Vec<u8>>, GatewayError> {
        self.partial
            .retain(|_, partial| partial.expires > Instant::now());
        let number = |key, default| -> Result<usize, GatewayError> {
            frame.header(key).map_or(Ok(default), |value| {
                value
                    .parse()
                    .map_err(|_| GatewayError::Invalid("invalid Feishu split-message header"))
            })
        };
        let sum = number("sum", 1)?;
        let seq = number("seq", 0)?;
        if sum == 0 || sum > MAX_PARTS || seq >= sum || frame.payload.len() > MAX_REQUEST_BYTES {
            return Err(GatewayError::Invalid("Feishu split message exceeds bounds"));
        }
        if sum == 1 {
            return Ok(Some(frame.payload.clone()));
        }
        let id = frame
            .header("message_id")
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or(GatewayError::Invalid(
                "Feishu split message has no identity",
            ))?;
        if !self.partial.contains_key(id) && self.partial.len() >= MAX_PARTIAL_MESSAGES {
            return Err(GatewayError::Invalid("too many partial Feishu messages"));
        }
        let total: usize = self.partial.values().map(|partial| partial.bytes).sum();
        let partial = self
            .partial
            .entry(id.into())
            .or_insert_with(|| PartialMessage {
                parts: vec![None; sum],
                bytes: 0,
                expires: Instant::now() + PARTIAL_TTL,
                service: frame.service,
            });
        if partial.parts.len() != sum || partial.service != frame.service {
            return Err(GatewayError::Invalid(
                "conflicting Feishu split-message identity",
            ));
        }
        if let Some(previous) = &partial.parts[seq] {
            if previous != &frame.payload {
                return Err(GatewayError::Invalid(
                    "conflicting Feishu split-message content",
                ));
            }
            return Ok(None);
        }
        if partial.bytes + frame.payload.len() > MAX_REQUEST_BYTES
            || total + frame.payload.len() > MAX_PARTIAL_BYTES
        {
            return Err(GatewayError::Invalid(
                "Feishu partial message exceeds size limit",
            ));
        }
        partial.bytes += frame.payload.len();
        partial.parts[seq] = Some(frame.payload.clone());
        if partial.parts.iter().any(Option::is_none) {
            return Ok(None);
        }
        let partial = self.partial.remove(id).expect("partial exists");
        Ok(Some(
            partial.parts.into_iter().flatten().flatten().collect(),
        ))
    }
}

pub fn normalize_message(
    value: &Value,
    app_id: &str,
) -> Result<Option<ChannelEvent>, GatewayError> {
    if value.get("schema").and_then(Value::as_str) != Some("2.0") {
        return Err(GatewayError::Invalid("unsupported Feishu event schema"));
    }
    if value.pointer("/header/app_id").and_then(Value::as_str) != Some(app_id) {
        return Err(GatewayError::Invalid(
            "Feishu event application does not match this gateway",
        ));
    }
    if value.pointer("/header/event_type").and_then(Value::as_str) != Some("im.message.receive_v1")
    {
        return Ok(None);
    }
    if value
        .pointer("/event/sender/sender_type")
        .and_then(Value::as_str)
        != Some("user")
        || value
            .pointer("/event/message/chat_type")
            .and_then(Value::as_str)
            != Some("p2p")
    {
        return Ok(None);
    }
    let Some(message_type @ ("text" | "file" | "image")) = value
        .pointer("/event/message/message_type")
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let id = |pointer| -> Result<String, GatewayError> {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|id| valid_id(id))
            .map(str::to_owned)
            .ok_or(GatewayError::Invalid("invalid Feishu message identity"))
    };
    let event_id = id("/event/message/message_id")?;
    let sender_id = id("/event/sender/sender_id/open_id")?;
    let conversation_id = id("/event/message/chat_id")?;
    let content = value
        .pointer("/event/message/content")
        .and_then(Value::as_str)
        .filter(|content| content.len() <= MAX_REQUEST_BYTES)
        .ok_or(GatewayError::Invalid("invalid Feishu message content"))?;
    let content: Value = serde_json::from_str(content)
        .map_err(|_| GatewayError::Invalid("invalid Feishu message content"))?;
    let (text, attachments) = if message_type == "text" {
        let text = content
            .as_object()
            .filter(|object| object.len() == 1)
            .and_then(|object| object.get("text"))
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty() && text.len() <= MAX_TEXT_BYTES)
            .ok_or(GatewayError::Invalid(
                "invalid or oversized Feishu text message",
            ))?;
        (text.to_owned(), Vec::new())
    } else {
        (
            String::new(),
            vec![InboundResource::from_content(message_type, &content)?],
        )
    };
    Ok(Some(ChannelEvent {
        source: "feishu".into(),
        event_id: event_id.clone(),
        sender_id,
        reply_target: conversation_id.clone(),
        conversation_id,
        text,
        message_type: message_type.into(),
        metadata: BTreeMap::from([
            ("chat_type".into(), json!("single")),
            ("request_id".into(), json!(event_id)),
        ]),
        attachments,
    }))
}

pub(crate) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(id: usize) -> Frame {
        Frame {
            service: 1,
            method: 1,
            payload: vec![b'a'],
            headers: vec![
                Header {
                    key: "message_id".into(),
                    value: id.to_string(),
                },
                Header {
                    key: "sum".into(),
                    value: "2".into(),
                },
                Header {
                    key: "seq".into(),
                    value: "0".into(),
                },
            ],
            ..Frame::default()
        }
    }

    #[test]
    fn split_messages_and_protobuf_inputs_have_bounded_conflict_checked_admission() {
        let mut assembly = Reassembler::default();
        for id in 0..MAX_PARTIAL_MESSAGES {
            assert_eq!(assembly.accept(&part(id)).unwrap(), None);
        }
        assert!(assembly.accept(&part(MAX_PARTIAL_MESSAGES)).is_err());
        let mut conflicting = part(0);
        conflicting.payload = vec![b'b'];
        assert!(assembly.accept(&conflicting).is_err());
        let mut oversized = part(0);
        oversized.payload = vec![0; MAX_REQUEST_BYTES + 1];
        assert!(assembly.accept(&oversized).is_err());
        let mut invalid = part(0);
        invalid.headers[2].value = "64".into();
        assert!(assembly.accept(&invalid).is_err());
        invalid.headers[1].value = "65".into();
        assert!(assembly.accept(&invalid).is_err());
        assert!(Frame::decode_binary(&vec![0; MAX_FRAME_BYTES + 1]).is_err());
        let mut duplicate_header = Frame::ping(1);
        duplicate_header
            .headers
            .push(duplicate_header.headers[0].clone());
        assert!(Frame::decode_binary(&duplicate_header.encode_binary()).is_err());
    }
}
