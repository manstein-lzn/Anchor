use std::{
    future::Future,
    io::Read,
    path::{Path, PathBuf},
    pin::Pin,
};

use anchor_graph_host::{FilePluginCatalog, PluginCatalog, PluginDefinition};
use anchor_platform_session::SessionStore;
use anchor_runtime::{
    ToolDefinition, ToolError, ToolPort, ToolResultContent, graph::GraphSnapshot,
};
use serde_json::{Value, json};

use crate::{
    application::{AdmissionOptions, RunApplication, RunTrigger, metadata::PilotRunSource},
    artifacts::HostArtifacts,
    resource_read::open_resource,
};

pub(crate) struct PilotTools {
    pub(crate) application: RunApplication,
    pub(crate) graph_name: String,
    pub(crate) catalog_root: PathBuf,
    pub(crate) library_root: PathBuf,
    pub(crate) data_root: PathBuf,
    pub(crate) workspace_root: PathBuf,
    pub(crate) sessions: SessionStore,
    pub(crate) owner: String,
    pub(crate) session: String,
    pub(crate) turn: String,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
}

fn parameter(arguments: &Value, name: &str) -> Result<String, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ToolError::Failed(format!("{name} must be a string")))
}

fn failed() -> ToolError {
    ToolError::Failed("resource unavailable or malformed".into())
}

fn public_plugin(definition: &PluginDefinition) -> Value {
    json!({"id":definition.id,"name":definition.name,"description":definition.description,
        "skills":definition.skills,"available":true,"digest":definition.digest})
}

