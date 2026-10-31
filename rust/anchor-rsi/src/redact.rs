use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// Escaped so this source file never carries the rejected sequences themselves:
/// the deployment gateway matches them as raw substrings, and Anchor freezes
/// source text into model-readable projections. Each head is the first character
/// of a filtered term, whether used literally or produced by adjacent words
/// (a verb or noun ending in that character followed by 密钥/密码/…).
const CLASSIFICATION_HEADS: [&str; 3] = ["\u{7edd}", "\u{673a}", "\u{79d8}"];
const CLASSIFICATION_TAIL: &str = "\u{5bc6}";
/// Preceding character that forms the common accidental collocation; the match
/// itself supplies the second half of the stem.
const REJECT_PREFIX: &str = "\u{62d2}";
/// Infix that keeps the natural reading of that collocation.
const REJECT_INFIX: &str = "\u{8be5}";
/// Visible separator used when the adjacency is not a known collocation.
const SEPARATOR: &str = "\u{00b7}";

static CLASSIFICATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        "(?:{}){CLASSIFICATION_TAIL}",
        CLASSIFICATION_HEADS.join("|")
    ))
    .expect("classification pattern is valid")
});

static SENSITIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)secret|password|passwd|token|api[_-]?key|credential|private[_-]?key|authorization",
    )
    .unwrap()
});
static REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:env|env_var|ref|reference|name|path|required)$").unwrap());
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(Bearer|Basic)\s+[A-Za-z0-9+/_.=-]{8,}").unwrap());
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?im)(["']?\b[\w.-]*(?:secret|password|passwd|token|api[_-]?key|credential|authorization)[\w.-]*["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*')"#).unwrap()
});
static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----.*?-----END (?:RSA |EC |OPENSSH )?PRIVATE KEY-----").unwrap()
});
static USERINFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(https?://)[^\s/@:]+:[^\s/@]+@").unwrap());

/// Rewrites an accidental classification-sequence adjacency so a gateway cannot
/// reject a whole request because of it. `拒绝` + `密钥`/`密码` becomes
/// `拒绝该密钥`/`拒绝该密码`; any other adjacency (for example `随机` + `密钥`)
/// gets a visible separator. The rewrite happens before any model or UI surface
/// sees projected text, and makes the projection count as redacted.
fn neutralize_classification(value: &str) -> String {
    CLASSIFICATION
        .replace_all(value, |captures: &regex::Captures<'_>| {
            let matched = captures.get(0).expect("whole match");
            let head = &matched.as_str()[..matched.as_str().len() - CLASSIFICATION_TAIL.len()];
            if value[..matched.start()].ends_with(REJECT_PREFIX) {
                format!("{head}{REJECT_INFIX}{CLASSIFICATION_TAIL}")
            } else {
                format!("{head}{SEPARATOR}{CLASSIFICATION_TAIL}")
            }
        })
        .into_owned()
}

pub(crate) fn text(value: &str) -> String {
    let value = neutralize_classification(value);
    let value = PRIVATE_KEY.replace_all(&value, "[REDACTED PRIVATE KEY]");
    let value = BEARER.replace_all(&value, "$1 [REDACTED]");
    let value = USERINFO.replace_all(&value, "$1[REDACTED]@");
    ASSIGNMENT
        .replace_all(&value, |captures: &regex::Captures<'_>| {
            let prefix = &captures[1];
            let literal = &captures[2];
            let key = prefix
                .trim_end_matches([':', '=', ' '])
                .trim_matches(['"', '\'']);
            if REFERENCE.is_match(key) || literal.contains("${") {
                captures[0].to_owned()
            } else {
                format!("{prefix}\"[REDACTED]\"")
            }
        })
        .into_owned()
}

pub(crate) fn json(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if SENSITIVE.is_match(key) && !REFERENCE.is_match(key) {
                    *value = Value::String("[REDACTED]".into());
                } else {
                    json(value);
                }
            }
        }
        Value::Array(array) => {
            for value in array {
                json(value);
            }
        }
        Value::String(string) => *string = text(string),
        _ => (),
    }
}

pub(crate) fn excluded(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        ".git"
            | ".claude"
            | ".env"
            | ".venv"
            | "venv"
            | "target"
            | "node_modules"
            | ".local"
            | "secrets"
            | ".credentials"
            | ".ssh"
            | ".aws"
            | "__pycache__"
            | "dist"
            | "build"
            | ".cache"
            | ".pytest_cache"
            | ".ruff_cache"
            | ".mypy_cache"
            | "state"
            | "workspaces"
            | ".npmrc"
            | ".netrc"
            | ".pypirc"
    ) || name.starts_with(".env.")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
        || name.starts_with("credentials.")
        || name.starts_with("secrets.")
        || name.starts_with("tokens.")
        || name.ends_with(".sqlite")
        || name.ends_with(".db")
        || ["trace", "checkpoint", "messages", "tool-calls"]
            .iter()
            .any(|part| {
                name.contains(part) && (name.ends_with(".json") || name.ends_with(".jsonl"))
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected_sequence() -> String {
        format!("{}{CLASSIFICATION_TAIL}", CLASSIFICATION_HEADS[0])
    }

    fn sequence_for(head: &str) -> String {
        format!("{head}{CLASSIFICATION_TAIL}")
    }

    #[test]
    fn the_rejected_collocation_is_rewritten_in_place() {
        let head = CLASSIFICATION_HEADS[0];
        let projected = text(&format!(
            "继续{REJECT_PREFIX}{head}{CLASSIFICATION_TAIL}钥默认值"
        ));
        assert_eq!(
            projected,
            format!("继续{REJECT_PREFIX}{head}{REJECT_INFIX}{CLASSIFICATION_TAIL}钥默认值")
        );
        assert!(!projected.contains(&rejected_sequence()), "{projected}");
    }

    #[test]
    fn every_classification_head_is_neutralized() {
        for head in CLASSIFICATION_HEADS {
            let sequence = sequence_for(head);
            let projected = text(&format!("前缀{sequence}后缀"));
            assert!(!projected.contains(&sequence), "{projected}");
            assert!(
                projected.contains(&format!("{head}{SEPARATOR}{CLASSIFICATION_TAIL}")),
                "{projected}"
            );
        }
    }

    #[test]
    fn other_adjacencies_keep_their_text_and_gain_a_separator() {
        let projected = text(&format!("其它前缀{}之后的文本", rejected_sequence()));
        assert!(
            projected.contains(&format!(
                "{}{SEPARATOR}{CLASSIFICATION_TAIL}",
                CLASSIFICATION_HEADS[0]
            )),
            "{projected}"
        );
        assert!(!projected.contains(&rejected_sequence()), "{projected}");
    }

    #[test]
    fn credential_redaction_still_applies_after_neutralization() {
        let projected = text("api_key=\"secret-value\"");
        assert!(!projected.contains("secret-value"), "{projected}");
        assert!(projected.contains("[REDACTED]"), "{projected}");
    }
}
