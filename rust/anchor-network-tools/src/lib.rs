//! General HTTP tools. The Host/Sandbox grants connectivity, including for
//! business Plugins with their own network clients; this server is not a proxy.

mod package;
pub use package::package_plugin;

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use anchor_workspace_files::WorkspaceFiles;
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
use sha2::{Digest, Sha256};

type Error = Box<dyn std::error::Error + Send + Sync>;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_DOWNLOAD: usize = 64 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub url: String,
    #[serde(default = "get_method")]
    pub method: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
    #[serde(default = "text_limit")]
    pub max_bytes: usize,
}

fn get_method() -> String {
    "GET".to_owned()
}
fn text_limit() -> usize {
    64 * 1024
}
fn download_limit() -> usize {
    16 * 1024 * 1024
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Download {
    pub url: String,
    pub path: String,
    #[serde(default = "download_limit")]
    pub max_bytes: usize,
}

#[derive(Clone)]
pub struct NetworkServer {
    client: reqwest::Client,
    workspace: WorkspaceFiles,
}

impl NetworkServer {
    pub fn new(workspace: impl AsRef<Path>) -> Result<Self, Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::limited(5))
                .user_agent("Anchor-Network/0.1")
                .build()?,
            workspace: WorkspaceFiles::new(workspace)?,
        })
    }

    pub async fn request(&self, request: Request) -> Result<Value, Error> {
        if request.max_bytes == 0 || request.max_bytes > MAX_TEXT {
            return Err("max_bytes must be between 1 and 1048576".into());
        }
        let mut builder = self
            .client
            .request(request.method.parse()?, http_url(&request.url)?);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            if body.len() > MAX_TEXT {
                return Err("request body exceeds 1048576 bytes".into());
            }
            builder = builder.body(body);
        }
        let response = builder.send().await?;
        let status = response.status().as_u16();
        let url = response.url().to_string();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let (bytes, truncated) = read_body(response, request.max_bytes).await?;
        Ok(json!({"url":url,"status":status,"headers":headers,
            "body":String::from_utf8_lossy(&bytes),"truncated":truncated}))
    }

    pub async fn download(&self, request: Download) -> Result<Value, Error> {
        if request.max_bytes == 0 || request.max_bytes > MAX_DOWNLOAD {
            return Err("max_bytes must be between 1 and 67108864".into());
        }
        WorkspaceFiles::validate_relative(&request.path)?;
        let response = self
            .client
            .get(http_url(&request.url)?)
            .send()
            .await?
            .error_for_status()?;
        let url = response.url().to_string();
        let (bytes, truncated) = read_body(response, request.max_bytes).await?;
        if truncated {
            return Err("download exceeds max_bytes; no file was saved".into());
        }
        self.workspace.write_new(&request.path, &bytes)?;
        Ok(json!({"url":url,"path":request.path,"bytes":bytes.len(),
            "sha256":format!("{:x}",Sha256::digest(&bytes))}))
    }
}

fn http_url(value: &str) -> Result<reqwest::Url, Error> {
    let url = reqwest::Url::parse(value)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("url must use HTTP or HTTPS".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("provide credentials through headers, not URL userinfo".into());
    }
    Ok(url)
}

async fn read_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<(Vec<u8>, bool), Error> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = limit - bytes.len();
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            return Ok((bytes, true));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes, false))
}

fn tool(name: &'static str, description: &'static str, properties: Value, required: Value) -> Tool {
    let schema = json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
    Tool::new(
        name,
        description,
        Arc::new(schema.as_object().unwrap().clone()),
    )
}

impl ServerHandler for NetworkServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("anchor-network", env!("CARGO_PKG_VERSION")),
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
                    "request",
                    "Make an HTTP(S) request and return status, headers and bounded text. The default method is GET.",
                    json!({"url":{"type":"string"},"method":{"type":"string","default":"GET"},
                    "headers":{"type":"object","additionalProperties":{"type":"string"}},"body":{"type":"string"},
                    "max_bytes":{"type":"integer","minimum":1,"maximum":MAX_TEXT,"default":text_limit()}}),
                    json!(["url"]),
                ),
                tool(
                    "download",
                    "Download a file into a relative task workspace path without overwriting an existing file.",
                    json!({"url":{"type":"string"},"path":{"type":"string"},
                    "max_bytes":{"type":"integer","minimum":1,"maximum":MAX_DOWNLOAD,"default":download_limit()}}),
                    json!(["url", "path"]),
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
            "request" => match serde_json::from_value(arguments) {
                Ok(arguments) => self.request(arguments).await,
                Err(error) => Err(error.into()),
            },
            "download" => match serde_json::from_value(arguments) {
                Ok(arguments) => self.download(arguments).await,
                Err(error) => Err(error.into()),
            },
            _ => return Err(ErrorData::invalid_params("unknown tool", None)),
        };
        match result {
            Ok(value) => {
                let mut response =
                    CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
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
