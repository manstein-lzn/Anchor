use anchor_runtime::ToolResultContent;
use rmcp::model::CallToolResult;

use crate::McpHostError;

pub(super) fn result_contents(
    result: &CallToolResult,
) -> Result<Vec<ToolResultContent>, McpHostError> {
    use rmcp::model::{ContentBlock, ResourceContents};

    let structured = result.structured_content.as_ref();
    let canonical_fallback = structured.map(serde_json::Value::to_string);
    let mut replaced_fallback = false;
    let mut mapped = Vec::with_capacity(result.content.len());
    let mut images = crate::ImageBudget::default();

    for block in &result.content {
        if !replaced_fallback
            && let (ContentBlock::Text(text), Some(fallback), Some(structured)) =
                (block, canonical_fallback.as_deref(), structured)
            && text.text == fallback
        {
            mapped.push(ToolResultContent::json(structured.clone()));
            replaced_fallback = true;
            continue;
        }

        match block {
            ContentBlock::Text(text) => mapped.push(ToolResultContent::text(text.text.clone())),
            ContentBlock::Image(image) => {
                images.validate(&image.data, &image.mime_type)?;
                mapped.push(ToolResultContent::image(&image.data, &image.mime_type));
            }
            ContentBlock::Resource(resource)
                if matches!(
                    &resource.resource,
                    ResourceContents::BlobResourceContents {
                        mime_type: Some(mime_type), ..
                    } if mime_type.starts_with("image/")
                ) =>
            {
                if let ResourceContents::BlobResourceContents {
                    blob,
                    mime_type: Some(mime_type),
                    ..
                } = &resource.resource
                {
                    images.validate(blob, mime_type)?;
                    mapped.push(ToolResultContent::image(blob, mime_type));
                }
            }
            _ => mapped.push(ToolResultContent::json(
                serde_json::to_value(block)
                    .map_err(|error| McpHostError::Encode(error.to_string()))?,
            )),
        }
    }

    if let Some(structured) = structured
        && !replaced_fallback
    {
        mapped.insert(0, ToolResultContent::json(structured.clone()));
    }

    if mapped.is_empty() {
        let text = if result.is_error == Some(true) {
            "the MCP tool reported an error"
        } else {
            ""
        };
        mapped.push(ToolResultContent::text(text));
    }

    Ok(mapped)
}

#[cfg(test)]
mod tests;
