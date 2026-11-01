//! L4: the hub's reads as a Model Context Protocol server over stdio, for
//! agents. One JSON-RPC message per line in, one per line out. Every tool
//! reads and none writes; each call first projects what was appended since
//! the last one. Answers are bounded: lists are paged and say how many items
//! there are, so an agent starts from the signals and drills down to the
//! records they cite instead of reading histories whole.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use serde::Serialize;
use serde_json::{Value, json};

use crate::adapters::{Hub, now_ms};
use crate::analyzers::{
    Analyzer, Finding,
    contrast::Contrast,
    gate::Gate,
    health::Health,
    profile::Profile,
    signals::{Signals, route_label},
    stall::Stall,
};
use crate::query::{Invocation, Query};

/// The protocol version answered when the client names none.
const PROTOCOL: &str = "2025-06-18";
/// Items per page unless told otherwise, and at most.
const PAGE: usize = 20;
const MAX_PAGE: usize = 200;
/// Turns, calls and gaps listed for one invocation, at most.
const DETAIL: usize = 500;
/// Evidence cited per finding in a page of findings.
const CITED: usize = 5;

const INSTRUCTIONS: &str = "Read-only facts about recorded Anchor Runs. Start with `signals`: a bounded table of what went wrong ranked by how many Runs it reaches, which harnesses are beaten on a graded task, and which invocations stand out among their peers; and `report` with analyzer `profile` for where each task's cost goes. Drill down only where they point: `invocation` by key, `calls` for what calls actually asked and answered, `records` by evidence key, `configurations` by digest, `report` for one analyzer's findings, `invocations` to list and filter. Lists are paged (20 by default); `total` says how many there are.";

/// Answer requests from stdin until it closes.
pub fn serve(mut hub: Hub) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&mut hub, &message),
            Err(e) => Some(failure(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")
                .and_then(|()| stdout.flush())
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// The reply to one JSON-RPC message; none for notifications and responses.
pub fn handle(hub: &mut Hub, message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(failure(id, -32600, "invalid request"));
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "anchor-hub", "version": env!("CARGO_PKG_VERSION")},
            "instructions": INSTRUCTIONS,
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => call(hub, &params),
        other => return Some(failure(id, -32601, &format!("method not found: {other}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn call(hub: &mut Hub, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);
    let answer = match hub.refresh() {
        Ok(_) => tool(&hub.query(Some(now_ms())), name, args),
        Err(e) => Err(e),
    };
    match answer {
        Ok(value) => {
            json!({"content": [{"type": "text", "text": value.to_string()}], "isError": false})
        }
        Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
    }
}

/// One tool's answer, as `tools/call` gives it and the `tool` command
/// prints it.
pub fn tool(query: &Query, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "signals" => {
            let mut signals = Signals::default();
            if let Some(limit) = number(args, "limit")? {
                signals.limit = limit.min(MAX_PAGE);
            }
            Ok(json!(signals.table(query)))
        }
        "report" => report(query, args),
        "invocations" => invocations(query, args),
        "invocation" => invocation(query, args),
        "configurations" => configurations(query, args),
        "records" => records(query, args),
        "calls" => calls(query, args),
        "check" => check(query, args),
        other => Err(format!("unknown tool {other}")),
    }
}

fn text<'v>(args: &'v Value, name: &str) -> Option<&'v str> {
    args.get(name).and_then(Value::as_str)
}

fn number(args: &Value, name: &str) -> Result<Option<usize>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(|n| Some(n as usize))
            .ok_or(format!("{name} must be a non-negative integer")),
    }
}

/// One page of `items`, by the `offset` and `limit` arguments.
fn page<T: Serialize>(items: Vec<T>, args: &Value) -> Result<Value, String> {
    let total = items.len();
    let offset = number(args, "offset")?.unwrap_or(0);
    let limit = number(args, "limit")?.unwrap_or(PAGE).clamp(1, MAX_PAGE);
    let items: Vec<T> = items.into_iter().skip(offset).take(limit).collect();
    let end = offset + items.len();
    Ok(json!({
        "total": total,
        "offset": offset,
        "items": items,
        "next_offset": (end < total).then_some(end),
    }))
}

fn bounded<T: Serialize>(items: Vec<T>) -> Value {
    json!({
        "total": items.len(),
        "items": items.into_iter().take(DETAIL).collect::<Vec<_>>(),
    })
}

