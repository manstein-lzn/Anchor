#![forbid(unsafe_code)]

mod archive;
mod filesystem;
mod validation;

pub use validation::{ElfIdentity, ElfRuntime};

use anchor_graph_host::FileGraphBundleLoader;
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
pub const GOOSE_SHA256: &str = "bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340";

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
    for tool in tools {
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
    add_snapshots(&mut entries, &snapshots[request.tools.len() + 2..])?;
    let admitted = FileGraphBundleLoader::new(root.join("bundle"))
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
Plugin services are not bundled. No Python installation is needed by the builder;\n\
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
Web assets, when supplied, live in web and require the Host's Web-root configuration.\n";

#[cfg(test)]
mod tests;
