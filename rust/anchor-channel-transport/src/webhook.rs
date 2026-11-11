use std::{fmt, time::Duration};

use serde_json::{Value, json};

use crate::{
    ChannelEvent, GatewayError, MAX_REQUEST_BYTES, http_client,
    ledger::{DeliveryReceipt, PendingSettlement},
    read_bounded,
};

pub struct WebhookConfig {
    pub url: String,
    pub api_key: String,
    pub timeout: Option<Duration>,
}

impl fmt::Debug for WebhookConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebhookConfig")
            .field("credentials", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

pub struct WebhookClient {
    client: reqwest::Client,
    config: WebhookConfig,
}

impl WebhookClient {
    pub fn new(config: WebhookConfig) -> Result<Self, GatewayError> {
        Ok(Self {
            client: http_client(config.timeout)?,
            config,
        })
    }

    pub fn api_key(&self) -> &str {
        &self.config.api_key
    }

    /// Transport-specific media parsing stays with the adapter. HTTP identity,
    /// limits and permanent refusal classification have one implementation.
    pub async fn handle(
        &self,
        event: &ChannelEvent,
        request_limit: usize,
        response_limit: usize,
    ) -> Result<Vec<u8>, GatewayError> {
        let response = self
            .post_json(&json!({"event": event}), request_limit)
            .await?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(match status {
                409 => {
                    if read_bounded(response, MAX_REQUEST_BYTES)
                        .await
                        .ok()
                        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
                        .is_some_and(|body| body["code"] == "assistant_stopped")
                    {
                        GatewayError::AssistantStopped
                    } else {
                        GatewayError::Rejected
                    }
                }
                400 | 422 => GatewayError::Rejected,
                _ => GatewayError::Webhook,
            });
        }
        read_bounded(response, response_limit).await
    }

    pub async fn settle(&self, settlement: &PendingSettlement) -> Result<(), GatewayError> {
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
        match response.status().as_u16() {
            200 => Ok(()),
            400 | 404 | 409 | 422 => Err(GatewayError::Rejected),
            _ => Err(GatewayError::Webhook),
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

pub fn parse_receipt(value: &Value) -> Result<DeliveryReceipt, GatewayError> {
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
