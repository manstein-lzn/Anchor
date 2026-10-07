use std::time::Duration;

use serde_json::{Value, json, map::Map as JsonMap};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

use crate::{
    BATCH_BUDGET_SECONDS, CitationRequest, Direction, Error, ReadManyRequest, ReadRequest,
    Scholarly, SearchRequest, Source,
};

const PROTOCOL_VERSION: &str = "2024-11-05";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn serve_stdio(scholarly: &Scholarly) -> std::io::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();
    let mut output = BufWriter::new(stdout);

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => handle_request(scholarly, request).await,
            Err(_) => Some(rpc_error(Value::Null, -32700, "parse error")),
        };
        if let Some(response) = response {
            let encoded = serde_json::to_vec(&response)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            output.write_all(&encoded).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
    }
    Ok(())
}

async fn handle_request(scholarly: &Scholarly, request: Value) -> Option<Value> {
    let has_id = request.get("id").is_some();
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return has_id.then(|| rpc_error(id, -32600, "invalid request"));
    };

    match method {
        "initialize" => Some(success(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": "scholarly",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "instructions": "Use scholarly tools for read-only literature search and document retrieval."
            }),
        )),
        "ping" => Some(success(id, json!({}))),
        "tools/list" => Some(success(id, json!({"tools": tool_definitions()}))),
        "tools/call" => match call_tool(scholarly, request.get("params")).await {
            Ok(value) => Some(success(id, tool_result(value, false))),
            Err(error) if error.message == "unknown tool" => {
                Some(rpc_error(id, -32602, "unknown tool"))
            }
            Err(error) => Some(success(id, tool_error(error))),
        },
        "notifications/initialized" | "notifications/cancelled" => None,
        _ if !has_id => None,
        _ => Some(rpc_error(id, -32601, "method not found")),
    }
}

async fn call_tool(scholarly: &Scholarly, params: Option<&Value>) -> Result<Value, Error> {
    let params = params
        .cloned()
        .unwrap_or_else(|| Value::Object(JsonMap::new()));
    let params = params
        .as_object()
        .ok_or_else(|| Error::input("tools/call params must be an object"))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::input("tools/call requires a string name"))?;
    let arguments = match params.get("arguments") {
        None => JsonMap::new(),
        Some(value) => value
            .as_object()
            .cloned()
            .ok_or_else(|| Error::input("tools/call arguments must be an object"))?,
    };

    match name {
        "scholarly_sources" => Ok(scholarly.sources(REQUEST_TIMEOUT).await),
        "scholarly_search" => {
            let request = SearchRequest {
                query: required_string(&arguments, "query")?,
                source: source_argument(&arguments)?,
                limit: optional_i64(&arguments, "limit", 8)?,
                offset: optional_i64(&arguments, "offset", 0)?,
            };
            scholarly.search(&request, REQUEST_TIMEOUT).await
        }
        "scholarly_search_many" => {
            let queries = string_array(&arguments, "queries")?;
            scholarly
                .search_many(
                    queries,
                    source_argument(&arguments)?,
                    optional_i64(&arguments, "limit", 8)?,
                    optional_i64(&arguments, "offset", 0)?,
                    optional_i64(&arguments, "budget", BATCH_BUDGET_SECONDS as i64)?,
                    REQUEST_TIMEOUT,
                )
                .await
        }
        "scholarly_read" => {
            let request = ReadRequest {
                url: required_string(&arguments, "url")?,
                offset: optional_i64(&arguments, "offset", 0)?,
                page_start: optional_i64(&arguments, "page_start", 0)?,
            };
            scholarly.read(&request, REQUEST_TIMEOUT).await
        }
        "scholarly_read_many" => {
            let request = ReadManyRequest {
                urls: string_array(&arguments, "urls")?,
                offset: optional_i64(&arguments, "offset", 0)?,
                page_start: optional_i64(&arguments, "page_start", 0)?,
            };
            scholarly.read_many(&request, REQUEST_TIMEOUT).await
        }
        "scholarly_citations" => {
            let request = CitationRequest {
                identifier: required_string(&arguments, "identifier")?,
                direction: direction_argument(&arguments)?,
                limit: optional_i64(&arguments, "limit", 8)?,
            };
            scholarly.citations(&request, REQUEST_TIMEOUT).await
        }
        _ => Err(Error::input("unknown tool")),
    }
}

