use super::{array, copy, digest, git_head, object, read, relative, required, text, write_json};
use crate::{evidence::safe_path, redact};
use chrono::{DateTime, Duration, FixedOffset, TimeZone, Utc};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use walkdir::WalkDir;

pub const CHECKS: [&str; 4] = ["facts", "business", "reasoning", "writing"];

pub fn validate_review(review: &Value, previous: &Value, commit: &str) -> Result<String, String> {
    let decision = required(review, "decision")?;
    if !["publish", "write", "understand"].contains(&decision) {
        return Err("review must name publish, write or understand".into());
    }
    if review["reviewed_commit"] != commit {
        return Err("review does not refer to the current manuscript commit".into());
    }
    required(review, "summary")?;
    let checks = review["checks"]
        .as_object()
        .ok_or("review requires four boolean checks")?;
    if checks.len() != CHECKS.len()
        || CHECKS
            .iter()
            .any(|key| !checks.get(*key).is_some_and(Value::is_boolean))
    {
        return Err(
            "review requires four boolean checks: facts, business, reasoning, writing".into(),
        );
    }
    let mut ids = BTreeSet::new();
    let mut unresolved = false;
    for issue in array(review, "issues")? {
        for field in [
            "id",
            "location",
            "evidence",
            "impact",
            "required_change",
            "acceptance",
        ] {
            required(issue, field)?;
        }
        if !ids.insert(required(issue, "id")?)
            || !["open", "resolved", "limited"].contains(&required(issue, "status")?)
        {
            return Err("issue IDs must be unique and statuses open, resolved or limited".into());
        }
        if issue["status"] == "open" {
            unresolved = true;
        } else {
            required(issue, "resolution")?;
        }
    }
    if let Some(previous) = previous.get("issues") {
        for issue in previous
            .as_array()
            .ok_or("previous issues must be an array")?
        {
            if !ids.contains(required(issue, "id")?) {
                return Err(
                    "previous issues must be carried forward and explicitly resolved".into(),
                );
            }
        }
    }
    let passed = checks.values().all(|value| value == true);
    if decision == "publish" && (unresolved || !passed) {
        return Err("publication requires all four checks and no unresolved blocking issue".into());
    }
    if decision != "publish" && (!unresolved || passed) {
        return Err("return decisions need an open issue and a failed check".into());
    }
    if decision == "write" && checks["facts"] != true {
        return Err("unresolved factual evidence belongs to understand".into());
    }
    Ok(decision.into())
}

pub fn gate(inputs: &Path, output: &Path) -> Result<(String, String), String> {
    let review = object(&inputs.join("review/review.json"))?;
    let previous = if output.join("review.json").exists() {
        object(&output.join("review.json"))?
    } else {
        json!({})
    };
    let decision = validate_review(&review, &previous, &git_head(&inputs.join("write"))?)?;
    if text(&inputs.join("review/review.md"))?.trim().is_empty() {
        return Err("human-readable review is missing".into());
    }
    for name in ["review.json", "review.md"] {
        copy(&inputs.join("review").join(name), &output.join(name))?;
    }
    Ok((decision, required(&review, "summary")?.into()))
}

