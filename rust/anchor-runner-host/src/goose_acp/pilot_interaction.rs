use crate::application::plugins::{PluginInstallPrecondition, PluginUninstallPrecondition};
use crate::pilot_tools::{PilotTools, workspace_directory};
use anchor_runtime::{Cancellation, ToolDefinition, ToolError, ToolResultContent};
use rmcp::{
    Peer, RoleServer,
    model::{ElicitRequestParams, ElicitationAction},
};
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, time::Duration};
mod channel_management;
mod initialization;

pub(crate) fn definitions() -> Vec<ToolDefinition> {
    let mut definitions=vec![
        ToolDefinition {
            name: "ask_user".into(),
            description: "Ask the user for necessary information using a native form; the current Turn waits for their answer. Decline/cancel is not consent.".into(),
            parameters: json!({"type":"object","properties":{"message":{"type":"string"},"requested_schema":{"type":"object"}},"required":["message","requested_schema"],"additionalProperties":false}),
        },
        ToolDefinition {
            name: "graph_delete".into(),
            description: "Delete the saved Graph and its retained Runs/workspaces only after a fresh native user confirmation of its exact current contents. Never infer confirmation from a chat message.".into(),
            parameters: json!({"type":"object","properties":{"graph":{"type":"string"}},"required":["graph"],"additionalProperties":false}),
        },
        ToolDefinition {
            name: "plugin_uninstall".into(),
            description: "Uninstall a Plugin after a fresh native user confirmation of the Plugin, its declared mutable paths and their size. Never infer confirmation from a chat message.".into(),
            parameters: json!({"type":"object","properties":{"plugin":{"type":"string"}},"required":["plugin"],"additionalProperties":false}),
        },
        ToolDefinition {
            name: "plugin_install".into(),
            description: "Install a Plugin candidate you prepared inside your own workspace into the Anchor Library. The user approves the exact reviewed content in a native form; approval is never a tool argument. Fetch, adapt, build and self-test the candidate first, then review it.".into(),
            parameters: json!({"type":"object","properties":{"directory":{"type":"string"},"id":{"type":"string"}},"required":["directory"],"additionalProperties":false}),
        },
    ];
    definitions.extend(
        crate::pilot_tools::PILOT_TOOLS
            .iter()
            .filter(|tool| {
                matches!(
                    tool.name,
                    "session_network"
                        | "plugin_mount"
                        | "channel_configure"
                        | "channel_enable"
                        | "run_abandon"
                )
            })
            .map(|tool| ToolDefinition {
                name: tool.name.into(),
                description: tool.description.into(),
                parameters: (tool.parameters)(),
            }),
    );
    definitions
}

fn failure(message: impl Into<String>) -> ToolError {
    ToolError::Failed(message.into())
}

/// Only a configured assistant Graph can depend on Library Plugins for its
/// instructions. The platform Pilot's built-in instructions have no Graph.
fn refuse_instruction_plugin(
    tools: &PilotTools,
    plugin: &str,
    operation: &str,
) -> Result<(), ToolError> {
    if tools.graph_name.is_empty() {
        return Ok(());
    }
    let (_path, bundle) = tools
        .application
        .load_graph(&tools.graph_name)
        .map_err(|error| failure(format!("the assistant Graph is unavailable: {error:?}")))?;
    if bundle.plugins.iter().any(|id| id == plugin) {
        return Err(failure(format!(
            "`{plugin}` provides this assistant's own instructions; ask the operator to {operation} it through the operator interface"
        )));
    }
    Ok(())
}

/// The assistant needs the concrete reason (for example a catalog field it has
/// to fix), so surface the application's own message rather than a Debug form.
fn application_reason(error: crate::application::ApplicationError) -> String {
    use crate::application::ApplicationError;
    match error {
        ApplicationError::Missing => "not found".into(),
        ApplicationError::Conflict(reason)
        | ApplicationError::Invalid(reason)
        | ApplicationError::Storage(reason) => reason,
    }
}

fn active(tools: &PilotTools, cancellation: &Cancellation) -> Result<(), ToolError> {
    if cancellation.load(Ordering::Acquire)
        || tools
            .sessions
            .get_turn(&tools.owner, &tools.session, &tools.turn)
            .map_err(|_| failure("Pilot Turn is unavailable"))?
            .status
            != anchor_platform_session::TurnStatus::Running
    {
        return Err(failure("Pilot Turn is not active"));
    }
    Ok(())
}

fn native_schema(schema: Value) -> Result<rmcp::model::ElicitationSchema, ToolError> {
    anchor_platform_session::validate_question_schema(&schema)
        .map_err(|error| failure(error.to_string()))?;
    let requested_schema: rmcp::model::ElicitationSchema =
        serde_json::from_value(schema.clone())
            .map_err(|_| failure("unsupported native form schema"))?;
    let serialized = serde_json::to_value(&requested_schema)
        .map_err(|_| failure("native form schema serialization failed"))?;
    let mut expected = schema;
    expected
        .as_object_mut()
        .unwrap()
        .remove("additionalProperties");
    if serialized != expected {
        return Err(failure(
            "native MCP form schema cannot preserve the requested constraints",
        ));
    }
    Ok(requested_schema)
}