fn required_string(arguments: &JsonMap<String, Value>, name: &str) -> Result<String, Error> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::input(format!("{name} must be a string")))
}

fn string_array(arguments: &JsonMap<String, Value>, name: &str) -> Result<Vec<String>, Error> {
    let values = arguments
        .get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::input(format!("{name} must be an array of strings")))?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::input(format!("{name}[{index}] must be a string")))
        })
        .collect()
}

fn optional_i64(
    arguments: &JsonMap<String, Value>,
    name: &str,
    default: i64,
) -> Result<i64, Error> {
    arguments.get(name).map_or(Ok(default), |value| {
        value
            .as_i64()
            .ok_or_else(|| Error::input(format!("{name} must be an integer")))
    })
}

fn source_argument(arguments: &JsonMap<String, Value>) -> Result<Source, Error> {
    match arguments
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("crossref")
    {
        "crossref" => Ok(Source::Crossref),
        "arxiv" => Ok(Source::Arxiv),
        "openalex" => Ok(Source::Openalex),
        _ => Err(Error::input("source must be crossref, arxiv, or openalex")),
    }
}

fn direction_argument(arguments: &JsonMap<String, Value>) -> Result<Direction, Error> {
    match arguments
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or("cited_by")
    {
        "cited_by" => Ok(Direction::CitedBy),
        "cites" => Ok(Direction::Cites),
        _ => Err(Error::input("direction must be cited_by or cites")),
    }
}

fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn tool_result(value: Value, is_error: bool) -> Value {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

fn tool_error(error: Error) -> Value {
    tool_result(
        json!({"code": error.code, "message": error.message, "retryable": error.retryable}),
        true,
    )
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "scholarly_sources",
            "Probe the configured public scholarly sources and report which answered.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        ),
        tool(
            "scholarly_search",
            "Search Crossref, arXiv, or OpenAlex for papers.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "source": {"type": "string", "enum": ["crossref", "arxiv", "openalex"], "default": "crossref"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 8},
                    "offset": {"type": "integer", "minimum": 0, "default": 0}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        tool(
            "scholarly_search_many",
            "Run a bounded sequence of scholarly searches while preserving per-query results.",
            json!({
                "type": "object",
                "properties": {
                    "queries": {"type": "array", "items": {"type": "string"}, "maxItems": 40},
                    "source": {"type": "string", "enum": ["crossref", "arxiv", "openalex"], "default": "crossref"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 8},
                    "offset": {"type": "integer", "minimum": 0, "default": 0},
                    "budget": {"type": "integer", "minimum": 0, "maximum": 3600, "default": 420}
                },
                "required": ["queries"],
                "additionalProperties": false
            }),
        ),
        tool(
            "scholarly_read",
            "Read and extract text from one HTML, text, or PDF document.",
            json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "offset": {"type": "integer", "minimum": 0, "default": 0},
                    "page_start": {"type": "integer", "minimum": 0, "default": 0}
                },
                "required": ["url"],
                "additionalProperties": false
            }),
        ),
        tool(
            "scholarly_read_many",
            "Read up to eight HTML, text, or PDF documents and preserve per-document failures.",
            json!({
                "type": "object",
                "properties": {
                    "urls": {"type": "array", "items": {"type": "string"}, "maxItems": 8},
                    "offset": {"type": "integer", "minimum": 0, "default": 0},
                    "page_start": {"type": "integer", "minimum": 0, "default": 0}
                },
                "required": ["urls"],
                "additionalProperties": false
            }),
        ),
        tool(
            "scholarly_citations",
            "Follow cited-by or cites relationships through OpenAlex.",
            json!({
                "type": "object",
                "properties": {
                    "identifier": {"type": "string"},
                    "direction": {"type": "string", "enum": ["cited_by", "cites"], "default": "cited_by"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 8}
                },
                "required": ["identifier"],
                "additionalProperties": false
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": input_schema})
}
