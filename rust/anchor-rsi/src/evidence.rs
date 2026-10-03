use crate::{
    ecosystem::{Dependency, discover},
    redact,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::Mutex,
};
use walkdir::WalkDir;

pub const DOMAINS: [&str; 6] = [
    "code",
    "graphs",
    "plugins",
    "runs",
    "dependencies",
    "previous",
];
const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_READ: usize = 64 * 1024;
pub const LIMITATIONS: &[&str] = &[
    "Inventories establish collection coverage, not model review or correctness.",
    "Redaction is heuristic, not a complete security audit; projections are not executable source replacements.",
    "Line locators refer to frozen projections; JSON formatting and redaction can change original line positions.",
    "Files above 8 MiB, binary/non-UTF8 files, symlinks and credential-named files are excluded with records.",
    "Run evidence contains mechanical metadata only; no prompts, messages, traces, checkpoints or reasoning.",
    "Optional roots and unavailable timestamps are explicitly reported; missing time is not counted as recent activity.",
];

#[derive(Clone)]
pub struct Config {
    pub source: PathBuf,
    pub data: PathBuf,
    pub rust_state: Option<PathBuf>,
    pub previous: Option<PathBuf>,
    pub evidence: PathBuf,
}

impl Config {
    pub fn from_environment() -> Result<Self, String> {
        let required = |name| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} is required"))
        };
        Ok(Self {
            source: required("ANCHOR_RSI_SOURCE_ROOT")?,
            data: required("ANCHOR_RSI_DATA_ROOT")?,
            rust_state: std::env::var_os("ANCHOR_RSI_RUST_STATE_ROOT").map(PathBuf::from),
            previous: std::env::var_os("ANCHOR_RSI_PREVIOUS_ROOT").map(PathBuf::from),
            evidence: required("ANCHOR_RSI_EVIDENCE_ROOT")?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub domain: String,
    pub source: String,
    pub frozen_file: String,
    pub sha256: String,
    pub original_sha256: String,
    pub lines: usize,
    pub redacted: bool,
    #[serde(skip)]
    pub(crate) content: String,
}

pub struct Evidence {
    pub entries: BTreeMap<String, Entry>,
    pub issues: Vec<Value>,
    pub dependencies: Vec<Dependency>,
    pub captured_at: DateTime<Utc>,
    audit: Mutex<fs::File>,
    supplementary: Mutex<BTreeMap<String, Entry>>,
    root: PathBuf,
}

pub fn safe_path(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            _ => return Err("paths must be absolute without parent components".into()),
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("symlinks are not authorized".into());
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.to_string()),
        }
    }
    if !path.is_absolute() {
        return Err("root must be absolute".into());
    }
    Ok(())
}

