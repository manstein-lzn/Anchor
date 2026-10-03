use crate::{ecosystem::Ecosystem, evidence::Evidence};
use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ErrorData, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
pub struct RsiService {
    pub evidence: Arc<Evidence>,
    pub ecosystem: Arc<Ecosystem>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexArguments {
    domain: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    path: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_read_limit")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EcosystemArguments {
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    20
}
fn default_read_limit() -> usize {
    80
}

impl RsiService {
    pub async fn dispatch(&self, name: &str, arguments: Value) -> Result<Value, String> {
        let result = self.dispatch_inner(name, arguments.clone()).await;
        // Audit before returning so an unrecorded read is never reported as
        // accepted evidence coverage to the model.
        let audited = if result.is_ok() {
            arguments
        } else {
            json!({"rejected_arguments":true})
        };
        self.evidence.audit(name, &audited, result.is_ok())?;
        Ok(match result {
            Ok(value) => value,
            Err(reason) => json!({"status":"invalid_request","error":reason}),
        })
    }
    async fn dispatch_inner(&self, name: &str, arguments: Value) -> Result<Value, String> {
        match name {
            "rsi_index" => {
                let args: IndexArguments =
                    serde_json::from_value(arguments).map_err(|e| e.to_string())?;
                self.evidence.index(&args.domain, args.offset, args.limit)
            }
            "rsi_read" => {
                let args: ReadArguments =
                    serde_json::from_value(arguments).map_err(|e| e.to_string())?;
                self.evidence.read(&args.path, args.offset, args.limit)
            }
            "rsi_ecosystem" => {
                let args: EcosystemArguments =
                    serde_json::from_value(arguments).map_err(|e| e.to_string())?;
                let limit = args.limit.clamp(1, 20);
                let targets = self
                    .evidence
                    .dependencies
                    .iter()
                    .skip(args.offset)
                    .take(limit)
                    .cloned()
                    .collect::<Vec<_>>();
                let mut result = self.ecosystem.research(&targets).await;
                result["offset"] = json!(args.offset);
                result["total"] = json!(self.evidence.dependencies.len());
                result["next_offset"] = json!(
                    args.offset
                        .checked_add(limit)
                        .filter(|next| *next < self.evidence.dependencies.len())
                );
                result["evidence_path"] = json!(self.evidence.store_ecosystem(&result)?);
                Ok(result)
            }
            _ => Err("unknown RSI tool".into()),
        }
    }
}

impl ServerHandler for RsiService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("anchor-rsi", "0.1.0"))
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let pagination = json!({"type":"integer","minimum":0});
        let limit = json!({"type":"integer","minimum":1,"maximum":100});
        let tools = [
            ("rsi_index","Page frozen evidence inventory for one domain. Counts are collection coverage, not reviewed coverage.",json!({"type":"object","properties":{"domain":{"type":"string","enum":["code","graphs","plugins","runs","dependencies","previous"]},"offset":pagination,"limit":limit},"required":["domain"],"additionalProperties":false})),
            ("rsi_read","Read only an indexed frozen evidence path, by zero-based line offset; returned locators are one-based frozen projection lines.",json!({"type":"object","properties":{"path":{"type":"string"},"offset":pagination,"limit":{"type":"integer","minimum":1,"maximum":200}},"required":["path"],"additionalProperties":false})),
            ("rsi_ecosystem","Research a page of direct dependencies from frozen manifests using credential-free HTTPS public metadata. Failures are evidence, not successful research.",json!({"type":"object","properties":{"offset":pagination,"limit":{"type":"integer","minimum":1,"maximum":20}},"additionalProperties":false})),
        ].into_iter().map(|(name,description,schema)|Tool::new(name.to_owned(),description.to_owned(),Arc::new(schema.as_object().unwrap().clone()))).collect();
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
        match self
            .dispatch(
                &request.name,
                Value::Object(request.arguments.unwrap_or_default()),
            )
            .await
        {
            Ok(result) => Ok(CallToolResult::structured(result)),
            Err(reason) => Ok(CallToolResult::structured_error(
                json!({"status":"error","error":reason}),
            )),
        }
    }
}
