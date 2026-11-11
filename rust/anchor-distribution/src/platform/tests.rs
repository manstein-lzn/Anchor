use super::*;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{os::unix::fs::PermissionsExt, path::Path};

pub(crate) struct Fixture {
    pub root: tempfile::TempDir,
    pub request: PlatformPackageRequest,
    pub digest: String,
}

impl Fixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        // A real static x86_64 executable with an exit(0) entrypoint. No
        // compilation, model or development installation is needed by fixtures.
        let mut bytes = vec![0_u8; 120];
        bytes[0..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&0x400078_u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&1_u32.to_le_bytes());
        bytes[68..72].copy_from_slice(&5_u32.to_le_bytes());
        bytes[80..88].copy_from_slice(&0x400000_u64.to_le_bytes());
        bytes[88..96].copy_from_slice(&0x400000_u64.to_le_bytes());
        bytes.extend_from_slice(&[0xb8, 0x3c, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05]);
        let size = bytes.len() as u64;
        bytes[96..104].copy_from_slice(&size.to_le_bytes());
        bytes[104..112].copy_from_slice(&size.to_le_bytes());
        bytes[112..120].copy_from_slice(&0x1000_u64.to_le_bytes());
        let executable = root.path().join("fixture-elf");
        fs::write(&executable, &bytes).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let channel_binary = |name: &str| {
            let path = root.path().join(name);
            let mut specific = bytes.clone();
            specific.extend_from_slice(name.as_bytes());
            fs::write(&path, specific).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let web = root.path().join("web");
        fs::create_dir(&web).unwrap();
        fs::write(web.join("index.html"), "<html>Anchor</html>\n").unwrap();
        fs::write(web.join("app.js"), "document.title = 'Anchor';\n").unwrap();
        let request = PlatformPackageRequest {
            host: executable.clone(),
            goose: executable.clone(),
            net_relay: executable.clone(),
            sandbox_network: executable.clone(),
            platform_launcher: executable.clone(),
            network_tools: executable.clone(),
            git_tools: executable.clone(),
            wecom_tools: channel_binary("anchor-wecom-tools"),
            feishu_gateway: channel_binary("anchor-feishu-gateway"),
            wecom_gateway: channel_binary("anchor-wecom-gateway"),
            uv: executable.clone(),
            rustup_init: executable.clone(),
            pixi: executable,
            web,
            output: root.path().join("platform.tar.gz"),
        };
        Self {
            root,
            request,
            digest,
        }
    }

    pub fn package(&self) -> Result<PlatformPackageReport> {
        prepare(
            &self.request,
            &self.digest,
            &self.digest,
            &self.digest,
            &self.digest,
        )?
        .publish()
    }

    pub fn extract(&self, destination: &Path) {
        tar::Archive::new(GzDecoder::new(File::open(&self.request.output).unwrap()))
            .unpack(destination)
            .unwrap();
    }
}

