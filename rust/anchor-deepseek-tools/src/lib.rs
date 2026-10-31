#![forbid(unsafe_code)]
mod package;
pub use package::package_plugin;

use anchor_workspace_files::WorkspaceFiles;
use chrono::Utc;
use reqwest::{Client, Url};
use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
const MAX_QUERY: usize = 128;
const MAX_URL: usize = 8192;
const MAX_PREVIEW: usize = 64 * 1024;

#[derive(Clone)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub workspace: std::path::PathBuf,
}

#[derive(Clone)]
pub struct DeepSeekServer {
    client: Client,
    endpoint: Url,
    api_key: Arc<String>,
    files: WorkspaceFiles,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_count")]
    pub count: u8,
    #[serde(default)]
    pub ori_question: Option<String>,
    #[serde(default)]
    pub localization: Option<String>,
    #[serde(default)]
    pub need_content: bool,
    pub save_as: String,
}

fn default_count() -> u8 {
    10
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchRequest {
    pub url: String,
    pub save_as: String,
}

impl DeepSeekServer {
    pub fn new(config: Config) -> Result<Self, Error> {
        let endpoint = endpoint_url(&config.base_url)?;
        if config.api_key.trim().is_empty() {
            return Err("api key must not be empty".into());
        }
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(45))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("Anchor-DeepSeek-Web/0.1")
                .build()?,
            endpoint,
            api_key: Arc::new(config.api_key),
            files: WorkspaceFiles::new(config.workspace)?,
        })
    }

    pub async fn search(&self, request: SearchRequest) -> Result<Value, Error> {
        if request.query.trim().is_empty() || request.query.chars().count() > MAX_QUERY {
            return Err("query must contain 1-128 characters".into());
        }
        if !(1..=50).contains(&request.count) {
            return Err("count must be between 1 and 50".into());
        }
        if request
            .ori_question
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.chars().count() > MAX_QUERY)
            || request
                .localization
                .as_ref()
                .is_some_and(|value| value.chars().count() > MAX_QUERY)
        {
            return Err(
                "ori_question must contain 1-128 characters; localization at most 128".into(),
            );
        }
        let mut body = json!({
            "query": request.query,
            "count": request.count,
            "need_content": request.need_content,
        });
        if let Some(value) = request.ori_question {
            body["ori_question"] = json!(value);
        }
        if let Some(value) = request.localization {
            body["localization"] = json!(value);
        }
        self.call("search", body, request.save_as).await
    }

    pub async fn fetch(&self, request: FetchRequest) -> Result<Value, Error> {
        if request.url.len() > MAX_URL {
            return Err("url exceeds 8192 bytes".into());
        }
        let url = source_url(&request.url)?;
        self.call("fetch", json!({"url": url}), request.save_as)
            .await
    }

    async fn call(&self, operation: &str, body: Value, save_as: String) -> Result<Value, Error> {
        WorkspaceFiles::validate_relative(&save_as)?;
        let url = self.endpoint.join(&format!("v1/tools/{operation}"))?;
        let result = self
            .client
            .post(url)
            .bearer_auth(self.api_key.as_str())
            .json(&body)
            .send()
            .await;
        let (status, response_value, complete) = match result {
            Ok(response) => {
                let status = response.status().as_u16();
                match response.bytes().await {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes)
                            .replace(self.api_key.as_str(), "[credential redacted]");
                        let value = serde_json::from_str::<Value>(&text)
                            .unwrap_or_else(|_| json!({"non_json_body": text}));
                        (Some(status), value, true)
                    }
                    Err(error) => (
                        Some(status),
                        json!({"transport_error":error.to_string()}),
                        false,
                    ),
                }
            }
            Err(error) => (None, json!({"transport_error":error.to_string()}), false),
        };
        let response_valid = match operation {
            "search" => response_value.get("results").is_some_and(Value::is_array),
            "fetch" => response_value
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|content| !content.trim().is_empty()),
            _ => false,
        };
        let envelope = json!({
            "format": 1,
            "kind": operation,
            "request": body,
            "fetched_at": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "http_status": status,
            "ok": complete && response_valid && status.is_some_and(|status| (200..300).contains(&status)),
            "response": response_value,
        });
        self.files
            .write_new(&save_as, envelope.to_string().as_bytes())?;
        Ok(preview(envelope, save_as))
    }
}

