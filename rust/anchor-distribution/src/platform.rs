//! Full-platform delivery without a placeholder business Graph. The standalone
//! Graph package keeps its separate, strict admission contract.
use crate::{
    ExecutableIdentity, GOOSE_SHA256, GOOSE_VERSION, PackageError, PluginInventory, ResourceFile,
    Result, add_snapshots, archive, collect_environment, filesystem::Output, filesystem::Snapshot,
    generated_file, resource, validation,
};
use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    path::PathBuf,
};

pub const UV_VERSION: &str = "0.11.6";
pub const UV_SHA256: &str = "61c5aaf37153f7b2b4c8d11ff5174fa30e930dc3dc2825f54593a2dd319eb8af";
pub const UV_SOURCE: &str =
    "https://github.com/astral-sh/uv/releases/download/0.11.6/uv-x86_64-unknown-linux-musl.tar.gz";
pub const RUSTUP_INIT_VERSION: &str = "1.29.0";
pub const RUSTUP_INIT_SHA256: &str =
    "9cd3fda5fd293890e36ab271af6a786ee22084b5f6c2b83fd8323cec6f0992c1";
pub const RUSTUP_INIT_SOURCE: &str =
    "https://static.rust-lang.org/rustup/archive/1.29.0/x86_64-unknown-linux-musl/rustup-init";
