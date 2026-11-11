//! Durable inbound events retain resource descriptors, never downloaded bytes.

use anchor_channel_transport::media::MAX_OUTBOUND_FILE_BYTES;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ChannelEvent, GatewayError, api::Api, protocol::valid_id};

const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ResourceKind {
    File,
    Image,
}

impl ResourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Image => "image",
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboundResource {
    kind: ResourceKind,
    key: String,
    name: String,
}

impl InboundResource {
    pub fn from_content(kind: &str, content: &Value) -> Result<Value, GatewayError> {
        let object = content
            .as_object()
            .ok_or(GatewayError::Invalid("invalid Feishu media content"))?;
        let (kind, key_field, name) = match kind {
            "image" if object.len() == 1 => (ResourceKind::Image, "image_key", "image"),
            "file" if object.len() == 2 => (
                ResourceKind::File,
                "file_key",
                object
                    .get("file_name")
                    .and_then(Value::as_str)
                    .filter(|name| safe_name(name))
                    .ok_or(GatewayError::Invalid("invalid Feishu inbound filename"))?,
            ),
            _ => return Err(GatewayError::Invalid("invalid Feishu media content")),
        };
        let key = object
            .get(key_field)
            .and_then(Value::as_str)
            .filter(|key| valid_id(key))
            .ok_or(GatewayError::Invalid("invalid Feishu resource key"))?;
        serde_json::to_value(Self {
            kind,
            key: key.into(),
            name: name.into(),
        })
        .map_err(|_| GatewayError::Invalid("invalid Feishu resource descriptor"))
    }
}

pub(crate) async fn enrich(
    api: &Api,
    original: &ChannelEvent,
) -> Result<ChannelEvent, GatewayError> {
    if original.message_type == "text" && original.attachments.is_empty() {
        return Ok(original.clone());
    }
    let [descriptor] = original.attachments.as_slice() else {
        return Err(GatewayError::Invalid(
            "Feishu media event requires one resource",
        ));
    };
    let descriptor: InboundResource = serde_json::from_value(descriptor.clone())
        .map_err(|_| GatewayError::Invalid("invalid stored Feishu resource"))?;
    if descriptor.kind.as_str() != original.message_type || !safe_name(&descriptor.name) {
        return Err(GatewayError::Invalid(
            "Feishu resource type or filename changed",
        ));
    }
    let token = api.token().await?;
    let bytes = api
        .resource(
            &original.event_id,
            &descriptor.key,
            descriptor.kind.as_str(),
            match descriptor.kind {
                ResourceKind::File => MAX_OUTBOUND_FILE_BYTES,
                ResourceKind::Image => MAX_IMAGE_BYTES,
            },
            &token,
        )
        .await?;
    let image = image_format(&bytes);
    if matches!(descriptor.kind, ResourceKind::Image) && image.is_none() {
        return Err(GatewayError::Invalid(
            "only PNG, JPEG and WebP inbound images are supported",
        ));
    }
    if image.is_some() && bytes.len() > MAX_IMAGE_BYTES {
        return Err(GatewayError::Invalid("inbound image exceeds 10 MiB"));
    }
    let name = match (descriptor.kind, image) {
        (ResourceKind::Image, Some((_, extension))) => format!("image.{extension}"),
        _ => descriptor.name,
    };
    let mut attachment = json!({"name":name,"data_base64":STANDARD.encode(bytes)});
    if let Some((mime, _)) = image {
        attachment["media_type"] = json!(mime);
    }
    let mut enriched = original.clone();
    enriched.attachments = vec![attachment];
    Ok(enriched)
}

fn safe_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= 255
        && !matches!(name, "." | "..")
        && !name
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
}

fn image_format(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}
