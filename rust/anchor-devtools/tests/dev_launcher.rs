use std::{fs, net::TcpListener, process::Command};

#[test]
fn development_launcher_ignores_shell_configuration_files() {
    let root = tempfile::tempdir().unwrap();
    let scripts = root.path().join("scripts");
    fs::create_dir(&scripts).unwrap();
    let script = scripts.join("dev.sh");
    fs::write(&script, include_bytes!("../../../scripts/dev.sh")).unwrap();
    let sentinel = root.path().join("sourced-env");
    fs::write(
        root.path().join(".env"),
        format!("touch '{}'\nif\n", sentinel.display()),
    )
    .unwrap();
    let api = TcpListener::bind("127.0.0.1:0").unwrap();
    let web = TcpListener::bind("127.0.0.1:0").unwrap();
    let output = Command::new("bash")
        .arg(&script)
        .arg("status")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env(
            "ANCHOR_RUNNER_LISTEN",
            api.local_addr().unwrap().to_string(),
        )
        .env(
            "ANCHOR_WEB_PORT",
            web.local_addr().unwrap().port().to_string(),
        )
        .env("ANCHOR_DEV_ENV_FILE", root.path().join("missing.env"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!sentinel.exists());
    assert!(root.path().join(".env").is_file());
    assert!(root.path().join(".local/rust/dev/lifecycle.lock").is_file());
}

fn executable(path: &std::path::Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

struct SourceCheckout {
    root: tempfile::TempDir,
    tools: std::path::PathBuf,
}

impl SourceCheckout {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let tools = root.path().join("tools");
        fs::create_dir_all(&tools).unwrap();
        fs::create_dir_all(root.path().join("scripts")).unwrap();
        fs::create_dir_all(root.path().join("apps/web/src")).unwrap();
        for name in ["index.html", "vite.config.ts", "tsconfig.json"] {
            fs::write(root.path().join("apps/web").join(name), "").unwrap();
        }
        fs::write(
            root.path().join("scripts/dev.sh"),
            include_bytes!("../../../scripts/dev.sh"),
        )
        .unwrap();
        fs::write(
            root.path().join("scripts/dev-examples.mjs"),
            include_bytes!("../../../scripts/dev-examples.mjs"),
        )
        .unwrap();
        fs::create_dir_all(root.path().join("examples")).unwrap();
        fs::write(
            root.path().join("examples/starter.json"),
            r#"{"plugins":{"anchor-steward":[]},"graphs":[]}"#,
        )
        .unwrap();
        fs::create_dir_all(root.path().join("plugins/anchor-steward")).unwrap();
        fs::write(
            root.path().join("plugins/anchor-steward/plugin.json"),
            r#"{"name":"Starter fixture"}"#,
        )
        .unwrap();
        fs::write(root.path().join("apps/web/package.json"), "{}").unwrap();
        fs::write(root.path().join("apps/web/package-lock.json"), "{}").unwrap();
        executable(&tools.join("bwrap"), "#!/bin/sh\nexit 0\n");
        executable(&tools.join("slirp4netns"), "#!/bin/sh\nexit 0\n");
        executable(
            &tools.join("cargo"),
            r#"#!/bin/sh
printf 'cargo %s\n' "$*" >> "$CHECKOUT/commands"
mkdir -p "$CARGO_TARGET_DIR/debug"
for tool in anchor-runner-host anchor-net-relay; do
  printf '#!/bin/sh\nexit 0\n' > "$CARGO_TARGET_DIR/debug/$tool"
  chmod +x "$CARGO_TARGET_DIR/debug/$tool"
done
cat > "$CARGO_TARGET_DIR/debug/anchor-library" <<'INSTALL'
#!/bin/sh
mkdir -p "$2/plugins/$7"
cp -R "$5/." "$2/plugins/$7/"
INSTALL
chmod +x "$CARGO_TARGET_DIR/debug/anchor-library"
"#,
        );
        executable(
            &tools.join("npm"),
            r#"#!/bin/sh
printf 'npm %s\n' "$*" >> "$CHECKOUT/commands"
case "$3" in
  ci)
    mkdir -p "$2/node_modules/.bin"
    for tool in vite tsc; do
      printf '#!/bin/sh\nexit 0\n' > "$2/node_modules/.bin/$tool"
      chmod +x "$2/node_modules/.bin/$tool"
    done ;;
  run) mkdir -p "$2/dist"; printf built > "$2/dist/index.html" ;;
esac
"#,
        );
        // Exercise launcher orchestration without downloading a model runtime.
        // The actual builder's pin/error behavior is covered below separately.
        executable(
            &root.path().join("scripts/build-goose-acp.sh"),
            r#"#!/bin/sh
[ "$1" = --ensure ] && [ "$2" = --output ] || exit 80
[ -x "$3" ] && exit 0
printf 'goose build\n' >> "$CHECKOUT/commands"
mkdir -p "$(dirname "$3")"
printf '#!/bin/sh\nexit 0\n' > "$3"
chmod +x "$3"
"#,
        );
        Self { root, tools }
    }

    fn run(&self, action: &str, overrides: &[(&str, &std::path::Path)]) -> std::process::Output {
        let mut command = Command::new("bash");
        command
            .arg(self.root.path().join("scripts/dev.sh"))
            .arg(action)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.tools.display()))
            .env("CHECKOUT", self.root.path());
        for (name, value) in overrides {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.path().join("commands")).unwrap_or_default()
    }
}