#[test]
fn platform_has_no_business_graph_and_exact_bootstrap_and_official_plugin_inventory() {
    let mut fixture = Fixture::new();
    let first = fixture.package().unwrap();
    fixture.request.output = fixture.root.path().join("second.tar.gz");
    let second = fixture.package().unwrap();
    assert_eq!(first.sha256, second.sha256);
    assert_eq!(first.inventory, second.inventory);
    assert_eq!(first.inventory.format, 3);
    assert_eq!(first.inventory.kind, "platform");
    assert_eq!(first.inventory.bootstrap.uv.version, UV_VERSION);
    assert_eq!(first.inventory.bootstrap.uv.source, UV_SOURCE);
    assert_eq!(first.inventory.bootstrap.uv.sha256, fixture.digest);
    assert_eq!(
        first.inventory.bootstrap.rustup_init.path,
        "bin/rustup-init"
    );
    assert_eq!(
        first.inventory.bootstrap.pixi,
        BootstrapTool {
            path: "bin/pixi".into(),
            version: PIXI_VERSION.into(),
            source: PIXI_SOURCE.into(),
            sha256: fixture.digest.clone(),
        }
    );
    assert_eq!(PIXI_VERSION, "0.83.0");
    assert_eq!(
        PIXI_SOURCE,
        "https://github.com/prefix-dev/pixi/releases/download/v0.83.0/pixi-x86_64-unknown-linux-musl"
    );
    assert_eq!(
        PIXI_SHA256,
        "50571451e96f4a82cf675cd83baaf66fff9b718f0aaf153900b9faf4c29bd401"
    );
    assert_eq!(first.inventory.executables.len(), 13);
    assert_eq!(
        first
            .inventory
            .plugins
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "network",
            "graph-tasks",
            "feishu",
            "wecom",
            "git",
            "anchor-steward",
        ]),
    );
    let starter: serde_json::Value =
        serde_json::from_str(include_str!("../../../../examples/starter.json")).unwrap();
    for plugin in &first.inventory.plugins {
        assert!(
            starter["plugins"][&plugin.id].is_array(),
            "official Plugin {} is missing from source onboarding",
            plugin.id
        );
    }
    let network = first
        .inventory
        .plugins
        .iter()
        .find(|plugin| plugin.id == "network")
        .unwrap();
    assert!(
        network
            .resources
            .contains(&"bin/anchor-network-tools".into())
    );
    assert_eq!(network.mcp_servers, ["http"]);
    let git = first
        .inventory
        .plugins
        .iter()
        .find(|plugin| plugin.id == "git")
        .unwrap();
    assert!(git.resources.contains(&"bin/anchor-git-tools".into()));
    assert_eq!(git.mcp_servers, ["git", "host", "https", "ssh"]);
    let dev = first
        .inventory
        .plugins
        .iter()
        .find(|plugin| plugin.id == "anchor-steward")
        .unwrap();
    assert!(dev.mcp_servers.is_empty());
    assert_eq!(dev.resources.len(), 8);
    let tasks = first
        .inventory
        .plugins
        .iter()
        .find(|plugin| plugin.id == "graph-tasks")
        .unwrap();
    assert!(tasks.mcp_servers.is_empty());
    assert_eq!(tasks.resources.len(), 2);
    for (id, binaries) in [
        (
            "feishu",
            [("bin/anchor-feishu-gateway", &fixture.request.feishu_gateway)].as_slice(),
        ),
        (
            "wecom",
            [
                ("bin/anchor-wecom-tools", &fixture.request.wecom_tools),
                ("bin/anchor-wecom-gateway", &fixture.request.wecom_gateway),
            ]
            .as_slice(),
        ),
    ] {
        let plugin = first
            .inventory
            .plugins
            .iter()
            .find(|plugin| plugin.id == id)
            .unwrap();
        assert!(plugin.resources.contains(&"channel.json".into()));
        for (relative, source) in binaries {
            assert!(plugin.resources.contains(&(*relative).to_owned()));
            let path = format!("plugins/{id}/{relative}");
            let executable = first
                .inventory
                .executables
                .iter()
                .find(|executable| executable.path == path)
                .unwrap();
            assert_eq!(
                executable.sha256,
                format!("{:x}", Sha256::digest(fs::read(source).unwrap())),
                "{path}",
            );
        }
    }
    assert!(
        first
            .inventory
            .files
            .iter()
            .all(|file| !file.path.starts_with("bundle/"))
    );
    let extracted = tempfile::tempdir().unwrap();
    fixture.extract(extracted.path());
    let runtime = extracted.path().join("anchor-runtime");
    assert!(!runtime.join("bundle").exists());
    assert!(runtime.join("web/index.html").is_file());
    assert_eq!(
        release_plugin_inventory(&runtime).unwrap(),
        first.inventory.plugins
    );
    for plugin in release_plugins() {
        for (relative, expected) in plugin.files {
            let path = format!("plugins/{}/{relative}", plugin.id);
            assert_eq!(fs::read(runtime.join(&path)).unwrap(), *expected, "{path}");
        }
    }
    let expected_paths = release_plugin_resources()
        .map(|(path, _)| path)
        .collect::<BTreeSet<_>>();
    let actual_paths = first
        .inventory
        .files
        .iter()
        .filter(|file| file.path.starts_with("plugins/"))
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_paths, expected_paths);
    for file in &first.inventory.files {
        let bytes = fs::read(runtime.join(&file.path)).unwrap();
        assert_eq!(
            file.sha256,
            format!("{:x}", Sha256::digest(&bytes)),
            "{}",
            file.path
        );
    }
    assert!(
        !serde_json::to_string(&first.inventory)
            .unwrap()
            .contains(fixture.root.path().to_str().unwrap())
    );
}

#[test]
fn public_platform_api_cannot_override_fixed_goose_or_bootstrap_pins() {
    let fixture = Fixture::new();
    assert!(
        build_platform_package(&fixture.request)
            .unwrap_err()
            .to_string()
            .contains("Goose v1.53.0")
    );
    assert!(
        prepare(
            &fixture.request,
            &fixture.digest,
            UV_SHA256,
            &fixture.digest,
            &fixture.digest
        )
        .err()
        .unwrap()
        .to_string()
        .contains("uv 0.11.6")
    );
    assert!(
        prepare(
            &fixture.request,
            &fixture.digest,
            &fixture.digest,
            RUSTUP_INIT_SHA256,
            &fixture.digest
        )
        .err()
        .unwrap()
        .to_string()
        .contains("rustup-init 1.29.0")
    );
    assert!(
        prepare(
            &fixture.request,
            &fixture.digest,
            &fixture.digest,
            &fixture.digest,
            PIXI_SHA256
        )
        .err()
        .unwrap()
        .to_string()
        .contains("pixi 0.83.0")
    );
    assert!(!fixture.request.output.exists());
}

