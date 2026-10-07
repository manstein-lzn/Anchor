use super::*;
use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use flate2::read::GzDecoder;
use serde_json::{Value, json};
use std::{
    io::Read,
    os::unix::{fs::symlink, net::UnixListener},
};

struct Fixture {
    root: tempfile::TempDir,
    request: PackageRequest,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let host = root.path().join("host");
        let goose = root.path().join("goose");
        fs::copy("/usr/bin/true", &host).unwrap();
        fs::copy("/usr/bin/true", &goose).unwrap();
        let bundle = root.path().join("bundle");
        fs::create_dir(&bundle).unwrap();
        let request = PackageRequest {
            host,
            goose,
            bundle,
            web: None,
            tools: Vec::new(),
            output: root.path().join("runtime.tar.gz"),
        };
        let fixture = Self { root, request };
        fixture.write_bundle(&[]);
        fixture
    }

    fn write_bundle(&self, plugins: &[&str]) {
        let graph = if plugins.is_empty() {
            json!({
                "objective":"distribution fixture", "entry":"work", "agents":{},
                "ops":{"work":{"run":"true"}},
                "nodes":[{"id":"work","op":"work","plugins":[]}], "edges":[]
            })
        } else {
            json!({
                "objective":"distribution fixture", "entry":"work",
                "agents":{"worker":{"model":"fixture","instructions":"inspect resources"}},
                "ops":{}, "nodes":[{"id":"work","agent":"worker","plugins":plugins}],
                "edges":[]
            })
        };
        fs::write(self.request.bundle.join("graph.json"), graph.to_string()).unwrap();
        let ids = plugins
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<Vec<_>>();
        let bindings = FilePluginCatalog::new(&self.request.bundle)
            .resolve(&ids)
            .unwrap();
        let plugins = bindings
            .into_iter()
            .map(|binding| {
                json!({
                    "id":binding.id,"digest":binding.digest,"resources":binding.resources,
                    "mcp_servers":binding.mcp_servers
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            self.request.bundle.join("manifest.json"),
            json!({
                "format":1,"graph":"graph.json","plugins":plugins
            })
            .to_string(),
        )
        .unwrap();
    }

    fn plugin(&self) -> PathBuf {
        let directory = self.request.bundle.join("plugins/demo");
        fs::create_dir_all(directory.join("skills/inspect")).unwrap();
        fs::write(
            directory.join("plugin.json"),
            json!({"name":"Demo"}).to_string(),
        )
        .unwrap();
        fs::write(
            directory.join("skills/inspect/SKILL.md"),
            "Read the supplied evidence.\n",
        )
        .unwrap();
        self.write_bundle(&["demo"]);
        directory
    }

    fn prepared(&self) -> Result<PreparedPackage> {
        prepare(&self.request, &digest(&self.request.goose))
    }

    fn package(&self) -> Result<PackageReport> {
        self.prepared()?.publish()
    }
}

fn digest(path: &Path) -> String {
    archive::sha256(&mut File::open(path).unwrap()).unwrap()
}

#[test]
fn public_api_requires_the_fixed_goose_identity() {
    let fixture = Fixture::new();
    let error = build_package(&fixture.request).unwrap_err();
    assert!(error.to_string().contains("pinned v1.53.0"));
    assert_eq!(GOOSE_VERSION, "1.53.0");
    assert_eq!(GOOSE_SHA256.len(), 64);
    assert!(!fixture.request.output.exists());
}

#[test]
fn deterministic_archive_has_exact_layout_normalized_headers_and_verified_inventory() {
    let mut fixture = Fixture::new();
    fixture.plugin();
    let web = fixture.root.path().join("web");
    fs::create_dir(&web).unwrap();
    fs::write(
        web.join("index.html"),
        "<html><script src='app.js'></script></html>",
    )
    .unwrap();
    fs::write(web.join("app.js"), "document.title = 'Fixture';\n").unwrap();
    fixture.request.web = Some(web);
    fixture.request.tools = vec![
        ToolBinary {
            name: "zeta".into(),
            path: fixture.request.host.clone(),
        },
        ToolBinary {
            name: "alpha".into(),
            path: fixture.request.host.clone(),
        },
    ];
    let first = fixture.package().unwrap();
    let first_bytes = fs::read(&first.output).unwrap();
    assert_eq!(&first_bytes[4..8], &[0, 0, 0, 0]);
    assert_eq!(first_bytes[9], 255);
    fixture.request.output = fixture.root.path().join("other.tar.gz");
    fixture.request.tools.reverse();
    let second = fixture.package().unwrap();
    assert_eq!(first.inventory, second.inventory);
    assert_eq!(first.sha256, second.sha256);
    assert_eq!(first_bytes, fs::read(second.output).unwrap());
    assert_eq!(digest(&first.output), first.sha256);
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(&first.output).unwrap()));
    let mut paths = Vec::new();
    let mut manifest = None;
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry
            .path()
            .unwrap()
            .to_str()
            .unwrap()
            .trim_end_matches('/')
            .to_owned();
        assert_eq!(entry.header().uid().unwrap(), 0);
        assert_eq!(entry.header().gid().unwrap(), 0);
        assert_eq!(entry.header().mtime().unwrap(), 0);
        assert_eq!(entry.header().username().unwrap(), Some(""));
        assert_eq!(entry.header().groupname().unwrap(), Some(""));
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if entry.header().entry_type().is_dir() {
            assert_eq!(entry.header().mode().unwrap(), 0o755);
        } else {
            assert!(entry.header().entry_type().is_file());
            let relative = path.strip_prefix("anchor-runtime/").unwrap();
            if relative == "runtime-manifest.json" {
                manifest = Some(serde_json::from_slice::<PackageInventory>(&bytes).unwrap());
            } else {
                let file = first
                    .inventory
                    .files
                    .iter()
                    .find(|file| file.path == relative)
                    .unwrap();
                assert_eq!(format!("{:x}", Sha256::digest(&bytes)), file.sha256);
                assert_eq!(bytes.len() as u64, file.size);
                assert_eq!(entry.header().mode().unwrap(), file.mode);
            }
        }
        paths.push(path);
    }
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);
    assert_eq!(manifest.unwrap(), first.inventory);
    assert_eq!(
        paths,
        [
            "anchor-runtime",
            "anchor-runtime/README.md",
            "anchor-runtime/bin",
            "anchor-runtime/bin/alpha",
            "anchor-runtime/bin/anchor-runner-host",
            "anchor-runtime/bin/goose",
            "anchor-runtime/bin/zeta",
            "anchor-runtime/bundle",
            "anchor-runtime/bundle/graph.json",
            "anchor-runtime/bundle/manifest.json",
            "anchor-runtime/bundle/plugins",
            "anchor-runtime/bundle/plugins/demo",
            "anchor-runtime/bundle/plugins/demo/plugin.json",
            "anchor-runtime/bundle/plugins/demo/skills",
            "anchor-runtime/bundle/plugins/demo/skills/inspect",
            "anchor-runtime/bundle/plugins/demo/skills/inspect/SKILL.md",
            "anchor-runtime/runtime-manifest.json",
            "anchor-runtime/web",
            "anchor-runtime/web/app.js",
            "anchor-runtime/web/index.html",
        ]
    );
    let extracted = tempfile::tempdir().unwrap();
    tar::Archive::new(GzDecoder::new(File::open(&first.output).unwrap()))
        .unpack(extracted.path())
        .unwrap();
    FileGraphBundleLoader::new(extracted.path().join("anchor-runtime/bundle"))
        .load()
        .unwrap();
    assert!(
        !serde_json::to_string(&first.inventory)
            .unwrap()
            .contains(fixture.root.path().to_str().unwrap())
    );
}

