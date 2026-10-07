//! Standalone Graph bundle loading and minimal in-process host adapter for
//! recursive Graph calls.
//!
//! The host supplies a graph catalog, the shared FileRunStore, artifact port,
//! node executor, and run control. Child Runs are always executed by the same
//! `GraphRunner` implementation. Bundle loading is read-only; result
//! materialization, session handoff, and platform APIs are outside this adapter.

use anchor_runtime_rig::Cancellation;
use anchor_runtime_rig::graph::{
    ArtifactPort, CallFileSelection, CallIdentity, CommitRef, GraphCallOutcome, GraphCallPort,
    GraphError, GraphRunRecord, GraphRunner, GraphSnapshot, InvocationKey,
    NodeExecutionCapabilities, NodeExecutionOutcome, NodeExecutionPort, NodeExecutionRequest,
    RunControl, RunLease, RunStatus, RunStore,
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Component, Path, PathBuf},
    pin::Pin,
};

/// Strict metadata for a distributable, secret-free Graph directory.
///
/// Format 1 contains one `graph.json`, this `manifest.json`, and explicitly
/// declared Plugin resource directories at `plugins/<id>/`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphBundleManifest {
    pub format: u32,
    pub graph: String,
    pub plugins: Vec<BundlePluginSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePluginSummary {
    pub id: String,
    pub digest: String,
    pub resources: Vec<String>,
    pub mcp_servers: Vec<String>,
}

/// Admitted expanded Graph snapshot and secret-free Plugin identities.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedGraphBundle {
    /// The editable graph definition as it appears in `graph.json`. Keep this
    /// beside the compiled Runtime snapshot so hosts can return and persist
    /// author-only fields without feeding them into Runtime execution.
    pub authoring_definition: Value,
    pub snapshot: GraphSnapshot,
    pub plugins: Vec<anchor_runtime_rig::graph::PluginBinding>,
}

/// Read-only loader for a single standalone Graph bundle directory.
pub struct FileGraphBundleLoader {
    root: PathBuf,
}

impl FileGraphBundleLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Load and admit `manifest.json`, `graph.json`, and explicitly declared
    /// `plugins/<id>/` assets. This function never dispatches Graph work.
    pub fn load(&self) -> Result<LoadedGraphBundle, GraphError> {
        let root = self.root.canonicalize()?;
        if !root.is_dir() {
            return Err(invalid("Graph bundle root must be a directory".into()));
        }
        let manifest_path = root.join("manifest.json");
        reject_symlink_components(&root, &manifest_path)?;
        let manifest: GraphBundleManifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)
            .map_err(GraphError::SnapshotDecode)?;
        if manifest.format != 1 || manifest.graph != "graph.json" {
            return Err(invalid(
                "unsupported Graph bundle format or graph path".into(),
            ));
        }
        let graph_path = root.join("graph.json");
        reject_symlink_components(&root, &graph_path)?;
        let authoring_definition: Value = serde_json::from_slice(&std::fs::read(&graph_path)?)
            .map_err(GraphError::SnapshotDecode)?;
        let snapshot = GraphSnapshot::from_authoring(authoring_definition.clone())?;

        let expected = snapshot
            .nodes
            .iter()
            .flat_map(|node| node.plugins.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        let ids = manifest
            .plugins
            .iter()
            .map(|plugin| plugin.id.clone())
            .collect::<Vec<_>>();
        let declared = ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if expected != declared || declared.len() != ids.len() {
            return Err(invalid(
                "manifest Plugin set must exactly match Graph references".into(),
            ));
        }

        let catalog = FilePluginCatalog::new(&root);
        let bindings = catalog.resolve(&ids)?;
        for (summary, binding) in manifest.plugins.iter().zip(&bindings) {
            if summary.digest != binding.digest
                || summary.resources != binding.resources
                || summary.mcp_servers != binding.mcp_servers
            {
                return Err(invalid(format!(
                    "Plugin `{}` summary does not match bundled resources",
                    summary.id
                )));
            }
        }

        // A bundle is deliberately closed: no undeclared payload, plugin, or
        // extra top-level config can be silently carried into deployment.
        let mut allowed = vec!["graph.json", "manifest.json"];
        if !ids.is_empty() {
            allowed.push("plugins");
        }
        allowed.sort_unstable();
        let mut actual = std::fs::read_dir(&root)?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        actual.sort_unstable();
        if actual != allowed {
            return Err(invalid(
                "Graph bundle contains undeclared top-level resources".into(),
            ));
        }
        if !ids.is_empty() {
            let plugin_root = root.join("plugins");
            reject_symlink_components(&root, &plugin_root)?;
            for id in &ids {
                reject_symlink_components(&root, &plugin_root.join(id))?;
            }
            let mut actual = std::fs::read_dir(plugin_root)?
                .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
                .collect::<Result<Vec<_>, _>>()?;
            actual.sort_unstable();
            let mut declared = ids;
            declared.sort_unstable();
            if actual != declared {
                return Err(invalid(
                    "bundle contains undeclared Plugin directories".into(),
                ));
            }
        }
        Ok(LoadedGraphBundle {
            authoring_definition,
            snapshot,
            plugins: bindings,
        })
    }
}

/// Resolves installed filesystem Plugins to secret-free immutable identities.
pub trait PluginCatalog: Send + Sync {
    fn resolve(
        &self,
        ids: &[String],
    ) -> Result<Vec<anchor_runtime_rig::graph::PluginBinding>, GraphError>;
}

/// Filesystem catalog rooted at the library directory containing `plugins/`.
pub struct FilePluginCatalog {
    root: PathBuf,
}

/// The installed, canonical Plugin definition.  This is the runtime shape of
/// a Plugin after installation has moved its manifest to the bundle root.
/// `mcp_servers` contains validated, unexpanded declarations; use
/// [`FilePluginCatalog::mcp_servers`] when a host is ready to resolve secrets.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginDefinition {
    pub id: String,
    pub directory: PathBuf,
    pub name: String,
    pub description: String,
    pub skills: Vec<String>,
    pub unsupported: Vec<String>,
    pub mcp_servers: Vec<McpServerDefinition>,
    pub channels: Vec<ChannelDefinition>,
    pub digest: String,
}

/// One validated MCP server declaration from `.mcp.json` or `plugin.json`.
/// The config is kept as JSON because the MCP adapter owns transport-specific
/// details and credentials; the catalog owns validation and expansion only.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerDefinition {
    pub name: String,
    pub config: Value,
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
        let (root, dir) = self.plugin_dir(id)?;
        let manifest_path = dir.join("plugin.json");
        reject_symlink_components(&dir, &manifest_path)?;
        let manifest = read_object(&manifest_path)?;

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
            .filter(|value| python_truthy(value))
            .or_else(|| {
                interface
                    .get("longDescription")
                    .filter(|value| python_truthy(value))
            })
            .or_else(|| {
                interface
                    .get("shortDescription")
                    .filter(|value| python_truthy(value))
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

        let resources = resource_files(&dir)?;
        let mut hash = Sha256::new();
        for relative in &resources {
            let content = std::fs::read(dir.join(relative))?;
            hash.update(relative.as_bytes());
            hash.update(Sha256::digest(content));
        }
        let digest = format!("{:x}", hash.finalize());

        let mcp_servers = self.mcp_servers_from_dir(id, &root, &dir, false)?;
        let channels = channels_from_dir(id, &dir)?;
        let unsupported = ["hooks", "commands", "agents", "apps"]
            .into_iter()
            .filter(|key| {
                manifest.get(*key).is_some_and(python_truthy)
                    || dir.join(key).exists()
                    || (*key == "apps" && dir.join(".app.json").exists())
            })
            .map(str::to_owned)
            .collect();

        Ok(PluginDefinition {
            id: id.into(),
            directory: dir,
            name,
            description,
            skills,
            unsupported,
            mcp_servers,
            channels,
            digest,
        })
    }

    /// Return MCP declarations after optionally applying the environment
    /// expansion defined by the Python Library contract.
    pub fn mcp_servers(
        &self,
        id: &str,
        resolve_env: bool,
    ) -> Result<Vec<McpServerDefinition>, GraphError> {
        let (root, dir) = self.plugin_dir(id)?;
        self.mcp_servers_from_dir(id, &root, &dir, resolve_env)
    }

    /// Return validated service-level channel declarations from `channel.json`.
    pub fn channels(&self, id: &str) -> Result<Vec<ChannelDefinition>, GraphError> {
        let (_root, dir) = self.plugin_dir(id)?;
        channels_from_dir(id, &dir)
    }

    /// Return the canonical installed Plugin directory after validating the
    /// reference and all path components.
    pub fn plugin_directory(&self, id: &str) -> Result<PathBuf, GraphError> {
        self.plugin_dir(id).map(|(_, dir)| dir)
    }

    fn plugin_dir(&self, id: &str) -> Result<(PathBuf, PathBuf), GraphError> {
        if !valid_reference(id) {
            return Err(invalid(format!("invalid Plugin reference `{id}`")));
        }
        let root = self.root.canonicalize()?;
        let plugins = root.join("plugins");
        let dir = plugins.join(id).canonicalize()?;
        if !dir.is_dir() {
            return Err(invalid(format!("unknown Plugin `{id}`")));
        }
        Ok((root, dir))
    }

    fn plugin(&self, id: &str) -> Result<anchor_runtime_rig::graph::PluginBinding, GraphError> {
        let definition = self.definition(id)?;
        Ok(anchor_runtime_rig::graph::PluginBinding {
            id: definition.id,
            digest: definition.digest,
            resources: resource_files(&definition.directory)?,
            mcp_servers: definition
                .mcp_servers
                .into_iter()
                .map(|server| server.name)
                .collect(),
        })
    }

    fn mcp_servers_from_dir(
        &self,
        id: &str,
        root: &Path,
        dir: &Path,
        resolve_env: bool,
    ) -> Result<Vec<McpServerDefinition>, GraphError> {
        let manifest_path = dir.join("plugin.json");
        reject_symlink_components(dir, &manifest_path)?;
        let manifest = read_object(&manifest_path)?;
        let mut servers = serde_json::Map::new();
        let dot_mcp = dir.join(".mcp.json");
        if dot_mcp.exists() {
            reject_symlink_components(dir, &dot_mcp)?;
            let config = read_object(&dot_mcp)?;
            collect_server_values(config.get("mcpServers"), &mut servers, id)?;
        }
        match manifest.get("mcpServers") {
            None => {}
            Some(Value::String(relative)) => {
                let config_path = inside_file(dir, relative)
                    .map_err(|_| invalid(format!("Plugin {id}: unsafe MCP config path")))?;
                reject_symlink_components(dir, &config_path)?;
                let config = read_object(&config_path)?;
                collect_server_values(config.get("mcpServers"), &mut servers, id)?;
            }
            Some(value) => collect_server_values(Some(value), &mut servers, id)?,
        }
        let mut result = Vec::new();
        for (name, value) in servers {
            let Some(config) = mcp_server(
                id,
                &name,
                value,
                dir,
                resolve_env,
                root.parent().unwrap_or(root),
            )?
            else {
                continue;
            };
            result.push(McpServerDefinition { name, config });
        }
        Ok(result)
    }
}