pub(super) async fn call(
    tools: &PilotTools,
    name: &str,
    arguments: Value,
    peer: Peer<RoleServer>,
    cancellation: &Cancellation,
) -> Result<Vec<ToolResultContent>, ToolError> {
    active(tools, cancellation)?;
    let arguments = arguments
        .as_object()
        .ok_or_else(|| failure("tool arguments must be an object"))?;
    if matches!(name, "session_network" | "plugin_mount") {
        return initialization::policy(tools, name, arguments, peer, cancellation).await;
    }
    if matches!(name, "channel_configure" | "channel_enable") {
        return channel_management::policy(tools, name, arguments, peer, cancellation).await;
    }
    if name == "plugin_install" {
        return install(tools, arguments, peer, cancellation).await;
    }
    if name == "plugin_uninstall" {
        return uninstall(tools, arguments, peer, cancellation).await;
    }
    if name == "run_abandon" {
        return abandon(tools, arguments, peer, cancellation).await;
    }
    if !matches!(name, "ask_user" | "graph_delete") {
        // Falling through to ask_user would show a generic form the model fills
        // in, which is exactly what a confirmed tool must never do.
        return Err(failure(format!(
            "`{name}` is declared as a confirmed tool but has no native confirmation implementation"
        )));
    }
    let (message, schema, deletion) = if name == "graph_delete" {
        if arguments.len() != 1 {
            return Err(failure(
                "graph_delete accepts only graph; confirmation cannot be supplied by the model",
            ));
        }
        let graph = arguments
            .get("graph")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("graph must be a string"))?;
        // The assistant may not delete the Graph that defines it.
        tools.refuse_self_graph(graph)?;
        let expected = tools
            .application
            .graph_delete_precondition(graph)
            .await
            .map_err(|error| failure(format!("Graph deletion rejected: {error}")))?;
        (
            format!(
                "确认删除 Graph「{graph}」及其保留的 Run、workspace 和产物？此操作不可撤销。确认只适用于当前内容：{expected}"
            ),
            json!({"type":"object","properties":{"confirm":{"type":"boolean","title":"确认删除此 Graph 及其运行数据","default":false}},"required":["confirm"],"additionalProperties":false}),
            Some((graph.to_owned(), expected)),
        )
    } else {
        if arguments.len() != 2 {
            return Err(failure(
                "ask_user accepts only message and requested_schema",
            ));
        }
        let message = arguments
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| !message.trim().is_empty() && message.len() <= 65536)
            .ok_or_else(|| failure("message must be nonblank and bounded"))?;
        let schema = arguments
            .get("requested_schema")
            .ok_or_else(|| failure("requested_schema is required"))?;
        (message.to_owned(), schema.clone(), None)
    };
    let requested_schema = native_schema(schema)?;
    let elicitation = peer.create_elicitation(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message,
        requested_schema,
    });
    tokio::pin!(elicitation);
    let result = tokio::select! {
        result = &mut elicitation => result.map_err(|_| failure("native user question could not complete"))?,
        _ = async {
            while !cancellation.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } => return Err(failure("Pilot was cancelled while waiting for the user")),
    };
    active(tools, cancellation)?;
    let value = if let Some((graph, expected)) = deletion {
        if result.action != ElicitationAction::Accept
            || result
                .content
                .as_ref()
                .and_then(|content| content.get("confirm"))
                != Some(&Value::Bool(true))
        {
            json!({"deleted":false,"graph":graph,"action":result.action})
        } else {
            let removed = tools
                .application
                .remove_graph_if_unchanged_guarded(&graph, &tools.workspace_root, &expected, || {
                    active(tools, cancellation).map_err(|error| {
                        crate::application::graphs::GraphManagementError::Conflict(
                            error.to_string(),
                        )
                    })
                })
                .await
                .map_err(|error| {
                    failure(format!(
                        "Graph deletion rejected; request a fresh confirmation: {error}"
                    ))
                })?;
            json!({"deleted":true,"graph":graph,"removed_runs":removed,"precondition":expected})
        }
    } else {
        serde_json::to_value(result).map_err(|_| failure("native answer serialization failed"))?
    };
    Ok(vec![ToolResultContent::text(value.to_string())])
}

/// Native confirmation binds one immutable Run identity, never a model-supplied approval.
fn abandon_target(arguments: &serde_json::Map<String, Value>) -> Result<&str, ToolError> {
    if arguments.len() != 1 {
        return Err(failure(
            "run_abandon accepts only run; confirmation cannot be supplied by the model",
        ));
    }
    arguments
        .get("run")
        .and_then(Value::as_str)
        .filter(|run| anchor_runtime::identifier::is_name(run))
        .ok_or_else(|| failure("run must be a valid Run ID"))
}

async fn apply_abandon_answer(
    tools: &PilotTools,
    run: &str,
    digest: &str,
    accepted: bool,
    confirm: bool,
    cancellation: &Cancellation,
) -> Result<Value, ToolError> {
    active(tools, cancellation)?;
    if !accepted || !confirm {
        return Ok(json!({"abandoned":false,"run":run}));
    }
    let current = tools
        .application
        .pilot_run_status(run)
        .await
        .map_err(|error| failure(application_reason(error)))?;
    if current["record"]["graph_digest"].as_str() != Some(digest) {
        return Err(failure(
            "Run identity changed; request a fresh abandon confirmation",
        ));
    }
    let outcome = tools
        .application
        .abandon_run(run, None)
        .await
        .map_err(|error| {
            failure(format!(
                "Run abandon rejected: {}",
                application_reason(error)
            ))
        })?;
    Ok(
        json!({"abandoned":true,"run":run,"action":outcome.action.as_str(),
        "reason":outcome.reason,"requested_at":outcome.requested_at}),
    )
}

async fn abandon(
    tools: &PilotTools,
    arguments: &serde_json::Map<String, Value>,
    peer: Peer<RoleServer>,
    cancellation: &Cancellation,
) -> Result<Vec<ToolResultContent>, ToolError> {
    let run = abandon_target(arguments)?;
    let status = tools
        .application
        .pilot_run_status(run)
        .await
        .map_err(|error| failure(application_reason(error)))?;
    if status["projection"]["actions"]["abandon"] != true {
        return Err(failure("Run cannot be abandoned; inspect run_status"));
    }
    let digest = status["record"]["graph_digest"]
        .as_str()
        .ok_or_else(|| failure("Run identity unavailable"))?;
    let form = peer.create_elicitation(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: format!("确认永久结束 Run「{run}」（Graph 摘要 {digest}）？将停止执行并标记为 aborted，之后不能恢复。历史、产物和 workspace 保留，不会删除。"),
        requested_schema: native_schema(json!({"type":"object","properties":{"confirm":{"type":"boolean","title":"确认永久结束此 Run（不可恢复）","default":false}},"required":["confirm"],"additionalProperties":false}))?,
    });
    tokio::pin!(form);
    let answer = tokio::select! {
        result = &mut form => result.map_err(|_| failure("native user confirmation could not complete"))?,
        _ = async {
            while !cancellation.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } => return Err(failure("Pilot was cancelled while waiting for the user")),
    };
    let value = apply_abandon_answer(
        tools,
        run,
        digest,
        answer.action == ElicitationAction::Accept,
        checked(answer.content.as_ref(), "confirm"),
        cancellation,
    )
    .await?;
    Ok(vec![ToolResultContent::text(value.to_string())])
}

/// The user's answer to the uninstall form, as data.
///
/// Split from the elicitation so the decision can be tested without a native
/// form: the tool path passes what the form returned, and the tests pass what a
/// human would have clicked.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct UninstallAnswer {
    /// The elicitation was accepted at all.
    pub(crate) accepted: bool,
    /// "确认卸载"
    pub(crate) confirm: bool,
    /// "确认删除这个插件唯一的一份数据（不可恢复）" — without this nothing is deleted.
    pub(crate) confirm_data_loss: bool,
}

impl UninstallAnswer {
    fn authorized(&self) -> bool {
        self.accepted && self.confirm && self.confirm_data_loss
    }
}

/// One checkbox of a native form answer.
fn checked(content: Option<&Value>, field: &str) -> bool {
    content.and_then(|content| content.get(field)) == Some(&Value::Bool(true))
}