#[test]
fn native_plugin_elf_keeps_execute_mode_and_environment_references_are_not_expanded() {
    let fixture = Fixture::new();
    let plugin = fixture.plugin();
    fs::create_dir(plugin.join("bin")).unwrap();
    fs::copy(&fixture.request.host, plugin.join("bin/helper")).unwrap();
    fs::write(
        plugin.join(".mcp.json"),
        json!({"mcpServers":{
            "native":{"command":"bin/helper","args":[],"cwd":".",
                "env":{"API_KEY":"${ANCHOR_DISTRIBUTION_FIXTURE_KEY}"}},
            "remote":{"url":"https://example.com/mcp", "headers":{
                "Authorization":"Bearer ${ANCHOR_DISTRIBUTION_FIXTURE_KEY}"}}
        }})
        .to_string(),
    )
    .unwrap();
    fixture.write_bundle(&["demo"]);
    let report = fixture.package().unwrap();
    let file = report
        .inventory
        .files
        .iter()
        .find(|file| file.path.ends_with("/bin/helper"))
        .unwrap();
    assert_eq!(file.mode, 0o755);
    assert_eq!(
        report.inventory.environment_references,
        ["ANCHOR_DISTRIBUTION_FIXTURE_KEY"]
    );
    assert_eq!(report.inventory.executables.len(), 3);
    let host = report
        .inventory
        .executables
        .iter()
        .find(|executable| executable.path == "bin/anchor-runner-host")
        .unwrap();
    assert!(host.runtime.interpreter.is_some());
    assert!(!host.runtime.needed.is_empty());
    let extracted = tempfile::tempdir().unwrap();
    tar::Archive::new(GzDecoder::new(File::open(report.output).unwrap()))
        .unpack(extracted.path())
        .unwrap();
    let helper = extracted
        .path()
        .join("anchor-runtime/bundle/plugins/demo/bin/helper");
    assert_eq!(
        fs::metadata(&helper).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::read(helper).unwrap(),
        fs::read(fixture.request.host).unwrap()
    );
}

