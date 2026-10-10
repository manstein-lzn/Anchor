use super::*;
use sha2::{Digest, Sha256};
use std::os::unix::fs::{PermissionsExt, symlink};

struct Fixture {
    root: tempfile::TempDir,
    runtime: PathBuf,
    env: BTreeMap<String, String>,
    goose_sha256: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("release/anchor-runtime");
        for directory in ["bin", "bundle", "web"] {
            fs::create_dir_all(runtime.join(directory)).unwrap();
            set_mode(&runtime.join(directory), 0o755);
        }
        set_mode(&runtime, 0o755);
        let binary = static_elf();
        for name in ["anchor-runner-host", "goose"] {
            let path = runtime.join("bin").join(name);
            fs::write(&path, &binary).unwrap();
            set_mode(&path, 0o755);
        }
        fs::write(runtime.join("bundle/graph.json"), "{}").unwrap();
        fs::write(runtime.join("bundle/manifest.json"), "{}").unwrap();
        fs::write(runtime.join("web/index.html"), "<!doctype html>").unwrap();
        for name in ["state", "workspaces", "catalog", "tools"] {
            let path = root.path().join(name);
            fs::create_dir(&path).unwrap();
            set_mode(&path, 0o700);
        }
        for name in ["sh", "git", "bwrap"] {
            let path = root.path().join("tools").join(name);
            fs::write(&path, "fixture: never executed").unwrap();
            set_mode(&path, 0o755);
        }
        let env_file = root.path().join("anchor.env");
        fs::write(&env_file, "not read or sourced by preflight").unwrap();
        set_mode(&env_file, 0o600);
        let goose_sha256 = digest(&runtime.join("bin/goose"));
        let mut env = BTreeMap::new();
        for (name, path) in [
            ("ANCHOR_ENV_FILE", env_file),
            ("ANCHOR_DEPLOYMENT_ROOT", runtime.clone()),
            (
                "ANCHOR_RUNNER_HOST_BINARY",
                runtime.join("bin/anchor-runner-host"),
            ),
            ("ANCHOR_GOOSE_BINARY", runtime.join("bin/goose")),
            ("ANCHOR_RUNNER_BUNDLE_ROOT", runtime.join("bundle")),
            ("ANCHOR_RUNNER_WEB_ROOT", runtime.join("web")),
            ("ANCHOR_RUNNER_STATE_ROOT", root.path().join("state")),
            (
                "ANCHOR_RUNNER_WORKSPACE_ROOT",
                root.path().join("workspaces"),
            ),
            ("ANCHOR_RUNNER_CATALOG_ROOT", root.path().join("catalog")),
            ("PATH", root.path().join("tools")),
        ] {
            env.insert(name.into(), path.to_str().unwrap().into());
        }
        for (name, configured) in [
            ("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh,git"),
            ("ANCHOR_RUNNER_LISTEN", "127.0.0.1:8077"),
            ("ANCHOR_GOOSE_BINARY_SHA256", goose_sha256.as_str()),
            ("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK", "1"),
            ("ANCHOR_MODEL_URL", "http://127.0.0.1:12345/v1"),
            (
                "ANCHOR_MODEL_API_KEY",
                "fixture-provider-secret-never-output",
            ),
            ("ANCHOR_MODEL_NAME", "fixture-model"),
            ("ANCHOR_MODEL_WIRE_API", "chat"),
            ("ANCHOR_MODEL_ALIASES", "{}"),
            ("ANCHOR_API_KEYS", "[]"),
        ] {
            env.insert(name.into(), configured.into());
        }
        let fixture = Self {
            root,
            runtime,
            env,
            goose_sha256,
        };
        fixture.write_manifest();
        fixture
    }

    fn write_manifest(&self) {
        let platform = json!({"architecture": "X86_64", "bits": 64, "little_endian": true});
        let mut files = Vec::new();
        for relative in [
            "bin/anchor-runner-host",
            "bin/goose",
            "bundle/graph.json",
            "bundle/manifest.json",
            "web/index.html",
        ] {
            let path = self.runtime.join(relative);
            set_mode(
                &path,
                if relative.starts_with("bin/") {
                    0o755
                } else {
                    0o644
                },
            );
            files.push(json!({
                "path": relative, "sha256": digest(&path),
                "size": path.metadata().unwrap().len(), "mode": path.metadata().unwrap().mode() & 0o7777,
            }));
        }
        let mut executables = Vec::new();
        for relative in ["bin/anchor-runner-host", "bin/goose"] {
            let path = self.runtime.join(relative);
            executables.push(json!({
                "path": relative, "sha256": digest(&path), "elf": platform,
                "runtime": elf::inspect(&path).unwrap(),
            }));
        }
        let manifest = json!({
            "format": 1, "goose_version": GOOSE_VERSION, "goose_sha256": self.goose_sha256,
            "platform": platform, "files": files, "executables": executables,
        });
        self.save_manifest(&manifest);
    }

    fn save_manifest(&self, manifest: &Value) {
        let path = self.runtime.join("runtime-manifest.json");
        fs::write(&path, serde_json::to_vec(manifest).unwrap()).unwrap();
        set_mode(&path, 0o644);
    }

    fn edit_manifest(&self, edit: impl FnOnce(&mut Value)) {
        let mut manifest =
            serde_json::from_slice(&fs::read(self.runtime.join("runtime-manifest.json")).unwrap())
                .unwrap();
        edit(&mut manifest);
        self.save_manifest(&manifest);
    }

    fn set(&mut self, name: &str, configured: &str) {
        self.env.insert(name.into(), configured.into());
    }

    fn remove(&mut self, name: &str) {
        self.env.remove(name);
    }

    fn path(&self, name: &str) -> PathBuf {
        PathBuf::from(&self.env[name])
    }

    fn check(&self) -> Result<Value, String> {
        check_with_pin(&self.env, &self.goose_sha256)
    }

    fn rejects(&self, expected: &str) {
        let error = self.check().unwrap_err();
        assert!(
            error.contains(expected),
            "expected {expected:?}, got {error:?}"
        );
        assert!(!error.contains(&self.env["ANCHOR_MODEL_API_KEY"]));
        assert!(!error.contains(&self.env["ANCHOR_MODEL_URL"]));
    }
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn digest(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn static_elf() -> Vec<u8> {
    let mut bytes = vec![0; 121];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4..7].copy_from_slice(&[2, 1, 1]);
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
    bytes[96..104].copy_from_slice(&121_u64.to_le_bytes());
    bytes[104..112].copy_from_slice(&121_u64.to_le_bytes());
    bytes[112..120].copy_from_slice(&0x1000_u64.to_le_bytes());
    bytes[120] = 0xc3;
    bytes
}

