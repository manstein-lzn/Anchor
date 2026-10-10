use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata},
    net::IpAddr,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use rustix::fs::{Access, AtFlags, FlockOperation, Mode, OFlags};
use serde_json::{Value, json};

#[path = "preflight/elf.rs"]
mod elf;
#[path = "preflight/runtime.rs"]
mod runtime;

const GOOSE_VERSION: &str = "1.53.0";
const GOOSE_SHA256: &str = "71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7";

pub fn run() -> Result<Value, String> {
    let mut environment = BTreeMap::new();
    for (name, value) in std::env::vars_os() {
        let Ok(name) = name.into_string() else {
            continue;
        };
        match value.into_string() {
            Ok(value) => {
                environment.insert(name, value);
            }
            Err(_) if name.starts_with("ANCHOR_") || name == "PATH" => {
                return Err("preflight configuration must be UTF-8".into());
            }
            Err(_) => {}
        }
    }
    check(&environment)
}

pub fn check(env: &BTreeMap<String, String>) -> Result<Value, String> {
    check_with_pin(env, GOOSE_SHA256)
}

fn check_with_pin(env: &BTreeMap<String, String>, goose_sha256: &str) -> Result<Value, String> {
    check_env_file(env)?;
    check_api_keys(env)?;
    check_endpoint(env)?;
    if let Some(window) = env.get("ANCHOR_MODEL_CONTEXT_WINDOW")
        && window
            .trim()
            .parse::<u64>()
            .map_or(true, |value| value == 0)
    {
        return Err("ANCHOR_MODEL_CONTEXT_WINDOW must be a positive integer".into());
    }
    // Mirrors the runtime decision: `0` shares the host network, `1` isolates, and
    // an unset value isolates whenever a relay ships beside the pinned Goose binary.
    let relay = relay_path(env);
    let isolated = match value(env, "ANCHOR_GOOSE_LOCAL_NETWORK", "") {
        "0" => false,
        "1" => true,
        _ => relay.as_deref().is_some_and(std::path::Path::is_file),
    };
    if !isolated && required(env, "ANCHOR_GOOSE_ALLOW_SHARED_NETWORK")? != "1" {
        return Err(
            "Goose needs either ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1 (shared host network) or an anchor-net-relay binary beside ANCHOR_GOOSE_BINARY (isolated sandbox)".into(),
        );
    }
    if isolated && !relay.as_deref().is_some_and(std::path::Path::is_file) {
        return Err(
            "isolated Goose needs an anchor-net-relay binary beside ANCHOR_GOOSE_BINARY or ANCHOR_GOOSE_RELAY_BINARY".into(),
        );
    }
    let runtime_root = runtime::check(env, goose_sha256)?;
    check_mutable_roots(env, &runtime_root)?;
    Ok(json!({"status": "passed"}))
}

/// The relay the runtime would use: an explicit path, else the one shipped beside
/// the pinned Goose binary.
fn relay_path(env: &BTreeMap<String, String>) -> Option<std::path::PathBuf> {
    if let Some(configured) = env
        .get("ANCHOR_GOOSE_RELAY_BINARY")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Some(std::path::PathBuf::from(configured));
    }
    let binary = env.get("ANCHOR_GOOSE_BINARY")?.trim();
    (!binary.is_empty()).then(|| std::path::Path::new(binary).with_file_name("anchor-net-relay"))
}

fn value<'env>(env: &'env BTreeMap<String, String>, name: &str, default: &'env str) -> &'env str {
    env.get(name).map(String::as_str).unwrap_or(default)
}

fn required<'env>(env: &'env BTreeMap<String, String>, name: &str) -> Result<&'env str, String> {
    let configured = value(env, name, "").trim();
    if configured.is_empty() {
        Err(format!("{name} is required"))
    } else {
        Ok(configured)
    }
}

fn absolute_path(configured: &str, name: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(configured);
    if !path.is_absolute() {
        return Err(format!("{name} must be an absolute path"));
    }
    Ok(path)
}

fn reject_symlinks(path: &Path, name: &str) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata =
            fs::symlink_metadata(&current).map_err(|_| format!("{name} is unavailable"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("{name} must not contain symlink path components"));
        }
    }
    Ok(())
}

fn metadata(path: &Path, name: &str) -> Result<Metadata, String> {
    reject_symlinks(path, name)?;
    fs::symlink_metadata(path).map_err(|_| format!("{name} is unavailable"))
}

fn accessible(path: &Path, access: Access) -> bool {
    rustix::fs::accessat(rustix::fs::CWD, path, access, AtFlags::EACCESS).is_ok()
}

