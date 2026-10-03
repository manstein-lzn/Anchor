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
use std::{fs::OpenOptions, io::Write, path::PathBuf, sync::Arc};

#[derive(Clone)]
struct Fixture {
    evidence: Option<PathBuf>,
    append_evidence: bool,
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
        Ok(ListToolsResult { tools: vec![Tool::new("fixture_suffix".to_owned(), "Return text with -checked appended. Deterministic local acceptance fixture.".to_owned(), Arc::new(serde_json::from_value(json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false})).unwrap()))], next_cursor:None, meta:None })
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
    let fixture = Fixture {
        evidence: std::env::args_os().nth(1).map(PathBuf::from),
        append_evidence: std::env::args().any(|arg| arg == "--append-evidence"),
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
