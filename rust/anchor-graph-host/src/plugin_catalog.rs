//! Installed Plugin metadata, resource inventory and execution bindings.

use crate::dependencies::{MutablePaths, PluginDependencies};
use crate::dependencies::{plugin_view_path, resolved_library_root};
use crate::mcp::{McpServerDefinition, valid_environment_key};
use crate::plugin_authorization::plugin_authorization_digest;
use crate::plugin_configuration::PluginConfiguration;
use crate::resources::{
    collect_skill_files, inside_directory, inside_file, read_object, read_object_bytes,
    reject_symlink_components, relative_of, resource_files, valid_reference,
};
use crate::{SystemPlugin, dependencies, invalid, mcp};
use anchor_runtime::graph::{GraphError, GraphSnapshot, NetworkMode, PluginBinding};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Resolves installed filesystem Plugins to secret-free immutable identities.
pub trait PluginCatalog: Send + Sync {
    fn resolve(&self, ids: &[String]) -> Result<Vec<PluginBinding>, GraphError>;

    /// Bind executable resources, not a node's offline/automatic policy reference.
    /// Catalogs with declaration metadata also derive Auto's provider dependency.
    fn resolve_snapshot(&self, snapshot: &GraphSnapshot) -> Result<Vec<PluginBinding>, GraphError> {
        self.resolve(&snapshot_plugin_ids(
            snapshot,
            snapshot
                .nodes
                .iter()
                .any(|node| snapshot.node_network_mode(node) == NetworkMode::Enabled),
        ))
    }
}

fn snapshot_plugin_ids(snapshot: &GraphSnapshot, network: bool) -> Vec<String> {
    let mut ids = snapshot
        .nodes
        .iter()
        .flat_map(|node| node.plugins.iter())
        .filter(|reference| reference.id != SystemPlugin::Network.id())
        .map(|reference| reference.id.clone())
        .collect::<BTreeSet<_>>();
    if network {
        ids.insert(SystemPlugin::Network.id().to_owned());
    }
    ids.into_iter().collect()
}

/// Filesystem catalog rooted at the library directory containing `plugins/`.
pub struct FilePluginCatalog {
    root: PathBuf,
}

/// The installed, canonical Plugin definition.  This is the runtime shape of
/// a Plugin after installation has moved its manifest to the bundle root.
/// `mcp_servers` contains validated, unexpanded declarations; use
/// [`FilePluginCatalog::resolve_mcp_servers`] when a host is ready to resolve secrets.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginDefinition {
    pub id: String,
    /// Verified platform identity; a manifest cannot request this classification.
    pub system: Option<SystemPlugin>,
    pub directory: PathBuf,
    pub name: String,
    pub description: String,
    pub skills: Vec<String>,
    pub unsupported: Vec<String>,
    pub mcp_servers: Vec<McpServerDefinition>,
    pub channels: Vec<ChannelDefinition>,
    pub configuration: PluginConfiguration,
    /// The Plugin's own run-time dependencies. Empty when it declares none.
    pub dependencies: PluginDependencies,
    /// This Plugin needs task-tool network access. HTTP MCP also implies it.
    pub network: bool,
    /// The paths the Plugin declares as its own mutable state: the platform
    /// provides the location and never copies, rewrites or verifies them.
    pub mutable: MutablePaths,
    /// The code resources covered by `digest`, from this same catalog read.
    pub resources: Vec<String>,
    /// Configuration files may contain literal secrets; hosts expose their
    /// safe declaration facts instead of returning these files to models.
    pub configuration_resources: Vec<String>,
    pub digest: String,
    /// Secret-free digest of the declarations that authorize Host capabilities.
    pub authorization_digest: anchor_runtime::graph::AuthorizationDigest,
}

/// Validated live catalog information, without a code-content identity. This can describe a
/// Plugin in a list, but cannot be used as an execution binding. The resource inventory is still
/// checked; reading the bytes and deriving a binding belongs to `definition`.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginMetadata {
    pub id: String,
    pub system: Option<SystemPlugin>,
    pub directory: PathBuf,
    pub name: String,
    pub description: String,
    pub skills: Vec<String>,
    pub unsupported: Vec<String>,
    pub mcp_servers: Vec<McpServerDefinition>,
    pub channels: Vec<ChannelDefinition>,
    pub configuration: PluginConfiguration,
    pub dependencies: PluginDependencies,
    pub network: bool,
    pub mutable: MutablePaths,
}

