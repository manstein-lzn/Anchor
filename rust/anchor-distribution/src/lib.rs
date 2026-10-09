#![forbid(unsafe_code)]

mod archive;
mod filesystem;
mod validation;

pub use validation::{ElfIdentity, ElfRuntime};

use anchor_graph_host::{
    FileGraphBundleLoader, FilePluginCatalog, GraphBundleManifest, PluginCatalog,
};
use filesystem::{Output, Snapshot};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const GOOSE_VERSION: &str = "1.53.0";
pub const GOOSE_SHA256: &str = "71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBinary {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRequest {
    pub host: PathBuf,
    pub goose: PathBuf,
    pub bundle: PathBuf,
    pub web: Option<PathBuf>,
    pub tools: Vec<ToolBinary>,
    pub scholarly: Option<PathBuf>,
    pub docmost_tools: Option<PathBuf>,
    pub wecom_tools: Option<PathBuf>,
    pub wecom_gateway: Option<PathBuf>,
    pub output: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableIdentity {
    pub path: String,
    pub sha256: String,
    pub elf: ElfIdentity,
    pub runtime: ElfRuntime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInventory {
    pub id: String,
    pub digest: String,
    pub resources: Vec<String>,
    pub mcp_servers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleInventory {
    pub graph_sha256: String,
    pub manifest_sha256: String,
    pub plugins: Vec<PluginInventory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInventory {
    pub format: u32,
    pub builder_version: String,
    pub goose_version: String,
    pub goose_sha256: String,
    pub platform: ElfIdentity,
    pub bundle: BundleInventory,
    pub executables: Vec<ExecutableIdentity>,
    pub environment_references: Vec<String>,
    pub files: Vec<ResourceFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageReport {
    pub output: PathBuf,
    pub sha256: String,
    pub inventory: PackageInventory,
}

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("invalid distribution input: {0}")]
    Invalid(String),
    #[error("distribution input changed during packaging: {0}")]
    Drift(PathBuf),
    #[error("Graph bundle failed shared admission")]
    Bundle,
    #[error("output already exists; refusing overwrite")]
    AlreadyExists,
    #[error("archive was published but directory durability is uncertain: {0}")]
    PublicationUncertain(PathBuf),
    #[error("distribution filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("distribution JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<rustix::io::Errno> for PackageError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}

pub type Result<T> = std::result::Result<T, PackageError>;

pub fn build_package(request: &PackageRequest) -> Result<PackageReport> {
    prepare(request, GOOSE_SHA256)?.publish()
}

struct PreparedPackage {
    output: Output,
    snapshots: Vec<Snapshot>,
    _staging: tempfile::TempDir,
    archive: File,
    sha256: String,
    inventory: PackageInventory,
}

impl PreparedPackage {
    fn publish(mut self) -> Result<PackageReport> {
        for snapshot in &self.snapshots {
            snapshot.verify()?;
        }
        self.output.publish(&mut self.archive)?;
        Ok(PackageReport {
            output: self.output.path,
            sha256: self.sha256,
            inventory: self.inventory,
        })
    }
}

fn prepare(request: &PackageRequest, goose_sha256: &str) -> Result<PreparedPackage> {
    validation::version(env!("CARGO_PKG_VERSION"))?;
    let mut tools = request.tools.iter().collect::<Vec<_>>();
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    let mut names = BTreeSet::new();
    for tool in &tools {
        validation::tool_name(&tool.name)?;
        if !names.insert(&tool.name) {
            return Err(PackageError::Invalid("duplicate tool name".into()));
        }
    }
    let output = Output::new(&request.output)?;
    let staging = tempfile::Builder::new()
        .prefix("anchor-distribution-")
        .tempdir()?;
    let root = staging.path();
    fs::create_dir(root.join("bin"))?;
    let mut snapshots = vec![
        Snapshot::file(&request.host, root, "bin/anchor-runner-host")?,
        Snapshot::file(&request.goose, root, "bin/goose")?,
    ];
    for tool in &tools {
        snapshots.push(Snapshot::file(
            &tool.path,
            root,
            &format!("bin/{}", tool.name),
        )?);
    }
    let mut entries = BTreeMap::from([(String::new(), None), ("bin".into(), None)]);
    add_snapshots(&mut entries, &snapshots)?;
    let goose_resource = resource(&entries, "bin/goose")?;
    if goose_resource.sha256 != goose_sha256 {
        return Err(PackageError::Invalid(
            "Goose does not match the pinned v1.53.0 x86_64 musl executable".into(),
        ));
    }
    let platform = validation::elf(&root.join("bin/goose"))?;
    snapshots.push(Snapshot::tree(&request.bundle, root, "bundle")?);
    if let Some(web) = &request.web {
        snapshots.push(Snapshot::tree(web, root, "web")?);
    }
    add_snapshots(&mut entries, &snapshots[tools.len() + 2..])?;
    let bundle_root = root.join("bundle");
    // A channel entrypoint is itself part of the closed Plugin bundle. Read
    // the manifest first so the reviewed channel binary can be staged before
    // shared admission checks that entrypoint existence.
    let manifest: GraphBundleManifest = serde_json::from_slice(
        &fs::read(bundle_root.join("manifest.json")).map_err(|_| PackageError::Bundle)?,
    )
    .map_err(|_| PackageError::Bundle)?;
    let plugin_ids = manifest
        .plugins
        .iter()
        .map(|plugin| plugin.id.clone())
        .collect::<Vec<_>>();
    let mut first_party_added = false;
    for (plugin_id, server_name, binary_name, source, option_name) in [
        (
            "docmost",
            "attachments",
            "anchor-docmost-tools",
            request.docmost_tools.as_deref(),
            "--docmost-tools",
        ),
        (
            "wecom",
            "wecom",
            "anchor-wecom-tools",
            request.wecom_tools.as_deref(),
            "--wecom-tools",
        ),
    ] {
        let referenced = plugin_ids.iter().any(|id| id == plugin_id);
        match (referenced, source) {
            (true, Some(source)) => {
                let mut files = BinaryInstallContext {
                    root,
                    bundle_root: &bundle_root,
                    entries: &mut entries,
                    snapshots: &mut snapshots,
                };
                install_plugin_binary(source, plugin_id, server_name, binary_name, &mut files)?;
                first_party_added = true;
            }
            (true, None) => {
                return Err(PackageError::Invalid(format!(
                    "{plugin_id} Plugin requires {option_name} with the {binary_name} ELF"
                )));
            }
            (false, Some(_)) => {
                return Err(PackageError::Invalid(format!(
                    "{option_name} requires the {plugin_id} Plugin in the Graph bundle"
                )));
            }
            (false, None) => {}
        }
    }
    let wecom_referenced = plugin_ids.iter().any(|id| id == "wecom");
    match (wecom_referenced, request.wecom_gateway.as_deref()) {
        (true, Some(source)) => {
            let mut files = BinaryInstallContext {
                root,
                bundle_root: &bundle_root,
                entries: &mut entries,
                snapshots: &mut snapshots,
            };
            install_channel_binary(
                source,
                "wecom",
                "wecom",
                "websocket",
                "bin/anchor-wecom-gateway",
                "anchor-wecom-gateway",
                &mut files,
            )?;
            first_party_added = true;
        }
        (true, None) => {
            return Err(PackageError::Invalid(
                "wecom Plugin requires --wecom-gateway with the anchor-wecom-gateway ELF".into(),
            ));
        }
        (false, Some(_)) => {
            return Err(PackageError::Invalid(
                "--wecom-gateway requires the wecom Plugin in the Graph bundle".into(),
            ));
        }
        (false, None) => {}
    }
    if first_party_added {
        refresh_bundle_manifest(&bundle_root, &plugin_ids, &mut entries)?;
    }
    let admitted = FileGraphBundleLoader::new(&bundle_root)
        .load()
        .map_err(|_| PackageError::Bundle)?;
    let scholarly_plugin = admitted
        .plugins
        .iter()
        .any(|plugin| plugin.id == "academic-research");
    match (scholarly_plugin, request.scholarly.as_ref()) {
        (true, Some(binary)) => {
            let plugin_ids = admitted
                .plugins
                .iter()
                .map(|plugin| plugin.id.clone())
                .collect::<Vec<_>>();
            install_scholarly_binary(
                binary,
                &plugin_ids,
                &bundle_root,
                &mut entries,
                &mut snapshots,
            )?;
        }
        (true, None) => {
            return Err(PackageError::Invalid(
                "academic-research Plugin requires --scholarly with the anchor-scholarly ELF"
                    .into(),
            ));
        }
        (false, Some(_)) => {
            return Err(PackageError::Invalid(
                "--scholarly requires the academic-research Plugin in the Graph bundle".into(),
            ));
        }
        (false, None) => {}
    }
    let admitted = FileGraphBundleLoader::new(&bundle_root)
        .load()
        .map_err(|_| PackageError::Bundle)?;
    let bundle = BundleInventory {
        graph_sha256: resource(&entries, "bundle/graph.json")?.sha256.clone(),
        manifest_sha256: resource(&entries, "bundle/manifest.json")?.sha256.clone(),
        plugins: admitted
            .plugins
            .into_iter()
            .map(|plugin| PluginInventory {
                id: plugin.id,
                digest: plugin.digest,
                resources: plugin.resources,
                mcp_servers: plugin.mcp_servers,
            })
            .collect(),
    };
    validate_bundle_closure(&entries, &bundle.plugins)?;
    let mut executables = Vec::new();
    let mut environment_references = BTreeSet::new();
    for file in entries.values().flatten() {
        if file.mode == 0o755 {
            let elf = validation::elf(&root.join(&file.path))?;
            if elf != platform {
                return Err(PackageError::Invalid(
                    "all executable ELF platforms must match the pinned Goose platform".into(),
                ));
            }
            executables.push(ExecutableIdentity {
                path: file.path.clone(),
                sha256: file.sha256.clone(),
                elf,
                runtime: validation::elf_runtime(&root.join(&file.path))?,
            });
        } else if file.path.ends_with(".json") {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(root.join(&file.path))?)?;
            collect_environment(&value, &mut environment_references);
        }
    }
    generated_file(root, "README.md", RUNTIME_README.as_bytes(), &mut entries)?;
    let inventory = PackageInventory {
        format: 1,
        builder_version: env!("CARGO_PKG_VERSION").into(),
        goose_version: GOOSE_VERSION.into(),
        goose_sha256: goose_sha256.into(),
        platform,
        bundle,
        executables,
        environment_references: environment_references.into_iter().collect(),
        files: entries.values().flatten().cloned().collect(),
    };
    let mut manifest = serde_json::to_vec_pretty(&inventory)?;
    manifest.push(b'\n');
    generated_file(root, "runtime-manifest.json", &manifest, &mut entries)?;
    let mut archive = archive::build(root, &entries)?;
    let sha256 = archive::sha256(&mut archive)?;
    Ok(PreparedPackage {
        output,
        snapshots,
        _staging: staging,
        archive,
        sha256,
        inventory,
    })
}

fn install_scholarly_binary(
    binary: &Path,
    plugin_ids: &[String],
    bundle_root: &Path,
    entries: &mut BTreeMap<String, Option<ResourceFile>>,
    snapshots: &mut Vec<Snapshot>,
) -> Result<()> {
    let catalog = FilePluginCatalog::new(bundle_root);
    let servers = catalog
        .mcp_servers("academic-research", false)
        .map_err(|_| PackageError::Bundle)?;
    let server = servers
        .iter()
        .find(|server| server.name == "scholarly")
        .ok_or_else(|| {
            PackageError::Invalid(
                "academic-research Plugin must declare its scholarly MCP server".into(),
            )
        })?;
    if server.config["command"] != "bin/anchor-scholarly"
        || server.config["args"] != serde_json::json!(["mcp"])
    {
        return Err(PackageError::Invalid(
            "academic-research scholarly MCP must launch bin/anchor-scholarly mcp".into(),
        ));
    }

    let relative = "bundle/plugins/academic-research/bin/anchor-scholarly";
    if entries.contains_key(relative) {
        return Err(PackageError::Invalid(
            "academic-research bundle already contains bin/anchor-scholarly".into(),
        ));
    }
    let directory = "bundle/plugins/academic-research/bin";
    if !entries.contains_key(directory) {
        fs::create_dir(bundle_root.join("plugins/academic-research/bin"))?;
        fs::set_permissions(
            bundle_root.join("plugins/academic-research/bin"),
            fs::Permissions::from_mode(0o755),
        )?;
        entries.insert(directory.into(), None);
    } else if entries[directory].is_some() {
        return Err(PackageError::Invalid(
            "academic-research Plugin bin path is not a directory".into(),
        ));
    }

    let snapshot = Snapshot::file(binary, bundle_root.parent().unwrap(), relative)?;
    let destination = bundle_root.join("plugins/academic-research/bin/anchor-scholarly");
    validation::elf(&destination)?;
    add_snapshots(entries, std::slice::from_ref(&snapshot))?;
    snapshots.push(snapshot);

    let bindings = catalog
        .resolve(plugin_ids)
        .map_err(|_| PackageError::Bundle)?;
    let plugins = bindings
        .into_iter()
        .map(|binding| {
            serde_json::json!({
                "id": binding.id,
                "digest": binding.digest,
                "resources": binding.resources,
                "mcp_servers": binding.mcp_servers
            })
        })
        .collect::<Vec<_>>();
    let mut bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "format": 1,
        "graph": "graph.json",
        "plugins": plugins
    }))?;
    bytes.push(b'\n');
    fs::write(bundle_root.join("manifest.json"), &bytes)?;
    fs::set_permissions(
        bundle_root.join("manifest.json"),
        fs::Permissions::from_mode(0o644),
    )?;
    let manifest_resource = entries
        .get_mut("bundle/manifest.json")
        .and_then(Option::as_mut)
        .ok_or_else(|| PackageError::Invalid("bundle manifest resource is missing".into()))?;
    manifest_resource.sha256 = format!("{:x}", Sha256::digest(&bytes));
    manifest_resource.size = bytes.len() as u64;
    Ok(())
}

