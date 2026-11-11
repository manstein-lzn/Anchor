use super::*;

impl PilotTools {
    pub(super) fn oauth_status(&self, arguments: &Value) -> Result<Value, ToolError> {
        let plugin = parameter(arguments, "plugin")?;
        let server = parameter(arguments, "server")?;
        self.application
            .oauth_status(&self.owner, &plugin, &server)
            .map_err(|error| rejected(format!("OAuth status unavailable: {error:?}")))
    }
    pub(super) async fn oauth_authorize(&self, arguments: &Value) -> Result<Value, ToolError> {
        self.require_active_turn()?;
        let object = arguments
            .as_object()
            .ok_or_else(|| rejected("provide plugin and server only"))?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "plugin" | "server"))
        {
            return Err(rejected(
                "provide plugin and server only; tokens and approval are not model arguments",
            ));
        }
        let plugin = parameter(arguments, "plugin")?;
        let server = parameter(arguments, "server")?;
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| rejected("Pilot Turn context unavailable"))?;
        let origin = crate::application::oauth_management::PilotOAuthOrigin {
            sessions: self.sessions.clone(),
            owner: self.owner.clone(),
            session: self
                .sessions
                .get(&self.owner, &self.session)
                .map_err(|_| failed())?,
            turn: self.turn.clone(),
        };
        self.application
            .authorize_pilot_oauth(&origin, &plugin, &server, &context.dependency_access())
            .await
            .map_err(|error| {
                rejected(format!(
                    "OAuth authorization could not be prepared: {error:?}"
                ))
            })
    }
    pub(super) async fn credential_request(&self, arguments: &Value) -> Result<Value, ToolError> {
        self.require_active_turn()?;
        let plugin = parameter(arguments, "plugin")?;
        let server = parameter(arguments, "server")?;
        // A bot's credentials belong to its Graph; MCP credentials do not.
        let server = match (server.starts_with("channel_"), arguments.get("graph")) {
            (true, Some(Value::String(graph))) => {
                let plan = self
                    .application
                    .check_channel_credentials_mutable(&self.owner, graph)
                    .map_err(|error| rejected(format!("{error:?}")))?;
                if plan.plugin != plugin {
                    return Err(rejected("this Graph's bot uses another Plugin"));
                }
                if server != format!("channel_{}", plan.platform.as_str()) {
                    return Err(rejected("this Graph's bot uses another channel platform"));
                }
                format!("{server}@{graph}")
            }
            (true, _) => return Err(rejected("a channel credential requires the bot's graph")),
            (false, None) => server,
            (false, Some(_)) => return Err(rejected("graph applies only to channel credentials")),
        };
        let field = parameter(arguments, "field")?;
        let session = self
            .sessions
            .get(&self.owner, &self.session)
            .map_err(|_| failed())?;
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| rejected("Pilot Turn context unavailable"))?;
        let request = crate::credential_store::request(
            &self.data_root,
            &self.library_root,
            &self.sessions,
            &self.owner,
            &session,
            &self.turn,
            &plugin,
            &server,
            &field,
            arguments
                .get("purpose")
                .and_then(Value::as_str)
                .unwrap_or("Configure the requested Plugin service"),
        )
        .map_err(rejected)?;
        loop {
            if context
                .cancellation
                .load(std::sync::atomic::Ordering::Acquire)
            {
                let _ = crate::credential_store::cancel(
                    &self.data_root,
                    &self.owner,
                    &session,
                    &request.id,
                    true,
                );
                return Err(rejected("credential input interrupted"));
            }
            self.require_active_turn()?;
            let request = crate::credential_store::get(
                &self.data_root,
                &self.sessions,
                &self.owner,
                &session,
                &request.id,
            )
            .map_err(rejected)?;
            if request.status != "pending" {
                if request.status == "configured" {
                    self.request_continuation("owner-bound Plugin credential configured")?;
                }
                return Ok(
                    json!({"request":request,"value_exposed":false,"effective_next_turn":true}),
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    pub(crate) fn request_continuation(&self, reason: &str) -> Result<(), ToolError> {
        self.require_active_turn()?;
        self.sessions
            .append_event(
                &self.owner,
                &self.session,
                "pilot.continuation.requested",
                std::collections::BTreeMap::from([
                    ("turn".into(), json!(self.turn)),
                    (
                        "request_id".into(),
                        json!(format!("continuation:{}", self.turn)),
                    ),
                    ("reason".into(), json!(reason)),
                ]),
            )
            .map_err(|_| rejected("could not persist the Session continuation"))?;
        Ok(())
    }

    pub(super) async fn runtime_prepare(&self, arguments: &Value) -> Result<Value, ToolError> {
        self.require_active_turn()?;
        let kind = parameter(arguments, "kind")?;
        let configuration = self
            .application
            .runtime_configuration()
            .ok_or_else(|| rejected("this deployment has no managed runtime configuration"))?;
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| rejected("Pilot Turn context unavailable"))?;
        let result = configuration
            .prepare(
                &kind,
                arguments.get("version").and_then(Value::as_str),
                &context.dependency_access(),
            )
            .await
            .map_err(rejected)?;
        if result["changed"] == true {
            self.request_continuation("shared runtime prepared")?;
        }
        Ok(result)
    }

    pub(super) async fn development_prepare(&self, arguments: &Value) -> Result<Value, ToolError> {
        self.require_active_turn()?;
        let object = arguments.as_object().ok_or_else(|| {
            rejected("provide project, action and optional inline dependency arrays")
        })?;
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "project" | "action" | "requirements" | "development_requirements"
            )
        }) {
            return Err(rejected(
                "development_prepare accepts project, action and optional inline dependency arrays only; requirements/pyproject files are installed through anchor_run(project=...) pip, and cannot be combined as a second declaration source here",
            ));
        }
        let project = parameter(arguments, "project")?;
        if project != "." {
            workspace_directory(&self.task_workspace(), &project)?;
        }
        let requirements: Vec<String> = serde_json::from_value(
            arguments
                .get("requirements")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map_err(|_| rejected("requirements must be a string array"))?;
        let development_requirements: Vec<String> = serde_json::from_value(
            arguments
                .get("development_requirements")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map_err(|_| rejected("development_requirements must be a string array"))?;
        let session = self
            .sessions
            .get(&self.owner, &self.session)
            .map_err(|_| failed())?;
        let scope = crate::pilot_host::scope(&self.data_root, &self.owner, &session);
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| rejected("Pilot Turn context unavailable"))?;
        let action = serde_json::from_value(
            arguments
                .get("action")
                .cloned()
                .unwrap_or_else(|| json!("ensure")),
        )
        .map_err(|_| rejected("action must be ensure, rebuild or discard"))?;
        if action != crate::application::development_environment::PrepareAction::Discard
            && context.python_status()["configured"] != true
        {
            return Err(rejected(
                "this frozen Turn does not authorize a Pilot Python interpreter",
            ));
        }
        self.application
            .prepare_development_environment(
                &self.library_root,
                &scope,
                &self.task_workspace(),
                &project,
                action,
                &requirements,
                &development_requirements,
                &context.dependency_access(),
            )
            .await
            .map_err(|error| {
                rejected(format!(
                    "development dependency preparation failed: {error:?}"
                ))
            })
    }
}
