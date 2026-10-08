//! Local-only MCP fixture for real-provider Graph acceptance. No business APIs.
//! Prints its ephemeral endpoint; optional first argument receives call evidence.
use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::local::LocalSessionManager,
    },
};
use serde_json::json;
use std::{
    fs::OpenOptions,
    io::{BufRead, Write},
    path::PathBuf,
    sync::Arc,
};

fn stdio_fixture(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mode = arguments
        .first()
        .map(String::as_str)
        .ok_or("stdio fixture mode is required")?;
    if !["inspect", "echo"].contains(&mode) {
        return Err("unknown stdio fixture mode".into());
    }
    let prefix = arguments
        .iter()
        .position(|value| value == "--prefix")
        .and_then(|position| arguments.get(position + 1))
        .map(String::as_str)
        .unwrap_or("");
    let mut stdout = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let request: serde_json::Value = serde_json::from_str(&line?)?;
        let Some(identity) = request.get("id") else {
            continue;
        };
        let method = request["method"].as_str().ok_or("missing fixture method")?;
        if mode == "echo" {
            writeln!(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("/workspace/mcp-log")?,
                "{method}"
            )?;
        }
        let result = match method {
            "initialize" => {
                json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"stdio-fixture","version":"1"}})
            }
            "tools/list" if mode == "inspect" => {
                json!({"tools":[{"name":"inspect","description":"Inspect only granted input","inputSchema":{"type":"object","properties":{}}}]})
            }
            "tools/list" => {
                json!({"tools":[{"name":"echo","description":"Echo with installed dependency prefix","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]})
            }
            "tools/call" => {
                let text = if mode == "inspect" {
                    let path = std::path::Path::new("/local-inputs/history/history.txt");
                    let value = if path.is_file() {
                        json!({"visible":true,"text":std::fs::read_to_string(path)?,"readonly":std::fs::write("/local-inputs/history/forbidden", "changed").is_err()})
                    } else {
                        json!({"visible":false})
                    };
                    value.to_string()
                } else {
                    format!(
                        "{prefix}{}",
                        request["params"]["arguments"]["value"]
                            .as_str()
                            .ok_or("fixture value must be a string")?
                    )
                };
                json!({"content":[{"type":"text","text":text}]})
            }
            _ => json!({}),
        };
        writeln!(
            stdout,
            "{}",
            json!({"jsonrpc":"2.0","id":identity,"result":result})
        )?;
        stdout.flush()?;
    }
    Ok(())
}

#[derive(Clone)]
struct Fixture {
    evidence: Option<PathBuf>,
    append_evidence: bool,
    large_tools: usize,
}
impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let target = Tool::new(
            "fixture_suffix".to_owned(),
            "Return text with -checked appended. Deterministic local acceptance fixture."
                .to_owned(),
            Arc::new(
                serde_json::from_value(json!({
                    "type":"object",
                    "properties":{"text":{"type":"string"}},
                    "required":["text"],
                    "additionalProperties":false
                }))
                .unwrap(),
            ),
        );
        let mut tools = vec![target];
        for index in 0..self.large_tools {
            tools.push(Tool::new(
                format!("fixture_noise_{index:04}"),
                format!(
                    "Unrelated capability {index}; do not select this operation for the requested transformation. {}",
                    "metadata ".repeat(32)
                ),
                Arc::new(
                    serde_json::from_value(json!({
                        "type":"object",
                        "properties": {
                            "value": {"type":"string"},
                            "scope": {"type":"string"},
                            "limit": {"type":"integer"}
                        },
                        "required":["value"],
                        "additionalProperties":false
                    }))
                    .unwrap(),
                ),
            ));
        }
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if request.name != "fixture_suffix" {
            return Err(ErrorData::invalid_params("unknown tool", None));
        }
        let args = request
            .arguments
            .ok_or_else(|| ErrorData::invalid_params("missing arguments", None))?;
        if args.len() != 1 {
            return Err(ErrorData::invalid_params("only text is supported", None));
        }
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("text must be a string", None))?;
        let output = json!({"text":format!("{text}-checked")});
        if let Some(path) = &self.evidence {
            let bytes =
                serde_json::to_vec(&json!({"tool":"fixture_suffix","input":text,"output":output}))
                    .unwrap();
            if self.append_evidence {
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|_| ErrorData::internal_error("fixture evidence open failed", None))?;
                file.write_all(&bytes)
                    .and_then(|()| file.write_all(b"\n"))
                    .and_then(|()| file.sync_all())
                    .map_err(|_| {
                        ErrorData::internal_error("fixture evidence write failed", None)
                    })?;
            } else {
                std::fs::write(path, bytes).map_err(|_| {
                    ErrorData::internal_error("fixture evidence write failed", None)
                })?;
            }
        }
        let mut result =
            CallToolResult::success(vec![ContentBlock::text("Local fixture completed")]);
        result.structured_content = Some(output);
        Ok(result)
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|argument| argument == "--stdio") {
        return stdio_fixture(&args[2..]);
    }
    let fixture = Fixture {
        evidence: args
            .get(1)
            .filter(|arg| !arg.starts_with('-'))
            .map(PathBuf::from),
        append_evidence: args.iter().any(|arg| arg == "--append-evidence"),
        large_tools: args
            .iter()
            .position(|arg| arg == "--large-tools")
            .and_then(|position| args.get(position + 1))
            .map(|value| value.parse::<usize>())
            .transpose()?
            .unwrap_or(0),
    };
    let service: StreamableHttpService<Fixture, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(fixture.clone()),
        Default::default(),
        StreamableHttpServerConfig::default().with_sse_keep_alive(None),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("http://{}/mcp", listener.local_addr()?);
    std::io::stdout().flush()?;
    axum::serve(listener, axum::Router::new().nest_service("/mcp", service)).await?;
    Ok(())
}