impl PluginCatalog for FilePluginCatalog {
    fn resolve(
        &self,
        ids: &[String],
    ) -> Result<Vec<anchor_runtime_rig::graph::PluginBinding>, GraphError> {
        ids.iter().map(|id| self.plugin(id)).collect()
    }
}

fn invalid(message: String) -> GraphError {
    GraphError::InvalidSnapshot(message)
}
fn valid_reference(value: &str) -> bool {
    !value.is_empty()
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
fn read_object(path: &Path) -> Result<serde_json::Map<String, Value>, GraphError> {
    let value: Value =
        serde_json::from_slice(&std::fs::read(path)?).map_err(GraphError::SnapshotDecode)?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid(format!("{} must contain a JSON object", path.display())))
}
fn reject_symlink_components(root: &Path, path: &Path) -> Result<(), GraphError> {
    let root = root.canonicalize()?;
    if !path.starts_with(&root) {
        return Err(invalid("Plugin path escapes catalog root".into()));
    }
    let mut current = root.clone();
    for component in path.strip_prefix(&root).unwrap().components() {
        if let Component::Normal(part) = component {
            current.push(part);
            if std::fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(invalid(format!(
                    "Plugin symlinks are not supported: {}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}
fn collect_files(root: &Path, current: &Path, out: &mut Vec<String>) -> Result<(), GraphError> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_symlink() {
            return Err(invalid(format!(
                "Plugin symlinks are not supported: {}",
                path.display()
            )));
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".env" || name.starts_with(".env.") || name == ".mcp-auth" {
            return Err(invalid(format!(
                "Plugin-local credentials are not supported: {}",
                path.display()
            )));
        }
        if ty.is_dir() {
            collect_files(root, &path, out)?;
        } else if ty.is_file() {
            out.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn inside_path(base: &Path, relative: &str, require_file: bool) -> Result<PathBuf, String> {
    let candidate = Path::new(relative);
    let base = base
        .canonicalize()
        .map_err(|error| format!("Plugin directory cannot be resolved: {error}"))?;
    let joined = base.join(candidate);
    reject_symlink_components(&base, &joined).map_err(|error| error.to_string())?;
    let resolved = base
        .join(candidate)
        .canonicalize()
        .map_err(|error| format!("resource cannot be resolved: {error}"))?;
    if !resolved.starts_with(&base) {
        return Err("resource escapes the Plugin bundle".into());
    }
    if require_file && !resolved.is_file() {
        return Err("resource is not a file".into());
    }
    if !require_file && !resolved.is_dir() {
        return Err("resource is not a directory".into());
    }
    Ok(resolved)
}

fn inside_directory(base: &Path, relative: &str) -> Result<PathBuf, String> {
    inside_path(base, relative, false)
}

fn inside_file(base: &Path, relative: &str) -> Result<PathBuf, String> {
    inside_path(base, relative, true)
}

fn collect_skill_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<String>,
) -> Result<(), GraphError> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid(format!(
                "Plugin symlinks are not supported: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            collect_skill_files(root, &path, out)?;
        } else if metadata.is_file() && path.file_name().is_some_and(|name| name == "SKILL.md") {
            out.push(
                path.strip_prefix(root)
                    .map_err(|error| invalid(error.to_string()))?
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

fn resource_files(root: &Path) -> Result<Vec<String>, GraphError> {
    let mut resources = Vec::new();
    collect_files(root, root, &mut resources)?;
    resources.sort();
    Ok(resources)
}

fn collect_server_values(
    value: Option<&Value>,
    servers: &mut serde_json::Map<String, Value>,
    id: &str,
) -> Result<(), GraphError> {
    let Some(value) = value else { return Ok(()) };
    let object = value
        .as_object()
        .ok_or_else(|| invalid(format!("Plugin {id}: mcpServers must be an object")))?;
    for (name, server) in object {
        if !valid_reference(name) || !server.is_object() {
            return Err(invalid(format!(
                "Plugin {id}: invalid MCP server declaration"
            )));
        }
        servers.insert(name.clone(), server.clone());
    }
    Ok(())
}

fn valid_environment_key(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn environment(plugin: &str, key: &str) -> Result<String, GraphError> {
    if !valid_environment_key(key) {
        return Err(invalid(format!(
            "Plugin {plugin}: invalid MCP environment variable name"
        )));
    }
    std::env::var(key).map_err(|_| {
        invalid(format!(
            "Plugin {plugin}: MCP environment variable {key} is not set"
        ))
    })
}

fn http_url_is_valid(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return false;
    };
    if rest.contains('#') || rest.contains('\0') {
        return false;
    }
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        return rest
            .split_once(']')
            .is_some_and(|(host, _)| !host.is_empty());
    }
    !authority.split(':').next().unwrap_or_default().is_empty()
}

fn normalize_inside(base: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !path.pop() || !path.starts_with(base) {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    path.starts_with(base).then_some(path)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn validate_mcp(
    id: &str,
    name: &str,
    server: &serde_json::Map<String, Value>,
) -> Result<(), GraphError> {
    let stdio = server.contains_key("command");
    let allowed: &[&str] = if stdio {
        &[
            "type",
            "enabled",
            "startup_timeout_sec",
            "tool_timeout_sec",
            "command",
            "args",
            "env",
            "env_vars",
            "optional_env_vars",
            "cwd",
        ]
    } else {
        &[
            "type",
            "enabled",
            "startup_timeout_sec",
            "tool_timeout_sec",
            "url",
            "headers",
            "http_headers",
            "env_http_headers",
            "bearer_token_env_var",
            "auth",
            "oauth_resource",
        ]
    };
    if let Some(key) = server.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(format!(
            "Plugin {id}, server {name}: unsupported MCP field: {key}"
        )));
    }
    if let Some(enabled) = server.get("enabled")
        && !enabled.is_boolean()
    {
        return Err(invalid(format!(
            "Plugin {id}, server {name}: enabled must be boolean"
        )));
    }
    let transport = server
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(if stdio { "stdio" } else { "http" });
    if (stdio && transport != "stdio") || (!stdio && !matches!(transport, "http" | "sse")) {
        return Err(invalid(format!(
            "Plugin {id}, server {name}: invalid MCP transport type"
        )));
    }
    for key in [
        "command",
        "cwd",
        "url",
        "bearer_token_env_var",
        "oauth_resource",
    ] {
        if let Some(value) = server.get(key)
            && value
                .as_str()
                .is_none_or(|value| value.is_empty() || value.contains('\0'))
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: {key} must be a nonempty string"
            )));
        }
    }
    for key in ["args", "env_vars", "optional_env_vars"] {
        if let Some(value) = server.get(key) {
            let Some(values) = value.as_array() else {
                return Err(invalid(format!(
                    "Plugin {id}, server {name}: {key} must be a list of strings"
                )));
            };
            if values
                .iter()
                .any(|value| value.as_str().is_none_or(|value| value.contains('\0')))
            {
                return Err(invalid(format!(
                    "Plugin {id}, server {name}: {key} must be a list of strings"
                )));
            }
        }
    }
    for key in ["env", "headers", "http_headers", "env_http_headers"] {
        if let Some(value) = server.get(key) {
            let Some(values) = value.as_object() else {
                return Err(invalid(format!(
                    "Plugin {id}, server {name}: {key} must contain string keys and values"
                )));
            };
            if values.iter().any(|(key, value)| {
                key.is_empty()
                    || key.contains('\0')
                    || value.as_str().is_none_or(|value| value.contains('\0'))
            }) {
                return Err(invalid(format!(
                    "Plugin {id}, server {name}: {key} must contain string keys and values"
                )));
            }
        }
    }
    for key in ["startup_timeout_sec", "tool_timeout_sec"] {
        if let Some(value) = server.get(key)
            && !value
                .as_f64()
                .is_some_and(|value| value > 0.0 && value <= 86_400.0)
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: {key} must be positive seconds, at most 86400"
            )));
        }
    }
    if !stdio {
        let url = server
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !http_url_is_valid(url) {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: url must be HTTP(S), without embedded credentials or fragment"
            )));
        }
        if server
            .get("auth")
            .is_some_and(|value| value.as_str() != Some("oauth"))
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: auth supports only oauth; use headers or bearer_token_env_var for tokens"
            )));
        }
        let oauth = server.get("auth").and_then(Value::as_str) == Some("oauth")
            || server.get("oauth_resource").is_some();
        if oauth
            && (server.contains_key("bearer_token_env_var")
                || ["headers", "http_headers", "env_http_headers"].iter().any(|key| {
                    server
                        .get(*key)
                        .and_then(Value::as_object)
                        .is_some_and(|values| {
                            values
                                .keys()
                                .any(|name| name.eq_ignore_ascii_case("authorization"))
                        })
                }))
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: OAuth cannot be combined with static Authorization"
            )));
        }
        if server.contains_key("headers") && server.contains_key("http_headers") {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: choose headers or http_headers, not both"
            )));
        }
    }
    Ok(())
}