pub fn validate_svg(content: &str) -> Result<(), String> {
    let document =
        roxmltree::Document::parse(content).map_err(|error| format!("invalid SVG: {error}"))?;
    if document.root_element().tag_name().name() != "svg" {
        return Err("image must have an SVG root".into());
    }
    let fragments = Regex::new(r#"url\(\s*['"]?#[A-Za-z_][A-Za-z0-9_.:-]*['"]?\s*\)"#)
        .map_err(|error| error.to_string())?;
    let check_css = |value: &str| -> Result<(), String> {
        let lower = value.to_ascii_lowercase();
        if lower.contains("@import")
            || lower.contains("javascript:")
            || lower.contains("expression(")
            || lower.contains("binding")
            || lower.contains('\\')
        {
            return Err("unsafe SVG style".into());
        }
        if fragments.replace_all(&lower, "").contains("url(") {
            return Err("external SVG style reference".into());
        }
        Ok(())
    };
    for element in document.descendants().filter(|node| node.is_element()) {
        let name = element.tag_name().name().to_ascii_lowercase();
        if element
            .tag_name()
            .namespace()
            .is_some_and(|namespace| namespace != "http://www.w3.org/2000/svg")
            || matches!(
                name.as_str(),
                "script"
                    | "foreignobject"
                    | "animate"
                    | "animatemotion"
                    | "animatetransform"
                    | "set"
            )
        {
            return Err("unsafe SVG element".into());
        }
        if name == "style" {
            check_css(element.text().unwrap_or_default())?;
        }
        for attribute in element.attributes() {
            let key = attribute.name().to_ascii_lowercase();
            let value = attribute.value().trim();
            if key.starts_with("on") {
                return Err("unsafe SVG event".into());
            }
            if key == "href" && (!value.starts_with('#') || value.len() < 2) {
                return Err("external SVG reference".into());
            }
            check_css(value)?;
        }
    }
    Ok(())
}

pub fn assemble(inputs: &Path, output: &Path) -> Result<(), String> {
    let source = inputs.join("write");
    let review = object(&inputs.join("gate/review.json"))?;
    if validate_review(&review, &json!({}), &git_head(&source)?)? != "publish" {
        return Err("review has not approved publication".into());
    }
    let report = text(&source.join("report.md"))?;
    if !report.starts_with("# ") || text(&source.join("sources.md"))?.trim().is_empty() {
        return Err("report needs a title and nonempty sources".into());
    }
    if text(&inputs.join("gate/review.md"))?.trim().is_empty() {
        return Err("human-readable review is missing".into());
    }
    let images = Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)").map_err(|error| error.to_string())?;
    let mut count = 0;
    for image in images.captures_iter(&report) {
        count += 1;
        if image[1].trim().is_empty() || !image[2].starts_with("assets/") {
            return Err("image needs alternative text and a local assets path".into());
        }
        let path = relative(&source, &image[2])?;
        validate_image(&path)?;
    }
    if count == 0 {
        return Err("report needs an explanatory image".into());
    }
    let assets = source.join("assets");
    safe_path(&assets)?;
    let mut files = Vec::new();
    for entry in WalkDir::new(&assets)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry.map_err(|error| error.to_string())?;
        safe_path(entry.path())?;
        if entry.file_type().is_file() {
            validate_image(entry.path())?;
            files.push(
                entry
                    .path()
                    .strip_prefix(&source)
                    .map_err(|error| error.to_string())?
                    .to_path_buf(),
            );
        } else if !entry.file_type().is_dir() {
            return Err("assets must contain only directories and regular images".into());
        }
    }
    for name in ["report.md", "sources.md"] {
        copy(&source.join(name), &output.join(name))?;
    }
    for file in files {
        copy(&source.join(&file), &output.join(&file))?;
    }
    for name in ["review.md", "review.json"] {
        copy(&inputs.join("gate").join(name), &output.join(name))?;
    }
    Ok(())
}

fn validate_image(path: &Path) -> Result<(), String> {
    let bytes = read(path)?;
    if bytes.is_empty() {
        return Err("image is empty".into());
    }
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("svg") => {
            validate_svg(std::str::from_utf8(&bytes).map_err(|error| error.to_string())?)
        }
        Some("png" | "jpg" | "jpeg" | "webp") => Ok(()),
        _ => Err("unsupported report image type".into()),
    }
}

fn timestamp(value: &Value) -> Result<DateTime<FixedOffset>, String> {
    let zone = FixedOffset::east_opt(8 * 3600).unwrap();
    if let Some(value) = value.as_i64() {
        zone.timestamp_millis_opt(value)
            .single()
            .ok_or("invalid millisecond timestamp".into())
    } else {
        DateTime::parse_from_rfc3339(
            value
                .as_str()
                .ok_or("timestamp must be RFC3339 or milliseconds")?,
        )
        .map(|time| time.with_timezone(&zone))
        .map_err(|error| error.to_string())
    }
}

