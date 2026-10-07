use crate::{PackageError, Result};
use object::{BinaryFormat, Object, ObjectKind, ObjectSegment, SegmentFlags};
use object::{
    elf,
    endian::Endianness,
    read::elf::{FileHeader, ProgramHeader},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeSet, fs, path::Path, str};

fn invalid(message: impl Into<String>) -> PackageError {
    PackageError::Invalid(message.into())
}

pub(crate) fn safe_component(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 100
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(invalid(format!("unsafe path component: {name:?}")));
    }
    Ok(())
}

pub(crate) fn tool_name(name: &str) -> Result<()> {
    safe_component(name)?;
    if !name.as_bytes()[0].is_ascii_alphanumeric()
        || matches!(name, "goose" | "anchor-runner-host")
        || Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "rs" | "py"
                        | "pyc"
                        | "pyo"
                        | "sh"
                        | "c"
                        | "h"
                        | "cpp"
                        | "go"
                        | "ts"
                        | "tsx"
                        | "js"
                        | "mjs"
                        | "cjs"
                )
            })
    {
        return Err(invalid(format!("unsafe or reserved tool name: {name}")));
    }
    resource_component(name)
}

pub(crate) fn version(version: &str) -> Result<()> {
    if version.is_empty()
        || version.len() > 100
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
    {
        return Err(invalid(
            "version must be a nonempty ASCII version identifier",
        ));
    }
    Ok(())
}

pub(crate) fn resource_component(name: &str) -> Result<()> {
    safe_component(name)?;
    let lower = name.to_ascii_lowercase();
    if (lower.starts_with('.') && lower != ".mcp.json")
        || matches!(
            lower.as_str(),
            "src"
                | "tests"
                | "target"
                | "node_modules"
                | "__pycache__"
                | "venv"
                | "data"
                | "state"
                | "sessions"
                | "runs"
                | "workspaces"
                | "artifacts"
                | "history"
                | "credentials"
                | "secrets"
                | "env"
                | "local-inputs.json"
                | "cargo.toml"
                | "cargo.lock"
                | "pyproject.toml"
                | "requirements.txt"
                | "package.json"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | "go.mod"
                | "go.sum"
                | "tool.json"
                | "config.yaml"
        )
        || [
            "credentials.",
            "secrets.",
            "private-key.",
            "id_rsa",
            "id_ed25519",
            "dotenv",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        || [
            ".env", ".pem", ".key", ".p12", ".pfx", ".sqlite", ".sqlite3", ".db", ".jsonl",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
        || lower.starts_with(".env")
    {
        return Err(invalid(format!(
            "repository, source, credential or runtime-data path forbidden: {name}"
        )));
    }
    Ok(())
}

pub(crate) fn archive_path(relative: &str) -> Result<()> {
    if relative.len() + "anchor-runtime/".len() > 240 {
        return Err(invalid("resource path exceeds deterministic USTAR limit"));
    }
    for component in relative.split('/') {
        safe_component(component)?;
    }
    Ok(())
}

pub(crate) fn resource_extension(relative: &str) -> Result<()> {
    let extension = Path::new(relative)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let common = [
        "json", "md", "txt", "yaml", "yml", "toml", "csv", "png", "jpg", "jpeg", "webp", "gif",
        "avif", "ico", "svg", "pdf", "html", "css", "woff", "woff2", "ttf", "otf",
    ];
    let compiled_web =
        relative.starts_with("web/") && matches!(extension.as_str(), "js" | "mjs" | "cjs" | "wasm");
    if !common.contains(&extension.as_str()) && !compiled_web && !native_resource(relative) {
        return Err(invalid(format!(
            "unsupported or source resource: {relative}"
        )));
    }
    Ok(())
}

pub(crate) fn native_resource(relative: &str) -> bool {
    let parts = relative.split('/').collect::<Vec<_>>();
    parts.len() == 5
        && parts[0] == "bundle"
        && parts[1] == "plugins"
        && parts[3] == "bin"
        && Path::new(parts[4]).extension().is_none()
}

fn sensitive(key: &str) -> bool {
    let normalized = key.replace('-', "_").to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxy_authorization"
            | "cookie"
            | "set_cookie"
            | "api_key"
            | "apikey"
            | "password"
            | "passwd"
            | "secret"
            | "token"
            | "access_token"
            | "refresh_token"
            | "client_secret"
            | "private_key"
            | "credential"
            | "credentials"
    ) || [
        "_api_key",
        "_apikey",
        "_password",
        "_secret",
        "_token",
        "_private_key",
    ]
    .iter()
    .any(|suffix| normalized.ends_with(suffix))
}

fn env_name(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_uppercase() || byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(crate) fn placeholders(value: &str) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    let mut tail = value;
    while let Some((_, following)) = tail.split_once("${") {
        let Some((name, remaining)) = following.split_once('}') else {
            break;
        };
        if env_name(name) {
            result.insert(name.to_owned());
        }
        tail = remaining;
    }
    result
}

fn secret_reference(value: &str) -> bool {
    let value = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("Basic "))
        .unwrap_or(value);
    value
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
        .is_some_and(env_name)
}