struct PluginInventory {
    metadata: PluginMetadata,
    resources: Vec<String>,
    configuration_resources: Vec<String>,
    manifest_bytes: Vec<u8>,
}

/// Operator-only evidence for an explicit System Plugin upgrade. This cannot
/// produce an execution binding or confer a platform capability identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPluginInspection {
    pub digest: String,
    pub resources: Vec<String>,
    pub mutable: MutablePaths,
}

impl PluginDefinition {
    pub fn requires_network(&self) -> bool {
        declaration_requires_network(self.network, &self.mcp_servers)
    }

    pub fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: self.id.clone(),
            system: self.system,
            directory: self.directory.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            skills: self.skills.clone(),
            unsupported: self.unsupported.clone(),
            mcp_servers: self.mcp_servers.clone(),
            channels: self.channels.clone(),
            configuration: self.configuration.clone(),
            dependencies: self.dependencies.clone(),
            network: self.network,
            mutable: self.mutable.clone(),
        }
    }

    /// Derive the secret-free identity without reading the Library again.
    pub fn binding(&self) -> anchor_runtime::graph::PluginBinding {
        anchor_runtime::graph::PluginBinding {
            id: self.id.clone(),
            digest: self.digest.clone(),
            resources: self.resources.clone(),
            mcp_servers: self
                .mcp_servers
                .iter()
                .map(|server| server.name.clone())
                .collect(),
            authorization_digest: self.authorization_digest.clone(),
        }
    }
}

impl PluginMetadata {
    pub fn requires_network(&self) -> bool {
        declaration_requires_network(self.network, &self.mcp_servers)
    }
}

fn declaration_requires_network(network: bool, servers: &[McpServerDefinition]) -> bool {
    network
        || servers
            .iter()
            .any(|server| server.config.get("url").is_some())
}

/// Service-level channel metadata.  Channels are not MCP servers and are
/// supervised by the host separately from AgentNode execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelDefinition {
    pub plugin: String,
    pub platform: String,
    pub transport: String,
    pub entrypoint: String,
    pub required_environment: Vec<String>,
    pub description: String,
    pub sdk: String,
}

impl FilePluginCatalog {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve the canonical installed Plugin definition and all of its
    /// resource metadata.  This is the single catalog path used by runtime
    /// identity resolution and by host adapters that need live resources.
    pub fn definition(&self, id: &str) -> Result<PluginDefinition, GraphError> {
        let PluginInventory {
            metadata,
            resources,
            configuration_resources,
            manifest_bytes,
        } = self.inventory(id)?;
        let digest = resource_digest(&metadata.directory, &resources, &manifest_bytes)?;
        let authorization_digest =
            metadata
                .configuration
                .authorization_digest(plugin_authorization_digest(
                    &metadata.mcp_servers,
                    metadata.network,
                    metadata.mutable.paths(),
                ));
        Ok(PluginDefinition {
            id: metadata.id,
            system: metadata.system,
            directory: metadata.directory,
            name: metadata.name,
            description: metadata.description,
            skills: metadata.skills,
            unsupported: metadata.unsupported,
            mcp_servers: metadata.mcp_servers,
            channels: metadata.channels,
            configuration: metadata.configuration,
            dependencies: metadata.dependencies,
            network: metadata.network,
            mutable: metadata.mutable,
            resources,
            configuration_resources,
            digest,
            authorization_digest: authorization_digest
                .try_into()
                .expect("SHA256 authority digest is non-empty"),
        })
    }

    /// Read live catalog information through the same manifest, resource-path and configuration
    /// validation as execution definitions, without computing their code digest. System Plugin
    /// identity additionally checks the small public resources against this Anchor release.
    pub fn metadata(&self, id: &str) -> Result<PluginMetadata, GraphError> {
        Ok(self.inventory(id)?.metadata)
    }

    /// A denied policy never needs a provider. Auto derives its need only from
    /// the same validated Plugin declarations used for execution.
    pub fn snapshot_requires_network(&self, snapshot: &GraphSnapshot) -> Result<bool, GraphError> {
        for node in &snapshot.nodes {
            match snapshot.node_network_mode(node) {
                NetworkMode::Denied => {}
                NetworkMode::Enabled => return Ok(true),
                NetworkMode::Auto => {
                    for plugin in &node.plugins {
                        if plugin.id != SystemPlugin::Network.id()
                            && self.metadata(&plugin.id)?.requires_network()
                        {
                            return Ok(true);
                        }
                    }
                }
            }
        }
        Ok(false)
    }

