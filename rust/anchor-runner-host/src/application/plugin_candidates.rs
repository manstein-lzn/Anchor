//! Scope-owned preparation and bounded MCP probes. These are host use cases,
//! not published Library assets or a second Agent loop.

use super::{ApplicationError, RunApplication};
use crate::dependency_provision::{DependencyAccess, ResolvedRequirements};
use crate::plugin_library::Catalog;
use crate::plugin_library::PluginLibrary;
use anchor_graph_host::host_environment_references;
use anchor_library::PluginReview;
use anchor_runtime::{ToolPort, graph::PluginBinding};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparationFact {
    format: u32,
    pub(crate) id: String,
    pub(crate) digest: String,
    pub(crate) declaration_digest: String,
    pub(crate) resolved: Option<ResolvedRequirements>,
}

pub(crate) struct PreparedCandidate {
    pub(crate) review: PluginReview,
    pub(crate) fact: PreparationFact,
    root: PathBuf,
    view: Option<PathBuf>,
    library_root: PathBuf,
    _dependency_scope: Option<anchor_library::DevelopmentScope>,
    _lease: anchor_file_lease::FileLease,
}

/// Frozen execution authority shared by Pilot and authorized node probes.
#[derive(Clone)]
pub(crate) struct CandidateTestContext {
    pub(crate) sandbox: Arc<anchor_sandbox_bwrap::BubblewrapSandbox>,
    pub(crate) workspace: PathBuf,
    pub(crate) access: DependencyAccess,
}

fn invalid(error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::Invalid(error.to_string())
}

impl PreparedCandidate {
    pub(crate) fn result(&self, network: bool) -> Value {
        json!({"prepared":true,"id":self.fact.id,"digest":self.fact.digest,
            "declaration_digest":self.fact.declaration_digest,
            "lock_digest":self.fact.resolved.as_ref().map(|resolved| &resolved.lock_digest),
            "packages":self.fact.resolved.as_ref().map(|resolved| &resolved.packages).cloned().unwrap_or_default(),
            "interpreter":self.fact.resolved.as_ref().map(|resolved| &resolved.interpreter),
            "network_allowed":network,"review":self.review})
    }
}

pub(crate) fn load_fact(
    root: &Path,
    id: &str,
    digest: &str,
) -> Result<Option<PreparationFact>, ApplicationError> {
    let root = root.join(id).join(digest);
    if let Some(fact) =
        crate::assistant::read_json::<PreparationFact>(&root.join("current-preparation.json"))
            .map_err(invalid)?
    {
        check_fact(&fact, id, digest)?;
        let retained =
            crate::assistant::read_json::<PreparationFact>(&preparation_path(&root, &fact))
                .map_err(invalid)?;
        if retained.as_ref() != Some(&fact) {
            return Err(invalid(
                "selected candidate preparation has no matching immutable ready fact",
            ));
        }
        return Ok(Some(fact));
    }
    let fact = crate::assistant::read_json::<PreparationFact>(&root.join("preparation.json"))
        .map_err(invalid)?;
    if let Some(fact) = &fact {
        check_fact(fact, id, digest)?;
    }
    Ok(fact)
}

fn check_fact(fact: &PreparationFact, id: &str, digest: &str) -> Result<(), ApplicationError> {
    if fact.format != 1 || fact.id != id || fact.digest != digest {
        return Err(invalid("candidate preparation identity changed"));
    }
    if fact.resolved.as_ref().is_some_and(|resolved| {
        resolved.lock_digest != format!("{:x}", Sha256::digest(resolved.lock.as_bytes()))
    }) {
        return Err(invalid("candidate preparation dependency lock changed"));
    }
    Ok(())
}

fn preparation_path(root: &Path, fact: &PreparationFact) -> PathBuf {
    let key = fact
        .resolved
        .as_ref()
        .map(crate::plugin_dependencies::resolved_view_key)
        .unwrap_or_else(|| "without-dependencies".into());
    root.join("preparations").join(format!("{key}.json"))
}