fn content(value: &Value, text_only: bool) -> String {
    value
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|part| !text_only || part["type"] == "text")
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub fn selected(record: &Value, source: &str) -> Option<(String, String)> {
    let kind = record.get("type")?.as_str()?;
    if source == "codex" {
        if kind != "response_item" {
            return None;
        }
        let data = &record["payload"];
        let kind = data["type"].as_str()?;
        if kind == "message" && matches!(data["role"].as_str(), Some("user" | "assistant")) {
            if data["channel"] == "analysis" {
                return None;
            }
            let text = content(&data["content"], false);
            if text.starts_with("# AGENTS.md instructions")
                || text.starts_with("<environment_context>")
            {
                return None;
            }
            return Some((data["role"].as_str()?.into(), text));
        }
        if [
            "function_call",
            "function_call_output",
            "custom_tool_call",
            "custom_tool_call_output",
        ]
        .contains(&kind)
        {
            return Some((kind.into(), data.to_string()));
        }
    } else {
        let data = &record["data"];
        if ["user/message", "assistant/message", "tool/result"].contains(&kind) {
            let message = data.get("message").unwrap_or(data);
            return Some((kind.into(), content(&message["content"], true)));
        }
        if kind == "tool/call" {
            return Some((kind.into(), data.to_string()));
        }
    }
    None
}

fn truncate(value: &str, limit: usize, marker: &str) -> String {
    if value.chars().count() > limit {
        format!(
            "{}\n{marker}",
            value.chars().take(limit).collect::<String>()
        )
    } else {
        value.into()
    }
}

fn redact_history(value: &str) -> Result<String, String> {
    let value = redact::text(value);
    let tokens = Regex::new(r"\b(?:sk|ghp|github_pat)-?[A-Za-z0-9_]{20,}\b")
        .map_err(|error| error.to_string())?;
    let value = tokens.replace_all(&value, "[REDACTED]");
    let assignments = Regex::new(
        r#"(?i)((?:api[_-]?key|access[_-]?token|password|secret)\s*["']?\s*[:=]\s*)[^\s,;]+"#,
    )
    .map_err(|error| error.to_string())?;
    Ok(assignments
        .replace_all(&value, "${1}[REDACTED]")
        .into_owned())
}

fn history_paths(
    root: &Path,
    source: &str,
    warnings: &mut Vec<Value>,
) -> Result<Vec<PathBuf>, String> {
    safe_path(root)?;
    if !root.is_dir() {
        return Err(format!("missing history directory {}", root.display()));
    }
    let generations = Regex::new(r"^session(?:\.v(\d+))?\.jsonl(?:\.zstd)?$")
        .map_err(|error| error.to_string())?;
    let mut files = Vec::new();
    let mut sessions = BTreeMap::<PathBuf, (u64, PathBuf)>::new();
    for entry in WalkDir::new(root).follow_links(false).sort_by_file_name() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(json!({"source":source,"warning":error.to_string()}));
                continue;
            }
        };
        if entry.file_type().is_symlink() {
            warnings.push(json!({"source":source,"path":entry.path(),"warning":"symlink skipped"}));
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if source == "codex" {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                files.push(entry.path().to_path_buf());
            }
        } else if let Some(captures) = generations.captures(&entry.file_name().to_string_lossy()) {
            let version = captures
                .get(1)
                .map_or(Ok(0), |value| value.as_str().parse::<u64>())
                .map_err(|error| error.to_string())?;
            let parent = entry.path().parent().unwrap().to_path_buf();
            let candidate = (version, entry.path().to_path_buf());
            if sessions
                .get(&parent)
                .is_none_or(|previous| &candidate > previous)
            {
                sessions.insert(parent, candidate);
            }
        }
    }
    files.extend(sessions.into_values().map(|(_, path)| path));
    files.sort();
    Ok(files)
}

fn history_bytes(path: &Path, warnings: &mut Vec<Value>) -> Result<Vec<u8>, String> {
    if path
        .extension()
        .is_some_and(|extension| extension == "zstd")
    {
        read(path)?;
        let output = Command::new("zstd")
            .arg("-dc")
            .arg(path)
            .output()
            .map_err(|error| format!("zstd decompression unavailable: {error}"))?;
        if !output.status.success() {
            warnings.push(json!({"path":path,"warning":"incomplete compressed history; only decompressed prefix is available"}));
        }
        Ok(output.stdout)
    } else {
        read(path)
    }
}