    /// Inspect old installed bytes before an operator reviews an explicit upgrade.
    /// Previous-release public resources are allowed here, never in execution reads.
    pub fn inspect_system_plugin(
        &self,
        system: SystemPlugin,
    ) -> Result<SystemPluginInspection, GraphError> {
        let inventory = self.read_inventory(system.id(), false)?;
        Ok(SystemPluginInspection {
            digest: resource_digest(
                &inventory.metadata.directory,
                &inventory.resources,
                &inventory.manifest_bytes,
            )?,
            resources: inventory.resources,
            mutable: inventory.metadata.mutable,
        })
    }

    fn inventory(&self, id: &str) -> Result<PluginInventory, GraphError> {
        self.read_inventory(id, true)
    }

    fn read_inventory(&self, id: &str, verify_system: bool) -> Result<PluginInventory, GraphError> {
        let (root, dir) = self.plugin_dir(id)?;
        let manifest_path = dir.join("plugin.json");
        reject_symlink_components(&dir, &manifest_path)?;
        let (manifest, manifest_bytes) = read_object_bytes(&manifest_path)?;
        // A field this catalog does not act on is refused rather than ignored:
        // a mistyped `dependencies` must not look like it took effect.
        dependencies::reject_unknown_fields(id, &manifest)?;
        let dependencies = dependencies::parse(id, &manifest)?;
        let network = dependencies::parse_network(id, &manifest)?;
        let mutable = dependencies::parse_mutable(id, &manifest)?;

        let name = manifest
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| invalid(format!("Plugin {id}: name is required")))?
            .to_owned();
        let interface = manifest
            .get("interface")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let description_value = manifest
            .get("description")
            .filter(|value| manifest_value_is_present(value))
            .or_else(|| {
                interface
                    .get("longDescription")
                    .filter(|value| manifest_value_is_present(value))
            })
            .or_else(|| {
                interface
                    .get("shortDescription")
                    .filter(|value| manifest_value_is_present(value))
            })
            .cloned()
            .unwrap_or_else(|| Value::String(String::new()));
        let description = description_value
            .as_str()
            .ok_or_else(|| invalid(format!("Plugin {id}: description must be a string")))?
            .to_owned();

