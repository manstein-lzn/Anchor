use std::{collections::BTreeSet, path::PathBuf};

use serde_json::{Value, json};

#[path = "cutover/filesystem.rs"]
mod filesystem;
#[path = "cutover/inventory.rs"]
mod inventory;
#[path = "cutover/prepare.rs"]
mod prepare;

#[cfg(test)]
#[path = "cutover/tests.rs"]
mod tests;

struct Options {
    legacy_root: PathBuf,
    rust_state_root: PathBuf,
    rust_workspace_root: PathBuf,
    rust_catalog_root: PathBuf,
    rust_library_root: PathBuf,
    config: Option<PathBuf>,
    credential_paths: Vec<PathBuf>,
    legacy_writer_stopped: bool,
    legacy_read_only_confirmed: bool,
    credentials_reviewed: bool,
    prepare: bool,
    output_dir: Option<PathBuf>,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut paths = std::collections::BTreeMap::new();
        let mut credential_paths = Vec::new();
        let mut switches = BTreeSet::new();
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            if matches!(argument.as_str(), "--help" | "-h") {
                return Err("cutover requires --rust-state-root, --rust-workspace-root and --rust-catalog-root; use anchor-devtools cutover --help".into());
            }
            let (flag, inline) = argument
                .split_once('=')
                .map_or((argument.as_str(), None), |(flag, value)| {
                    (flag, Some(value))
                });
            match flag {
                "--legacy-writer-stopped"
                | "--legacy-read-only-confirmed"
                | "--credentials-reviewed"
                | "--prepare" => {
                    if inline.is_some() {
                        return Err(format!("{flag} does not take a value"));
                    }
                    switches.insert(flag.to_owned());
                }
                "--legacy-root"
                | "--rust-state-root"
                | "--rust-workspace-root"
                | "--rust-catalog-root"
                | "--rust-library-root"
                | "--config"
                | "--credential-path"
                | "--output-dir" => {
                    let value = inline.or_else(|| arguments.next().map(String::as_str));
                    let value = value
                        .filter(|value| !value.is_empty() && !value.starts_with("--"))
                        .ok_or_else(|| format!("missing path for {flag}"))?;
                    let path = filesystem::absolute(value)?;
                    if flag == "--credential-path" {
                        credential_paths.push(path);
                    } else {
                        paths.insert(flag.to_owned(), path);
                    }
                }
                _ => return Err(format!("unknown cutover argument: {flag}")),
            }
        }
        let mut required = |flag: &str| {
            paths
                .remove(flag)
                .ok_or_else(|| format!("missing required cutover argument: {flag}"))
        };
        let rust_state_root = required("--rust-state-root")?;
        let rust_workspace_root = required("--rust-workspace-root")?;
        let rust_catalog_root = required("--rust-catalog-root")?;
        let prepare = switches.contains("--prepare");
        let output_dir = paths.remove("--output-dir");
        if prepare && output_dir.is_none() {
            return Err("--prepare requires --output-dir".into());
        }
        Ok(Self {
            legacy_root: match paths.remove("--legacy-root") {
                Some(path) => path,
                None => filesystem::absolute("~/.anchor")?,
            },
            rust_state_root,
            rust_workspace_root,
            rust_library_root: paths
                .remove("--rust-library-root")
                .unwrap_or_else(|| rust_catalog_root.clone()),
            rust_catalog_root,
            config: paths.remove("--config"),
            credential_paths,
            legacy_writer_stopped: switches.contains("--legacy-writer-stopped"),
            legacy_read_only_confirmed: switches.contains("--legacy-read-only-confirmed"),
            credentials_reviewed: switches.contains("--credentials-reviewed"),
            prepare,
            output_dir,
        })
    }
}

