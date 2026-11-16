//! Direct project access reuses NodeTools; the resident's own workspace stays intact.
use anchor_runtime::{Cancellation, ToolDefinition, ToolError, ToolPort, ToolResultContent};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::Future,
    os::unix::fs::MetadataExt,
    path::{Component, PathBuf},
    pin::Pin,
    sync::{Arc, atomic::Ordering},
};

#[derive(Default)]
pub(crate) struct ProjectGrants(BTreeMap<String, Arc<ProjectGrant>>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    owner: String,
    path: PathBuf,
    #[serde(default)]
    exclude: Vec<PathBuf>,
}

pub(crate) struct ProjectGrant {
    pub(crate) owner: String,
    pub(crate) path: PathBuf,
    pub(crate) exclude: Vec<PathBuf>,
    identity: (u64, u64),
}

impl ProjectGrants {
    pub(crate) fn get(&self, graph: &str) -> Option<Arc<ProjectGrant>> {
        self.0.get(graph).cloned()
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        let entries: BTreeMap<String, Configuration> = serde_json::from_str(value)
            .map_err(|error| format!("ANCHOR_PROJECT_WORKSPACES: {error}"))?;
        let mut grants = BTreeMap::new();
        for (graph, configuration) in entries {
            if !anchor_runtime::identifier::is_name(&graph)
                || !anchor_runtime::identifier::is_name(&configuration.owner)
            {
                return Err("project grants require explicit Graph and owner identities".into());
            }
            let canonical = configuration
                .path
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if canonical != configuration.path || canonical.parent().is_none() {
                return Err("project path must be a canonical absolute directory, not root".into());
            }
            let directory =
                anchor_file_lease::open_directory(&canonical).map_err(|e| e.to_string())?;
            let metadata = directory.metadata().map_err(|e| e.to_string())?;
            for excluded in &configuration.exclude {
                if excluded.as_os_str().is_empty()
                    || excluded
                        .components()
                        .any(|c| !matches!(c, Component::Normal(_)))
                {
                    return Err("excluded project paths must be plain relative directories".into());
                }
                let path = canonical.join(excluded);
                if path.canonicalize().map_err(|e| e.to_string())? != path || !path.is_dir() {
                    return Err(
                        "excluded project paths must be existing non-link directories".into(),
                    );
                }
            }
            grants.insert(
                graph,
                Arc::new(ProjectGrant {
                    owner: configuration.owner,
                    path: canonical,
                    exclude: configuration.exclude,
                    identity: (metadata.dev(), metadata.ino()),
                }),
            );
        }
        Ok(Self(grants))
    }
}

impl ProjectGrant {
    pub(crate) fn verify(&self) -> Result<(), String> {
        let directory = anchor_file_lease::open_directory(&self.path).map_err(|e| e.to_string())?;
        let metadata = directory.metadata().map_err(|e| e.to_string())?;
        if self.path.canonicalize().map_err(|e| e.to_string())? != self.path
            || (metadata.dev(), metadata.ino()) != self.identity
        {
            return Err("project directory changed; operator must renew the binding".into());
        }
        for excluded in &self.exclude {
            anchor_file_lease::open_directory(&self.path.join(excluded))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

pub(crate) type ProjectAuthorization =
    Arc<dyn Fn() -> Result<Arc<ProjectGrant>, String> + Send + Sync>;

pub(crate) fn wrap(
    inner: Arc<dyn ToolPort>,
    tools: Arc<dyn ToolPort>,
    grant: Arc<ProjectGrant>,
    authorize: ProjectAuthorization,
    cancellation: Cancellation,
) -> Arc<dyn ToolPort> {
    Arc::new(ProjectTools {
        inner,
        tools,
        authorize,
        grant,
        cancellation,
    })
}

struct ProjectTools {
    inner: Arc<dyn ToolPort>,
    tools: Arc<dyn ToolPort>,
    authorize: ProjectAuthorization,
    grant: Arc<ProjectGrant>,
    cancellation: Cancellation,
}

fn native(name: &str) -> Option<&'static str> {
    match name {
        "project_read" => Some(crate::tool_names::READ_TOOL_NAME),
        "project_edit" => Some(crate::tool_names::EDIT_TOOL_NAME),
        "project_run" => Some(crate::tool_names::RUN_TOOL_NAME),
        _ => None,
    }
}

impl ToolPort for ProjectTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.inner.definitions();
        definitions.extend(self.tools.definitions().into_iter().map(|mut definition| {
            let name = match definition.name.as_str() {
                crate::tool_names::READ_TOOL_NAME => "project_read",
                crate::tool_names::EDIT_TOOL_NAME => "project_edit",
                crate::tool_names::RUN_TOOL_NAME => "project_run",
                _ => unreachable!("only NodeTools definitions"),
            };
            definition.name = name.into();
            let description = definition.description
                .replace(crate::tool_names::READ_TOOL_NAME, "project_read")
                .replace(crate::tool_names::EDIT_TOOL_NAME, "project_edit")
                .replace(crate::tool_names::RUN_TOOL_NAME, "project_run");
            definition.description = format!(
                "DIRECT local project {}, without Codex. In THIS tool /workspace means the real project, not the Manager scratch workspace. Excluded directories: {:?}. {} Do not use this path to bypass Codex approvals. Other writers return busy; use separate Codex worktrees or wait for handoff.",
                self.grant.path.display(), self.grant.exclude, description
            );
            definition
        }));
        definitions
    }

    fn is_read_only(&self, name: &str) -> bool {
        native(name)
            .map(|name| self.tools.is_read_only(name))
            .unwrap_or_else(|| self.inner.is_read_only(name))
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let Some(native) = native(name) else {
                return self.inner.call(name, arguments).await;
            };
            if self.cancellation.load(Ordering::Acquire) {
                return Err(ToolError::Failed(
                    "project operation cancelled before execution".into(),
                ));
            }
            let grant = (self.authorize)().map_err(ToolError::Failed)?;
            if !Arc::ptr_eq(&grant, &self.grant) {
                return Err(ToolError::Failed(
                    "project grant changed; start a new Turn".into(),
                ));
            }
            let lease = anchor_file_lease::ProjectLease::acquire(&grant.path).map_err(|e| {
                ToolError::Failed(format!(
                    "project busy or unavailable; no operation started: {e}"
                ))
            })?;
            grant.verify().map_err(ToolError::Failed)?;
            let tools = self.tools.clone();
            // Dropping an RPC future must not unlock a still-running command.
            // NodeTools observes the same cancellation token and reaps children.
            tokio::spawn(async move {
                let _lease = lease;
                tools.call(native, arguments).await
            })
            .await
            .map_err(|error| ToolError::Failed(format!("project execution task failed: {error}")))?
        })
    }
}

pub(crate) struct NoTools;
impl ToolPort for NoTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![]
    }
    fn call<'a>(
        &'a self,
        name: &'a str,
        _: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move { Err(ToolError::Unknown(name.into())) })
    }
}

#[cfg(test)]
mod tests;
