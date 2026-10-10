//! Progressive disclosure for node tools.
//!
//! A node can mount far more tools than the model should carry in every request.
//! This decorator leaves the `ToolPort` contract untouched and only changes what
//! `definitions()` advertises:
//!
//! * tools the host declares always-visible stay listed;
//! * everything else stays *callable* but is reached through two meta tools that
//!   search the wrapped port's own definitions and then call it by name.
//!
//! Authorization, error semantics, receipts and tool facts are unchanged: every
//! call still goes through the wrapped port, so the host remains the only source
//! of authority and the bridge still observes each business call.

use std::sync::Arc;

use anchor_runtime::{ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent};
use serde_json::{Value, json};

/// Search the tools this node can call.
pub(crate) const SEARCH_TOOL: &str = "anchor_tools";
/// Call one of those tools by name.
pub(crate) const CALL_TOOL: &str = "anchor_tools_call";

/// Below this many hidden tools the two meta tools cost more than the schemas they
/// replace (measured: one tool costs 489 B listed against 890 B disclosed, so the
/// crossover is around two or three hidden tools).
const MIN_HIDDEN_FOR_DISCLOSURE: usize = 3;

const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 200;
const SUMMARY_CHARS: usize = 160;

/// How much a node discloses up front.
pub(crate) struct DisclosurePolicy {
    /// Tool names that must stay in the model's tool list, in order.
    always_visible: Vec<String>,
}

impl DisclosurePolicy {
    pub(crate) fn new(always_visible: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            always_visible: always_visible.into_iter().map(Into::into).collect(),
        }
    }
}

/// Wrap the node's tools when the host asks for on-demand disclosure.
///
/// `always_visible` names the tools a node's completion and continuity depend on;
/// everything else stays callable through the two meta tools.
pub(crate) fn maybe_wrap(
    inner: Arc<dyn ToolPort>,
    always_visible: impl IntoIterator<Item = impl Into<String>>,
) -> Arc<dyn ToolPort> {
    let policy = DisclosurePolicy::new(always_visible);
    let hidden = inner
        .definitions()
        .into_iter()
        .filter(|definition| !policy.listed(&definition.name))
        .count();
    let flag = std::env::var("ANCHOR_NODE_TOOL_DISCLOSURE").ok();
    if !should_disclose(flag.as_deref(), hidden) {
        return inner;
    }
    wrap(inner, policy)
}

/// The decision, kept pure so it is testable without the process environment.
///
/// `1` forces disclosure, `0` disables it, and anything else (including an
/// unrecognised value) discloses only when the node mounts enough hidden tools for
/// the meta tools to pay for themselves.
fn should_disclose(flag: Option<&str>, hidden: usize) -> bool {
    match flag {
        Some("0") => false,
        Some("1") => true,
        _ => hidden > MIN_HIDDEN_FOR_DISCLOSURE,
    }
}

/// Which tools keep their place in the list, given the node's defaults.
///
/// A host may narrow the always-visible set with `ANCHOR_NODE_ALWAYS_VISIBLE` (a JSON
/// array of tool names) when a node should reach even its own tools on demand.
pub(crate) fn always_visible(defaults: Vec<String>) -> Vec<String> {
    match std::env::var("ANCHOR_NODE_ALWAYS_VISIBLE") {
        Ok(configured) => parse_always_visible(&configured).unwrap_or(defaults),
        Err(_) => defaults,
    }
}

fn parse_always_visible(configured: &str) -> Option<Vec<String>> {
    serde_json::from_str::<Vec<String>>(configured).ok()
}

/// Whether the host forced disclosure, so the node boundary text only claims what
/// the model is certain to see. Automatic disclosure is explained by the meta
/// tools' own descriptions.
pub(crate) fn forced() -> bool {
    std::env::var("ANCHOR_NODE_TOOL_DISCLOSURE").as_deref() == Ok("1")
}

pub(crate) fn wrap(inner: Arc<dyn ToolPort>, policy: DisclosurePolicy) -> Arc<dyn ToolPort> {
    Arc::new(DisclosedTools { inner, policy })
}

struct DisclosedTools {
    inner: Arc<dyn ToolPort>,
    policy: DisclosurePolicy,
}