fn check_directory(path: &Path, name: &str) -> Result<(), String> {
    let metadata = metadata(path, name)?;
    if !metadata.is_dir() {
        return Err(format!("{name} must be a real directory"));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!("{name} must be owned by the service user"));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(format!("{name} must not be group- or world-writable"));
    }
    if !accessible(path, Access::READ_OK | Access::WRITE_OK | Access::EXEC_OK) {
        return Err(format!(
            "{name} is not accessible with the required permissions"
        ));
    }
    Ok(())
}

fn check_env_file(env: &BTreeMap<String, String>) -> Result<(), String> {
    let path = absolute_path(required(env, "ANCHOR_ENV_FILE")?, "ANCHOR_ENV_FILE")?;
    let metadata = metadata(&path, "ANCHOR_ENV_FILE")?;
    if !metadata.is_file() || ![0, rustix::process::geteuid().as_raw()].contains(&metadata.uid()) {
        return Err("ANCHOR_ENV_FILE must be a regular root- or service-owned file".into());
    }
    if metadata.mode() & 0o077 != 0 {
        return Err("ANCHOR_ENV_FILE must have mode 0600 or stricter".into());
    }
    Ok(())
}

fn check_endpoint(env: &BTreeMap<String, String>) -> Result<(), String> {
    let endpoint = required(env, "ANCHOR_MODEL_URL")?;
    let invalid =
        || "ANCHOR_MODEL_URL must be an absolute HTTPS URL or loopback HTTP URL".to_owned();
    let (_, authority) = endpoint.split_once("://").ok_or_else(invalid)?;
    let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty()
        || authority.starts_with('/')
        || authority.contains('@')
        || endpoint
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || endpoint.contains('\\')
    {
        return Err(invalid());
    }
    let parsed = url::Url::parse(endpoint).map_err(|_| invalid())?;
    let raw_host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    let loopback = matches!(
        parsed.host(),
        Some(url::Host::Domain("localhost"))
            | Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST))
            | Some(url::Host::Ipv6(std::net::Ipv6Addr::LOCALHOST))
    );
    if parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some_and(|query| !query.is_empty())
        || parsed
            .fragment()
            .is_some_and(|fragment| !fragment.is_empty())
        || !(parsed.scheme() == "https"
            || parsed.scheme() == "http"
                && loopback
                && matches!(
                    raw_host.to_ascii_lowercase().as_str(),
                    "127.0.0.1" | "::1" | "localhost"
                ))
        || parsed.port() == Some(0)
    {
        return Err(invalid());
    }
    required(env, "ANCHOR_MODEL_API_KEY")?;
    let model = required(env, "ANCHOR_MODEL_NAME")?;
    if model.starts_with("REPLACE_") {
        return Err("replace the model name placeholder before starting".into());
    }
    if model.chars().any(|character| (character as u32) < 32) {
        return Err("model name contains control characters".into());
    }
    if !matches!(
        value(env, "ANCHOR_MODEL_WIRE_API", "responses"),
        "chat" | "responses"
    ) {
        return Err("ANCHOR_MODEL_WIRE_API must be chat or responses".into());
    }
    serde_json::from_str::<BTreeMap<String, String>>(value(env, "ANCHOR_MODEL_ALIASES", "{}"))
        .map_err(|_| "ANCHOR_MODEL_ALIASES must be a JSON object of strings".to_owned())?;
    Ok(())
}

fn check_api_keys(env: &BTreeMap<String, String>) -> Result<(), String> {
    let listen = value(env, "ANCHOR_RUNNER_LISTEN", "127.0.0.1:8077");
    let invalid = || "ANCHOR_RUNNER_LISTEN must be a host:port address".to_owned();
    let (host, port_text) = if let Some(bracketed) = listen.strip_prefix('[') {
        let (host, port) = bracketed.split_once("]:").ok_or_else(invalid)?;
        if !matches!(host.parse::<IpAddr>(), Ok(IpAddr::V6(_))) {
            return Err(invalid());
        }
        (host, port)
    } else {
        let (host, port) = listen.rsplit_once(':').ok_or_else(invalid)?;
        if host.contains(':') {
            return Err(invalid());
        }
        (host, port)
    };
    if host.is_empty()
        || host
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        || port_text.is_empty()
        || !port_text.bytes().all(|byte| byte.is_ascii_digit())
        || !port_text.parse::<u16>().is_ok_and(|port| port != 0)
    {
        return Err(invalid());
    }
    let loopback = host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    let raw_keys = value(env, "ANCHOR_API_KEYS", "").trim();
    let keys: Vec<String> = if raw_keys.is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(raw_keys)
            .map_err(|_| "ANCHOR_API_KEYS must be a JSON array of strings".to_owned())?
    };
    if keys.iter().any(|key| key.len() < 32)
        || keys.iter().collect::<BTreeSet<_>>().len() != keys.len()
    {
        return Err("ANCHOR_API_KEYS must contain unique secrets of at least 32 bytes".into());
    }
    if !loopback && keys.is_empty() {
        return Err("ANCHOR_API_KEYS is required for non-loopback listeners".into());
    }
    Ok(())
}

