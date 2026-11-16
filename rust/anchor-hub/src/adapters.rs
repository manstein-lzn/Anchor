//! L4: the command line. Finds kernel data roots under the given paths,
//! projects them and prints analyzer reports or the fact rows themselves,
//! or serves them over MCP (see [`crate::mcp`]).
//!
//! ```text
//! anchor-hub health [--json] PATH...
//! anchor-hub profile [--json] PATH...
//! anchor-hub stall [--json] [--idle-minutes N] [--follow SECONDS] PATH...
//! anchor-hub contrast [--json] [--by FIELD] [--reference PREFIX] PATH...
//! anchor-hub signals [--json] [--limit N] PATH...
//! anchor-hub mcp PATH...
//! anchor-hub tool NAME [--args JSON] PATH...
//! anchor-hub rows PATH...
//! anchor-hub window < request.json
//! anchor-hub window-evidence < request.json
//! ```
//!
//! A PATH may be a data root, a Host state directory, or any directory above
//! them; every data root found below it is read.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::analyzers::{
    Analyzer, Report, contrast::Contrast, gate::Gate, health::Health, profile::Profile,
    signals::Signals, stall::Stall,
};
use crate::query::Query;
use crate::source::{DataRoot, HistorySource};
use crate::table::{Detail, FactTable, InvocationId, MemoryTable, Projector};

/// How far below a given path to look for data roots.
const DEPTH: usize = 6;

