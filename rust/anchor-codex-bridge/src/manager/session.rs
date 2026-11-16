//! Project-scoped native lifecycle; loading and forking never send a task prompt.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Models {
    cursor: Option<String>,
    #[serde(default = "page_size")]
    limit: u32,
    #[serde(default)]
    include_hidden: bool,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HistoryView {
    #[default]
    Turns,
    Items,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct History {
    thread_id: String,
    #[serde(default)]
    view: HistoryView,
    turn_id: Option<String>,
    cursor: Option<String>,
    #[serde(default = "page_size")]
    limit: u32,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Load,
    Unload,
    Fork,
    Rename,
    Configure,
    Unarchive,
    Compact,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Session {
    thread_id: String,
    action: Action,
    title: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    worktree: Option<String>,
    base_ref: Option<String>,
}
#[derive(Default)]
pub(super) struct Fork {
    pub title: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub worktree: Option<String>,
    pub base_ref: Option<String>,
    pub ephemeral: bool,
}
fn page_size() -> u32 {
    20
}
fn validate_page(limit: u32) -> Result<(), String> {
    if (1..=100).contains(&limit) {
        Ok(())
    } else {
        Err("limit must be between 1 and 100".into())
    }
}

impl Manager {
    pub(super) async fn models(&self, a: Models) -> Result<Value, String> {
        validate_page(a.limit)?;
        self.client
            .request(
                "model/list",
                json!({
                    "cursor": a.cursor, "limit": a.limit, "includeHidden": a.include_hidden
                }),
            )
            .await
    }

    pub(super) async fn history(&self, a: History, roots: &[PathBuf]) -> Result<Value, String> {
        validate_page(a.limit)?;
        if matches!(a.view, HistoryView::Turns) && a.turn_id.is_some() {
            return Err("turn_id requires view items".into());
        }
        self.thread(&a.thread_id, roots, false).await?;
        let mut params = json!({
            "threadId": a.thread_id, "cursor": a.cursor,
            "limit": a.limit, "sortDirection": "desc"
        });
        let method = match a.view {
            HistoryView::Turns => "thread/turns/list",
            HistoryView::Items => {
                if let Some(id) = a.turn_id {
                    params["turnId"] = json!(id);
                }
                "thread/items/list"
            }
        };
        self.client.request(method, params).await
    }

    // Shared by explicit load and task continuation; replaces the inline resume path.
    pub(super) async fn load_thread(
        &self,
        t: &TrackedThread,
        mode: &str,
        approval: Approval,
    ) -> Result<(), String> {
        if t.loaded && t.subscribed {
            return Ok(());
        }
        if t.archived {
            return Err("archived session: use session unarchive first".into());
        }
        self.check_writer(&t.cwd, mode, Some(&t.thread_id)).await?;
        let _lease = self
            .client
            .prepare_writer(&t.thread_id, &t.cwd, mode)
            .await?;
        if let Some(current) = self.state.threads.write().await.get_mut(&t.thread_id) {
            if mode == "workspace-write" {
                current.mode = mode.into();
            }
            current.status = WorkerStatus::Unknown;
        }
        let response = self
            .client
            .writer_request(
                "thread/resume",
                json!({
                    "threadId": t.thread_id, "sandbox": mode, "cwd": t.cwd,
                    "approvalPolicy": approval.name(), "approvalsReviewer": "user"
                }),
                &t.thread_id,
                &t.cwd,
                mode,
            )
            .await?;
        if let Some(current) = self.state.threads.write().await.get_mut(&t.thread_id) {
            current.loaded = true;
            current.subscribed = true;
            current.mode = mode.into();
            if current.active_turn_id.is_none() {
                current.status = WorkerStatus::Idle;
            }
            current.model = response["model"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| current.model.clone());
            current.reasoning_effort = response["reasoningEffort"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| current.reasoning_effort.clone());
        }
        self.refresh_native_goal(&t.thread_id).await?;
        self.refresh_native_queue(&t.thread_id).await?;
        self.state.release_idle_writer(&t.thread_id).await;
        Ok(())
    }

    // One native fork path for persistent workstreams and ephemeral side conversations.
    pub(super) async fn fork_thread(
        &self,
        parent: &TrackedThread,
        a: &Fork,
    ) -> Result<String, String> {
        if a.base_ref.is_some() && a.worktree.is_none() {
            return Err("base_ref requires worktree".into());
        }
        let prepared = match &a.worktree {
            Some(slug) => Some(
                git::ensure_worktree(
                    &self.config.worktree_root,
                    parent.project_path.as_deref().unwrap_or(&parent.cwd),
                    slug,
                    a.base_ref.as_deref(),
                )
                .await?,
            ),
            None => None,
        };
        let cwd = prepared
            .as_ref()
            .map_or_else(|| parent.cwd.clone(), |w| w.path.clone());
        let mut params = json!({
            "threadId": parent.thread_id, "ephemeral": a.ephemeral, "excludeTurns": true,
            "cwd": cwd, "sandbox": "read-only",
            "approvalPolicy": "never", "approvalsReviewer": "user"
        });
        if !a.ephemeral {
            params["deferGoalContinuation"] = json!(true);
        }
        if let Some(model) = a.model.as_ref().or(parent.model.as_ref()) {
            params["model"] = json!(model);
        }
        if let Some(effort) = a
            .reasoning_effort
            .as_ref()
            .or(parent.reasoning_effort.as_ref())
        {
            params["config"] = json!({"model_reasoning_effort": effort});
        }
        let response = self.client.request("thread/fork", params).await?;
        let id = response["thread"]["id"]
            .as_str()
            .ok_or("thread/fork returned no thread id")?
            .to_owned();
        let mut thread = TrackedThread::new(
            id.clone(),
            a.title.clone(),
            parent.project_path.clone(),
            cwd,
            a.worktree.clone().or_else(|| parent.worktree_slug.clone()),
            "read-only".into(),
        );
        thread.git_common_dir = prepared
            .map(|w| w.git_common_dir)
            .or_else(|| parent.git_common_dir.clone());
        if a.ephemeral {
            thread.parent_thread_id = Some(parent.thread_id.clone());
        }
        thread.model = response["model"].as_str().map(str::to_owned);
        thread.reasoning_effort = response["reasoningEffort"].as_str().map(str::to_owned);
        self.state.threads.write().await.insert(id.clone(), thread);
        if !a.ephemeral {
            self.refresh_native_goal(&id).await?;
        }
        // Ephemeral titles live only in the projection; no persistent name write.
        if !a.ephemeral
            && let Some(title) = &a.title
        {
            self.client
                .request("thread/name/set", json!({"threadId": id, "name": title}))
                .await
                .map_err(|e| {
                    format!("fork {id} created but naming failed: {e}; inspect before retrying")
                })?;
        }
        Ok(id)
    }

    pub(super) async fn session(&self, a: Session, roots: &[PathBuf]) -> Result<Value, String> {
        if a.title.as_ref().is_some_and(|s| s.trim().is_empty())
            || a.model.as_ref().is_some_and(|s| s.trim().is_empty())
            || a.reasoning_effort
                .as_ref()
                .is_some_and(|s| s.trim().is_empty())
        {
            return Err("title, model and reasoning_effort must be nonempty when supplied".into());
        }
        if (a.title.is_some() && !matches!(a.action, Action::Rename | Action::Fork))
            || ((a.model.is_some() || a.reasoning_effort.is_some())
                && !matches!(a.action, Action::Configure | Action::Fork))
            || ((a.worktree.is_some() || a.base_ref.is_some()) && !matches!(a.action, Action::Fork))
        {
            return Err("parameters do not apply to this session action; title: rename/fork, model/effort: configure/fork, worktree/base_ref: fork".into());
        }
        if matches!(a.action, Action::Rename) && a.title.is_none() {
            return Err("rename requires title".into());
        }
        if matches!(a.action, Action::Configure)
            && a.model.is_none()
            && a.reasoning_effort.is_none()
        {
            return Err("configure requires model or reasoning_effort".into());
        }
        let t = self.thread(&a.thread_id, roots, true).await?;
        if t.parent_thread_id.is_some() && !matches!(a.action, Action::Configure | Action::Rename) {
            return Err(
                "temporary btw conversations use inspect, steer_or_resume and control close".into(),
            );
        }
        if t.archived && !matches!(a.action, Action::Unarchive | Action::Rename) {
            return Err("archived session: use session unarchive first".into());
        }
        let mut native = Value::Null;
        let action = match a.action {
            Action::Load => {
                self.load_thread(&t, &t.mode, Approval::OnRequest).await?;
                "loaded"
            }
            Action::Unload => {
                if busy(&t) {
                    return Err("session is busy; inspect or interrupt before unloading".into());
                }
                native = self
                    .client
                    .request("thread/unsubscribe", json!({"threadId": t.thread_id}))
                    .await?;
                if let Some(current) = self.state.threads.write().await.get_mut(&t.thread_id) {
                    current.subscribed = false;
                    if native["status"] == "notLoaded" {
                        current.loaded = false;
                    }
                }
                "unsubscribe_requested"
            }
            Action::Fork => {
                let id = self
                    .fork_thread(
                        &t,
                        &Fork {
                            title: a.title,
                            model: a.model,
                            reasoning_effort: a.reasoning_effort,
                            worktree: a.worktree,
                            base_ref: a.base_ref,
                            ephemeral: false,
                        },
                    )
                    .await?;
                let mut value = self.wait_summary(&id, 0).await?;
                value["action"] = json!("forked");
                value["source_thread_id"] = json!(t.thread_id);
                return Ok(value);
            }
            Action::Rename => {
                self.client
                    .request(
                        "thread/name/set",
                        json!({"threadId": t.thread_id, "name": a.title}),
                    )
                    .await?;
                if let Some(current) = self.state.threads.write().await.get_mut(&t.thread_id) {
                    current.title = a.title;
                }
                "renamed"
            }
            Action::Configure => {
                self.load_thread(&t, &t.mode, Approval::OnRequest).await?;
                let mut params = json!({"threadId": t.thread_id});
                if let Some(model) = a.model {
                    params["model"] = json!(model);
                }
                if let Some(effort) = a.reasoning_effort {
                    params["effort"] = json!(effort);
                }
                self.client
                    .request("thread/settings/update", params)
                    .await?;
                // Effective values arrive through thread/settings/updated, not guessed inputs.
                "settings_updated_for_next_turn"
            }
            Action::Unarchive => {
                self.client
                    .request("thread/unarchive", json!({"threadId": t.thread_id}))
                    .await?;
                if let Some(current) = self.state.threads.write().await.get_mut(&t.thread_id) {
                    current.archived = false;
                }
                "unarchived"
            }
            Action::Compact => {
                if busy(&t) {
                    return Err("session is busy; inspect or interrupt before compaction".into());
                }
                self.load_thread(&t, &t.mode, Approval::OnRequest).await?;
                // Reserve admission until native turn events or inspect reconcile it.
                self.check_writer(&t.cwd, &t.mode, Some(&t.thread_id))
                    .await?;
                let _lease = self
                    .client
                    .prepare_writer(&t.thread_id, &t.cwd, &t.mode)
                    .await?;
                if let Some(current) = self.state.threads.write().await.get_mut(&t.thread_id) {
                    current.admission_epoch += 1;
                    current.status = WorkerStatus::Unknown;
                }
                self.client
                    .writer_request(
                        "thread/compact/start",
                        json!({"threadId": t.thread_id}),
                        &t.thread_id,
                        &t.cwd,
                        &t.mode,
                    )
                    .await
                    .map_err(|e| {
                        format!("compaction admission uncertain: {e}; inspect before retrying")
                    })?;
                "compaction_requested"
            }
        };
        let threads = self.state.threads.read().await;
        let current = threads.get(&a.thread_id).ok_or("session disappeared")?;
        let mut value = current.inspect_detail(None);
        value["action"] = json!(action);
        if !native.is_null() {
            value["native"] = native;
        }
        Ok(value)
    }
}
