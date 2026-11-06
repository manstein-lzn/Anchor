//! Shared disclosure policy; execution remains in the original authorized port.
//! Pilot uses the policy without a ToolPort decorator so native MCP peers survive.

use crate::plugin_disclosure::{self, PluginDisclosureCatalog};
use anchor_runtime::{ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) const SEARCH_TOOL: &str = "anchor_tools";
pub(crate) const DETAIL_TOOL: &str = "anchor_tool_detail";
pub(crate) const CALL_TOOL: &str = "anchor_tools_call";
const MIN_HIDDEN_FOR_DISCLOSURE: usize = 3;
#[cfg(test)]
const SUMMARY_CHARS: usize = 160;

fn unavailable_tool(name: &str) -> ToolError {
    plugin_disclosure::rejected(format!(
        "`{name}` is not available in this invocation; use anchor_tools to discover current authorized names before reading a schema or calling a tool"
    ))
}

pub(crate) struct DisclosurePolicy {
    always_visible: Vec<String>,
}
impl DisclosurePolicy {
    pub(crate) fn new(always_visible: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            always_visible: always_visible.into_iter().map(Into::into).collect(),
        }
    }
    fn listed(&self, name: &str) -> bool {
        self.always_visible.iter().any(|held| held == name)
    }
}

fn should_disclose(flag: Option<&str>, hidden: usize) -> bool {
    match flag {
        Some("0") => false,
        Some("1") => true,
        _ => hidden > MIN_HIDDEN_FOR_DISCLOSURE,
    }
}

pub(crate) fn always_visible(defaults: Vec<String>) -> Vec<String> {
    match std::env::var("ANCHOR_NODE_ALWAYS_VISIBLE") {
        Ok(configured) => parse_always_visible(&configured).unwrap_or(defaults),
        Err(_) => defaults,
    }
}
fn parse_always_visible(configured: &str) -> Option<Vec<String>> {
    serde_json::from_str(configured).ok()
}
pub(crate) fn forced() -> bool {
    std::env::var("ANCHOR_NODE_TOOL_DISCLOSURE").as_deref() == Ok("1")
}

/// A pure view of the actual inventory, used by both native Pilot and Node.
pub(crate) struct ToolDisclosure {
    definitions: Vec<ToolDefinition>,
    read_only: BTreeMap<String, bool>,
    policy: DisclosurePolicy,
    catalog: PluginDisclosureCatalog,
    enabled: bool,
}

impl ToolDisclosure {
    pub(crate) fn new(
        definitions: Vec<ToolDefinition>,
        read_only: BTreeMap<String, bool>,
        policy: DisclosurePolicy,
        mut catalog: PluginDisclosureCatalog,
    ) -> Self {
        catalog.owners.retain(|name, _| {
            definitions
                .iter()
                .any(|definition| definition.name == *name)
        });
        let hidden = definitions
            .iter()
            .filter(|definition| !policy.listed(&definition.name))
            .count();
        let enabled = should_disclose(
            std::env::var("ANCHOR_NODE_TOOL_DISCLOSURE").ok().as_deref(),
            hidden,
        );
        Self {
            definitions,
            read_only,
            policy,
            catalog,
            enabled,
        }
    }