struct BinaryInstallContext<'a> {
    root: &'a Path,
    bundle_root: &'a Path,
    entries: &'a mut BTreeMap<String, Option<ResourceFile>>,
    snapshots: &'a mut Vec<Snapshot>,
}

fn install_plugin_binary(
    binary: &Path,
    plugin_id: &str,
    server_name: &str,
    binary_name: &str,
    files: &mut BinaryInstallContext<'_>,
) -> Result<()> {
    let catalog = FilePluginCatalog::new(files.bundle_root);
    let servers = catalog
        .mcp_servers(plugin_id, false)
        .map_err(|_| PackageError::Bundle)?;
    let server = servers
        .iter()
        .find(|server| server.name == server_name)
        .ok_or_else(|| {
            PackageError::Invalid(format!(
                "{plugin_id} Plugin must declare its {server_name} MCP server"
            ))
        })?;
    let expected_command = format!("bin/{binary_name}");
    if server.config["command"] != expected_command
        || server.config["args"] != serde_json::json!([])
    {
        return Err(PackageError::Invalid(format!(
            "{plugin_id} {server_name} MCP must launch {expected_command} without arguments"
        )));
    }

    install_bundle_binary(binary, plugin_id, binary_name, files)
}

fn install_channel_binary(
    binary: &Path,
    plugin_id: &str,
    platform: &str,
    transport: &str,
    entrypoint: &str,
    sdk: &str,
    files: &mut BinaryInstallContext<'_>,
) -> Result<()> {
    let channel: serde_json::Value = serde_json::from_slice(
        &fs::read(
            files
                .bundle_root
                .join("plugins")
                .join(plugin_id)
                .join("channel.json"),
        )
        .map_err(|_| PackageError::Bundle)?,
    )
    .map_err(|_| PackageError::Bundle)?;
    if channel["platform"] != platform
        || channel["transport"] != transport
        || channel["entrypoint"] != entrypoint
        || channel["sdk"] != sdk
    {
        return Err(PackageError::Invalid(format!(
            "{plugin_id} {platform} channel must use {transport} entrypoint {entrypoint} ({sdk})"
        )));
    }

    let binary_name = entrypoint
        .strip_prefix("bin/")
        .ok_or_else(|| PackageError::Invalid("channel binary must be under bin/".into()))?;
    install_bundle_binary(binary, plugin_id, binary_name, files)?;
    FilePluginCatalog::new(files.bundle_root)
        .channels(plugin_id)
        .map_err(|_| PackageError::Bundle)?;
    Ok(())
}

