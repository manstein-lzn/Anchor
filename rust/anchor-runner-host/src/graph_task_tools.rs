//! Host tools for the official graph-tasks Skill. Model arguments describe work,
//! never caller identity, grants, credentials, or a new Plugin installation.
use crate::{
    application::{
        ApplicationError, RunApplication,
        plugin_candidates::CandidateTestContext,
        scheduling::{TaskScheduleRequest, TaskScheduleUpdate},
    },
    plugin_disclosure::{PluginDisclosureCatalog, ToolOwner},
};
use anchor_runtime::{
    ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent,
    graph::{InvocationKey, PluginBinding},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
};

fn rejected(reason: impl Into<String>) -> ToolError {
    ToolError::Failed(reason.into())
}
fn failure(error: ApplicationError) -> ToolError {
    match error {
        ApplicationError::Missing => rejected("task, Graph, or Session not found in this scope"),
        ApplicationError::Invalid(reason) | ApplicationError::Conflict(reason) => rejected(reason),
        ApplicationError::Storage(_) => {
            rejected("task storage is unavailable; inspect retained facts before retrying")
        }
    }
}

pub(crate) fn wrap(
    inner: Arc<dyn ToolPort>,
    bindings: &[PluginBinding],
    key: InvocationKey,
    application: RunApplication,
    context: CandidateTestContext,
    disclosure: &mut PluginDisclosureCatalog,
) -> Arc<dyn ToolPort> {
    if !bindings.iter().any(|plugin| plugin.id == "graph-tasks") {
        return inner;
    }
    for definition in definitions() {
        disclosure.owners.insert(
            definition.name.clone(),
            ToolOwner {
                plugin: "graph-tasks".into(),
                server: "host".into(),
                tool: definition.name,
            },
        );
    }
    Arc::new(GraphTaskTools {
        inner,
        bindings: bindings.to_vec(),
        key,
        application,
        context,
    })
}