fn report(query: &Query, args: &Value) -> Result<Value, String> {
    let report = match text(args, "analyzer") {
        Some("health") => Health.analyze(query),
        Some("profile") => Profile.analyze(query),
        Some("stall") => {
            let mut stall = Stall::default();
            if let Some(minutes) = number(args, "idle_minutes")? {
                stall.idle_ms = minutes as u64 * 60_000;
            }
            stall.analyze(query)
        }
        Some("contrast") => {
            let mut contrast = Contrast::default();
            if let Some(by) = text(args, "by") {
                contrast.by = by.to_owned();
            }
            contrast.reference = text(args, "reference").map(str::to_owned);
            contrast.analyze(query)
        }
        Some("gate") => {
            let mut gate = Gate::default();
            if let Some(by) = text(args, "by") {
                gate.by = by.to_owned();
            }
            gate.reference = text(args, "reference").map(str::to_owned);
            gate.analyze(query)
        }
        other => {
            return Err(format!(
                "analyzer must be health, profile, stall, contrast or gate, not {other:?}"
            ));
        }
    };
    let mut codes: BTreeMap<&str, usize> = BTreeMap::new();
    for finding in &report.findings {
        *codes.entry(finding.code).or_insert(0) += 1;
    }
    let (code, subject) = (text(args, "code"), text(args, "subject"));
    let findings: Vec<Value> = report
        .findings
        .iter()
        .filter(|f| code.is_none_or(|c| f.code == c))
        .filter(|f| subject.is_none_or(|s| f.subject.contains(s)))
        .map(cited)
        .collect();
    Ok(json!({
        "analyzer": report.analyzer,
        "version": report.version,
        "not_applicable": report.not_applicable,
        "summary": report.summary,
        "codes": codes,
        "findings": page(findings, args)?,
    }))
}

fn cited(finding: &Finding) -> Value {
    json!({
        "code": finding.code,
        "subject": finding.subject,
        "detail": finding.detail,
        "evidence": finding.evidence.iter().take(CITED).collect::<Vec<_>>(),
        "evidence_total": finding.evidence.len(),
    })
}

fn outcome(invocation: &Invocation) -> String {
    invocation
        .lifecycles()
        .last()
        .and_then(|life| json!(life.outcome).as_str().map(str::to_owned))
        .unwrap_or_else(|| "unobserved".into())
}

/// An invocation in one line: where, under what, how it ended and what it
/// cost.
fn brief(query: &Query, invocation: &Invocation) -> Value {
    let calls = invocation.calls();
    let turns = invocation.turns();
    let grades = query.grades();
    let grade = grades.get(invocation.run_id.as_str()).map(|graded| {
        json!({
            "task": format!("{} v{}", graded.grade.task, graded.grade.task_version),
            "score": graded.grade.score(),
            "passed": graded.grade.passed(),
        })
    });
    json!({
        "key": invocation.key(),
        "config": invocation.config(),
        "accepted": invocation.result.is_some(),
        "outcome": outcome(invocation),
        "agent_ms": invocation
            .lifecycles()
            .iter()
            .map(|l| l.ended_at_ms.saturating_sub(l.started_at_ms))
            .sum::<u64>(),
        "tokens": turns.iter().map(|t| t.total_tokens).sum::<u64>(),
        "turns": turns.len(),
        "calls": calls.len(),
        "failed_calls": calls.iter().filter(|c| !c.ok).count(),
        "route": route_label(&invocation.route()),
        "grade": grade,
    })
}

fn invocations(query: &Query, args: &Value) -> Result<Value, String> {
    let (run, node, config) = (text(args, "run"), text(args, "node"), text(args, "config"));
    let (wanted, tool, task) = (
        text(args, "outcome"),
        text(args, "tool"),
        text(args, "task"),
    );
    let grades = query.grades();
    let listed: Vec<Value> = query
        .invocations()
        .iter()
        .filter(|i| run.is_none_or(|r| i.run_id == r))
        .filter(|i| node.is_none_or(|n| i.node_id == n))
        .filter(|i| config.is_none_or(|c| i.config().is_some_and(|d| d.starts_with(c))))
        .filter(|i| wanted.is_none_or(|o| outcome(i) == o))
        .filter(|i| tool.is_none_or(|t| i.calls().iter().any(|c| c.tool == t)))
        .filter(|i| {
            task.is_none_or(|t| {
                grades.get(i.run_id.as_str()).is_some_and(|g| {
                    g.grade.task == t || format!("{} v{}", g.grade.task, g.grade.task_version) == t
                })
            })
        })
        .map(|i| brief(query, i))
        .collect();
    page(listed, args)
}

