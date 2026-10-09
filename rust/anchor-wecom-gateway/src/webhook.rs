use reqwest::{Client, redirect::Policy};
use serde_json::{Value, json};

use crate::{
    ChannelEvent, GatewayError, MAX_MEDIA_REQUEST_BYTES, MAX_MEDIA_RESPONSE_BYTES,
    MAX_REQUEST_BYTES, MAX_TEXT_BYTES, WebhookConfig,
    ledger::{DeliveryReceipt, PendingSettlement},
    media::{ImageItem, rejected_response_field, reply_images},
};

pub(crate) struct WebhookReply {
    pub text: Option<String>,
    pub items: Vec<ImageItem>,
    pub receipt: Option<DeliveryReceipt>,
    pub superseded: bool,
}

pub(crate) struct Webhook {
    client: Client,
    config: WebhookConfig,
}

impl Webhook {
    /// The Host authenticates every channel route with this key, including the
    /// read-only progress projection.
    pub(crate) fn api_key(&self) -> &str {
        &self.config.api_key
    }

    pub fn new(config: WebhookConfig) -> Result<Self, GatewayError> {
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .retry(reqwest::retry::never());
        if let Some(timeout) = config.timeout {
            builder = builder.timeout(timeout);
        }
        let client = builder.build().map_err(|_| GatewayError::Webhook)?;
        Ok(Self { client, config })
    }

    pub async fn handle(&self, event: &ChannelEvent) -> Result<WebhookReply, GatewayError> {
        // Only an event that carries fetched media may exceed the text bound.
        let limit = if event.attachments.is_empty() {
            MAX_REQUEST_BYTES
        } else {
            MAX_MEDIA_REQUEST_BYTES
        };
        let mut response = self.post_json(&json!({"event": event}), limit).await?;
        if response.status() != reqwest::StatusCode::OK {
            eprintln!(
                "anchor-wecom-gateway: channel webhook returned HTTP {}",
                response.status().as_u16()
            );
            // 400/409/422 mean the Host will never accept this event; retrying
            // it after a restart only repeats the refusal.
            return Err(match response.status().as_u16() {
                400 | 409 | 422 => GatewayError::Rejected,
                _ => GatewayError::Webhook,
            });
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_MEDIA_RESPONSE_BYTES as u64)
        {
            return Err(GatewayError::Webhook);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| GatewayError::Webhook)? {
            if bytes.len() + chunk.len() > MAX_MEDIA_RESPONSE_BYTES {
                return Err(GatewayError::Webhook);
            }
            bytes.extend_from_slice(&chunk);
        }
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

    pub async fn settle(&self, settlement: &PendingSettlement) -> Result<(), GatewayError> {
        // A settlement identifies the event and its receipt; the Host never
        // accepts attachment content on this path, and fetched media is not
        // resendable. The identity fields must survive unchanged.
        let mut event = settlement.event.clone();
        event.attachments.clear();
        let response = self
            .post_json(
                &json!({
                    "event": event,
                    "settlement": {
                        "key": settlement.receipt.key,
                        "content_sha256": settlement.receipt.content_sha256,
                        "status": settlement.status.as_str(),
                    }
                }),
                MAX_REQUEST_BYTES,
            )
            .await?;
        if response.status() == reqwest::StatusCode::OK {
            Ok(())
        } else if matches!(response.status().as_u16(), 400 | 404 | 409 | 422) {
            // The Host will never accept this receipt; retrying it only replays
            // a refusal forever.
            Err(GatewayError::Rejected)
        } else {
            Err(GatewayError::Webhook)
        }
    }

    async fn post_json(
        &self,
        value: &Value,
        limit: usize,
    ) -> Result<reqwest::Response, GatewayError> {
        let body = serde_json::to_vec(value).map_err(|_| GatewayError::Webhook)?;
        if body.len() > limit {
            return Err(GatewayError::Invalid("webhook request exceeds size limit"));
        }
        self.client
            .post(&self.config.url)
            .bearer_auth(&self.config.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| GatewayError::Webhook)
    }
}

pub(crate) fn parse_receipt(value: &Value) -> Result<DeliveryReceipt, GatewayError> {
    let object = value
        .as_object()
        .ok_or(GatewayError::Invalid("webhook receipt has invalid shape"))?;
    if object.len() != 2 || !object.contains_key("key") || !object.contains_key("content_sha256") {
        return Err(GatewayError::Invalid("webhook receipt has invalid fields"));
    }
    let key = object
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| {
            (9..=256).contains(&key.len())
                && key.starts_with("channel-")
                && key[8..].bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
                })
        })
        .ok_or(GatewayError::Invalid("webhook receipt key is invalid"))?;
    let content_sha256 = object
        .get("content_sha256")
        .and_then(Value::as_str)
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or(GatewayError::Invalid("webhook receipt digest is invalid"))?;
    Ok(DeliveryReceipt {
        key: key.to_owned(),
        content_sha256: content_sha256.to_owned(),
    })
}