fn install_bundle_binary(
    binary: &Path,
    plugin_id: &str,
    binary_name: &str,
    files: &mut BinaryInstallContext<'_>,
) -> Result<()> {
    let relative = format!("bundle/plugins/{plugin_id}/bin/{binary_name}");
    if files.entries.contains_key(&relative) {
        return Err(PackageError::Invalid(format!(
            "{plugin_id} bundle already contains bin/{binary_name}"
        )));
    }
    let directory = format!("bundle/plugins/{plugin_id}/bin");
    match files.entries.get(&directory) {
        Some(None) => {}
        Some(Some(_)) => {
            return Err(PackageError::Invalid(format!(
                "{plugin_id} Plugin bin path is not a directory"
            )));
        }
        None => {
            let path = files
                .bundle_root
                .join("plugins")
                .join(plugin_id)
                .join("bin");
            fs::create_dir(path)?;
            fs::set_permissions(
                files
                    .bundle_root
                    .join("plugins")
                    .join(plugin_id)
                    .join("bin"),
                fs::Permissions::from_mode(0o755),
            )?;
            files.entries.insert(directory, None);
        }
    }

    let snapshot = Snapshot::file(binary, files.root, &relative)?;
    validation::elf(&files.root.join(&relative))?;
    add_snapshots(files.entries, std::slice::from_ref(&snapshot))?;
    files.snapshots.push(snapshot);
    Ok(())
}