fn endpoint_url(base: &str) -> Result<Url, Error> {
    let mut url = Url::parse(base)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("base_url must be an HTTPS origin without URL credentials".into());
    }
    if !matches!(url.path().trim_end_matches('/'), "" | "/v1")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("base_url must be an HTTPS origin, optionally ending in /v1".into());
    }
    url.set_path("/");
    Ok(url)
}

fn source_url(value: &str) -> Result<String, Error> {
    let url = Url::parse(value)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("url must use HTTP or HTTPS without URL credentials".into());
    }
    Ok(value.to_owned())
}

fn preview(mut envelope: Value, save_as: String) -> Value {
    let mut truncated = false;
    if envelope["kind"] == "search"
        && let Some(results) = envelope["response"]["results"].as_array_mut()
    {
        truncated |= results.len() > 10;
        results.truncate(10);
        for result in results {
            let mut remaining = MAX_PREVIEW / 10;
            truncated |= truncate_strings(result, &mut remaining, 1024);
        }
    }
    let mut remaining = MAX_PREVIEW;
    truncated |= truncate_strings(&mut envelope["response"], &mut remaining, MAX_PREVIEW);
    envelope["response_truncated"] = Value::Bool(truncated);
    envelope["saved_as"] = Value::String(save_as);
    envelope
}

fn truncate_strings(value: &mut Value, remaining: &mut usize, per_string: usize) -> bool {
    match value {
        Value::String(text) => {
            let count = text.len();
            let mut keep = count.min(*remaining).min(per_string);
            while !text.is_char_boundary(keep) {
                keep -= 1;
            }
            *remaining -= keep;
            if keep < count {
                text.truncate(keep);
                text.push_str(" [preview truncated]");
            }
            keep < count
        }
        Value::Array(values) => values.iter_mut().fold(false, |truncated, value| {
            truncate_strings(value, remaining, per_string) | truncated
        }),
        Value::Object(values) => values.values_mut().fold(false, |truncated, value| {
            truncate_strings(value, remaining, per_string) | truncated
        }),
        _ => false,
    }
}

fn tool(name: &'static str, description: &'static str, properties: Value, required: Value) -> Tool {
    let schema = json!({
        "type":"object",
        "properties":properties,
        "additionalProperties":false,
        "required":required
    });
    Tool::new(
        name,
        description,
        Arc::new(schema.as_object().unwrap().clone()),
    )
}

impl ServerHandler for DeepSeekServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("anchor-deepseek", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: vec![
                tool(
                    "search",
                    "Search the live web with DeepSeek Search API. Save the complete raw response and request metadata to a new workspace JSON file; returned response is bounded. Search snippets are discovery evidence; fetch important originals.",
                    json!({
                        "query":{"type":"string","minLength":1,"maxLength":128},
                        "count":{"type":"integer","minimum":1,"maximum":50,"default":10},
                        "ori_question":{"type":"string","minLength":1,"maxLength":128},
                        "localization":{"type":"string","maxLength":128},
                        "need_content":{"type":"boolean","default":false},
                        "save_as":{"type":"string","description":"New relative JSON evidence path inside /workspace; existing files are never overwritten."}
                    }),
                    json!(["query", "save_as"]),
                ),
                tool(
                    "fetch",
                    "Fetch a source URL with DeepSeek Fetch API and save the complete raw response plus request metadata to a new workspace JSON file; returned content is bounded.",
                    json!({
                        "url":{"type":"string","maxLength":MAX_URL},
                        "save_as":{"type":"string","description":"New relative JSON evidence path inside /workspace; existing files are never overwritten."}
                    }),
                    json!(["url", "save_as"]),
                ),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let arguments = json!(request.arguments.unwrap_or_default());
        let result = match request.name.as_ref() {
            "search" => match serde_json::from_value(arguments) {
                Ok(request) => self.search(request).await,
                Err(error) => Err(error.into()),
            },
            "fetch" => match serde_json::from_value(arguments) {
                Ok(request) => self.fetch(request).await,
                Err(error) => Err(error.into()),
            },
            _ => return Err(ErrorData::invalid_params("unknown tool", None)),
        };
        match result {
            Ok(value) => {
                let mut response = if value["ok"] == true {
                    CallToolResult::success(vec![ContentBlock::text(value.to_string())])
                } else {
                    CallToolResult::error(vec![ContentBlock::text(value.to_string())])
                };
                response.structured_content = Some(value);
                Ok(response)
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }
}

#[cfg(test)]
mod tests;
