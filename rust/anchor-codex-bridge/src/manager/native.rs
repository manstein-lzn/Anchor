//! Native schemas are Codex-owned; this module owns only project admission.
use super::*;
use crate::protocol::ProtocolCatalog;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Discover {
    #[serde(default = "request_direction")]
    direction: String,
    method: Option<String>,
    search: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "page_limit")]
    limit: usize,
}
fn request_direction() -> String {
    "request".into()
}
fn page_limit() -> usize {
    50
}
fn empty_params() -> Value {
    json!({})
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Call {
    method: String,
    #[serde(default = "empty_params")]
    params: Value,
    #[serde(default)]
    confirm: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Events {
    thread_id: String,
    after: Option<u64>,
    #[serde(default = "page_limit")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Respond {
    thread_id: String,
    request_key: String,
    result: Value,
}

// Explicit permission classification, NOT a second parameter schema. New native
// methods appear in discovery immediately; unreviewed permissions fail closed.
fn policy(method: &str) -> &'static str {
    match method {
        "thread/start"
        | "thread/resume"
        | "thread/fork"
        | "turn/start"
        | "turn/steer"
        | "turn/interrupt"
        | "turn/settings/update"
        | "thread/settings/update"
        | "thread/name/set"
        | "thread/archive"
        | "thread/unarchive"
        | "thread/unsubscribe"
        | "thread/compact/start"
        | "thread/delete"
        | "thread/revert"
        | "thread/read"
        | "thread/turns/list"
        | "thread/items/list"
        | "thread/items/read"
        | "thread/searchOccurrences"
        | "thread/timeline/list"
        | "thread/readState/update"
        | "thread/guardianReview"
        | "thread/memory/read"
        | "thread/memoryMode/set"
        | "thread/inject_items"
        | "thread/goal/get"
        | "thread/goal/set"
        | "thread/goal/clear"
        | "thread/queue/add"
        | "thread/queue/list"
        | "thread/queue/update"
        | "thread/queue/delete"
        | "thread/queue/reorder"
        | "thread/queue/start"
        | "thread/attachment/add"
        | "thread/attachment/list"
        | "thread/attachment/remove"
        | "thread/backgroundTerminals/list"
        | "thread/backgroundTerminals/clean"
        | "thread/backgroundTerminals/terminate"
        | "thread/metadata/update"
        | "review/start" => "project_thread",
        "thread/list" | "thread/search" | "thread/loaded/list" => "filtered_project_list",
        "skills/list" | "hooks/list" => "project_paths",
        "model/list"
        | "modelProvider/capabilities/read"
        | "collaborationMode/list"
        | "experimentalFeature/list" => "catalog_read",
        "thread/shellCommand" => "blocked: native unsandboxed host shell; use a sandboxed turn",
        "thread/sample" | "thread/sample/cancel" | "thread/prediction/request" => {
            "blocked: independent native execution ownership is not represented; use btw"
        }
        _ => "blocked: host-wide or unreviewed authority; not part of the project grant",
    }
}
fn allowed(method: &str) -> bool {
    !policy(method).starts_with("blocked:")
}
fn starts_work(method: &str) -> bool {
    matches!(
        method,
        "turn/start"
            | "thread/queue/start"
            | "thread/queue/add"
            | "thread/compact/start"
            | "review/start"
            | "thread/goal/set"
    )
}
fn requires_idle(method: &str) -> bool {
    (starts_work(method) && !matches!(method, "thread/goal/set" | "thread/queue/add"))
        || matches!(
            method,
            "thread/resume"
                | "thread/delete"
                | "thread/revert"
                | "thread/unsubscribe"
                | "thread/archive"
                | "thread/inject_items"
                | "thread/memoryMode/set"
        )
}
fn validate_page(limit: usize) -> Result<(), String> {
    if (1..=100).contains(&limit) {
        Ok(())
    } else {
        Err("limit must be 1..100".into())
    }
}
fn validate_schema(schema: &Value, value: &Value) -> Result<(), String> {
    // Codex schemas leave unknown fields open, while serde may accept aliases.
    // Do not let unadvertised top-level fields bypass admission checks.
    let mut schema = schema.clone();
    let root_ref = schema["$ref"].as_str().map(str::to_owned);
    let root = if let Some(reference) = root_ref {
        schema
            .pointer_mut(reference.trim_start_matches('#'))
            .ok_or("unresolved native schema")?
    } else {
        &mut schema
    };
    if root["type"] == "object" {
        root["additionalProperties"] = json!(false);
    }
    let validator = jsonschema::validator_for(&schema)
        .map_err(|e| format!("native schema unavailable: {e}"))?;
    if let Some(error) = validator.iter_errors(value).next() {
        return Err(format!(
            "native argument at {}: {}",
            error.instance_path, error
        ));
    }
    Ok(())
}

impl Manager {
    async fn protocol(&self) -> Result<&ProtocolCatalog, String> {
        self.protocol
            .get_or_try_init(|| ProtocolCatalog::load(&self.config.codex_bin))
            .await
    }

    pub(super) async fn native_discover(&self, a: Discover) -> Result<Value, String> {
        validate_page(a.limit)?;
        let protocol = self.protocol().await?;
        if let Some(method) = a.method {
            let mut value = protocol.describe(&a.direction, &method)?;
            if a.direction == "request" {
                value["bridge_policy"] = json!(policy(&method));
                value["callable"] = json!(allowed(&method));
            }
            value["boundary"] = json!(
                "Native schema is authoritative. Project calls retain directory, writer and user-approval checks. Config overrides only model_reasoning_effort, model_reasoning_summary, model_verbosity; no host auth/config, unsandboxed shell, alternate rollout history/path, named permissions or environment injection. Destructive delete/revert requires confirm=true. Native work is asynchronous; inspect/events, never replay uncertain mutations."
            );
            return Ok(value);
        }
        if !["request", "notification", "server_request"].contains(&a.direction.as_str()) {
            return Err("direction must be request, notification or server_request".into());
        }
        let all: Vec<_> = protocol
            .methods(&a.direction)
            .into_iter()
            .filter(|entry| {
                a.search
                    .as_ref()
                    .is_none_or(|q| entry.to_string().to_lowercase().contains(&q.to_lowercase()))
            })
            .map(|mut entry| {
                if a.direction == "request" {
                    let method = entry["method"].as_str().unwrap_or_default().to_owned();
                    entry["bridge_policy"] = json!(policy(&method));
                    entry["callable"] = json!(allowed(&method));
                }
                entry
            })
            .collect();
        let end = a.offset.saturating_add(a.limit).min(all.len());
        Ok(
            json!({"direction":a.direction,"total":all.len(),"methods":all.iter().skip(a.offset).take(a.limit).collect::<Vec<_>>(),"next_offset":(end<all.len()).then_some(end),"source":"installed codex generate-json-schema --experimental; cached until Bridge restart"}),
        )
    }

    pub(super) async fn native_events(
        &self,
        a: Events,
        roots: &[PathBuf],
    ) -> Result<Value, String> {
        validate_page(a.limit)?;
        self.thread(&a.thread_id, roots, false).await?;
        Ok(self
            .state
            .native_events(&a.thread_id, a.after, a.limit)
            .await)
    }

    pub(super) async fn native_call(&self, a: Call, roots: &[PathBuf]) -> Result<Value, String> {
        if !allowed(&a.method) {
            return Err(policy(&a.method).into());
        }
        // Remove this restriction only when the returned reviewThreadId owns
        // admission and terminal lease tracking, not the source thread.
        if a.method == "review/start" && a.params["delivery"] == "detached" {
            return Err(
                "detached review execution ownership is not tracked; use inline review".into(),
            );
        }
        let protocol = self.protocol().await?;
        if !protocol.contains(&a.method) {
            return Err("method is not supported by the installed Codex".into());
        }
        let descriptor = protocol.describe("request", &a.method)?;
        let mut params = a.params;
        if !params.is_object() {
            return Err("params must be an object".into());
        }
        validate_schema(&descriptor["params_schema"], &params)?;
        let method = a.method.as_str();
        let work = starts_work(method)
            && !(method == "thread/goal/set"
                && matches!(
                    params["status"].as_str(),
                    Some("paused" | "blocked" | "complete" | "usageLimited" | "budgetLimited")
                ));
        if matches!(method, "thread/delete" | "thread/revert") && !a.confirm {
            return Err(
                "destructive operation requires explicit user authorization and confirm=true"
                    .into(),
            );
        }
        self.check_native_overrides(&params, roots).await?;
        if policy(method) == "catalog_read" {
            return self.client.request(method, params).await;
        }
        if policy(method) == "filtered_project_list" {
            if let Some(cwd) = params["cwd"].as_str() {
                params["cwd"] = json!(self.config.resolve_project(cwd, roots)?);
            }
            let mut response = self.client.request(method, params).await?;
            let mut visible = Vec::new();
            for entry in response["data"].as_array().cloned().unwrap_or_default() {
                let id = entry
                    .as_str()
                    .or_else(|| entry["id"].as_str())
                    .or_else(|| entry["thread"]["id"].as_str())
                    .or_else(|| entry["threadId"].as_str());
                if let Some(id) = id
                    && self.thread(id, roots, false).await.is_ok()
                {
                    visible.push(entry);
                }
            }
            if let Some(states) = response["readStates"].as_object_mut() {
                states.retain(|id, _| visible.iter().any(|v| v["id"].as_str() == Some(id)));
            }
            response["data"] = json!(visible);
            // Keep native cursors even for empty filtered pages.
            return Ok(response);
        }
        if policy(method) == "project_paths" {
            let default_cwds = if let Some(id) = params["threadId"].as_str() {
                vec![self.thread(id, roots, false).await?.cwd]
            } else {
                roots.to_vec()
            };
            if params["cwds"].is_null() || params["cwds"].as_array().is_some_and(Vec::is_empty) {
                params["cwds"] = json!(default_cwds);
            }
            let cwds = params["cwds"]
                .as_array()
                .ok_or("cwds must be an array")?
                .iter()
                .map(|cwd| {
                    self.config
                        .resolve_project(cwd.as_str().ok_or("cwd must be a string")?, roots)
                })
                .collect::<Result<Vec<_>, String>>()?;
            params["cwds"] = json!(cwds);
            return self.client.request(method, params).await;
        }
        if method == "thread/start" {
            return self.native_create(method, params, None, roots).await;
        }
        let id = params["threadId"]
            .as_str()
            .ok_or("threadId is required for project admission")?
            .to_owned();
        let mut thread = self.thread(&id, roots, true).await?;
        if method == "thread/fork" {
            return self
                .native_create(method, params, Some(&thread), roots)
                .await;
        }
        if thread.native_queue_pending {
            self.refresh_native_queue(&id).await?;
            thread = self
                .state
                .threads
                .read()
                .await
                .get(&id)
                .cloned()
                .ok_or("thread disappeared")?;
        }
        let occupied = if matches!(method, "turn/start" | "thread/queue/start" | "review/start") {
            executing(&thread)
        } else {
            busy(&thread)
        };
        if requires_idle(method) && occupied {
            return Err(
                "session is busy or uncertain; inspect/interrupt before this operation".into(),
            );
        }
        if thread.archived
            && !matches!(
                method,
                "thread/unarchive"
                    | "thread/delete"
                    | "thread/name/set"
                    | "thread/read"
                    | "thread/turns/list"
                    | "thread/items/list"
                    | "thread/items/read"
                    | "thread/searchOccurrences"
            )
        {
            return Err("unarchive this session before modifying or loading it".into());
        }
        if thread.parent_thread_id.is_some()
            && (params["sandbox"] == "workspace-write"
                || params["sandboxPolicy"]["type"] == "workspaceWrite"
                || params["sandboxPolicy"]["networkAccess"] == true
                || params.get("cwd").is_some_and(|v| v != &json!(thread.cwd)))
        {
            return Err("btw conversation permissions cannot be expanded".into());
        }
        if matches!(
            method,
            "thread/resume" | "turn/start" | "thread/settings/update"
        ) {
            if busy(&thread)
                && (params.get("cwd").is_some()
                    || params.get("sandboxPolicy").is_some()
                    || params.get("sandbox").is_some())
            {
                return Err("execution permissions/cwd cannot change while work is active".into());
            }
            self.native_execution_settings(&mut params, &mut thread, method, roots)
                .await?;
        }
        if work {
            self.check_writer(&thread.cwd, &thread.mode, Some(&id))
                .await?;
            self.load_thread(&thread, &thread.mode, Approval::OnRequest)
                .await?;
        }
        let permission_change = matches!(method, "thread/settings/update" | "thread/resume");
        let _lease = if work || permission_change {
            self.client
                .prepare_writer(&id, &thread.cwd, &thread.mode)
                .await?
        } else {
            None
        };
        if work && method != "turn/start" {
            self.client.writer_request("thread/settings/update", json!({"threadId":id,"sandboxPolicy":native_sandbox(&thread),"approvalsReviewer":"user","approvalPolicy":if thread.parent_thread_id.is_some() { "never" } else { "on-request" }}), &id, &thread.cwd, &thread.mode).await?;
        }
        if method == "turn/start" {
            if let Some(current) = self.state.threads.write().await.get_mut(&id) {
                current.cwd = thread.cwd;
                current.mode = thread.mode;
                current.network_access = thread.network_access;
            }
            let approval =
                if params["approvalPolicy"] == "never" || thread.parent_thread_id.is_some() {
                    Approval::Never
                } else {
                    Approval::OnRequest
                };
            return self.start_native_turn(&id, params, approval).await;
        }
        if matches!(
            method,
            "thread/settings/update" | "turn/steer" | "turn/settings/update"
        ) {
            self.load_thread(&thread, &thread.mode, Approval::OnRequest)
                .await?;
        }
        if method == "thread/queue/add"
            && let Some(current) = self.state.threads.write().await.get_mut(&id)
        {
            current.native_queue_pending = true;
        }
        if work && thread.active_turn_id.is_none() {
            // Reserve the writer on ambiguous admission; native turn events release it.
            if let Some(current) = self.state.threads.write().await.get_mut(&id) {
                current.admission_epoch += 1;
                current.status = WorkerStatus::Unknown;
            }
        }
        if permission_change {
            // A lost response must never leave a writable native thread recorded
            // as read-only. Retain the union until a subsequent bounded sync.
            if let Some(current) = self.state.threads.write().await.get_mut(&id)
                && thread.mode == "workspace-write"
            {
                current.mode = thread.mode.clone();
            }
        }
        let response = match if method == "thread/goal/get" {
            self.refresh_native_goal(&id).await
        } else if work || permission_change {
            self.client
                .writer_request(method, params.clone(), &id, &thread.cwd, &thread.mode)
                .await
        } else {
            self.client.request(method, params.clone()).await
        } {
            Ok(value) => value,
            Err(error) => {
                if permission_change
                    && let Some(current) = self.state.threads.write().await.get_mut(&id)
                {
                    current.status = WorkerStatus::Unknown;
                    current.last_error = Some(
                        "Native permission update outcome uncertain; inspect before further work"
                            .into(),
                    );
                }
                return Err(format!("{error}; inspect before retrying a mutation"));
            }
        };
        if work && response["turn"]["id"].is_string() {
            let turn_id = response["turn"]["id"].as_str();
            if self
                .state
                .threads
                .read()
                .await
                .get(&id)
                .is_some_and(|t| t.last_turn_id.as_deref() != turn_id)
            {
                self.state
                    .apply_notification(
                        "turn/started",
                        &json!({"threadId":id,"turn":response["turn"]}),
                    )
                    .await;
            }
        }
        let previously_executing = executing(&thread);
        if let Some(current) = self.state.threads.write().await.get_mut(&id) {
            match method {
                "thread/goal/set" => {
                    current.native_goal = response.get("goal").filter(|v| !v.is_null()).cloned();
                    if current.active_turn_id.is_none() && work {
                        current.status = WorkerStatus::Idle;
                    }
                }
                "thread/goal/clear" => current.native_goal = None,
                "thread/resume" | "thread/settings/update" => {
                    current.cwd = thread.cwd;
                    current.mode = thread.mode;
                    current.network_access = thread.network_access;
                    current.loaded = true;
                    current.subscribed = true;
                    if current.active_turn_id.is_none() && !previously_executing {
                        current.status = WorkerStatus::Idle;
                    }
                    if method == "thread/resume" {
                        current.model = response["model"].as_str().map(str::to_owned);
                        current.reasoning_effort =
                            response["reasoningEffort"].as_str().map(str::to_owned);
                    }
                }
                "thread/unsubscribe" => {
                    current.subscribed = false;
                    if response["status"] == "notLoaded" {
                        current.loaded = false;
                    }
                }
                "thread/archive" => current.archived = true,
                "thread/unarchive" => current.archived = false,
                "thread/name/set" => current.title = params["name"].as_str().map(str::to_owned),
                _ => {}
            }
        }
        if matches!(
            method,
            "thread/queue/add" | "thread/queue/delete" | "thread/queue/start"
        ) {
            self.refresh_native_queue(&id).await?;
        }
        if method == "thread/resume" {
            self.refresh_native_goal(&id).await?;
            self.refresh_native_queue(&id).await?;
        }
        self.state.release_idle_writer(&id).await;
        if method == "thread/delete" {
            self.state.threads.write().await.remove(&id);
        }
        Ok(response)
    }

    pub(super) async fn refresh_native_queue(&self, id: &str) -> Result<(), String> {
        let epoch = self
            .state
            .threads
            .read()
            .await
            .get(id)
            .ok_or("thread disappeared")?
            .native_queue_epoch;
        let result = self
            .client
            .request("thread/queue/list", json!({"threadId":id,"limit":1}))
            .await?;
        let pending = !result["data"]
            .as_array()
            .ok_or("native queue response missing data")?
            .is_empty();
        if let Some(t) = self.state.threads.write().await.get_mut(id)
            && t.native_queue_epoch == epoch
        {
            t.native_queue_pending = pending;
            self.state.release_idle_writer_locked(id, t);
        }
        Ok(())
    }

    pub(super) async fn refresh_native_goal(&self, id: &str) -> Result<Value, String> {
        let (epoch, goal, cursor) = {
            let threads = self.state.threads.read().await;
            let t = threads.get(id).ok_or("thread disappeared")?;
            (
                t.admission_epoch,
                t.native_goal.clone(),
                self.state.native_events(id, None, 0).await["last_cursor"].clone(),
            )
        };
        let response = self
            .client
            .request("thread/goal/get", json!({"threadId":id}))
            .await?;
        // Reads cannot undo a newer admission or event, even an identical goal update.
        if let Some(t) = self.state.threads.write().await.get_mut(id)
            && t.admission_epoch == epoch
            && t.native_goal == goal
            && self.state.native_events(id, None, 0).await["last_cursor"] == cursor
        {
            t.native_goal = response.get("goal").filter(|v| !v.is_null()).cloned();
        }
        Ok(response)
    }

    async fn check_native_overrides(
        &self,
        params: &Value,
        roots: &[PathBuf],
    ) -> Result<(), String> {
        for key in [
            "path",
            "history",
            "permissions",
            "environments",
            "runtimeWorkspaceRoots",
            "selectedCapabilityRoots",
            "projectId",
            "experimentalPredictionMode",
            "attachToSource",
        ] {
            if params
                .get(key)
                .is_some_and(|v| !v.is_null() && v != &json!(false))
            {
                return Err(format!(
                    "{key} changes ownership or authority outside this project client"
                ));
            }
        }
        if let Some(config) = params["config"].as_object()
            && let Some(key) = config.keys().find(|k| {
                ![
                    "model_reasoning_effort",
                    "model_reasoning_summary",
                    "model_verbosity",
                ]
                .contains(&k.as_str())
            })
        {
            return Err(format!(
                "config.{key} is local-user configuration, not project authority"
            ));
        }
        if !params["approvalsReviewer"].is_null() && params["approvalsReviewer"] != "user" {
            return Err("approvalsReviewer must remain user".into());
        }
        if !params["approvalPolicy"].is_null()
            && params["approvalPolicy"] != "on-request"
            && params["approvalPolicy"] != "never"
        {
            return Err("approvalPolicy must be on-request or never".into());
        }
        if !params["sandbox"].is_null()
            && params["sandbox"] != "read-only"
            && params["sandbox"] != "workspace-write"
        {
            return Err("sandbox exceeds the project grant".into());
        }
        for key in ["parentThreadId", "ancestorThreadId", "attachedToThreadId"] {
            if let Some(id) = params[key].as_str() {
                self.thread(id, roots, false).await?;
            }
        }
        Ok(())
    }

    async fn native_execution_settings(
        &self,
        params: &mut Value,
        thread: &mut TrackedThread,
        method: &str,
        roots: &[PathBuf],
    ) -> Result<(), String> {
        if let Some(cwd) = params["cwd"].as_str() {
            let cwd = self.config.resolve_project(cwd, roots)?;
            if cwd != thread.cwd {
                return Err("moving an existing session changes workspace ownership; fork into the target project/worktree instead".into());
            }
        }
        if let Some(mode) = params["sandbox"].as_str() {
            thread.mode = mode.into();
        }
        if let Some(sandbox) = params["sandboxPolicy"].as_object() {
            thread.mode = match sandbox.get("type").and_then(Value::as_str) {
                Some("readOnly") => "read-only",
                Some("workspaceWrite") => "workspace-write",
                _ => return Err("sandboxPolicy exceeds the project grant".into()),
            }
            .into();
            thread.network_access = sandbox
                .get("networkAccess")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(writable) = sandbox.get("writableRoots").and_then(Value::as_array) {
                for root in writable {
                    let path = PathBuf::from(root.as_str().ok_or("invalid writable root")?);
                    let canonical = path.canonicalize().map_err(|e| e.to_string())?;
                    if !canonical.starts_with(&thread.cwd)
                        && thread.git_common_dir.as_ref() != Some(&canonical)
                    {
                        return Err("writableRoots exceeds this session's workspace".into());
                    }
                }
            }
        }
        self.check_writer(&thread.cwd, &thread.mode, Some(&thread.thread_id))
            .await?;
        params["approvalsReviewer"] = json!("user");
        if thread.parent_thread_id.is_some() || params["approvalPolicy"].is_null() {
            params["approvalPolicy"] = json!(if thread.parent_thread_id.is_some() {
                "never"
            } else {
                "on-request"
            });
        }
        if method == "thread/resume" {
            params["cwd"] = json!(thread.cwd);
            params["sandbox"] = json!(thread.mode);
        } else if method == "turn/start" || params.get("sandboxPolicy").is_some() {
            params["sandboxPolicy"] = native_sandbox(thread);
        }
        Ok(())
    }

    async fn native_create(
        &self,
        method: &str,
        mut params: Value,
        parent: Option<&TrackedThread>,
        roots: &[PathBuf],
    ) -> Result<Value, String> {
        let cwd = params["cwd"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| parent.map(|t| t.cwd.to_string_lossy().into_owned()))
            .ok_or("native thread/start requires cwd within an authorized project")?;
        let cwd = self.config.resolve_project(&cwd, roots)?;
        let mode = params["sandbox"].as_str().unwrap_or("read-only").to_owned();
        if parent.is_some_and(|t| t.parent_thread_id.is_some()) {
            return Err("fork the main session, not a temporary btw conversation".into());
        }
        self.check_writer(&cwd, &mode, None).await?;
        // Creation does not start work; persistent forks defer goal continuation.
        let _admission = if mode == "workspace-write" {
            Some(
                anchor_file_lease::ProjectLease::acquire(&cwd)
                    .map_err(|e| format!("project writer lease unavailable: {e}"))?,
            )
        } else {
            None
        };
        params["cwd"] = json!(cwd);
        params["sandbox"] = json!(mode);
        params["approvalsReviewer"] = json!("user");
        if params["approvalPolicy"].is_null() {
            params["approvalPolicy"] = json!("on-request");
        }
        if method == "thread/fork" && params["ephemeral"] != true {
            params["deferGoalContinuation"] = json!(true);
        }
        let result = self.client.request(method, params).await.map_err(|e| {
            format!("{e}; creation outcome may be uncertain, inspect board before retrying")
        })?;
        let id = result["thread"]["id"]
            .as_str()
            .ok_or("created thread has no id; inspect board before retrying")?;
        self.import_thread(&result["thread"], roots).await?;
        let managed = git::managed_worktree_info(&self.config.worktree_root, &cwd).await?;
        if let Some(t) = self.state.threads.write().await.get_mut(id) {
            t.mode = mode;
            t.loaded = true;
            t.subscribed = true;
            t.project_path = Some(cwd);
            if let Some((repo, slug, common)) = managed {
                t.project_path = Some(repo);
                t.worktree_slug = Some(slug);
                t.git_common_dir = Some(common);
            }
            t.model = result["model"].as_str().map(str::to_owned);
            t.reasoning_effort = result["reasoningEffort"].as_str().map(str::to_owned);
        }
        if method == "thread/fork" && result["thread"]["ephemeral"] != true {
            self.refresh_native_goal(id).await?;
        }
        Ok(result)
    }

    pub(super) async fn native_respond(
        &self,
        a: Respond,
        roots: &[PathBuf],
    ) -> Result<Value, String> {
        let thread = self.thread(&a.thread_id, roots, false).await?;
        let request = thread
            .pending_requests
            .iter()
            .find(|r| r.request_key == a.request_key)
            .ok_or("request is no longer pending; inspect again")?;
        let descriptor = self
            .protocol()
            .await?
            .describe("server_request", &request.method)?;
        let schema = descriptor
            .get("response_schema")
            .filter(|v| !v.is_null())
            .ok_or("no native response schema; use steer_or_resume or interrupt")?;
        validate_schema(schema, &a.result)?;
        if thread.parent_thread_id.is_some() && request.method.contains("Approval") {
            return Err("btw cannot approve permission escalation".into());
        }
        if request.method == "item/permissions/requestApproval" {
            if a.result["scope"] != "turn" {
                return Err("permission grants are limited to this turn".into());
            }
            if a.result["permissions"] != json!({})
                && a.result["permissions"] != request.params["permissions"]
            {
                return Err(
                    "permission response must decline or match the pending request exactly".into(),
                );
            }
        }
        if matches!(
            request.method.as_str(),
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
        ) && !a.result["decision"].is_string()
        {
            return Err("approval policy amendments remain local-user configuration".into());
        }
        self.client
            .respond_to_server_request(
                &a.thread_id,
                &a.request_key,
                request.rpc_id.clone(),
                a.result,
            )
            .await?;
        if let Some(t) = self.state.threads.write().await.get_mut(&a.thread_id) {
            t.pending_requests
                .retain(|r| r.request_key != a.request_key);
        }
        Ok(
            json!({"thread_id":a.thread_id,"request_key":a.request_key,"status":"response_written","completed":false}),
        )
    }
}

pub(super) fn native_sandbox(thread: &TrackedThread) -> Value {
    if thread.mode == "read-only" {
        json!({"type":"readOnly","networkAccess":thread.network_access})
    } else {
        let mut roots = vec![thread.cwd.clone()];
        if let Some(common) = &thread.git_common_dir {
            roots.push(common.clone());
        }
        json!({"type":"workspaceWrite","writableRoots":roots,"networkAccess":thread.network_access,"excludeSlashTmp":true,"excludeTmpdirEnvVar":true})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_validation_rejects_undeclared_authority_and_preserves_payloads() {
        let schema = json!({"type":"object","properties":{"threadId":{"type":"string"},"outputSchema":{}},"required":["threadId"]});
        assert!(validate_schema(&schema, &json!({"threadId":"t","sandbox":"read-only"})).is_err());
        assert!(
            validate_schema(
                &schema,
                &json!({"threadId":"t","outputSchema":{"arbitrary":{"type":"string"}}})
            )
            .is_ok()
        );
        for method in [
            "config/value/write",
            "account/login/start",
            "fs/writeFile",
            "thread/shellCommand",
            "future/unknown",
        ] {
            assert!(!allowed(method));
        }
        for method in [
            "thread/queue/add",
            "thread/goal/set",
            "turn/settings/update",
            "thread/turns/list",
        ] {
            assert!(allowed(method));
        }
    }

    #[tokio::test]
    async fn native_permissions_and_goal_lease_preserve_project_writer_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let config = BridgeConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            bearer_token: "x".repeat(32),
            allow_roots: vec![project.clone()],
            worktree_root: tmp.path().join("worktrees"),
            codex_bin: "unused".into(),
            codex_args: vec![],
        }
        .validate()
        .unwrap();
        let manager = Manager::new(config);
        let roots = vec![project.clone()];
        // The configured binary does not exist: refusal must precede execution.
        assert!(
            manager
                .native_call(
                    Call {
                        method: "review/start".into(),
                        params: json!({"threadId":"t","delivery":"detached"}),
                        confirm: false,
                    },
                    &roots
                )
                .await
                .unwrap_err()
                .contains("detached review")
        );
        let mut thread = TrackedThread::new(
            "t".into(),
            None,
            Some(project.clone()),
            project.clone(),
            None,
            "workspace-write".into(),
        );
        thread.native_goal = Some(json!({"status":"active"}));
        manager
            .state
            .threads
            .write()
            .await
            .insert("t".into(), thread.clone());
        assert!(
            manager
                .check_writer(&project, "workspace-write", None)
                .await
                .is_err()
        );
        manager
            .state
            .threads
            .write()
            .await
            .get_mut("t")
            .unwrap()
            .native_goal = Some(json!({"status":"paused"}));
        assert!(
            manager
                .check_writer(&project, "workspace-write", None)
                .await
                .is_ok()
        );
        for params in [
            json!({"config":{"shell_environment_policy":{"inherit":"all"}}}),
            json!({"permissions":"full-access"}),
            json!({"path":"/tmp/alternate-rollout"}),
            json!({"approvalsReviewer":"guardian"}),
        ] {
            assert!(
                manager
                    .check_native_overrides(&params, &roots)
                    .await
                    .is_err()
            );
        }
        assert!(
            manager
                .check_native_overrides(&json!({"config":{"model_reasoning_effort":"max"}}), &roots)
                .await
                .is_ok()
        );
        let mut params =
            json!({"sandboxPolicy":{"type":"workspaceWrite","writableRoots":[tmp.path()]}});
        assert!(
            manager
                .native_execution_settings(&mut params, &mut thread, "turn/start", &roots)
                .await
                .is_err()
        );
        let mut params = json!({"sandboxPolicy":{"type":"dangerFullAccess"}});
        assert!(
            manager
                .native_execution_settings(&mut params, &mut thread, "turn/start", &roots)
                .await
                .is_err()
        );
        let mut params = json!({"sandboxPolicy":{"type":"workspaceWrite","writableRoots":[project],"excludeSlashTmp":false}});
        manager
            .native_execution_settings(&mut params, &mut thread, "turn/start", &roots)
            .await
            .unwrap();
        assert_eq!(params["sandboxPolicy"]["excludeSlashTmp"], true);
        assert_eq!(params["approvalsReviewer"], "user");
        manager.shutdown().await;

        #[cfg(unix)]
        {
            // Hold goal/get until the test has applied the newer event/admission.
            let script = r#"
held=
while IFS= read -r line; do
    id=${line#*'"id":'}
    id=${id%%,*}
    case "$line" in
        *'"method":"initialized"'*) ;;
        *'"method":"thread/goal/get"'*) held=$id ;;
        *'"method":"barrier"'*)
            if [ -n "$held" ]; then ready=true; else ready=false; fi
            printf '{"id":%s,"result":{"held":%s}}\n' "$id" "$ready" ;;
        *'"method":"release"'*)
            printf '{"id":%s,"result":{"goal":null}}\n' "$held"
            held=
            printf '{"id":%s,"result":{}}\n' "$id" ;;
        *'"method":'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
    esac