pub fn run(arguments: &[String]) -> Result<Value, String> {
    let options = Options::parse(arguments)?;
    let roots = [
        ("legacy", &options.legacy_root),
        ("rust_state", &options.rust_state_root),
        ("rust_workspace", &options.rust_workspace_root),
        ("rust_catalog", &options.rust_catalog_root),
        ("rust_library", &options.rust_library_root),
    ];
    let mut issues = BTreeSet::new();
    let mut blockers = BTreeSet::new();
    let snapshots = roots
        .iter()
        .map(|(label, path)| filesystem::Snapshot::inspect(path, label, &mut issues))
        .collect::<Vec<_>>();
    let legacy = &snapshots[0];
    let rust_state = &snapshots[1];
    if legacy.directory.is_none() {
        issues.insert(format!(
            "missing root or unsafe legacy root: {}",
            options.legacy_root.display()
        ));
    }
    for (position, (label, root)) in roots.iter().enumerate() {
        for (other_label, other) in &roots[position + 1..] {
            if *label == "rust_catalog" && *other_label == "rust_library" && root == other {
                continue;
            }
            if filesystem::overlap(root, other) {
                blockers.insert(format!("data roots overlap: {label}/{other_label}"));
            }
        }
    }
    if let Some(output) = &options.output_dir {
        if roots
            .iter()
            .any(|(_, root)| filesystem::overlap(output, root))
        {
            blockers.insert("preparation output overlaps a data root".into());
        }
        if let Err(error) = filesystem::directory(output, false) {
            issues.insert(format!(
                "unsafe preparation output {}: {error}",
                output.display()
            ));
        }
    }
    let credentials = inventory::credentials(&options, legacy, &mut issues);
    let legacy_records = inventory::legacy(legacy, &credentials, &mut issues);
    let rust_runs = inventory::rust_run_ids(rust_state, &mut issues);
    let rust_records = inventory::sqlite(
        rust_state,
        "platform/sessions.sqlite",
        &["sessions", "turns"],
        false,
        &credentials,
        &mut issues,
    );
    let conflicts = json!({
        "legacy_duplicate_run_ids": legacy_records.run_ids.iter()
            .filter(|(_, locations)| locations.len() > 1)
            .map(|(identifier, locations)| (identifier.clone(), json!(locations)))
            .collect::<serde_json::Map<_, _>>(),
        "rust_run_ids": legacy_records.run_ids.keys().cloned()
            .chain(legacy_records.referenced_run_ids.iter().cloned())
            .collect::<BTreeSet<_>>().intersection(&rust_runs).collect::<Vec<_>>(),
        "rust_session_ids": legacy_records.session_ids.intersection(&rust_records.sessions).collect::<Vec<_>>(),
        "rust_turn_ids": legacy_records.turns.ids.intersection(&rust_records.turns).collect::<Vec<_>>(),
    });
    let legacy_lock = options.legacy_root.join("library/plugins/.install.lock");
    let legacy_lock_status = filesystem::lock_status(&legacy_lock);
    let deployment_lock = options
        .rust_state_root
        .join("deployment-locks/.deployment-writer.lock");
    let deployment_lock_status = filesystem::lock_status(&deployment_lock);
    let mut run_locks = std::collections::BTreeMap::new();
    for entry in &rust_state.entries {
        let Some(name) = entry.path.strip_prefix("runs/") else {
            continue;
        };
        if !name.contains('/') && name.starts_with('.') && name.ends_with(".lock") {
            let path = options.rust_state_root.join(&entry.path);
            run_locks.insert(
                path.to_string_lossy().into_owned(),
                filesystem::lock_status(&path),
            );
        }
    }
    let has_data = !legacy.entries.is_empty();
    let readonly_observed = legacy.readonly();
    let readonly_confirmed = readonly_observed || options.legacy_read_only_confirmed;
    let writer_confirmed = options.legacy_writer_stopped || !has_data;
    if !legacy_records.unfinished_runs.is_empty() {
        blockers.insert("legacy unfinished or unreadable Run/admission exists".into());
    }
    if !legacy_records.turns.running.is_empty() {
        blockers.insert("legacy running Turn exists".into());
    }
    if conflicts
        .as_object()
        .unwrap()
        .values()
        .any(|value| match value {
            Value::Array(items) => !items.is_empty(),
            Value::Object(items) => !items.is_empty(),
            _ => true,
        })
    {
        blockers.insert("Run, Session, or Turn IDs conflict".into());
    }
    if !credentials.is_empty() && !options.credentials_reviewed {
        blockers.insert("credential paths need explicit review; no secrets are copied".into());
    }
    if !matches!(legacy_lock_status, "absent" | "available") {
        blockers.insert(format!(
            "legacy Library install lock is {legacy_lock_status}"
        ));
    }
    if !matches!(deployment_lock_status, "absent" | "available") {
        blockers.insert(format!(
            "Rust deployment writer lock is {deployment_lock_status}"
        ));
    }
    if run_locks.values().any(|status| *status != "available") {
        blockers.insert("Rust Run write lock is held or cannot be checked".into());
    }
    if !writer_confirmed {
        blockers.insert(
            "legacy service stop is not confirmed (legacy has no durable host-wide lease)".into(),
        );
    }
    if !readonly_confirmed {
        blockers.insert(
            "legacy root is writable; mount it read-only or explicitly confirm the external policy"
                .into(),
        );
    }
    for snapshot in &snapshots {
        if let Err(error) = snapshot.verify() {
            issues.insert(error);
        }
    }
    if !issues.is_empty() {
        blockers.insert("inventory is incomplete or contains unsafe filesystem entries".into());
    }
    let decision = if !blockers.is_empty() {
        "blocked"
    } else if has_data {
        "legacy_read_only"
    } else {
        "migration_candidate"
    };
    let files = legacy
        .entries
        .iter()
        .filter(|entry| entry.kind == "file")
        .collect::<Vec<_>>();
    let mut report = json!({
        "schema_version": 1,
        "decision": decision,
        "blockers": blockers,
        "inventory_issues": issues,
        "inventory": {
            "legacy_root": options.legacy_root,
            "entry_count": legacy.entries.len(),
            "file_count": files.len(),
            "bytes": files.iter().map(|entry| entry.bytes).sum::<u64>(),
            "runs": legacy_records.runs,
            "sessions": legacy_records.sessions,
            "legacy_turn_ids": legacy_records.turns.ids,
            "paths": legacy.entries,
        },
        "unfinished_runs": legacy_records.unfinished_runs,
        "running_turn_ids": legacy_records.turns.running,
        "id_conflicts": conflicts,
        "credential_paths": credentials,
        "locks": {
            "legacy_library_install": {"path": legacy_lock, "status": legacy_lock_status},
            "rust_deployment_writer": {"path": deployment_lock, "status": deployment_lock_status},
            "rust_run_writers": run_locks,
            "rust_single_writer_policy": "one deployment lease; recheck at Host startup",
        },
        "policy": {
            "legacy_data": "read-only; no import, move, delete, or dual-write",
            "rust_state_root": options.rust_state_root,
            "rust_workspace_root": options.rust_workspace_root,
            "rust_catalog_root": options.rust_catalog_root,
            "rust_library_root": options.rust_library_root,
            "legacy_read_only_observed": readonly_observed,
            "legacy_read_only_confirmed": readonly_confirmed,
            "legacy_writer_stopped_confirmed": writer_confirmed,
            "secret_migration": "never; provision Rust credentials separately",
            "data_migration": "not_performed; inspect/import only in a separately authorized operation",
        },
        "rollback_preconditions": [
            "preserve the complete legacy root unchanged",
            "stop Rust writer before restoring legacy service",
            "preserve Rust state root; never point legacy at Rust-owned facts",
            "reconcile or retain every Run/Session/Turn accepted after cutover before rollback",
            "recheck ID conflicts and credential provisioning before either direction",
        ],
        "preparation_output": options.output_dir,
    });
    if options.prepare {
        if report["decision"] == "blocked" {
            report["error"] = json!("cutover is blocked; no preparation files were written");
        } else {
            match prepare::write(&report, options.output_dir.as_ref().unwrap(), &snapshots) {
                Ok(files) => report["prepared_files"] = json!(files),
                Err(error) => {
                    report["decision"] = json!("blocked");
                    report["blockers"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(error));
                    report["error"] = json!(error);
                }
            }
        }
    }
    Ok(report)
}