fn invocation(query: &Query, args: &Value) -> Result<Value, String> {
    let key = text(args, "key").ok_or("key is required: run/node#invocation")?;
    let invocations = query.invocations();
    let found = invocations
        .iter()
        .find(|i| i.key() == key)
        .ok_or_else(|| format!("no invocation {key}"))?;
    let gaps: Vec<Value> = found
        .gaps()
        .iter()
        .map(|gap| {
            json!({
                "start": gap.start,
                "ms": gap.ms(),
                "place": gap.place(),
                "after": gap.after,
                "before": gap.before,
            })
        })
        .collect();
    Ok(json!({
        "brief": brief(query, found),
        "lifecycles": found.lifecycles(),
        "turns": bounded(found.turns()),
        "calls": bounded(found.calls()),
        "gaps": bounded(gaps),
        "evidence": found.evidence(),
    }))
}

fn configurations(query: &Query, args: &Value) -> Result<Value, String> {
    let prefix = text(args, "digest").unwrap_or("");
    let matching: Vec<_> = query
        .configurations()
        .filter(|(digest, _)| digest.starts_with(prefix))
        .collect();
    if let [(digest, manifest)] = matching.as_slice() {
        return Ok(json!({
            "digest": digest, "manifest": manifest,
            "envelope": query.manifest(&manifest.envelope_sha256),
        }));
    }
    let mut used: BTreeMap<&str, usize> = BTreeMap::new();
    for invocation in query.invocations() {
        if let Some(digest) = invocation.config() {
            *used.entry(digest).or_insert(0) += 1;
        }
    }
    let listed: Vec<Value> = matching
        .iter()
        .map(|(digest, manifest)| {
            json!({
                "digest": digest,
                "node": manifest.node_id,
                "model": manifest.model,
                "graph": manifest.graph_digest.get(..12),
                "plugins": manifest.plugins.iter().map(|p| &p.id).collect::<Vec<_>>(),
                "host": manifest.host_sha256.get(..12),
                "tools": manifest.tools_sha256.get(..12),
                "envelope": manifest.envelope_sha256.get(..12),
                "invocations": used.get(digest).copied().unwrap_or(0),
            })
        })
        .collect();
    page(listed, args)
}

/// Tool calls at the value level: what they asked and answered, cut to a
/// head, beside their facts. Either the calls of one invocation, or calls
/// across invocations picked by tool, program and how they ended, so that a
/// failure class is read from its instances rather than guessed.
fn calls(query: &Query, args: &Value) -> Result<Value, String> {
    let key = text(args, "key");
    let (tool, program, ended) = (
        text(args, "tool"),
        text(args, "program"),
        text(args, "ended"),
    );
    if key.is_none() && tool.is_none() && program.is_none() && ended.is_none() {
        return Err("give an invocation key, or filter by tool, program or ended".into());
    }
    // Across invocations one failure repeats verbatim (one missing program,
    // dozens of Runs); asked to, calls that ended alike are one row that
    // counts them. Opt-in, because recorded checks point into the listed
    // shape and must keep reading the same answer.
    let group = match args.get("group") {
        None => false,
        Some(value) => value.as_bool().ok_or("group must be true or false")?,
    };
    let invocations = query.invocations();
    let mut listed = Vec::new();
    let mut groups: BTreeMap<Ending, Group> = BTreeMap::new();
    for invocation in invocations
        .iter()
        .filter(|i| key.is_none_or(|k| i.key() == k))
    {
        for (index, call, detail) in invocation.detailed() {
            let class = match (call.ok, call.command) {
                (false, _) => json!(call.failure).as_str().unwrap_or("failed").to_owned(),
                (true, None) => "ok".to_owned(),
                (true, Some(end)) if end.clean() => "clean".to_owned(),
                (true, Some(end)) => match end.status {
                    anchor_facts::CommandStatus::NotStarted => "not_started".to_owned(),
                    anchor_facts::CommandStatus::TimedOut => "timed_out".to_owned(),
                    anchor_facts::CommandStatus::Exited => "exit_nonzero".to_owned(),
                },
            };
            if tool.is_some_and(|t| call.tool != t)
                || program.is_some_and(|p| call.program != Some(p))
                || ended.is_some_and(|e| {
                    let wanted = if e == "not_done" {
                        !call.done()
                    } else {
                        class == e
                    };
                    !wanted
                })
            {
                continue;
            }
            let instance = json!({
                "invocation": invocation.key(),
                "index": index,
                "model_request": call.model_request,
                "tool": call.tool,
                "ended": class,
                "duration_ms": call.duration_ms(),
                "exit_code": call.command.and_then(|end| end.exit_code),
                "arguments": detail.map(|d| d.arguments.as_str()),
                "answer": detail.map(|d| d.answer.as_str()),
            });
            if !group {
                listed.push(instance);
                continue;
            }
            let first_line = detail
                .and_then(|d| d.answer.lines().next())
                .map(|line| line.chars().take(GROUP_LINE).collect())
                .unwrap_or_default();
            let entry = groups
                .entry((
                    call.tool.to_owned(),
                    call.program.map(str::to_owned),
                    class,
                    call.command.and_then(|end| end.exit_code),
                    first_line,
                ))
                .or_default();
            entry.calls += 1;
            entry.runs.insert(invocation.run_id.clone());
            let key = invocation.key();
            if entry.invocations.len() < GROUP_INVOCATIONS && !entry.invocations.contains(&key) {
                entry.invocations.push(key);
            }
            entry.example.get_or_insert(instance);
        }
    }
    if group {
        listed = grouped(groups);
    }
    page(listed, args)
}