fn text_resource(root: &Path, relative: &str) -> Result<String, ToolError> {
    let mut bytes = Vec::new();
    open_resource(root, relative)
        .map_err(|_| failed())?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failed())?;
    if bytes.len() > 1024 * 1024 {
        return Err(ToolError::Failed(
            "text resource exceeds 1 MiB preview limit".into(),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| ToolError::Failed("binary artifact cannot be decoded as text".into()))
}

impl PilotTools {
    pub(crate) fn for_turn(mut self, turn: &str) -> Self {
        self.turn = turn.into();
        self
    }

    fn require_active_turn(&self) -> Result<(), ToolError> {
        let turn = self
            .sessions
            .get_turn(&self.owner, &self.session, &self.turn)
            .map_err(|_| failed())?;
        if turn.status != anchor_platform_session::TurnStatus::Running {
            return Err(ToolError::Failed("Pilot Turn is not active".into()));
        }
        Ok(())
    }

    async fn execute(&self, name: &str, arguments: Value) -> Result<Value, ToolError> {
        match name {
            "graph_create" => {
                self.require_active_turn()?;
                let graph = parameter(&arguments, "name")?;
                self.application
                    .create_graph(&graph, arguments.get("definition").cloned())
                    .await
                    .map_err(|failure| {
                        ToolError::Failed(format!("Graph creation rejected: {failure:?}"))
                    })
            }
            "graph_update" => {
                self.require_active_turn()?;
                let graph = parameter(&arguments, "graph")?;
                let definition = arguments
                    .get("definition")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| ToolError::Failed("definition must be an object".into()))?;
                self.application
                    .update_graph(&graph, definition)
                    .await
                    .map_err(|failure| {
                        ToolError::Failed(format!("Graph update rejected: {failure:?}"))
                    })
            }
            "graph_run" => {
                self.require_active_turn()?;
                let graph = parameter(&arguments, "graph")?;
                let objective = match arguments.get("objective") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(value)) => Some(value.clone()),
                    _ => return Err(ToolError::Failed("objective must be a string".into())),
                };
                let path = self
                    .application
                    .graph_path_checked(&graph)
                    .map_err(|failure| {
                        ToolError::Failed(format!("Graph unavailable: {failure:?}"))
                    })?;
                let lease = self
                    .application
                    .graph_admission_lease(&path)
                    .map_err(|failure| {
                        ToolError::Failed(format!("Run admission rejected: {failure:?}"))
                    })?;
                let (path, bundle) = self.application.load_graph(&graph).map_err(|failure| {
                    ToolError::Failed(format!("Graph unavailable: {failure:?}"))
                })?;
                let run = self
                    .application
                    .admit(
                        graph.clone(),
                        &path,
                        bundle,
                        arguments.get("input").cloned().unwrap_or_else(|| json!({})),
                        AdmissionOptions {
                            objective,
                            trigger: RunTrigger::default(),
                            pilot: Some(PilotRunSource {
                                owner: self.owner.clone(),
                                session: self.session.clone(),
                                turn: self.turn.clone(),
                            }),
                            oauth_owner: Some(crate::api::oauth::binding_owner(&self.owner)),
                        },
                        lease,
                    )
                    .await
                    .map_err(|failure| {
                        ToolError::Failed(format!("Run admission rejected: {failure:?}"))
                    })?;
                let mut result =
                    json!({"run":run,"graph":graph,"session":self.session,"accepted":true});
                if self
                    .sessions
                    .associate_run(&self.owner, &self.session, &self.turn, &run)
                    .is_err()
                {
                    result["association_error"] = json!(
                        "Run accepted; Session association pending reconciliation. Do not retry graph_run; inspect this Run ID."
                    );
                }
                Ok(result)
            }
            "run_pause" | "run_resume" | "run_stop" => {
                self.require_active_turn()?;
                let run = parameter(&arguments, "run")?;
                if !identifier(&run) {
                    return Err(failed());
                }
                let operation = name.strip_prefix("run_").expect("matched Run control");
                self.application
                    .control(&run, operation)
                    .await
                    .map_err(|failure| {
                        ToolError::Failed(format!("Run control rejected: {failure:?}"))
                    })?;
                Ok(json!({"run":run,"operation":operation,"requested":true}))
            }
            "graph_list" => {
                let mut names = vec![self.graph_name.clone()];
                if let Ok(entries) = std::fs::read_dir(&self.catalog_root) {
                    for entry in entries {
                        let entry = entry.map_err(|_| failed())?;
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if identifier(&name)
                            && !names.contains(&name)
                            && entry.file_type().is_ok_and(|kind| kind.is_dir())
                            && entry.path().join("graph.json").is_file()
                        {
                            names.push(name);
                        }
                    }
                }
                names.sort();
                let mut graphs = Vec::new();
                for graph in names {
                    let active = self.application.active_runs(Some(&graph)).await;
                    graphs.push(json!({"graph":graph,"active_runs":active}));
                }
                Ok(json!({"graphs":graphs}))
            }
            "graph_read" => {
                let graph = parameter(&arguments, "graph")?;
                if !identifier(&graph) {
                    return Err(failed());
                }
                let definition =
                    text_resource(&self.application.graph_bundle_path(&graph), "graph.json")?;
                Ok(
                    json!({"graph":graph,"definition":serde_json::from_str::<Value>(&definition).map_err(|_| failed())?}),
                )
            }
            "graph_validate" => {
                let definition = arguments
                    .get("definition")
                    .filter(|value| value.is_object())
                    .ok_or_else(failed)?;
                let validation = (|| {
                    let snapshot =
                        GraphSnapshot::from_authoring(definition.clone()).map_err(|_| failed())?;
                    crate::reject_snapshot(&snapshot).map_err(|_| failed())?;
                    let ids = snapshot
                        .nodes
                        .iter()
                        .flat_map(|node| node.plugins.iter().cloned())
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>();
                    FilePluginCatalog::new(&self.library_root)
                        .resolve(&ids)
                        .map_err(|_| failed())?;
                    Ok::<_, ToolError>(snapshot)
                })();
                match validation {
                    Ok(snapshot) => Ok(
                        json!({"valid":true,"entry":snapshot.entry,"nodes":snapshot.nodes.iter().map(|node| &node.id).collect::<Vec<_>>()}),
                    ),
                    Err(_) => {
                        Ok(json!({"valid":false,"error":"invalid Graph or unavailable Plugin"}))
                    }
                }
            }
            "plugin_list" => {
                let catalog = FilePluginCatalog::new(&self.library_root);
                let mut plugins = Vec::new();
                match std::fs::read_dir(self.library_root.join("plugins")) {
                    Ok(entries) => {
                        for entry in entries {
                            let entry = entry.map_err(|_| failed())?;
                            let id = entry.file_name().to_string_lossy().into_owned();
                            if id.starts_with('.') {
                                continue;
                            }
                            plugins.push(match catalog.definition(&id) {
                                Ok(definition) => public_plugin(&definition),
                                Err(_) => json!({"id":id,"available":false}),
                            });
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(failed()),
                }
                plugins.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
                Ok(json!({"plugins":plugins}))
            }
            "plugin_read" => {
                let plugin = parameter(&arguments, "plugin")?;
                let definition = FilePluginCatalog::new(&self.library_root)
                    .definition(&plugin)
                    .map_err(|_| failed())?;
                let mut result = public_plugin(&definition);
                let mut instructions = Vec::new();
                if definition.directory.join("instructions.md").is_file() {
                    instructions.push(text_resource(&definition.directory, "instructions.md")?);
                } else {
                    for skill in &definition.skills {
                        instructions.push(text_resource(&definition.directory, skill)?);
                    }
                }
                result["instructions"] = Value::String(instructions.join("\n\n"));
                Ok(result)
            }
            "run_list" => {
                let runs = self
                    .application
                    .records()
                    .map_err(|_| failed())?
                    .into_iter()
                    .map(|(id, record)| json!({"run":id,"status":record.status}))
                    .collect::<Vec<_>>();
                Ok(json!({"runs":runs}))
            }
            "run_status" => {
                let run = parameter(&arguments, "run")?;
                if !identifier(&run) {
                    return Err(failed());
                }
                let record = self
                    .application
                    .records()
                    .map_err(|_| failed())?
                    .into_iter()
                    .find(|(id, _)| id == &run)
                    .ok_or_else(failed)?
                    .1;
                Ok(json!({"run":run,"record":record}))
            }
            "artifact_read" => {
                let run = parameter(&arguments, "run")?;
                let node = parameter(&arguments, "node")?;
                let path = parameter(&arguments, "path")?;
                if !identifier(&run) {
                    return Err(failed());
                }
                let record = self
                    .application
                    .records()
                    .map_err(|_| failed())?
                    .into_iter()
                    .find(|(id, _)| id == &run)
                    .ok_or_else(failed)?
                    .1;
                let result = record
                    .results
                    .get(&node)
                    .and_then(|results| results.last())
                    .ok_or_else(failed)?;
                let artifacts = HostArtifacts::new(
                    self.data_root.join("artifacts"),
                    self.workspace_root.clone(),
                );
                let files = artifacts.files_path(&result.commit).map_err(|_| failed())?;
                let text = text_resource(&files, &path)?;
                Ok(json!({"run":run,"node":node,"path":path,"text":text}))
            }
            "session_wait" => {
                let session = self
                    .sessions
                    .get(&self.owner, &self.session)
                    .map_err(|_| failed())?;
                let retained = self
                    .application
                    .records()
                    .map_err(|_| failed())?
                    .into_iter()
                    .filter(|(id, _)| session.run_ids.contains(id))
                    .map(|(id, record)| json!({"run":id,"record":record}))
                    .collect::<Vec<_>>();
                Ok(json!({"session":session,"runs":retained}))
            }
            _ => Err(ToolError::Unknown(name.into())),
        }
    }
}