fn matches_interpreter(fact: &PreparationFact, interpreter: Option<&(PathBuf, String)>) -> bool {
    match (&fact.resolved, interpreter) {
        (Some(resolved), Some((path, digest))) => {
            resolved.interpreter_path == *path && resolved.interpreter_digest == *digest
        }
        (None, None) => true,
        _ => false,
    }
}

fn retained_preparation(
    root: &Path,
    current: Option<PreparationFact>,
    review: &PluginReview,
    interpreter: Option<&(PathBuf, String)>,
) -> Result<Option<PreparationFact>, ApplicationError> {
    let declaration = review.dependencies.declaration_digest();
    if let Some(fact) = &current {
        if fact.declaration_digest != declaration {
            return Err(invalid("candidate dependency declaration changed"));
        }
        if matches_interpreter(fact, interpreter) {
            return Ok(current);
        }
    }
    // A Turn that already selected an older runtime must never pick a later
    // Turn's solve. Retain and reuse exact ready facts for each interpreter.
    let mut retained = None;
    let history = root.join("preparations");
    if history.exists() {
        for entry in std::fs::read_dir(&history).map_err(invalid)? {
            let entry = entry.map_err(invalid)?;
            if entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "json")
            {
                continue;
            }
            let fact = crate::assistant::read_json::<PreparationFact>(&entry.path())
                .map_err(invalid)?
                .ok_or_else(|| invalid("candidate preparation ready fact vanished"))?;
            check_fact(&fact, &review.id, &review.digest)?;
            if entry.path() != preparation_path(root, &fact)
                || fact.declaration_digest != declaration
            {
                return Err(invalid("candidate preparation history identity changed"));
            }
            if matches_interpreter(&fact, interpreter) {
                if retained.as_ref().is_some_and(|previous| previous != &fact) {
                    return Err(invalid(
                        "candidate has conflicting ready facts for this interpreter",
                    ));
                }
                retained = Some(fact);
            }
        }
    }
    Ok(retained)
}

fn publish_preparation(root: &Path, fact: &PreparationFact) -> Result<(), ApplicationError> {
    let original = root.join("preparation.json");
    match crate::assistant::read_json::<PreparationFact>(&original).map_err(invalid)? {
        Some(legacy) => {
            check_fact(&legacy, &fact.id, &fact.digest)?;
            crate::assistant::save_immutable(&preparation_path(root, &legacy), &legacy)
                .map_err(invalid)?;
        }
        None => crate::assistant::save_immutable(&original, fact).map_err(invalid)?,
    }
    crate::assistant::save_immutable(&preparation_path(root, fact), fact).map_err(invalid)?;
    // Successful preparation is the only point that changes current selection;
    // earlier facts and views remain available for frozen Turns and inspection.
    let mut temporary = tempfile::NamedTempFile::new_in(root).map_err(invalid)?;
    temporary
        .write_all(&serde_json::to_vec(fact).map_err(invalid)?)
        .map_err(invalid)?;
    temporary.as_file().sync_all().map_err(invalid)?;
    temporary
        .persist(root.join("current-preparation.json"))
        .map_err(invalid)?;
    std::fs::File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(invalid)
}

fn checked_view(
    view: &Path,
    store: &Path,
    resolved: &ResolvedRequirements,
) -> Result<(), ApplicationError> {
    if view.join("bin/python").canonicalize().map_err(invalid)? != resolved.interpreter_path
        || format!(
            "{:x}",
            Sha256::digest(std::fs::read(&resolved.interpreter_path).map_err(invalid)?)
        ) != resolved.interpreter_digest
    {
        return Err(invalid(
            "candidate view no longer names its prepared interpreter",
        ));
    }
    let observed = crate::plugin_view::referenced_store_directories(view, store)
        .map_err(invalid)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let expected = resolved
        .packages
        .iter()
        .map(|package| {
            store
                .join(crate::dependency_provision::normalize_name(&package.name))
                .join(&package.version)
        })
        .collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(invalid(
            "candidate view no longer references its prepared package set",
        ));
    }
    Ok(())
}

