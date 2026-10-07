use reqwest::{Client, Url};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde_json::{Map, Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::Instant};

const API_BASE: &str = "https://qyapi.weixin.qq.com";

#[derive(Clone)]
pub struct Config {
    corp_id: String,
    agent_id: String,
    secret: String,
    api_base: Url,
}

impl Config {
    pub fn new(
        corp_id: impl Into<String>,
        agent_id: impl Into<String>,
        secret: impl Into<String>,
        api_base: &str,
    ) -> Result<Self, WecomError> {
        let api_base = Url::parse(api_base.trim_end_matches('/'))
            .map_err(|_| WecomError::Configuration("WECOM_API_BASE_URL is invalid"))?;
        if !matches!(api_base.scheme(), "http" | "https")
            || api_base.host_str().is_none()
            || !api_base.username().is_empty()
            || api_base.password().is_some()
            || api_base.query().is_some()
            || api_base.fragment().is_some()
        {
            return Err(WecomError::Configuration("WECOM_API_BASE_URL is invalid"));
        }
        Ok(Self {
            corp_id: corp_id.into().trim().to_owned(),
            agent_id: agent_id.into().trim().to_owned(),
            secret: secret.into().trim().to_owned(),
            api_base,
        })
    }

    pub fn from_env() -> Result<Self, WecomError> {
        let api_base = match std::env::var("WECOM_API_BASE_URL") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => API_BASE.to_owned(),
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(WecomError::Configuration("WECOM_API_BASE_URL is invalid"));
            }
        };
        Self::new(
            std::env::var("WECOM_CORP_ID").unwrap_or_default(),
            std::env::var("WECOM_AGENT_ID").unwrap_or_default(),
            std::env::var("WECOM_SECRET").unwrap_or_default(),
            &api_base,
        )
    }

    fn credentials(&self) -> Result<(), WecomError> {
        if self.corp_id.is_empty() {
            return Err(WecomError::Configuration("WECOM_CORP_ID is not configured"));
        }
        if self.secret.is_empty() {
            return Err(WecomError::Configuration("WECOM_SECRET is not configured"));
        }
        Ok(())
    }

    fn agent_id(&self) -> Result<i64, WecomError> {
        if self.agent_id.is_empty() {
            return Err(WecomError::Configuration(
                "WECOM_AGENT_ID is not configured",
            ));
        }
        self.agent_id
            .parse()
            .map_err(|_| WecomError::Configuration("WECOM_AGENT_ID must be an integer"))
    }

    fn endpoint(&self, path: &str) -> Url {
        let mut url = self.api_base.clone();
        url.set_path(&format!("{}{path}", url.path().trim_end_matches('/')));
        url
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WecomError {
    #[error("{0}")]
    Configuration(&'static str),
    #[error("{0}")]
    Validation(&'static str),
    #[error("WeCom API request failed")]
    Transport,
    #[error("WeCom API HTTP failure {0}")]
    Http(u16),
    #[error("WeCom API returned an invalid response")]
    Response,
    #[error("WeCom API error {0}")]
    Api(i64),
    #[error("WeCom API did not return access_token")]
    Token,
    #[error("WeCom message delivery is unknown: {0}; request was not retried")]
    UnknownDelivery(&'static str),
}

struct CachedToken {
    value: String,
    expires_at: Instant,
}

#[derive(Clone)]
pub struct WecomService {
    config: Config,
    client: Client,
    token: Arc<Mutex<Option<CachedToken>>>,
}

impl WecomService {
    pub fn new(config: Config) -> Result<Self, WecomError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| WecomError::Transport)?;
        Ok(Self {
            config,
            client,
            token: Arc::new(Mutex::new(None)),
        })
    }

    pub fn from_env() -> Result<Self, WecomError> {
        Self::new(Config::from_env()?)
    }

    async fn request(
        &self,
        path: &str,
        params: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Value, WecomError> {
        let mutating = body.is_some();
        let url = self.config.endpoint(path);
        let request = match body {
            Some(body) => self.client.post(url).json(&body),
            None => self.client.get(url),
        }
        .query(params);
        let response = request.send().await.map_err(|_| {
            if mutating {
                WecomError::UnknownDelivery("transport failure")
            } else {
                WecomError::Transport
            }
        })?;
        if !response.status().is_success() {
            return Err(if mutating {
                WecomError::UnknownDelivery("HTTP failure")
            } else {
                WecomError::Http(response.status().as_u16())
            });
        }
        let result: Value = response.json().await.map_err(|_| {
            if mutating {
                WecomError::UnknownDelivery("invalid response")
            } else {
                WecomError::Response
            }
        })?;
        if !result.is_object() {
            return Err(if mutating {
                WecomError::UnknownDelivery("non-object response")
            } else {
                WecomError::Response
            });
        }
        if let Some(errcode) = result.get("errcode") {
            match errcode.as_i64() {
                Some(0) => {}
                Some(code) => return Err(WecomError::Api(code)),
                None => {
                    return Err(if mutating {
                        WecomError::UnknownDelivery("invalid API status")
                    } else {
                        WecomError::Response
                    });
                }
            }
        }
        Ok(result)
    }

    async fn access_token(&self) -> Result<String, WecomError> {
        let mut cached = self.token.lock().await;
        let now = Instant::now();
        if let Some(token) = cached.as_ref()
            && token.expires_at.saturating_duration_since(now) > Duration::from_secs(60)
        {
            return Ok(token.value.clone());
        }
        self.config.credentials()?;
        let response = self
            .request(
                "/cgi-bin/gettoken",
                &[
                    ("corpid", &self.config.corp_id),
                    ("corpsecret", &self.config.secret),
                ],
                None,
            )
            .await?;
        let value = response
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(WecomError::Token)?
            .to_owned();
        let expires = match response.get("expires_in") {
            None => 7200.0,
            Some(Value::String(value)) => value.parse().map_err(|_| WecomError::Response)?,
            Some(value) => value.as_f64().ok_or(WecomError::Response)?,
        };
        if !expires.is_finite() {
            return Err(WecomError::Response);
        }
        let duration =
            Duration::try_from_secs_f64(expires.max(0.0)).map_err(|_| WecomError::Response)?;
        let expires_at = now.checked_add(duration).ok_or(WecomError::Response)?;
        *cached = Some(CachedToken {
            value: value.clone(),
            expires_at,
        });
        Ok(value)
    }

    async fn send(
        &self,
        arguments: Map<String, Value>,
        msgtype: &str,
    ) -> Result<Value, WecomError> {
        let content = required_text(&arguments, "content", "content is required")?;
        let mut body = Map::new();
        for name in ["touser", "toparty", "totag"] {
            match arguments.get(name) {
                None | Some(Value::Null) => {}
                Some(Value::String(value)) if value.is_empty() => {}
                Some(value) => {
                    let target = match value {
                        Value::String(value) => value.clone(),
                        Value::Bool(true) => "True".to_owned(),
                        Value::Bool(false) => "False".to_owned(),
                        Value::Number(value) => value.to_string(),
                        _ => return Err(WecomError::Validation("message target must be a string")),
                    };
                    body.insert(name.to_owned(), Value::String(target));
                }
            }
        }
        if body.is_empty() {
            return Err(WecomError::Validation(
                "one of touser, toparty, or totag is required",
            ));
        }
        body.insert("agentid".to_owned(), self.config.agent_id()?.into());
        body.insert("msgtype".to_owned(), msgtype.into());
        body.insert(msgtype.to_owned(), json!({"content": content}));
        let token = self.access_token().await?;
        self.request(
            "/cgi-bin/message/send",
            &[("access_token", &token)],
            Some(Value::Object(body)),
        )
        .await
    }

    pub async fn call(
        &self,
        name: &str,
        arguments: Map<String, Value>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = match name {
            "wecom_send_text" => self.send(arguments, "text").await,
            "wecom_send_markdown" => self.send(arguments, "markdown").await,
            "wecom_get_user" => match required_text(&arguments, "userid", "userid is required") {
                Ok(userid) => match self.access_token().await {
                    Ok(token) => {
                        self.request(
                            "/cgi-bin/user/get",
                            &[("access_token", &token), ("userid", userid)],
                            None,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            },
            _ => return Err(ErrorData::invalid_params("unknown tool", None)),
        };
        Ok(match result {
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
            Err(error) => {
                let mut value = json!({"error": error.to_string()});
                if matches!(error, WecomError::UnknownDelivery(_)) {
                    value["delivery"] = "unknown".into();
                }
                CallToolResult::error(vec![ContentBlock::text(value.to_string())])
            }
        })
    }
}

fn required_text<'arguments>(
    arguments: &'arguments Map<String, Value>,
    key: &str,
    error: &'static str,
) -> Result<&'arguments str, WecomError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(WecomError::Validation(error))
}

pub fn tools() -> Vec<Tool> {
    [
        (
            "wecom_send_text",
            "Send a text message through a WeCom application.",
            json!({"type":"object", "properties": {
                "content":{"type":"string"}, "touser":{"type":"string"},
                "toparty":{"type":"string"}, "totag":{"type":"string"}
            }, "required":["content"]}),
        ),
        (
            "wecom_send_markdown",
            "Send a Markdown message through a WeCom application.",
            json!({"type":"object", "properties": {
                "content":{"type":"string"}, "touser":{"type":"string"},
                "toparty":{"type":"string"}, "totag":{"type":"string"}
            }, "required":["content"]}),
        ),
        (
            "wecom_get_user",
            "Get a WeCom member by userid.",
            json!({"type":"object", "properties":{"userid":{"type":"string"}},
                "required":["userid"]}),
        ),
    ]
    .into_iter()
    .map(|(name, description, schema)| {
        Tool::new(
            name,
            description,
            Arc::new(schema.as_object().unwrap().clone()),
        )
    })
    .collect()
}

impl ServerHandler for WecomService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("wecom", "1.0.0"))
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(&request.name, request.arguments.unwrap_or_default())
            .await
    }
}
