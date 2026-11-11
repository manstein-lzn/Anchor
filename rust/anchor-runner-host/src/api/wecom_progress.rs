//! Read-only progress projection for an admitted channel message.
//!
//! Channel transports can update a status message in place, so the channel can
//! show what the agent is doing instead of a static placeholder. This endpoint
//! observes work that is already happening: it reads the notifications an active
//! Goose invocation retains in memory, maps them to a fixed vocabulary of short
//! status lines, and streams them to the caller.
//!
//! It is deliberately *not* a fact: nothing here is persisted, nothing here can
//! settle, retry, or fail a Run, and the only content that ever leaves the
//! process is a whitelisted short line. Tool names, arguments, paths, command
//! text, tool results and governance decisions never appear in the output.

use super::*;
use axum::{
    extract::{Path as AxumPath, Query as AxumQuery},
    response::sse::{Event, KeepAlive, Sse},
};
use futures_util::Stream;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    convert::Infallible,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(300);
const STREAM_LIMIT: Duration = Duration::from_secs(300);
/// A turn never shows more than this many progress lines.
const MAX_UPDATES: usize = 64;
const MAX_LINE_CHARS: usize = 600;
const PREPARING: &str = "正在准备…";
const LINE_FINAL_RESULT: &str = "正在整理回复";
const LINE_HISTORY: &str = "正在查看历史记录";
const LINE_IMAGE: &str = "正在准备图片";
const LINE_FILE: &str = "正在查看文件";
const LINE_COMMAND: &str = "正在工作区执行命令";
const LINE_PLUGIN: &str = "正在查询业务系统";
const LINE_DEFAULT: &str = "正在处理";
/// Stable machine tokens for the status line's category. The transport turns
/// these into presentation (icon, step marker); they are never localized here.
const CATEGORY_PREPARING: &str = "preparing";
const CATEGORY_READ_FILE: &str = "read_file";
const CATEGORY_COMMAND: &str = "command";
const CATEGORY_HISTORY: &str = "history";
const CATEGORY_IMAGE: &str = "image";
const CATEGORY_PLUGIN: &str = "plugin";
const CATEGORY_FINAL_RESULT: &str = "final_result";
const CATEGORY_DEFAULT: &str = "default";

/// Commands whose *category* may be described as reading files. Membership is
/// decided from the argument only to pick a category: the argument itself is
/// never rendered.
const READ_ONLY_FILE_COMMANDS: [&str; 12] = [
    "cat", "head", "tail", "less", "more", "ls", "find", "grep", "rg", "read", "sed", "awk",
];

#[derive(serde::Deserialize, Default)]
pub(super) struct ProgressQuery {
    after: Option<u64>,
}

struct ProgressItem {
    id: String,
    line: String,
    category: &'static str,
    /// 1-based index among the tool steps of this turn; the leading
    /// "preparing" item carries none.
    step: Option<u32>,
}