    pub(crate) fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self
            .definitions
            .iter()
            .filter(|definition| !self.enabled || self.policy.listed(&definition.name))
            .cloned()
            .collect::<Vec<_>>();
        definitions.extend(plugin_disclosure::definitions());
        definitions.push(ToolDefinition::new(ToolName::new(SEARCH_TOOL).unwrap(),
            "Search the complete host-authorized tool inventory with AND terms, optional exact name/Plugin and bounded paging. Results include real Plugin/server/tool ownership. Fetch anchor_tool_detail {name} for its complete description and parameter schema, then call anchor_tools_call {name,arguments}. Exact name and offset paging reach entries omitted from initial context; reread after compaction.",
            json!({"type":"object","properties":{"query":{"type":"string"},"name":{"type":"string"},"plugin":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":plugin_disclosure::MAX_PAGE}},"additionalProperties":false})));
        definitions.push(ToolDefinition::new(ToolName::new(DETAIL_TOOL).unwrap(),
            "Get the host-authorized tool's full description, input parameter schema and Plugin ownership. Use an exact name returned by anchor_tools, not a name inferred from history. The schema is complete or a clear size error is returned; it is never silently truncated. Invoke with anchor_tools_call using these constraints.",
            json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false})));
        definitions.push(ToolDefinition::new(ToolName::new(CALL_TOOL).unwrap(),
            "Call a host-authorized tool by its exact name and object arguments after reading anchor_tool_detail. The result, errors, media, native confirmation and cancellation retain the target tool's semantics. Never use this for final_result (node completion) or disclosure tools; call those directly.",
            json!({"type":"object","properties":{"name":{"type":"string","minLength":1},"arguments":{"type":"object"}},"required":["name"],"additionalProperties":false})));
        definitions
    }

    pub(crate) fn instructions(&self) -> String {
        self.catalog.instructions()
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.definitions
            .iter()
            .any(|definition| definition.name == name)
    }
    pub(crate) fn is_read_only(&self, name: &str) -> bool {
        if is_meta(name) {
            name != CALL_TOOL
        } else {
            self.read_only.get(name).copied().unwrap_or(false)
        }
    }
    pub(crate) fn resolve(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<(String, Value), ToolError> {
        let (name, arguments) = resolve_call(name, arguments)?;
        if !is_meta(&name) && !self.contains(&name) {
            return Err(unavailable_tool(&name));
        }
        Ok((name, arguments))
    }
    pub(crate) fn inspect(
        &self,
        name: &str,
        arguments: &Value,
    ) -> Result<Vec<ToolResultContent>, ToolError> {
        if name == plugin_disclosure::LIST_TOOL || name == plugin_disclosure::READ_TOOL {
            return self.catalog.call(name, arguments);
        }
        let value = match name {
            SEARCH_TOOL => {
                let query = arguments
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let exact = arguments.get("name").and_then(Value::as_str);
                let plugin = arguments.get("plugin").and_then(Value::as_str);
                let mut items = self.definitions.iter().filter(|definition| {
                    let owner = self.catalog.owners.get(&definition.name);
                    exact.is_none_or(|name|name == definition.name)
                        && plugin.is_none_or(|plugin|owner.is_some_and(|owner|owner.plugin == plugin))
                        && (exact.is_some() || plugin_disclosure::matches(query,&format!("{} {} {}",definition.name,definition.description,owner.map(|owner|format!("{} {} {}",owner.plugin,owner.server,owner.tool)).unwrap_or_default())))
                }).map(|definition|json!({"name":definition.name,"summary":plugin_disclosure::summary(&definition.description),"read_only":self.is_read_only(&definition.name),"owner":self.catalog.owners.get(&definition.name)})).collect::<Vec<_>>();
                items.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
                let mut result = plugin_disclosure::page(items, "tools", arguments)?;
                result["note"] = json!(
                    "Fetch the complete schema using anchor_tool_detail {name}, then invoke anchor_tools_call {name,arguments}. Owner=null identifies a Host tool. Use exact name or paging if search misses."
                );
                result
            }
            DETAIL_TOOL => {
                let name = arguments
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        plugin_disclosure::rejected("anchor_tool_detail requires name")
                    })?;
                let definition = self
                    .definitions
                    .iter()
                    .find(|definition| definition.name == name)
                    .ok_or_else(|| unavailable_tool(name))?;
                plugin_disclosure::bounded(
                    json!({"name":name,"description":definition.description,"parameters":definition.parameters,"read_only":self.is_read_only(name),"owner":self.catalog.owners.get(name),"complete":true}),
                )?
            }
            _ => return Err(ToolError::Unknown(name.into())),
        };
        Ok(vec![ToolResultContent::json(value)])
    }
}

pub(crate) fn is_meta(name: &str) -> bool {
    matches!(
        name,
        SEARCH_TOOL
            | DETAIL_TOOL
            | CALL_TOOL
            | plugin_disclosure::LIST_TOOL
            | plugin_disclosure::READ_TOOL
    )
}

/// Bridge parses the envelope before observations; the original port still
/// checks authorization, and the outer ACP ID is kept for native correlation.
pub(crate) fn resolve_call(name: &str, arguments: Value) -> Result<(String, Value), ToolError> {
    if name != CALL_TOOL {
        return Ok((name.into(), arguments));
    }
    let target = arguments
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            plugin_disclosure::rejected("anchor_tools_call needs name; use anchor_tools")
        })?;
    if is_meta(target) || target == "final_result" {
        return Err(plugin_disclosure::rejected(format!(
            "`{target}` is a disclosure tool or native completion; call it directly"
        )));
    }
    let inner = arguments
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !inner.is_object() {
        return Err(plugin_disclosure::rejected(
            "anchor_tools_call arguments must be an object",
        ));
    }
    Ok((target.into(), inner))
}

pub(crate) fn maybe_wrap(
    inner: Arc<dyn ToolPort>,
    visible: impl IntoIterator<Item = impl Into<String>>,
    catalog: PluginDisclosureCatalog,
) -> (Arc<dyn ToolPort>, String) {
    let definitions = inner.definitions();
    let read_only = definitions
        .iter()
        .map(|definition| {
            (
                definition.name.clone(),
                inner.is_read_only(&definition.name),
            )
        })
        .collect();
    let disclosure = ToolDisclosure::new(
        definitions,
        read_only,
        DisclosurePolicy::new(visible),
        catalog,
    );
    let instructions = disclosure.instructions();
    (Arc::new(DisclosedTools { inner, disclosure }), instructions)
}

#[cfg(test)]
pub(crate) fn wrap(inner: Arc<dyn ToolPort>, policy: DisclosurePolicy) -> Arc<dyn ToolPort> {
    let definitions = inner.definitions();
    let read_only = definitions
        .iter()
        .map(|definition| {
            (
                definition.name.clone(),
                inner.is_read_only(&definition.name),
            )
        })
        .collect();
    let mut disclosure = ToolDisclosure::new(
        definitions,
        read_only,
        policy,
        PluginDisclosureCatalog::default(),
    );
    disclosure.enabled = true;
    Arc::new(DisclosedTools { inner, disclosure })
}

struct DisclosedTools {
    inner: Arc<dyn ToolPort>,
    disclosure: ToolDisclosure,
}
impl ToolPort for DisclosedTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.disclosure.definitions()
    }
    fn is_read_only(&self, name: &str) -> bool {
        self.disclosure.is_read_only(name)
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
            let (name, arguments) = self.disclosure.resolve(name, arguments)?;
            if is_meta(&name) {
                self.disclosure.inspect(&name, &arguments)
            } else {
                self.inner.call(&name, arguments).await
            }
        })
    }
}

#[cfg(test)]
mod tests;
