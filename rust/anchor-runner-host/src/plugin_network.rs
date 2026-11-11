//! Task networking is a Host permission, exposed through the `network` Plugin.
//! Requirements are derived from mounted Plugins; they never write a Session
//! switch or override a human's explicit network denial.

use std::{collections::BTreeMap, path::Path};

use crate::plugin_library::Catalog;
use anchor_graph_host::{PluginCatalog, PluginDefinition, PluginMetadata};
use anchor_runtime::graph::{GraphSnapshot, NetworkMode, PluginBinding};

pub(crate) const NETWORK_PLUGIN: &str = "network";
const MISSING_NETWORK: &str = "联网能力未安装或声明无效：需要 network Plugin（network: true）";

pub(crate) fn session_mode(mounts: &BTreeMap<String, bool>, allowed: bool) -> NetworkMode {
    if !allowed {
        NetworkMode::Denied
    } else if mounts.get(NETWORK_PLUGIN) == Some(&true) {
        NetworkMode::Enabled
    } else {
        NetworkMode::Auto
    }
}

/// Shared by approval and delegated-Graph admission. Denied requirements cannot
/// turn into an executable network grant, including on command nodes.
pub(crate) fn snapshot_requires_network(
    root: &Path,
    snapshot: &GraphSnapshot,
) -> Result<bool, String> {
    Catalog::new(root)
        .snapshot_requires_network(snapshot)
        .map_err(|error| error.to_string())
}

pub(crate) fn requires_network(plugin: &PluginDefinition) -> bool {
    plugin.requires_network()
}

pub(crate) fn metadata_requires_network(plugin: &PluginMetadata) -> bool {
    plugin.requires_network()
}

pub(crate) struct NetworkResolution {
    pub(crate) enabled: bool,
    pub(crate) allowed: bool,
    pub(crate) enabled_by: Vec<String>,
    pub(crate) errors: BTreeMap<String, String>,
}

pub(crate) fn resolve(root: &Path, mounted: &[String], permission: bool) -> NetworkResolution {
    let catalog = Catalog::new(root);
    resolve_with(
        mounted,
        permission,
        |id| {
            catalog
                .definition(id)
                .is_ok_and(|plugin| requires_network(&plugin))
        },
        || match catalog.definition(NETWORK_PLUGIN) {
            Ok(plugin) if plugin.network => {
                crate::plugin_view::prepare_definition_view(root, &plugin)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            _ => Err(MISSING_NETWORK.into()),
        },
    )
}

/// Lists report live declarations and view availability. Execution still uses full definitions;
/// the same policy calculation never turns this display projection into an execution grant.
pub(crate) fn resolve_metadata(
    root: &Path,
    definitions: &BTreeMap<String, PluginMetadata>,
    mounted: &[String],
    permission: bool,
) -> NetworkResolution {
    resolve_with(
        mounted,
        permission,
        |id| definitions.get(id).is_some_and(metadata_requires_network),
        || match definitions.get(NETWORK_PLUGIN) {
            Some(plugin) if plugin.network => {
                crate::plugin_view::prepare_dependency_view(root, &plugin.id, &plugin.dependencies)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            _ => Err(MISSING_NETWORK.into()),
        },
    )
}

fn resolve_with(
    mounted: &[String],
    permission: bool,
    mut requires_network: impl FnMut(&str) -> bool,
    provider: impl FnOnce() -> Result<(), String>,
) -> NetworkResolution {
    let mut enabled_by = mounted
        .iter()
        .filter(|id| id.as_str() != NETWORK_PLUGIN)
        .filter(|id| requires_network(id))
        .cloned()
        .collect::<Vec<_>>();
    enabled_by.sort();
    enabled_by.dedup();
    let enabled = mounted.iter().any(|id| id == NETWORK_PLUGIN) || !enabled_by.is_empty();
    let problem = if !enabled {
        None
    } else if !permission {
        Some("当前任务已禁止联网".to_owned())
    } else {
        provider().err()
    };
    let allowed = enabled && problem.is_none();
    let mut errors = BTreeMap::new();
    if let Some(problem) = problem {
        errors.insert(NETWORK_PLUGIN.to_owned(), problem.clone());
        for id in &enabled_by {
            errors.insert(id.clone(), format!("该插件需要联网：{problem}"));
        }
    }
    NetworkResolution {
        enabled,
        allowed,
        enabled_by,
        errors,
    }
}

/// Apply the same capability resolution the Session UI reports. Network-needed
/// tools stay absent when denied; offline tools still run in a network namespace
/// without host connectivity.
pub(crate) fn apply_node(
    root: &Path,
    bindings: &mut Vec<PluginBinding>,
    mode: NetworkMode,
    ceiling: bool,
    reasons: &mut Vec<String>,
) -> Result<bool, String> {
    if mode != NetworkMode::Enabled {
        bindings.retain(|binding| binding.id != NETWORK_PLUGIN);
    }
    let mut ids = bindings
        .iter()
        .map(|binding| binding.id.clone())
        .collect::<Vec<_>>();
    if mode == NetworkMode::Enabled && !ids.iter().any(|id| id == NETWORK_PLUGIN) {
        ids.push(NETWORK_PLUGIN.to_owned());
    }
    let resolution = resolve(root, &ids, ceiling && mode != NetworkMode::Denied);
    if !bindings.iter().any(|binding| binding.id == NETWORK_PLUGIN)
        && let Some(problem) = resolution.errors.get(NETWORK_PLUGIN)
    {
        reasons.push(format!("Plugin `{NETWORK_PLUGIN}`: {problem}"));
    }
    bindings.retain(|binding| {
        if let Some(problem) = resolution.errors.get(&binding.id) {
            reasons.push(format!("Plugin `{}`: {problem}", binding.id));
            // Channel delivery is Host control-plane traffic. Its optional MCP
            // business APIs remain subject to the task's network policy.
            Catalog::new(root)
                .metadata(&binding.id)
                .is_ok_and(|metadata| metadata.system.is_some() && !metadata.channels.is_empty())
        } else {
            true
        }
    });
    if resolution.allowed && !bindings.iter().any(|binding| binding.id == NETWORK_PLUGIN) {
        let binding = Catalog::new(root)
            .resolve(&[NETWORK_PLUGIN.to_owned()])
            .map_err(|error| error.to_string())?
            .pop()
            .ok_or_else(|| "network Plugin is unavailable".to_owned())?;
        bindings.push(binding);
    }
    Ok(resolution.allowed)
}