fn elf_with_interpreter(interpreter: &str) -> Vec<u8> {
    let mut bytes = static_elf();
    let size = 177 + interpreter.len() + 1;
    bytes.resize(size, 0);
    bytes[24..32].copy_from_slice(&0x4000b0_u64.to_le_bytes());
    bytes[56..58].copy_from_slice(&2_u16.to_le_bytes());
    bytes[96..104].copy_from_slice(&(size as u64).to_le_bytes());
    bytes[104..112].copy_from_slice(&(size as u64).to_le_bytes());
    bytes[120..124].copy_from_slice(&3_u32.to_le_bytes());
    bytes[124..128].copy_from_slice(&4_u32.to_le_bytes());
    bytes[128..136].copy_from_slice(&177_u64.to_le_bytes());
    bytes[136..144].copy_from_slice(&0x4000b1_u64.to_le_bytes());
    bytes[152..160].copy_from_slice(&((interpreter.len() + 1) as u64).to_le_bytes());
    bytes[160..168].copy_from_slice(&((interpreter.len() + 1) as u64).to_le_bytes());
    bytes[176] = 0xc3;
    bytes[177..size - 1].copy_from_slice(interpreter.as_bytes());
    bytes
}

#[test]
fn valid_fixture_passes_with_non_sensitive_json_and_releases_writer_lease() {
    let fixture = Fixture::new();
    assert_eq!(fixture.check().unwrap(), json!({"status": "passed"}));
    let path = fixture
        .path("ANCHOR_RUNNER_STATE_ROOT")
        .join("deployment-locks/.deployment-writer.lock");
    assert_eq!(path.metadata().unwrap().mode() & 0o7777, 0o600);
    let lock = File::options().read(true).write(true).open(&path).unwrap();
    rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
    assert!(path.exists());
}