impl ToolPort for PilotTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let object = json!({"type":"object","properties":{},"additionalProperties":false});
        let single = |name: &str| json!({"type":"object","properties":{name:{"type":"string"}},"required":[name],"additionalProperties":false});
        [
            ("graph_list", "List saved Graphs without running them", object.clone()),
            ("graph_read", "Read one saved Graph definition", single("graph")),
            ("graph_validate", "Validate a Graph definition without saving or running it", json!({"type":"object","properties":{"definition":{"type":"object"}},"required":["definition"],"additionalProperties":false})),
            ("graph_create", "Create a saved Graph the user explicitly requested; this does not run it", json!({"type":"object","properties":{"name":{"type":"string"},"definition":{"type":"object"}},"required":["name"],"additionalProperties":false})),
            ("graph_update", "Update a saved Graph the user explicitly requested; frozen Run resources are preserved", json!({"type":"object","properties":{"graph":{"type":"string"},"definition":{"type":"object"}},"required":["graph","definition"],"additionalProperties":false})),
            ("graph_run", "Start the Graph the user explicitly requested; returns an accepted Run ID, not completion", json!({"type":"object","properties":{"graph":{"type":"string"},"objective":{"type":"string"},"input":{}},"required":["graph"],"additionalProperties":false})),
            ("plugin_list", "List installed Plugins and their availability", object.clone()),
            ("plugin_read", "Read one Plugin's instructions without executing it", single("plugin")),
            ("run_list", "List durable Graph Runs", object.clone()),
            ("run_status", "Read one durable Run state", single("run")),
            ("run_pause", "Request a pause at the next Graph node boundary; inspect run_status afterwards", single("run")),
            ("run_resume", "Resume an existing Run through the same Runner and frozen resources", single("run")),
            ("run_stop", "Request cancellation of an existing Run; inspect run_status afterwards", single("run")),
            ("artifact_read", "Read a committed UTF-8 artifact (at most 1 MiB), not a live workspace", json!({"type":"object","properties":{"run":{"type":"string"},"node":{"type":"string"},"path":{"type":"string"}},"required":["run","node","path"],"additionalProperties":false})),
            ("session_wait", "Read this Session and its associated Runs without waiting", object),
        ].into_iter().map(|(name, description, parameters)| ToolDefinition { name:name.into(), description:description.into(), parameters }).collect()
    }

    fn is_read_only(&self, name: &str) -> bool {
        matches!(
            name,
            "graph_list"
                | "graph_read"
                | "graph_validate"
                | "plugin_list"
                | "plugin_read"
                | "run_list"
                | "run_status"
                | "artifact_read"
                | "session_wait"
        )
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let result = self.execute(name, arguments).await?;
            Ok(vec![ToolResultContent::text(result.to_string())])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_platform_session::CreateSession;

    fn fixture() -> (tempfile::TempDir, PilotTools) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("fixture")).unwrap();
        std::fs::write(root.path().join("fixture/graph.json"), r#"{"entry":"idle","ops":{"idle":{"run":"true"}},"nodes":[{"id":"idle","op":"idle"}],"edges":[]}"#).unwrap();
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
            catalog_root: root.path().to_path_buf(),
            library_root: root.path().to_path_buf(),
            data_root: root.path().join("state"),
            workspace_root: root.path().join("work"),
            sessions,
            owner: "local".into(),
            session: "pilot".into(),
            turn: String::new(),
        };
        (root, tools)
    }

    #[tokio::test]
    async fn read_tools_do_not_create_runs_or_modify_definitions() {
        let (root, tools) = fixture();
        let original = std::fs::read(root.path().join("fixture/graph.json")).unwrap();
        assert_eq!(tools.definitions().len(), 15);
        assert_eq!(
            tools
                .definitions()
                .iter()
                .filter(|tool| tools.is_read_only(&tool.name))
                .count(),
            9
        );
        assert!(!tools.is_read_only("graph_run"));
        assert_eq!(
            tools
                .execute("graph_read", json!({"graph":"fixture"}))
                .await
                .unwrap()["definition"]["entry"],
            "idle"
        );
        assert_eq!(
            tools
                .execute(
                    "graph_validate",
                    json!({"definition":serde_json::from_slice::<Value>(&original).unwrap()})
                )
                .await
                .unwrap()["valid"],
            true
        );
        assert_eq!(
            tools.execute("plugin_list", json!({})).await.unwrap(),
            json!({"plugins":[]})
        );
        assert_eq!(
            tools.execute("run_list", json!({})).await.unwrap(),
            json!({"runs":[]})
        );
        assert_eq!(
            tools.execute("session_wait", json!({})).await.unwrap()["session"]["id"],
            "pilot"
        );
        assert!(
            tools
                .execute("graph_run", json!({"graph":"fixture"}))
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("fixture/graph.json")).unwrap(),
            original
        );
        assert!(!root.path().join("state").exists());
    }

    #[tokio::test]
    async fn graph_mutations_require_an_owned_running_turn_and_do_not_start_runs() {
        let (root, tools) = fixture();
        let definition = json!({"entry":"idle","ops":{"idle":{"run":"true"}},
            "nodes":[{"id":"idle","op":"idle"}],"edges":[]});
        assert!(
            tools
                .execute(
                    "graph_create",
                    json!({"name":"new","definition":definition})
                )
                .await
                .is_err()
        );
        let (turn, _) = tools
            .sessions
            .create_turn("local", "pilot", "write-fixture", Some("create a Graph"))
            .unwrap();
        let mut tools = tools.for_turn(&turn.id);
        tools.owner = "responses-other".into();
        assert!(
            tools
                .execute(
                    "graph_create",
                    json!({"name":"new","definition":definition})
                )
                .await
                .is_err()
        );
        assert!(!root.path().join("new").exists());
        tools.owner = "local".into();
        let created = tools
            .execute(
                "graph_create",
                json!({"name":"new","definition":definition}),
            )
            .await
            .unwrap();
        assert_eq!(created["graph"], "new");
        assert!(tools.application.records().unwrap().is_empty());
        let mut updated = definition;
        updated["objective"] = json!("updated only, not executed");
        tools
            .execute("graph_update", json!({"graph":"new","definition":updated}))
            .await
            .unwrap();
        assert_eq!(
            tools
                .execute("graph_read", json!({"graph":"new"}))
                .await
                .unwrap()["definition"],
            updated
        );
        assert!(tools.application.records().unwrap().is_empty());
        assert!(
            tools
                .execute("graph_delete", json!({"graph":"new"}))
                .await
                .is_err()
        );
        tools
            .sessions
            .finish_turn(
                "local",
                "pilot",
                &turn.id,
                anchor_platform_session::TurnStatus::Completed,
                None,
            )
            .unwrap();
        assert!(
            tools
                .execute("graph_update", json!({"graph":"new","definition":updated}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn graph_paths_symlinks_and_plugin_credentials_fail_closed() {
        let (root, tools) = fixture();
        assert!(
            tools
                .execute("graph_read", json!({"graph":"../secret"}))
                .await
                .is_err()
        );
        std::fs::write(root.path().join("secret.json"), "host-secret").unwrap();
        std::fs::remove_file(root.path().join("fixture/graph.json")).unwrap();
        std::os::unix::fs::symlink(
            root.path().join("secret.json"),
            root.path().join("fixture/graph.json"),
        )
        .unwrap();
        assert!(
            tools
                .execute("graph_read", json!({"graph":"fixture"}))
                .await
                .is_err()
        );
        let plugin = root.path().join("plugins/demo");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.json"), r#"{"name":"Demo","mcpServers":{"remote":{"url":"https://private.invalid/?key=do-not-expose","headers":{"Authorization":"private-key"}}}}"#).unwrap();
        let public = tools
            .execute("plugin_list", json!({}))
            .await
            .unwrap()
            .to_string();
        assert!(
            !public.contains("private-key")
                && !public.contains("private.invalid")
                && !public.contains("do-not-expose"),
            "{public}"
        );
    }
}