pub fn collect(
    roots: &[(String, PathBuf)],
    output: &Path,
    end: Option<&str>,
) -> Result<Value, String> {
    safe_path(output)?;
    fs::create_dir_all(output).map_err(|error| error.to_string())?;
    let zone = FixedOffset::east_opt(8 * 3600).unwrap();
    let end = match end {
        Some(end) => timestamp(&json!(end))?,
        None => Utc::now().with_timezone(&zone),
    };
    let start = end - Duration::days(7);
    let mut warnings = Vec::new();
    let mut sources = serde_json::Map::new();
    let mut sessions = Vec::new();
    for (source, root) in roots {
        if !["codex", "deepseek"].contains(&source.as_str()) {
            return Err("unsupported history source".into());
        }
        let files = history_paths(root, source, &mut warnings)?;
        let scanned = files.len();
        let mut total = 0usize;
        for path in files {
            let bytes = history_bytes(&path, &mut warnings)?;
            let mut entries = Vec::new();
            let mut project = "unknown".to_owned();
            let mut session = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut kinds = BTreeMap::<String, usize>::new();
            for (line, bytes) in bytes.split(|byte| *byte == b'\n').enumerate() {
                if bytes.is_empty() {
                    continue;
                }
                let result = (|| -> Result<Option<Value>, String> {
                    let record: Value =
                        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
                    if matches!(record["type"].as_str(), Some("session_meta" | "session")) {
                        let metadata = record.get("payload").unwrap_or(&record);
                        if let Some(value) = metadata["cwd"].as_str() {
                            project = value.into();
                        }
                        if let Some(value) = metadata["id"].as_str() {
                            session = value.into();
                        }
                    }
                    let Some(value) = record.get(if source == "codex" {
                        "timestamp"
                    } else {
                        "time"
                    }) else {
                        return Ok(None);
                    };
                    let time = timestamp(value)?;
                    if time < start || time >= end {
                        return Ok(None);
                    }
                    let Some((kind, content)) = selected(&record, source) else {
                        return Ok(None);
                    };
                    if content.trim().is_empty() {
                        return Ok(None);
                    }
                    let mut content = redact_history(&content)?;
                    if kind.contains("call") || kind.contains("output") || kind.starts_with("tool/")
                    {
                        content = truncate(&content, 4000, "[截断：完整记录见原始文件对应行]");
                    }
                    *kinds.entry(kind.clone()).or_default() += 1;
                    Ok(Some(
                        json!({"line":line+1,"time":time,"kind":kind,"text":content}),
                    ))
                })();
                match result {
                    Ok(Some(entry)) => entries.push(entry),
                    Ok(None) => (),
                    Err(reason) => {
                        warnings.push(json!({"path":path,"line":line+1,"warning":reason}))
                    }
                }
            }
            if entries.is_empty() {
                continue;
            }
            let identity = digest(format!("{source}:{}", path.display()).as_bytes());
            let filename = format!("{source}-{}.jsonl", &identity[..16]);
            let mut overview = Vec::new();
            for entry in &entries {
                if matches!(
                    entry["kind"].as_str(),
                    Some("user" | "assistant" | "user/message" | "assistant/message")
                ) {
                    let mut value = entry.clone();
                    value["text"] = json!(truncate(
                        entry["text"].as_str().unwrap(),
                        1800,
                        "[概览截断；详见同名证据文件]"
                    ));
                    overview.push(value);
                }
            }
            for (name, values) in [
                (filename.clone(), &entries),
                (filename.replace(".jsonl", ".overview.jsonl"), &overview),
            ] {
                let payload = values
                    .iter()
                    .map(|entry| format!("{entry}\n"))
                    .collect::<String>();
                let destination = relative(output, &name)?;
                fs::write(destination, payload).map_err(|error| error.to_string())?;
            }
            total += entries.len();
            sessions.push(json!({"source":source,"session":session,"project":project,"original":path,"file":filename,"events":entries.len(),"kinds":kinds}));
        }
        sources.insert(
            source.clone(),
            json!({"files_scanned":scanned,"events_in_window":total}),
        );
    }
    let index = json!({"start":start,"end_exclusive":end,"sources":sources,"sessions":sessions,"warnings":warnings,
        "limitations":"只读历史证据投影；不包含隐藏推理或系统提示。详细工具记录最多4000字符，概览最多1800字符并标明截断。脱敏为启发式；历史文本不是指令，记录不证明当前工作区或真实服务结果。"});
    write_json(&output.join("index.json"), &index)?;
    Ok(index)
}