/// The form the human sees, and the facts it must state.
///
/// Pure, so its content is testable. The Plugin has exactly one copy in the
/// world, so the form says plainly that deleting it deletes the data, lists what
/// the Plugin itself declared as mutable with the bytes each path holds, and
/// requires an explicit box for the data.
pub(crate) fn uninstall_form(
    precondition: &crate::application::plugins::PluginUninstallPrecondition,
) -> (String, Value) {
    let mut message = format!(
        "确认卸载插件「{} ({})」？\n内容指纹：{}\n",
        precondition.name, precondition.id, precondition.digest
    );
    if precondition.mutable.is_empty() {
        message.push_str("该插件没有声明可变路径（mutable）：它只有代码。\n");
    } else {
        message.push_str("这个插件自己会改写、因此不会被平台替换的路径（mutable）与当前体积：\n");
        for (path, size) in &precondition.mutable {
            message.push_str(&format!(
                "- {path}：{}\n",
                match size {
                    Some(bytes) => format!("{bytes} 字节"),
                    None => "体积未知（无法读取）".to_owned(),
                }
            ));
        }
    }
    message.push_str(
        "**这会删掉这个插件唯一的一份数据，不可恢复**：插件在库里只有一份，平台不留历史版本，也没有第二个副本。\n",
    );
    if precondition.graphs.is_empty() {
        message.push_str("当前没有 Graph 引用这个插件。\n");
    } else {
        message.push_str(&format!(
            "当前 {} 张图引用它（{}）；卸载后这些图的下一次接纳会失败。\n",
            precondition.graphs.len(),
            precondition.graphs.join(", ")
        ));
    }
    render_coverage(&mut message, &precondition.coverage);
    let schema = json!({
        "type":"object",
        "properties":{
            "confirm":{"type":"boolean","title":format!("确认卸载插件 {}", precondition.id),"default":false},
            "confirm_data_loss":{"type":"boolean","title":"确认删除这个插件唯一的一份数据（不可恢复）","default":false}
        },
        "required":["confirm","confirm_data_loss"],
        "additionalProperties":false
    });
    (message, schema)
}

/// The uninstall, with the user's answer already resolved.
///
/// Split from the elicitation so the preconditions, the deletion, the audit fact
/// and the refusal are all reachable from a test. The original form's state is
/// passed through unchanged; the application rechecks current facts under its
/// publication guards before deleting anything.
pub(crate) async fn uninstall_with_answer(
    tools: &PilotTools,
    precondition: &PluginUninstallPrecondition,
    answer: UninstallAnswer,
    cancellation: &Cancellation,
) -> Result<Value, ToolError> {
    let plugin = &precondition.id;
    if !anchor_runtime::identifier::is_plugin_id(plugin) {
        return Err(failure("plugin must be a Plugin identifier"));
    }
    // Uninstalling the Plugin that provides this assistant's own instructions is
    // self-modification; the operator interface owns that.
    refuse_instruction_plugin(tools, plugin, "uninstall")?;
    if let Some(refusal) = precondition.refusal() {
        return Err(failure(format!(
            "Plugin `{plugin}` cannot be uninstalled: {refusal}"
        )));
    }
    if !answer.authorized() {
        // Nothing was deleted, and the reason says which box was missing. This is
        // the fail-closed half of "只有勾选才删数据": the safe outcome is "no
        // uninstall", because there is no uninstall that keeps the data.
        return Ok(json!({
            "uninstalled":false,
            "plugin":plugin,
            "reason":"没有勾选「确认删除这个插件唯一的一份数据」：未卸载任何内容，插件目录与数据原样保留"
        }));
    }
    let outcome = tools
        .application
        .uninstall_plugin_if_unchanged_with_access(
            &tools.library_root,
            precondition,
            &tools.sessions,
            &crate::dependency_provision::DependencyAccess {
                network: false,
                cancellation: cancellation.clone(),
            },
        )
        .await
        .map_err(|error| {
            failure(format!(
                "Plugin uninstall was rejected: {}",
                application_reason(error)
            ))
        })?;
    // The audit fact: who, which Plugin, that its data went with it, and what was
    // freed. It lives on the Session that asked, which is where this host keeps
    // its durable, owner-scoped audit trail.
    let mut audit = std::collections::BTreeMap::new();
    audit.insert("plugin".into(), json!(outcome.id));
    audit.insert("name".into(), json!(precondition.name));
    audit.insert("digest".into(), json!(precondition.digest));
    audit.insert("actor".into(), json!(tools.owner));
    audit.insert("turn".into(), json!(tools.turn));
    audit.insert("data_removed".into(), json!(true));
    audit.insert("removed_bytes".into(), json!(outcome.removed_bytes));
    audit.insert("removed_views".into(), json!(outcome.removed_views));
    audit.insert(
        "reclaimed_package_versions".into(),
        json!(outcome.reclaimed_package_versions),
    );
    audit.insert("reclaimed_bytes".into(), json!(outcome.reclaimed_bytes));
    if let Some(reason) = &outcome.reclamation {
        audit.insert("reclamation_error".into(), json!(reason));
    }
    tools
        .sessions
        .append_event(&tools.owner, &tools.session, "plugin.uninstalled", audit)
        .map_err(|_| {
            failure("the Plugin was uninstalled, but its audit fact could not be recorded")
        })?;
    Ok(json!({
        "uninstalled":true,
        "plugin":outcome.id,
        "data_removed":true,
        "removed_bytes":outcome.removed_bytes,
        "removed_views":outcome.removed_views,
        "reclaimed_package_versions":outcome.reclaimed_package_versions,
        "reclaimed_bytes":outcome.reclaimed_bytes,
        "reclamation_error":outcome.reclamation,
    }))
}

/// Confirm and uninstall one Plugin.
async fn uninstall(
    tools: &PilotTools,
    arguments: &serde_json::Map<String, Value>,
    peer: Peer<RoleServer>,
    cancellation: &Cancellation,
) -> Result<Vec<ToolResultContent>, ToolError> {
    // Only the target is a model argument: approval is not.
    if let Some(key) = arguments.keys().find(|key| key.as_str() != "plugin") {
        return Err(failure(format!(
            "plugin_uninstall does not accept `{key}`; the user approves the deletion, not the model"
        )));
    }
    let plugin = arguments
        .get("plugin")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("plugin must be a string"))?
        .to_owned();
    // The form is built from the current facts, and a refusal means no form at all.
    let precondition = tools
        .application
        .plugin_uninstall_precondition(&tools.library_root, &plugin, &tools.sessions)
        .await
        .map_err(|error| {
            failure(format!(
                "Plugin uninstall was rejected: {}",
                application_reason(error)
            ))
        })?;
    if let Some(refusal) = precondition.refusal() {
        return Err(failure(format!(
            "Plugin `{plugin}` cannot be uninstalled: {refusal}"
        )));
    }
    let (message, schema) = uninstall_form(&precondition);
    let requested_schema = native_schema(schema)?;
    let elicitation = peer.create_elicitation(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message,
        requested_schema,
    });
    tokio::pin!(elicitation);
    let result = tokio::select! {
        result = &mut elicitation => result.map_err(|_| failure("native user confirmation could not complete"))?,
        _ = async {
            while !cancellation.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } => return Err(failure("Pilot was cancelled while waiting for the user")),
    };
    active(tools, cancellation)?;
    let answer = UninstallAnswer {
        accepted: result.action == ElicitationAction::Accept,
        confirm: checked(result.content.as_ref(), "confirm"),
        confirm_data_loss: checked(result.content.as_ref(), "confirm_data_loss"),
    };
    let operation_cancellation = tools
        .context
        .as_ref()
        .map(|context| &context.cancellation)
        .unwrap_or(cancellation);
    let value = uninstall_with_answer(tools, &precondition, answer, operation_cancellation).await?;
    Ok(vec![ToolResultContent::text(value.to_string())])
}

