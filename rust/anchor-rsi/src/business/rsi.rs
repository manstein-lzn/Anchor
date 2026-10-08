use super::{
    array, copy, digest, git_head, object, read, relative, required, text, verify_commit,
    write_json,
};
use crate::evidence::{Entry, safe_path};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const DOMAINS: [&str; 5] = ["runs", "code", "graphs", "plugins", "dependencies"];
const CHECKS: [&str; 4] = ["facts", "research", "architecture", "writing"];
const UNSAFE: [&str; 5] = [
    "全量暴露",
    "整文件不脱敏",
    "关闭脱敏",
    "取消权限保护",
    "绕过权限",
];
const OVERSTRONG: [&str; 6] = [
    "只能来自未提交工作树或服务重启",
    "本次审计 run 是首次 fanout/join 执行",
    "本次审计run是首次fanout/join执行",
    "才是首次 fanout/join",
    "才是首次fanout/join",
    "零 production completed 样本",
];

pub struct FrozenEvidence {
    pub index: Value,
    entries: BTreeMap<String, (Entry, PathBuf)>,
    reads: BTreeSet<String>,
}

impl FrozenEvidence {
    pub fn load(root: &Path) -> Result<Self, String> {
        let index = object(&root.join("index.json"))?;
        let raw = index["entries"]
            .as_object()
            .ok_or("evidence index requires entries")?;
        let mut entries = BTreeMap::new();
        for (name, entry) in raw {
            let entry: Entry =
                serde_json::from_value(entry.clone()).map_err(|error| error.to_string())?;
            if entry.path != *name {
                return Err("evidence index path differs from entry".into());
            }
            entries.insert(name.clone(), (entry, root.into()));
        }
        for path in fs::read_dir(root).map_err(|error| error.to_string())? {
            let path = path.map_err(|error| error.to_string())?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with("ecosystem-") && name.ends_with(".index.json") {
                let entry: Entry =
                    serde_json::from_slice(&read(&path)?).map_err(|error| error.to_string())?;
                if !entry.path.starts_with("dependencies/ecosystem-") {
                    return Err("invalid ecosystem evidence namespace".into());
                }
                if entries
                    .insert(entry.path.clone(), (entry, root.into()))
                    .is_some()
                {
                    return Err("duplicate evidence entry".into());
                }
            }
        }
        let mut reads = BTreeSet::new();
        let audit = root.join("tool-calls.jsonl");
        if audit.exists() {
            for line in text(&audit)?.lines().filter(|line| !line.is_empty()) {
                let call: Value = serde_json::from_str(line)
                    .map_err(|error| format!("invalid evidence audit: {error}"))?;
                if call["success"] == true
                    && call["tool"] == "rsi_read"
                    && let Some(path) = call["arguments"]["path"].as_str()
                {
                    reads.insert(path.into());
                }
            }
        }
        Ok(Self {
            index,
            entries,
            reads,
        })
    }

    pub fn extend(&mut self, root: &Path) -> Result<(), String> {
        let additional = Self::load(root)?;
        for (name, entry) in additional.entries {
            if self.entries.insert(name, entry).is_some() {
                return Err("duplicate evidence identity across collections".into());
            }
        }
        self.reads.extend(additional.reads);
        Ok(())
    }

    pub fn validate_reference(&self, reference: &Value, require_read: bool) -> Result<(), String> {
        let path = required(reference, "path")?;
        required(reference, "locator")?;
        let (entry, root) = self
            .entries
            .get(path)
            .ok_or_else(|| format!("unindexed evidence {path:?}"))?;
        if digest(&read(&relative(root, &entry.frozen_file)?)?) != entry.sha256 {
            return Err(format!("frozen evidence changed: {path}"));
        }
        if reference["sha256"] != entry.sha256
            || reference["redacted"] != entry.redacted
            || reference["line_basis"] != "frozen projection, not original source"
            || reference["layer"] != layer(path)?
        {
            return Err(format!(
                "evidence identity/layer differs from frozen index: {path}"
            ));
        }
        if require_read && !self.reads.contains(path) {
            return Err(format!(
                "evidence was not successfully read through rsi_read: {path}"
            ));
        }
        Ok(())
    }

    fn previous_ids(&self) -> Result<BTreeSet<String>, String> {
        let mut ids = BTreeSet::new();
        for (entry, root) in self
            .entries
            .values()
            .filter(|(entry, _)| entry.domain == "previous")
        {
            let bytes = read(&relative(root, &entry.frozen_file)?)?;
            if digest(&bytes) != entry.sha256 {
                return Err("previous report evidence changed".into());
            }
            if entry.path.ends_with(".json") {
                let value: Value =
                    serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                collect_ids(&value, &mut ids);
            }
        }
        Ok(ids)
    }
}