#[test]
fn fresh_source_setup_prepares_web_host_relay_and_goose_and_reuses_them() {
    let checkout = SourceCheckout::new();
    // A leftover release output must not hide a missing development relay.
    fs::create_dir_all(checkout.root.path().join("rust/target/release")).unwrap();
    executable(
        &checkout
            .root
            .path()
            .join("rust/target/release/anchor-runner-host"),
        "#!/bin/sh\nexit 77\n",
    );
    let output = checkout.run("setup", &[]);
    assert!(output.status.success(), "{output:?}");
    let first = checkout.log();
    assert!(first.contains("npm --prefix"));
    assert!(first.contains(" ci\n"));
    assert!(first.contains("-p anchor-runner-host -p anchor-sandbox-bwrap --bins --locked"));
    assert!(first.contains("goose build"));
    assert!(first.contains("run build"));
    assert!(checkout.root.path().join(".local/bin/goose").is_file());
    assert!(
        !checkout
            .root
            .path()
            .join(".local/rust/dev/native-host.pid")
            .exists()
    );
    let output = checkout.run("setup", &[]);
    assert!(output.status.success(), "{output:?}");
    let second = checkout.log();
    assert_eq!(
        second.matches(" ci\n").count(),
        1,
        "dependencies are reused"
    );
    assert_eq!(second.matches("goose build").count(), 1, "Goose is reused");
    assert_eq!(
        second.matches("run build").count(),
        1,
        "Web assets are reused"
    );
    assert_eq!(
        second.matches("cargo ").count(),
        3,
        "Cargo checks source updates incrementally"
    );
    // A missing relay is repaired even when the Host already exists.
    fs::remove_file(
        checkout
            .root
            .path()
            .join("rust/target/debug/anchor-net-relay"),
    )
    .unwrap();
    let output = checkout.run("setup", &[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(checkout.log().matches("cargo ").count(), 4);
    // Updating the dependency lock invalidates the install stamp.
    fs::write(
        checkout.root.path().join("apps/web/package-lock.json"),
        "{\"updated\":true}",
    )
    .unwrap();
    let output = checkout.run("setup", &[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(checkout.log().matches(" ci\n").count(), 2);
}

#[test]
fn source_examples_preserve_plugin_edits_and_do_not_restore_deletions() {
    let checkout = SourceCheckout::new();
    let plugin = checkout
        .root
        .path()
        .join(".local/rust/library/plugins/anchor-steward");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Pre-existing plugin"}"#,
    )
    .unwrap();
    let output = checkout.run("setup", &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        fs::read_to_string(plugin.join("plugin.json"))
            .unwrap()
            .contains("Pre-existing plugin")
    );
    fs::write(plugin.join("plugin.json"), r#"{"name":"User edit"}"#).unwrap();
    assert!(checkout.run("setup", &[]).status.success());
    assert!(
        fs::read_to_string(plugin.join("plugin.json"))
            .unwrap()
            .contains("User edit")
    );
    fs::remove_dir_all(&plugin).unwrap();
    assert!(checkout.run("setup", &[]).status.success());
    assert!(
        !plugin.exists(),
        "startup must respect an operator's deletion"
    );
}

#[test]
fn source_setup_reports_proc_failure_before_builds_or_downloads() {
    let checkout = SourceCheckout::new();
    executable(
        &checkout.tools.join("bwrap"),
        "#!/bin/sh\nfor argument do\n  if [ \"$argument\" = --proc ]; then\n    echo \"bwrap: Can't mount proc on /newroot/proc: Operation not permitted\" >&2\n    exit 1\n  fi\ndone\nexit 0\n",
    );
    let output = checkout.run("setup", &[]);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("required namespaces with a private /proc"));
    assert!(stderr.contains("Can't mount proc on /newroot/proc: Operation not permitted"));
    assert!(checkout.log().is_empty());
}

#[test]
fn source_setup_does_not_replace_missing_explicit_runtime_paths() {
    let checkout = SourceCheckout::new();
    let missing = checkout.root.path().join("custom-host");
    let output = checkout.run("setup", &[("ANCHOR_RUNNER_BINARY", &missing)]);
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Explicit Host/relay paths"));
    assert!(!checkout.log().contains("cargo "));
    assert!(!missing.exists());
}

#[test]
fn source_status_and_invalid_action_do_not_prepare_dependencies() {
    let checkout = SourceCheckout::new();
    assert!(checkout.run("status", &[]).status.success());
    let invalid = checkout.run("unknown", &[]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("setup"));
    assert!(checkout.log().is_empty());
}

#[test]
fn ensure_goose_reuses_machine_local_output_and_rebuilds_changed_inputs() {
    use sha2::{Digest, Sha256};

    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("build-goose-acp.sh");
    fs::write(
        &script,
        include_bytes!("../../../scripts/build-goose-acp.sh"),
    )
    .unwrap();
    let patches = root.path().join("patches");
    fs::create_dir(&patches).unwrap();
    let mut inputs = format!("{:x}\n", Sha256::digest(fs::read(&script).unwrap()));
    for (name, contents) in [
        (
            "goose-1.53.0-responses-reasoning.patch",
            include_str!("../../../scripts/patches/goose-1.53.0-responses-reasoning.patch"),
        ),
        (
            "goose-1.53.0-external-completion.patch",
            include_str!("../../../scripts/patches/goose-1.53.0-external-completion.patch"),
        ),
        (
            "goose-1.53.0-provider-errors.patch",
            include_str!("../../../scripts/patches/goose-1.53.0-provider-errors.patch"),
        ),
    ] {
        fs::write(patches.join(name), contents).unwrap();
        inputs.push_str(&format!("{:x}\n", Sha256::digest(contents.as_bytes())));
    }
    let input_digest = format!("{:x}", Sha256::digest(inputs.as_bytes()));
    let output_path = root.path().join("goose");
    let contents = "#!/bin/sh\n# local compiler output\nexit 0\n";
    executable(&output_path, contents);
    let digest = format!("{:x}", Sha256::digest(contents.as_bytes()));
    fs::write(
        root.path().join("goose.sha256"),
        format!("{digest}  goose\n#source-inputs {input_digest}\n"),
    )
    .unwrap();
    let run = || {
        Command::new("bash")
            .arg(&script)
            .args(["--ensure", "--output"])
            .arg(&output_path)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap()
    };
    let output = run();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), digest);
    assert_eq!(fs::read_to_string(&output_path).unwrap(), contents);

    // A changed patch requires a build, even when the previous executable exists.
    fs::write(
        patches.join("goose-1.53.0-provider-errors.patch"),
        "changed patch",
    )
    .unwrap();
    let output = run();
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing build prerequisite: rustup"));
    assert_eq!(fs::read_to_string(output_path).unwrap(), contents);
    let output = Command::new("bash")
        .arg(script)
        .arg("--ensure")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--ensure requires --output"));
}
