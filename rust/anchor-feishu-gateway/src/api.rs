use std::time::{Duration, Instant};

use anchor_channel_transport::{http_client, read_bounded, validate_endpoint_url};
use reqwest::multipart;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{GatewayConfig, GatewayError, MAX_REQUEST_BYTES, protocol::valid_id};

const UPLOAD_FAILED: GatewayError = GatewayError::Invalid(
    "Feishu file upload failed; no message was dispatched. Verify application-identity permission im:resource:upload (or im:resource), publish the app version, and check file constraints. Renaming a file does not fix missing upload permission.",
);

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub(crate) struct ClientConfig {
    pub reconnect_count: Option<i32>,
    pub reconnect_interval: Option<u64>,
    pub reconnect_nonce: Option<u64>,
    pub ping_interval: Option<u64>,
}

pub(crate) struct Endpoint {
    pub url: String,
    pub service_id: i32,
    pub config: ClientConfig,
}

struct AccessToken {
    value: String,
    expires: Instant,
}

#[derive(Debug)]
pub(crate) enum ApiError {
    Rejected,
    Unknown,
}

impl From<ApiError> for GatewayError {
    fn from(_: ApiError) -> Self {
        Self::Disconnected
    }
}

pub(crate) struct Api {
    client: reqwest::Client,
    base: reqwest::Url,
    app_id: String,
    app_secret: String,
    token: Mutex<Option<AccessToken>>,
}

impl Api {
    pub fn new(config: &GatewayConfig) -> Result<Self, GatewayError> {
        Ok(Self {
            client: http_client(Some(config.timing.http_timeout))?,
            base: reqwest::Url::parse(&config.api_base_url)
                .map_err(|_| GatewayError::Invalid("invalid Feishu API base URL"))?,
            app_id: config.app_id.clone(),
            app_secret: config.app_secret.clone(),
            token: Mutex::new(None),
        })
    }

