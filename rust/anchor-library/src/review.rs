//! Operator-facing review of a Plugin candidate before activation.
//!
//! A review never publishes anything: the candidate is staged and validated
//! through the same code path an installation uses, and the resulting summary
//! is what a human confirms before the install is allowed to happen. The
//! digest in [`PluginReview`] is therefore the digest the installed Plugin
//! will carry, so a review cannot describe a different tree than the one that
//! ends up in the Library.

use std::{fs::File, io::Read, path::Path};

use anchor_graph_host::{FilePluginCatalog, PluginCatalog, PluginDefinition, PluginDependencies};
use serde::Serialize;

use crate::InstallError;

const ELF_MAGIC: &[u8] = b"\x7fELF";

/// File names that indicate sources a reviewer could read for compiled output.
const SOURCE_MARKERS: &[&str] = &[
    "Cargo.toml",
    "go.mod",
    "Makefile",
    "CMakeLists.txt",
    "build.rs",
    "pyproject.toml",
];

/// Extensions of sources a reviewer could read for compiled output.
const SOURCE_SUFFIXES: &[&str] = &[".rs", ".go", ".c", ".cc", ".cpp", ".h", ".hpp", ".zig"];

/// One declared MCP server, with credential values reduced to names.
///
/// Servers the manifest disables with `enabled: false` never reach this list:
/// the catalog drops them, so a review describes the effective surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerReview {
    pub name: String,
    pub transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// Environment variable names the server reads.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<String>,
    /// Environment variable names whose absence disables the server.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub optional_environment: Vec<String>,
    /// `header=field` pairs of channel identity the Host attests to this server.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub channel_identity: Vec<String>,
}

/// One declared service-level channel entry point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelReview {
    pub plugin: String,
    pub platform: String,
    pub transport: String,
    pub entrypoint: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_environment: Vec<String>,
}