fn check_json(value: &Value, context: &str) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if sensitive(key)
                    && !value.is_null()
                    && !value
                        .as_str()
                        .is_some_and(|value| value.is_empty() || secret_reference(value))
                {
                    return Err(invalid(format!(
                        "literal credential field in {context}; use environment references"
                    )));
                }
                check_json(value, context)?;
            }
        }
        Value::Array(array) => {
            for value in array {
                check_json(value, context)?;
            }
        }
        Value::String(value) => {
            if let Some((_, authority)) = value.split_once("://")
                && authority
                    .split(['/', '?', '#'])
                    .next()
                    .is_some_and(|authority| authority.contains('@'))
            {
                return Err(invalid(format!("URL credentials in {context}")));
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn resource_content(path: &Path, relative: &str) -> Result<()> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "json"
            | "md"
            | "txt"
            | "yaml"
            | "yml"
            | "toml"
            | "csv"
            | "html"
            | "css"
            | "js"
            | "mjs"
            | "cjs"
            | "svg"
    ) {
        return Ok(());
    }
    if fs::metadata(path)?.len() > 64 * 1024 * 1024 {
        return Err(invalid(format!("text resource exceeds 64 MiB: {relative}")));
    }
    let text = fs::read_to_string(path)?;
    if text.contains("-----BEGIN PRIVATE KEY-----")
        || text.contains("-----BEGIN RSA PRIVATE KEY-----")
        || text.contains("-----BEGIN OPENSSH PRIVATE KEY-----")
        || text.contains("-----BEGIN EC PRIVATE KEY-----")
    {
        return Err(invalid(format!("private key in {relative}")));
    }
    if extension == "json" {
        check_json(&serde_json::from_str(&text)?, relative)?;
    } else if matches!(extension.as_str(), "yaml" | "yml") {
        let value: Value =
            serde_yaml_ng::from_str(&text).map_err(|_| invalid("malformed YAML resource"))?;
        check_json(&value, relative)?;
    } else if extension == "toml" {
        let value: toml::Value =
            toml::from_str(&text).map_err(|_| invalid("malformed TOML resource"))?;
        check_json(&serde_json::to_value(value)?, relative)?;
    } else {
        for line in text.lines() {
            if let Some((key, value)) = line
                .trim()
                .strip_prefix("export ")
                .unwrap_or(line.trim())
                .split_once('=')
                && sensitive(key.trim())
                && !value.trim().is_empty()
                && !secret_reference(value.trim().trim_matches(['\'', '"']))
            {
                return Err(invalid(format!(
                    "literal credential assignment in {relative}"
                )));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElfIdentity {
    pub architecture: String,
    pub bits: u8,
    pub little_endian: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElfRuntime {
    pub interpreter: Option<String>,
    pub needed: Vec<String>,
}

pub(crate) fn elf(path: &Path) -> Result<ElfIdentity> {
    if fs::metadata(path)?.len() > 1024 * 1024 * 1024 {
        return Err(invalid("ELF exceeds 1 GiB"));
    }
    let bytes = fs::read(path)?;
    let file = object::File::parse(bytes.as_slice())
        .map_err(|_| invalid("binary is not a valid ELF executable"))?;
    if file.format() != BinaryFormat::Elf
        || !matches!(file.kind(), ObjectKind::Executable | ObjectKind::Dynamic)
        || file.entry() == 0
        || !file.segments().any(|segment| {
            matches!(segment.flags(), SegmentFlags::Elf { p_type, p_flags }
            if p_type == object::elf::PT_LOAD && p_flags & object::elf::PF_X == object::elf::PF_X)
                && file.entry() >= segment.address()
                && file
                    .entry()
                    .checked_sub(segment.address())
                    .is_some_and(|offset| offset < segment.size())
                && segment.data().is_ok_and(|data| !data.is_empty())
        })
    {
        return Err(invalid(
            "binary must be an ELF executable with an executable entry point",
        ));
    }
    Ok(ElfIdentity {
        architecture: format!("{:?}", file.architecture()),
        bits: if file.is_64() { 64 } else { 32 },
        little_endian: file.is_little_endian(),
    })
}

pub(crate) fn elf_runtime(path: &Path) -> Result<ElfRuntime> {
    let bytes = fs::read(path)?;
    let file = object::File::parse(bytes.as_slice())
        .map_err(|_| invalid("binary is not a valid ELF executable"))?;
    if file.format() != BinaryFormat::Elf {
        return Err(invalid("binary is not an ELF executable"));
    }
    let interpreter = if file.is_64() {
        interpreter_from_header(
            object::elf::FileHeader64::<Endianness>::parse(bytes.as_slice())
                .map_err(|_| invalid("ELF header is invalid"))?,
            &bytes,
        )?
    } else {
        interpreter_from_header(
            object::elf::FileHeader32::<Endianness>::parse(bytes.as_slice())
                .map_err(|_| invalid("ELF header is invalid"))?,
            &bytes,
        )?
    };
    let mut needed = Vec::new();
    for library in file
        .import_libraries()
        .map_err(|_| invalid("ELF dynamic library table is invalid"))?
    {
        let library = library.map_err(|_| invalid("ELF dynamic library entry is invalid"))?;
        needed.push(
            str::from_utf8(library.name())
                .map_err(|_| invalid("ELF dynamic library name is not UTF-8"))?
                .to_owned(),
        );
    }
    needed.sort();
    needed.dedup();
    Ok(ElfRuntime {
        interpreter,
        needed,
    })
}

fn interpreter_from_header<H>(header: &H, bytes: &[u8]) -> Result<Option<String>>
where
    H: FileHeader<Endian = Endianness>,
{
    let endian = header
        .endian()
        .map_err(|_| invalid("ELF endian is invalid"))?;
    let headers = header
        .program_headers(endian, bytes)
        .map_err(|_| invalid("ELF program header table is invalid"))?;
    for program in headers {
        if program.p_type(endian) != elf::PT_INTERP {
            continue;
        }
        let (offset, size) = program.file_range(endian);
        let end = offset
            .checked_add(size)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| invalid("ELF interpreter range is invalid"))?;
        let data = bytes
            .get(
                usize::try_from(offset).map_err(|_| invalid("ELF interpreter range is invalid"))?
                    ..end,
            )
            .ok_or_else(|| invalid("ELF interpreter range is invalid"))?;
        let data = data
            .split(|byte| *byte == 0)
            .next()
            .ok_or_else(|| invalid("ELF interpreter data is empty"))?;
        return str::from_utf8(data)
            .map(str::to_owned)
            .map(Some)
            .map_err(|_| invalid("ELF interpreter is not UTF-8"));
    }
    Ok(None)
}