/// Characters of an answer's first line that tell like endings apart.
const GROUP_LINE: usize = 200;
/// Invocations named per group; `runs` and `calls` count all of them.
const GROUP_INVOCATIONS: usize = 5;

/// What makes two calls end alike: tool, program, ending, exit code and the
/// first line of the answer.
type Ending = (String, Option<String>, String, Option<i32>, String);

#[derive(Default)]
struct Group {
    calls: usize,
    runs: std::collections::BTreeSet<String>,
    invocations: Vec<String>,
    example: Option<Value>,
}

/// Groups of calls that ended alike (tool, program, ending, exit code and
/// the first line of the answer), the most widespread first, each with one
/// instance to read.
fn grouped(groups: BTreeMap<Ending, Group>) -> Vec<Value> {
    let mut rows: Vec<_> = groups.into_iter().collect();
    rows.sort_by(|(a_key, a), (b_key, b)| {
        (b.runs.len(), b.calls)
            .cmp(&(a.runs.len(), a.calls))
            .then_with(|| a_key.cmp(b_key))
    });
    rows.into_iter()
        .map(|((tool, program, ended, exit_code, first_line), group)| {
            json!({
                "tool": tool,
                "program": program,
                "ended": ended,
                "exit_code": exit_code,
                "first_line": first_line,
                "calls": group.calls,
                "runs": group.runs.len(),
                "invocations": group.invocations,
                "example": group.example,
            })
        })
        .collect()
}

/// Run checks: each names a read-only tool and its arguments, a JSON Pointer
/// into the answer, and optionally a comparison with an expected value. A
/// claim about the data is only as good as a check anyone can repeat; this
/// is that repetition, the same for an agent over MCP and for a script over
/// the command line.
fn check(query: &Query, args: &Value) -> Result<Value, String> {
    let checks = args
        .get("checks")
        .and_then(Value::as_array)
        .ok_or("checks is required: a list of {tool, args, path, op, value}")?;
    if checks.len() > MAX_PAGE {
        return Err(format!("at most {MAX_PAGE} checks per call"));
    }
    let results: Vec<Value> = checks
        .iter()
        .map(|spec| match run_check(query, spec) {
            Ok((status, actual)) => json!({"status": status, "actual": actual}),
            Err(message) => json!({"status": "error", "actual": null, "message": message}),
        })
        .collect();
    Ok(json!({"results": results}))
}