fn layer(path: &str) -> Result<&'static str, String> {
    if path.starts_with("dependencies/ecosystem-") {
        Ok("external_metadata")
    } else if ["code/", "graphs/", "plugins/", "dependencies/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        Ok("frozen_source")
    } else if path.starts_with("runs/") {
        Ok("run_projection")
    } else if path.starts_with("previous/") {
        Ok("previous_report")
    } else {
        Err("unknown evidence namespace".into())
    }
}

fn references(
    values: &[Value],
    inputs: &Path,
    frozen: Option<&FrozenEvidence>,
    coverage: bool,
) -> Result<(), String> {
    if values.is_empty() {
        return Err("evidence references must be nonempty".into());
    }
    for reference in values {
        if let Some(frozen) = frozen {
            if let Some(path) = reference.as_str() {
                if !coverage {
                    return Err("finding references need a path and locator".into());
                }
                let (entry, root) = frozen
                    .entries
                    .get(path)
                    .ok_or("unindexed coverage evidence")?;
                if !frozen.reads.contains(path)
                    || digest(&read(&relative(root, &entry.frozen_file)?)?) != entry.sha256
                {
                    return Err("coverage evidence was not read or has changed".into());
                }
            } else if reference.get("sha256").is_some() {
                frozen.validate_reference(reference, true)?;
            } else {
                let path = required(reference, "path")?;
                required(reference, "locator")?;
                let (entry, root) = frozen
                    .entries
                    .get(path)
                    .ok_or("unindexed finding evidence")?;
                if !frozen.reads.contains(path)
                    || digest(&read(&relative(root, &entry.frozen_file)?)?) != entry.sha256
                {
                    return Err("finding evidence was not read or has changed".into());
                }
            }
        } else {
            let path = required(reference, "path")?
                .strip_prefix("/in/")
                .ok_or("evidence path must begin with /in/")?;
            required(reference, "locator")?;
            let path = relative(inputs, path)?;
            if coverage && path.is_dir() {
                continue;
            }
            read(&path)?;
        }
    }
    Ok(())
}

fn joined(inputs: &Path, manifest: &str, expected: usize) -> Result<Vec<(String, Value)>, String> {
    let value = object(&inputs.join(manifest).join("join.json"))?;
    let branches = array(&value, "branches")?;
    if branches.len() != expected {
        return Err(format!("{manifest} requires exactly {expected} branches"));
    }
    let mut names = BTreeSet::new();
    let mut commits = Vec::new();
    for branch in branches {
        let name = required(branch, "output")?;
        if name.contains('/') || !names.insert(name.to_owned()) {
            return Err("invalid or duplicate join branch".into());
        }
        let directory = relative(inputs, name)?;
        let records = array(branch, "nodes")?
            .iter()
            .filter(|item| item["node"] == name)
            .collect::<Vec<_>>();
        if records.len() != 1 {
            return Err("join must bind each branch output exactly once".into());
        }
        let commit = records[0]
            .get("commit")
            .ok_or("join branch has no commit")?;
        verify_commit(&directory, commit, name)?;
        commits.push((name.into(), commit.clone()));
    }
    Ok(commits)
}

fn validate_audits(inputs: &Path, frozen: Option<&FrozenEvidence>) -> Result<(), String> {
    let mut domains = BTreeSet::new();
    for (name, _) in joined(inputs, "audit-join", DOMAINS.len())? {
        let data = object(&inputs.join(&name).join("findings.json"))?;
        let domain = required(&data, "domain")?;
        if !DOMAINS.contains(&domain) || !domains.insert(domain.to_owned()) {
            return Err("unknown or duplicate specialist domain".into());
        }
        required(&data, "summary")?;
        let coverage = data.get("coverage").ok_or("missing audit coverage")?;
        references(array(coverage, "read")?, inputs, frozen, true)?;
        array(coverage, "not_reviewed")?;
        array(coverage, "limitations")?;
        let mut ids = BTreeSet::new();
        for finding in array(&data, "findings")? {
            let id = required(finding, "id")?;
            if !ids.insert(id)
                || !["observed", "inferred", "unknown"].contains(&required(finding, "kind")?)
                || !["high", "medium", "low"].contains(&required(
                    finding,
                    if frozen.is_some() {
                        "priority"
                    } else {
                        "confidence"
                    },
                )?)
            {
                return Err(format!(
                    "{name}/{id}: invalid finding identity/kind/confidence"
                ));
            }
            required(finding, "claim")?;
            references(array(finding, "evidence")?, inputs, frozen, false)?;
        }
    }
    Ok(())
}