impl DisclosurePolicy {
    /// Whether a tool keeps its place in the model's tool list under this policy.
    fn listed(&self, name: &str) -> bool {
        DisclosedTools::is_meta(name) || self.always_visible.iter().any(|held| held == name)
    }
}

impl DisclosedTools {
    fn is_meta(name: &str) -> bool {
        name == SEARCH_TOOL || name == CALL_TOOL
    }

    fn listed(&self, name: &str) -> bool {
        self.policy.listed(name)
    }

    /// One line per tool is enough to decide whether to look closer.
    fn summary(definition: &ToolDefinition) -> String {
        let first = definition
            .description
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default();
        if first.chars().count() <= SUMMARY_CHARS {
            return first.to_owned();
        }
        let mut summary = first.chars().take(SUMMARY_CHARS).collect::<String>();
        summary.push('…');
        summary
    }

    fn search(&self, arguments: Value) -> Result<Vec<ToolResultContent>, ToolError> {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_PAGE as u64)
            .clamp(1, MAX_PAGE as u64) as usize;
        let mut matches = self
            .inner
            .definitions()
            .into_iter()
            .filter(|definition| !Self::is_meta(&definition.name))
            .filter(|definition| {
                query.is_empty()
                    || definition.name.to_lowercase().contains(&query)
                    || definition.description.to_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.name.cmp(&right.name));
        let total = matches.len();
        let page = matches
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|definition| {
                json!({
                    "name": definition.name,
                    "summary": Self::summary(&definition),
                    "read_only": self.inner.is_read_only(&definition.name),
                })
            })
            .collect::<Vec<_>>();
        let returned = page.len();
        let next_offset = offset + returned;
        Ok(vec![ToolResultContent::json(json!({
            "total": total,
            "offset": offset,
            "returned": returned,
            "tools": page,
            "truncated": next_offset < total,
            "next_offset": (next_offset < total).then_some(next_offset),
            "note": "call one with anchor_tools_call {name, arguments}",
        }))])
    }

    async fn invoke(&self, arguments: Value) -> Result<Vec<ToolResultContent>, ToolError> {
        let name = arguments
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                ToolError::Failed(
                    "anchor_tools_call needs `name`; use anchor_tools to list what is available"
                        .to_owned(),
                )
            })?;
        if Self::is_meta(name) {
            return Err(ToolError::Failed(format!(
                "`{name}` is a disclosure tool and cannot be called through anchor_tools_call"
            )));
        }
        let known = self
            .inner
            .definitions()
            .iter()
            .any(|definition| definition.name == name);
        if !known {
            return Err(ToolError::Failed(format!(
                "`{name}` is not available in this node; call anchor_tools to list what is"
            )));
        }
        let inner_arguments = arguments
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        self.inner.call(name, inner_arguments).await
    }
}

impl ToolPort for DisclosedTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self
            .inner
            .definitions()
            .into_iter()
            .filter(|definition| self.listed(&definition.name))
            .collect::<Vec<_>>();
        definitions.push(ToolDefinition::new(
            ToolName::new(SEARCH_TOOL).expect("static tool name"),
            "List or search the tools this node can call. Returns one line per tool \
             (name, summary, read_only) with paging; follow the `note` and call the tool you \
             need with anchor_tools_call. Available tools are host-authorized: nothing outside \
             this list can be called.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "case-insensitive substring of a name or summary"},
                    "offset": {"type": "integer", "minimum": 0},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_PAGE}
                },
                "additionalProperties": false
            }),
        ));
        definitions.push(ToolDefinition::new(
            ToolName::new(CALL_TOOL).expect("static tool name"),
            "Call a tool listed by anchor_tools, by name, with its own arguments object. \
             The result is the tool's own result, unchanged.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "minLength": 1},
                    "arguments": {"type": "object"}
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        ));
        definitions
    }

    fn is_read_only(&self, name: &str) -> bool {
        // The target of a call is only known from the arguments, so a call cannot
        // be advertised as read-only; the search tool only inspects definitions.
        name == SEARCH_TOOL
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a,
        >,
    > {
        Box::pin(async move {
            match name {
                SEARCH_TOOL => self.search(arguments),
                CALL_TOOL => self.invoke(arguments).await,
                other => self.inner.call(other, arguments).await,
            }
        })
    }
}

#[cfg(test)]
mod tests;