fn refresh_bundle_manifest(
    bundle_root: &Path,
    plugin_ids: &[String],
    entries: &mut BTreeMap<String, Option<ResourceFile>>,
) -> Result<()> {
    let bindings = FilePluginCatalog::new(bundle_root)
        .resolve(plugin_ids)
        .map_err(|_| PackageError::Bundle)?;
    let plugins = bindings
        .into_iter()
        .map(|binding| {
            serde_json::json!({
                "id": binding.id,
                "digest": binding.digest,
                "resources": binding.resources,
                "mcp_servers": binding.mcp_servers
            })
        })
        .collect::<Vec<_>>();
    let mut bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "format": 1,
        "graph": "graph.json",
        "plugins": plugins
    }))?;
    bytes.push(b'\n');
    fs::write(bundle_root.join("manifest.json"), &bytes)?;
    fs::set_permissions(
        bundle_root.join("manifest.json"),
        fs::Permissions::from_mode(0o644),
    )?;
    let manifest = entries
        .get_mut("bundle/manifest.json")
        .and_then(Option::as_mut)
        .ok_or_else(|| PackageError::Invalid("bundle manifest resource is missing".into()))?;
    manifest.sha256 = format!("{:x}", Sha256::digest(&bytes));
    manifest.size = bytes.len() as u64;
    Ok(())
}

