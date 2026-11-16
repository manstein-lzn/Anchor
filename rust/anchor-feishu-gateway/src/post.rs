//! Feishu post bodies replace the former unsupported-message path. Downloads
//! remain in media::enrich, so the ledger stores descriptors rather than bytes.

use serde_json::{Value, json};

use crate::{GatewayError, MAX_TEXT_BYTES, media::InboundResource, protocol::strip_self_mention};

pub(crate) fn normalize(
    content: &Value,
    self_mention: Option<&str>,
    bot_open_id: Option<&str>,
) -> Result<Option<(String, Vec<Value>)>, GatewayError> {
    let object = content
        .as_object()
        .ok_or(GatewayError::Invalid("invalid Feishu post content"))?;
    let body = if ["title", "content", "content_v2"]
        .iter()
        .any(|key| object.contains_key(*key))
    {
        content
    } else {
        ["zh_cn", "en_us", "ja_jp"]
            .into_iter()
            .filter_map(|locale| object.get(locale))
            .find(|value| value.is_object())
            .or_else(|| object.values().find(|value| value.is_object()))
            .unwrap_or(content)
    };
    let blocks = body
        .get("content_v2")
        .and_then(Value::as_array)
        .filter(|blocks| !blocks.is_empty())
        .or_else(|| body.get("content").and_then(Value::as_array));
    let mut lines = vec![field(body, "title").to_owned()];
    let mut resources = Vec::new();
    let mut mentioned = self_mention.is_none();
    for paragraph in blocks.into_iter().flatten() {
        let elements = paragraph
            .as_array()
            .ok_or(GatewayError::Invalid("invalid Feishu post paragraph"))?;
        let mut line = String::new();
        for element in elements {
            let text = field(element, "text");
            match field(element, "tag") {
                "at" => {
                    let id = field(element, "user_id");
                    if self_mention.is_some()
                        && (Some(id) == self_mention || Some(id) == bot_open_id)
                    {
                        mentioned = true;
                    } else {
                        line.push('@');
                        let name = field(element, "user_name");
                        line.push_str(if name.is_empty() { id } else { name });
                    }
                }
                "a" => {
                    line.push_str(text);
                    let href = field(element, "href");
                    if !href.is_empty() && href != text {
                        line.push_str(&format!(" ({href})"));
                    }
                }
                "img" => {}
                "media" => line.push_str("[Video attachment not loaded]"),
                "emotion" => line.push_str(&format!(":{}:", field(element, "emoji_type"))),
                "code_block" => line.push_str(&format!(
                    "\n```{}\n{text}\n```\n",
                    field(element, "language")
                )),
                "hr" => line.push_str("\n---\n"),
                "md" => {
                    let (clean, found) = markdown_mentions(text, self_mention, bot_open_id);
                    mentioned |= found;
                    line.push_str(&clean);
                }
                _ => line.push_str(text),
            }
        }
        lines.push(line);
    }
    // Both renderings can contain the same image. Keep one download in original order.
    for key in ["content_v2", "content"] {
        for paragraph in body
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for element in paragraph.as_array().into_iter().flatten() {
                match field(element, "tag") {
                    "img" => add_resource(
                        &mut resources,
                        "image",
                        json!({"image_key":element.get("image_key")}),
                    )?,
                    "md" => {
                        let mut remaining = field(element, "text");
                        while let Some((_, tail)) = remaining.split_once("![") {
                            let Some((_, link)) = tail.split_once("](") else {
                                break;
                            };
                            let Some((key, rest)) = link.split_once(')') else {
                                break;
                            };
                            if key.starts_with("img_") {
                                add_resource(&mut resources, "image", json!({"image_key":key}))?;
                            }
                            remaining = rest;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    for container in [content, body] {
        for file in container
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if file.get("is_folder").and_then(Value::as_bool) == Some(true) {
                lines.push(format!(
                    "[Folder attachment not loaded: {}]",
                    field(file, "file_name")
                ));
                continue;
            }
            add_resource(
                &mut resources,
                "file",
                json!({"file_key":file.get("file_key"),"file_name":file.get("file_name")}),
            )?;
        }
        if std::ptr::eq(content, body) {
            break;
        }
    }
    let text = lines.join("\n");
    let text = if let Some(clean) = self_mention.and_then(|key| strip_self_mention(&text, key)) {
        mentioned = true;
        clean
    } else {
        text.trim().to_owned()
    };
    if text.len() > MAX_TEXT_BYTES {
        return Err(GatewayError::Invalid("oversized Feishu post text"));
    }
    Ok((mentioned && (!text.is_empty() || !resources.is_empty())).then_some((text, resources)))
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn add_resource(
    resources: &mut Vec<Value>,
    kind: &str,
    content: Value,
) -> Result<(), GatewayError> {
    let descriptor = InboundResource::from_content(kind, &content)?;
    if !resources.contains(&descriptor) {
        resources.push(descriptor);
    }
    Ok(())
}

fn markdown_mentions(text: &str, key: Option<&str>, bot: Option<&str>) -> (String, bool) {
    let Some(key) = key else {
        return (text.to_owned(), false);
    };
    let mut output = String::new();
    let mut remaining = text;
    let mut found = false;
    while let Some(start) = remaining.find("<at ") {
        let tag = &remaining[start..];
        let Some(end) = tag.find('>') else { break };
        let Some(close) = tag[end + 1..].find("</at>") else {
            break;
        };
        let length = end + 1 + close + 5;
        output.push_str(&remaining[..start]);
        if [Some(key), bot].into_iter().flatten().any(|id| {
            ['\"', '\'']
                .into_iter()
                .any(|quote| tag[..end].contains(&format!(" user_id={quote}{id}{quote}")))
        }) {
            found = true;
        } else {
            output.push_str(&tag[..length]);
        }
        remaining = &tag[length..];
    }
    output.push_str(remaining);
    (output, found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_title_paragraphs_links_and_attachment_zone_are_preserved() {
        let post = json!({"en_us":{"title":"Review","content":[
            [{"tag":"text","text":"Look at "},{"tag":"a","text":"project","href":"https://example.org"}],
            [{"tag":"img","image_key":"img_one"},{"tag":"img","image_key":"img_two"}]
        ]},"files":[{"file_key":"file_one","file_name":"notes.txt","is_folder":false}]});
        let (text, resources) = normalize(&post, None, None).unwrap().unwrap();
        assert_eq!(text, "Review\nLook at project (https://example.org)");
        assert_eq!(resources.len(), 3);
        assert_eq!(resources[2]["name"], "notes.txt");
    }

    #[test]
    fn markdown_v2_removes_only_self_mention_and_deduplicates_image_descriptors() {
        let post = json!({"content_v2":[[{"tag":"md","text":"<at user_id=\"@_user_1\">Bot</at> compare @_user_10 ![Image](img_one)"}]],
            "content":[[{"tag":"at","user_id":"@_user_1"},{"tag":"img","image_key":"img_one"}]]});
        let (text, resources) = normalize(&post, Some("@_user_1"), Some("ou_self"))
            .unwrap()
            .unwrap();
        assert_eq!(text, "compare @_user_10 ![Image](img_one)");
        assert_eq!(resources.len(), 1);
        assert!(
            normalize(&post, Some("@_user_2"), Some("ou_other"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn image_only_mentions_empty_posts_and_oversized_posts_have_explicit_admission() {
        for id in ["@_user_1", "ou_self"] {
            let post = json!({"content":[[{"tag":"at","user_id":id},{"tag":"img","image_key":"img_one"}]]});
            let (text, resources) = normalize(&post, Some("@_user_1"), Some("ou_self"))
                .unwrap()
                .unwrap();
            assert!(text.is_empty());
            assert_eq!(resources.len(), 1);
        }
        assert!(
            normalize(&json!({"content":[]}), None, None)
                .unwrap()
                .is_none()
        );
        assert!(normalize(&json!({"title":"x".repeat(MAX_TEXT_BYTES + 1)}), None, None).is_err());
        assert!(normalize(&json!({"content":[{}]}), None, None).is_err());
    }
}
