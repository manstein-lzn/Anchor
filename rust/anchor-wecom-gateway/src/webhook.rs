use anchor_channel_transport::WebhookClient;
pub(crate) use anchor_channel_transport::parse_receipt;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::{
    ChannelEvent, GatewayError, MAX_MEDIA_REQUEST_BYTES, MAX_MEDIA_RESPONSE_BYTES,
    MAX_REQUEST_BYTES, MAX_TEXT_BYTES, WebhookConfig,
    ledger::DeliveryReceipt,
    media::{MediaItem, rejected_response_field, reply_images},
};

pub(crate) struct WebhookReply {
    pub text: Option<String>,
    pub items: Vec<MediaItem>,
    pub receipt: Option<DeliveryReceipt>,
    pub superseded: bool,
}

pub(crate) struct Webhook {
    pub(crate) client: Arc<WebhookClient>,
}

impl Webhook {
    pub(crate) fn api_key(&self) -> &str {
        self.client.api_key()
    }

    pub fn new(config: WebhookConfig) -> Result<Self, GatewayError> {
        Ok(Self {
            client: Arc::new(WebhookClient::new(config)?),
        })
    }

    pub async fn handle(&self, event: &ChannelEvent) -> Result<WebhookReply, GatewayError> {
        // Only an event that carries fetched media may exceed the text bound.
        let limit = if event.attachments.is_empty() {
            MAX_REQUEST_BYTES
        } else {
            MAX_MEDIA_REQUEST_BYTES
        };
        let bytes = self
            .client
            .handle(event, limit, MAX_MEDIA_RESPONSE_BYTES)
            .await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| GatewayError::Webhook)?;
        let object = value.as_object().ok_or(GatewayError::Webhook)?;
        if object
            .get("error")
            .is_some_and(|value| !value.is_null() && value != &json!(false) && value != "")
        {
            return Err(GatewayError::Webhook);
        }
        if rejected_response_field(object).is_some() {
            return Err(GatewayError::UnsupportedMedia);
        }
        let items = reply_images(object.get("msg_item"))?;
        // Only a reply that actually carries validated images may exceed the
        // text-only control-response bound.
        if items.is_empty() && bytes.len() > MAX_REQUEST_BYTES {
            return Err(GatewayError::Webhook);
        }
        let receipt = object.get("receipt").map(parse_receipt).transpose()?;
        let superseded = object.get("superseded") == Some(&json!(true));
        if superseded {
            return Ok(WebhookReply {
                text: None,
                items: Vec::new(),
                receipt,
                superseded,
            });
        }
        let text = object.get("text").or_else(|| object.get("reply"));
        let text = match text {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if text.trim().is_empty() => None,
            Some(Value::String(text)) if text.len() <= MAX_TEXT_BYTES => Some(text.clone()),
            _ => {
                return Err(GatewayError::Invalid(
                    "webhook text reply exceeds limits or has invalid type",
                ));
            }
        };
        if receipt.is_some() && text.is_none() && items.is_empty() {
            return Err(GatewayError::Invalid(
                "webhook receipt requires a text reply, image reply or superseded outcome",
            ));
        }
        Ok(WebhookReply {
            text,
            items,
            receipt,
            superseded: false,
        })
    }
}