impl RunApplication {
    pub(crate) async fn prepare_plugin_candidate(
        &self,
        library_root: &Path,
        directory: &Path,
        id: &str,
        preparation_root: &Path,
        access: &DependencyAccess,
    ) -> Result<PreparedCandidate, ApplicationError> {
        access.check().map_err(invalid)?;
        crate::host_fs::directory(preparation_root).map_err(invalid)?;
        let temporary = tempfile::Builder::new()
            .prefix("candidate-")
            .tempdir_in(preparation_root)
            .map_err(invalid)?;
        let (layout, review) = PluginLibrary::new(library_root)
            .stage_directory(id, directory, temporary.path())
            .map_err(invalid)?;
        let root = preparation_root.join(id).join(&review.digest);
        crate::host_fs::directory(&root).map_err(invalid)?;
        let lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("preparation.lock"))
            .map_err(invalid)?;
        let lease = anchor_file_lease::FileLease::acquire(
            lease,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .map_err(|_| invalid("this candidate is already being prepared or tested"))?;
        let staged = root.join("validation");
        if !staged.exists() {
            std::fs::rename(layout, &staged).map_err(invalid)?;
        }
        Catalog::new(&staged)
            .checked_definition(
                id,
                crate::plugin_library::ExpectedDefinition::CodeDigest(&review.digest),
            )
            .map_err(|error| match error {
                crate::plugin_library::DefinitionError::Read(error) => invalid(error),
                crate::plugin_library::DefinitionError::Mismatch => {
                    invalid("the staged candidate changed; retained preparation cannot cover it")
                }
            })?;
        let existing = load_fact(preparation_root, id, &review.digest)?;
        let interpreter = if review.dependencies.is_empty() {
            None
        } else {
            let provisioner = self.dependency_provisioner()?;
            let path = provisioner.config.python.canonicalize().map_err(invalid)?;
            let digest = format!(
                "{:x}",
                Sha256::digest(std::fs::read(&path).map_err(invalid)?)
            );
            Some((path, digest))
        };
        let resolved = match retained_preparation(&root, existing, &review, interpreter.as_ref())? {
            Some(fact) => fact.resolved,
            None => {
                self.plugin_dependency_plan(library_root, &review, access)
                    .await?
                    .1
            }
        };
        let mut dependency_scope = None;
        let view = if let Some(resolved) = &resolved {
            let scope = PluginLibrary::new(library_root)
                .development_scope(
                    preparation_root
                        .parent()
                        .ok_or_else(|| invalid("candidate scope unavailable"))?,
                    true,
                )
                .map_err(invalid)?
                .ok_or_else(|| invalid("candidate scope unavailable"))?;
            let provisioner = self.dependency_provisioner()?;
            let landed = crate::plugin_dependencies::prepare_candidate(
                &provisioner,
                library_root,
                &scope,
                id,
                &review.dependencies.declaration_digest(),
                resolved,
                access,
            )
            .await
            .map_err(invalid)?;
            checked_view(
                &landed.view,
                &anchor_graph_host::store_root(&anchor_graph_host::resolved_library_root(
                    library_root,
                )),
                resolved,
            )?;
            dependency_scope = Some(scope);
            Some(landed.view)
        } else {
            None
        };
        access.check().map_err(invalid)?;
        let fact = PreparationFact {
            format: 1,
            id: id.into(),
            digest: review.digest.clone(),
            declaration_digest: review.dependencies.declaration_digest(),
            resolved,
        };
        publish_preparation(&root, &fact)?;
        Ok(PreparedCandidate {
            review,
            fact,
            root: staged,
            view,
            library_root: library_root.to_path_buf(),
            _dependency_scope: dependency_scope,
            _lease: lease,
        })
    }