fn collect_ids(value: &Value, ids: &mut BTreeSet<String>) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                if ["proposals", "carry_forward"].contains(&key.as_str())
                    && let Some(items) = child.as_array()
                {
                    for item in items {
                        if let Some(id) = item["id"].as_str() {
                            ids.insert(id.into());
                        }
                    }
                }
                collect_ids(child, ids);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_ids(item, ids);
            }
        }
        _ => (),
    }
}

fn validate_proposals(inputs: &Path, frozen: Option<&FrozenEvidence>) -> Result<(), String> {
    let evolution = object(&inputs.join("analyze/evolution.json"))?;
    let index = if let Some(frozen) = frozen {
        frozen.index.clone()
    } else {
        object(&inputs.join("collect/evidence/index.json"))?
    };
    let window = json!({"start":index["window_start"],"end_exclusive":index["window_end"]});
    if window["start"].is_null() || window["end_exclusive"].is_null() {
        return Err("evidence collection window is missing".into());
    }
    if frozen.is_none() && evolution["window"] != window {
        return Err("evolution.window must equal the collected evidence window".into());
    }
    let proposals = array(&evolution, "proposals")?;
    let carry = array(&evolution, "carry_forward")?;
    let mut ids = BTreeSet::new();
    for (proposal, is_new) in proposals
        .iter()
        .map(|item| (item, true))
        .chain(carry.iter().map(|item| (item, false)))
    {
        let id = required(proposal, "id")?;
        if !ids.insert(id.to_owned()) {
            return Err(format!("duplicate proposal continuity ID {id}"));
        }
        let status = required(proposal, "status")?;
        if !["proposed", "continue", "hold", "resolved"].contains(&status)
            || (!is_new && status == "proposed")
        {
            return Err("invalid proposal continuity status".into());
        }
        if is_new {
            for key in if frozen.is_some() {
                ["problem", "change", "validation", "risk", "rollback"]
            } else {
                ["scope", "change", "acceptance", "risk", "rollback"]
            } {
                required(proposal, key)?;
            }
        } else {
            required(proposal, "reason")?;
        }
        if is_new || status == "resolved" {
            references(array(proposal, "evidence")?, inputs, frozen, false)?;
        }
        if let Some(frozen) = frozen
            && is_new
        {
            if proposal["source_snapshot"]["captured_at"] != index["captured_at"]
                || proposal["collection_window"] != window
            {
                return Err(format!(
                    "{id}: proposal snapshot/window differs from frozen collection"
                ));
            }
            let source_layer = required(proposal, "source_layer")?;
            let expected_scope = match source_layer {
                "frozen_source" => "current_source",
                "run_projection" => "observed_run_window",
                "external_metadata" => "public_metadata_snapshot",
                "previous_report" => "previous_published_report",
                _ => return Err("unknown proposal evidence layer".into()),
            };
            if proposal["claim_scope"] != expected_scope
                || array(proposal, "evidence")?
                    .iter()
                    .any(|reference| reference["layer"] != source_layer)
            {
                return Err("proposal source layer and claim scope are inconsistent".into());
            }
            for reference in array(proposal, "evidence")? {
                frozen.validate_reference(reference, true)?;
            }
        }
    }
    let previous = if let Some(frozen) = frozen {
        frozen.previous_ids()?
    } else {
        let mut ids = BTreeSet::new();
        collect_ids(
            &object(&inputs.join("collect/evidence/previous.json"))?,
            &mut ids,
        );
        ids
    };
    if !previous.is_subset(&ids) {
        return Err(format!(
            "previous proposal IDs disappeared: {:?}",
            previous.difference(&ids).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

fn safety_issues(inputs: &Path) -> Result<Vec<Value>, String> {
    let value = object(&inputs.join("analyze/evolution.json"))?.to_string()
        + "\n"
        + &text(&inputs.join("analyze/rsi-report.md"))?;
    Ok(UNSAFE.iter().chain(OVERSTRONG.iter()).filter(|phrase| value.contains(**phrase))
        .map(|phrase| json!({"id":format!("safety/{phrase}"),"status":"open","domain":"report","location":"evolution.json/rsi-report.md","evidence":phrase,"required_change":"保留权限、脱敏和证据保护；限定主张范围，不以扩大暴露或无依据的历史首次/完备因果断言放行","acceptance":"提案和报告保留保护边界与可核查事实范围"})).collect())
}

pub fn aggregate(inputs: &Path, previous: &Value) -> Result<Value, String> {
    let commit = git_head(&inputs.join("analyze"))?;
    let mut checks = CHECKS
        .iter()
        .map(|key| ((*key).into(), Value::Bool(false)))
        .collect::<serde_json::Map<String, Value>>();
    let mut issues = safety_issues(inputs)?;
    let mut reviewers = Vec::new();
    let mut summaries = Vec::new();
    let result = (|| -> Result<String, String> {
        let branches = joined(inputs, "review-join", 2)?;
        let expected = BTreeSet::from(["fact-review", "proposal-review"]);
        if branches
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<BTreeSet<_>>()
            != expected
        {
            return Err("review-join requires both independent reviewers".into());
        }
        let mut decisions = Vec::new();
        for (name, joined_commit) in branches {
            let review = object(&inputs.join(&name).join("review.json"))?;
            if review["reviewed_commit"] != commit {
                return Err(format!(
                    "{name}: reviewed_commit differs from current analysis"
                ));
            }
            let decision = required(&review, "decision")?;
            if !["publish", "revise", "reaudit"].contains(&decision) {
                return Err("invalid independent review decision".into());
            }
            let keys = if name == "fact-review" {
                ["facts", "research"]
            } else {
                ["architecture", "writing"]
            };
            let owned = review["checks"]
                .as_object()
                .ok_or("review checks must be an object")?;
            if owned.len() != 2
                || keys
                    .iter()
                    .any(|key| !owned.get(*key).is_some_and(Value::is_boolean))
            {
                return Err("review must contain exactly its two boolean checks".into());
            }
            for key in keys {
                checks.insert(key.into(), owned[key].clone());
            }
            required(&review, "summary")?;
            if text(&inputs.join(&name).join("review.md"))?
                .trim()
                .is_empty()
            {
                return Err("independent human-readable review is missing".into());
            }
            let mut ids = BTreeSet::new();
            let mut open = false;
            for issue in array(&review, "issues")? {
                let id = required(issue, "id")?;
                if !ids.insert(format!("{name}/{id}"))
                    || !["open", "resolved", "limited"].contains(&required(issue, "status")?)
                {
                    return Err("malformed or duplicate review issue".into());
                }
                for field in ["location", "evidence", "required_change", "acceptance"] {
                    required(issue, field)?;
                }
                open |= issue["status"] == "open";
                let mut issue = issue.clone();
                issue["id"] = json!(format!("{name}/{id}"));
                issue["reviewer"] = json!(name);
                issues.push(issue);
            }
            for issue in previous
                .get("issues")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|issue| issue["reviewer"] == name)
            {
                if !ids.contains(required(issue, "id")?) {
                    return Err("previous independent review issue disappeared".into());
                }
            }
            if decision == "publish" && (open || owned.values().any(|value| value != true)) {
                return Err("a reviewer cannot approve open issues or failed checks".into());
            }
            decisions.push(decision.to_owned());
            reviewers.push(json!({"node":name,"commit":joined_commit,"reviewed_commit":commit,"decision":decision}));
            summaries.push(format!("{name}: {}", required(&review, "summary")?));
        }
        if decisions.iter().any(|decision| decision == "reaudit") {
            return Ok("reaudit".into());
        }
        if decisions.iter().any(|decision| decision != "publish")
            || checks.values().any(|value| value != true)
            || issues.iter().any(|issue| issue["status"] == "open")
        {
            Ok("revise".into())
        } else {
            Ok("publish".into())
        }
    })();
    let decision = match result {
        Ok(decision) => decision,
        Err(reason) => {
            issues.push(json!({"id":"review-contract","status":"open","domain":"review","required_change":reason}));
            summaries.push(reason);
            "revise".into()
        }
    };
    Ok(
        json!({"decision":decision,"reviewed_commit":commit,"checks":checks,"issues":issues,"reviewers":reviewers,"summary":summaries.join("\n")}),
    )
}

pub fn evaluate(inputs: &Path, frozen: Option<&FrozenEvidence>) -> (String, Vec<String>) {
    if let Err(reason) = validate_audits(inputs, frozen) {
        return (
            (if frozen.is_some() {
                "analyze"
            } else {
                "audit-context"
            })
            .into(),
            vec![reason],
        );
    }
    let result = (|| -> Result<(), String> {
        for name in ["rsi-report.md", "evolution.json", "sources.md"] {
            if text(&inputs.join("analyze").join(name))?.trim().is_empty() {
                return Err(format!("missing nonempty {name}"));
            }
        }
        validate_proposals(inputs, frozen)?;
        let safety = safety_issues(inputs)?;
        if !safety.is_empty() {
            return Err(format!("proposal safety/scope gate refused: {safety:?}"));
        }
        let review = object(&inputs.join("review/review.json"))?;
        if review["reviewed_commit"] != git_head(&inputs.join("analyze"))? {
            return Err("review does not correspond to current analysis commit".into());
        }
        if frozen.is_some() {
            if review["passed"] != true || !array(&review, "issues")?.is_empty() {
                return Err("independent reviewer has not approved publication".into());
            }
            array(&review, "coverage")?;
            array(&review, "limitations")?;
        } else {
            if review["decision"] != "publish"
                || CHECKS.iter().any(|key| review["checks"][*key] != true)
                || array(&review, "issues")?
                    .iter()
                    .any(|issue| !matches!(issue["status"].as_str(), Some("resolved" | "limited")))
            {
                return Err(format!(
                    "review requests {}: {}",
                    review["decision"], review["issues"]
                ));
            }
            let recomputed = aggregate(inputs, &json!({}))?;
            for field in [
                "decision",
                "reviewed_commit",
                "checks",
                "issues",
                "reviewers",
            ] {
                if review[field] != recomputed[field] {
                    return Err(
                        "combined review differs from its independent commit-bound inputs".into(),
                    );
                }
            }
        }
        if text(&inputs.join("review/review.md"))?.trim().is_empty() {
            return Err("human-readable review is missing".into());
        }
        Ok(())
    })();
    match result {
        Ok(()) => (
            "publish".into(),
            vec!["review, branch commits, evidence and proposal continuity verified".into()],
        ),
        Err(reason) => {
            let reaudit = frozen.is_none()
                && object(&inputs.join("review/review.json")).is_ok_and(|review| {
                    review["decision"] == "reaudit"
                        && git_head(&inputs.join("analyze"))
                            .is_ok_and(|head| review["reviewed_commit"] == head)
                });
            (
                (if reaudit { "audit-context" } else { "analyze" }).into(),
                vec![reason],
            )
        }
    }
}

pub fn gate(
    inputs: &Path,
    output: &Path,
    frozen: Option<&FrozenEvidence>,
) -> Result<(String, String), String> {
    let (target, reasons) = evaluate(inputs, frozen);
    let feedback = format!(
        "{}\ntarget={target}\n{}\n",
        if target == "publish" {
            "PASS"
        } else {
            "REVISE"
        },
        reasons.join("\n")
    );
    safe_path(&output.join("gate.txt"))?;
    fs::write(output.join("gate.txt"), feedback).map_err(|error| error.to_string())?;
    Ok((target, reasons.join("; ")))
}

pub fn assemble(
    inputs: &Path,
    output: &Path,
    frozen: Option<&FrozenEvidence>,
) -> Result<(), String> {
    let (target, reasons) = evaluate(inputs, frozen);
    if target != "publish" {
        return Err(format!("publication refused: {}", reasons.join("; ")));
    }
    for name in ["rsi-report.md", "evolution.json", "sources.md"] {
        copy(&inputs.join("analyze").join(name), &output.join(name))?;
    }
    for name in ["review.json", "review.md"] {
        copy(&inputs.join("review").join(name), &output.join(name))?;
    }
    copy(
        &inputs.join("audit-join/join.json"),
        &output.join("audit-manifest.json"),
    )?;
    if frozen.is_none() {
        copy(
            &inputs.join("review-join/join.json"),
            &output.join("review-manifest.json"),
        )?;
    }
    copy(&inputs.join("gate/gate.txt"), &output.join("gate.txt"))?;
    Ok(())
}

pub fn save_review(inputs: &Path, output: &Path) -> Result<(), String> {
    let previous = if output.join("review.json").exists() {
        object(&output.join("review.json"))?
    } else {
        json!({})
    };
    let review = aggregate(inputs, &previous)?;
    write_json(&output.join("review.json"), &review)?;
    safe_path(&output.join("review.md"))?;
    fs::write(
        output.join("review.md"),
        format!(
            "{}\n\n{}\n",
            required(&review, "decision")?,
            required(&review, "summary")?
        ),
    )
    .map_err(|error| error.to_string())
}