fn expand_env(value: Value, plugin_dir: &Path, plugin_id: &str) -> Result<Value, GraphError> {
    match value {
        Value::String(text) => {
            let mut output = String::with_capacity(text.len());
            let bytes = text.as_bytes();
            let mut index = 0;
            while index < bytes.len() {
                if bytes[index] == b'$' && bytes.get(index + 1) == Some(&b'{') {
                    let Some(end) = text[index + 2..].find('}') else {
                        output.push(bytes[index] as char);
                        index += 1;
                        continue;
                    };
                    let end = index + 2 + end;
                    let token = &text[index + 2..end];
                    let (key, default) = token
                        .split_once(":-")
                        .map(|(key, default)| (key, Some(default)))
                        .unwrap_or((token, None));
                    if !valid_environment_key(key) {
                        output.push_str(&text[index..=end]);
                        index = end + 1;
                        continue;
                    }
                    let replacement = match key {
                        "CODEX_PLUGIN_ROOT" | "CLAUDE_PLUGIN_ROOT" | "PLUGIN_ROOT" => {
                            plugin_dir.to_string_lossy().into_owned()
                        }
                        _ => match std::env::var(key) {
                            Ok(value) if !value.is_empty() => value,
                            Ok(value) => default.unwrap_or(&value).to_owned(),
                            Err(_) => default.map(str::to_owned).ok_or_else(|| {
                                invalid(format!(
                                    "Plugin {plugin_id}: MCP environment variable {key} is not set"
                                ))
                            })?,
                        },
                    };
                    output.push_str(&replacement);
                    index = end + 1;
                } else {
                    let character = text[index..].chars().next().expect("index in string");
                    output.push(character);
                    index += character.len_utf8();
                }
            }
            Ok(Value::String(output))
        }
        Value::Array(items) => items
            .into_iter()
            .map(|item| expand_env(item, plugin_dir, plugin_id))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(items) => items
            .into_iter()
            .map(|(key, item)| Ok((key, expand_env(item, plugin_dir, plugin_id)?)))
            .collect::<Result<serde_json::Map<_, _>, GraphError>>()
            .map(Value::Object),
        other => Ok(other),
    }
}

fn mcp_server(
    id: &str,
    name: &str,
    value: Value,
    plugin_dir: &Path,
    resolve_env: bool,
    data_root: &Path,
) -> Result<Option<Value>, GraphError> {
    let mut server = value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid(format!("Plugin {id}: MCP server {name} must be an object")))?;
    validate_mcp(id, name, &server)?;
    if server.get("enabled") == Some(&Value::Bool(false)) {
        return Ok(None);
    }

    if resolve_env {
        server = expand_env(Value::Object(server), plugin_dir, id)?
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("expanded MCP server is not an object".into()))?;

        if let Some(command) = server.get("command").and_then(Value::as_str) {
            let executable = Path::new(command);
            // External interpreters are deployment resources. Preserve their
            // declaration here; the host sandbox must authorize the exact
            // configured tool environment before launching a process.
            if executable.is_absolute()
                && executable.starts_with(plugin_dir)
                && (!executable.is_file() || !is_executable(executable))
            {
                return Err(invalid(format!(
                    "Plugin {id}: MCP command is not an executable Plugin file"
                )));
            }
            if let Some(Value::Array(values)) = server.get("optional_env_vars")
                && values.iter().filter_map(Value::as_str).any(|name| {
                    std::env::var(name)
                        .ok()
                        .is_none_or(|value| value.trim().is_empty())
                })
            {
                return Ok(None);
            }
        }
    }

    if let Some(Value::String(cwd)) = server.get("cwd").cloned()
        && !Path::new(&cwd).is_absolute()
    {
        let path = normalize_inside(plugin_dir, &cwd)
            .ok_or_else(|| invalid(format!("Plugin {id}: MCP cwd escapes the Plugin bundle")))?;
        let path = if path.exists() {
            let resolved = path.canonicalize()?;
            if !resolved.starts_with(plugin_dir) {
                return Err(invalid(format!(
                    "Plugin {id}: MCP cwd escapes the Plugin bundle"
                )));
            }
            resolved
        } else {
            path
        };
        server.insert(
            "cwd".into(),
            Value::String(path.to_string_lossy().into_owned()),
        );
    }

    if resolve_env {
        let stdio = server.contains_key("command");
        if stdio {
            let mut env = server
                .get("env")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let names = server
                .get("env_vars")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .chain(
                    server
                        .get("optional_env_vars")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten(),
                )
                .filter_map(Value::as_str);
            for key in names {
                let value = environment(id, key)?;
                env.insert(key.into(), Value::String(value));
            }
            server.insert("env".into(), Value::Object(env));
        } else {
            let mut headers = server
                .get("http_headers")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(explicit) = server.get("headers").and_then(Value::as_object) {
                headers.extend(explicit.clone());
            }
            if let Some(values) = server.get("env_http_headers").and_then(Value::as_object) {
                for (header, variable) in values {
                    let variable = variable.as_str().ok_or_else(|| {
                        invalid(format!(
                            "Plugin {id}: invalid MCP environment variable name"
                        ))
                    })?;
                    headers.insert(header.clone(), Value::String(environment(id, variable)?));
                }
            }
            if let Some(variable) = server.get("bearer_token_env_var").and_then(Value::as_str) {
                if headers
                    .keys()
                    .any(|key| key.eq_ignore_ascii_case("authorization"))
                {
                    return Err(invalid(format!(
                        "Plugin {id}: duplicate MCP authorization configuration"
                    )));
                }
                let token = environment(id, variable)?;
                headers.insert(
                    "Authorization".into(),
                    Value::String(format!("Bearer {token}")),
                );
            }
            server.insert("headers".into(), Value::Object(headers));
        }
        server.insert("_anchor_plugin_id".into(), Value::String(id.into()));
        server.insert(
            "_anchor_plugin_dir".into(),
            Value::String(plugin_dir.to_string_lossy().into_owned()),
        );
        server.insert("_anchor_server_name".into(), Value::String(name.into()));
        server.insert(
            "_anchor_auth_dir".into(),
            Value::String(
                data_root
                    .join("state")
                    .join("mcp-auth")
                    .join(id)
                    .join(name)
                    .to_string_lossy()
                    .into_owned(),
            ),
        );
    }
    Ok(Some(Value::Object(server)))
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

pub trait GraphCatalog: Send + Sync {
    fn snapshot(&self, name: &str) -> Result<Option<GraphSnapshot>, GraphError>;

    /// Short admission lock shared with target-Graph mutation. This lock is
    /// released before child execution; child execution uses only its Run lease.
    fn lock_admission(&self, _name: &str) -> Result<Box<dyn RunLease>, GraphError> {
        Ok(Box::new(NoopLease))
    }

    /// Resolve one immutable child bundle. Catalogs that only provide snapshots
    /// keep the provider-free adapter behavior; production catalogs should
    /// return the bundle's Plugin pins as well.
    fn bundle(&self, name: &str) -> Result<Option<LoadedGraphBundle>, GraphError> {
        self.snapshot(name)?
            .map(|snapshot| {
                let authoring_definition =
                    serde_json::to_value(&snapshot).map_err(GraphError::SnapshotDecode)?;
                Ok(LoadedGraphBundle {
                    authoring_definition,
                    snapshot,
                    plugins: Vec::new(),
                })
            })
            .transpose()
    }

    /// Persist API-visible identity metadata alongside a durable child Run.
    fn record_child_admission(
        &self,
        _run_id: &str,
        _graph: &str,
        _snapshot: &GraphSnapshot,
        _plugins: &[anchor_runtime_rig::graph::PluginBinding],
        _identity: &CallIdentity,
        _mode: &str,
    ) -> Result<(), GraphError> {
        Ok(())
    }

    /// Resolve and authorize an existing channel Session. Returned context
    /// supplies the trusted Session identity and channel input for the child.
    fn prepare_session_call<'a>(
        &'a self,
        _identity: &'a CallIdentity,
        _session: &'a str,
        _graph: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
        Box::pin(async {
            Err(GraphError::Unsupported(
                "Op.call session handoff is not supported by this host".into(),
            ))
        })
    }

    /// Persist trusted Session metadata before the child Run becomes durable.
    fn record_session_call(&self, _run_id: &str, _context: &Value) -> Result<(), GraphError> {
        Err(GraphError::Unsupported(
            "Op.call session handoff is not supported by this host".into(),
        ))
    }

    /// Delegate Session scheduling, execution and message delivery to its
    /// owning host. `None` certifies successful execution and delivery; the
    /// adapter then reads the completed child to export the ordinary result.
    fn session_call_outcome<'a>(
        &'a self,
        _run_id: &'a str,
        _mode: &'a str,
        _cancellation: Cancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Option<GraphCallOutcome>, GraphError>> + Send + 'a>>
    {
        Box::pin(async {
            Err(GraphError::Unsupported(
                "Op.call session handoff is not supported by this host".into(),
            ))
        })
    }

    /// Register a wait child with the host's per-Run control registry. The
    /// returned token belongs to the child; the host may additionally link
    /// parent cancellation for wait-mode calls.
    fn begin_child_execution<'a>(
        &'a self,
        _run_id: &'a str,
        _graph: &'a str,
        parent_cancellation: Cancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ChildRunControl, GraphError>> + Send + 'a>> {
        Box::pin(async move { Ok(ChildRunControl::linked_to_parent(parent_cancellation)) })
    }

    fn end_child_execution<'a>(
        &'a self,
        _run_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn child_execution_finished<'a>(
        &'a self,
        _run_id: &'a str,
        _graph: &'a str,
        _status: RunStatus,
    ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    /// Admit a detached child to host-owned background execution. The default
    /// standalone adapter keeps admission-only behavior.
    fn dispatch_detached<'a>(
        &'a self,
        _run_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    /// Verify that an existing durable child still has its API-visible source
    /// identity. This must not require the mutable Graph definition to exist.
    fn verify_child_identity(
        &self,
        _run_id: &str,
        _graph: &str,
        _snapshot: &GraphSnapshot,
        _identity: &CallIdentity,
        _mode: &str,
    ) -> Result<(), GraphError> {
        Ok(())
    }

    fn has_child_admission(&self, _run_id: &str) -> Result<bool, GraphError> {
        Ok(false)
    }

    /// Plugin-bearing children need their pinned resources rechecked before
    /// resuming. Implementations should inspect Plugin resources directly and
    /// must not use the current Graph definition as execution truth.
    fn verify_child_plugins(
        &self,
        graph: &str,
        bindings: &[anchor_runtime_rig::graph::PluginBinding],
    ) -> Result<(), GraphError> {
        let current = self
            .bundle(graph)?
            .ok_or_else(|| GraphError::InvalidSnapshot(format!("unknown child graph `{graph}`")))?;
        if current.plugins != bindings {
            return Err(GraphError::RunConflict);
        }
        Ok(())
    }
}

