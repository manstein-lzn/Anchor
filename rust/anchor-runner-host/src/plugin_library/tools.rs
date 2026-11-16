//! Shared preparation steps; callers own grouping, order and error handling.
use crate::plugin_view::{PreparedPlugin, Unusable};
use anchor_runtime::ReadOnlyInput;
use anchor_runtime::graph::PluginBinding;
use std::{collections::BTreeMap, path::PathBuf};

/// Everything a Host invocation derives from its verified Plugin definitions.
/// The bindings, MCP declarations, mounts and views all describe the same read.
pub(crate) struct PreparedPlugins {
    pub(crate) bindings: Vec<PluginBinding>,
    pub(crate) mcp: crate::tool_host::McpToolConfig,
    pub(crate) mounts: Vec<ReadOnlyInput>,
    pub(crate) views: BTreeMap<String, PreparedPlugin>,
    pub(crate) disclosure: crate::plugin_disclosure::PluginDisclosureCatalog,
}

/// Prepare live-default Library code under the selected authorization baseline.
/// The read lease ends before OAuth/MCP startup, preserving live-install behavior.
#[cfg(test)]
pub(crate) async fn prepare_plugins_async(
    bindings: Vec<PluginBinding>,
    library_root: Option<PathBuf>,
    fixture: crate::tool_host::PluginToolHost,
    cancellation: anchor_runtime::Cancellation,
) -> Result<PreparedPlugins, Unusable> {
    prepare_plugins_for_owner_async(bindings, library_root, fixture, cancellation, None).await
}

pub(crate) async fn prepare_plugins_for_owner_async(
    bindings: Vec<PluginBinding>,
    library_root: Option<PathBuf>,
    fixture: crate::tool_host::PluginToolHost,
    cancellation: anchor_runtime::Cancellation,
    credentials: Option<(PathBuf, String)>,
) -> Result<PreparedPlugins, Unusable> {
    prepare_plugins_for_owner_with_revisions_async(
        bindings,
        library_root,
        fixture,
        cancellation,
        credentials,
        BTreeMap::new(),
    )
    .await
}

pub(crate) async fn prepare_plugins_for_owner_with_revisions_async(
    bindings: Vec<PluginBinding>,
    library_root: Option<PathBuf>,
    fixture: crate::tool_host::PluginToolHost,
    cancellation: anchor_runtime::Cancellation,
    credentials: Option<(PathBuf, String)>,
    revisions: BTreeMap<String, String>,
) -> Result<PreparedPlugins, Unusable> {
    fixture
        .for_bindings(&bindings)
        .map_err(|error| Unusable::recorded(error.to_string()))?;
    let needs_library = bindings
        .iter()
        .any(|binding| !crate::tool_host::is_fake_fixture(binding));
    let lease = if needs_library {
        let root = library_root.as_ref().ok_or_else(|| {
            Unusable::unconfigured("Plugin preparation requires the deployment Library root")
        })?;
        let access = crate::dependency_provision::DependencyAccess {
            network: false,
            cancellation: cancellation.clone(),
        };
        crate::plugin_library::catalog_read_lease(root, &access, "invocation preparation")
            .await
            .map_err(|error| Unusable::unconfigured(format!("{error:?}")))?
    } else {
        None
    };
    tokio::task::spawn_blocking(move || {
        let _lease = lease;
        let root = library_root.unwrap_or_default();
        let library = anchor_library::Library::new(&root);
        let mut sources = Vec::new();
        let mut definitions = BTreeMap::new();
        let mut current = Vec::with_capacity(bindings.len());
        let mut mounts = Vec::new();
        let mut views = BTreeMap::new();
        let mut disclosure = crate::plugin_disclosure::PluginDisclosureCatalog::default();
        for binding in &bindings {
            if cancellation.load(std::sync::atomic::Ordering::Acquire) {
                return Err(Unusable::unconfigured("Plugin preparation was cancelled"));
            }
            if crate::tool_host::is_fake_fixture(binding) {
                if revisions.contains_key(&binding.id) {
                    return Err(Unusable::recorded(
                        "fixture Plugin cannot name a Library revision",
                    ));
                }
                current.extend(
                    fixture
                        .resolve_fixture_plugins(std::slice::from_ref(&binding.id))
                        .map_err(Unusable::recorded)?,
                );
                continue;
            }
            let revision = revisions.get(&binding.id).map(String::as_str);
            let source = std::sync::Arc::new(
                library
                    .resolve_source(&binding.id, revision)
                    .map_err(Unusable::from)?,
            );
            let definition = source.definition();
            if !super::ExpectedDefinition::Authorization(binding).matches(definition)
                || revision.is_some() && binding.digest != definition.digest
            {
                return Err(Unusable::recorded(format!(
                    "Plugin `{}` authorization or exact revision changed during preparation",
                    binding.id,
                )));
            }
            let current_binding = definition.binding();
            views.insert(binding.id.clone(), PreparedPlugin::from_source(&source)?);
            mounts.push(ReadOnlyInput::new(
                &definition.directory,
                format!("/plugins/{}", definition.id),
            ));
            current.push(current_binding);
            definitions.insert(binding.id.clone(), definition.clone());
            disclosure.insert_source(source.clone());
            sources.push(source);
        }
        let mut mcp = crate::tool_host::McpToolConfig::from_definitions_for_owner_execution(
            &root,
            &current,
            &definitions,
            credentials
                .as_ref()
                .map(|(data, owner)| (data.as_path(), owner.as_str())),
        )
        .map_err(Unusable::from)?;
        mcp.retain_revision_sources(sources);
        Ok(PreparedPlugins {
            bindings: current,
            mcp,
            mounts,
            views,
            disclosure,
        })
    })
    .await
    .map_err(|error| {
        Unusable::unconfigured(format!("Plugin invocation preparation failed: {error}"))
    })?
}

/// Connect one already prepared group, then associate live tools with the same
/// disclosure catalog. Grouping and soft/hard failure decisions stay at callers.
pub(crate) async fn connect(
    host: &crate::tool_host::PluginToolHost,
    mcp: &crate::tool_host::McpToolConfig,
    bindings: &[PluginBinding],
    network: bool,
    launch: Option<crate::tool_host::SandboxLaunch<'_>>,
    disclosure: &mut crate::plugin_disclosure::PluginDisclosureCatalog,
) -> Result<std::sync::Arc<crate::tool_host::CombinedPluginTools>, crate::tool_host::PreparationError>
{
    let port = host
        .assemble_for_execution(mcp, bindings, network, launch)
        .await?;
    disclosure.attach_tools(port.tool_owners());
    disclosure.unavailable_servers(port.unavailable_servers());
    Ok(port)
}
