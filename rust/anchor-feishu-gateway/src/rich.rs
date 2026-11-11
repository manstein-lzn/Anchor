//! Feishu cards are a transport projection, not a second reply identity.

use serde_json::json;

use crate::GatewayError;

const MAX_CARD_REQUEST_BYTES: usize = 30_000;

pub(crate) fn markdown_card(text: &str) -> Result<String, GatewayError> {
    let card = json!({
        "schema":"2.0",
        "config":{"update_multi":true},
        "body":{"elements":[{"tag":"markdown","content":without_mentions(text)}]},
    });
    let content =
        serde_json::to_string(&card).map_err(|_| GatewayError::Invalid("invalid card"))?;
    // Feishu bounds the serialized request, including the serialized card
    // inside `content`. Reserve the largest allowed recipient envelope so the
    // same validated projection is safe for create, reply and PATCH.
    let envelope = json!({"receive_id":"x".repeat(200),"msg_type":"interactive","content":content});
    if serde_json::to_vec(&envelope)
        .map_err(|_| GatewayError::Invalid("invalid card"))?
        .len()
        > MAX_CARD_REQUEST_BYTES
    {
        return Err(GatewayError::Invalid(
            "rendered Markdown card exceeds 30000 bytes",
        ));
    }
    Ok(content)
}

/// Only platform mention tags are neutralized. Ordinary Markdown, code and
/// angle brackets remain intact rather than being globally HTML-escaped.
fn without_mentions(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut escape_end = false;
    for (index, ch) in text.char_indices() {
        if ch != '<' {
            if ch == '>' && escape_end {
                output.push_str("&gt;");
                escape_end = false;
            } else {
                output.push(ch);
            }
            continue;
        }
        let tag = text[index + 1..].trim_start();
        let tag = tag.strip_prefix('/').unwrap_or(tag).trim_start();
        let mention = tag
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("at"))
            && tag[2..]
                .chars()
                .next()
                .is_none_or(|ch| ch.is_whitespace() || matches!(ch, '>' | '/'));
        if mention {
            output.push_str("&lt;");
            escape_end = true;
        } else {
            output.push('<');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn markdown_preserves_code_tables_and_quotes_but_cannot_inject_mentions() {
        let text = "# Result\n> quote\n```html\n<div>x < 4</div>\n```\n| A | B |\n| - | - |\n| 1 | 2 |\n<at id=all>everyone</at> <AT user_id='ou_other'>other</AT> @literal";
        let card: Value = serde_json::from_str(&markdown_card(text).unwrap()).unwrap();
        assert_eq!(card["schema"], "2.0");
        assert_eq!(card["config"]["update_multi"], true);
        assert_eq!(card["body"]["elements"][0]["tag"], "markdown");
        let rendered = card["body"]["elements"][0]["content"].as_str().unwrap();
        assert!(rendered.starts_with("# Result\n> quote\n```html\n<div>x < 4</div>"));
        assert!(rendered.contains("| A | B |"));
        assert!(rendered.contains("&lt;at id=all&gt;everyone&lt;/at&gt;"));
        assert!(!rendered.contains("<AT"));
        assert!(rendered.ends_with("@literal"));
        for text in [
            "<at broken <at id=all>all</at>",
            "< at id=all>all</ at>",
            "<AT\n id=all>all</AT>",
        ] {
            let rendered = without_mentions(text);
            assert!(!rendered.contains('<'));
        }
    }

    #[test]
    fn card_limit_counts_the_serialized_projection_not_just_input_characters() {
        assert!(markdown_card(&"x".repeat(20_480)).is_ok());
        assert!(markdown_card(&"\\".repeat(20_480)).is_err());
        assert!(markdown_card(&"\"".repeat(9_000)).is_err());
        for text in [
            "<",
            "<a",
            "</",
            "<at",
            "<at\n",
            "<atomic>code</atomic>",
            "<\u{fffd}>",
        ] {
            assert!(markdown_card(text).is_ok());
        }
    }
}