struct NoopLease;
impl RunLease for NoopLease {}

#[derive(Clone)]
pub struct ChildRunControl {
    pub cancellation: Cancellation,
    pub pause: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ChildRunControl {
    pub fn linked_to_parent(cancellation: Cancellation) -> Self {
        Self {
            cancellation,
            pause: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

/// Adapts GraphCallPort on top of shared kernel ports. Instantiate one host
/// around the same store/artifact/node/control ports used by the outer Runner.
pub struct InProcessGraphHost<'a, S, A, N, C> {
    catalog: &'a dyn GraphCatalog,
    store: &'a S,
    artifacts: &'a A,
    nodes: &'a N,
    plugin_catalog: Option<&'a dyn PluginCatalog>,
    _control: std::marker::PhantomData<fn() -> C>,
}

struct BoundNodes<'a, N> {
    nodes: &'a N,
    plugins: std::collections::BTreeMap<String, anchor_runtime_rig::graph::PluginBinding>,
}

impl<N: NodeExecutionPort> NodeExecutionPort for BoundNodes<'_, N> {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        self.nodes.capabilities()
    }
    fn resolve_plugins(
        &self,
        ids: &[String],
    ) -> Result<Vec<anchor_runtime_rig::graph::PluginBinding>, GraphError> {
        ids.iter()
            .map(|id| {
                self.plugins.get(id).cloned().ok_or_else(|| {
                    GraphError::InvalidSnapshot(format!("child Plugin `{id}` is not bundle-bound"))
                })
            })
            .collect()
    }
    fn completion_fact<'b>(
        &'b self,
        key: &'b anchor_runtime_rig::graph::InvocationKey,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<anchor_runtime_rig::graph::CompletionFact, GraphError>>
                + Send
                + 'b,
        >,
    > {
        self.nodes.completion_fact(key)
    }
    fn execute<'b>(
        &'b self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'b>> {
        self.nodes.execute(request)
    }
}

struct ChildControl {
    control: ChildRunControl,
}

impl ChildControl {
    fn new(control: ChildRunControl) -> Self {
        Self { control }
    }
}

impl RunControl for ChildControl {
    fn pause_requested(&self) -> bool {
        self.control
            .pause
            .load(std::sync::atomic::Ordering::Acquire)
    }
    fn stop_requested(&self) -> bool {
        self.control
            .cancellation
            .load(std::sync::atomic::Ordering::Acquire)
    }
    fn cancellation(&self) -> Cancellation {
        self.control.cancellation.clone()
    }
}

impl<'a, S, A, N, C> InProcessGraphHost<'a, S, A, N, C> {
    pub fn new(
        catalog: &'a dyn GraphCatalog,
        store: &'a S,
        artifacts: &'a A,
        nodes: &'a N,
        _control: &'a C,
    ) -> Self {
        Self {
            catalog,
            store,
            artifacts,
            nodes,
            plugin_catalog: None,
            _control: std::marker::PhantomData,
        }
    }

    /// Configure the filesystem (or other) catalog used to pin Plugin identities.
    pub fn with_plugin_catalog(mut self, catalog: &'a dyn PluginCatalog) -> Self {
        self.plugin_catalog = Some(catalog);
        self
    }
}

impl<S: RunStore, A: ArtifactPort, N: NodeExecutionPort, C: RunControl> NodeExecutionPort
    for InProcessGraphHost<'_, S, A, N, C>
{
    fn capabilities(&self) -> NodeExecutionCapabilities {
        self.nodes.capabilities()
    }
    fn resolve_plugins(
        &self,
        ids: &[String],
    ) -> Result<Vec<anchor_runtime_rig::graph::PluginBinding>, GraphError> {
        match self.plugin_catalog {
            Some(catalog) => catalog.resolve(ids),
            None => self.nodes.resolve_plugins(ids),
        }
    }
    fn completion_fact<'b>(
        &'b self,
        key: &'b anchor_runtime_rig::graph::InvocationKey,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<anchor_runtime_rig::graph::CompletionFact, GraphError>>
                + Send
                + 'b,
        >,
    > {
        self.nodes.completion_fact(key)
    }
    fn execute<'b>(
        &'b self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'b>> {
        self.nodes.execute(request)
    }
    fn graph_call_port(&self) -> Option<&dyn GraphCallPort> {
        Some(self)
    }
}