impl Evidence {
    pub fn collect(config: Config) -> Result<Self, String> {
        for (name, root) in [("source", &config.source), ("data", &config.data)] {
            safe_path(root)?;
            if !root.is_dir() {
                return Err(format!(
                    "required {name} root is missing or not a directory"
                ));
            }
        }
        safe_path(&config.evidence)?;
        if config.evidence.exists()
            && fs::read_dir(&config.evidence)
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
        {
            return Err("evidence root must be empty for an independent collection".into());
        }
        fs::create_dir_all(config.evidence.join("files")).map_err(|e| e.to_string())?;
        let audit = fs::OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(config.evidence.join("tool-calls.jsonl"))
            .map_err(|e| e.to_string())?;
        let mut result = Self {
            entries: BTreeMap::new(),
            issues: Vec::new(),
            dependencies: Vec::new(),
            captured_at: Utc::now(),
            audit: Mutex::new(audit),
            supplementary: Mutex::new(BTreeMap::new()),
            root: config.evidence.clone(),
        };
        result.walk(&config.source, "code", false)?;
        let workspaces = config.data.join("workspaces");
        if workspaces.is_dir() {
            for directory in sorted_dirs(&workspaces)? {
                result.file(&directory.join("graph.json"), &config.data, "graphs")?;
                let runs = directory.join("runs");
                if runs.is_dir() {
                    for run in sorted_dirs(&runs)? {
                        result.run(
                            &run.join("run.json"),
                            &config.data,
                            directory.file_name().unwrap().to_string_lossy().as_ref(),
                        )?;
                    }
                }
            }
        } else {
            result.issue("graphs/runs", "workspace directory is absent");
        }
        for kind in ["plugins", "skills"] {
            let root = config.data.join("library").join(kind);
            if root.is_dir() {
                result.walk(&root, "plugins", false)?;
            } else {
                result.issue("plugins", &format!("library/{kind} is absent"));
            }
        }
        if let Some(root) = config.rust_state {
            safe_path(&root)?;
            if root.is_dir() {
                let runs = root.join("runs");
                if runs.is_dir() {
                    safe_path(&runs)?;
                    for entry in fs::read_dir(&runs).map_err(|e| e.to_string())? {
                        let path = entry.map_err(|e| e.to_string())?.path();
                        if path.extension().is_some_and(|e| e == "json") {
                            result.run(&path, &root, "rust-native")?;
                        }
                    }
                } else {
                    result.issue("runs", "optional Rust state has no runs directory");
                }
            } else {
                result.issue("runs", "configured optional Rust state root is missing");
            }
        } else {
            result.issue("runs", "optional Rust state root was not configured");
        }
        if let Some(root) = config.previous {
            safe_path(&root)?;
            if root.is_dir() {
                result.walk(&root, "previous", true)?;
            } else {
                result.issue("previous", "configured previous-report root is missing");
            }
        } else {
            result.issue("previous", "previous-report root was not configured");
        }
        let manifests = result
            .entries
            .values()
            .filter(|entry| {
                entry.domain == "code"
                    && ["Cargo.toml", "pyproject.toml", "package.json"]
                        .iter()
                        .any(|name| entry.path.ends_with(name))
            })
            .cloned()
            .collect::<Vec<_>>();
        let (dependencies, errors) = discover(&manifests);
        result.dependencies = dependencies;
        result.issues.extend(errors);
        let declaration =
            serde_json::to_string_pretty(&result.dependencies).map_err(|e| e.to_string())?;
        result.add(
            "dependencies/declarations.json",
            "dependencies",
            "frozen source manifests",
            &declaration,
            &declaration,
        )?;
        let index = json!({"format":1,"captured_at":result.captured_at,"window_start":result.captured_at-Duration::days(7),"window_end":result.captured_at,"entries":result.entries,"issues":result.issues,"limitations":LIMITATIONS});
        fs::write(
            result.root.join("index.json"),
            serde_json::to_vec_pretty(&index).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(result)
    }

    fn issue(&mut self, path: &str, reason: &str) {
        self.issues.push(json!({"path":path,"reason":reason}));
    }

    fn walk(&mut self, root: &Path, domain: &str, reports_only: bool) -> Result<(), String> {
        safe_path(root)?;
        let output_root = self.root.clone();
        let mut skipped = Vec::new();
        for entry in WalkDir::new(root).follow_links(false).sort_by_file_name().into_iter().filter_entry(|entry| {
            let included = entry.path() == root || (!redact::excluded(&entry.file_name().to_string_lossy()) && !entry.path().starts_with(&output_root));
            if !included { skipped.push(json!({"path":format!("{domain}/{}",entry.path().strip_prefix(root).unwrap().display()),"reason":"excluded credential/runtime/cache path"})); }
            included
        }) {
            match entry {
                Ok(entry) if entry.file_type().is_symlink() => self.issue(&format!("{domain}/{}",entry.path().strip_prefix(root).unwrap().display()), "symlink skipped"),
                Ok(entry) if entry.file_type().is_file() => {
                    let extension = entry.path().extension().and_then(|e| e.to_str()).unwrap_or("");
                    if reports_only && !matches!(extension,"md"|"json"|"txt") { continue; }
                    if !reports_only && !matches!(extension,"rs"|"py"|"js"|"jsx"|"ts"|"tsx"|"css"|"html"|"md"|"toml"|"json"|"yaml"|"yml"|"txt"|"sh"|"lock"|"c"|"h"|"in"|"cfg"|"ini") && !matches!(entry.file_name().to_str(), Some("Dockerfile"|"Makefile"|"LICENSE")) { continue; }
                    self.file(entry.path(), root, domain)?;
                }
                Ok(_) => (),
                Err(error) => self.issue(domain, &format!("directory scan failed: {}",error.io_error().map(|e| e.kind().to_string()).unwrap_or_default())),
            }
        }
        self.issues.extend(skipped);
        Ok(())
    }

    fn file(&mut self, path: &Path, root: &Path, domain: &str) -> Result<(), String> {
        if !path.exists() {
            self.issue(domain, "declared file is absent");
            return Ok(());
        }
        if let Err(reason) = safe_path(path) {
            self.issue(domain, &reason);
            return Ok(());
        }
        let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
        let namespace = if domain == "plugins" {
            format!(
                "{domain}/{}",
                root.file_name().unwrap_or_default().to_string_lossy()
            )
        } else {
            domain.to_owned()
        };
        let virtual_path = format!("{namespace}/{}", relative.display());
        if relative.components().any(|c| {
            let name = c.as_os_str().to_string_lossy();
            redact::excluded(&name) && !(domain == "graphs" && name == "workspaces")
        }) {
            self.issue(&virtual_path, "credential or private runtime file skipped");
            return Ok(());
        }
        let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            self.issue(&virtual_path, "non-regular file skipped");
            return Ok(());
        }
        if metadata.len() > MAX_FILE {
            self.issue(&virtual_path, "file exceeds 8 MiB limit");
            return Ok(());
        }
        let original = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => {
                self.issue(&virtual_path, "unreadable or non-UTF8 file");
                return Ok(());
            }
        };
        if original.contains('\0') {
            self.issue(&virtual_path, "binary file skipped");
            return Ok(());
        }
        let projection = if path.extension().is_some_and(|e| e == "json") {
            match serde_json::from_str::<Value>(&original) {
                Ok(mut value) => {
                    redact::json(&mut value);
                    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
                }
                Err(_) => redact::text(&original),
            }
        } else {
            redact::text(&original)
        };
        self.add(
            &virtual_path,
            domain,
            &path.display().to_string(),
            &original,
            &projection,
        )
    }

    fn run(&mut self, path: &Path, root: &Path, graph: &str) -> Result<(), String> {
        if let Err(reason) = safe_path(path) {
            self.issue("runs", &reason);
            return Ok(());
        }
        let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            self.issue("runs", "non-regular Run record skipped");
            return Ok(());
        }
        if metadata.len() > MAX_FILE {
            self.issue("runs", "run JSON exceeds 8 MiB; metadata unavailable");
            return Ok(());
        }
        let original = match fs::read_to_string(path) {
            Ok(value) => value,
            Err(_) => {
                self.issue("runs", "run metadata is unreadable");
                return Ok(());
            }
        };
        let raw: Value = match serde_json::from_str(&original) {
            Ok(value) => value,
            Err(_) => {
                self.issue("runs", "run JSON is invalid");
                return Ok(());
            }
        };
        let mut projection = json!({"graph":graph,"source_locator":path.display().to_string(),"run":raw.get("run_id").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(||path.parent().and_then(|p|p.file_name()).unwrap_or_default().to_string_lossy().to_string())});
        for key in [
            "status",
            "started",
            "updated",
            "created_at",
            "updated_at",
            "graph_digest",
            "sequence",
        ] {
            if let Some(value) = raw.get(key)
                && (value.is_string() || value.is_number() || value.is_null())
            {
                projection[key] = value.clone();
            }
        }
        // Presence is mechanical metadata. Omitted bodies cannot establish
        // that their fields are absent from the original Run record.
        let omitted = [
            "reason",
            "error",
            "exit_status",
            "exit_code",
            "status_reason",
            "failure_class",
            "input",
            "objective",
            "cursor",
            "nodes",
            "runs",
            "results",
            "messages",
            "history",
            "trace",
            "checkpoint",
        ];
        let presence = omitted
            .iter()
            .chain(["graph_digest", "graph_revision", "code_version", "commit"].iter())
            .map(|key| ((*key).to_owned(), json!(raw.get(*key).is_some())))
            .collect::<serde_json::Map<String, Value>>();
        projection["source_field_presence"] = Value::Object(presence);
        projection["projection_omitted"] = json!(
            omitted
                .iter()
                .filter(|key| raw.get(**key).is_some())
                .collect::<Vec<_>>()
        );
        let timestamp = ["updated", "updated_at", "started", "created_at"]
            .iter()
            .find_map(|key| {
                raw.get(key)
                    .and_then(Value::as_str)
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            });
        projection["within_last_seven_days"] = timestamp.map_or(Value::Null, |time| {
            json!(time >= self.captured_at - Duration::days(7) && time <= self.captured_at)
        });
        projection["time_basis"] = if timestamp.is_some() {
            json!("declared timestamp")
        } else {
            json!("timestamp unavailable; not inferred from file mtime")
        };
        redact::json(&mut projection);
        let content = serde_json::to_string_pretty(&projection).map_err(|e| e.to_string())?;
        // Original hashes bind the private record without retaining its body.
        let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
        let virtual_path = format!(
            "runs/{:x}.json",
            Sha256::digest(format!("{graph}:{}", relative.display()).as_bytes())
        );
        self.add(
            &virtual_path,
            "runs",
            &path.display().to_string(),
            &original,
            &content,
        )
    }

    fn add(
        &mut self,
        path: &str,
        domain: &str,
        source: &str,
        original: &str,
        content: &str,
    ) -> Result<(), String> {
        if self.entries.contains_key(path) {
            return Err(format!("duplicate frozen evidence identity {path}"));
        }
        let frozen_file = format!("files/{:x}.txt", Sha256::digest(path.as_bytes()));
        fs::write(self.root.join(&frozen_file), content).map_err(|e| e.to_string())?;
        self.entries.insert(
            path.into(),
            Entry {
                path: path.into(),
                domain: domain.into(),
                source: source.into(),
                frozen_file,
                sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
                original_sha256: format!("{:x}", Sha256::digest(original.as_bytes())),
                lines: content.lines().count(),
                redacted: content != original,
                content: content.into(),
            },
        );
        Ok(())
    }

    pub fn index(&self, domain: &str, offset: usize, limit: usize) -> Result<Value, String> {
        if !DOMAINS.contains(&domain) {
            return Err("unknown evidence domain".into());
        }
        let mut files = self
            .entries
            .values()
            .filter(|entry| entry.domain == domain)
            .cloned()
            .collect::<Vec<_>>();
        files.extend(
            self.supplementary
                .lock()
                .map_err(|_| "evidence lock failed")?
                .values()
                .filter(|entry| entry.domain == domain)
                .cloned(),
        );
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let limit = limit.clamp(1, 100);
        Ok(
            json!({"domain":domain,"count":files.len(),"offset":offset,"limit":limit,"files":files.iter().skip(offset).take(limit).collect::<Vec<_>>(),"next_offset":offset.checked_add(limit).filter(|next|*next<files.len()),"issue_count":self.issues.len(),"issues":self.issues.iter().take(20).collect::<Vec<_>>(),"issue_locator":"index.json in operator evidence root contains all collection issues","direct_dependency_count":self.dependencies.len(),"limitations":LIMITATIONS,"captured_at":self.captured_at}),
        )
    }

    pub fn read(&self, path: &str, offset: usize, limit: usize) -> Result<Value, String> {
        if path.starts_with('/')
            || path.contains('\\')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == ".." || part == ".")
        {
            return Err("read accepts only an indexed relative evidence path".into());
        }
        let entry = match self.entries.get(path) {
            Some(entry) => entry.clone(),
            None => self
                .supplementary
                .lock()
                .map_err(|_| "evidence lock failed")?
                .get(path)
                .cloned()
                .ok_or("path is not part of frozen evidence")?,
        };
        let lines = entry.content.lines().collect::<Vec<_>>();
        let limit = limit.clamp(1, 200);
        let mut page = Vec::new();
        let mut bytes = 0usize;
        for (i, text) in lines.iter().enumerate().skip(offset).take(limit) {
            if text.len() > MAX_READ {
                return Err("single evidence line exceeds 64 KiB read limit; inspect the recorded source through an authorized operator".into());
            }
            if bytes.saturating_add(text.len()) > MAX_READ {
                break;
            }
            bytes += text.len();
            page.push(json!({"line":i+1,"text":text}));
        }
        let next = offset.saturating_add(page.len());
        Ok(
            json!({"path":path,"source":entry.source,"frozen_file":entry.frozen_file,"sha256":entry.sha256,"redacted":entry.redacted,"line_basis":"frozen projection, not original source","total_lines":lines.len(),"offset":offset,"lines":page,"max_content_bytes":MAX_READ,"truncated":next<lines.len(),"next_offset":(next<lines.len()).then_some(next)}),
        )
    }

    pub fn audit(&self, tool: &str, arguments: &Value, success: bool) -> Result<(), String> {
        let mut arguments = arguments.clone();
        redact::json(&mut arguments);
        let record = json!({"time":Utc::now(),"tool":tool,"arguments":arguments,"success":success});
        let mut file = self.audit.lock().map_err(|_| "audit lock failed")?;
        serde_json::to_writer(&mut *file, &record).map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())?;
        file.sync_data().map_err(|e| e.to_string())
    }

    pub fn store_ecosystem(&self, value: &Value) -> Result<String, String> {
        let mut value = value.clone();
        redact::json(&mut value);
        let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let path = format!("dependencies/ecosystem-{digest}.json");
        let frozen_file = format!("files/{digest}.txt");
        let mut supplementary = self
            .supplementary
            .lock()
            .map_err(|_| "evidence lock failed")?;
        if !supplementary.contains_key(&path) {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(self.root.join(&frozen_file))
                .map_err(|e| e.to_string())?;
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            let content = String::from_utf8(bytes).map_err(|e| e.to_string())?;
            let entry = Entry {
                path: path.clone(),
                domain: "dependencies".into(),
                source: "public endpoints and retrieved_at recorded in projection".into(),
                frozen_file,
                sha256: digest.clone(),
                original_sha256: digest.clone(),
                lines: content.lines().count(),
                redacted: false,
                content,
            };
            fs::write(
                self.root.join(format!("ecosystem-{digest}.index.json")),
                serde_json::to_vec_pretty(&entry).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            supplementary.insert(path.clone(), entry);
        }
        Ok(path)
    }
}

fn sorted_dirs(root: &Path) -> Result<Vec<PathBuf>, String> {
    safe_path(root)?;
    let mut paths = fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}