done
"#;
            let manager = Manager::new(BridgeConfig {
                codex_bin: "/bin/sh".into(),
                codex_args: vec!["-c".into(), script.into()],
                ..manager.config.clone()
            });
            let state = &manager.state;
            let client = &manager.client;
            state.threads.write().await.insert("t".into(), thread);
            let goal = json!({"status":"active"});
            for change in ["unchanged", "goal_updated", "new_admission"] {
                drop(
                    state
                        .acquire_writer("t", &project, "workspace-write")
                        .await
                        .unwrap(),
                );
                {
                    let mut threads = state.threads.write().await;
                    let t = threads.get_mut("t").unwrap();
                    t.native_goal = Some(goal.clone());
                    t.status = WorkerStatus::Idle;
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    let update = async {
                        while client.request("barrier", json!({})).await.unwrap()["held"] != true {}
                        match change {
                            "goal_updated" => {
                                // Identical contents still invalidate the older null response.
                                state
                                    .apply_native_notification(
                                        "thread/goal/updated",
                                        &json!({"threadId":"t","goal":goal}),
                                    )
                                    .await;
                            }
                            "new_admission" => {
                                drop(
                                    state
                                        .acquire_writer("t", &project, "workspace-write")
                                        .await
                                        .unwrap(),
                                );
                                state.threads.write().await.get_mut("t").unwrap().status =
                                    WorkerStatus::Idle;
                            }
                            _ => {}
                        }
                        client.request("release", json!({})).await.unwrap();
                    };
                    let (result, ()) = tokio::join!(manager.refresh_native_goal("t"), update);
                    result.unwrap();
                })
                .await
                .expect("goal/get barrier timed out");
                state.release_idle_writer("t").await;
                let retained = change != "unchanged";
                assert_eq!(
                    anchor_file_lease::ProjectLease::acquire(&project).is_err(),
                    retained,
                    "old goal/get released the writer after {change}"
                );
                assert_eq!(
                    state.threads.read().await["t"].native_goal,
                    retained.then(|| goal.clone()),
                    "old goal/get overwrote the goal after {change}"
                );
            }
            manager.shutdown().await;
        }
    }
}