impl<S: RunStore, A: ArtifactPort, N: NodeExecutionPort, C: RunControl> GraphCallPort
    for InProcessGraphHost<'_, S, A, N, C>
{
    fn call<'b>(
        &'b self,
        identity: &'b CallIdentity,
        spec: &'b Value,
        input: &'b Value,
        input_commits: &'b [CommitRef],
        cancellation: Cancellation,
    ) -> Pin<Box<dyn Future<Output = Result<GraphCallOutcome, GraphError>> + Send + 'b>> {
        Box::pin(async move {
            let parent = self
                .store
                .load(&identity.parent_run_id)
                .map_err(|e| GraphError::Unsupported(format!("load parent Run for call: {e}")))?
                .ok_or_else(|| GraphError::CorruptRun("Graph call parent Run is missing".into()))?;
            let cursor = parent.cursor.as_ref().ok_or_else(|| {
                GraphError::CorruptRun("Graph call parent has no active cursor".into())
            })?;
            if parent.graph_digest != identity.parent_graph_digest
                || cursor.node_id != identity.node_id
                || cursor.key.invocation != identity.invocation
                || cursor.prepared_input != *input
            {
                return Err(GraphError::CorruptRun(
                    "Graph call identity does not match durable parent cursor".into(),
                ));
            }
            let parent_node = parent
                .snapshot
                .nodes
                .iter()
                .find(|node| node.id == identity.node_id)
                .ok_or_else(|| {
                    GraphError::CorruptRun("Graph call node is missing from parent snapshot".into())
                })?;
            let frozen_spec = parent_node
                .op
                .as_ref()
                .and_then(|op_name| parent.snapshot.ops.get(op_name))
                .and_then(|op| op.get("call"))
                .ok_or_else(|| {
                    GraphError::CorruptRun("parent cursor is not an Op.call node".into())
                })?;
            let spec_digest = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(frozen_spec).map_err(GraphError::SnapshotDecode)?
                )
            );
            if frozen_spec != spec || identity.call_spec_digest != spec_digest {
                return Err(GraphError::CorruptRun(
                    "Graph call spec does not match frozen parent snapshot".into(),
                ));
            }
            // A typed, `deny_unknown_fields` decode rejects malformed
            // input/input_map/files/result and any stray field before the host
            // can create a child Run or run a child side effect.
            let call_spec: CallSpec = serde_json::from_value(spec.clone()).map_err(|error| {
                GraphError::InvalidSnapshot(format!("invalid op.call spec: {error}"))
            })?;
            if !matches!(call_spec.mode.as_str(), "wait" | "detach") {
                return Err(GraphError::InvalidSnapshot(
                    "op.call.mode must be wait or detach".into(),
                ));
            }
            let graph_name = call_spec.graph.as_str();
            let mode = call_spec.mode.as_str();
            let session_context = if let Some(session) = call_spec.session.as_deref() {
                let context = self
                    .catalog
                    .prepare_session_call(identity, session, graph_name)
                    .await?;
                if context.get("session").and_then(Value::as_str) != Some(session)
                    || !context.get("channel").is_some_and(Value::is_object)
                {
                    return Err(GraphError::CorruptRun(
                        "session call context does not match the authorized Session".into(),
                    ));
                }
                Some(context)
            } else {
                None
            };
            let mut child_input = call_spec.child_input(input)?;
            if let Some(context) = session_context.as_ref() {
                let values = child_input
                    .as_object_mut()
                    .expect("child_input returns an object");
                values.insert("session".into(), context["session"].clone());
                values.insert("channel".into(), context["channel"].clone());
            }
            let file_selections = call_spec.file_selections();
            let run_id = child_run_id(identity);
            let child_lease = self
                .store
                .acquire_lease(&run_id)
                .map_err(|e| GraphError::Unsupported(format!("acquire child lease: {e}")))?;
            let existing = self
                .store
                .load(&run_id)
                .map_err(|e| GraphError::Unsupported(format!("load child Run: {e}")))?;
            let mut child = if let Some(existing) = existing {
                if existing.input != expected_input(&existing.snapshot, &child_input) {
                    return Err(GraphError::RunConflict);
                }
                self.catalog.verify_child_identity(
                    &run_id,
                    graph_name,
                    &existing.snapshot,
                    identity,
                    mode,
                )?;
                if !existing.plugin_bindings.is_empty() {
                    let pins = existing
                        .plugin_bindings
                        .values()
                        .cloned()
                        .collect::<Vec<_>>();
                    self.catalog.verify_child_plugins(graph_name, &pins)?;
                }
                self.artifacts
                    .stage_call_inputs(&run_id, input_commits, &file_selections)
                    .await?;
                existing
            } else {
                // Serialize only the short child admission against mutations to
                // the target Graph. Existing children never consult this catalog
                // definition unless their pinned Plugin resources need checking.
                let _admission_lease = self.catalog.lock_admission(graph_name)?;
                // Recheck after taking the target lock in case another call won
                // the same identity between the first read and admission.
                if let Some(existing) = self.store.load(&run_id)? {
                    if existing.input != expected_input(&existing.snapshot, &child_input) {
                        return Err(GraphError::RunConflict);
                    }
                    self.catalog.verify_child_identity(
                        &run_id,
                        graph_name,
                        &existing.snapshot,
                        identity,
                        mode,
                    )?;
                    if !existing.plugin_bindings.is_empty() {
                        let pins = existing
                            .plugin_bindings
                            .values()
                            .cloned()
                            .collect::<Vec<_>>();
                        self.catalog.verify_child_plugins(graph_name, &pins)?;
                    }
                    self.artifacts
                        .stage_call_inputs(&run_id, input_commits, &file_selections)
                        .await?;
                    existing
                } else {
                    if self.catalog.has_child_admission(&run_id)? {
                        return Err(GraphError::CorruptRun(
                            "child admission metadata exists but its durable Run record is missing"
                                .into(),
                        ));
                    }
                    let bundle = self
                        .catalog
                        .bundle(graph_name)
                        .map_err(|e| GraphError::Unsupported(format!("load child bundle: {e}")))?
                        .ok_or_else(|| {
                            GraphError::InvalidSnapshot(format!(
                                "unknown child graph `{graph_name}`"
                            ))
                        })?;
                    let snapshot = bundle.snapshot;
                    if snapshot.ops.values().any(|op| op.get("call").is_some()) {
                        return Err(GraphError::Unsupported(
                            "nested Graph calls are not supported by this standalone host yet"
                                .into(),
                        ));
                    }
                    // Stage the read-only `/in/call` bundle before the child
                    // becomes durable. A crash can therefore never leave a
                    // Ready child (including a startup-recovered detach child)
                    // without the inputs its frozen identity selected.
                    self.artifacts
                        .stage_call_inputs(&run_id, input_commits, &file_selections)
                        .await?;
                    let mut record = GraphRunRecord::create(snapshot.clone(), child_input)?;
                    record.run_id = run_id.clone();
                    record.plugin_bindings = bundle
                        .plugins
                        .iter()
                        .cloned()
                        .map(|plugin| (plugin.id.clone(), plugin))
                        .collect();
                    record.plugin_bindings_initialized = true;
                    self.catalog
                        .record_child_admission(
                            &run_id,
                            graph_name,
                            &record.snapshot,
                            &record.plugin_bindings.values().cloned().collect::<Vec<_>>(),
                            identity,
                            mode,
                        )
                        .map_err(|e| {
                            GraphError::Unsupported(format!("persist child identity metadata: {e}"))
                        })?;
                    if let Some(context) = session_context.as_ref() {
                        self.catalog.record_session_call(&run_id, context)?;
                    }
                    self.store
                        .save(&record)
                        .map_err(|e| GraphError::Unsupported(format!("persist child Run: {e}")))?;
                    record
                }
            };
            drop(child_lease);
            if session_context.is_some() {
                if let Some(outcome) = self
                    .catalog
                    .session_call_outcome(&run_id, mode, cancellation.clone())
                    .await?
                {
                    return Ok(outcome);
                }
                child = self.store.load(&run_id)?.ok_or_else(|| {
                    GraphError::CorruptRun("completed Session child Run is missing".into())
                })?;
                if child.status != RunStatus::Completed {
                    return Err(GraphError::CorruptRun(
                        "Session host confirmed delivery without a completed child Run".into(),
                    ));
                }
            }
            // FileRunStore's record is the durable identity → child admission:
            // deterministic ID plus frozen snapshot/input are checked on retry.
            if mode == "detach" && session_context.is_none() {
                self.catalog.dispatch_detached(&run_id).await?;
                return Ok(GraphCallOutcome::Detached {
                    child_run_id: run_id,
                });
            }
            // A durable child that already reached a terminal or recovery state
            // is never re-entered: its own facts decide the parent outcome and
            // re-running it could replay an unknown side effect. Only the
            // non-terminal states are resumed along the same child Run.
            let resumable = session_context.is_none()
                && matches!(
                    child.status,
                    RunStatus::Ready
                        | RunStatus::Running
                        | RunStatus::Paused
                        | RunStatus::Stopped
                        | RunStatus::BudgetStopped
                );
            if child.status == RunStatus::WaitingCall {
                return Err(GraphError::CorruptRun(
                    "called Graph child is itself waiting on a nested Graph call".into(),
                ));
            }
            if resumable {
                if cancellation.load(std::sync::atomic::Ordering::Acquire) {
                    return Ok(GraphCallOutcome::Waiting {
                        child_run_id: run_id,
                    });
                }
                let child_control = ChildControl::new(
                    self.catalog
                        .begin_child_execution(&run_id, graph_name, cancellation.clone())
                        .await?,
                );
                let child_nodes = BoundNodes {
                    nodes: self.nodes,
                    plugins: child.plugin_bindings.clone(),
                };
                child = GraphRunner::new(self.store, self.artifacts, &child_nodes, &child_control)
                    .run(child)
                    .await
                    .map_err(|e| GraphError::Unsupported(format!("execute child Run: {e}")))?;
                self.catalog.end_child_execution(&run_id).await;
                self.catalog
                    .child_execution_finished(&run_id, graph_name, child.status)
                    .await?;
            }
            match child.status {
                RunStatus::Completed => {
                    let output = if let Some(selector) = call_spec.result.as_ref() {
                        let result = child
                            .results
                            .get(&selector.node)
                            .and_then(|results| results.iter().max_by_key(|r| r.sequence))
                            .ok_or_else(|| {
                                GraphError::CorruptRun(format!(
                                    "called Graph `{graph_name}` produced no result node `{}`",
                                    selector.node
                                ))
                            })?;
                        let call_key = InvocationKey {
                            run_id: identity.parent_run_id.clone(),
                            graph_digest: identity.parent_graph_digest.clone(),
                            node_id: identity.node_id.clone(),
                            invocation: identity.invocation,
                        };
                        let copied = self
                            .artifacts
                            .export_call_result_files(&call_key, &result.commit, &selector.files)
                            .await?;
                        serde_json::json!({
                            "graph": graph_name,
                            "run": child.run_id,
                            "mode": "wait",
                            "status": "completed",
                            "summary": result.completion.submission,
                            "result": {
                                "node": result.node_id,
                                "commit": result.commit,
                                "files": copied,
                            },
                        })
                    } else {
                        child
                            .results
                            .values()
                            .flatten()
                            .max_by_key(|result| result.sequence)
                            .map(|result| result.completion.output.clone())
                            .ok_or_else(|| {
                                GraphError::CorruptRun("completed child has no result".into())
                            })?
                    };
                    Ok(GraphCallOutcome::Completed {
                        child_run_id: run_id,
                        output,
                    })
                }
                RunStatus::WaitingCall
                | RunStatus::WaitingRecovery
                | RunStatus::BudgetStopped
                | RunStatus::Paused
                | RunStatus::Stopped => Ok(GraphCallOutcome::Waiting {
                    child_run_id: run_id,
                }),
                RunStatus::Aborted => Ok(GraphCallOutcome::Failed {
                    child_run_id: Some(run_id),
                    reason: child.error.unwrap_or_else(|| "child Graph failed".into()),
                }),
                // A known failure clears its cursor; a retained cursor means the
                // child could not prove its terminal fact (unknown side effect
                // or an unproven completion). Report that as Uncertain so the
                // parent records it and never treats the call as a settled
                // known failure it may replay.
                RunStatus::Failed if child.cursor.is_some() => Ok(GraphCallOutcome::Uncertain {
                    child_run_id: Some(run_id),
                    reason: child.error.unwrap_or_else(|| {
                        "child Graph failed without a proven terminal fact".into()
                    }),
                }),
                RunStatus::Failed => Ok(GraphCallOutcome::Failed {
                    child_run_id: Some(run_id),
                    reason: child.error.unwrap_or_else(|| "child Graph failed".into()),
                }),
                RunStatus::Ready | RunStatus::Running => Ok(GraphCallOutcome::Uncertain {
                    child_run_id: Some(run_id),
                    reason: format!("child Run returned nonterminal status {:?}", child.status),
                }),
            }
        })
    }
}

fn child_run_id(identity: &CallIdentity) -> String {
    format!("call-{}", identity.durable_key())
}

