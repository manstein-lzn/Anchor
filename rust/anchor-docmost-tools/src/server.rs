use std::sync::Arc;

use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde::Deserialize;
use serde_json::json;

use crate::Uploader;

pub fn attachment_tool() -> Tool {
    let schema = json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Absolute path under /in/publish/assets"},
            "pageId": {"type": "string", "description": "Target Docmost page UUID"},
            "attachmentId": {"type": "string", "description": "Optional existing attachment UUID to replace"}
        },
        "required": ["path", "pageId"]
    });
    Tool::new(
        "upload_page_image",
        "Upload a report image from /in/publish/assets to a Docmost page and return its Markdown URL. Set attachmentId to replace an existing image on that page.",
        Arc::new(
            schema
                .as_object()
                .expect("fixed tool schema is an object")
                .clone(),
        ),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Arguments {
    path: String,
    page_id: String,
    attachment_id: Option<String>,
}

#[derive(Clone)]
pub struct AttachmentServer {
    uploader: Uploader,
}

impl AttachmentServer {
    pub fn new(uploader: Uploader) -> Self {
        Self { uploader }
    }
}

impl ServerHandler for AttachmentServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("docmost-attachments", "1.0.0"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: vec![attachment_tool()],
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if request.name != "upload_page_image" {
            return Err(ErrorData::invalid_params("unknown tool", None));
        }
        let arguments = serde_json::from_value::<Arguments>(json!(request.arguments));
        let arguments = match arguments {
            Ok(arguments) => arguments,
            Err(_) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(
                    "path and pageId must be strings; attachmentId must be a UUID string when provided",
                )]));
            }
        };
        match self
            .uploader
            .upload(
                &arguments.path,
                &arguments.page_id,
                arguments.attachment_id.as_deref(),
            )
            .await
        {
            Ok(attachment) => Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string(&attachment).map_err(|_| {
                    ErrorData::internal_error("could not serialize attachment", None)
                })?,
            )])),
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }
}