pub const PIXI_VERSION: &str = "0.83.0";
pub const PIXI_SHA256: &str = "50571451e96f4a82cf675cd83baaf66fff9b718f0aaf153900b9faf4c29bd401";
pub const PIXI_SOURCE: &str =
    "https://github.com/prefix-dev/pixi/releases/download/v0.83.0/pixi-x86_64-unknown-linux-musl";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformPackageRequest {
    pub host: PathBuf,
    pub goose: PathBuf,
    pub net_relay: PathBuf,
    pub sandbox_network: PathBuf,
    pub platform_launcher: PathBuf,
    pub web: PathBuf,
    pub network_tools: PathBuf,
    pub uv: PathBuf,
    pub rustup_init: PathBuf,
    pub pixi: PathBuf,
    pub output: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapTool {
    pub path: String,
    pub version: String,
    pub sha256: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapInventory {
    pub uv: BootstrapTool,
    pub rustup_init: BootstrapTool,
    pub pixi: BootstrapTool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformPackageInventory {
    pub format: u32,
    pub kind: String,
    pub builder_version: String,
    pub goose_version: String,
    pub goose_sha256: String,
    pub platform: crate::ElfIdentity,
    pub bootstrap: BootstrapInventory,
    pub plugins: Vec<PluginInventory>,
    pub executables: Vec<ExecutableIdentity>,
    pub environment_references: Vec<String>,
    pub files: Vec<ResourceFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformPackageReport {
    pub output: PathBuf,
    pub sha256: String,
    pub inventory: PlatformPackageInventory,
}

pub fn build_platform_package(request: &PlatformPackageRequest) -> Result<PlatformPackageReport> {
    prepare(
        request,
        GOOSE_SHA256,
        UV_SHA256,
        RUSTUP_INIT_SHA256,
        PIXI_SHA256,
    )?
    .publish()
}

struct PreparedPlatform {
    output: Output,
    snapshots: Vec<Snapshot>,
    _staging: tempfile::TempDir,
    archive: File,
    sha256: String,
    inventory: PlatformPackageInventory,
}

impl PreparedPlatform {
    fn publish(mut self) -> Result<PlatformPackageReport> {
        for snapshot in &self.snapshots {
            snapshot.verify()?;
        }
        self.output.publish(&mut self.archive)?;
        Ok(PlatformPackageReport {
            output: self.output.path,
            sha256: self.sha256,
            inventory: self.inventory,
        })
    }
}

fn prepare(
    request: &PlatformPackageRequest,
    goose_sha256: &str,
    uv_sha256: &str,
    rustup_sha256: &str,
    pixi_sha256: &str,
) -> Result<PreparedPlatform> {
    validation::version(env!("CARGO_PKG_VERSION"))?;
    let output = Output::new(&request.output)?;
    let staging = tempfile::Builder::new()
        .prefix("anchor-platform-distribution-")
        .tempdir()?;
    let root = staging.path();
    for directory in [
        "bin",
        "plugins/network/bin",
        "plugins/network/skills/network",
    ] {
        fs::create_dir_all(root.join(directory))?;
    }
    let mut entries = BTreeMap::from([(String::new(), None)]);
    for directory in [
        "bin",
        "plugins",
        "plugins/network",
        "plugins/network/bin",
        "plugins/network/skills",
        "plugins/network/skills/network",
    ] {
        entries.insert(directory.into(), None);
    }
    let mut snapshots = Vec::new();
    for (source, path) in [
        (&request.host, "bin/anchor-runner-host"),
        (&request.goose, "bin/goose"),
        (&request.net_relay, "bin/anchor-net-relay"),
        (&request.sandbox_network, "bin/anchor-sandbox-network"),
        (&request.platform_launcher, "bin/anchor-platform"),
        (&request.uv, "bin/uv"),
        (&request.rustup_init, "bin/rustup-init"),
        (&request.pixi, "bin/pixi"),
        (
            &request.network_tools,
            "plugins/network/bin/anchor-network-tools",
        ),
    ] {
        snapshots.push(Snapshot::file(source, root, path)?);
    }
    add_snapshots(&mut entries, &snapshots)?;
    for (path, digest, identity) in [
        ("bin/goose", goose_sha256, "Goose v1.53.0"),
        ("bin/uv", uv_sha256, "uv 0.11.6"),
        ("bin/rustup-init", rustup_sha256, "rustup-init 1.29.0"),
        ("bin/pixi", pixi_sha256, "pixi 0.83.0"),
    ] {
        if resource(&entries, path)?.sha256 != digest {
            return Err(PackageError::Invalid(format!(
                "{identity} does not match the pinned x86_64 musl executable"
            )));
        }
    }
    for (path, bytes) in [
        (
            "plugins/network/plugin.json",
            include_bytes!("../../../plugins/network/plugin.json").as_slice(),
        ),
        (
            "plugins/network/skills/network/SKILL.md",
            include_bytes!("../../../plugins/network/skills/network/SKILL.md").as_slice(),
        ),
    ] {
        generated_file(root, path, bytes, &mut entries)?;
        validation::resource_content(&root.join(path), path)?;
    }
    let plugins = FilePluginCatalog::new(root)
        .resolve(&["network".into()])
        .map_err(|error| {
            PackageError::Invalid(format!(
                "native network Plugin failed shared admission: {error}"
            ))
        })?
        .into_iter()
        .map(|binding| PluginInventory {
            id: binding.id,
            digest: binding.digest,
            resources: binding.resources,
            mcp_servers: binding.mcp_servers,
        })
        .collect();
    let web = Snapshot::tree(&request.web, root, "web")?;
    add_snapshots(&mut entries, std::slice::from_ref(&web))?;
    snapshots.push(web);
    if !entries.get("web/index.html").is_some_and(Option::is_some) {
        return Err(PackageError::Invalid(
            "a platform package requires the compiled WebUI web/index.html".into(),
        ));
    }
    let platform = validation::elf(&root.join("bin/goose"))?;
    let mut executables = Vec::new();
    let mut environment_references = BTreeSet::new();
    for file in entries.values().flatten() {
        if file.mode == 0o755 {
            let path = root.join(&file.path);
            let elf = validation::elf(&path)?;
            if elf != platform {
                return Err(PackageError::Invalid(
                    "all executable ELF platforms must match the pinned Goose platform".into(),
                ));
            }
            let runtime = validation::elf_runtime(&path)?;
            if matches!(
                file.path.as_str(),
                "bin/goose" | "bin/uv" | "bin/rustup-init" | "bin/pixi"
            ) && (runtime.interpreter.is_some() || !runtime.needed.is_empty())
            {
                // The delivery bootstrap must not need an interpreter or the
                // development toolchain it exists to prepare.
                return Err(PackageError::Invalid(
                    "Goose, uv, rustup-init and pixi must be static musl executables".into(),
                ));
            }
            executables.push(ExecutableIdentity {
                path: file.path.clone(),
                sha256: file.sha256.clone(),
                elf,
                runtime,
            });
        } else if file.path.ends_with(".json") {
            collect_environment(
                &serde_json::from_slice(&fs::read(root.join(&file.path))?)?,
                &mut environment_references,
            );
        }
    }
    generated_file(root, "README.md", PLATFORM_README.as_bytes(), &mut entries)?;
    let inventory = PlatformPackageInventory {
        format: 3,
        kind: "platform".into(),
        builder_version: env!("CARGO_PKG_VERSION").into(),
        goose_version: GOOSE_VERSION.into(),
        goose_sha256: goose_sha256.into(),
        platform,
        bootstrap: BootstrapInventory {
            uv: BootstrapTool {
                path: "bin/uv".into(),
                version: UV_VERSION.into(),
                sha256: uv_sha256.into(),
                source: UV_SOURCE.into(),
            },
            rustup_init: BootstrapTool {
                path: "bin/rustup-init".into(),
                version: RUSTUP_INIT_VERSION.into(),
                sha256: rustup_sha256.into(),
                source: RUSTUP_INIT_SOURCE.into(),
            },
            pixi: BootstrapTool {
                path: "bin/pixi".into(),
                version: PIXI_VERSION.into(),
                sha256: pixi_sha256.into(),
                source: PIXI_SOURCE.into(),
            },
        },
        plugins,
        executables,
        environment_references: environment_references.into_iter().collect(),
        files: entries.values().flatten().cloned().collect(),
    };
    let mut manifest = serde_json::to_vec_pretty(&inventory)?;
    manifest.push(b'\n');
    generated_file(root, "runtime-manifest.json", &manifest, &mut entries)?;
    let mut archive = archive::build(root, &entries)?;
    let sha256 = archive::sha256(&mut archive)?;
    Ok(PreparedPlatform {
        output,
        snapshots,
        _staging: staging,
        archive,
        sha256,
        inventory,
    })
}

const PLATFORM_README: &str = "# Anchor platform\n\n\
This archive contains the Anchor Host, fixed Goose, sandbox network relay,\n\
compiled WebUI, native network Plugin and pinned uv/Rustup/pixi bootstrap tools.\n\
It contains no business Graph, Python interpreter, Rust toolchain, user model\n\
configuration, credentials or mutable runtime state. Platform startup uses the\n\
same Host and Runtime Kernel as standalone Graph packages.\n\n\
Verify the archive SHA256 against its trusted build report before extraction.\n\
runtime-manifest.json records identities and file hashes, not a signature.\n\
Keep this release read-only and mutable roots outside it. Bootstrap identity\n\
is discovered from the Host's release; Pilot prepares Python/Rust on demand\n\
into the Host's managed data root. User model configuration and network consent\n\
are supplied outside this archive. Shipping the network Plugin does not grant\n\
task network permission.\n\n\
The supported machine must have Bubblewrap, slirp4netns, Git, shell, CA certificates and\n\
runtime libraries named in the executable inventory. Rust compilation also\n\
needs an authorized system linker. The distribution builder does not install\n\
these system prerequisites or execute its supplied binaries.\n";

#[cfg(test)]
pub(crate) mod tests;