/// One check: `pass` or `fail` with the value found, or just `value` when no
/// comparison was asked for.
fn run_check(query: &Query, spec: &Value) -> Result<(&'static str, Value), String> {
    let name = text(spec, "tool").ok_or("a check names a tool")?;
    if name == "check" {
        return Err("a check cannot run checks".into());
    }
    let empty = json!({});
    let answer = tool(query, name, spec.get("args").unwrap_or(&empty))?;
    let path = text(spec, "path").unwrap_or("");
    let found = answer.pointer(path).cloned();
    let Some(op) = text(spec, "op") else {
        return Ok(("value", found.unwrap_or(Value::Null)));
    };
    let expected = spec.get("value").cloned().unwrap_or(Value::Null);
    let actual = found.clone().unwrap_or(Value::Null);
    let number = |v: &Value| v.as_f64();
    let passed = match op {
        "exists" => found.is_some(),
        "absent" => found.is_none(),
        "==" => {
            actual == expected
                || matches!((number(&actual), number(&expected)), (Some(a), Some(b)) if a == b)
        }
        "!=" => {
            actual != expected
                && !matches!((number(&actual), number(&expected)), (Some(a), Some(b)) if a == b)
        }
        ">=" | "<=" | ">" | "<" => {
            let (Some(a), Some(b)) = (number(&actual), number(&expected)) else {
                return Err(format!("{op} needs numbers, found {actual} and {expected}"));
            };
            match op {
                ">=" => a >= b,
                "<=" => a <= b,
                ">" => a > b,
                _ => a < b,
            }
        }
        "contains" => match (&actual, &expected) {
            (Value::String(a), Value::String(b)) => a.contains(b.as_str()),
            (Value::Array(items), wanted) => items.contains(wanted),
            (Value::Object(map), Value::String(key)) => map.contains_key(key),
            _ => actual.to_string().contains(&expected.to_string()),
        },
        other => return Err(format!("unknown op {other}")),
    };
    // The value is kept short: the check says what was found, it does not
    // carry the answer.
    let shown = match &actual {
        Value::String(s) => Value::String(crate::table::head(s)),
        Value::Array(items) if items.len() > 5 => json!({"length": items.len()}),
        Value::Object(map) if map.len() > 10 => json!({"keys": map.len()}),
        other => other.clone(),
    };
    Ok((if passed { "pass" } else { "fail" }, shown))
}

/// Fact rows of one resource, as cited by evidence keys.
fn records(query: &Query, args: &Value) -> Result<Value, String> {
    let resource = text(args, "resource").ok_or("resource is required")?;
    let root = text(args, "root");
    let place = number(args, "place")?;
    let rows: Vec<_> = query
        .rows()
        .iter()
        .filter(|row| row.key.resource == resource)
        .filter(|row| root.is_none_or(|r| row.key.root.contains(r)))
        .filter(|row| place.is_none_or(|p| row.key.place == p))
        .collect();
    page(rows, args)
}

