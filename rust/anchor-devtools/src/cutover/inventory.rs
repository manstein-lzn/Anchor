use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

use super::{
    Options,
    filesystem::{self, Snapshot, Stamp},
};

const SECRET_KEYS: &[&str] = &[
    "secret_file",
    "credentials_file",
    "token_file",
    "api_key_file",
    "private_key_file",
];
const SECRET_PARTS: &[&str] = &["secret", "credential", "token", "password", "private-key"];

pub(super) fn credentials(
    options: &Options,
    legacy: &Snapshot,
    issues: &mut BTreeSet<String>,
) -> BTreeSet<PathBuf> {
    let mut candidates = options
        .credential_paths
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for entry in &legacy.entries {
        if entry.kind == "directory" {
            continue;
        }
        let name = Path::new(&entry.path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        if name == ".env"
            || SECRET_PARTS.iter().any(|part| name.contains(part))
            || name.ends_with(".pem")
            || name.ends_with(".key")
        {
            candidates.insert(legacy.path.join(&entry.path));
        }
    }
    if let Some(config) = &options.config {
        let result = (|| {
            if candidates.contains(config) {
                return Err("config is a credential path; refusing to read its contents".to_owned());
            }
            let parent = filesystem::directory(config.parent().unwrap(), false)
                .map_err(|error| error.to_string())?
                .ok_or("config parent is missing")?;
            let file = filesystem::regular(
                &parent,
                config
                    .file_name()
                    .unwrap()
                    .to_str()
                    .ok_or("invalid config filename")?,
            )
            .map_err(|error| error.to_string())?;
            let value = filesystem::json_file(file).map_err(|error| error.to_string())?;
            configured_paths(&value, config.parent().unwrap(), &mut candidates)
        })();
        if let Err(error) = result {
            issues.insert(format!(
                "config could not be inspected for credential paths: {}: {error}",
                config.display()
            ));
        }
    }
    candidates
}

fn configured_paths(
    value: &Value,
    base: &Path,
    found: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    match value {
        Value::Object(items) => {
            for (name, item) in items {
                if SECRET_KEYS.contains(&name.to_lowercase().as_str()) {
                    if let Some(value) = item.as_str().filter(|value| !value.trim().is_empty()) {
                        let path = if value.starts_with('~') || Path::new(value).is_absolute() {
                            filesystem::absolute(value)?
                        } else {
                            filesystem::absolute(
                                base.join(value)
                                    .to_str()
                                    .ok_or("invalid configured credential path")?,
                            )?
                        };
                        found.insert(path);
                    } else if !item.is_null() && item.as_str().is_none() {
                        return Err("configured credential file path is not a string".into());
                    }
                } else {
                    configured_paths(item, base, found)?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                configured_paths(item, base, found)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct Turns {
    pub(super) ids: BTreeSet<String>,
    pub(super) running: BTreeSet<String>,
}

#[derive(Default)]
pub(super) struct Legacy {
    pub(super) runs: Vec<Value>,
    pub(super) run_ids: BTreeMap<String, Vec<String>>,
    pub(super) unfinished_runs: Vec<Value>,
    pub(super) sessions: Vec<Value>,
    pub(super) session_ids: BTreeSet<String>,
    pub(super) referenced_run_ids: BTreeSet<String>,
    pub(super) turns: Turns,
}

fn record(
    snapshot: &Snapshot,
    path: &str,
    credentials: &BTreeSet<PathBuf>,
) -> Result<Value, String> {
    if credentials.contains(&snapshot.path.join(path)) {
        return Err("record is a credential path; refusing to read its contents".into());
    }
    let root = snapshot
        .directory
        .as_ref()
        .ok_or("unsafe or missing root")?;
    filesystem::relative_file(root, path)
        .and_then(filesystem::json_file)
        .map_err(|error| error.to_string())
}

fn populated(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
        Some(Value::Number(value)) => value.as_f64() != Some(0.0),
    }
}

pub(super) fn legacy(
    snapshot: &Snapshot,
    credentials: &BTreeSet<PathBuf>,
    issues: &mut BTreeSet<String>,
) -> Legacy {
    let mut found = Legacy::default();
    for path in ["workspaces", "sessions", "state"] {
        snapshot.require_directory(path, issues);
    }
    for entry in &snapshot.entries {
        let components = entry.path.split('/').collect::<Vec<_>>();
        if components.len() == 3 && components[0] == "workspaces" && components[2] == "runs" {
            snapshot.require_directory(&entry.path, issues);
        }
        if entry.kind == "directory"
            && components.len() == 4
            && components[0] == "workspaces"
            && components[2] == "runs"
        {
            let identifier = components[3];
            let location = format!("{}/run.json", entry.path);
            if !snapshot.contains(&location) {
                let admission = format!("{}/admission.json", entry.path);
                found.unfinished_runs.push(json!({
                    "id": identifier,
                    "status": if snapshot.contains(&admission) { "pending_admission" } else { "unknown" },
                    "path": if snapshot.contains(&admission) { admission } else { entry.path.clone() },
                }));
                found
                    .run_ids
                    .entry(identifier.into())
                    .or_default()
                    .push(entry.path.clone());
                continue;
            }
            found
                .run_ids
                .entry(identifier.into())
                .or_default()
                .push(location.clone());
            let result = (|| {
                let state = record(snapshot, &location, credentials)?;
                for field in ["id", "run_id"] {
                    if let Some(value) = state.get(field)
                        && value.as_str() != Some(identifier)
                    {
                        return Err("Run path/id mismatch".to_owned());
                    }
                }
                let status = state["status"]
                    .as_str()
                    .filter(|status| !status.is_empty())
                    .ok_or("missing Run status")?;
                found.runs.push(json!({"id": identifier, "workspace": components[1], "status": status, "path": location}));
                if !matches!(status, "finished" | "failed" | "stopped")
                    || ["cursor", "active", "parallel"]
                        .iter()
                        .any(|field| populated(state.get(*field)))
                {
                    found
                        .unfinished_runs
                        .push(json!({"id": identifier, "status": status, "path": location}));
                }
                Ok::<_, String>(())
            })();
            if let Err(error) = result {
                issues.insert(format!(
                    "unreadable Run record {}: {error}",
                    snapshot.path.join(&location).display()
                ));
                found
                    .unfinished_runs
                    .push(json!({"id": identifier, "status": "unknown", "path": location}));
            }
        } else if entry.kind == "directory" && components.len() == 2 && components[0] == "sessions"
        {
            let identifier = components[1];
            let location = format!("{}/session.json", entry.path);
            found.session_ids.insert(identifier.into());
            let result = (|| {
                let session = record(snapshot, &location, credentials)?;
                if session["id"].as_str() != Some(identifier) {
                    return Err("Session path/id mismatch".to_owned());
                }
                let status = session["status"]
                    .as_str()
                    .filter(|status| !status.is_empty())
                    .ok_or("missing Session status")?;
                if !matches!(
                    status,
                    "active" | "waiting_user" | "interrupted" | "archived"
                ) {
                    return Err("unknown Session status".into());
                }
                if let Some(run_ids) = session.get("run_ids") {
                    for identifier in run_ids
                        .as_array()
                        .ok_or("Session run_ids must be an array")?
                    {
                        let identifier = identifier
                            .as_str()
                            .filter(|identifier| !identifier.is_empty())
                            .ok_or("invalid Session Run ID")?;
                        found.referenced_run_ids.insert(identifier.into());
                    }
                }
                found
                    .sessions
                    .push(json!({"id": identifier, "status": status, "path": location}));
                Ok::<_, String>(())
            })();
            if let Err(error) = result {
                issues.insert(format!(
                    "unreadable Session record {}: {error}",
                    snapshot.path.join(&location).display()
                ));
            }
        }
    }
    let turns = sqlite(
        snapshot,
        "state/pilot-turns.sqlite",
        &["turns"],
        true,
        credentials,
        issues,
    );
    found.turns.ids = turns.turns;
    found.turns.running = turns.running;
    found
}

pub(super) fn rust_run_ids(snapshot: &Snapshot, issues: &mut BTreeSet<String>) -> BTreeSet<String> {
    let mut identifiers = BTreeSet::new();
    for directory in ["runs", "run-metadata"] {
        snapshot.require_directory(directory, issues);
        for entry in &snapshot.entries {
            let Some(name) = entry.path.strip_prefix(&format!("{directory}/")) else {
                continue;
            };
            if name.contains('/') {
                continue;
            }
            if entry.kind != "file" {
                issues.insert(format!(
                    "{} in Rust state directory: {}",
                    if entry.kind == "symlink" {
                        "symlink"
                    } else {
                        "non-regular entry"
                    },
                    snapshot.path.join(&entry.path).display()
                ));
            } else if let Some(identifier) = name.strip_suffix(".json") {
                if identifier.is_empty() {
                    issues.insert("empty Rust Run ID".into());
                } else {
                    identifiers.insert(identifier.into());
                }
            }
        }
    }
    identifiers
}

#[derive(Default)]
pub(super) struct SqliteRecords {
    pub(super) sessions: BTreeSet<String>,
    pub(super) turns: BTreeSet<String>,
    pub(super) running: BTreeSet<String>,
}

fn sqlite_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            uri.push(*byte as char);
        } else {
            use std::fmt::Write;
            write!(&mut uri, "%{byte:02X}").unwrap();
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    uri
}

pub(super) fn sqlite(
    snapshot: &Snapshot,
    relative: &str,
    tables: &[&str],
    turn_status: bool,
    credentials: &BTreeSet<PathBuf>,
    issues: &mut BTreeSet<String>,
) -> SqliteRecords {
    let mut records = SqliteRecords::default();
    if let Some(parent) = Path::new(relative).parent().and_then(Path::to_str) {
        snapshot.require_directory(parent, issues);
    }
    if !snapshot.contains(relative) {
        return records;
    }
    let path = snapshot.path.join(relative);
    let result = (|| {
        if credentials.contains(&path) {
            return Err("database is a credential path; refusing to read its contents".into());
        }
        let root = snapshot.directory.as_ref().ok_or("unsafe database root")?;
        let sidecars = || -> Result<(), String> {
            let parent = filesystem::directory(path.parent().unwrap(), false)
                .map_err(|error| error.to_string())?
                .ok_or("database parent is missing")?;
            for suffix in ["-wal", "-shm", "-journal"] {
                let name = format!("{}{suffix}", path.file_name().unwrap().to_string_lossy());
                match rustix::fs::statat(
                    &parent,
                    name.as_str(),
                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                ) {
                    Err(rustix::io::Errno::NOENT) => {}
                    Err(_) => return Err("cannot inspect SQLite sidecars".into()),
                    Ok(_) => {
                        return Err(format!(
                            "SQLite {} sidecar present; refusing an incomplete immutable snapshot: {}",
                            suffix.to_uppercase(),
                            path.with_file_name(name).display()
                        ));
                    }
                }
            }
            Ok(())
        };
        sidecars()?;
        let file = filesystem::relative_file(root, relative).map_err(|error| error.to_string())?;
        let before = Stamp::of(&rustix::fs::fstat(&file).map_err(|error| error.to_string())?);
        let database = Connection::open_with_flags(
            sqlite_uri(&path),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NOFOLLOW
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| "SQLite immutable read-only open failed")?;
        database
            .busy_timeout(Duration::from_millis(200))
            .map_err(|_| "SQLite timeout setup failed")?;
        database
            .pragma_update(None, "query_only", true)
            .map_err(|_| "SQLite query-only setup failed")?;
        for table in tables {
            let exists: bool = database
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table],
                    |row| row.get(0),
                )
                .map_err(|_| "cannot inspect SQLite schema")?;
            if !exists {
                return Err(format!("required SQLite table is missing: {table}"));
            }
            let query = match (*table, turn_status) {
                ("turns", true) => "SELECT id, status FROM turns ORDER BY id",
                ("turns", false) => "SELECT id FROM turns ORDER BY id",
                ("sessions", _) => "SELECT id FROM sessions ORDER BY id",
                _ => return Err("unsupported SQLite table".into()),
            };
            let mut statement = database
                .prepare(query)
                .map_err(|_| "cannot inspect SQLite ID/status columns")?;
            let mut rows = statement
                .query([])
                .map_err(|_| "cannot inspect SQLite IDs")?;
            while let Some(row) = rows.next().map_err(|_| "cannot read SQLite IDs")? {
                let identifier: String = row.get(0).map_err(|_| "SQLite ID must be text")?;
                let identifiers = if *table == "sessions" {
                    &mut records.sessions
                } else {
                    &mut records.turns
                };
                if identifier.is_empty() || !identifiers.insert(identifier.clone()) {
                    return Err("empty or duplicate SQLite ID".into());
                }
                if *table == "turns" && turn_status {
                    let status: String =
                        row.get(1).map_err(|_| "SQLite Turn status must be text")?;
                    if status == "running" {
                        records.running.insert(identifier);
                    } else if !matches!(
                        status.as_str(),
                        "completed"
                            | "failed"
                            | "stopped"
                            | "interrupted"
                            | "waiting_approval"
                            | "waiting_user"
                    ) {
                        return Err("unknown legacy Turn status".into());
                    }
                }
            }
        }
        drop(database);
        sidecars()?;
        let current =
            filesystem::relative_file(root, relative).map_err(|error| error.to_string())?;
        if before != Stamp::of(&rustix::fs::fstat(&current).map_err(|error| error.to_string())?) {
            return Err("SQLite database changed during immutable inspection".into());
        }
        Ok::<_, String>(())
    })();
    if let Err(error) = result {
        issues.insert(format!("cannot inspect {}: {error}", path.display()));
    }
    records
}