fn overlaps(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

fn system_executable(env: &BTreeMap<String, String>, configured: &str) -> bool {
    let executable = |path: &Path| path.is_file() && accessible(path, Access::EXEC_OK);
    if Path::new(configured).is_absolute() {
        return executable(Path::new(configured));
    }
    if Path::new(configured).components().count() != 1 {
        return false;
    }
    std::env::split_paths(value(env, "PATH", "/usr/bin:/bin"))
        .any(|directory| directory.is_absolute() && executable(&directory.join(configured)))
}

fn check_mutable_roots(env: &BTreeMap<String, String>, runtime_root: &Path) -> Result<(), String> {
    let mut roots = Vec::new();
    for name in [
        "ANCHOR_RUNNER_STATE_ROOT",
        "ANCHOR_RUNNER_WORKSPACE_ROOT",
        "ANCHOR_RUNNER_CATALOG_ROOT",
    ] {
        let path = absolute_path(required(env, name)?, name)?;
        check_directory(&path, name)?;
        roots.push(fs::canonicalize(path).map_err(|_| format!("{name} is unavailable"))?);
    }
    if overlaps(&roots[0], &roots[1]) {
        return Err("state and workspace roots must be separate, non-nested directories".into());
    }
    for (name, path) in ["state", "workspace", "catalog"].into_iter().zip(&roots) {
        if overlaps(path, runtime_root) {
            return Err(format!(
                "{name} root must remain outside the source-free runtime"
            ));
        }
    }
    let commands = required(env, "ANCHOR_RUNNER_ALLOWED_COMMANDS")?
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>();
    if commands.iter().filter(|command| **command == "sh").count() != 1 {
        return Err("ANCHOR_RUNNER_ALLOWED_COMMANDS must explicitly include sh once".into());
    }
    if commands.iter().any(|command| {
        command.is_empty() || Path::new(command).file_name() != Some(std::ffi::OsStr::new(command))
    }) || commands.iter().collect::<BTreeSet<_>>().len() != commands.len()
    {
        return Err("ANCHOR_RUNNER_ALLOWED_COMMANDS must be unique executable basenames".into());
    }
    for command in ["sh", "git"] {
        if !system_executable(env, command) {
            return Err(format!("required system command is unavailable: {command}"));
        }
    }
    if !system_executable(env, value(env, "ANCHOR_BWRAP", "bwrap")) {
        return Err("Bubblewrap executable is unavailable".into());
    }
    check_writer_lease(&roots[0])
}

fn check_writer_lease(state: &Path) -> Result<(), String> {
    let directory_error = || {
        "deployment lock directory must be service-owned and not group- or world-writable"
            .to_owned()
    };
    let state_fd = rustix::fs::open(
        state,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| directory_error())?;
    match rustix::fs::mkdirat(&state_fd, "deployment-locks", Mode::from_raw_mode(0o750)) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::EXIST => {}
        Err(_) => return Err(directory_error()),
    }
    let directory = File::from(
        rustix::fs::openat(
            &state_fd,
            "deployment-locks",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| directory_error())?,
    );
    let metadata = directory.metadata().map_err(|_| directory_error())?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
        return Err(directory_error());
    }
    let lock_error =
        || "deployment writer lock must be a private service-owned regular file".to_owned();
    let lock = File::from(
        rustix::fs::openat(
            &directory,
            ".deployment-writer.lock",
            OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| lock_error())?,
    );
    let metadata = lock.metadata().map_err(|_| lock_error())?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(lock_error());
    }
    rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| {
        "state root has another writing Host or cannot acquire its writer lock".to_owned()
    })?;
    rustix::fs::flock(&lock, FlockOperation::Unlock)
        .map_err(|_| "state root writer lock could not be released".to_owned())
}

#[cfg(test)]
#[path = "preflight/tests.rs"]
mod tests;
