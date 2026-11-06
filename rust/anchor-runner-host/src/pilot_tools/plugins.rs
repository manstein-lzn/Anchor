//! The plugins tools the assistant may call.
//!
//! Split out of the single dispatch in `pilot_tools`; the table there stays
//! the only description of what these tools accept.

use super::*;

impl PilotTools {
    pub(super) async fn plugin_candidate(
        &self,
        name: &str,
        arguments: &Value,
    ) -> Result<Value, ToolError> {
        self.require_active_turn()?;
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| rejected("Pilot Turn context unavailable"))?;
        let directory = parameter(arguments, "directory")?;
        let candidate = workspace_directory(&self.task_workspace(), &directory)?;
        let id = arguments
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| candidate.file_name().and_then(|name| name.to_str()))
            .filter(|id| anchor_runtime::identifier::is_plugin_id(id))
            .ok_or_else(|| rejected("id must be a Plugin identifier"))?;
        let invocation = match (arguments.get("server"), arguments.get("tool")) {
            (None, None) if arguments.get("arguments").is_none() => None,
            (Some(Value::String(server)), Some(Value::String(tool))) => Some((
                server.as_str(),
                tool.as_str(),
                arguments
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            )),
            _ => {
                return Err(rejected(
                    "pass server and tool together, with optional object arguments",
                ));
            }
        };
        let prepared = self
            .application
            .prepare_plugin_candidate(
                &self.library_root,
                &candidate,
                id,
                &self.preparation_root()?,
                &context.dependency_access(),
            )
            .await
            .map_err(|error| rejected(format!("Plugin preparation failed: {error:?}")))?;
        if name == "plugin_prepare" {
            let mut result = prepared.result(context.network);
            result["rust_development"] = context.rust_status();
            result["python_development"] = context.python_status();
            return Ok(result);
        }
        self.application
            .test_plugin_candidate(&prepared, &context.candidate_test_context(), invocation)
            .await
            .map_err(|error| rejected(format!("Plugin candidate test failed: {error:?}")))
    }
    pub(super) async fn plugin_list(&self, arguments: &Value) -> Result<Value, ToolError> {
        let catalog =
            crate::application::plugins::catalog::InstalledPluginCatalog::read(&self.library_root)
                .map_err(|error| rejected(format!("{error:?}")))?;
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let exact = arguments.get("plugin").and_then(Value::as_str);
        let mut rows = Vec::new();
        for entry in &catalog.entries {
            if exact.is_some_and(|id| id != entry.id) {
                continue;
            }
            if exact.is_none() && !query.trim().is_empty() {
                let document = entry
                    .metadata
                    .as_ref()
                    .map(crate::plugin_disclosure::metadata_search_document)
                    .unwrap_or_else(|| entry.id.clone());
                if !crate::plugin_disclosure::matches(query, &document) {
                    continue;
                }
            }
            let mut row = match &entry.metadata {
                Some(metadata) => json!({"id":entry.id,
                    "name":crate::plugin_disclosure::summary(&metadata.name),
                    "description":crate::plugin_disclosure::summary(&metadata.description),
                    "skill_count":metadata.skills.iter().filter(|path| path.as_str() != "instructions.md").count(),
                    "mcpServerNames":metadata.mcp_servers.iter().map(|server| &server.name).collect::<Vec<_>>(),
                    "network_required":crate::plugin_network::metadata_requires_network(metadata),
                    "dependency_count":metadata.dependencies.python.len(),
                    "read_with":"plugin_read"}),
                None => json!({"id":entry.id, "name":entry.id, "read_with":Value::Null}),
            };
            entry.project_health(&mut row);
            rows.push(row);
        }
        let mut result = crate::plugin_disclosure::page(rows, "plugins", arguments)?;
        result["scope"] = json!("installed_library");
        result["note"] = json!(
            "Installed Library management only. available reports valid manifest/resource metadata and a ready dependency view. Listing does not read every code file, derive an execution digest, mount, authorize or connect servers; contextAuthorized and connectionVerified are unknown. Read a selected installed Plugin using plugin_read; details can fail if its current code cannot be read. Execution capabilities are listed separately by anchor_plugins."
        );
        Ok(result)
    }

    pub(super) async fn plugin_read(&self, arguments: &Value) -> Result<Value, ToolError> {
        let plugin = parameter(arguments, "plugin")?;
        let catalog = crate::application::plugins::catalog::InstalledPluginCatalog::read_selected(
            &self.library_root,
            &plugin,
        )
        .map_err(|error| rejected(format!("{error:?}")))?;
        let definition = catalog.definition(&plugin).map_err(|_| failed())?;
        let mut disclosure = crate::plugin_disclosure::PluginDisclosureCatalog::default();
        disclosure.insert(definition);
        let mut selection = arguments.clone();
        if let Some(resource) = arguments.get("resource").and_then(Value::as_str) {
            if arguments.get("section").is_some() || arguments.get("path").is_some() {
                return Err(rejected("use either resource or section/path"));
            }
            selection["section"] = json!("resource");
            selection["path"] = json!(resource);
        }
        let mut result = disclosure.read(&selection)?;
        if selection
            .get("section")
            .and_then(Value::as_str)
            .unwrap_or("overview")
            == "overview"
            && let Some(entry) = catalog.entries.iter().find(|entry| entry.id == plugin)
        {
            entry.project_health(&mut result);
        }
        result["scope"] = json!("installed_library");
        Ok(result)
    }

    pub(super) async fn plugin_review(&self, arguments: &Value) -> Result<Value, ToolError> {
        let directory = parameter(arguments, "directory")?;
        let candidate = workspace_directory(&self.task_workspace(), &directory)?;
        let id = match arguments.get("id").and_then(Value::as_str) {
            Some(id) if anchor_runtime::identifier::is_plugin_id(id) => id.to_owned(),
            Some(_) => return Err(rejected("id must be a Plugin identifier")),
            None => candidate
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| anchor_runtime::identifier::is_plugin_id(name))
                .ok_or_else(|| rejected("the directory name is not a usable Plugin id; pass `id`"))?
                .to_owned(),
        };
        let review = crate::plugin_library::PluginLibrary::new(&self.library_root)
            .review_directory(&id, &candidate)
            .map_err(|failure| match failure {
                // The assistant needs the catalog's reason to fix the
                // manifest; nothing secret is echoed back.
                anchor_library::InstallError::InvalidPluginReason(reason) => {
                    rejected(format!("Plugin candidate rejected: {reason}"))
                }
                anchor_library::InstallError::MissingManifest => {
                    rejected("Plugin candidate has no plugin.json manifest")
                }
                _ => rejected("Plugin candidate could not be reviewed"),
            })?;
        serde_json::to_value(&review).map_err(|_| rejected("Plugin review could not be encoded"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plugin_management_catalog_projects_library_health_without_execution_grants() {
        let root = tempfile::tempdir().unwrap();
        for (id, manifest) in [
            ("healthy", r#"{"name":"Healthy"}"#),
            (
                "needsview",
                r#"{"name":"Needs view","dependencies":{"python":["idna==3.7"]}}"#,
            ),
            ("broken", "{broken"),
        ] {
            let directory = root.path().join("plugins").join(id);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("plugin.json"), manifest).unwrap();
        }
        let skill = root.path().join("plugins/healthy/skills/probe");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: Search Probe\ndescription: unabridged-skill-keyword\n---\nInstructions",
        )
        .unwrap();
        let code = root.path().join("plugins/healthy/code.bin");
        std::fs::write(&code, "ordinary code body").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&code, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let tools = PilotTools {
            application: RunApplication::new(root.path().join("state"), root.path().into()),
            graph_name: "fixture".into(),
            library_root: root.path().into(),
            data_root: root.path().join("state"),
            workspace_root: root.path().join("work"),
            sessions: SessionStore::open(root.path().join("sessions.sqlite")).unwrap(),
            owner: "local".into(),
            session: "pilot".into(),
            turn: String::new(),
            context: None,
        };
        let listed = tools.plugin_list(&json!({})).await.unwrap();
        let catalog =
            crate::application::plugins::catalog::InstalledPluginCatalog::read(root.path())
                .unwrap();
        for entry in &catalog.entries {
            let mut expected = json!({});
            entry.project_health(&mut expected);
            let row = listed["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == entry.id)
                .unwrap();
            for field in [
                "definitionValid",
                "dependenciesReady",
                "contextAuthorized",
                "connectionVerified",
                "available",
                "error",
            ] {
                assert_eq!(row[field], expected[field], "{field}: {row}");
            }
        }
        let selected = tools
            .plugin_read(&json!({"plugin":"needsview"}))
            .await
            .unwrap();
        assert_eq!(selected["definitionValid"], true);
        assert_eq!(selected["dependenciesReady"], false);
        assert_eq!(selected["available"], false);
        let search = tools
            .plugin_list(&json!({"query":"unabridged-skill-keyword"}))
            .await
            .unwrap();
        assert_eq!(search["total"], 1);
        assert_eq!(search["plugins"][0]["id"], "healthy");
        assert_eq!(search["plugins"][0]["definitionValid"], true);
        assert!(
            search["plugins"][0].get("digest").is_none(),
            "management metadata must not invent a code identity"
        );
        // On ordinary accounts this is the counterexample: metadata is valid
        // while a complete code read fails. Superusers can bypass file modes.
        if std::fs::read(&code).is_err() {
            assert!(catalog.definition("healthy").is_err());
            assert!(
                tools
                    .plugin_read(&json!({"plugin":"healthy"}))
                    .await
                    .is_err()
            );
        }
        assert!(!listed.to_string().contains(root.path().to_str().unwrap()));
    }
}