#[test]
fn public_check_cannot_override_the_fixed_goose_pin_from_environment_or_manifest() {
    let fixture = Fixture::new();
    assert!(
        check(&fixture.env)
            .unwrap_err()
            .contains("fixed v1.53.0 pin")
    );
    fixture.edit_manifest(|manifest| {
        manifest["goose_sha256"] = json!(GOOSE_SHA256);
        manifest["executables"][1]["sha256"] = json!(GOOSE_SHA256);
    });
    assert!(
        check(&fixture.env)
            .unwrap_err()
            .contains("packaged Goose file digest")
    );
}

#[test]
fn rejects_wrong_pin_model_placeholder_and_missing_shared_network_authorization() {
    for (name, configured, expected) in [
        ("ANCHOR_GOOSE_BINARY_SHA256", "invalid", "fixed Goose"),
        (
            "ANCHOR_MODEL_NAME",
            "REPLACE_WITH_MODEL_NAME",
            "model name placeholder",
        ),
        (
            "ANCHOR_GOOSE_ALLOW_SHARED_NETWORK",
            "0",
            "ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1",
        ),
        (
            "ANCHOR_MODEL_API_KEY",
            " ",
            "ANCHOR_MODEL_API_KEY is required",
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.set(name, configured);
        let error = fixture.check().unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn isolation_defaults_to_a_relay_beside_the_goose_binary() {
    let mut fixture = Fixture::new();
    // No ANCHOR_GOOSE_LOCAL_NETWORK and no shared-network opt-in: the shipped relay
    // is what decides, exactly like the runtime.
    fixture.remove("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK");
    let relay = fixture.runtime.join("bin/anchor-net-relay");
    fs::write(&relay, "fixture: never executed").unwrap();
    set_mode(&relay, 0o755);
    assert_eq!(fixture.check().unwrap()["status"], "passed");

    // Without the relay the same configuration must ask for the shared-network opt-in.
    fs::remove_file(&relay).unwrap();
    let error = fixture.check().unwrap_err();
    assert!(
        error.contains("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK"),
        "{error}"
    );
}

#[test]
fn explicit_isolation_requires_a_relay_that_exists() {
    let mut fixture = Fixture::new();
    fixture.set("ANCHOR_GOOSE_LOCAL_NETWORK", "1");
    fixture.remove("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK");
    let error = fixture.check().unwrap_err();
    assert!(error.contains("anchor-net-relay"), "{error}");
    fixture.set("ANCHOR_GOOSE_RELAY_BINARY", "/nonexistent/anchor-net-relay");
    let error = fixture.check().unwrap_err();
    assert!(error.contains("anchor-net-relay"), "{error}");
}

#[test]
fn rejects_a_context_window_that_is_not_a_positive_integer() {
    for window in ["0", "abc", "-1", " "] {
        let mut fixture = Fixture::new();
        fixture.set("ANCHOR_MODEL_CONTEXT_WINDOW", window);
        let error = fixture.check().unwrap_err();
        assert!(error.contains("ANCHOR_MODEL_CONTEXT_WINDOW"), "{error}");
    }
}

#[test]
fn model_endpoint_rejects_unsafe_urls_without_echoing_credentials_or_url_content() {
    let mut fixture = Fixture::new();
    for endpoint in [
        "http://provider.example/v1",
        "ftp://127.0.0.1/v1",
        "https://user:private-secret@example.com/v1",
        "https://@example.com/v1",
        "https://example.com/v1?token=private-secret",
        "https://example.com/v1#private-secret",
        "https://example.com:0/v1",
        "https://example.com:65536/v1",
        "https:///example.com/v1",
        "https:example.com/v1",
        "https://exam\nple.com/v1",
        "http://[::1/v1",
        "http://localhost\\private-secret",
        "http://2130706433/v1",
        "http://127.1/v1",
        "http://127.0.0.2/v1",
        "http://0x7f000001/v1",
    ] {
        fixture.set("ANCHOR_MODEL_URL", endpoint);
        let error = check_endpoint(&fixture.env).unwrap_err();
        assert!(error.contains("ANCHOR_MODEL_URL"));
        assert!(!error.contains(endpoint));
        assert!(!error.contains("private-secret"));
    }
    for endpoint in [
        "https://provider.example/v1",
        "http://localhost:123/v1",
        "http://[::1]:123/v1",
    ] {
        fixture.set("ANCHOR_MODEL_URL", endpoint);
        check_endpoint(&fixture.env).unwrap();
    }
}

#[test]
fn rejects_model_controls_invalid_wire_protocol_and_non_string_aliases() {
    for (name, configured, expected) in [
        (
            "ANCHOR_MODEL_NAME",
            "fixture\u{1f}model",
            "control characters",
        ),
        ("ANCHOR_MODEL_WIRE_API", "other", "chat or responses"),
        ("ANCHOR_MODEL_ALIASES", "[]", "JSON object of strings"),
        (
            "ANCHOR_MODEL_ALIASES",
            "{\"model\":42}",
            "JSON object of strings",
        ),
        ("ANCHOR_MODEL_ALIASES", "broken", "JSON object of strings"),
    ] {
        let mut fixture = Fixture::new();
        fixture.set(name, configured);
        fixture.rejects(expected);
    }
}

#[test]
fn non_loopback_listeners_require_valid_unique_private_api_keys() {
    let mut fixture = Fixture::new();
    for listen in ["0.0.0.0:8077", "[::]:8077", "provider.example:8077"] {
        fixture.set("ANCHOR_RUNNER_LISTEN", listen);
        fixture.rejects("non-loopback");
    }
    fixture.set(
        "ANCHOR_API_KEYS",
        "[\"fixture-api-secret-with-at-least-32-bytes\"]",
    );
    fixture.check().unwrap();
    for keys in [
        "not-json-with-private-secret",
        "{}",
        "[42]",
        "[\"short-private-secret\"]",
        "[\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"]",
    ] {
        fixture.set("ANCHOR_API_KEYS", keys);
        let error = fixture.check().unwrap_err();
        assert!(error.contains("ANCHOR_API_KEYS"));
        assert!(!error.contains(keys));
        assert!(!error.contains("private-secret"));
    }
    fixture.set("ANCHOR_API_KEYS", "[]");
    for listen in ["localhost:8077", "127.1.2.3:8077", "[::1]:8077"] {
        fixture.set("ANCHOR_RUNNER_LISTEN", listen);
        fixture.check().unwrap();
    }
}

#[test]
fn listener_rejects_invalid_hosts_brackets_and_ports() {
    let mut fixture = Fixture::new();
    for listen in [
        ":8077",
        "127.0.0.1",
        "::1:8077",
        "[::1]8077",
        "[localhost]:8077",
        "127.0.0.1:0",
        "localhost:65536",
        "localhost: 8077",
        "localhost:８０７７",
        "local host:8077",
        "localhost:+8077",
    ] {
        fixture.set("ANCHOR_RUNNER_LISTEN", listen);
        fixture.rejects("host:port");
    }
}

#[test]
fn rejects_env_file_permissions_symlinks_and_non_regular_files() {
    let mut fixture = Fixture::new();
    set_mode(&fixture.path("ANCHOR_ENV_FILE"), 0o640);
    fixture.rejects("0600 or stricter");
    set_mode(&fixture.path("ANCHOR_ENV_FILE"), 0o600);
    let link = fixture.root.path().join("env-link");
    symlink(fixture.path("ANCHOR_ENV_FILE"), &link).unwrap();
    fixture.set("ANCHOR_ENV_FILE", link.to_str().unwrap());
    fixture.rejects("symlink path components");
    fixture.set(
        "ANCHOR_ENV_FILE",
        fixture.root.path().to_str().unwrap().to_owned().as_str(),
    );
    fixture.rejects("regular root- or service-owned file");
}

#[test]
fn rejects_mutable_root_permissions_nesting_and_overlap_with_runtime() {
    for (name, expected) in [
        ("ANCHOR_RUNNER_STATE_ROOT", "state root"),
        ("ANCHOR_RUNNER_WORKSPACE_ROOT", "workspace root"),
        ("ANCHOR_RUNNER_CATALOG_ROOT", "catalog root"),
    ] {
        let mut fixture = Fixture::new();
        set_mode(&fixture.path(name), 0o770);
        fixture.rejects("group- or world-writable");
        set_mode(&fixture.path(name), 0o700);
        fixture.set(name, fixture.runtime.to_str().unwrap().to_owned().as_str());
        fixture.rejects(expected);
    }
    let mut fixture = Fixture::new();
    let nested = fixture.path("ANCHOR_RUNNER_STATE_ROOT").join("workspaces");
    fs::create_dir(&nested).unwrap();
    set_mode(&nested, 0o700);
    fixture.set("ANCHOR_RUNNER_WORKSPACE_ROOT", nested.to_str().unwrap());
    fixture.rejects("separate, non-nested");
}

#[test]
fn rejects_symlinked_bundle_and_other_release_host_but_accepts_selected_host_alias() {
    let mut fixture = Fixture::new();
    let link = fixture.root.path().join("bundle-link");
    symlink(fixture.runtime.join("bundle"), &link).unwrap();
    fixture.set("ANCHOR_RUNNER_BUNDLE_ROOT", link.to_str().unwrap());
    fixture.rejects("symlink path components");
    fixture.set(
        "ANCHOR_RUNNER_BUNDLE_ROOT",
        fixture.runtime.join("bundle").to_str().unwrap(),
    );
    let other = fixture.root.path().join("other-host");
    fs::write(&other, static_elf()).unwrap();
    fixture.set("ANCHOR_RUNNER_HOST_BINARY", other.to_str().unwrap());
    fixture.rejects("selected runtime Host");
    let current = fixture.root.path().join("current");
    symlink(&fixture.runtime, &current).unwrap();
    fixture.set(
        "ANCHOR_RUNNER_HOST_BINARY",
        current.join("bin/anchor-runner-host").to_str().unwrap(),
    );
    fixture.check().unwrap();
}

#[test]
fn rejects_web_root_from_another_release_and_missing_web_root() {
    let mut fixture = Fixture::new();
    let missing = fixture.root.path().join("missing-web");
    fixture.set("ANCHOR_RUNNER_WEB_ROOT", missing.to_str().unwrap());
    fixture.rejects("ANCHOR_RUNNER_WEB_ROOT is unavailable");
    fs::create_dir(&missing).unwrap();
    fixture.rejects("selected source-free runtime");
}

#[test]
fn rejects_inventory_tampering_duplicate_unsafe_missing_and_symlinked_files() {
    let fixture = Fixture::new();
    fs::write(fixture.runtime.join("web/index.html"), "modified").unwrap();
    fixture.rejects("digest or size mismatch");
    fixture.write_manifest();
    fixture.edit_manifest(|manifest| {
        let entry = manifest["files"][0].clone();
        manifest["files"].as_array_mut().unwrap().push(entry);
    });
    fixture.rejects("duplicate file paths");
    for path in [
        "../outside-secret",
        "/tmp/outside-secret",
        "bundle/../graph.json",
        "./bundle/graph.json",
        "bundle//graph.json",
        "",
    ] {
        fixture.write_manifest();
        fixture.edit_manifest(|manifest| manifest["files"][0]["path"] = json!(path));
        fixture.rejects("unsafe path");
    }
    fixture.write_manifest();
    fixture.edit_manifest(|manifest| {
        manifest["files"].as_array_mut().unwrap().pop();
    });
    fixture.rejects("missing web/index.html");
    fixture.write_manifest();
    let outside = fixture.root.path().join("outside-web");
    fs::rename(fixture.runtime.join("web/index.html"), &outside).unwrap();
    symlink(outside, fixture.runtime.join("web/index.html")).unwrap();
    fixture.rejects("symlink path components");
}

#[test]
fn rejects_runtime_platform_executable_inventory_and_mode_drift() {
    let fixture = Fixture::new();
    fixture.edit_manifest(|manifest| manifest["platform"]["bits"] = json!(32));
    fixture.rejects("64-bit little-endian");
    fixture.write_manifest();
    fixture.edit_manifest(|manifest| manifest["executables"][0]["sha256"] = json!("wrong"));
    fixture.rejects("executable identity");
    fixture.write_manifest();
    fixture.edit_manifest(|manifest| {
        let identity = manifest["executables"][0].clone();
        manifest["executables"]
            .as_array_mut()
            .unwrap()
            .push(identity);
    });
    fixture.rejects("duplicate executable paths");
    fixture.write_manifest();
    fixture.edit_manifest(|manifest| {
        manifest["executables"].as_array_mut().unwrap().pop();
    });
    fixture.rejects("both Host and Goose");
    fixture.write_manifest();
    set_mode(&fixture.runtime.join("bin/anchor-runner-host"), 0o644);
    fixture.rejects("mode does not match");
    fixture.edit_manifest(|manifest| manifest["files"][0]["mode"] = json!(0o644));
    fixture.rejects("not executable");
}

#[test]
fn rejects_false_elf_metadata_and_non_elf_binaries_even_with_consistent_digests() {
    let fixture = Fixture::new();
    fixture.edit_manifest(|manifest| manifest["executables"][0]["runtime"]["needed"] = json!([""]));
    fixture.rejects("ELF runtime metadata");
    fixture.write_manifest();
    fixture.edit_manifest(|manifest| {
        manifest["executables"][0]["runtime"]["needed"] = json!(["imaginary-library.so"])
    });
    fixture.rejects("metadata does not match");
    fixture.write_manifest();
    fixture.edit_manifest(|manifest| {
        manifest["executables"][1]["runtime"]["interpreter"] = json!("/usr/bin/true")
    });
    fixture.rejects("static musl");
    fs::write(fixture.runtime.join("bin/anchor-runner-host"), "not an ELF").unwrap();
    fixture.edit_manifest(|manifest| {
        let path = fixture.runtime.join("bin/anchor-runner-host");
        manifest["files"][0]["sha256"] = json!(digest(&path));
        manifest["files"][0]["size"] = json!(path.metadata().unwrap().len());
        manifest["executables"][0]["sha256"] = json!(digest(&path));
    });
    fixture.rejects("valid ELF");
}

#[test]
fn checks_actual_elf_interpreter_availability_and_dynamic_library_inventory() {
    let fixture = Fixture::new();
    let host = fixture.runtime.join("bin/anchor-runner-host");
    let interpreter = fixture.root.path().join("fixture-interpreter");
    fs::write(&interpreter, "fixture interpreter: never executed").unwrap();
    set_mode(&interpreter, 0o755);
    fs::write(&host, elf_with_interpreter(interpreter.to_str().unwrap())).unwrap();
    fixture.write_manifest();
    fixture.check().unwrap();
    set_mode(&interpreter, 0o644);
    fixture.rejects("ELF interpreter from runtime manifest is unavailable");
    set_mode(&interpreter, 0o755);
    fs::remove_file(&interpreter).unwrap();
    fixture.rejects("ELF interpreter from runtime manifest is unavailable");
    fs::copy("/usr/bin/true", &host).unwrap();
    fixture.write_manifest();
    let actual = elf::inspect(&host).unwrap();
    assert!(!actual.needed.is_empty());
    fixture.check().unwrap();
    fixture.edit_manifest(|manifest| manifest["executables"][0]["runtime"]["needed"] = json!([]));
    fixture.rejects("metadata does not match");
}

#[test]
fn rejects_wrong_actual_elf_architecture_and_non_executable_entry_point() {
    let fixture = Fixture::new();
    let host = fixture.runtime.join("bin/anchor-runner-host");
    for (offset, replacement, expected) in [
        (18, vec![183, 0], "64-bit little-endian x86_64"),
        (24, vec![0; 8], "ELF executable entry point"),
        (68, vec![4, 0, 0, 0], "ELF executable entry point"),
    ] {
        let mut binary = static_elf();
        binary[offset..offset + replacement.len()].copy_from_slice(&replacement);
        fs::write(&host, binary).unwrap();
        let error = elf::inspect(&host).unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn refuses_held_writer_lease_without_modifying_lock_bytes_or_removing_inode() {
    let fixture = Fixture::new();
    fixture.check().unwrap();
    let path = fixture
        .path("ANCHOR_RUNNER_STATE_ROOT")
        .join("deployment-locks/.deployment-writer.lock");
    fs::write(&path, b"existing stable writer inode").unwrap();
    let inode = path.metadata().unwrap().ino();
    let held = File::options().read(true).write(true).open(&path).unwrap();
    held.try_lock().unwrap();
    fixture.rejects("another writing Host");
    assert_eq!(path.metadata().unwrap().ino(), inode);
    assert_eq!(fs::read(&path).unwrap(), b"existing stable writer inode");
    drop(held);
    fixture.check().unwrap();
}

#[test]
fn rejects_symlinked_or_untrusted_lock_directory_and_writer_lock() {
    let fixture = Fixture::new();
    let directory = fixture
        .path("ANCHOR_RUNNER_STATE_ROOT")
        .join("deployment-locks");
    symlink(fixture.root.path().join("tools"), &directory).unwrap();
    fixture.rejects("deployment lock directory");
    fs::remove_file(&directory).unwrap();
    fs::create_dir(&directory).unwrap();
    set_mode(&directory, 0o770);
    fixture.rejects("deployment lock directory");
    set_mode(&directory, 0o750);
    let lock = directory.join(".deployment-writer.lock");
    symlink(fixture.path("ANCHOR_ENV_FILE"), &lock).unwrap();
    fixture.rejects("deployment writer lock");
    fs::remove_file(&lock).unwrap();
    fs::write(&lock, "existing").unwrap();
    set_mode(&lock, 0o640);
    fixture.rejects("deployment writer lock");
    fs::remove_file(&lock).unwrap();
    rustix::fs::mkfifoat(rustix::fs::CWD, &lock, Mode::from_raw_mode(0o600)).unwrap();
    fixture.rejects("deployment writer lock");
}

#[test]
fn rejects_unsafe_command_allowlist_and_missing_bubblewrap_without_execution() {
    let mut fixture = Fixture::new();
    for commands in [
        "git",
        "sh,sh",
        "sh,/usr/bin/git",
        "sh,../git",
        "sh,",
        "sh,git,git",
        "sh,.",
        "sh,git/",
    ] {
        fixture.set("ANCHOR_RUNNER_ALLOWED_COMMANDS", commands);
        fixture.rejects("ANCHOR_RUNNER_ALLOWED_COMMANDS");
    }
    fixture.set("ANCHOR_RUNNER_ALLOWED_COMMANDS", " sh , git ");
    fixture.check().unwrap();
    fixture.set("ANCHOR_BWRAP", "/missing-bubblewrap");
    fixture.rejects("Bubblewrap executable");
    fixture.set("ANCHOR_BWRAP", "bwrap");
    fs::remove_file(fixture.root.path().join("tools/git")).unwrap();
    fixture.rejects("system command is unavailable: git");
}

#[test]
fn systemd_service_has_rust_only_lifecycle_and_no_preflight_start_hook() {
    let unit = include_str!("../../../../deploy/systemd/anchor.service");
    for expected in [
        "ExecStart=/opt/anchor/current/anchor-runtime/bin/anchor-runner-host serve",
        "Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        "User=anchor\nGroup=anchor",
        "KillSignal=SIGTERM",
        "KillMode=control-group",
        "TimeoutStopSec=30",
        "ReadWritePaths=/var/lib/anchor\n",
    ] {
        assert!(unit.contains(expected));
    }
    assert!(!unit.contains("ExecStartPre="));
    assert!(!unit.to_lowercase().contains("python"));
    assert!(!unit.contains("anchor-runtime/bundle"));
}

#[test]
fn deployment_templates_use_concrete_release_paths_and_secret_placeholders() {
    let host_env = include_str!("../../../../deploy/systemd/anchor.env.example");
    let gateway_env = include_str!("../../../../deploy/systemd/anchor-wecom-gateway.env.example");
    for expected in [
        "ANCHOR_RUNNER_BUNDLE_ROOT=/opt/anchor/releases/REPLACE_WITH_RELEASE_ID/anchor-runtime/bundle",
        "ANCHOR_RUNNER_WEB_ROOT=/opt/anchor/releases/REPLACE_WITH_RELEASE_ID/anchor-runtime/web",
        "ANCHOR_GOOSE_BINARY=/opt/anchor/releases/REPLACE_WITH_RELEASE_ID/anchor-runtime/bin/goose",
    ] {
        assert!(host_env.contains(expected));
    }
    let assignments = host_env
        .lines()
        .chain(gateway_env.lines())
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .collect::<BTreeMap<_, _>>();
    for name in [
        "ANCHOR_MODEL_API_KEY",
        "ANCHOR_API_KEY",
        "WECOM_BOT_SECRET",
        "ANCHOR_CHANNEL_CONTROL_TOKEN",
    ] {
        assert!(assignments[name].starts_with("REPLACE_WITH_"), "{name}");
    }
}

#[test]
fn optional_gateway_unit_uses_rust_binary_and_private_shared_contract() {
    let unit = include_str!("../../../../deploy/systemd/anchor-wecom-gateway.service");
    let env = include_str!("../../../../deploy/systemd/anchor-wecom-gateway.env.example");
    for expected in [
        "Requires=anchor.service",
        "After=anchor.service",
        "PartOf=anchor.service",
        "ExecStart=/opt/anchor/current/anchor-runtime/bin/anchor-wecom-gateway",
        "User=anchor\nGroup=anchor",
        "KillSignal=SIGTERM",
        "KillMode=control-group",
        "ReadWritePaths=/var/lib/anchor/channels/wecom",
    ] {
        assert!(unit.contains(expected));
    }
    assert!(!unit.to_lowercase().contains("python"));
    for expected in [
        "/channels/wecom/events",
        "/var/lib/anchor/channels/wecom/control.json",
        "ANCHOR_API_KEY=",
    ] {
        assert!(env.contains(expected));
    }
}