#[test]
fn partial_archive_read_failure_cleans_candidate_and_never_publishes() {
    struct InterruptedArchive {
        wrote_prefix: bool,
    }

    impl Read for InterruptedArchive {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.wrote_prefix {
                return Err(std::io::Error::other("fixture archive read failed"));
            }
            let prefix = b"incomplete archive prefix";
            let length = buffer.len().min(prefix.len());
            buffer[..length].copy_from_slice(&prefix[..length]);
            self.wrote_prefix = true;
            Ok(length)
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.tar.gz");
    let output = Output::new(&path).unwrap();
    let mut archive = InterruptedArchive {
        wrote_prefix: false,
    };
    assert!(matches!(
        output.publish(&mut archive),
        Err(PackageError::Io(_))
    ));
    assert!(!path.exists());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);

    fs::write(&path, "concurrent winner").unwrap();
    let mut archive = InterruptedArchive {
        wrote_prefix: false,
    };
    assert!(output.publish(&mut archive).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"concurrent winner");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn refuses_existing_outputs_and_preserves_a_concurrent_publish_winner() {
    let fixture = Fixture::new();
    fs::write(&fixture.request.output, "existing").unwrap();
    assert!(matches!(
        fixture.prepared(),
        Err(PackageError::AlreadyExists)
    ));
    assert_eq!(fs::read(&fixture.request.output).unwrap(), b"existing");
    fs::remove_file(&fixture.request.output).unwrap();
    let package = fixture.prepared().unwrap();
    fs::write(&fixture.request.output, "winner").unwrap();
    assert!(matches!(
        package.publish(),
        Err(PackageError::AlreadyExists)
    ));
    assert_eq!(fs::read(&fixture.request.output).unwrap(), b"winner");
    assert!(fs::read_dir(fixture.root.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp")
    }));
}