#[allow(clippy::result_large_err)]
pub(super) async fn wecom_progress(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((graph, event_id)): AxumPath<(String, String)>,
    AxumQuery(query): AxumQuery<ProgressQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, HttpResponse> {
    let last = match headers.get("last-event-id") {
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "invalid Last-Event-ID"))?,
        None => 0,
    };
    let after = query.after.unwrap_or(0).max(last);
    let owner = private_owner(&state, &headers);
    // Message ids are unique per bot only: the Graph's own account scopes them.
    let channel = state
        .application
        .channel_snapshot(&graph)
        .map_err(application_error)?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "no such channel event"))?;
    let source = channel.platform.as_str();
    let account = channel.account.unwrap_or_default();
    let inbound = wecom::inbound_id_for(source, &event_id);
    let lookup_state = state.clone();
    let lookup_owner = owner.clone();
    let lookup_account = account.clone();
    let lookup_inbound = inbound.clone();
    let found = blocking(move || {
        store(&lookup_state)?
            .channel_run_for_inbound(&lookup_owner, source, &lookup_account, &lookup_inbound)
            .map_err(session_error)
    })
    .await?;
    if found.is_none() {
        return Err(error(StatusCode::NOT_FOUND, "no such channel event"));
    }
    let cursor = Cursor {
        state: state.clone(),
        owner,
        account,
        source,
        inbound,
        after,
        emitted: BTreeSet::new(),
        pending: VecDeque::new(),
        ended: false,
        started: Instant::now(),
        warned: false,
    };
    let stream = futures_util::stream::unfold(cursor, |mut cursor| async move {
        loop {
            if let Some(event) = cursor.pending.pop_front() {
                return Some((Ok(event), cursor));
            }
            if cursor.ended || cursor.started.elapsed() >= STREAM_LIMIT {
                return None;
            }
            cursor.poll().await;
            if cursor.pending.is_empty() && !cursor.ended {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}

struct Cursor {
    state: ApiState,
    owner: String,
    account: String,
    source: &'static str,
    inbound: String,
    after: u64,
    emitted: BTreeSet<String>,
    pending: VecDeque<Event>,
    ended: bool,
    started: Instant,
    warned: bool,
}

impl Cursor {
    #[allow(clippy::result_large_err)]
    async fn poll(&mut self) {
        let state = self.state.clone();
        let owner = self.owner.clone();
        let account = self.account.clone();
        let inbound = self.inbound.clone();
        let source = self.source;
        let lookup = tokio::task::spawn_blocking(move || {
            store(&state)?
                .channel_run_for_inbound(&owner, source, &account, &inbound)
                .map_err(session_error)
        })
        .await;
        let found = match lookup {
            Ok(Ok(Some(found))) => found,
            Ok(Ok(None)) => {
                self.settle("missing");
                return;
            }
            Ok(Err(_)) | Err(_) => {
                if !self.warned {
                    self.warned = true;
                    self.pending.push_back(
                        Event::default()
                            .event("error")
                            .data(json!({"error":"channel progress lookup failed"}).to_string()),
                    );
                }
                return;
            }
        };
        let Some(run) = found.run_id else {
            return;
        };
        if let Ok(Some(metadata)) = crate::run_metadata::load(&self.state.data_root, &run)
            && let Some(source) = metadata.assistant.as_ref()
        {
            let input = store(&self.state).and_then(|sessions| {
                sessions
                    .get_channel_inbound(&self.owner, &found.session_id, &self.inbound)
                    .map_err(session_error)
            });
            match input {
                Ok(input)
                    if input.relation.superseded_by_turn_id.is_some()
                        || input.turn.status != anchor_platform_session::TurnStatus::Running =>
                {
                    self.settle(if input.relation.superseded_by_turn_id.is_some() {
                        "superseded"
                    } else {
                        "completed"
                    });
                    return;
                }
                Err(_) => return,
                _ => {}
            }
            // Which work invocation handled this Turn? The Session store
            // persisted the wait invocation that claimed it, and every node of a
            // round carries the same invocation number, so the work key is the
            // wait key with the node substituted. Records admitted before that
            // association existed fall back to the Run cursor.
            let wait_key = {
                let state = self.state.clone();
                let owner = self.owner.clone();
                let session = found.session_id.clone();
                let turn = found.turn_id.clone();
                tokio::task::spawn_blocking(move || {
                    store(&state)?
                        .channel_assistant_wait_key(&owner, &session, &turn)
                        .map_err(session_error)
                })
                .await
            };
            let key = match wait_key {
                Ok(Ok(Some(wait_key))) => round_work_invocation(
                    &wait_key,
                    &run,
                    &metadata.graph_digest,
                    &source.work_node,
                ),
                _ => None,
            }
            .or_else(|| {
                self.state
                    .application
                    .assistant_progress_work_invocation(&run, &source.work_node)
                    .ok()
                    .flatten()
            });
            // The Turn is admitted and still running, so it is owed at least the
            // guidance line: the user's message is accepted and the round will
            // work on it. A work invocation that cannot be named yet, one that
            // has not started, and one that runs without having retained a
            // notification yet (`Some(vec![])`) all read the same way — nothing
            // has been reported — and none of them may leave a running Turn with
            // no update at all.
            let notifications = key
                .as_ref()
                .and_then(|key| {
                    crate::goose_acp::live_notifications(&self.state.data_root, key)
                        .ok()
                        .flatten()
                })
                .unwrap_or_default();
            self.push_updates(&notifications);
            return;
        }
        // Only a persistent assistant answers channel messages.
        self.settle("missing");
    }

    /// Queue every line of the projection the caller has not seen yet.
    fn push_updates(&mut self, notifications: &[Value]) {
        for (seq, item) in new_progress(notifications, self.after, &mut self.emitted) {
            self.pending.push_back(
                Event::default()
                    .id(seq.to_string())
                    .event("update")
                    .data(update_payload(seq, &item).to_string()),
            );
        }
    }

    fn settle(&mut self, status: &str) {
        let seq = self.emitted.len() as u64;
        self.pending.push_back(
            Event::default()
                .event("settled")
                .data(json!({"seq":seq,"settled":true,"status":status}).to_string()),
        );
        self.ended = true;
    }
}

/// The work invocation that handled one round of a persistent assistant.
///
/// `InvocationKey::durable_key` is `run:digest:node:invocation`, and every node
/// of a round carries the same invocation number, so the wait invocation the
/// Session store persisted for this Turn names the round and the work key is
/// that key with the node substituted. Anything malformed, inconsistent or from
/// another Run yields `None` so the caller can fall back instead of guessing.
pub(super) fn round_work_invocation(
    wait_key: &str,
    run: &str,
    graph_digest: &str,
    work_node: &str,
) -> Option<anchor_runtime::graph::InvocationKey> {
    let (run_id, digest, _wait_node, invocation) = split_durable_key(wait_key)?;
    if run_id != run || digest != graph_digest {
        return None;
    }
    let invocation = invocation.parse::<u64>().ok()?;
    Some(anchor_runtime::graph::InvocationKey {
        run_id: run_id.to_owned(),
        graph_digest: digest.to_owned(),
        node_id: work_node.to_owned(),
        invocation,
    })
}

fn split_durable_key(value: &str) -> Option<(&str, &str, &str, &str)> {
    let mut fields = value.splitn(4, ':');
    let run = fields.next()?;
    let digest = fields.next()?;
    let node = fields.next()?;
    let invocation = fields.next()?;
    (!run.is_empty() && !digest.is_empty() && !node.is_empty() && !invocation.is_empty())
        .then_some((run, digest, node, invocation))
}

/// The incremental step of the stream: project the retained notifications, drop
/// everything the cursor already covered, and return `(seq, line)` pairs in
/// order. `seq` is the 1-based position in the projection, so a cursor is a
/// stable best-effort resume point; `emitted` keeps one connection from
/// repeating a line when the projection shifts.
///
/// The caller only reaches this for a Turn that is still running, so the
/// guidance line is always part of the projection — including when the live
/// trace has retained nothing yet. Emitting it first also fixes its `seq`: the
/// tool steps that arrive later keep their positions instead of shifting down.
fn new_progress(
    notifications: &[Value],
    after: u64,
    emitted: &mut BTreeSet<String>,
) -> Vec<(u64, ProgressItem)> {
    let mut fresh = Vec::new();
    for (index, item) in progress_projection(notifications, true)
        .into_iter()
        .enumerate()
    {
        let seq = index as u64 + 1;
        if seq <= after || !emitted.insert(item.id.clone()) {
            continue;
        }
        fresh.push((seq, item));
    }
    fresh
}

/// One `update` event payload. `step` is omitted when the item has none, so a
/// client that only understands `content` keeps working.
fn update_payload(seq: u64, item: &ProgressItem) -> Value {
    let mut payload = json!({
        "seq":seq,
        "kind":"status",
        "content":item.line,
        "category":item.category,
        "settled":false,
    });
    if let Some(step) = item.step {
        payload["step"] = json!(step);
    }
    payload
}

/// Pure projection: retained Goose notifications in, channel-safe status lines
/// out. Only `tool_call` and its correlated status updates are read. `agent_message_chunk`,
/// `agent_thought_chunk` and `user_message_chunk` are ignored outright, and no
/// tool argument, path, result or title is ever copied into the output.
///
/// `guidance` adds the leading "preparing" line. The endpoint always asks for
/// it: an invocation that has retained nothing yet is a real state of a running
/// Turn, and the guidance line is the only honest thing to show for it.
fn progress_projection(notifications: &[Value], guidance: bool) -> Vec<ProgressItem> {
    let mut items = Vec::new();
    let mut calls = BTreeMap::<String, ToolProgress>::new();
    let mut steps = 0u32;
    for event in notifications {
        if event["method"] != "session/update" {
            continue;
        }
        let update = &event["params"]["update"];
        let Some(id) = update["toolCallId"].as_str() else {
            continue;
        };
        match update["sessionUpdate"].as_str() {
            Some("tool_call") if !calls.contains_key(id) => {
                let (line, category) = tool_line(update);
                let repeated = items
                    .last()
                    .is_some_and(|item: &ProgressItem| item.line == line);
                // Collapsed repeats share their category step; completion still
                // belongs to the actual tool id, never its title or arguments.
                if !repeated {
                    steps += 1;
                    items.push(ProgressItem {
                        id: format!("tool:{id}"),
                        line: line.to_owned(),
                        category,
                        step: Some(steps),
                    });
                }
                calls.insert(
                    id.to_owned(),
                    ToolProgress {
                        line,
                        category,
                        step: steps,
                        status: "pending",
                    },
                );
            }
            Some("tool_call_update") => {
                let Some(call) = calls.get_mut(id) else {
                    continue;
                };
                let Some(status @ ("in_progress" | "completed" | "failed")) =
                    update["status"].as_str()
                else {
                    continue;
                };
                if call.status == status || matches!(call.status, "completed" | "failed") {
                    continue;
                }
                call.status = match status {
                    "completed" => "completed",
                    "failed" => "failed",
                    _ => "in_progress",
                };
                let line = tool_status_line(call);
                if items.last().is_some_and(|item| item.line == line) {
                    continue;
                }
                items.push(ProgressItem {
                    id: format!("tool:{id}:{status}"),
                    line: line.to_owned(),
                    category: call.category,
                    step: Some(call.step),
                });
            }
            _ => continue,
        }
        if items.len() >= MAX_UPDATES {
            break;
        }
    }
    if guidance {
        items.insert(
            0,
            ProgressItem {
                id: "start".to_owned(),
                line: PREPARING.to_owned(),
                category: CATEGORY_PREPARING,
                step: None,
            },
        );
        items.truncate(MAX_UPDATES);
    }
    debug_assert!(
        items
            .iter()
            .all(|item| item.line.chars().count() <= MAX_LINE_CHARS)
    );
    items
}

struct ToolProgress {
    line: &'static str,
    category: &'static str,
    step: u32,
    status: &'static str,
}

fn tool_status_line(call: &ToolProgress) -> &'static str {
    match (call.category, call.status) {
        (CATEGORY_READ_FILE, "completed") => "文件查看已结束",
        (CATEGORY_READ_FILE, "failed") => "文件查看失败",
        (CATEGORY_COMMAND, "completed") => "工作区命令已结束",
        (CATEGORY_COMMAND, "failed") => "工作区命令失败",
        (CATEGORY_HISTORY, "completed") => "历史记录查看已结束",
        (CATEGORY_HISTORY, "failed") => "历史记录查看失败",
        (CATEGORY_IMAGE, "completed") => "图片准备已结束",
        (CATEGORY_IMAGE, "failed") => "图片准备失败",
        (CATEGORY_PLUGIN, "completed") => "业务查询已结束",
        (CATEGORY_PLUGIN, "failed") => "业务查询失败",
        (CATEGORY_FINAL_RESULT, "completed") => "回复整理已结束",
        (CATEGORY_FINAL_RESULT, "failed") => "回复整理失败",
        (_, "completed") => "工具处理已结束",
        (_, "failed") => "工具处理失败",
        _ => call.line,
    }
}

/// The projection of a stream whose notifications may still be empty.
///
/// The endpoint always asks [`progress_projection`] for the guidance line; this
/// shorthand keeps the pure-projection tests readable.
#[cfg(test)]
fn progress_items(notifications: &[Value]) -> Vec<ProgressItem> {
    progress_projection(notifications, !notifications.is_empty())
}

fn tool_line(update: &Value) -> (&'static str, &'static str) {
    let tool = update["_meta"]["goose"]["toolCall"]["toolName"]
        .as_str()
        .unwrap_or_default();
    tool_line_for(tool, update)
}

/// The fixed status line for one tool call, plus its category token.
fn tool_line_for(tool: &str, update: &Value) -> (&'static str, &'static str) {
    // Anchor's own tools reach the node through the MCP server, so the live
    // name is namespaced ("anchor__anchor_run"). Compare the tool segment, and
    // keep the plugin fallback for names that belong to another server.
    let namespaced = tool.contains("__");
    let name = tool.rsplit("__").next().unwrap_or(tool);
    match name {
        "final_result" => (LINE_FINAL_RESULT, CATEGORY_FINAL_RESULT),
        "anchor_conversation_history" => (LINE_HISTORY, CATEGORY_HISTORY),
        "wecom_attach_image" => (LINE_IMAGE, CATEGORY_IMAGE),
        "anchor_run" => {
            if READ_ONLY_FILE_COMMANDS.contains(&first_command_token(update).as_str()) {
                (LINE_FILE, CATEGORY_READ_FILE)
            } else {
                (LINE_COMMAND, CATEGORY_COMMAND)
            }
        }
        _ if namespaced => (LINE_PLUGIN, CATEGORY_PLUGIN),
        _ => (LINE_DEFAULT, CATEGORY_DEFAULT),
    }
}

/// The command a tool call is about to run, with a shell wrapper unwrapped.
///
/// `anchor_run` receives argv, and the node usually wraps its work in
/// `sh -c "<script>"`, so the interesting token is the script's first word.
/// The token is only ever compared against [`READ_ONLY_FILE_COMMANDS`]; it is
/// never rendered into a progress line.
fn first_command_token(update: &Value) -> String {
    let parts: Vec<&str> = match update["rawInput"]["command"].as_array() {
        Some(parts) => parts.iter().filter_map(Value::as_str).collect(),
        None => update["rawInput"]["command"].as_str().into_iter().collect(),
    };
    let parts: Vec<&str> = parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect();
    let script = match parts.as_slice() {
        [shell, flag, script, ..] if matches!(*shell, "sh" | "bash" | "env") && *flag == "-c" => {
            script
        }
        [first, ..] => first,
        [] => "",
    };
    script
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `(seq, line, category, step)` for readable assertions.
    fn steps(items: &[(u64, ProgressItem)]) -> Vec<(u64, &str, &str, Option<u32>)> {
        items
            .iter()
            .map(|(seq, item)| (*seq, item.line.as_str(), item.category, item.step))
            .collect()
    }

    fn categories(items: &[ProgressItem]) -> Vec<(&str, Option<u32>)> {
        items
            .iter()
            .map(|item| (item.category, item.step))
            .collect()
    }

    fn tool_call(id: &str, tool: &str, raw: Value) -> Value {
        json!({"method":"session/update","params":{"sessionId":"native","update":{
            "sessionUpdate":"tool_call","toolCallId":id,"status":"pending",
            "_meta":{"goose":{"toolCall":{"toolName":tool}}},"rawInput":raw
        }}})
    }

    #[test]
    fn tool_calls_map_to_a_fixed_vocabulary_without_leaking_arguments() {
        let notifications = vec![
            tool_call(
                "call-1",
                "anchor_run",
                json!({"command":["cat","/in/channel/secret-report.docx"]}),
            ),
            tool_call(
                "call-2",
                "anchor_run",
                json!({"command":["sh","-c","curl http://169.254.169.254/latest/meta-data"]}),
            ),
            tool_call(
                "call-3",
                "final_result",
                json!({"summary":"internal draft"}),
            ),
            tool_call("call-4", "anchor_conversation_history", json!({"limit":5})),
            tool_call(
                "call-5",
                "wecom_attach_image",
                json!({"path":"/workspace/chart.png"}),
            ),
            tool_call(
                "call-6",
                "research__search",
                json!({"query":"private research topic"}),
            ),
            tool_call(
                "call-7",
                "totally_unknown_tool",
                json!({"payload":"secret"}),
            ),
        ];
        let items = progress_items(&notifications);
        let lines: Vec<&str> = items.iter().map(|item| item.line.as_str()).collect();
        assert_eq!(
            lines,
            vec![
                PREPARING,
                LINE_FILE,
                LINE_COMMAND,
                LINE_FINAL_RESULT,
                LINE_HISTORY,
                LINE_IMAGE,
                LINE_PLUGIN,
                LINE_DEFAULT,
            ]
        );
        let rendered = items
            .iter()
            .map(|item| item.line.clone())
            .collect::<Vec<_>>()
            .join("\n");
        for secret in [
            "/in/channel/secret-report.docx",
            "169.254.169.254",
            "curl",
            "internal draft",
            "chart.png",
            "private research topic",
            "payload",
            "secret",
            "anchor_run",
            "research__search",
            "final_result",
        ] {
            assert!(
                !rendered.contains(secret),
                "{secret} leaked into progress output: {rendered}"
            );
        }
    }

    #[test]
    fn status_items_carry_a_category_token_and_a_gapless_step_number() {
        let notifications = vec![
            tool_call(
                "c1",
                "anchor__anchor_run",
                json!({"command":["sh","-c","ls -la /workspace"]}),
            ),
            // A repeated category collapses, so it must not consume a number.
            tool_call(
                "c2",
                "anchor__anchor_run",
                json!({"command":["sh","-c","ls /tmp"]}),
            ),
            tool_call(
                "c3",
                "anchor__anchor_run",
                json!({"command":["sh","-c","printf x > /workspace/a"]}),
            ),
            tool_call("c4", "anchor_conversation_history", json!({})),
            tool_call("c5", "wecom_attach_image", json!({})),
            tool_call("c6", "research__search", json!({})),
            tool_call("c7", "totally_unknown", json!({})),
            tool_call("c8", "anchor__final_result", json!({"summary":"draft"})),
        ];
        let items = progress_items(&notifications);
        assert_eq!(
            categories(&items),
            vec![
                (CATEGORY_PREPARING, None),
                (CATEGORY_READ_FILE, Some(1)),
                (CATEGORY_COMMAND, Some(2)),
                (CATEGORY_HISTORY, Some(3)),
                (CATEGORY_IMAGE, Some(4)),
                (CATEGORY_PLUGIN, Some(5)),
                (CATEGORY_DEFAULT, Some(6)),
                (CATEGORY_FINAL_RESULT, Some(7)),
            ]
        );
        // Every token comes from the fixed set and no argument text leaks,
        // including through the new fields.
        const TOKENS: [&str; 8] = [
            CATEGORY_PREPARING,
            CATEGORY_READ_FILE,
            CATEGORY_COMMAND,
            CATEGORY_HISTORY,
            CATEGORY_IMAGE,
            CATEGORY_PLUGIN,
            CATEGORY_FINAL_RESULT,
            CATEGORY_DEFAULT,
        ];
        let rendered = items
            .iter()
            .map(|item| format!("{}|{}", item.line, item.category))
            .collect::<Vec<_>>()
            .join("\n");
        for item in &items {
            assert!(
                TOKENS.contains(&item.category),
                "unknown category token: {}",
                item.category
            );
        }
        for secret in [
            "/workspace",
            "printf",
            "ls -la",
            "draft",
            "anchor__anchor_run",
            "totally_unknown",
        ] {
            assert!(
                !rendered.contains(secret),
                "{secret} leaked into progress output: {rendered}"
            );
        }
    }

    #[test]
    fn update_payload_carries_the_category_and_only_a_present_step() {
        let items = progress_items(&[tool_call(
            "c1",
            "anchor__anchor_run",
            json!({"command":["sh","-c","cat /workspace/secret"]}),
        )]);
        let payloads = items
            .iter()
            .enumerate()
            .map(|(index, item)| update_payload(index as u64 + 1, item))
            .collect::<Vec<_>>();
        assert_eq!(payloads[0]["content"], PREPARING);
        assert_eq!(payloads[0]["category"], CATEGORY_PREPARING);
        assert_eq!(payloads[0]["settled"], false);
        assert!(payloads[0].get("step").is_none(), "preparing has no step");
        assert_eq!(payloads[1]["content"], LINE_FILE);
        assert_eq!(payloads[1]["category"], CATEGORY_READ_FILE);
        assert_eq!(payloads[1]["step"], 1);
        let rendered = payloads
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn only_tool_calls_produce_progress_and_repeats_collapse() {
        let notifications = vec![
            json!({"method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"narration must not stream"}}}}),
            json!({"method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"reasoning must not stream"}}}}),
            json!({"method":"session/update","params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"user text must not stream"}}}}),
            tool_call(
                "call-1",
                "anchor_run",
                json!({"command":["ls","/workspace"]}),
            ),
            tool_call("call-2", "anchor_run", json!({"command":["ls","/tmp"]})),
            tool_call("call-3", "anchor_run", json!({"command":["grep","-r","x"]})),
        ];
        let items = progress_items(&notifications);
        assert_eq!(
            items
                .iter()
                .map(|item| item.line.as_str())
                .collect::<Vec<_>>(),
            vec![PREPARING, LINE_FILE]
        );
        let rendered = items
            .iter()
            .map(|item| item.line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for secret in ["narration", "reasoning", "user text", "/workspace", "/tmp"] {
            assert!(!rendered.contains(secret), "{secret} leaked: {rendered}");
        }
    }

    #[test]
    fn progress_is_bounded_and_ids_are_stable() {
        let notifications: Vec<Value> = (0..80)
            .map(|index| {
                tool_call(
                    &format!("call-{index}"),
                    if index % 2 == 0 {
                        "anchor_run"
                    } else {
                        "final_result"
                    },
                    json!({"command":["cat","x"]}),
                )
            })
            .collect();
        let items = progress_items(&notifications);
        assert_eq!(items.len(), MAX_UPDATES);
        assert_eq!(items[0].id, "start");
        let repeated = progress_items(&notifications);
        assert_eq!(
            items.iter().map(|item| item.id.clone()).collect::<Vec<_>>(),
            repeated
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>()
        );
        assert!(progress_items(&[]).is_empty());
    }

    #[test]
    fn cursor_returns_only_new_lines_and_never_repeats_one() {
        let first = vec![tool_call(
            "call-1",
            "anchor_run",
            json!({"command":["cat","a"]}),
        )];
        let mut emitted = BTreeSet::new();
        assert_eq!(
            steps(&new_progress(&first, 0, &mut emitted)),
            vec![
                (1, PREPARING, CATEGORY_PREPARING, None),
                (2, LINE_FILE, CATEGORY_READ_FILE, Some(1)),
            ]
        );
        assert!(new_progress(&first, 0, &mut emitted).is_empty());

        // Notifications arrive over time: the projection grows, and only the new
        // tail is returned.
        let later = vec![
            tool_call("call-1", "anchor_run", json!({"command":["cat","a"]})),
            tool_call("call-2", "final_result", json!({})),
        ];
        assert_eq!(
            steps(&new_progress(&later, 0, &mut emitted)),
            vec![(3, LINE_FINAL_RESULT, CATEGORY_FINAL_RESULT, Some(2))]
        );

        // A resume cursor skips what the caller already saw...
        let mut resumed = BTreeSet::new();
        assert_eq!(
            steps(&new_progress(&later, 2, &mut resumed)),
            vec![(3, LINE_FINAL_RESULT, CATEGORY_FINAL_RESULT, Some(2))]
        );
        // ...and a cursor past the end yields nothing.
        assert!(new_progress(&later, 9, &mut BTreeSet::new()).is_empty());
    }

    #[test]
    fn an_invocation_that_retained_nothing_yet_still_gets_the_guidance_line() {
        // The trace is live but its prompt has not produced a notification yet.
        // The endpoint must not answer that with silence.
        let mut emitted = BTreeSet::new();
        assert_eq!(
            steps(&new_progress(&[], 0, &mut emitted)),
            vec![(1, PREPARING, CATEGORY_PREPARING, None)]
        );
        assert!(new_progress(&[], 0, &mut emitted).is_empty());

        // The guidance line keeps its identity and position, so the first tool
        // step that arrives later is the second line of the same stream.
        let later = vec![tool_call(
            "call-1",
            "anchor_run",
            json!({"command":["cat","a"]}),
        )];
        assert_eq!(
            steps(&new_progress(&later, 0, &mut emitted)),
            vec![(2, LINE_FILE, CATEGORY_READ_FILE, Some(1))]
        );
    }

    #[test]
    fn thought_and_message_notifications_alone_still_yield_the_guidance_line() {
        let notifications = vec![
            json!({"method":"session/update","params":{"sessionId":"native","update":{
                "sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"plan /workspace"}}}}),
            json!({"method":"session/update","params":{"sessionId":"native","update":{
                "sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}}}),
        ];
        let items = progress_items(&notifications);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].line, PREPARING);
        assert_eq!(items[0].category, CATEGORY_PREPARING);
        assert!(items[0].step.is_none());
        let payload = update_payload(1, &items[0]);
        assert_eq!(payload["seq"], 1);
        assert_eq!(payload["kind"], "status");
        assert_eq!(payload["category"], CATEGORY_PREPARING);
        assert_eq!(payload["settled"], false);
        assert!(payload.get("step").is_none(), "preparing has no step");
        let rendered = payload.to_string();
        for secret in ["plan", "/workspace", "hello", "thought", "message"] {
            assert!(!rendered.contains(secret), "{secret} leaked: {rendered}");
        }
    }

    #[test]
    fn namespaced_anchor_tools_and_shell_wrapped_commands_keep_their_category() {
        let notifications = vec![
            tool_call(
                "call-1",
                "anchor__anchor_run",
                json!({"command":["sh","-c","ls -la /workspace"]}),
            ),
            tool_call(
                "call-2",
                "anchor__anchor_run",
                json!({"command":["sh","-c","printf 'hello' > /workspace/note.txt && ls -l /workspace"]}),
            ),
            tool_call(
                "call-3",
                "anchor__anchor_run",
                json!({"command":["sh","-c","cat /workspace/note.txt; echo; wc -c /workspace/note.txt"]}),
            ),
            tool_call("call-4", "anchor__final_result", json!({"summary":"draft"})),
            tool_call("call-5", "scholarly__search", json!({"query":"topic"})),
        ];
        let items = progress_items(&notifications);
        let lines: Vec<&str> = items.iter().map(|item| item.line.as_str()).collect();
        assert_eq!(
            lines,
            vec![
                PREPARING,
                LINE_FILE,
                LINE_COMMAND,
                LINE_FILE,
                LINE_FINAL_RESULT,
                LINE_PLUGIN,
            ]
        );
        let rendered = items
            .iter()
            .map(|item| item.line.clone())
            .collect::<Vec<_>>()
            .join("\n");
        for secret in [
            "/workspace",
            "note.txt",
            "ls -la",
            "printf",
            "hello",
            "wc -c",
            "draft",
            "topic",
            "anchor__anchor_run",
        ] {
            assert!(
                !rendered.contains(secret),
                "{secret} leaked into progress output: {rendered}"
            );
        }
    }

    #[test]
    fn the_persisted_round_names_the_work_invocation() {
        // The Session store keeps the wait invocation that claimed the Turn;
        // every node of a round carries the same invocation number.
        let key = round_work_invocation(
            "assistant-1:digest:wait_input:3",
            "assistant-1",
            "digest",
            "assistant",
        )
        .unwrap();
        assert_eq!(key.run_id, "assistant-1");
        assert_eq!(key.graph_digest, "digest");
        assert_eq!(key.node_id, "assistant");
        assert_eq!(key.invocation, 3);
    }

    #[test]
    fn an_inconsistent_or_malformed_round_is_rejected_instead_of_guessed() {
        for (wait_key, run, digest) in [
            // another Run
            ("assistant-2:digest:wait_input:1", "assistant-1", "digest"),
            // another Graph identity
            ("assistant-1:other:wait_input:1", "assistant-1", "digest"),
            // not a durable key at all (fixtures, older records)
            ("fixture-wait-key", "assistant-1", "digest"),
            // invocation is not a number
            (
                "assistant-1:digest:wait_input:next",
                "assistant-1",
                "digest",
            ),
            // too few fields
            ("assistant-1:digest:wait_input", "assistant-1", "digest"),
            // empty field
            ("assistant-1::wait_input:1", "assistant-1", "digest"),
        ] {
            assert!(
                round_work_invocation(wait_key, run, digest, "assistant").is_none(),
                "accepted {wait_key}"
            );
        }
    }

    #[test]
    fn argument_bearing_titles_are_not_used_as_tool_identity() {
        let notifications = vec![json!({"method":"session/update","params":{"update":{
            "sessionUpdate":"tool_call","toolCallId":"call-1","title":"anchor_run: cat /etc/passwd",
            "rawInput":{"command":["cat","/etc/passwd"]}
        }}})];
        let items = progress_items(&notifications);
        assert_eq!(items[1].line, LINE_DEFAULT);
        assert!(!items.iter().any(|item| item.line.contains("/etc/passwd")));
    }

    #[test]
    fn sparse_native_lifecycle_updates_keep_identity_step_and_resume_order() {
        let mut call = tool_call(
            "call-1",
            "anchor__anchor_run",
            json!({"command":["cat","/in/private"]}),
        );
        // Pending is the ACP default and is absent on the pinned Goose wire.
        call["params"]["update"]
            .as_object_mut()
            .unwrap()
            .remove("status");
        let mut notifications = vec![call];
        let mut emitted = BTreeSet::new();
        assert_eq!(
            steps(&new_progress(&notifications, 0, &mut emitted)),
            vec![
                (1, PREPARING, CATEGORY_PREPARING, None),
                (2, LINE_FILE, CATEGORY_READ_FILE, Some(1)),
            ]
        );
        for (id, status) in [
            ("unpaired", Some("failed")),
            ("call-1", None),
            ("call-1", Some("in_progress")),
            ("call-1", Some("completed")),
            ("call-1", Some("failed")),
        ] {
            notifications.push(json!({"method":"session/update","params":{"update":{
                "sessionUpdate":"tool_call_update","toolCallId":id,"status":status,
                "title":"private title","content":[{"text":"private output"}],"rawOutput":"private result"
            }}}));
        }
        assert_eq!(
            steps(&new_progress(&notifications, 0, &mut emitted)),
            vec![(3, "文件查看已结束", CATEGORY_READ_FILE, Some(1)),]
        );
        let resumed = new_progress(&notifications, 2, &mut BTreeSet::new());
        assert_eq!(
            steps(&resumed),
            vec![(3, "文件查看已结束", CATEGORY_READ_FILE, Some(1))]
        );
        let rendered = update_payload(3, &resumed[0].1).to_string();
        assert!(!rendered.contains("private"));
        assert!(new_progress(&notifications, 0, &mut emitted).is_empty());
    }

    #[test]
    fn failed_tool_is_an_activity_not_a_successful_reply() {
        let notifications = vec![
            tool_call(
                "call-1",
                "anchor__anchor_run",
                json!({"command":["sh","-c","private-command"]}),
            ),
            json!({"method":"session/update","params":{"update":{
                "sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"failed",
                "content":[{"text":"private failure"}]
            }}}),
        ];
        let items = progress_items(&notifications);
        assert_eq!(items[2].line, "工作区命令失败");
        assert_eq!(items[1].step, items[2].step);
        assert_eq!(update_payload(3, &items[2])["settled"], false);
    }
}