/// Everything a human needs in order to accept or reject a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginReview {
    pub id: String,
    /// Derived from the verified catalog identity, never from a manifest flag.
    pub kind: &'static str,
    /// The digest the installed Plugin will carry.
    pub digest: String,
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// Manifest or directory kinds Anchor validates but does not execute.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unsupported: Vec<String>,
    pub resources: Vec<String>,
    pub resource_bytes: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<ServerReview>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelReview>,
    /// The Plugin's own run-time dependencies. What these resolve to is a
    /// separate, deployment-authorized step; the review shows the declaration.
    #[serde(skip_serializing_if = "PluginDependencies::is_empty")]
    pub dependencies: PluginDependencies,
    /// Files that look like compiled output.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub binaries: Vec<String>,
    /// Readable sources accompanying compiled output, if any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// True when compiled output ships without anything a reviewer can read.
    pub binary_without_source: bool,
    /// Declarations that carry literal values inside the bundle, such as
    /// `server:env.KEY`. Those values are distributed with the Plugin.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inline_values: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) fn summarize(
    id: &str,
    catalog: &FilePluginCatalog,
    definition: &PluginDefinition,
) -> Result<PluginReview, InstallError> {
    let binding = catalog
        .resolve(std::slice::from_ref(&id.to_owned()))
        .map_err(|error| InstallError::InvalidPluginReason(error.to_string()))?
        .into_iter()
        .next()
        .ok_or(InstallError::InvalidPlugin)?;
    let resources = binding.resources;
    let mut resource_bytes = 0_u64;
    let mut binaries = Vec::new();
    let mut sources = Vec::new();
    for relative in &resources {
        let path = definition.directory.join(relative);
        resource_bytes += std::fs::metadata(&path)?.len();
        if is_elf(&path)? {
            binaries.push(relative.clone());
        } else if is_source(relative) {
            sources.push(relative.clone());
        }
    }
    binaries.sort();
    sources.sort();
    let binary_without_source = !binaries.is_empty() && sources.is_empty();

    let mut servers = Vec::new();
    let mut inline_values = Vec::new();
    // The dependency-view token is left as declared here: a review stages the
    // candidate under a temporary root, so any path derived from this catalog
    // would name the staging directory rather than the deployment's view. The
    // approval form expands it against the real Library root.
    for server in &definition.mcp_servers {
        let config = &server.config;
        let command = config.get("command").and_then(serde_json::Value::as_str);
        let transport = config
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(if command.is_some() { "stdio" } else { "http" })
            .to_owned();
        let mut environment = names(config, "env_vars");
        let optional_environment = names(config, "optional_env_vars");
        if let Some(variable) = config
            .get("bearer_token_env_var")
            .and_then(serde_json::Value::as_str)
        {
            environment.push(variable.to_owned());
        }
        environment.extend(
            config
                .get("env_http_headers")
                .and_then(serde_json::Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(_, value)| value.as_str().map(str::to_owned)),
        );
        environment.sort();
        environment.dedup();
        for field in ["env", "headers", "http_headers"] {
            for key in literal_keys(config, field) {
                inline_values.push(format!("{}:{field}.{key}", server.name));
            }
        }
        servers.push(ServerReview {
            name: server.name.clone(),
            transport,
            command: command.map(str::to_owned),
            args: config
                .get("args")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect(),
            cwd: config
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            url: config
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            auth: (config.get("auth").and_then(serde_json::Value::as_str) == Some("oauth")
                || config.get("oauth_resource").is_some())
            .then(|| "oauth".to_owned()),
            environment,
            optional_environment,
            channel_identity: anchor_graph_host::channel_identity_headers(config)
                .into_iter()
                .map(|(header, field)| format!("{header}={field}"))
                .collect(),
        });
    }
    servers.sort_by(|left, right| left.name.cmp(&right.name));
    inline_values.sort();

    let channels = definition
        .channels
        .iter()
        .map(|channel| ChannelReview {
            plugin: channel.plugin.clone(),
            platform: channel.platform.clone(),
            transport: channel.transport.clone(),
            entrypoint: channel.entrypoint.clone(),
            required_environment: channel.required_environment.clone(),
        })
        .collect();

    let mut warnings = Vec::new();
    if binary_without_source {
        warnings.push(
            "compiled binaries ship without readable sources or build files; move sources into the Plugin or accept an unreadable artifact"
                .to_owned(),
        );
    }
    if !inline_values.is_empty() {
        warnings.push(
            "literal MCP values are distributed inside the Plugin; declare credentials through environment references instead"
                .to_owned(),
        );
    }
    if !definition.unsupported.is_empty() {
        warnings.push(format!(
            "manifest declares {} which Anchor validates but does not execute",
            definition.unsupported.join(", ")
        ));
    }

    Ok(PluginReview {
        id: id.to_owned(),
        kind: if definition.system.is_some() {
            "system"
        } else {
            "extension"
        },
        digest: definition.digest.clone(),
        name: definition.name.clone(),
        description: definition.description.clone(),
        skills: definition.skills.clone(),
        unsupported: definition.unsupported.clone(),
        resources,
        resource_bytes,
        servers,
        channels,
        dependencies: definition.dependencies.clone(),
        binaries,
        sources,
        binary_without_source,
        inline_values,
        warnings,
    })
}

fn names(config: &serde_json::Value, field: &str) -> Vec<String> {
    config
        .get(field)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// Keys of `field` that carry a literal non-empty value.
fn literal_keys(config: &serde_json::Value, field: &str) -> Vec<String> {
    config
        .get(field)
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, value)| value.as_str().is_some_and(|text| !text.trim().is_empty()))
        .map(|(key, _)| key.clone())
        .collect()
}

fn is_elf(path: &Path) -> Result<bool, InstallError> {
    let mut file = File::open(path)?;
    let mut magic = [0_u8; 4];
    match file.read_exact(&mut magic) {
        Ok(()) => Ok(magic == ELF_MAGIC),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn is_source(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    SOURCE_MARKERS.contains(&name) || SOURCE_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}
