use reqwest::{Client, redirect::Policy};
use serde_json::{Value, json};

use crate::{
    ChannelEvent, GatewayError, MAX_REQUEST_BYTES, MAX_TEXT_BYTES, WebhookConfig,
    ledger::{DeliveryReceipt, PendingSettlement},
};

pub(crate) struct WebhookReply {
    pub text: Option<String>,
    pub receipt: Option<DeliveryReceipt>,
    pub superseded: bool,
}

pub(crate) struct Webhook {
    client: Client,
    config: WebhookConfig,
}

impl Webhook {
    pub fn new(config: WebhookConfig) -> Result<Self, GatewayError> {
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .timeout(config.timeout)
            .build()
            .map_err(|_| GatewayError::Webhook)?;
        Ok(Self { client, config })
    }

    pub async fn handle(&self, event: &ChannelEvent) -> Result<WebhookReply, GatewayError> {
        let mut response = self.post_json(&json!({"event": event})).await?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(GatewayError::Webhook);
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_REQUEST_BYTES as u64)
        {
            return Err(GatewayError::Webhook);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| GatewayError::Webhook)? {
            if bytes.len() + chunk.len() > MAX_REQUEST_BYTES {
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
        if [
            "msg_item",
            "attachments",
            "image",
            "file",
            "voice",
            "video",
            "audio",
            "stream",
        ]
        .iter()
        .any(|key| {
            object.get(*key).is_some_and(|value| {
                !value.is_null() && value.as_array().is_none_or(|items| !items.is_empty())
            })
        }) {
            return Err(GatewayError::UnsupportedMedia);
        }
        let receipt = object.get("receipt").map(parse_receipt).transpose()?;
        let superseded = object.get("superseded") == Some(&json!(true));
        if superseded {
            return Ok(WebhookReply {
                text: None,
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
        if receipt.is_some() && text.is_none() {
            return Err(GatewayError::Invalid(
                "webhook receipt requires a text reply or superseded outcome",
            ));
        }
        Ok(WebhookReply {
            text,
            receipt,
            superseded: false,
        })
    }

    pub async fn settle(&self, settlement: &PendingSettlement) -> Result<(), GatewayError> {
        let response = self
            .post_json(&json!({
                "event": settlement.event,
                "settlement": {
                    "key": settlement.receipt.key,
                    "content_sha256": settlement.receipt.content_sha256,
                    "status": settlement.status.as_str(),
                }
            }))
            .await?;
        if response.status() == reqwest::StatusCode::OK {
            Ok(())
        } else {
            Err(GatewayError::Webhook)
        }
    }

    async fn post_json(&self, value: &Value) -> Result<reqwest::Response, GatewayError> {
        let body = serde_json::to_vec(value).map_err(|_| GatewayError::Webhook)?;
        if body.len() > MAX_REQUEST_BYTES {
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