/// Only the candidate location is a model argument: approval is not.
fn install_arguments(
    arguments: &serde_json::Map<String, Value>,
) -> Result<(String, Option<String>), ToolError> {
    if let Some(key) = arguments
        .keys()
        .find(|key| !matches!(key.as_str(), "directory" | "id"))
    {
        return Err(failure(format!(
            "plugin_install does not accept `{key}`; the user approves the installation, not the model"
        )));
    }
    let directory = arguments
        .get("directory")
        .and_then(Value::as_str)
        .filter(|directory| !directory.trim().is_empty())
        .ok_or_else(|| failure("directory must be a nonempty workspace-relative path"))?
        .to_owned();
    let id = match arguments.get("id").and_then(Value::as_str) {
        Some(id) if anchor_runtime::identifier::is_plugin_id(id) => Some(id.to_owned()),
        Some(_) => return Err(failure("id must be a Plugin identifier")),
        None => None,
    };
    Ok((directory, id))
}

/// Say how complete an impact report was, whatever it was about.
///
/// A count may only be read as "this is everything" when the scan really saw
/// everything, so a partial or unknown scan says so instead of showing a
/// smaller number.
fn render_coverage(message: &mut String, coverage: &crate::application::graphs::ImpactCoverage) {
    use crate::application::graphs::ImpactCoverage;
    match coverage {
        ImpactCoverage::Complete => {}
        ImpactCoverage::Partial {
            considered,
            total,
            missing,
        } => {
            message.push_str(&format!(
                "影响范围只能部分确认（已考察 {considered}/{total} 个图）：{}。下面这个数字不是全部。\n",
                missing
                    .iter()
                    .map(|missing| format!("{}（{}）", missing.graph, missing.reason))
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        }
        ImpactCoverage::Unknown { reason } => {
            message.push_str(&format!("影响范围未知：{reason}（不能读成 0 个引用）。\n"))
        }
    }
}

fn needs_view_repair(
    already_installed: bool,
    dependencies: &anchor_graph_host::PluginDependencies,
    library_root: &std::path::Path,
    id: &str,
) -> bool {
    already_installed
        && !dependencies.is_empty()
        && crate::plugin_view::view_needs_repair(library_root, id)
}

fn install_message(precondition: &PluginInstallPrecondition) -> String {
    let review = &precondition.review;
    let mut message = format!(
        "确认把插件「{}」安装进 Anchor 插件库？\n内容指纹：{}\n",
        review.id, review.digest
    );
    message.push_str(
        if precondition.already_installed() && precondition.dependencies.is_some() {
            "该 id 已安装且内容指纹相同；本次不会改写插件，只补建它声明的依赖视图。\n"
        } else if precondition.replacing() {
            "该 id 已安装，本次会覆盖现有版本。\n"
        } else {
            "这是新建，不会覆盖任何现有插件。\n"
        },
    );
    // Which Graphs bind the Plugin is still worth showing; what it does to Runs
    // is not a question any more. Upgrading takes effect from each Run's next node
    // call and strands nothing, so there is nothing to confirm and nothing that
    // will refuse to continue.
    let impact = &precondition.impact;
    if impact.graphs.is_empty() {
        message.push_str("当前没有 Graph 绑定这个插件。\n");
    } else {
        message.push_str(&format!(
            "当前依赖这个插件：{} 张图（{}）\n",
            impact.graphs.len(),
            impact.graphs.join(", ")
        ));
    }
    if precondition.replacing() && !precondition.already_installed() {
        message.push_str(
            "升级从每个正在跑的 Run 的**下一次节点调用**起生效；已完成的步骤不回滚、不重放。\n",
        );
    }
    // Outside the branch on purpose: an empty count beside a partial scan would
    // otherwise read as "nothing depends on it" when a Graph could not be read.
    render_coverage(&mut message, &impact.coverage);
    // A view placeholder stands for a deployment path, and the human is being
    // asked to approve that path: show the interpreter the server will run on,
    // and the packages the declaration resolves to.
    let view = precondition
        .dependencies
        .as_ref()
        .map(|plan| plan.view.clone());
    let shown = |value: &str| match view.as_ref() {
        Some(view) => value.replace("${ANCHOR_PLUGIN_VIEW}", &view.to_string_lossy()),
        None => value.to_owned(),
    };
    if let Some(plan) = &precondition.dependencies {
        message.push_str(&format!(
            "该插件声明了 Python 依赖，宿主按部署授权的来源解析并安装：\n解释器：{}（{}）\n来源：{}\n视图：{}\n",
            plan.interpreter,
            plan.interpreter_path.display(),
            if plan.sources.is_empty() {
                "部署未列出来源".to_owned()
            } else {
                plan.sources.join(", ")
            },
            plan.view.display(),
        ));
        for package in &plan.packages {
            message.push_str(&format!("  - {}=={}\n", package.name, package.version));
        }
    }
    for server in &review.servers {
        match (&server.command, &server.url) {
            (Some(command), _) if server.args.is_empty() => {
                message.push_str(&format!("会运行：{}（{}）\n", shown(command), server.name));
            }
            (Some(command), _) => {
                message.push_str(&format!(
                    "会运行：{} {}（{}）\n",
                    shown(command),
                    server
                        .args
                        .iter()
                        .map(|argument| shown(argument))
                        .collect::<Vec<_>>()
                        .join(" "),
                    server.name
                ));
            }
            (_, Some(url)) => {
                message.push_str(&format!("会连接：{url}（{}）\n", server.name));
            }
            _ => message.push_str(&format!("声明了 server {}（无命令与地址）\n", server.name)),
        }
    }
    let mut names = review
        .servers
        .iter()
        .flat_map(|server| server.environment.iter().map(String::as_str))
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    if !names.is_empty() {
        message.push_str(&format!(
            "需要的环境变量（只列名字，不含值）：{}\n",
            names.join(", ")
        ));
    }
    for warning in &review.warnings {
        message.push_str(&format!("注意：{warning}\n"));
    }
    if !review.unsupported.is_empty() {
        message.push_str(&format!(
            "清单还声明了不会被执行的：{}\n",
            review.unsupported.join(", ")
        ));
    }
    message.chars().take(16_000).collect()
}

async fn install(
    tools: &PilotTools,
    arguments: &serde_json::Map<String, Value>,
    peer: Peer<RoleServer>,
    cancellation: &Cancellation,
) -> Result<Vec<ToolResultContent>, ToolError> {
    let (directory, id) = install_arguments(arguments)?;
    let candidate = workspace_directory(&tools.task_workspace(), &directory)?;
    let id = match id {
        Some(id) => id,
        None => candidate
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| anchor_runtime::identifier::is_plugin_id(name))
            .ok_or_else(|| failure("the directory name is not a usable Plugin id; pass `id`"))?
            .to_owned(),
    };
    // Installing a Plugin that provides this assistant's own instructions would
    // let the assistant change itself; the operator interface owns that.
    refuse_instruction_plugin(tools, &id, "install")?;
    let precondition = tools
        .application
        .plugin_install_precondition_with_access(
            tools.library_root.clone(),
            candidate,
            id,
            tools
                .context
                .as_ref()
                .ok_or_else(|| failure("Pilot Turn context unavailable"))?
                .dependency_access(),
        )
        .await
        .map_err(|error| {
            failure(format!(
                "Plugin candidate was rejected: {}",
                application_reason(error)
            ))
        })?;
    // An already-published tree normally means there is nothing to do. The one
    // exception is a dependency view that is missing or unusable: that is exactly
    // the state a repair has to go through, so it is not reported as "up to date".
    let repairing = needs_view_repair(
        precondition.already_installed(),
        &precondition.review.dependencies,
        &tools.library_root,
        &precondition.review.id,
    );
    if precondition.already_installed() && !repairing {
        return Ok(vec![ToolResultContent::text(
            json!({"installed":false,"already_installed":true,"id":precondition.review.id,"digest":precondition.review.digest})
                .to_string(),
        )]);
    }
    // Repairing a view rewrites no Plugin bytes, so it is not a replacement and
    // must not be confirmed as one.
    let replacing = precondition.replacing() && !precondition.already_installed();
    let mut properties = serde_json::Map::new();
    properties.insert(
        "confirm".into(),
        json!({"type":"boolean","title":"确认安装这个插件","default":false}),
    );
    if replacing {
        properties.insert(
            "replace_existing".into(),
            json!({"type":"boolean","title":format!("确认覆盖已安装的 {}", precondition.review.id),"default":false}),
        );
    }
    if precondition.review.binary_without_source {
        properties.insert(
            "accept_unreadable".into(),
            json!({"type":"boolean","title":"我接受这些无法阅读源码的编译产物","default":false}),
        );
    }

    let required = properties.keys().cloned().collect::<Vec<_>>();
    let schema = json!({
        "type":"object",
        "properties":Value::Object(properties),
        "required":required,
        "additionalProperties":false
    });
    let requested_schema = native_schema(schema)?;
    let elicitation = peer.create_elicitation(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: install_message(&precondition),
        requested_schema,
    });
    tokio::pin!(elicitation);
    let result = tokio::select! {
        result = &mut elicitation => result.map_err(|_| failure("native user confirmation could not complete"))?,
        _ = async {
            while !cancellation.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } => return Err(failure("Pilot was cancelled while waiting for the user")),
    };
    active(tools, cancellation)?;
    let approved = |field: &str| {
        result
            .content
            .as_ref()
            .and_then(|content| content.get(field))
            == Some(&Value::Bool(true))
    };
    let accepted = result.action == ElicitationAction::Accept
        && approved("confirm")
        && (!replacing || approved("replace_existing"))
        && (!precondition.review.binary_without_source || approved("accept_unreadable"));
    if !accepted {
        return Ok(vec![ToolResultContent::text(
            json!({"installed":false,"id":precondition.review.id,"action":result.action})
                .to_string(),
        )]);
    }
    let outcome = tools
        .application
        .install_plugin_if_unchanged(tools.library_root.clone(), precondition.clone())
        .await
        .map_err(|error| {
            failure(format!(
                "Plugin installation rejected: {}",
                application_reason(error)
            ))
        })?;
    // The answer names what the install produced, including the view and the
    // packages it landed, so an operator can see the solved set without reading
    // the host's filesystem.
    let mut result = json!({
        "installed":true,
        "id":outcome.id,
        "digest":outcome.digest,
        "replaced":precondition.replacing()
    });
    if let Some(plan) = &precondition.dependencies {
        result["dependencies"] = json!({
            "view": plan.view,
            "declaration_digest": plan.declaration_digest,
            "interpreter": plan.interpreter,
            "lock_digest": plan.lock_digest,
            "packages": plan
                .packages
                .iter()
                .map(|package| json!({"name":package.name,"version":package.version}))
                .collect::<Vec<_>>(),
        });
    }
    if let Some(prepared) = crate::application::plugin_candidates::load_fact(
        &tools.preparation_root()?,
        &precondition.review.id,
        &precondition.review.digest,
    )
    .map_err(|error| {
        failure(format!(
            "Plugin installed but preparation comparison is unavailable: {error:?}"
        ))
    })? {
        result["prepared_lock_digest"] = json!(
            prepared
                .resolved
                .as_ref()
                .map(|resolved| &resolved.lock_digest)
        );
        result["prepared_dependency_set_matches"] = json!(
            prepared
                .resolved
                .as_ref()
                .map(|resolved| &resolved.packages)
                == precondition
                    .resolved
                    .as_ref()
                    .map(|resolved| &resolved.packages)
        );
    }
    Ok(vec![ToolResultContent::text(result.to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The form shows what will actually run: a dependency view is a real path,
    /// not the placeholder the manifest declares, and the packages the
    /// declaration resolved to are named before anybody approves them.
    #[test]
    fn the_install_form_expands_the_dependency_view_for_the_human() {
        use crate::application::graphs::ImpactCoverage;
        use crate::application::plugins::{PluginDependencyPlan, PluginImpact};
        use crate::dependency_provision::LockedPackage;

        let root = tempfile::tempdir().unwrap();
        let candidate = root.path().join("candidate");
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::write(
            candidate.join("plugin.json"),
            json!({
                "name": "Probe",
                "dependencies": {"python": ["idna==3.6"]},
                "mcpServers": {
                    "probe": {"command": "${ANCHOR_PLUGIN_VIEW}/bin/python", "args": ["server.py"]}
                }
            })
            .to_string(),
        )
        .unwrap();
        let review = crate::plugin_library::PluginLibrary::new(root.path().join("library"))
            .review_directory("probe", &candidate)
            .unwrap();
        let view = root.path().join("library/dependencies/views/probe/abc123");
        let precondition = PluginInstallPrecondition {
            review,
            authorization_digest: "a".repeat(64),
            network: false,
            mutable: Vec::new(),
            directory: candidate,
            existing_digest: None,
            impact: PluginImpact {
                graphs: Vec::new(),
                coverage: ImpactCoverage::Complete,
            },
            dependencies: Some(PluginDependencyPlan {
                sources: vec!["directory:/opt/wheels".into()],
                interpreter: "3.12.13".into(),
                interpreter_path: std::path::PathBuf::from("/usr/bin/python3.12"),
                interpreter_digest: "f".repeat(64),
                lock_digest: "a".repeat(64),
                packages: vec![LockedPackage {
                    name: "idna".into(),
                    version: "3.6".into(),
                    hashes: vec!["b".repeat(64)],
                }],
                view: view.clone(),
                declaration_digest: "c".repeat(64),
            }),
            resolved: None,
            access: crate::dependency_provision::DependencyAccess::default(),
        };

        let message = install_message(&precondition);
        assert!(
            message.contains(&view.join("bin/python").to_string_lossy().into_owned()),
            "the form must show the interpreter the server will run on: {message}"
        );
        assert!(
            !message.contains("ANCHOR_PLUGIN_VIEW"),
            "the placeholder must not reach the human: {message}"
        );
        assert!(message.contains("idna==3.6"), "{message}");
        assert!(message.contains("directory:/opt/wheels"), "{message}");
    }

    /// An already-installed Plugin whose view is gone is not "up to date": the
    /// confirmation gate is the only installation entry point, so it is the one
    /// that has to offer the repair.
    #[test]
    fn an_already_installed_plugin_without_its_view_is_repair_work() {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("library");
        let plugin = library.join("plugins/probe");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            json!({
                "name": "Probe",
                "dependencies": {"python": ["idna==3.6"]},
                "mcpServers": {"probe": {"command": "${ANCHOR_PLUGIN_VIEW}/bin/python"}}
            })
            .to_string(),
        )
        .unwrap();
        let dependencies = crate::plugin_view::prepare_for_plugin(&library, "probe");
        assert!(
            dependencies.is_err(),
            "no view exists yet: {dependencies:?}"
        );
        assert!(needs_view_repair(
            true,
            &anchor_graph_host::PluginDependencies {
                python: vec!["idna==3.6".to_owned()]
            },
            &library,
            "probe"
        ));

        // A Plugin that declares nothing needs no repair, and neither does one
        // whose view is complete.
        assert!(!needs_view_repair(
            true,
            &anchor_graph_host::PluginDependencies::default(),
            &library,
            "probe"
        ));
        assert!(!needs_view_repair(
            false,
            &Default::default(),
            &library,
            "probe"
        ));

        // A complete view: the file the standard requires, an interpreter, and a
        // pointer into the shared store.
        let store = anchor_graph_host::store_root(&library.canonicalize().unwrap());
        std::fs::create_dir_all(store.join("idna/3.6/idna")).unwrap();
        let view = anchor_graph_host::plugin_view_path(
            &library.canonicalize().unwrap(),
            "probe",
            &anchor_graph_host::PluginDependencies {
                python: vec!["idna==3.6".to_owned()],
            }
            .declaration_digest(),
        );
        let site = view.join("lib/python3.12/site-packages");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::create_dir_all(view.join("bin")).unwrap();
        std::fs::write(view.join("pyvenv.cfg"), "home = /usr/bin").unwrap();
        std::os::unix::fs::symlink("/bin/sh", view.join("bin/python")).unwrap();
        std::os::unix::fs::symlink(store.join("idna/3.6/idna"), site.join("idna")).unwrap();
        assert!(!needs_view_repair(
            true,
            &anchor_graph_host::PluginDependencies {
                python: vec!["idna==3.6".to_owned()]
            },
            &library,
            "probe"
        ));
    }

    #[test]
    fn native_form_cannot_silently_drop_schema_constraints() {
        assert!(native_schema(json!({"type":"object","properties":{"confirm":{"type":"boolean","default":false}},"required":["confirm"],"additionalProperties":false})).is_ok());
        for schema in [
            json!({"type":"object","properties":{"value":{"type":"string","pattern":"^yes$"}}}),
            json!({"type":"object","properties":{"value":{"type":"boolean","enum":[true]}}}),
            json!({"type":"object","properties":{"value":{"type":"integer","multipleOf":2}}}),
            json!({"type":"object","properties":{"value":{"type":"string"}},"minProperties":1}),
        ] {
            assert!(native_schema(schema).is_err());
        }
    }

    #[test]
    fn install_arguments_refuse_any_model_supplied_approval() {
        let arguments = |value: Value| {
            value
                .as_object()
                .cloned()
                .expect("test arguments are objects")
        };
        assert_eq!(
            install_arguments(&arguments(json!({"directory":"candidate"}))).unwrap(),
            ("candidate".to_owned(), None)
        );
        assert_eq!(
            install_arguments(&arguments(json!({"directory":"candidate","id":"demo"}))).unwrap(),
            ("candidate".to_owned(), Some("demo".to_owned()))
        );
        for refused in [
            json!({"directory":"candidate","confirm":true}),
            json!({"directory":"candidate","replace_existing":true}),
            json!({"directory":"candidate","accept_unreadable":true}),
            json!({"id":"demo"}),
            json!({"directory":""}),
            json!({"directory":"candidate","id":"bad id"}),
        ] {
            assert!(
                install_arguments(&arguments(refused.clone())).is_err(),
                "{refused} must be refused"
            );
        }
    }
}

/// `plugin_uninstall`: the form's facts, the data checkbox, and the audit fact.
#[cfg(test)]
mod uninstall_tool_tests {
    use super::*;
    use crate::application::RunApplication;
    use anchor_graph_host::{plugin_view_path, resolved_library_root, store_root, views_root};
    use anchor_platform_session::{CreateSession, SessionStore};
    use std::path::{Path, PathBuf};

    fn fixture() -> (tempfile::TempDir, PilotTools) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("fixture")).unwrap();
        std::fs::write(
            root.path().join("fixture/graph.json"),
            r#"{"entry":"idle","ops":{"idle":{"run":"true"}},"nodes":[{"id":"idle","op":"idle"}],"edges":[]}"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join("fixture/manifest.json"),
            r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
        )
        .unwrap();
        let sessions = SessionStore::open(root.path().join("sessions.sqlite")).unwrap();
        sessions
            .create(
                "local",
                CreateSession {
                    id: Some("pilot".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let tools = PilotTools {
            application: RunApplication::new(root.path().join("state"), root.path().to_path_buf()),
            graph_name: "fixture".into(),
            library_root: root.path().to_path_buf(),
            data_root: root.path().join("state"),
            workspace_root: root.path().join("work"),
            sessions,
            owner: "local".into(),
            session: "pilot".into(),
            turn: "turn-1".into(),
            context: None,
        };
        (root, tools)
    }

    #[tokio::test]
    async fn run_abandon_requires_native_consent_and_reuses_terminal_application_control() {
        use crate::application::run_access::fixtures;
        let (_root, mut tools) = fixture();
        let (turn, _) = tools
            .sessions
            .create_turn("local", "pilot", "abandon", None)
            .unwrap();
        tools.turn = turn.id;
        assert_eq!(
            crate::pilot_tools::authority("run_abandon"),
            Some(crate::pilot_tools::ToolAuthority::Confirm)
        );
        let definition = definitions()
            .into_iter()
            .find(|tool| tool.name == "run_abandon")
            .unwrap();
        assert_eq!(definition.parameters["additionalProperties"], false);
        for arguments in [
            json!({"run":"stuck","confirm":true}),
            json!({"run":"../stuck"}),
            json!({"confirmed":true}),
        ] {
            assert!(abandon_target(arguments.as_object().unwrap()).is_err());
        }
        let record = fixtures::work_record("stuck");
        fixtures::create(&tools.application, &record);
        crate::run_metadata::save(
            &tools.data_root,
            &crate::run_metadata::RunMetadata::new(
                "stuck".into(),
                "fixture".into(),
                record.graph_digest.clone(),
                &tools.library_root.join("fixture"),
            )
            .unwrap(),
        )
        .unwrap();
        fixtures::control(
            &tools.application,
            "stuck",
            "stop",
            "Goose ended without a validated final_result",
        );
        let before = tools.application.pilot_run_status("stuck").await.unwrap();
        let cancellation = Cancellation::default();
        for (accepted, confirm) in [(false, true), (true, false)] {
            let answer = apply_abandon_answer(
                &tools,
                "stuck",
                &record.graph_digest,
                accepted,
                confirm,
                &cancellation,
            )
            .await
            .unwrap();
            assert_eq!(answer["abandoned"], false);
            assert_eq!(
                tools.application.pilot_run_status("stuck").await.unwrap(),
                before
            );
            assert!(tools.application.abandon_intent("stuck").unwrap().is_none());
        }
        assert!(
            apply_abandon_answer(&tools, "stuck", "changed-digest", true, true, &cancellation)
                .await
                .is_err()
        );
        cancellation.store(true, Ordering::Release);
        assert!(
            apply_abandon_answer(
                &tools,
                "stuck",
                &record.graph_digest,
                true,
                true,
                &cancellation
            )
            .await
            .is_err()
        );
        cancellation.store(false, Ordering::Release);
        let answer = apply_abandon_answer(
            &tools,
            "stuck",
            &record.graph_digest,
            true,
            true,
            &cancellation,
        )
        .await
        .unwrap();
        assert_eq!(answer["abandoned"], true);
        assert_eq!(answer["action"], "abandoned");
        let status = tools.application.pilot_run_status("stuck").await.unwrap();
        assert_eq!(status["projection"]["outcome"], "aborted");
        assert_eq!(status["record"]["snapshot"], before["record"]["snapshot"]);
        assert!(tools.application.control("stuck", "resume").await.is_err());
        assert_eq!(
            tools
                .application
                .abandon_intent("stuck")
                .unwrap()
                .unwrap()
                .reason,
            "operator"
        );
    }

    fn candidate(root: &Path, name: &str, data: &[u8]) -> PathBuf {
        let directory = root.join(format!("candidate-{name}"));
        std::fs::create_dir_all(directory.join("state")).unwrap();
        std::fs::write(
            directory.join("plugin.json"),
            json!({"name": name, "mutable": ["state/"]}).to_string(),
        )
        .unwrap();
        std::fs::write(directory.join("state/data.bin"), data).unwrap();
        directory
    }

    async fn install(tools: &PilotTools, directory: PathBuf, id: &str) {
        let installed = tools.library_root.join("plugins").join(id);
        let first_install = !installed.exists();
        let runtime_data = std::fs::read(directory.join("state/data.bin")).unwrap();
        let precondition = tools
            .application
            .plugin_install_precondition(tools.library_root.clone(), directory, id.into())
            .await
            .unwrap();
        tools
            .application
            .install_plugin_if_unchanged(tools.library_root.clone(), precondition)
            .await
            .unwrap();
        if first_install {
            // Import provides empty data locations. Model the Plugin writing
            // its runtime state only after installation; upgrades keep it.
            let data = installed.join("state/data.bin");
            assert!(!data.exists(), "source data must not be imported");
            std::fs::write(data, runtime_data).unwrap();
        }
    }

    fn library_bytes(root: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(root: &Path, directory: &Path, files: &mut Vec<(String, Vec<u8>)>) {
            let mut entries = std::fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    walk(root, &path, files);
                } else {
                    let relative = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    files.push((relative, std::fs::read(&path).unwrap()));
                }
            }
        }
        let mut files = Vec::new();
        for subtree in ["plugins", "dependencies"] {
            let path = root.join(subtree);
            if path.is_dir() {
                walk(root, &path, &mut files);
            }
        }
        files
    }

    fn approved() -> UninstallAnswer {
        UninstallAnswer {
            accepted: true,
            confirm: true,
            confirm_data_loss: true,
        }
    }

    async fn uninstall_current(
        tools: &PilotTools,
        id: &str,
        answer: UninstallAnswer,
    ) -> Result<Value, ToolError> {
        let precondition = tools
            .application
            .plugin_uninstall_precondition(&tools.library_root, id, &tools.sessions)
            .await
            .map_err(|error| failure(application_reason(error)))?;
        uninstall_with_answer(
            tools,
            &precondition,
            answer,
            &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
    }

    #[tokio::test]
    async fn platform_pilot_without_a_graph_can_confirm_a_plugin_uninstall() {
        let (root, mut tools) = fixture();
        tools.graph_name.clear();
        std::fs::remove_dir_all(root.path().join("fixture")).unwrap();
        install(&tools, candidate(root.path(), "Demo", b"precious"), "demo").await;
        let value = uninstall_current(&tools, "demo", approved()).await.unwrap();
        assert_eq!(value["uninstalled"], true, "{value}");
        assert_eq!(value["data_removed"], true, "{value}");
        assert!(!root.path().join("plugins/demo").exists());
        assert!(
            tools
                .sessions
                .events("local", "pilot", 0)
                .unwrap()
                .iter()
                .any(|event| event.kind == "plugin.uninstalled")
        );
    }

    #[tokio::test]
    async fn configured_instruction_plugins_remain_protected_for_install_and_uninstall() {
        let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
        let (root, tools) = fixture();
        install(&tools, candidate(root.path(), "Demo", b"precious"), "demo").await;
        tools.application.update_graph(
            "fixture",
            json!({"entry":"pilot","agents":{"pilot":{"model":"models.worker","instructions":"Protected legacy assistant"}},"nodes":[{"id":"pilot","agent":"pilot","plugins":["demo"]}],"edges":[]}),
        ).await.unwrap();
        let before = library_bytes(root.path());
        for operation in ["install", "uninstall"] {
            let error = refuse_instruction_plugin(&tools, "demo", operation).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("provides this assistant's own instructions"),
                "{error}"
            );
            assert!(error.to_string().contains(operation), "{error}");
            assert!(refuse_instruction_plugin(&tools, "unrelated", operation).is_ok());
        }
        let error = uninstall_current(&tools, "demo", approved())
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("provides this assistant's own instructions"),
            "{error}"
        );
        assert_eq!(library_bytes(root.path()), before);
        std::fs::remove_dir_all(root.path().join("fixture")).unwrap();
        for operation in ["install", "uninstall"] {
            let error = refuse_instruction_plugin(&tools, "demo", operation).unwrap_err();
            assert!(
                error.to_string().contains("assistant Graph is unavailable"),
                "{error}"
            );
        }
        assert_eq!(library_bytes(root.path()), before);
    }

    #[tokio::test]
    async fn an_answer_for_the_original_form_cannot_uninstall_a_replacement() {
        let (root, tools) = fixture();
        install(
            &tools,
            candidate(root.path(), "Original", b"precious"),
            "demo",
        )
        .await;
        let approved_state = tools
            .application
            .plugin_uninstall_precondition(&tools.library_root, "demo", &tools.sessions)
            .await
            .unwrap();
        let (message, _) = uninstall_form(&approved_state);
        assert!(message.contains("Original"), "{message}");
        // A second Session can install a replacement while the original native
        // form is open. Its answer must remain bound to the original digest.
        install(
            &tools,
            candidate(root.path(), "Replacement", b"unused"),
            "demo",
        )
        .await;
        let before = library_bytes(root.path());
        let error = uninstall_with_answer(
            &tools,
            &approved_state,
            approved(),
            &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(library_bytes(root.path()), before);
        assert_eq!(
            std::fs::read(root.path().join("plugins/demo/state/data.bin")).unwrap(),
            b"precious"
        );
        assert!(
            tools
                .sessions
                .events("local", "pilot", 0)
                .unwrap()
                .iter()
                .all(|event| event.kind != "plugin.uninstalled")
        );
    }

    #[tokio::test]
    async fn the_uninstall_form_states_the_data_loss_and_the_declared_bytes() {
        let (root, tools) = fixture();
        install(
            &tools,
            candidate(root.path(), "Demo", b"0123456789"),
            "demo",
        )
        .await;
        let precondition = tools
            .application
            .plugin_uninstall_precondition(&tools.library_root, "demo", &tools.sessions)
            .await
            .unwrap();
        let (message, schema) = uninstall_form(&precondition);
        assert!(message.contains("demo"), "{message}");
        assert!(message.contains("Demo"), "{message}");
        assert!(message.contains("state/"), "{message}");
        assert!(message.contains("10 字节"), "{message}");
        assert!(
            message.contains("这会删掉这个插件唯一的一份数据，不可恢复"),
            "{message}"
        );
        assert_eq!(
            schema["required"],
            json!(["confirm", "confirm_data_loss"]),
            "{schema}"
        );
        assert!(
            schema["properties"]["confirm_data_loss"]["title"]
                .as_str()
                .is_some_and(|title| title.contains("不可恢复")),
            "{schema}"
        );
    }

    #[tokio::test]
    async fn an_unchecked_data_box_uninstalls_nothing() {
        let (root, tools) = fixture();
        install(&tools, candidate(root.path(), "Demo", b"precious"), "demo").await;
        let before = library_bytes(root.path());
        let value = uninstall_current(
            &tools,
            "demo",
            UninstallAnswer {
                accepted: true,
                confirm: true,
                confirm_data_loss: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(value["uninstalled"], false, "{value}");
        assert!(
            value["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("数据")),
            "{value}"
        );
        assert_eq!(library_bytes(root.path()), before);
        assert_eq!(
            std::fs::read(root.path().join("plugins/demo/state/data.bin")).unwrap(),
            b"precious"
        );
        assert!(
            tools
                .sessions
                .events("local", "pilot", 0)
                .unwrap()
                .iter()
                .all(|event| event.kind != "plugin.uninstalled"),
            "nothing happened, so there is nothing to audit"
        );
    }

    #[tokio::test]
    async fn a_session_mounted_plugin_is_refused_and_moves_no_bytes() {
        let (root, tools) = fixture();
        install(&tools, candidate(root.path(), "Demo", b"precious"), "demo").await;
        tools
            .sessions
            .set_plugin_mount("local", "pilot", "demo", true, "operator", "click")
            .unwrap();
        let before = library_bytes(root.path());
        let error = uninstall_current(&tools, "demo", approved())
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("先在所有会话里卸载它"), "{message}");
        assert!(message.contains("pilot"), "{message}");
        assert_eq!(library_bytes(root.path()), before);
        assert_eq!(
            std::fs::read(root.path().join("plugins/demo/state/data.bin")).unwrap(),
            b"precious"
        );
    }

    #[tokio::test]
    async fn a_confirmed_uninstall_removes_the_plugin_reclaims_and_audits() {
        let (root, tools) = fixture();
        install(&tools, candidate(root.path(), "Demo", b"precious"), "demo").await;
        // A second Plugin stays installed, so the reclaimer's "nothing installed"
        // guard does not fire and the sweep really runs.
        install(&tools, candidate(root.path(), "Other", b"x"), "other").await;
        let resolved = resolved_library_root(&tools.library_root);
        let view = plugin_view_path(
            &resolved,
            "demo",
            &crate::plugin_library::Catalog::new(&tools.library_root)
                .definition("demo")
                .unwrap()
                .dependencies
                .declaration_digest(),
        );
        std::fs::create_dir_all(view.join("bin")).unwrap();
        std::fs::write(view.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        let store = store_root(&resolved);
        let orphan = store.join("idna/3.6");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("idna.py"), "IDNA = 1\n").unwrap();
        let keeper = store.join("requests/2.32");
        std::fs::create_dir_all(&keeper).unwrap();
        std::fs::write(keeper.join("requests.py"), "REQUESTS = 1\n").unwrap();
        let keeper_view = views_root(&resolved).join("keeper/aaaa");
        std::fs::create_dir_all(keeper_view.join("lib/python3.12/site-packages")).unwrap();
        std::fs::write(keeper_view.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        std::os::unix::fs::symlink(
            &keeper,
            keeper_view.join("lib/python3.12/site-packages/requests"),
        )
        .unwrap();

        let value = uninstall_current(&tools, "demo", approved()).await.unwrap();
        assert_eq!(value["uninstalled"], true, "{value}");
        assert_eq!(value["data_removed"], true, "{value}");
        assert!(value["removed_bytes"].as_u64().unwrap() > 0, "{value}");
        assert!(!root.path().join("plugins/demo").exists());
        assert!(root.path().join("plugins/other").is_dir());
        assert!(!view.exists());
        assert!(
            !orphan.exists(),
            "the freed package version must be reclaimed: {value}"
        );
        assert!(keeper.is_dir(), "a referenced package version must be kept");
        assert!(
            value["reclaimed_package_versions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|version| version == "idna/3.6"),
            "{value}"
        );
        // The audit fact names who, which Plugin, that the data went with it, and
        // what was freed.
        let events = tools.sessions.events("local", "pilot", 0).unwrap();
        let audit = events
            .iter()
            .find(|event| event.kind == "plugin.uninstalled")
            .expect("the uninstall must leave an audit fact");
        assert_eq!(audit.data["plugin"], "demo");
        assert_eq!(audit.data["actor"], "local");
        assert_eq!(audit.data["data_removed"], true);
        assert!(audit.data["removed_bytes"].as_u64().unwrap() > 0);

        // A second attempt is a named refusal, not a second success.
        let error = uninstall_current(&tools, "demo", approved())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not installed"), "{error}");
    }
}