#[test]
fn platform_rejects_missing_web_sources_state_credentials_and_unsafe_inputs() {
    for path in [
        "index.html",
        "private.env",
        "state/file.txt",
        "app.js.map",
        "sources.py",
    ] {
        let fixture = Fixture::new();
        if path == "index.html" {
            fs::remove_file(fixture.request.web.join(path)).unwrap();
        } else {
            let destination = fixture.request.web.join(path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::write(destination, "unreviewed\n").unwrap();
        }
        assert!(fixture.package().is_err(), "{path}");
        assert!(!fixture.request.output.exists());
    }
    let mut fixture = Fixture::new();
    let alias = fixture.root.path().join("unsafe-uv");
    std::os::unix::fs::symlink(&fixture.request.uv, &alias).unwrap();
    fixture.request.uv = alias;
    assert!(fixture.package().is_err());
}

#[test]
fn platform_rechecks_sources_and_refuses_archive_overwrite() {
    let fixture = Fixture::new();
    let prepared = prepare(
        &fixture.request,
        &fixture.digest,
        &fixture.digest,
        &fixture.digest,
        &fixture.digest,
    )
    .unwrap();
    fs::write(fixture.request.web.join("added.css"), "body{}\n").unwrap();
    assert!(prepared.publish().is_err());
    assert!(!fixture.request.output.exists());
    let fixture = Fixture::new();
    fixture.package().unwrap();
    let original = fs::read(&fixture.request.output).unwrap();
    assert!(matches!(
        fixture.package(),
        Err(PackageError::AlreadyExists)
    ));
    assert_eq!(fs::read(&fixture.request.output).unwrap(), original);
}

#[test]
fn bootstrap_must_be_static_even_when_private_test_pins_match() {
    for name in ["uv", "pixi"] {
        let mut fixture = Fixture::new();
        let dynamic = fixture.root.path().join("dynamic-bootstrap");
        fs::copy("/usr/bin/true", &dynamic).unwrap();
        let digest = format!("{:x}", Sha256::digest(fs::read(&dynamic).unwrap()));
        let mut uv_digest = fixture.digest.as_str();
        let mut pixi_digest = fixture.digest.as_str();
        if name == "uv" {
            fixture.request.uv = dynamic;
            uv_digest = &digest;
        } else {
            fixture.request.pixi = dynamic;
            pixi_digest = &digest;
        }
        assert!(
            prepare(
                &fixture.request,
                &fixture.digest,
                uv_digest,
                &fixture.digest,
                pixi_digest
            )
            .err()
            .unwrap()
            .to_string()
            .contains("static musl"),
            "{name}"
        );
        assert!(!fixture.request.output.exists());
    }
}

#[test]
fn embedded_official_resources_cover_the_source_public_files_exactly() {
    fn files(root: &Path, path: &Path, found: &mut BTreeSet<String>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                files(root, &entry.path(), found);
            } else {
                assert!(kind.is_file(), "official sources must not contain links");
                found.insert(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .into(),
                );
            }
        }
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for plugin in release_plugins() {
        let mut actual = BTreeSet::new();
        files(&repo, &repo.join("plugins").join(plugin.id), &mut actual);
        let expected = plugin
            .files
            .iter()
            .map(|(path, _)| format!("plugins/{}/{path}", plugin.id))
            .collect();
        assert_eq!(
            actual, expected,
            "update the explicit public resource inventory for {}",
            plugin.id
        );
    }
}

#[test]
fn platform_rejects_missing_or_non_elf_plugin_tools_and_gateways() {
    for name in [
        "network-tools",
        "git-tools",
        "wecom-tools",
        "feishu-gateway",
        "wecom-gateway",
    ] {
        for missing in [true, false] {
            let mut fixture = Fixture::new();
            let path = fixture.root.path().join("invalid-binary");
            if !missing {
                fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            match name {
                "network-tools" => fixture.request.network_tools = path,
                "git-tools" => fixture.request.git_tools = path,
                "wecom-tools" => fixture.request.wecom_tools = path,
                "feishu-gateway" => fixture.request.feishu_gateway = path,
                "wecom-gateway" => fixture.request.wecom_gateway = path,
                _ => unreachable!(),
            }
            assert!(fixture.package().is_err(), "{name}, missing={missing}");
            assert!(!fixture.request.output.exists());
        }
    }
}
