use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

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

pub(crate) fn text(value: &str) -> String {
    let value = PRIVATE_KEY.replace_all(value, "[REDACTED PRIVATE KEY]");
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
