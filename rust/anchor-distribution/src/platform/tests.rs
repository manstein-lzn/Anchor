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
fn platform_has_no_business_graph_and_exact_bootstrap_and_network_inventory() {
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
    assert_eq!(first.inventory.executables.len(), 9);
    assert_eq!(first.inventory.plugins.len(), 1);
    assert_eq!(first.inventory.plugins[0].id, "network");
    assert!(
        first.inventory.plugins[0]
            .resources
            .contains(&"bin/anchor-network-tools".into())
    );
    assert_eq!(first.inventory.plugins[0].mcp_servers, ["http"]);
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
        FilePluginCatalog::new(&runtime)
            .resolve(&["network".into()])
            .unwrap()[0]
            .digest,
        first.inventory.plugins[0].digest
    );
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