/// Strict, typed view of a frozen `ops.<name>.call` object. Unknown fields are
/// rejected so any future/unrecognized capability cannot be
/// silently ignored before a child Run or side effect is created.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallSpec {
    graph: String,
    mode: String,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    input_map: BTreeMap<String, String>,
    #[serde(default)]
    files: Vec<CallSpecFile>,
    #[serde(default)]
    result: Option<CallSpecResult>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallSpecFile {
    node: String,
    path: String,
    #[serde(rename = "as")]
    alias: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallSpecResult {
    node: String,
    #[serde(default)]
    files: Vec<String>,
}

impl CallSpec {
    /// Resolve the `input` constants merged with the explicit `input_map`
    /// pointers. Pointers are evaluated with RFC6901 semantics against the
    /// parent Run's own input (the frozen `input` field of the prepared input),
    /// matching the contract that only explicitly selected data crosses the
    /// boundary.
    fn child_input(&self, prepared_input: &Value) -> Result<Value, GraphError> {
        let source = prepared_input.get("input").unwrap_or(&Value::Null);
        let mut values = match &self.input {
            Some(value) if value.is_object() => value.clone(),
            Some(_) => {
                return Err(GraphError::InvalidSnapshot(
                    "op.call input must be an object".into(),
                ));
            }
            None => serde_json::json!({}),
        };
        let target = values
            .as_object_mut()
            .expect("checked object input before mapping");
        for (key, pointer) in &self.input_map {
            let value = source.pointer(pointer).ok_or_else(|| {
                GraphError::InvalidSnapshot(format!(
                    "op.call input_map pointer `{pointer}` is not visible from the parent input"
                ))
            })?;
            target.insert(key.clone(), value.clone());
        }
        Ok(values)
    }

    fn file_selections(&self) -> Vec<CallFileSelection> {
        self.files
            .iter()
            .map(|file| CallFileSelection {
                node: file.node.clone(),
                path: file.path.clone(),
                alias: file.alias.clone(),
            })
            .collect()
    }
}

fn expected_input(snapshot: &GraphSnapshot, provided: &Value) -> Value {
    // GraphRunRecord::create applies the kernel's input merge rules. Keep this
    // helper aligned with the currently supported object merge behavior.
    fn merge(base: &Value, over: &Value) -> Value {
        match (base, over) {
            (Value::Object(a), Value::Object(b)) => {
                let mut result = a.clone();
                for (key, value) in b {
                    let merged = result
                        .get(key)
                        .filter(|old| old.is_object() && value.is_object())
                        .map(|old| merge(old, value))
                        .unwrap_or_else(|| value.clone());
                    result.insert(key.clone(), merged);
                }
                Value::Object(result)
            }
            (_, Value::Null) => base.clone(),
            (_, value) => value.clone(),
        }
    }
    merge(&snapshot.input, provided)
}

#[cfg(test)]
mod plugin_catalog_tests {
    use super::*;
    use std::fs;

    fn catalog_with_plugin() -> (tempfile::TempDir, FilePluginCatalog) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plugins/demo");
        fs::create_dir_all(dir.join("skills/example")).unwrap();
        fs::write(dir.join("plugin.json"), r#"{"name":"Demo","mcpServers":{"inline":{"command":"run","env":{"TOKEN":"top-secret"}},"disabled":{"enabled":false,"url":"https://example.test/mcp"}}}"#).unwrap();
        fs::write(
            dir.join(".mcp.json"),
            r#"{"mcpServers":{"config":{"url":"https://example.test/mcp","headers":{"Authorization":"secret-value"}}}}"#,
        )
        .unwrap();
        fs::write(dir.join("skills/example/SKILL.md"), "hello").unwrap();
        let catalog = FilePluginCatalog::new(tmp.path());
        (tmp, catalog)
    }

    #[test]
    fn resolves_sorted_bundle_digest_resources_and_secret_free_mcp_names() {
        let (_tmp, catalog) = catalog_with_plugin();
        let binding = catalog.resolve(&["demo".into()]).unwrap().remove(0);
        assert_eq!(binding.id, "demo");
        assert_eq!(
            binding.resources,
            vec![".mcp.json", "plugin.json", "skills/example/SKILL.md"]
        );
        assert_eq!(binding.mcp_servers, vec!["config", "inline"]);
        let serialized = serde_json::to_string(&binding).unwrap();
        assert!(!serialized.contains("top-secret"));
        assert!(!serialized.contains("secret-value"));
        assert!(!serialized.contains("TOKEN"));
    }

    #[test]
    fn bundle_mutation_changes_digest() {
        let (tmp, catalog) = catalog_with_plugin();
        let before = catalog.resolve(&["demo".into()]).unwrap().remove(0).digest;
        fs::write(
            tmp.path().join("plugins/demo/skills/example/SKILL.md"),
            "changed",
        )
        .unwrap();
        let after = catalog.resolve(&["demo".into()]).unwrap().remove(0).digest;
        assert_ne!(before, after);
    }

    #[test]
    fn unknown_plugin_is_rejected() {
        let (_tmp, catalog) = catalog_with_plugin();
        assert!(catalog.resolve(&["missing".into()]).is_err());
        assert!(catalog.resolve(&["../escape".into()]).is_err());
    }

    #[test]
    fn package_local_credentials_are_rejected() {
        let (tmp, catalog) = catalog_with_plugin();
        fs::write(tmp.path().join("plugins/demo/.env"), "TOKEN=secret").unwrap();
        assert!(catalog.resolve(&["demo".into()]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;
        let (tmp, catalog) = catalog_with_plugin();
        let outside = tmp.path().join("outside.json");
        fs::write(&outside, r#"{"mcpServers":{"escape":{}}}"#).unwrap();
        symlink(&outside, tmp.path().join("plugins/demo/escape.json")).unwrap();
        assert!(catalog.resolve(&["demo".into()]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn plugin_root_symlink_is_accepted() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source/demo");
        fs::create_dir_all(source.join("skills/example")).unwrap();
        fs::write(
            source.join("plugin.json"),
            r#"{"name":"Demo","mcpServers":{}}"#,
        )
        .unwrap();
        fs::write(source.join("skills/example/SKILL.md"), "hello").unwrap();
        fs::create_dir_all(tmp.path().join("plugins")).unwrap();
        symlink(&source, tmp.path().join("plugins/demo")).unwrap();

        let binding = FilePluginCatalog::new(tmp.path())
            .resolve(&["demo".into()])
            .unwrap()
            .remove(0);
        assert_eq!(
            binding.resources,
            vec!["plugin.json", "skills/example/SKILL.md"]
        );
    }

    #[test]
    fn adopts_python_manifest_skills_description_channels_and_mcp_merge() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plugins/contract");
        fs::create_dir_all(dir.join("custom/one")).unwrap();
        fs::write(dir.join("custom/one/SKILL.md"), "skill").unwrap();
        fs::write(dir.join("gateway.py"), "# entrypoint").unwrap();
        fs::write(
            dir.join("plugin.json"),
            r#"{
                "name":"Contract",
                "description":"",
                "interface":{"longDescription":"from interface"},
                "skills":["custom"],
                "hooks":[],
                "mcpServers":"mcp-config.json"
            }"#,
        )
        .unwrap();
        fs::write(
            dir.join("mcp-config.json"),
            r#"{"mcpServers":{"gateway":{"url":"https://${MISSING:-example.test}/mcp","headers":{"X-Root":"${CODEX_PLUGIN_ROOT}","X-Literal":"${bad-name}"}}}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("channel.json"),
            r#"{"platform":"demo_platform","transport":"websocket","entrypoint":"gateway.py","required_environment":["DEMO_TOKEN"],"description":"channel","sdk":"sdk"}"#,
        )
        .unwrap();

        let catalog = FilePluginCatalog::new(tmp.path());
        let definition = catalog.definition("contract").unwrap();
        assert_eq!(definition.description, "from interface");
        assert_eq!(definition.skills, vec!["custom/one/SKILL.md"]);
        assert!(definition.unsupported.is_empty());
        assert_eq!(definition.mcp_servers.len(), 1);
        assert_eq!(definition.channels[0].platform, "demo_platform");
        assert_eq!(
            definition.channels[0].required_environment,
            vec!["DEMO_TOKEN"]
        );

        let servers = catalog.mcp_servers("contract", true).unwrap();
        let config = servers[0].config.as_object().unwrap();
        assert_eq!(config["url"], "https://example.test/mcp");
        assert_eq!(config["headers"]["X-Root"], dir.to_string_lossy().as_ref());
        assert_eq!(config["headers"]["X-Literal"], "${bad-name}");
    }

    #[test]
    fn mcp_environment_and_paths_are_validated_before_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plugins/invalid");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("plugin.json"),
            r#"{"name":"Invalid","mcpServers":{"bad":{"command":"echo","env_vars":["bad-name"]}}}"#,
        )
        .unwrap();
        let catalog = FilePluginCatalog::new(tmp.path());
        assert!(catalog.mcp_servers("invalid", true).is_err());

        fs::write(
            dir.join("plugin.json"),
            r#"{"name":"Invalid","mcpServers":{"bad":{"url":"https://example.test","headers":{"Authorization":"x"},"bearer_token_env_var":"MISSING_ANCHOR_TOKEN"}}}"#,
        )
        .unwrap();
        assert!(catalog.mcp_servers("invalid", true).is_err());

        fs::write(
            dir.join("plugin.json"),
            r#"{"name":"Invalid","mcpServers":{"oauth":{"url":"https://example.test","auth":"oauth","headers":{"authorization":"static-secret"}}}}"#,
        )
        .unwrap();
        assert!(catalog.mcp_servers("invalid", true).is_err());
    }
}

/// Strict parsing/selection tests for the frozen Op.call spec. These run
/// provider-free and cover the boundary checks the host performs before it
/// creates any child Run or side effect.
#[cfg(test)]
mod call_spec_tests {
    use super::*;
    use serde_json::json;

    fn parse(value: Value) -> Result<CallSpec, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn accepts_session_and_rejects_unknown_fields_and_wrong_types() {
        let call = parse(json!({"graph":"c","mode":"wait","session":"ops"})).unwrap();
        assert_eq!(call.session.as_deref(), Some("ops"));
        assert!(parse(json!({"graph":"c","mode":"wait","session":1})).is_err());
        assert!(parse(json!({"graph":"c","mode":"wait","bogus":1})).is_err());
        assert!(parse(json!({"graph":"c","mode":"wait","input":{"a":1},"input_map":[]})).is_err());
        assert!(
            parse(json!({"graph":"c","mode":"wait","result":{"node":"n","files":"x"}})).is_err()
        );
        assert!(
            parse(json!({"graph":"c","mode":"wait","files":[{"node":"n","path":"p"}]})).is_err()
        );
    }

    #[test]
    fn input_map_resolves_rfc6901_pointers_against_the_parent_input() {
        let call = parse(json!({
            "graph":"c","mode":"wait",
            "input":{"constant":1},
            "input_map":{"topic":"/nested/topic","escaped":"/a~1b"}
        }))
        .unwrap();
        let resolved = call
            .child_input(&json!({"input":{"nested":{"topic":"x"},"a/b":"y"}}))
            .unwrap();
        assert_eq!(resolved, json!({"constant":1,"topic":"x","escaped":"y"}));
        // An unreachable pointer must fail rather than silently drop the key.
        assert!(matches!(
            call.child_input(&json!({"input":{}})),
            Err(GraphError::InvalidSnapshot(_))
        ));
        // A non-object `input` constant is rejected before admission.
        let bad = parse(json!({"graph":"c","mode":"wait","input":[1,2]})).unwrap();
        assert!(matches!(
            bad.child_input(&json!({"input":{}})),
            Err(GraphError::InvalidSnapshot(_))
        ));
    }
}

#[cfg(test)]
mod session_call_tests {
    use super::*;
    use anchor_runtime_rig::graph::{CompletionFact, FileRunStore, NodeCompletion, RunCursor};
    use serde_json::json;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct Artifacts {
        exports: Mutex<Vec<(InvocationKey, CommitRef, Vec<String>)>>,
    }

    impl ArtifactPort for Artifacts {
        fn freeze<'a>(
            &'a self,
            key: &'a InvocationKey,
            _: &'a NodeCompletion,
        ) -> Pin<Box<dyn Future<Output = Result<CommitRef, GraphError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(CommitRef {
                    id: format!("commit-{}", key.durable_key()),
                    node_id: key.node_id.clone(),
                    invocation: key.invocation,
                })
            })
        }

        fn resolve<'a>(
            &'a self,
            _: &'a CommitRef,
        ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
            Box::pin(async { Ok(json!({})) })
        }

        fn export_call_result_files<'a>(
            &'a self,
            key: &'a InvocationKey,
            commit: &'a CommitRef,
            files: &'a [String],
        ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, GraphError>> + Send + 'a>> {
            Box::pin(async move {
                self.exports
                    .lock()
                    .unwrap()
                    .push((key.clone(), commit.clone(), files.to_vec()));
                Ok(files.iter().map(|file| format!("result/{file}")).collect())
            })
        }
    }

    #[derive(Default)]
    struct Nodes(AtomicUsize);

    impl NodeExecutionPort for Nodes {
        fn capabilities(&self) -> NodeExecutionCapabilities {
            NodeExecutionCapabilities {
                agent: false,
                op_run: true,
                exact_provider_request_budget: true,
            }
        }

        fn completion_fact<'a>(
            &'a self,
            _: &'a InvocationKey,
        ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
            Box::pin(async { Ok(CompletionFact::NotStarted) })
        }

        fn execute<'a>(
            &'a self,
            request: NodeExecutionRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>>
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(NodeExecutionOutcome::Completed(NodeCompletion {
                    submission: "session reply".into(),
                    route: None,
                    model_requests: 0,
                    output: request.input,
                }))
            })
        }
    }

    struct Control;

    impl RunControl for Control {
        fn pause_requested(&self) -> bool {
            false
        }
        fn stop_requested(&self) -> bool {
            false
        }
        fn cancellation(&self) -> Cancellation {
            Cancellation::default()
        }
    }

    fn child_snapshot() -> GraphSnapshot {
        GraphSnapshot::from_authoring(json!({
            "objective":"child", "input":{"default":1}, "entry":"work",
            "agents":{}, "ops":{"work":{"run":"true"}},
            "nodes":[{"id":"work","op":"work"}], "edges":[]
        }))
        .unwrap()
    }

    struct OrdinaryCatalog;

    impl GraphCatalog for OrdinaryCatalog {
        fn snapshot(&self, _: &str) -> Result<Option<GraphSnapshot>, GraphError> {
            Ok(Some(child_snapshot()))
        }
    }

    #[derive(Clone, Copy)]
    enum SessionAction {
        Waiting,
        Detached,
        Complete,
        Unproven,
    }

    struct SessionCatalog<'a> {
        store: FileRunStore,
        artifacts: &'a Artifacts,
        context: Value,
        authorized: bool,
        action: Mutex<SessionAction>,
        admissions: AtomicUsize,
        contexts: Mutex<Vec<Value>>,
        nodes: Nodes,
    }

    impl<'a> SessionCatalog<'a> {
        fn new(store: &FileRunStore, artifacts: &'a Artifacts, action: SessionAction) -> Self {
            Self {
                store: store.clone(),
                artifacts,
                context: json!({
                    "session":"trusted-session", "reply_node":"work",
                    "conversation_id":"trusted-user",
                    "channel":{"source":"wecom", "sender_id":"trusted-user"}
                }),
                authorized: true,
                action: Mutex::new(action),
                admissions: AtomicUsize::new(0),
                contexts: Mutex::new(Vec::new()),
                nodes: Nodes::default(),
            }
        }
    }

    impl GraphCatalog for SessionCatalog<'_> {
        fn snapshot(&self, _: &str) -> Result<Option<GraphSnapshot>, GraphError> {
            Ok(Some(child_snapshot()))
        }

        fn record_child_admission(
            &self,
            run_id: &str,
            _: &str,
            _: &GraphSnapshot,
            _: &[anchor_runtime_rig::graph::PluginBinding],
            _: &CallIdentity,
            _: &str,
        ) -> Result<(), GraphError> {
            assert!(self.store.load(run_id)?.is_none());
            self.admissions.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn prepare_session_call<'a>(
            &'a self,
            _: &'a CallIdentity,
            session: &'a str,
            graph: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Value, GraphError>> + Send + 'a>> {
            Box::pin(async move {
                assert_eq!(session, "trusted-session");
                assert_eq!(graph, "child");
                if self.authorized {
                    Ok(self.context.clone())
                } else {
                    Err(GraphError::Unsupported("Session is not authorized".into()))
                }
            })
        }

        fn record_session_call(&self, run_id: &str, context: &Value) -> Result<(), GraphError> {
            assert_eq!(self.admissions.load(Ordering::SeqCst), 1);
            assert!(self.store.load(run_id)?.is_none());
            self.contexts.lock().unwrap().push(context.clone());
            Ok(())
        }

        fn session_call_outcome<'a>(
            &'a self,
            run_id: &'a str,
            mode: &'a str,
            _: Cancellation,
        ) -> Pin<Box<dyn Future<Output = Result<Option<GraphCallOutcome>, GraphError>> + Send + 'a>>
        {
            Box::pin(async move {
                drop(self.store.acquire_lease(run_id)?);
                assert_eq!(self.contexts.lock().unwrap().len(), 1);
                let action = *self.action.lock().unwrap();
                match action {
                    SessionAction::Waiting => Ok(Some(GraphCallOutcome::Waiting {
                        child_run_id: run_id.into(),
                    })),
                    SessionAction::Detached => {
                        assert_eq!(mode, "detach");
                        Ok(Some(GraphCallOutcome::Detached {
                            child_run_id: run_id.into(),
                        }))
                    }
                    SessionAction::Complete => {
                        let child = self.store.load(run_id)?.unwrap();
                        let completed =
                            GraphRunner::new(&self.store, self.artifacts, &self.nodes, &Control)
                                .run(child)
                                .await?;
                        assert_eq!(completed.status, RunStatus::Completed);
                        Ok(None)
                    }
                    SessionAction::Unproven => Ok(None),
                }
            })
        }

        fn dispatch_detached<'a>(
            &'a self,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), GraphError>> + Send + 'a>> {
            Box::pin(async { panic!("Session child bypassed its owning host") })
        }
    }

    fn parent_call(store: &FileRunStore, spec: &Value) -> (CallIdentity, Value) {
        let snapshot = GraphSnapshot::from_authoring(json!({
            "objective":"parent", "entry":"invoke", "agents":{},
            "ops":{"invoke":{"call":spec}},
            "nodes":[{"id":"invoke","op":"invoke"}], "edges":[]
        }))
        .unwrap();
        let mut parent = GraphRunRecord::create(snapshot, json!({})).unwrap();
        let input = json!({"input":{"session":"forged", "channel":{"sender_id":"forged"}}});
        parent.status = RunStatus::Running;
        parent.invocations.insert("invoke".into(), 1);
        parent.passes.insert("invoke".into(), 1);
        parent.cursor = Some(RunCursor {
            node_id: "invoke".into(),
            key: InvocationKey {
                run_id: parent.run_id.clone(),
                graph_digest: parent.graph_digest.clone(),
                node_id: "invoke".into(),
                invocation: 1,
            },
            input_commits: Vec::new(),
            prepared_input: input.clone(),
        });
        let identity = CallIdentity {
            parent_run_id: parent.run_id.clone(),
            parent_graph_digest: parent.graph_digest.clone(),
            node_id: "invoke".into(),
            invocation: 1,
            call_spec_digest: format!("{:x}", Sha256::digest(serde_json::to_vec(spec).unwrap())),
        };
        store.save(&parent).unwrap();
        (identity, input)
    }

    fn session_spec(mode: &str) -> Value {
        json!({
            "graph":"child", "mode":mode, "session":"trusted-session",
            "input":{"session":"forged", "channel":{"sender_id":"forged"}},
            "input_map":{"session":"/session", "channel":"/channel"}
        })
    }

    #[tokio::test]
    async fn session_wait_reuses_admission_and_exports_completed_result() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileRunStore::new(dir.path());
        let artifacts = Artifacts::default();
        let catalog = SessionCatalog::new(&store, &artifacts, SessionAction::Waiting);
        let nodes = Nodes::default();
        let host = InProcessGraphHost::new(&catalog, &store, &artifacts, &nodes, &Control);
        let mut spec = session_spec("wait");
        spec["result"] = json!({"node":"work", "files":["report.txt"]});
        let (identity, input) = parent_call(&store, &spec);
        let child_id = child_run_id(&identity);
        let waiting = host
            .call(&identity, &spec, &input, &[], Cancellation::default())
            .await
            .unwrap();
        assert_eq!(
            waiting,
            GraphCallOutcome::Waiting {
                child_run_id: child_id.clone()
            }
        );
        let child = store.load(&child_id).unwrap().unwrap();
        assert_eq!(child.status, RunStatus::Ready);
        assert_eq!(child.input["session"], catalog.context["session"]);
        assert_eq!(child.input["channel"], catalog.context["channel"]);
        assert_eq!(catalog.contexts.lock().unwrap()[0], catalog.context);
        assert_eq!(nodes.0.load(Ordering::SeqCst), 0);

        *catalog.action.lock().unwrap() = SessionAction::Complete;
        let completed = host
            .call(&identity, &spec, &input, &[], Cancellation::default())
            .await
            .unwrap();
        let GraphCallOutcome::Completed {
            child_run_id,
            output,
        } = completed
        else {
            panic!("expected completed Session call");
        };
        assert_eq!(child_run_id, child_id);
        assert_eq!(output["summary"], "session reply");
        assert_eq!(output["result"]["files"], json!(["result/report.txt"]));
        assert_eq!(catalog.admissions.load(Ordering::SeqCst), 1);
        assert_eq!(catalog.contexts.lock().unwrap().len(), 1);
        assert_eq!(catalog.nodes.0.load(Ordering::SeqCst), 1);
        assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
        let exports = artifacts.exports.lock().unwrap();
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].0.run_id, identity.parent_run_id);
        assert_eq!(exports[0].1.node_id, "work");
        assert_eq!(exports[0].2, vec!["report.txt"]);
    }

    #[tokio::test]
    async fn session_detach_uses_session_host_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileRunStore::new(dir.path());
        let artifacts = Artifacts::default();
        let catalog = SessionCatalog::new(&store, &artifacts, SessionAction::Detached);
        let nodes = Nodes::default();
        let host = InProcessGraphHost::new(&catalog, &store, &artifacts, &nodes, &Control);
        let spec = session_spec("detach");
        let (identity, input) = parent_call(&store, &spec);
        assert_eq!(
            host.call(&identity, &spec, &input, &[], Cancellation::default())
                .await
                .unwrap(),
            GraphCallOutcome::Detached {
                child_run_id: child_run_id(&identity)
            }
        );
        assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn session_rejection_or_invalid_context_never_admits_child() {
        for context in [
            None,
            Some(json!({"session":"other-session", "channel":{}})),
            Some(json!({"session":"trusted-session", "channel":false})),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let store = FileRunStore::new(dir.path());
            let artifacts = Artifacts::default();
            let mut catalog = SessionCatalog::new(&store, &artifacts, SessionAction::Complete);
            if let Some(context) = context {
                catalog.context = context;
            } else {
                catalog.authorized = false;
            }
            let nodes = Nodes::default();
            let host = InProcessGraphHost::new(&catalog, &store, &artifacts, &nodes, &Control);
            let spec = session_spec("wait");
            let (identity, input) = parent_call(&store, &spec);
            let result = host
                .call(&identity, &spec, &input, &[], Cancellation::default())
                .await;
            assert!(matches!(
                result,
                Err(GraphError::Unsupported(_) | GraphError::CorruptRun(_))
            ));
            assert!(store.load(&child_run_id(&identity)).unwrap().is_none());
            assert_eq!(catalog.admissions.load(Ordering::SeqCst), 0);
            assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn session_delivery_confirmation_requires_completed_child() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileRunStore::new(dir.path());
        let artifacts = Artifacts::default();
        let catalog = SessionCatalog::new(&store, &artifacts, SessionAction::Unproven);
        let nodes = Nodes::default();
        let host = InProcessGraphHost::new(&catalog, &store, &artifacts, &nodes, &Control);
        let spec = session_spec("wait");
        let (identity, input) = parent_call(&store, &spec);
        assert!(matches!(
            host.call(&identity, &spec, &input, &[], Cancellation::default()).await,
            Err(GraphError::CorruptRun(message)) if message.contains("without a completed child")
        ));
        assert_eq!(nodes.0.load(Ordering::SeqCst), 0);
        assert!(artifacts.exports.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn ordinary_host_rejects_session_but_keeps_ordinary_wait_behavior() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileRunStore::new(dir.path());
        let artifacts = Artifacts::default();
        let nodes = Nodes::default();
        let host = InProcessGraphHost::new(&OrdinaryCatalog, &store, &artifacts, &nodes, &Control);
        let spec = session_spec("wait");
        let (identity, input) = parent_call(&store, &spec);
        assert!(matches!(
            host.call(&identity, &spec, &input, &[], Cancellation::default()).await,
            Err(GraphError::Unsupported(message)) if message.contains("session handoff")
        ));
        assert!(store.load(&child_run_id(&identity)).unwrap().is_none());
        assert_eq!(nodes.0.load(Ordering::SeqCst), 0);

        let spec = json!({"graph":"child", "mode":"wait"});
        let (identity, input) = parent_call(&store, &spec);
        assert!(matches!(
            host.call(&identity, &spec, &input, &[], Cancellation::default())
                .await,
            Ok(GraphCallOutcome::Completed { .. })
        ));
        assert_eq!(nodes.0.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod bundle_loader_tests {
    use super::*;
    use std::fs;

    fn graph_json(plugins: &[&str]) -> Value {
        let mut graph = serde_json::json!({
            "objective":"demo",
            "entry":"work",
            "agents":{},
            "ops":{"work":{"run":"true"}},
            "nodes":[{"id":"work","op":"work","plugins":plugins}],
            "edges":[]
        });
        if !plugins.is_empty() {
            graph["agents"] = serde_json::json!({
                "worker":{"model":"fixture","instructions":"work"}
            });
            graph["ops"] = serde_json::json!({});
            graph["nodes"][0] = serde_json::json!({
                "id":"work","agent":"worker","plugins":plugins
            });
        }
        graph
    }

    fn bundle() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("plugins/demo/skills")).unwrap();
        fs::write(
            tmp.path().join("plugins/demo/plugin.json"),
            r#"{"name":"Demo"}"#,
        )
        .unwrap();
        fs::write(
            tmp.path().join("plugins/demo/skills/SKILL.md"),
            "safe resource",
        )
        .unwrap();
        fs::write(
            tmp.path().join("graph.json"),
            graph_json(&["demo"]).to_string(),
        )
        .unwrap();
        let binding = FilePluginCatalog::new(tmp.path())
            .resolve(&["demo".into()])
            .unwrap()
            .remove(0);
        let manifest = serde_json::json!({
            "format":1,
            "graph":"graph.json",
            "plugins":[{
                "id":binding.id,
                "digest":binding.digest,
                "resources":binding.resources,
                "mcp_servers":binding.mcp_servers
            }]
        });
        fs::write(tmp.path().join("manifest.json"), manifest.to_string()).unwrap();
        tmp
    }

    #[test]
    fn loads_graph_through_shared_admission_and_returns_only_plugin_summary() {
        let tmp = bundle();
        let loaded = FileGraphBundleLoader::new(tmp.path()).load().unwrap();
        assert_eq!(loaded.snapshot.objective, "demo");
        assert_eq!(loaded.authoring_definition, graph_json(&["demo"]));
        assert_eq!(loaded.plugins.len(), 1);
        assert_eq!(loaded.plugins[0].id, "demo");
        assert!(loaded.plugins[0].resources.contains(&"plugin.json".into()));
    }

    #[test]
    fn retains_authoring_graph_and_layout_while_loading_flat_runtime_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let authoring = serde_json::json!({
            "objective":"module graph",
            "entry":"stage",
            "agents":{},
            "ops":{"work":{"run":"true"}},
            "nodes":[{"id":"stage","graph":"inner"}],
            "edges":[],
            "graphs":{"inner":{
                "entry":"inside",
                "exit":"inside",
                "nodes":[{"id":"inside","op":"work"}],
                "edges":[]
            }},
            "layout":{"positions":{"stage":{"x":17,"y":29}},
                       "edgeLabels":{"stage|done":"Finished"}}
        });
        fs::write(
            tmp.path().join("graph.json"),
            serde_json::to_vec(&authoring).unwrap(),
        )
        .unwrap();
        fs::write(
            tmp.path().join("manifest.json"),
            r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
        )
        .unwrap();

        let loaded = FileGraphBundleLoader::new(tmp.path()).load().unwrap();
        assert_eq!(loaded.authoring_definition, authoring);
        assert_eq!(
            loaded.authoring_definition["layout"]["positions"]["stage"]["x"],
            17
        );
        assert_eq!(
            loaded.authoring_definition["graphs"]["inner"]["exit"],
            "inside"
        );
        assert_eq!(loaded.snapshot.nodes[0].id, "stage/inside");
        assert_eq!(loaded.snapshot.entry, "stage/inside");
    }

    #[test]
    fn manifest_plugin_set_matches_references_after_module_expansion() {
        let tmp = bundle();
        let authoring = serde_json::json!({
            "objective":"module plugin graph",
            "entry":"stage",
            "agents":{"worker":{"model":"fixture","instructions":"work"}},
            "ops":{},
            "nodes":[{"id":"stage","graph":"inner"}],
            "edges":[],
            "graphs":{"inner":{
                "entry":"inside",
                "exit":"inside",
                "nodes":[{"id":"inside","agent":"worker","plugins":["demo"]}],
                "edges":[]
            }},
            "layout":{"positions":{"stage":{"x":31,"y":47}}}
        });
        fs::write(
            tmp.path().join("graph.json"),
            serde_json::to_vec(&authoring).unwrap(),
        )
        .unwrap();
        let binding = FilePluginCatalog::new(tmp.path())
            .resolve(&["demo".into()])
            .unwrap()
            .remove(0);
        fs::write(
            tmp.path().join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "format":1,
                "graph":"graph.json",
                "plugins":[{
                    "id":binding.id,
                    "digest":binding.digest,
                    "resources":binding.resources,
                    "mcp_servers":binding.mcp_servers
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let loaded = FileGraphBundleLoader::new(tmp.path()).load().unwrap();
        assert_eq!(loaded.authoring_definition, authoring);
        assert_eq!(loaded.snapshot.nodes[0].id, "stage/inside");
        assert_eq!(loaded.snapshot.nodes[0].plugins, ["demo"]);
        assert_eq!(loaded.plugins.len(), 1);
    }

    #[test]
    fn rejects_manifest_unknown_fields_graph_paths_and_plugin_set_drift() {
        let tmp = bundle();
        let path = tmp.path().join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["extra"] = Value::Bool(true);
        fs::write(&path, manifest.to_string()).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        let path = tmp.path().join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["graph"] = Value::String("../outside.json".into());
        fs::write(&path, manifest.to_string()).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        let path = tmp.path().join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["format"] = Value::Number(2.into());
        fs::write(&path, manifest.to_string()).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        let path = tmp.path().join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["plugins"] = serde_json::json!([]);
        fs::write(&path, manifest.to_string()).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        fs::write(tmp.path().join("graph.json"), graph_json(&[]).to_string()).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());
    }

    #[test]
    fn rejects_resource_digest_drift_extra_files_and_credentials() {
        let tmp = bundle();
        fs::write(tmp.path().join("plugins/demo/skills/SKILL.md"), "tampered").unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        fs::write(tmp.path().join("unexpected.txt"), "payload").unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        fs::write(tmp.path().join("plugins/demo/.env"), "TOKEN=secret").unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_graph_and_plugin_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = bundle();
        let outside = tmp.path().join("outside.json");
        fs::write(&outside, graph_json(&["demo"]).to_string()).unwrap();
        fs::remove_file(tmp.path().join("graph.json")).unwrap();
        symlink(&outside, tmp.path().join("graph.json")).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::remove_dir_all(tmp.path().join("plugins/demo/skills")).unwrap();
        symlink(&outside, tmp.path().join("plugins/demo/skills")).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());

        let tmp = bundle();
        let outside = tmp.path().join("outside/demo");
        fs::create_dir_all(outside.parent().unwrap()).unwrap();
        fs::rename(tmp.path().join("plugins/demo"), &outside).unwrap();
        symlink(&outside, tmp.path().join("plugins/demo")).unwrap();
        assert!(FileGraphBundleLoader::new(tmp.path()).load().is_err());
    }
}