    pub async fn endpoint(&self) -> Result<Endpoint, GatewayError> {
        let value = self
            .post(
                &["callback", "ws", "endpoint"],
                &json!({"AppID":self.app_id,"AppSecret":self.app_secret}),
                None,
                &[],
            )
            .await?;
        let data = value.get("data").ok_or(GatewayError::Disconnected)?;
        let url = data
            .get("URL")
            .and_then(Value::as_str)
            .filter(|url| url.len() <= 8192)
            .ok_or(GatewayError::Disconnected)?;
        validate_endpoint_url(url)?;
        let parsed = reqwest::Url::parse(url).map_err(|_| GatewayError::Disconnected)?;
        // An insecure bootstrap can only direct credentials to the same literal
        // loopback fixture; a production bootstrap must return a TLS endpoint.
        if self.base.scheme() == "https" && parsed.scheme() != "wss" {
            return Err(GatewayError::Invalid(
                "Feishu bootstrap returned an insecure endpoint",
            ));
        }
        if self.base.scheme() == "http" && parsed.host_str() != self.base.host_str() {
            return Err(GatewayError::Invalid(
                "loopback bootstrap returned a different host",
            ));
        }
        let services: Vec<_> = parsed
            .query_pairs()
            .filter(|(key, _)| key == "service_id")
            .map(|(_, value)| value.parse::<i32>())
            .collect();
        let service_id = match services.as_slice() {
            [Ok(id)] if *id > 0 => *id,
            _ => {
                return Err(GatewayError::Invalid(
                    "Feishu endpoint has no unique service identity",
                ));
            }
        };
        let config = data
            .get("ClientConfig")
            .filter(|value| !value.is_null())
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|_| GatewayError::Invalid("invalid Feishu connection configuration"))?
            .unwrap_or_default();
        Ok(Endpoint {
            url: url.into(),
            service_id,
            config,
        })
    }

    /// Token lookup precedes the durable send claim. A token failure therefore
    /// never turns an undispatched reply into an unknown external effect.
    pub async fn token(&self) -> Result<String, GatewayError> {
        let mut cached = self.token.lock().await;
        if let Some(token) = cached
            .as_ref()
            .filter(|token| token.expires > Instant::now())
        {
            return Ok(token.value.clone());
        }
        let value = self
            .post(
                &["open-apis", "auth", "v3", "tenant_access_token", "internal"],
                &json!({"app_id":self.app_id,"app_secret":self.app_secret}),
                None,
                &[],
            )
            .await?;
        let token = value
            .get("tenant_access_token")
            .and_then(Value::as_str)
            .filter(|value| {
                !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
            })
            .ok_or(GatewayError::Disconnected)?;
        let expire = value
            .get("expire")
            .and_then(Value::as_u64)
            .filter(|expire| (1..=86_400).contains(expire))
            .ok_or(GatewayError::Disconnected)?;
        *cached = Some(AccessToken {
            value: token.into(),
            expires: Instant::now() + Duration::from_secs(expire.saturating_sub(60)),
        });
        Ok(token.into())
    }

    pub async fn reply(
        &self,
        message_id: &str,
        card: &str,
        token: &str,
    ) -> Result<String, GatewayError> {
        if !valid_id(message_id) {
            return Err(GatewayError::Invalid(
                "invalid Feishu reply message identity",
            ));
        }
        let value = self
            .post(
                &["open-apis", "im", "v1", "messages", message_id, "reply"],
                &json!({"msg_type":"interactive","content":card}),
                Some(token),
                &[],
            )
            .await
            .map_err(|_| GatewayError::Unconfirmed)?;
        sent_message_id(&value)
    }

    pub async fn send(
        &self,
        open_id: &str,
        card: &str,
        token: &str,
    ) -> Result<String, GatewayError> {
        self.send_content(open_id, "interactive", card, token).await
    }

    pub async fn send_file(
        &self,
        open_id: &str,
        file_key: &str,
        token: &str,
    ) -> Result<String, GatewayError> {
        self.send_content(
            open_id,
            "file",
            &json!({"file_key":file_key}).to_string(),
            token,
        )
        .await
    }

    async fn send_content(
        &self,
        open_id: &str,
        kind: &str,
        content: &str,
        token: &str,
    ) -> Result<String, GatewayError> {
        let value = self
            .post(
                &["open-apis", "im", "v1", "messages"],
                &json!({"receive_id":open_id,"msg_type":kind,"content":content}),
                Some(token),
                &[("receive_id_type", "open_id")],
            )
            .await
            .map_err(|_| GatewayError::Unconfirmed)?;
        sent_message_id(&value)
    }

    /// The update API returns `data:{}`, not the sent-message response shape.
    pub async fn update_card(
        &self,
        message_id: &str,
        card: &str,
        token: &str,
    ) -> Result<(), ApiError> {
        if !valid_id(message_id) {
            return Err(ApiError::Rejected);
        }
        let request = self
            .client
            .patch(
                self.url(&["open-apis", "im", "v1", "messages", message_id], &[])
                    .map_err(|_| ApiError::Rejected)?,
            )
            .bearer_auth(token)
            .json(&json!({"content":card}));
        self.json_response(request).await.map(|_| ())
    }

    /// Upload is retryable: only the later message send receives a durable claim.
    pub async fn upload_file(
        &self,
        name: &str,
        bytes: Vec<u8>,
        token: &str,
    ) -> Result<String, GatewayError> {
        let form = multipart::Form::new()
            .text("file_type", "stream")
            .text("file_name", name.to_owned())
            .part(
                "file",
                multipart::Part::bytes(bytes).file_name(name.to_owned()),
            );
        let request = self
            .client
            .post(self.url(&["open-apis", "im", "v1", "files"], &[])?)
            .bearer_auth(token)
            .multipart(form);
        let value = self
            .json_response(request)
            .await
            .map_err(|_| UPLOAD_FAILED)?;
        value
            .pointer("/data/file_key")
            .and_then(Value::as_str)
            .filter(|key| valid_id(key))
            .map(str::to_owned)
            .ok_or(UPLOAD_FAILED)
    }

    /// Resource retrieval is always bound to this admitted message and its key;
    /// no URL from an event or model can become an authenticated request.
    pub async fn resource(
        &self,
        message_id: &str,
        key: &str,
        kind: &str,
        limit: usize,
        token: &str,
    ) -> Result<Vec<u8>, GatewayError> {
        if !valid_id(message_id) || !valid_id(key) || !matches!(kind, "image" | "file") {
            return Err(GatewayError::Invalid("invalid Feishu resource identity"));
        }
        let response = self
            .client
            .get(self.url(
                &[
                    "open-apis",
                    "im",
                    "v1",
                    "messages",
                    message_id,
                    "resources",
                    key,
                ],
                &[("type", kind)],
            )?)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| GatewayError::Media)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(GatewayError::Media);
        }
        let bytes = read_bounded(response, limit)
            .await
            .map_err(|_| GatewayError::Media)?;
        if bytes.is_empty() {
            return Err(GatewayError::Media);
        }
        Ok(bytes)
    }

    async fn post(
        &self,
        path: &[&str],
        body: &Value,
        token: Option<&str>,
        query: &[(&str, &str)],
    ) -> Result<Value, GatewayError> {
        let mut request = self.client.post(self.url(path, query)?).json(body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        self.json_response(request).await.map_err(Into::into)
    }

    fn url(&self, path: &[&str], query: &[(&str, &str)]) -> Result<reqwest::Url, GatewayError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| GatewayError::Disconnected)?
            .clear()
            .extend(path.iter().copied());
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        Ok(url)
    }

    async fn json_response(&self, request: reqwest::RequestBuilder) -> Result<Value, ApiError> {
        let response = request.send().await.map_err(|_| ApiError::Unknown)?;
        let status = response.status();
        if status != reqwest::StatusCode::OK && !status.is_client_error() {
            return Err(ApiError::Unknown);
        }
        let bytes = read_bounded(response, MAX_REQUEST_BYTES)
            .await
            .map_err(|_| ApiError::Unknown)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| ApiError::Unknown)?;
        match value.get("code").and_then(Value::as_i64) {
            Some(0) if status == reqwest::StatusCode::OK => Ok(value),
            // Only an explicit API rejection permits a different delivery path.
            // A timeout, malformed response or server error may have taken effect.
            Some(code) if code != 0 => Err(ApiError::Rejected),
            _ => Err(ApiError::Unknown),
        }
    }
}

fn sent_message_id(value: &Value) -> Result<String, GatewayError> {
    value
        .pointer("/data/message_id")
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))
        .map(str::to_owned)
        .ok_or(GatewayError::Unconfirmed)
}