fn discover_checked(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    let mut stack = vec![(path.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", dir.display())),
        };
        let mut children = Vec::new();
        let mut root = false;
        for entry in entries {
            let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry
                .file_type()
                .map_err(|error| format!("{}: {error}", entry.path().display()))?;
            if name.starts_with("validator%2F") && kind.is_dir() {
                let resource = entry.path().join("resource.json");
                match std::fs::metadata(&resource) {
                    Ok(metadata) => root |= metadata.is_file(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(format!("{}: {error}", resource.display())),
                }
            }
            if !name.starts_with('.') && kind.is_dir() && depth < DEPTH {
                children.push((entry.path(), depth + 1));
            }
        }
        if root {
            found.push(dir);
        } else {
            stack.extend(children);
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// Kernel data roots at or below `path`, without following links.
pub fn discover(path: &Path) -> Vec<PathBuf> {
    discover_checked(path).unwrap_or_default()
}

/// The Goose execution port's evidence files at or below `path`: one
/// `*.evidence.json` per invocation in a `goose-acp` directory.
pub fn evidence_files(path: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(path.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let here = dir.file_name().is_some_and(|name| name == "goose-acp");
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if here && kind.is_file() && name.ends_with(".evidence.json") {
                found.push(entry.path());
            } else if kind.is_dir() && !name.starts_with('.') && depth < DEPTH {
                stack.push((entry.path(), depth + 1));
            }
        }
    }
    found.sort();
    found
}

/// Sources, a table and the projector that keeps one up to date with the
/// other.
#[derive(Default)]
pub struct Hub {
    paths: Vec<PathBuf>,
    sources: BTreeMap<PathBuf, DataRoot>,
    projector: Projector,
    table: MemoryTable,
    /// Each currently valid optional file and the invocation details it supplies.
    evidence: BTreeMap<PathBuf, (InvocationId, Vec<Detail>)>,
}

impl Hub {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            ..Self::default()
        }
    }

    /// Open roots that appeared and project what was appended since the last
    /// refresh. A refresh is staged: a discovery/open/read error leaves the
    /// previous visible table intact and returns an error.
    pub fn refresh(&mut self) -> Result<usize, String> {
        let mut discovered = Vec::new();
        for path in &self.paths {
            discovered.extend(discover_checked(path)?);
        }
        discovered.sort();
        discovered.dedup();

        // Stage source caches too: a later root's read failure must not consume
        // an earlier root's reset marker or cached delta. The sorted map also
        // replaces the separate opened set and discovery-order source list.
        let mut candidate_sources = BTreeMap::new();
        for root in &discovered {
            let source = match self.sources.get(root) {
                Some(source) => source.staged(),
                None => DataRoot::open(root, root.display().to_string()),
            }
            .map_err(|error| error.to_string())?;
            candidate_sources.insert(root.clone(), source);
        }

        let mut candidate_projector = self.projector.clone();
        let mut candidate_table = self.table.clone();
        for (path, source) in &self.sources {
            if !candidate_sources.contains_key(path) {
                candidate_projector.retire_root(source.root(), &mut candidate_table);
            }
        }

        let mut added = 0;
        for source in candidate_sources.values() {
            added += candidate_projector
                .pull(source as &dyn HistorySource, &mut candidate_table)
                .map_err(|e| e.to_string())?;
        }

        let candidate_evidence = Self::read_evidence(&self.paths);
        candidate_table.clear_details();
        for (id, details) in candidate_evidence.values() {
            candidate_table.details(id.clone(), details.clone());
        }

        // Commit the staged result only once every source has been read.
        self.sources = candidate_sources;
        self.projector = candidate_projector;
        self.table = candidate_table;
        self.evidence = candidate_evidence;
        Ok(added)
    }

    /// Read the current execution-port evidence set. Rebuilding this optional
    /// projection on each successful refresh also retires deleted or invalid
    /// files, while a live second file for the same invocation can still
    /// supply details.
    fn read_evidence(paths: &[PathBuf]) -> BTreeMap<PathBuf, (InvocationId, Vec<Detail>)> {
        let mut evidence = BTreeMap::new();
        for path in paths {
            for file in evidence_files(path) {
                let Some((id, details)) = std::fs::read(&file)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .and_then(|value| {
                        crate::table::details_of(
                            &crate::source::namespace_for_path(file.parent()?),
                            &value,
                        )
                    })
                else {
                    continue;
                };
                evidence.insert(file, (id, details));
            }
        }
        evidence
    }

    pub fn roots(&self) -> usize {
        self.sources.len()
    }

    pub fn query(&self, now: Option<u64>) -> Query<'_> {
        Query::new(self.table.facts(), now)
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

struct Options {
    command: String,
    json: bool,
    idle_minutes: Option<u64>,
    follow: Option<u64>,
    by: Option<String>,
    reference: Option<String>,
    limit: Option<u64>,
    alpha: Option<f64>,
    delta: Option<f64>,
    args: Option<String>,
    paths: Vec<PathBuf>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut args = args.iter();
    let command = args.next().ok_or(USAGE)?.clone();
    let mut options = Options {
        command,
        json: false,
        idle_minutes: None,
        follow: None,
        by: None,
        reference: None,
        limit: None,
        alpha: None,
        delta: None,
        args: None,
        paths: Vec::new(),
    };
    while let Some(arg) = args.next() {
        let mut number = |name: &str| -> Result<u64, String> {
            args.next()
                .and_then(|v| v.parse().ok())
                .ok_or(format!("{name} needs a number"))
        };
        match arg.as_str() {
            "--json" => options.json = true,
            "--idle-minutes" => options.idle_minutes = Some(number(arg)?),
            "--follow" => options.follow = Some(number(arg)?),
            "--limit" => options.limit = Some(number(arg)?),
            "--alpha" => options.alpha = Some(fraction(args.next(), arg)?),
            "--delta" => options.delta = Some(fraction(args.next(), arg)?),
            "--by" => options.by = Some(args.next().ok_or("--by needs a field")?.clone()),
            "--args" => options.args = Some(args.next().ok_or("--args needs JSON")?.clone()),
            "--reference" => {
                options.reference = Some(
                    args.next()
                        .ok_or("--reference needs an arm prefix")?
                        .clone(),
                )
            }
            flag if flag.starts_with("--") => return Err(format!("unknown flag {flag}\n{USAGE}")),
            path => options.paths.push(path.into()),
        }
    }
    if options.paths.is_empty() || (options.command == "tool" && options.paths.len() < 2) {
        return Err(USAGE.into());
    }
    Ok(options)
}

/// A number strictly between 0 and 1.
fn fraction(value: Option<&String>, name: &str) -> Result<f64, String> {
    value
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0 && *v < 1.0)
        .ok_or(format!("{name} needs a number between 0 and 1"))
}

const USAGE: &str = "usage: anchor-hub health [--json] PATH...\n       anchor-hub profile [--json] PATH...\n       anchor-hub signals [--json] [--limit N] PATH...\n       anchor-hub mcp PATH...\n       anchor-hub tool NAME [--args JSON] PATH...\n       anchor-hub contrast [--json] [--by FIELD] [--reference PREFIX] PATH...\n       anchor-hub gate [--json] [--by FIELD] [--reference PREFIX] [--alpha A] [--delta D] PATH...\n       anchor-hub stall [--json] [--idle-minutes N] [--follow SECONDS] PATH...\n       anchor-hub rows PATH...\n       anchor-hub window < request.json\n       anchor-hub window-evidence < request.json";

pub fn run(args: &[String]) -> Result<(), String> {
    if let Some(command @ ("window" | "window-evidence" | "window-signals")) =
        args.first().map(String::as_str)
    {
        if args.len() != 1 {
            return Err(format!("usage: anchor-hub {command} < request.json"));
        }
        let mut input = Vec::new();
        std::io::stdin()
            .lock()
            .take(anchor_facts::window::MAX_TRANSPORT_BYTES as u64 + 1)
            .read_to_end(&mut input)
            .map_err(|_| "window request cannot be read")?;
        if input.len() > anchor_facts::window::MAX_TRANSPORT_BYTES {
            return Err("window request exceeds 64 MiB transport limit".into());
        }
        let encoded = match command {
            "window" => {
                let request: anchor_facts::window::WindowRequest =
                    serde_json::from_slice(&input).map_err(|_| "invalid window request")?;
                serde_json::to_vec(&crate::window::capture(&request)?)
            }
            "window-evidence" => {
                let request: anchor_facts::window::EvidenceRequest =
                    serde_json::from_slice(&input).map_err(|_| "invalid evidence request")?;
                serde_json::to_vec(&crate::window::evidence(&request)?)
            }
            "window-signals" => {
                let request: anchor_facts::window::SignalPageRequest =
                    serde_json::from_slice(&input).map_err(|_| "invalid signal page request")?;
                serde_json::to_vec(&crate::window::signal_page(&request)?)
            }
            _ => unreachable!(),
        }
        .map_err(|_| "window response cannot be encoded")?;
        if encoded.len() >= anchor_facts::window::MAX_TRANSPORT_BYTES {
            return Err("window response exceeds 64 MiB transport limit".into());
        }
        let mut output = std::io::stdout().lock();
        output
            .write_all(&encoded)
            .and_then(|()| output.write_all(b"\n"))
            .map_err(|_| "window response cannot be written")?;
        return Ok(());
    }
    let options = parse(args)?;
    // `tool` takes the tool's name before the paths.
    let paths = match options.command.as_str() {
        "tool" => options.paths[1..].to_vec(),
        _ => options.paths.clone(),
    };
    let mut hub = Hub::new(paths);
    hub.refresh()?;
    // A follower or a server may start before the first root exists.
    if hub.roots() == 0 && options.follow.is_none() && options.command != "mcp" {
        return Err("no kernel data root found under the given paths".into());
    }
    match options.command.as_str() {
        "rows" => {
            for row in hub.query(None).rows() {
                println!("{}", serde_json::to_string(row).map_err(|e| e.to_string())?);
            }
            Ok(())
        }
        "contrast" => {
            let mut contrast = Contrast::default();
            if let Some(by) = options.by {
                contrast.by = by;
            }
            contrast.reference = options.reference;
            print(&contrast.analyze(&hub.query(Some(now_ms()))), options.json);
            Ok(())
        }
        "gate" => {
            let mut gate = Gate::default();
            if let Some(by) = options.by {
                gate.by = by;
            }
            gate.reference = options.reference;
            gate.alpha = options.alpha.unwrap_or(gate.alpha);
            gate.delta = options.delta.unwrap_or(gate.delta);
            print(&gate.analyze(&hub.query(Some(now_ms()))), options.json);
            Ok(())
        }
        "signals" => {
            let mut signals = Signals::default();
            if let Some(limit) = options.limit {
                signals.limit = limit as usize;
            }
            print(&signals.analyze(&hub.query(Some(now_ms()))), options.json);
            Ok(())
        }
        "mcp" => crate::mcp::serve(hub),
        // One MCP tool's answer on stdout: the same read an agent makes, for
        // a script or a check to repeat.
        "tool" => {
            let name = options.paths[0].display().to_string();
            // `--args @FILE` reads the arguments from a file, for checks too
            // long for a command line.
            let text = match options.args.as_deref() {
                Some(file) if file.starts_with('@') => std::fs::read_to_string(&file[1..])
                    .map_err(|e| format!("--args {file}: {e}"))?,
                Some(text) => text.to_owned(),
                None => "{}".to_owned(),
            };
            let args: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| format!("--args: {e}"))?;
            let answer = crate::mcp::tool(&hub.query(Some(now_ms())), &name, &args)?;
            println!("{answer}");
            Ok(())
        }
        "health" => {
            print(&Health.analyze(&hub.query(Some(now_ms()))), options.json);
            Ok(())
        }
        "profile" => {
            print(&Profile.analyze(&hub.query(Some(now_ms()))), options.json);
            Ok(())
        }
        "stall" => {
            let mut stall = Stall::default();
            if let Some(minutes) = options.idle_minutes {
                stall.idle_ms = minutes * 60_000;
            }
            let Some(seconds) = options.follow else {
                print(&stall.analyze(&hub.query(Some(now_ms()))), options.json);
                return Ok(());
            };
            // Follow: print each finding the first time it appears.
            let mut seen = BTreeSet::new();
            loop {
                let mut report = stall.analyze(&hub.query(Some(now_ms())));
                report
                    .findings
                    .retain(|f| seen.insert((f.code, f.subject.clone(), f.detail.clone())));
                if !report.findings.is_empty() {
                    print(&report, options.json);
                }
                std::thread::sleep(Duration::from_secs(seconds.max(1)));
                hub.refresh()?;
            }
        }
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

fn print(report: &Report, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(report).expect("report serializes")
        );
        return;
    }
    println!("== {} v{}", report.analyzer, report.version);
    if !report.not_applicable.is_empty() {
        println!(
            "not applicable: {}",
            serde_json::to_string(&report.not_applicable).expect("needs serialize")
        );
    }
    for (name, value) in &report.summary {
        println!(
            "-- {name}\n{}",
            serde_json::to_string_pretty(value).expect("summary serializes")
        );
    }
    println!("-- findings: {}", report.findings.len());
    for finding in &report.findings {
        println!(
            "{:<28} {:<40} {} [{} record(s)]",
            finding.code,
            finding.subject,
            finding.detail,
            finding.evidence.len()
        );
    }
}