fn tools() -> Value {
    let paging = json!({
        "offset": {"type": "integer", "minimum": 0},
        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_PAGE},
    });
    let with_paging = |properties: Value| {
        let mut properties = properties;
        for (name, schema) in paging.as_object().expect("an object") {
            properties[name] = schema.clone();
        }
        properties
    };
    let read_only = json!({"readOnlyHint": true, "idempotentHint": true, "openWorldHint": false});
    json!([
        {
            "name": "signals",
            "description": "The ranked signal table: findings merged by kind and place, harnesses beaten on a graded task by another on score, tokens and agent time, the front of unbeaten harnesses per task, and exemplar invocations (far cheaper than their peers, or a route nobody else took). Each signal has a stable id, coordinates in the harness space and evidence keys. What did not fit is counted in `omitted`.",
            "inputSchema": {"type": "object", "properties": {"limit": {"type": "integer", "minimum": 1, "maximum": MAX_PAGE}}},
            "annotations": read_only,
        },
        {
            "name": "report",
            "description": "One analyzer's report: health (record integrity, tool and command failures, check failures, startup), profile (per task: cost spread, turns, context growth, model and tool time shares, cheapest and dearest Runs), stall (stalled tasks, gaps, slow calls, loops) or contrast (graded Runs compared by a configuration field). Findings are paged and can be filtered by code and subject.",
            "inputSchema": {"type": "object", "required": ["analyzer"], "properties": with_paging(json!({
                "analyzer": {"type": "string", "enum": ["health", "profile", "stall", "contrast", "gate"]},
                "code": {"type": "string"},
                "subject": {"type": "string", "description": "substring"},
                "idle_minutes": {"type": "integer", "minimum": 1},
                "by": {"type": "string", "description": "contrast: manifest field naming the arm, host_sha256 by default"},
                "reference": {"type": "string", "description": "contrast: prefix of the reference arm"},
            }))},
            "annotations": read_only,
        },
        {
            "name": "invocations",
            "description": "AgentNode invocations, one line each: configuration, outcome, agent time, tokens, turns, calls, failures, route and grade. Filters combine.",
            "inputSchema": {"type": "object", "properties": with_paging(json!({
                "run": {"type": "string"},
                "node": {"type": "string"},
                "config": {"type": "string", "description": "digest prefix"},
                "outcome": {"type": "string", "enum": ["completed", "failed", "cancelled", "interrupted", "unobserved"]},
                "tool": {"type": "string", "description": "called this tool"},
                "task": {"type": "string", "description": "graded task name, or name and version as `task v1`"},
            }))},
            "annotations": read_only,
        },
        {
            "name": "invocation",
            "description": "One invocation in full: lifecycles, model turns, tool calls (shapes of arguments, never their content), uncovered gaps and the records it rests on.",
            "inputSchema": {"type": "object", "required": ["key"], "properties": {"key": {"type": "string", "description": "run/node#invocation"}}},
            "annotations": read_only,
        },
        {
            "name": "configurations",
            "description": "AgentNode configuration manifests: Graph, node, model, plugins, Host and tool digests. With a digest prefix matching one manifest, that manifest whole; otherwise a page of them with how many invocations ran under each.",
            "inputSchema": {"type": "object", "properties": with_paging(json!({"digest": {"type": "string", "description": "prefix"}}))},
            "annotations": read_only,
        },
        {
            "name": "calls",
            "description": "Tool calls at the value level: arguments and the head of the answer (a command's stderr, else stdout, or the refusal), with how each ended: ok, clean, exit_nonzero, not_started, timed_out, or a failure class. Give an invocation key for its calls in order, or filter across invocations by tool, program and ended (`not_done` = anything but a clean or ok end). With `group: true`, calls that ended alike (tool, program, ending, exit code, first line of the answer) are one row each with how many calls and Runs, up to 5 invocation keys and one example, most widespread first; read groups first when looking across Runs. Use it to read a failure from its instances before explaining it.",
            "inputSchema": {"type": "object", "properties": with_paging(json!({
                "key": {"type": "string", "description": "run/node#invocation"},
                "group": {"type": "boolean", "description": "group calls that ended alike instead of listing each; default false"},
                "tool": {"type": "string"},
                "program": {"type": "string", "description": "base name of the command a command tool ran"},
                "ended": {"type": "string", "description": "ok, clean, exit_nonzero, not_started, timed_out, not_done, or a failure class such as invalid_arguments"},
            }))},
            "annotations": read_only,
        },
        {
            "name": "check",
            "description": "Run up to 200 checks and report pass, fail or error for each, with the value found. A check is {tool, args, path, op, value}: `tool` and `args` are one of this server's read tools and its arguments, `path` a JSON Pointer into its answer (\"/total\", \"/summary/tools/anchor_run/commands/python3/not_started\"), `op` one of == != >= <= > < contains exists absent, `value` the expected value. Without `op` the value found is returned. Use it to confirm a claim before stating it.",
            "inputSchema": {"type": "object", "required": ["checks"], "properties": {"checks": {"type": "array", "maxItems": MAX_PAGE, "items": {"type": "object", "required": ["tool"], "properties": {
                "tool": {"type": "string"},
                "args": {"type": "object"},
                "path": {"type": "string", "description": "JSON Pointer"},
                "op": {"type": "string", "enum": ["==", "!=", ">=", "<=", ">", "<", "contains", "exists", "absent"]},
                "value": {},
            }}}}},
            "annotations": read_only,
        },
        {
            "name": "records",
            "description": "Fact rows of one resource, as evidence keys cite them: {root, resource, place}. Rows carry stamps, verdicts and observations, no business content.",
            "inputSchema": {"type": "object", "required": ["resource"], "properties": with_paging(json!({
                "resource": {"type": "string"},
                "root": {"type": "string", "description": "substring of the data root"},
                "place": {"type": "integer", "minimum": 1},
            }))},
            "annotations": read_only,
        },
    ])
}