fn add_snapshots(
    entries: &mut BTreeMap<String, Option<ResourceFile>>,
    snapshots: &[Snapshot],
) -> Result<()> {
    for snapshot in snapshots {
        for path in snapshot.directories() {
            if entries.insert(path.clone(), None).is_some() {
                return Err(PackageError::Invalid("duplicate archive directory".into()));
            }
        }
        for file in snapshot.resources() {
            if entries
                .insert(file.path.clone(), Some(file.clone()))
                .is_some()
            {
                return Err(PackageError::Invalid("duplicate archive resource".into()));
            }
        }
    }
    Ok(())
}

fn resource<'resource>(
    entries: &'resource BTreeMap<String, Option<ResourceFile>>,
    path: &str,
) -> Result<&'resource ResourceFile> {
    entries
        .get(path)
        .and_then(Option::as_ref)
        .ok_or_else(|| PackageError::Invalid(format!("required resource is missing: {path}")))
}

fn validate_bundle_closure(
    entries: &BTreeMap<String, Option<ResourceFile>>,
    plugins: &[PluginInventory],
) -> Result<()> {
    let mut declared = BTreeSet::from([
        "bundle/graph.json".to_owned(),
        "bundle/manifest.json".to_owned(),
    ]);
    for plugin in plugins {
        for resource in &plugin.resources {
            declared.insert(format!("bundle/plugins/{}/{resource}", plugin.id));
        }
    }
    for file in declared.clone() {
        let mut path = file.as_str();
        while let Some((parent, _)) = path.rsplit_once('/') {
            declared.insert(parent.to_owned());
            path = parent;
        }
    }
    let actual = entries
        .keys()
        .filter(|path| path.as_str() == "bundle" || path.starts_with("bundle/"))
        .cloned()
        .collect::<BTreeSet<_>>();
    if actual != declared {
        return Err(PackageError::Invalid(
            "Graph bundle must contain exactly declared files and their parent directories".into(),
        ));
    }
    Ok(())
}