struct GraphTaskTools {
    inner: Arc<dyn ToolPort>,
    bindings: Vec<PluginBinding>,
    key: InvocationKey,
    application: RunApplication,
    context: CandidateTestContext,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphArg {
    graph: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DefinitionArg {
    definition: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArg {
    graph: String,
    definition: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArg {
    graph: String,
    #[serde(default = "object")]
    input: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArg {
    run: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleArg {
    graph: String,
    #[serde(default)]
    rule: Option<Value>,
    #[serde(default)]
    rules: Option<Vec<Value>>,
    #[serde(default = "object")]
    input: Value,
    #[serde(default)]
    remaining: Option<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleUpdateArg {
    id: String,
    graph: String,
    rule: Value,
    #[serde(default = "object")]
    input: Value,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    remaining: Option<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleDeleteArg {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    group: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateArg {
    directory: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    arguments: Option<Value>,
}
fn object() -> Value {
    json!({})
}
fn yes() -> bool {
    true
}
fn args<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, ToolError> {
    serde_json::from_value(value)
        .map_err(|error| rejected(format!("invalid graph-tasks arguments: {error}")))
}

impl GraphTaskTools {
    async fn invoke(&self, name: &str, arguments: Value) -> Result<Value, ToolError> {
        let origin = self
            .application
            .graph_task_origin(&self.key, &self.bindings)
            .map_err(failure)?;
        match name {
            "graph_tasks_list" => {
                let _: std::collections::BTreeMap<String, Value> = args(arguments.clone())?;
                if arguments != json!({}) {
                    return Err(rejected("this tool accepts no parameters"));
                }
                self.application
                    .list_task_graphs(&origin)
                    .await
                    .map_err(failure)
            }
            "graph_tasks_read" => {
                let arg: GraphArg = args(arguments)?;
                self.application
                    .read_task_graph(&origin, &arg.graph)
                    .map_err(failure)
            }
            "graph_tasks_validate" => {
                let arg: DefinitionArg = args(arguments)?;
                let snapshot = self
                    .application
                    .validate_task_graph(&origin, &arg.definition)
                    .map_err(failure)?;
                Ok(
                    json!({"valid":true,"entry":snapshot.entry,"nodes":snapshot.nodes.iter().map(|node|&node.id).collect::<Vec<_>>()}),
                )
            }
            "graph_tasks_create" => {
                let arg: DefinitionArg = args(arguments)?;
                let request = crate::goose_tool_context::operation_request_id(&self.key, name)?;
                self.application
                    .create_task_graph(&origin, arg.definition, request)
                    .await
                    .map_err(failure)
            }
            "graph_tasks_update" => {
                let arg: UpdateArg = args(arguments)?;
                self.application
                    .update_task_graph(&origin, &arg.graph, arg.definition)
                    .await
                    .map_err(failure)
            }
            "graph_tasks_run" => {
                let arg: RunArg = args(arguments)?;
                let request = crate::goose_tool_context::operation_request_id(&self.key, name)?;
                let run = self
                    .application
                    .admit_delegated_graph(
                        &origin,
                        &arg.graph,
                        arg.input,
                        request,
                        Default::default(),
                    )
                    .await
                    .map_err(failure)?;
                Ok(
                    json!({"accepted":true,"run":run,"graph":arg.graph,"result_session":origin.session}),
                )
            }
            "graph_tasks_status" => {
                let arg: StatusArg = args(arguments)?;
                self.application
                    .task_run_status(&origin, &arg.run)
                    .map_err(failure)
            }
            "graph_tasks_schedule_list" => {
                if arguments != json!({}) {
                    return Err(rejected("this tool accepts no parameters"));
                }
                self.application
                    .list_task_schedules(&origin)
                    .map_err(failure)
            }
            "graph_tasks_schedule_create" => {
                let arg: ScheduleArg = args(arguments)?;
                let rules = match (arg.rule, arg.rules) {
                    (Some(rule), None) => vec![rule],
                    (None, Some(rules)) => rules,
                    _ => return Err(rejected("pass exactly one of rule or rules")),
                };
                let request_id = crate::goose_tool_context::operation_request_id(&self.key, name)?;
                self.application
                    .create_task_schedules(
                        &origin,
                        &TaskScheduleRequest {
                            graph: arg.graph,
                            rules,
                            input: arg.input,
                            remaining: arg.remaining,
                            request_id,
                        },
                    )
                    .await
                    .map_err(failure)
            }
            "graph_tasks_schedule_update" => {
                let arg: ScheduleUpdateArg = args(arguments)?;
                self.application
                    .update_task_schedule(
                        &origin,
                        &arg.id,
                        &TaskScheduleUpdate {
                            graph: arg.graph,
                            rule: arg.rule,
                            input: arg.input,
                            enabled: arg.enabled,
                            remaining: arg.remaining,
                        },
                    )
                    .await
                    .map_err(failure)
            }
            "graph_tasks_schedule_delete" => {
                let arg: ScheduleDeleteArg = args(arguments)?;
                match (arg.id, arg.group) {
                    (Some(id), None) => self.application.delete_task_schedule(&origin, &id),
                    (None, Some(group)) => {
                        self.application.delete_task_schedule_group(&origin, &group)
                    }
                    _ => return Err(rejected("pass exactly one schedule id or group")),
                }
                .map_err(failure)
            }
            "graph_tasks_plugin_prepare" | "graph_tasks_plugin_test" => {
                self.application
                    .authorize_task_plugin_development(&origin)
                    .map_err(failure)?;
                let arg: CandidateArg = args(arguments)?;
                let candidate =
                    crate::host_fs::workspace_directory(&self.context.workspace, &arg.directory)?;
                let id = arg
                    .id
                    .as_deref()
                    .or_else(|| candidate.file_name().and_then(|name| name.to_str()))
                    .filter(|id| anchor_runtime::identifier::is_plugin_id(id))
                    .ok_or_else(|| rejected("id must be a Plugin identifier"))?;
                let invocation = match (arg.server.as_deref(), arg.tool.as_deref(), arg.arguments) {
                    (None, None, None) => None,
                    (Some(server), Some(tool), arguments) => {
                        Some((server, tool, arguments.unwrap_or_else(object)))
                    }
                    _ => {
                        return Err(rejected(
                            "pass server and tool together, with optional object arguments",
                        ));
                    }
                };
                let root = self
                    .application
                    .data_root
                    .join("task-development")
                    .join(origin.scope_key())
                    .join("candidates");
                let prepared = self
                    .application
                    .prepare_plugin_candidate(
                        self.application.library_root(),
                        &candidate,
                        id,
                        &root,
                        &self.context.access,
                    )
                    .await
                    .map_err(failure)?;
                if name == "graph_tasks_plugin_prepare" {
                    return Ok(prepared.result(self.context.access.network));
                }
                self.application
                    .test_plugin_candidate(&prepared, &self.context, invocation)
                    .await
                    .map_err(failure)
            }
            _ => Err(ToolError::Unknown(name.into())),
        }
    }
}

impl ToolPort for GraphTaskTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut all = self.inner.definitions();
        all.extend(definitions());
        all
    }
    fn is_read_only(&self, name: &str) -> bool {
        if name.starts_with("graph_tasks_") {
            matches!(
                name,
                "graph_tasks_list"
                    | "graph_tasks_read"
                    | "graph_tasks_validate"
                    | "graph_tasks_status"
                    | "graph_tasks_schedule_list"
            )
        } else {
            self.inner.is_read_only(name)
        }
    }
    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if name.starts_with("graph_tasks_") {
                let cancelled = || rejected("this invocation has been cancelled");
                let mut operation = std::pin::pin!(self.invoke(name, arguments));
                // A lock becoming available must not resume a cancelled Turn's
                // writes, even before the cancellation timer next wakes.
                let guarded = std::future::poll_fn(|cx| {
                    if self.context.access.cancellation.load(Ordering::Acquire) {
                        return std::task::Poll::Ready(Err(cancelled()));
                    }
                    operation.as_mut().poll(cx)
                });
                return tokio::select! {
                    biased;
                    _ = self.context.access.cancelled() => Err(cancelled()),
                    result = guarded => result,
                }
                .map(|value| vec![ToolResultContent::json(value)]);
            }
            self.inner.call(name, arguments).await
        })
    }
}

fn definitions() -> Vec<ToolDefinition> {
    let graph = json!({"type":"string","description":"Exact graph_tasks_list Graph identifier."});
    let input = json!({"type":"object","description":"JSON input for the selected Graph. No author workspace files are implicitly transferred."});
    let definition = json!({"type":"object","description":"Anchor authoring JSON: objective, entry, agents, ops, nodes, edges. Must fit this node's creation ceiling."});
    let rule = json!({"type":"object","description":"Host-local time rule, no timezone offsets: once {type:'once',at:'YYYY-MM-DDTHH:MM:SS'}, interval {type:'interval',seconds:N}, daily {type:'daily',time:'09:00'}, weekly {type:'weekly',weekdays:[0,1,2,3,4],time:'09:00'} (Monday=0), monthly {type:'monthly',day:1,time:'09:00'}. Workdays here mean Mon-Fri, not a holiday calendar."});
    let count = json!({"type":"integer","minimum":1,"description":"Remaining successful Run admissions, not business successes. Omit for unbounded repetition."});
    let candidate = json!({"directory":{"type":"string","description":"Candidate directory relative to /workspace, not a host path."},"id":{"type":"string"},"server":{"type":"string"},"tool":{"type":"string"},"arguments":{"type":"object"}});
    let rows = [
        (
            "graph_tasks_list",
            "List visible Graphs and effective new-Graph/development limits; unavailable assets are not executable.",
            json!({}),
            vec![],
        ),
        (
            "graph_tasks_read",
            "Read a visible Graph contract. Reading does not authorize its execution.",
            json!({"graph":graph}),
            vec!["graph"],
        ),
        (
            "graph_tasks_validate",
            "Validate a proposed new Graph against runtime and authorization limits without publishing it.",
            json!({"definition":definition}),
            vec!["definition"],
        ),
        (
            "graph_tasks_create",
            "Create a normal Graph asset owned by this task scope. The Host assigns its stable identifier. Native tool-call retries return the same asset; no Plugin installation or new grants.",
            json!({"definition":definition}),
            vec!["definition"],
        ),
        (
            "graph_tasks_update",
            "Update only a Graph created by this scope, within the same current capability limit.",
            json!({"graph":graph,"definition":definition}),
            vec!["graph", "definition"],
        ),
        (
            "graph_tasks_run",
            "Start a visible Graph asynchronously. Returns its real Run, not its result. Same native tool-call retries return the same Run. An unfinished Run for that Graph conflicts; do not clone it to bypass exclusion. Assistant-originated terminal results enter this Session for later processing, not automatic sending.",
            json!({"graph":graph,"input":input}),
            vec!["graph"],
        ),
        (
            "graph_tasks_status",
            "Read the actual status and latest committed outputs of a Run delegated by this conversation scope.",
            json!({"run":{"type":"string"}}),
            vec!["run"],
        ),
        (
            "graph_tasks_schedule_list",
            "List only this scope's schedules. Returned Host-local time cursor/count are authoritative; rules use the Host's configured timezone, not a per-schedule timezone.",
            json!({}),
            vec![],
        ),
        (
            "graph_tasks_schedule_create",
            "Persist a scoped schedule. Pass rule OR rules (multiple rules must be once); remaining limits a recurring rule. Busy occurrences remain pending. Returned group cancels a finite set of times together.",
            json!({"graph":graph,"input":input,"rule":rule,"rules":{"type":"array","minItems":1,"items":rule},"remaining":count}),
            vec!["graph"],
        ),
        (
            "graph_tasks_schedule_update",
            "Fully replace or pause this scope's schedule; carry input and remaining from the list when retaining them (omitted remaining means unbounded). An overdue admitted/uncertain occurrence cannot have its content changed; cancel and create a new schedule instead.",
            json!({"id":{"type":"string"},"graph":graph,"rule":rule,"input":input,"enabled":{"type":"boolean"},"remaining":count}),
            vec!["id", "graph", "rule"],
        ),
        (
            "graph_tasks_schedule_delete",
            "Cancel future occurrences of one scope-owned schedule or group. Already admitted Runs are not stopped.",
            json!({"id":{"type":"string"},"group":{"type":"string"}}),
            vec![],
        ),
        (
            "graph_tasks_plugin_prepare",
            "Prepare a scope-owned Plugin candidate from the workspace. Readiness only; never installs, mounts, or grants host credentials.",
            candidate.clone(),
            vec!["directory"],
        ),
        (
            "graph_tasks_plugin_test",
            "Probe candidate MCP startup/discovery, optionally call one server/tool. Runs in the current sandbox/network authority and can have external effects. Passing is not formal installation or proof of business correctness.",
            candidate,
            vec!["directory"],
        ),
    ];
    rows.into_iter().map(|(name,description,properties,required)|ToolDefinition::new(
        ToolName::new(name).expect("static tool name"),description,
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    )).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schemas_do_not_accept_identity_or_installation_and_names_are_unique() {
        let definitions = definitions();
        let names = definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(names.len(), definitions.len());
        assert_eq!(names.len(), 13);
        for definition in definitions {
            let schema = serde_json::to_value(&definition).unwrap();
            let text = schema.to_string();
            assert!(!text.contains("request_id") && !text.contains("oauth_owner"));
        }
        assert!(!names.iter().any(|name| name.contains("install")));
    }
}