#[test]
fn input_mutation_replacement_permissions_and_tree_changes_never_publish() {
    for change in ["bytes", "inode", "mode", "addition", "remove", "hardlink"] {
        let fixture = Fixture::new();
        let package = fixture.prepared().unwrap();
        match change {
            "bytes" => fs::write(&fixture.request.host, b"mutated").unwrap(),
            "inode" => {
                let replacement = fixture.root.path().join("replacement");
                fs::copy(&fixture.request.host, &replacement).unwrap();
                fs::rename(replacement, &fixture.request.host).unwrap();
            }
            "mode" => fs::set_permissions(&fixture.request.host, fs::Permissions::from_mode(0o644))
                .unwrap(),
            "addition" => fs::write(fixture.request.bundle.join("note.txt"), "new").unwrap(),
            "remove" => fs::remove_file(fixture.request.bundle.join("graph.json")).unwrap(),
            "hardlink" => {
                fs::hard_link(&fixture.request.host, fixture.root.path().join("alias")).unwrap()
            }
            _ => unreachable!(),
        }
        assert!(package.publish().is_err(), "{change}");
        assert!(!fixture.request.output.exists(), "{change}");
    }
}

#[test]
fn replaced_output_parent_is_not_used_for_publication() {
    let mut fixture = Fixture::new();
    let parent = fixture.root.path().join("output");
    fs::create_dir(&parent).unwrap();
    fixture.request.output = parent.join("runtime.tar.gz");
    let package = fixture.prepared().unwrap();
    let moved = fixture.root.path().join("moved");
    fs::rename(&parent, &moved).unwrap();
    fs::create_dir(&parent).unwrap();
    fs::write(parent.join("sentinel"), "preserved").unwrap();
    assert!(package.publish().is_err());
    assert_eq!(fs::read_dir(&moved).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"preserved");
}

#[test]
fn rejects_undeclared_bundle_files_plugin_files_and_empty_directories() {
    for extra in [
        "note.txt",
        "plugins/demo/unlisted.txt",
        "plugins/demo/empty",
        "plugins/unknown",
    ] {
        let fixture = Fixture::new();
        fixture.plugin();
        let path = fixture.request.bundle.join(extra);
        if extra.ends_with(".txt") {
            fs::write(path, "unlisted").unwrap();
        } else {
            fs::create_dir_all(path).unwrap();
        }
        assert!(fixture.package().is_err(), "{extra}");
        assert!(!fixture.request.output.exists());
    }
}

#[test]
fn rejects_graph_plugin_digest_and_manifest_admission_errors() {
    for change in [
        "digest",
        "graph",
        "unknown_manifest_field",
        "resource_bytes",
        "plugin_set",
    ] {
        let fixture = Fixture::new();
        let plugin = fixture.plugin();
        let path = fixture.request.bundle.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        match change {
            "digest" => manifest["plugins"][0]["digest"] = json!("invalid"),
            "graph" => manifest["graph"] = json!("../outside.json"),
            "unknown_manifest_field" => manifest["unknown"] = json!(true),
            "resource_bytes" => {
                fs::write(plugin.join("skills/inspect/SKILL.md"), "changed").unwrap()
            }
            "plugin_set" => manifest["plugins"] = json!([]),
            _ => unreachable!(),
        }
        fs::write(path, manifest.to_string()).unwrap();
        assert!(fixture.package().is_err(), "{change}");
        assert!(!fixture.request.output.exists());
    }
}