    pub(crate) async fn test_plugin_candidate(
        &self,
        prepared: &PreparedCandidate,
        context: &CandidateTestContext,
        invocation: Option<(&str, &str, Value)>,
    ) -> Result<Value, ApplicationError> {
        let access = &context.access;
        access.check().map_err(invalid)?;
        let catalog = Catalog::new(&prepared.root);
        let definition = catalog.definition(&prepared.fact.id).map_err(invalid)?;
        let binding: PluginBinding = definition.binding();
        if binding.mcp_servers.is_empty() {
            return Err(invalid("candidate declares no enabled MCP server to test"));
        }
        if !crate::plugin_library::ExpectedDefinition::CodeDigest(&prepared.fact.digest)
            .matches(&definition)
        {
            return Err(invalid("candidate code changed after preparation"));
        }
        if prepared.view.is_some() != prepared.fact.resolved.is_some() {
            return Err(invalid(
                "candidate dependency view disagrees with its prepared solve",
            ));
        }
        // A declaration cannot ask the host to disclose arbitrary environment
        // variables or reuse an installed Plugin's owner-bound OAuth grant.
        for server in catalog.mcp_servers(&binding.id, false).map_err(invalid)? {
            if !host_environment_references(&server.config).is_empty() {
                return Err(invalid(format!(
                    "candidate server `{}` references host environment; candidate testing permits only Plugin path templates and does not grant host credentials",
                    server.name
                )));
            }
            for field in [
                "env_vars",
                "optional_env_vars",
                "bearer_token_env_var",
                "env_http_headers",
                "oauth_resource",
                "auth",
            ] {
                if server.config.get(field).is_some() {
                    return Err(invalid(format!(
                        "candidate server `{}` needs `{field}`; candidate testing does not grant host credentials",
                        server.name
                    )));
                }
            }
        }
        let mut views = BTreeMap::new();
        let view = if let Some(view) = &prepared.view {
            let resolved = prepared
                .fact
                .resolved
                .as_ref()
                .ok_or_else(|| invalid("candidate dependency view has no prepared solve"))?;
            checked_view(
                view,
                &anchor_graph_host::store_root(&anchor_graph_host::resolved_library_root(
                    &prepared.library_root,
                )),
                resolved,
            )?;
            Some(
                crate::plugin_view::PreparedView::for_candidate(
                    view,
                    &anchor_graph_host::store_root(&anchor_graph_host::resolved_library_root(
                        &prepared.library_root,
                    )),
                    &prepared.fact.declaration_digest,
                )
                .map_err(invalid)?,
            )
        } else {
            None
        };
        views.insert(
            binding.id.clone(),
            crate::plugin_view::PreparedPlugin::for_candidate(&catalog, &binding.id, view)
                .map_err(invalid)?,
        );
        let mut mcp = crate::tool_host::McpToolConfig::from_catalog(
            &prepared.root,
            std::slice::from_ref(&binding),
        )
        .map_err(invalid)?;
        mcp.startup_cancellation = Some(access.cancellation.clone());
        // Startup itself owns and awaits any partially initialized child.
        let port = mcp
            .bind_with_node_inputs(
                std::slice::from_ref(&binding),
                access.network,
                crate::tool_host::SandboxLaunch {
                    sandbox: &context.sandbox,
                    workspace: &context.workspace,
                    readonly_inputs: &[],
                    plugin_views: &views,
                },
            )
            .await
            .map_err(invalid)?;
        let probe = async {
            let tools = port.definitions();
            let mut result = prepared.result(access.network);
            result["protocol_started"] = json!(true);
            result["tools"] = json!(tools);
            result["business_tested"] = json!(invocation.is_some());
            if let Some((server, tool, arguments)) = invocation {
                let name = format!("{}-{server}_{tool}", binding.id);
                let output = port.call(&name, arguments).await.map_err(invalid)?;
                let bytes = serde_json::to_vec(&output).map_err(invalid)?;
                if bytes.len() > 1024 * 1024 {
                    return Err(invalid(
                        "candidate tool result exceeded the 1 MiB test limit",
                    ));
                }
                result["call"] = json!({"server":server,"tool":tool,"output":output});
            }
            Ok(result)
        };
        let result = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(120), probe) => result
                .unwrap_or_else(|_| Err(invalid("candidate MCP test timed out after 120s"))),
            _ = access.cancelled() => Err(invalid("candidate MCP test was cancelled")),
        };
        port.shutdown().await.map_err(invalid)?;
        result
    }
}