        let skill_roots = match manifest.get("skills") {
            Some(Value::String(path)) => vec![path.clone()],
            Some(Value::Array(paths)) => paths
                .iter()
                .map(|path| {
                    path.as_str().map(str::to_owned).ok_or_else(|| {
                        invalid(format!(
                            "Plugin {id}: skills must be a path or list of paths"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => {
                return Err(invalid(format!(
                    "Plugin {id}: skills must be a path or list of paths"
                )));
            }
            None if dir.join("skills").is_dir() => vec!["skills/".into()],
            None => Vec::new(),
        };
        let mut skills = Vec::new();
        for skill_root in skill_roots {
            let path = inside_directory(&dir, &skill_root).map_err(|error| {
                invalid(format!(
                    "Plugin {id}: skill path is outside the bundle or not a directory: {skill_root} ({error})"
                ))
            })?;
            let mut discovered = Vec::new();
            collect_skill_files(&dir, &path, &mut discovered)?;
            discovered.sort();
            for skill in discovered {
                if !skills.contains(&skill) {
                    skills.push(skill);
                }
            }
        }
        let legacy = dir.join("instructions.md");
        if legacy.is_file() {
            skills.push("instructions.md".into());
        }

        // The digest covers *code* only: a declared mutable path changes while
        // the Plugin runs, so it can never be part of "which code is this".
        let resources = resource_files(&dir, &mutable)?;
        let system = verify_system.then(|| SystemPlugin::from_id(id)).flatten();
        if let Some(system) = system {
            system.verify(&dir, &resources, &manifest_bytes)?;
        }

        let mcp_servers =
            mcp::servers_from_manifest(id, &root, &dir, &manifest, &dependencies, false)?;
        let configuration = PluginConfiguration::parse(id, &manifest, &mcp_servers)?;
        let channels = channels_from_dir(id, &dir)?;
        let unsupported = ["hooks", "commands", "agents", "apps"]
            .into_iter()
            .filter(|key| {
                manifest.get(*key).is_some_and(manifest_value_is_present)
                    || dir.join(key).exists()
                    || (*key == "apps" && dir.join(".app.json").exists())
            })
            .map(str::to_owned)
            .collect();

        let mut configuration_resources = vec![
            "plugin.json".into(),
            ".mcp.json".into(),
            "channel.json".into(),
        ];
        if let Some(Value::String(relative)) = manifest.get("mcpServers") {
            // Match the canonical resource inventory, including ./ and repeated
            // separators accepted by the already validated manifest path.
            configuration_resources.push(relative_of(
                &dir,
                &inside_file(&dir, relative).map_err(invalid)?,
            ));
        }
        Ok(PluginInventory {
            metadata: PluginMetadata {
                id: id.into(),
                system,
                directory: dir,
                name,
                description,
                skills,
                unsupported,
                mcp_servers,
                channels,
                configuration,
                dependencies,
                network,
                mutable,
            },
            resources,
            configuration_resources,
            manifest_bytes,
        })
    }

    /// Return MCP declarations after optionally applying the environment
    /// expansion defined by the Library manifest contract.
    pub fn mcp_servers(
        &self,
        id: &str,
        resolve_env: bool,
    ) -> Result<Vec<McpServerDefinition>, GraphError> {
        if SystemPlugin::from_id(id).is_some() {
            self.metadata(id)?;
        }
        let (root, dir) = self.plugin_dir(id)?;
        mcp::servers_from_dir(id, &root, &dir, resolve_env)
    }

    /// Expand a previously read declaration without consulting its files again.
    /// Callers can check the authority of exactly the configuration they launch.
    pub fn resolve_mcp_servers(
        &self,
        definition: &PluginDefinition,
    ) -> Result<Vec<McpServerDefinition>, GraphError> {
        self.resolve_mcp_servers_with(definition, &|key| std::env::var(key).ok())
    }

    /// Resolve an already validated declaration through the Host's scoped
    /// credential lookup. The resolver does not modify process environment or
    /// reread mutable Plugin files; the Host owns identity and authorization.
    pub fn resolve_mcp_servers_with(
        &self,
        definition: &PluginDefinition,
        environment: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Vec<McpServerDefinition>, GraphError> {
        let root = self.root.canonicalize()?;
        let mut servers = Vec::new();
        for server in &definition.mcp_servers {
            if let Some(config) = mcp::mcp_server(
                &definition.id,
                &server.name,
                server.config.clone(),
                &definition.directory,
                true,
                root.parent().unwrap_or(&root),
                environment,
            )? {
                servers.push(McpServerDefinition {
                    name: server.name.clone(),
                    config,
                });
            }
        }
        Ok(servers)
    }

    /// Return validated service-level channel declarations from `channel.json`.
    pub fn channels(&self, id: &str) -> Result<Vec<ChannelDefinition>, GraphError> {
        Ok(self.metadata(id)?.channels)
    }

    /// Return the canonical installed Plugin directory after validating the
    /// reference and all path components.
    pub fn plugin_directory(&self, id: &str) -> Result<PathBuf, GraphError> {
        self.plugin_dir(id).map(|(_, dir)| dir)
    }

    /// The view this Plugin's declaration maps to, if it declares dependencies.
    ///
    /// Absolute and canonical, because it is both what `${ANCHOR_PLUGIN_VIEW}`
    /// expands to and the path the sandbox mounts. `None` means the Plugin needs
    /// no dependency environment at all.
    pub fn view_path(&self, id: &str) -> Result<Option<PathBuf>, GraphError> {
        let (root, _dir) = self.plugin_dir(id)?;
        let dependencies = self.definition(id)?.dependencies;
        if dependencies.is_empty() {
            return Ok(None);
        }
        Ok(Some(plugin_view_path(
            &resolved_library_root(&root),
            id,
            &dependencies.declaration_digest(),
        )))
    }

    fn plugin_dir(&self, id: &str) -> Result<(PathBuf, PathBuf), GraphError> {
        if !valid_reference(id) {
            return Err(invalid(format!("invalid Plugin reference `{id}`")));
        }
        let root = self.root.canonicalize()?;
        let plugins = root.join("plugins");
        // Name the Plugin and the Library root: a caller that resolved an id from a
        // bundle needs to know *where* it looked, not just that an open failed.
        let candidate = plugins.join(id);
        let dir = candidate.canonicalize().map_err(|_| {
            invalid(format!(
                "Plugin `{id}` is not installed in the Library at {}",
                candidate.display()
            ))
        })?;
        if !dir.is_dir() {
            return Err(invalid(format!("unknown Plugin `{id}`")));
        }
        Ok((root, dir))
    }

    fn plugin(&self, id: &str) -> Result<anchor_runtime::graph::PluginBinding, GraphError> {
        Ok(self.definition(id)?.binding())
    }
}

fn resource_digest(
    directory: &Path,
    resources: &[String],
    manifest_bytes: &[u8],
) -> Result<String, GraphError> {
    let mut hash = Sha256::new();
    for relative in resources {
        hash.update(relative.as_bytes());
        if relative == "plugin.json" {
            hash.update(Sha256::digest(manifest_bytes));
        } else {
            hash.update(Sha256::digest(std::fs::read(directory.join(relative))?));
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

impl PluginCatalog for FilePluginCatalog {
    fn resolve(&self, ids: &[String]) -> Result<Vec<PluginBinding>, GraphError> {
        ids.iter().map(|id| self.plugin(id)).collect()
    }

    fn resolve_snapshot(&self, snapshot: &GraphSnapshot) -> Result<Vec<PluginBinding>, GraphError> {
        self.resolve(&snapshot_plugin_ids(
            snapshot,
            self.snapshot_requires_network(snapshot)?,
        ))
    }
}

fn manifest_value_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn channels_from_dir(id: &str, dir: &Path) -> Result<Vec<ChannelDefinition>, GraphError> {
    let path = dir.join("channel.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    reject_symlink_components(dir, &path)?;
    let spec = read_object(&path)?;
    let platform = spec
        .get("platform")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("Plugin {id}: channel platform is required")))?;
    if platform.is_empty()
        || !platform.chars().enumerate().all(|(index, character)| {
            if index == 0 {
                character.is_ascii_alphabetic()
            } else {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-')
            }
        })
    {
        return Err(invalid(format!(
            "Plugin {id}: channel platform is required"
        )));
    }
    if let Some(system) = SystemPlugin::from_id(platform)
        .filter(|system| matches!(system, SystemPlugin::Feishu | SystemPlugin::Wecom))
        && system.id() != id
    {
        return Err(invalid(format!(
            "Plugin {id}: channel platform {platform} is owned by its Anchor System Plugin"
        )));
    }
    if spec.get("transport").and_then(Value::as_str) != Some("websocket") {
        return Err(invalid(format!(
            "Plugin {id}: only websocket channels are supported"
        )));
    }
    let entrypoint = spec
        .get("entrypoint")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(format!("Plugin {id}: channel entrypoint is required")))?;
    let _ = inside_file(dir, entrypoint).map_err(|error| invalid(error.to_string()))?;
    let required = match spec.get("required_environment") {
        None => Vec::new(),
        Some(Value::Array(values)) => values.clone(),
        Some(_) => {
            return Err(invalid(format!(
                "Plugin {id}: required_environment must be a list of names"
            )));
        }
    };
    let mut required_environment = Vec::new();
    for item in required {
        let item = item.as_str().ok_or_else(|| {
            invalid(format!(
                "Plugin {id}: required_environment must be a list of names"
            ))
        })?;
        if !valid_environment_key(item) {
            return Err(invalid(format!(
                "Plugin {id}: required_environment must be a list of names"
            )));
        }
        required_environment.push(item.to_owned());
    }
    let description = match spec.get("description") {
        None => String::new(),
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| invalid(format!("Plugin {id}: channel description must be a string")))?,
    };
    let sdk = match spec.get("sdk") {
        None => String::new(),
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| invalid(format!("Plugin {id}: channel sdk must be a string")))?,
    };
    Ok(vec![ChannelDefinition {
        plugin: id.into(),
        platform: platform.into(),
        transport: "websocket".into(),
        entrypoint: entrypoint.into(),
        required_environment,
        description,
        sdk,
    }])
}

#[cfg(test)]
#[path = "plugin_catalog_tests.rs"]
mod tests;