#[test]
fn rejects_symlink_inputs_roots_ancestors_outputs_and_resources() {
    for target in ["host", "bundle", "ancestor", "output", "plugin", "resource"] {
        let mut fixture = Fixture::new();
        match target {
            "host" => {
                let original = fixture.root.path().join("original");
                fs::rename(&fixture.request.host, &original).unwrap();
                symlink(original, &fixture.request.host).unwrap();
            }
            "bundle" => {
                let original = fixture.root.path().join("original-bundle");
                fs::rename(&fixture.request.bundle, &original).unwrap();
                symlink(original, &fixture.request.bundle).unwrap();
            }
            "ancestor" => {
                let alias = fixture.root.path().join("alias");
                symlink(fixture.root.path(), &alias).unwrap();
                fixture.request.host = alias.join("host");
            }
            "output" => {
                symlink(fixture.root.path().join("missing"), &fixture.request.output).unwrap()
            }
            "plugin" => {
                let plugin = fixture.plugin();
                let original = fixture.root.path().join("original-plugin");
                fs::rename(&plugin, &original).unwrap();
                symlink(original, plugin).unwrap();
            }
            "resource" => {
                let plugin = fixture.plugin();
                symlink(&fixture.request.host, plugin.join("unsafe.txt")).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(fixture.package().is_err(), "{target}");
        if target != "output" {
            assert!(!fixture.request.output.exists());
        }
    }
}

#[test]
fn binary_and_resource_hardlinks_require_independent_copies() {
    for binary in [true, false] {
        let fixture = Fixture::new();
        let source = if binary {
            fixture.request.host.clone()
        } else {
            fixture.plugin().join("skills/inspect/SKILL.md")
        };
        let alias = fixture.root.path().join("alias");
        fs::hard_link(&source, &alias).unwrap();
        assert!(
            fixture
                .prepared()
                .err()
                .unwrap()
                .to_string()
                .contains("non-hardlinked")
        );
        fs::remove_file(alias).unwrap();
        assert!(fixture.package().is_ok());
    }
}

#[test]
fn rejects_special_files_and_non_executable_or_non_elf_binaries() {
    for change in [
        "socket",
        "mode",
        "script",
        "malformed_elf",
        "foreign_platform",
    ] {
        let fixture = Fixture::new();
        let socket;
        match change {
            "socket" => {
                socket =
                    Some(UnixListener::bind(fixture.request.bundle.join("input.txt")).unwrap());
            }
            "mode" => {
                socket = None;
                fs::set_permissions(&fixture.request.host, fs::Permissions::from_mode(0o644))
                    .unwrap();
            }
            "script" => {
                socket = None;
                fs::write(&fixture.request.host, "#!/bin/sh\nexit 0\n").unwrap();
            }
            "malformed_elf" => {
                socket = None;
                fs::write(&fixture.request.host, b"\x7fELF").unwrap();
            }
            "foreign_platform" => {
                socket = None;
                let mut bytes = fs::read(&fixture.request.host).unwrap();
                bytes[18..20].copy_from_slice(&183_u16.to_le_bytes());
                fs::write(&fixture.request.host, bytes).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(fixture.package().is_err(), "{change}");
        assert!(!fixture.request.output.exists());
        drop(socket);
    }
}

#[test]
fn rejects_unsafe_duplicate_reserved_and_source_tool_names() {
    for name in [
        "",
        "../escape",
        "goose",
        "anchor-runner-host",
        ".env",
        "tool.py",
        "unsafe name",
    ] {
        let mut fixture = Fixture::new();
        fixture.request.tools.push(ToolBinary {
            name: name.into(),
            path: fixture.request.host.clone(),
        });
        assert!(fixture.package().is_err(), "{name}");
        assert!(!fixture.request.output.exists());
    }
    let mut fixture = Fixture::new();
    fixture.request.tools = vec![
        ToolBinary {
            name: "duplicate".into(),
            path: fixture.request.host.clone()
        };
        2
    ];
    assert!(fixture.package().is_err());
}

#[test]
fn rejects_parent_traversal_and_invalid_output_extensions() {
    let mut fixture = Fixture::new();
    fixture.request.host = fixture.root.path().join("bundle/../host");
    assert!(fixture.package().is_err());
    fixture.request.host = fixture.root.path().join("host");
    fixture.request.output = fixture.root.path().join("runtime.zip");
    assert!(fixture.package().is_err());
    fixture.request.output = fixture.root.path().join("absent/runtime.tar.gz");
    assert!(fixture.package().is_err());
}

#[test]
fn rejects_source_credential_state_and_non_utf8_paths() {
    use std::os::unix::ffi::OsStringExt;
    for name in [
        ".env",
        ".git",
        "src",
        "state",
        "Cargo.toml",
        "source.rs",
        "source.py",
        "source.pyc",
        "source.sh",
        "data.sqlite3",
        "history.jsonl",
        "private-key.txt",
    ] {
        let fixture = Fixture::new();
        let plugin = fixture.plugin();
        fs::write(plugin.join(name), "unsafe").unwrap();
        assert!(fixture.package().is_err(), "{name}");
        assert!(!fixture.request.output.exists());
    }
    let fixture = Fixture::new();
    let plugin = fixture.plugin();
    fs::write(
        plugin.join(std::ffi::OsString::from_vec(vec![0xff])),
        "invalid name",
    )
    .unwrap();
    assert!(fixture.package().is_err());
}

#[test]
fn compiled_javascript_is_only_allowed_in_explicit_web_assets() {
    let mut fixture = Fixture::new();
    let plugin = fixture.plugin();
    fs::write(plugin.join("unsafe.js"), "console.log('fixture');").unwrap();
    fixture.write_bundle(&["demo"]);
    assert!(fixture.package().is_err());
    fs::remove_file(plugin.join("unsafe.js")).unwrap();
    fixture.write_bundle(&["demo"]);
    let web = fixture.root.path().join("web");
    fs::create_dir(&web).unwrap();
    fs::write(web.join("app.js"), "console.log('fixture');").unwrap();
    fixture.request.web = Some(web.clone());
    assert!(fixture.package().is_ok());
    fs::write(web.join("app.js.map"), "source map").unwrap();
    fixture.request.output = fixture.root.path().join("with-source-map.tar.gz");
    assert!(fixture.package().is_err());
}

#[test]
fn structured_credentials_private_keys_and_url_userinfo_are_rejected_without_values_in_errors() {
    let marker = "distribution-fixture-secret-not-production";
    for (name, content) in [
        (
            "auth.json",
            format!(r#"{{"headers":{{"Authorization":"Bearer {marker}"}}}}"#),
        ),
        ("auth.JSON", format!(r#"{{"token":"{marker}"}}"#)),
        (
            "settings.yaml",
            format!("service:\n  'api_key': '{marker}'\n"),
        ),
        (
            "settings.yml",
            format!("service:\n  token: >-\n    {marker}\n"),
        ),
        (
            "settings.toml",
            format!("[service]\n'secret' = '{marker}'\n"),
        ),
        ("auth.txt", format!("export API_KEY='{marker}'\n")),
        (
            "key.txt",
            format!("-----BEGIN PRIVATE KEY-----\n{marker}\n"),
        ),
        (
            "url.json",
            format!(r#"{{"url":"https://user:{marker}@example.com/mcp"}}"#),
        ),
    ] {
        let fixture = Fixture::new();
        let plugin = fixture.plugin();
        fs::write(plugin.join(name), content).unwrap();
        let error = fixture.prepared().err().unwrap();
        assert!(!error.to_string().contains(marker), "{name}");
        assert!(!fixture.request.output.exists());
    }
}

#[test]
fn structured_environment_references_remain_deployment_inputs() {
    let fixture = Fixture::new();
    let plugin = fixture.plugin();
    for (name, content) in [
        (
            "settings.yaml",
            "service:\n  api_key: '${UNCONFIGURED_KEY}'\n",
        ),
        (
            "settings.toml",
            "[service]\nsecret = '${UNCONFIGURED_KEY}'\n",
        ),
        ("settings.json", r#"{"token":"${UNCONFIGURED_KEY}"}"#),
    ] {
        fs::write(plugin.join(name), content).unwrap();
    }
    fixture.write_bundle(&["demo"]);
    assert!(fixture.package().is_ok());
}

#[test]
fn long_ustar_paths_fail_before_any_archive_is_published() {
    let fixture = Fixture::new();
    let mut directory = fixture.plugin();
    for _ in 0..4 {
        directory = directory.join("a".repeat(80));
        fs::create_dir(&directory).unwrap();
    }
    fs::write(directory.join("note.txt"), "too deep").unwrap();
    assert!(fixture.package().is_err());
    assert!(!fixture.request.output.exists());
}