fn generated_file(
    root: &Path,
    path: &str,
    bytes: &[u8],
    entries: &mut BTreeMap<String, Option<ResourceFile>>,
) -> Result<()> {
    fs::write(root.join(path), bytes)?;
    fs::set_permissions(root.join(path), fs::Permissions::from_mode(0o644))?;
    let file = ResourceFile {
        path: path.into(),
        sha256: format!("{:x}", Sha256::digest(bytes)),
        size: bytes.len() as u64,
        mode: 0o644,
    };
    if entries.insert(path.into(), Some(file)).is_some() {
        return Err(PackageError::Invalid("duplicate generated resource".into()));
    }
    Ok(())
}

fn collect_environment(value: &serde_json::Value, references: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for value in object.values() {
                collect_environment(value, references);
            }
        }
        serde_json::Value::Array(array) => {
            for value in array {
                collect_environment(value, references);
            }
        }
        serde_json::Value::String(value) => references.extend(validation::placeholders(value)),
        _ => {}
    }
}

const RUNTIME_README: &str = "# Anchor Goose Runtime\n\n\
This archive contains the supplied Anchor Host, pinned Goose v1.53.0, one\n\
admitted format-1 Graph bundle, and only explicitly supplied tools and Web assets.\n\
The packaging process does not execute binaries, call providers, or grant permissions.\n\n\
runtime-manifest.json records ELF identities and SHA256 values for all payload\n\
files except itself. Verify the archive SHA256 against the build report before\n\
extracting. The manifest is not a signature or an attestation of Host features.\n\n\
Deployment requires a compatible Linux machine, Bubblewrap, Git, required system\n\
libraries and operator-authorized commands. Dynamic ELF dependencies and external\n\
Plugin services are not bundled. The builder does not install interpreters;\n\
third-party Plugin compatibility and runtime dependency closure need separate acceptance.\n\n\
Configure ANCHOR_GOOSE_BINARY to the absolute bin/goose path and\n\
ANCHOR_GOOSE_BINARY_SHA256 to the manifest's goose_sha256. Configure\n\
ANCHOR_RUNNER_BUNDLE_ROOT to the absolute bundle directory. Keep\n\
ANCHOR_RUNNER_STATE_ROOT, ANCHOR_RUNNER_WORKSPACE_ROOT and HOME outside this archive\n\
in private operator-managed directories. Supply model configuration, API keys,\n\
Plugin environment references and sandbox authorization through the deployment\n\
environment. Do not put credentials or mutable runtime data inside the bundle.\n\n\
Run bin/anchor-runner-host serve with an operator-selected listen address and API\n\
authorization, or use the Host's framed standalone start_bundle interface.\n\
Adding tools to bin does not automatically authorize them or rewrite Plugin commands.\n\
When the Graph uses academic-research, --scholarly embeds its executable in that\n\
Plugin and updates its frozen resource summary; the Plugin does not use host PATH.\n\
Web assets, when supplied, live in web and require the Host's Web-root configuration.\n";

#[cfg(test)]
mod tests;
