use super::*;
use anchor_runtime::{NetworkPolicy, SandboxPort};
use anchor_sandbox_bwrap::BubblewrapSandbox;
use serde_json::json;

use crate::test_support::Environment;

#[test]
fn python_probe_times_out_and_reaps_the_configured_process() {
    let root = tempfile::tempdir().unwrap();
    let interpreter = root.path().join("python3");
    let pid_file = root.path().join("pid");
    write_launcher(
        &interpreter,
        &format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > {}\nexec /bin/sleep 60\n",
            shell_path(&pid_file).unwrap()
        ),
    )
    .unwrap();
    let started = Instant::now();
    let error = probe_python(&interpreter, Duration::from_millis(150)).unwrap_err();
    assert!(error.contains("ANCHOR_DEPENDENCIES_PYTHON probe timed out"));
    assert!(started.elapsed() < Duration::from_secs(3));
    let pid = fs::read_to_string(pid_file).unwrap();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}

#[test]
fn python_probe_bounds_output_and_does_not_wait_for_an_inherited_pipe() {
    let root = tempfile::tempdir().unwrap();
    let interpreter = root.path().join("python3");
    write_launcher(&interpreter, "#!/bin/sh\nexec /usr/bin/yes\n").unwrap();
    let error = probe_python(&interpreter, Duration::from_secs(1)).unwrap_err();
    assert!(error.contains("probe output exceeded 4096 bytes"));

    let interpreter = root.path().join("python-with-descendant");
    let pid_file = root.path().join("descendant-pid");
    write_launcher(
        &interpreter,
        &format!(
            "#!/bin/sh\n/bin/sleep 60 &\nprintf '%s' \"$!\" > {}\nprintf '3.12\\n'\n",
            shell_path(&pid_file).unwrap()
        ),
    )
    .unwrap();
    let started = Instant::now();
    let output = probe_python(&interpreter, Duration::from_secs(1)).unwrap();
    assert_eq!(output.stdout, b"3.12\n");
    assert!(started.elapsed() < Duration::from_secs(2));
    let pid = fs::read_to_string(pid_file).unwrap();
    // A descendant can briefly remain a zombie under the host's init process,
    // but it must no longer be running or keep a probe pipe alive.
    let stopped = Instant::now();
    loop {
        let status = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        if status.is_empty() || status.split_whitespace().nth(2) == Some("Z") {
            break;
        }
        assert!(stopped.elapsed() < Duration::from_secs(1), "{status}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn python_full_version_probe_preserves_the_execution_abi_probe() {
    let root = tempfile::tempdir().unwrap();
    let interpreter = root.path().join("python3");
    write_launcher(
        &interpreter,
        "#!/bin/sh\n[ \"$1\" = -I ] && [ \"$2\" = -S ] && [ \"$3\" = -c ] || exit 1\ncase \"$4\" in\n*sys.version_info.micro*) printf '3.12.13\\n' ;;\n*) printf '3.12\\n' ;;\nesac\n",
    )
    .unwrap();
    assert_eq!(probe_python_version(&interpreter).unwrap(), "3.12.13");
    assert_eq!(
        probe_python(&interpreter, Duration::from_secs(1))
            .unwrap()
            .stdout,
        b"3.12\n"
    );
}

#[test]
fn python_full_version_probe_reports_failed_and_malformed_results() {
    let root = tempfile::tempdir().unwrap();
    let interpreter = root.path().join("python3");
    for version in ["3.12", "3.12.x", "3.12.13\\nextra"] {
        write_launcher(&interpreter, &format!("#!/bin/sh\nprintf '{version}\\n'\n")).unwrap();
        let error = probe_python_version(&interpreter).unwrap_err();
        assert!(
            error.contains("numeric major.minor.patch version"),
            "{error}"
        );
        fs::remove_file(&interpreter).unwrap();
    }
    write_launcher(
        &interpreter,
        "#!/bin/sh\nprintf 'fixture interpreter failure' >&2\nexit 1\n",
    )
    .unwrap();
    let error = probe_python_version(&interpreter).unwrap_err();
    assert_eq!(error, "Python version probe failed");
    assert!(!error.contains("fixture interpreter failure"));
    fs::remove_file(&interpreter).unwrap();
    let error = probe_python_version(&interpreter).unwrap_err();
    assert!(error.contains("cannot be probed"), "{error}");
}

#[test]
fn python_full_version_probe_bounds_time_output_and_reaps_the_process() {
    let root = tempfile::tempdir().unwrap();
    let interpreter = root.path().join("python3");
    let pid_file = root.path().join("pid");
    write_launcher(
        &interpreter,
        &format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > {}\nexec /bin/sleep 60\n",
            shell_path(&pid_file).unwrap()
        ),
    )
    .unwrap();
    let started = Instant::now();
    let error = probe_python_version(&interpreter).unwrap_err();
    assert!(error.contains("probe timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(3));
    let pid = fs::read_to_string(pid_file).unwrap();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    fs::remove_file(&interpreter).unwrap();
    write_launcher(&interpreter, "#!/bin/sh\nexec /usr/bin/yes\n").unwrap();
    let error = probe_python_version(&interpreter).unwrap_err();
    assert!(
        error.contains("probe output exceeded 4096 bytes"),
        "{error}"
    );
}

#[tokio::test]
async fn cargo_cache_writes_stay_in_the_session_and_shared_sources_are_read_only() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("shared/bin");
    let home = root.path().join("shared/rustup");
    let cargo = root.path().join("shared/cargo");
    for directory in [&bin, &home, &cargo] {
        fs::create_dir_all(directory).unwrap();
    }
    for name in ["rustup", "cargo", "rustc"] {
        fs::copy("/usr/bin/true", bin.join(name)).unwrap();
    }
    let source = cargo.join("registry/src/probe/source.rs");
    let git_source = cargo.join("git/checkouts/probe/source.rs");
    for path in [&source, &git_source] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "shared-original").unwrap();
    }
    fs::hard_link(&source, source.with_file_name("hardlink.rs")).unwrap();
    std::os::unix::fs::symlink(
        "/anchor-rust/cache/registry/src/probe/source.rs",
        source.with_file_name("symlink.rs"),
    )
    .unwrap();
    fs::write(cargo.join("credentials.toml"), "host-private").unwrap();
    fs::create_dir(cargo.join("anchor-pilot")).unwrap();
    fs::write(
        cargo.join("anchor-pilot/config.toml"),
        "old-session-settings",
    )
    .unwrap();
    let mut tools = ToolEnvironment::default();
    tools
        .configure_pilot_rust(&bin.join("rustup"), &home, &cargo)
        .unwrap();
    let sandbox =
        BubblewrapSandbox::new(tools.authorize(
            BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(root.path()),
        ))
        .unwrap();
    let work1 = root.path().join("session-1");
    let work2 = root.path().join("session-2");
    let work3 = root.path().join("session-3");
    for work in [&work1, &work2, &work3] {
        fs::create_dir(work).unwrap();
    }
    for (work, command) in [
        (
            &work1,
            r#"set -eu
cargo --version & first=$!
cargo --version & second=$!
wait "$first"
wait "$second"
test "$CARGO_HOME" = /workspace/.anchor-rust/cargo
test ! -e "$CARGO_HOME/registry/registry"
test ! -e "$CARGO_HOME/credentials.toml"
test ! -e "$CARGO_HOME/config.toml"
test "$(cat "$CARGO_HOME/registry/src/probe/source.rs")" = shared-original
printf session-1 > "$CARGO_HOME/registry/src/probe/source.rs"
printf session-1 > "$CARGO_HOME/registry/src/probe/hardlink.rs"
printf session-1 > "$CARGO_HOME/git/checkouts/probe/source.rs"
if printf pollution > /anchor-rust/cache/registry/src/probe/source.rs; then exit 11; fi
if printf pollution > /anchor-rust/cache/git/checkouts/probe/source.rs; then exit 12; fi
if printf pollution > "$CARGO_HOME/registry/src/probe/symlink.rs"; then exit 13; fi
printf private > "$CARGO_HOME/config.toml"
"#,
        ),
        (
            &work2,
            r#"set -eu
"$EXACT_CARGO" --version
test "$(cat "$CARGO_HOME/registry/src/probe/source.rs")" = shared-original
test "$(cat "$CARGO_HOME/git/checkouts/probe/source.rs")" = shared-original
test ! -e "$CARGO_HOME/config.toml"
"#,
        ),
        (
            &work1,
            r#"set -eu
cargo --version
test "$(cat "$CARGO_HOME/registry/src/probe/source.rs")" = session-1
test "$(cat "$CARGO_HOME/config.toml")" = private
"#,
        ),
    ] {
        let mut request = SandboxRequest::new(work, ["sh", "-c", command]);
        tools.apply(&mut request);
        request.environment.push(SandboxEnvironment::new(
            "EXACT_CARGO",
            bin.join("cargo").to_string_lossy(),
        ));
        let scoped = tools.sandbox_for_command(&sandbox, &mut request).unwrap();
        assert!(request.writable_inputs.is_empty());
        let result = scoped.run(request).await.unwrap();
        assert_eq!(result.exit_code, Some(0), "{result:?}");
    }
    assert_eq!(fs::read_to_string(&source).unwrap(), "shared-original");
    assert_eq!(fs::read_to_string(&git_source).unwrap(), "shared-original");
    use std::os::unix::fs::MetadataExt;
    assert_ne!(
        fs::metadata(&source).unwrap().ino(),
        fs::metadata(work1.join(".anchor-rust/cargo/registry/src/probe/source.rs"))
            .unwrap()
            .ino()
    );

    // Seeding an attacker-controlled cache destination still happens inside
    // Bubblewrap, so this symlink cannot turn a Host copy into a shared write.
    fs::create_dir(work3.join(".anchor-rust")).unwrap();
    std::os::unix::fs::symlink(
        "/anchor-rust/cache/registry",
        work3.join(".anchor-rust/cargo"),
    )
    .unwrap();
    let mut request = SandboxRequest::new(&work3, ["cargo", "--version"]);
    tools.apply(&mut request);
    let result = tools
        .sandbox_for_command(&sandbox, &mut request)
        .unwrap()
        .run(request)
        .await
        .unwrap();
    assert_ne!(result.exit_code, Some(0));
    assert!(!cargo.join("registry/.anchor-cache-seeded").exists());

    // A stopped first copy must not expose a partial cache or prevent a retry.
    let work4 = root.path().join("session-4");
    let fault_bin = root.path().join("fault/bin");
    fs::create_dir(&work4).unwrap();
    fs::create_dir_all(&fault_bin).unwrap();
    write_launcher(
        &fault_bin.join("cp"),
        "#!/bin/sh\nmkdir -p \"$3\"\nprintf partial > \"$3/truncated\"\nprintf copy-started\nexec /bin/sleep 60\n",
    )
    .unwrap();
    let mut interrupted = tools.clone();
    interrupted.mount_at_original_path(&fault_bin).unwrap();
    interrupted.dirs.insert(0, fault_bin);
    let mut request = SandboxRequest::new(&work4, ["cargo", "--version"]);
    request.timeout = Duration::from_millis(250);
    interrupted.apply(&mut request);
    let result = interrupted
        .sandbox_for_command(&sandbox, &mut request)
        .unwrap()
        .run(request)
        .await
        .unwrap();
    assert_eq!(result.status, anchor_runtime::SandboxStatus::TimedOut);
    assert!(result.stdout.starts_with("copy-started"), "{result:?}");
    assert!(!work4.join(".anchor-rust/cargo/registry").exists());
    assert!(
        !work4
            .join(".anchor-rust/cargo/.anchor-cache-seeded")
            .exists()
    );
    let mut retry = SandboxRequest::new(&work4, ["cargo", "--version"]);
    tools.apply(&mut retry);
    let result = tools
        .sandbox_for_command(&sandbox, &mut retry)
        .unwrap()
        .run(retry)
        .await
        .unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(
        fs::read_to_string(work4.join(".anchor-rust/cargo/registry/src/probe/source.rs")).unwrap(),
        "shared-original"
    );
}

#[tokio::test]
#[ignore = "requires deployment Rustup grants and ANCHOR_TEST_RUST_TOOLCHAIN"]
async fn shared_rustup_default_and_named_cargo_build_in_private_proc_namespace() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir_all(work.join("src")).unwrap();
    let named =
        std::env::var("ANCHOR_TEST_RUST_TOOLCHAIN").expect("set an installed Rust toolchain");
    let cache = root.path().join("shared-cargo");
    let dependency = cache.join("registry/src/cache-probe");
    fs::create_dir_all(dependency.join("src")).unwrap();
    fs::write(
        dependency.join("Cargo.toml"),
        "[package]\nname = \"cache-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dependency.join("src/lib.rs"),
        "pub fn value() -> &'static str { \"cached-dependency-ok\" }\n",
    )
    .unwrap();
    fs::create_dir_all(cache.join("git/checkouts/probe")).unwrap();
    fs::write(cache.join("git/checkouts/probe/source"), "shared-original").unwrap();
    fs::write(cache.join("credentials.toml"), "host-private").unwrap();
    let proxy = std::env::var_os("ANCHOR_PILOT_RUSTUP").expect("deployment Rustup proxy");
    let home = std::env::var_os("ANCHOR_PILOT_RUSTUP_HOME").expect("shared Rustup home");
    let mut tools = ToolEnvironment::default();
    tools
        .configure_pilot_rust(Path::new(&proxy), Path::new(&home), &cache)
        .unwrap();
    let rust = tools
        .rust
        .as_ref()
        .expect("deployment Rustup must be configured");
    let sandbox = BubblewrapSandbox::new(
        tools.authorize(
            BubblewrapPolicy::new("bwrap", ["cargo", "rustc", "rustup", "sh"])
                .authorize_workspace_root(&work),
        ),
    )
    .unwrap();
    fs::write(
        work.join("Cargo.toml"),
        "[package]\nname = \"private-proc-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\ncache-probe = { path = \".anchor-rust/cargo/registry/src/cache-probe\" }\n",
    )
    .unwrap();
    fs::write(
        work.join("src/main.rs"),
        "fn main() { assert_eq!(cache_probe::value(), \"cached-dependency-ok\"); println!(\"shared-rust-build-ok\"); }\n",
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    fs::write(work.join("build.rs"), format!(r#"
fn main() {{
    assert!(std::fs::read_link("/proc/self/exe").is_ok());
    assert!(!std::path::Path::new("/proc/{}/status").exists());
    let address = "{}".parse().unwrap();
    assert!(std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(100)).is_err());
    assert!(std::fs::write({:?}, "denied").is_err());
    let cargo_home = std::env::var("CARGO_HOME").unwrap();
    assert_eq!(cargo_home, "/workspace/.anchor-rust/cargo");
    assert!(!std::path::Path::new(&cargo_home).join("credentials.toml").exists());
    assert!(std::fs::write(std::path::Path::new(&cargo_home).join("registry/src/cache-probe/private-build-data"), "private").is_ok());
    assert!(std::fs::write("/anchor-rust/cache/registry/src/cache-probe/src/lib.rs", "denied").is_err());
    assert!(std::fs::write("/anchor-rust/cache/git/checkouts/probe/source", "denied").is_err());
}}
"#, std::process::id(), listener.local_addr().unwrap(), rust.home.join("anchor-write-denied").to_str().unwrap())).unwrap();
    for selector in [None, Some(named.as_str())] {
        let mut command = vec!["cargo".to_owned()];
        if let Some(selector) = selector {
            command.push(format!("+{selector}"));
        }
        let mut version_command = command.clone();
        version_command.push("--version".into());
        let mut version = SandboxRequest::new(&work, version_command);
        tools.apply(&mut version);
        let result = tools
            .sandbox_for_command(&sandbox, &mut version)
            .unwrap()
            .run(version)
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0), "{result:?}");
        assert!(result.stdout.starts_with("cargo "), "{result:?}");
        let target = selector.unwrap_or("default");
        command.extend([
            "build".into(),
            "--offline".into(),
            "--target-dir".into(),
            format!("target/{target}"),
        ]);
        let mut request = SandboxRequest::new(&work, command);
        request.timeout = std::time::Duration::from_secs(60);
        tools.apply(&mut request);
        let result = tools
            .sandbox_for_command(&sandbox, &mut request)
            .unwrap()
            .run(request)
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0), "{target}: {result:?}");
        let mut run = SandboxRequest::new(
            &work,
            vec![
                "sh".into(),
                "-c".into(),
                format!("target/{target}/debug/private-proc-probe"),
            ],
        );
        tools.apply(&mut run);
        let result = tools
            .sandbox_for_command(&sandbox, &mut run)
            .unwrap()
            .run(run)
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0), "{result:?}");
        assert_eq!(result.stdout.trim(), "shared-rust-build-ok");
    }
    let mut shell = SandboxRequest::new(
        &work,
        [
            "sh",
            "-c",
            "cargo build --offline --target-dir target/shell && cp target/shell/debug/private-proc-probe shell-probe && ./shell-probe",
        ],
    );
    shell.timeout = std::time::Duration::from_secs(60);
    tools.apply(&mut shell);
    let result = tools
        .sandbox_for_command(&sandbox, &mut shell)
        .unwrap()
        .run(shell)
        .await
        .unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout.trim(), "shared-rust-build-ok");
    assert_eq!(
        fs::read_to_string(dependency.join("src/lib.rs")).unwrap(),
        "pub fn value() -> &'static str { \"cached-dependency-ok\" }\n"
    );
    assert!(!dependency.join("private-build-data").exists());
}

fn available_pilot_python() -> Option<PathBuf> {
    let configured = std::env::var_os("ANCHOR_TEST_PILOT_PYTHON").map(PathBuf::from);
    configured
        .into_iter()
        .chain([
            PathBuf::from(
                "/home/mansteinl/.local/share/uv/python/cpython-3.12.13-linux-x86_64-gnu/bin/python3",
            ),
            PathBuf::from("/usr/bin/python3.12"),
            PathBuf::from("/usr/bin/python3.11"),
            PathBuf::from("/usr/bin/python3.10"),
            PathBuf::from("/usr/bin/python3.9"),
            PathBuf::from("/usr/bin/python3.8"),
        ])
        .find(|path| path.is_file())
}

#[test]
fn platform_status_distinguishes_python_and_rust_grants_without_exposing_paths() {
    let root = tempfile::tempdir().unwrap();
    let python_prefix = root.path().join("shared-python");
    let python_bin = python_prefix.join("bin");
    fs::create_dir_all(&python_bin).unwrap();
    let interpreter = python_bin.join("python3");
    fs::write(&interpreter, "test interpreter").unwrap();

    let rust_bin = root.path().join("rust/bin");
    let rust_home = root.path().join("rustup");
    fs::create_dir_all(&rust_bin).unwrap();
    fs::create_dir_all(rust_home.join("toolchains/stable-x86_64-unknown-linux-gnu")).unwrap();
    let proxy = rust_bin.join("rustup");
    for name in ["rustup", "cargo", "rustc"] {
        let executable = rust_bin.join(name);
        fs::write(&executable, "authorized proxy").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let environment = ToolEnvironment {
        python: Some(PythonDevelopment {
            interpreter: interpreter.clone(),
            version: "3.12".into(),
        }),
        rust: Some(RustDevelopment {
            proxy,
            home: rust_home,
            toolchain: None,
        }),
        ..ToolEnvironment::default()
    };
    let status = environment.pilot_platform_status(
        true,
        json!({
            "configured":true,
            "shared_interpreter_configured":true,
            "shared_interpreter_available":true,
            "uv_available":true,
            "uv_digest_verified":true,
            "sources":[{"type":"index","configured":true,"reachability_checked":false}]
        }),
    );
    assert_eq!(status["python"]["shared_interpreter_configured"], true);
    assert_eq!(status["python"]["authoring_granted"], true);
    assert_eq!(status["python"]["version"], "3.12");
    assert_eq!(status["python"]["pip_write"], false);
    assert_eq!(status["python"]["pip_write_scope"], "shared_interpreter");
    assert_eq!(status["python"]["private_project_pip"]["available"], true);
    assert_eq!(
        status["python"]["private_project_pip"]["requires_project_preparation"],
        true
    );
    let python_status = environment.pilot_python_status();
    assert_eq!(python_status["pip_write"], false);
    assert_eq!(python_status["pip_write_scope"], "shared_interpreter");
    assert!(
        python_status["guidance"]
            .as_str()
            .unwrap()
            .contains("development_prepare")
    );
    assert_eq!(
        status["python"]["dependency_preparation"]["uv_available"],
        true
    );
    assert_eq!(
        status["python"]["dependency_preparation"]["network_fetch_allowed_this_turn"],
        true
    );
    assert_eq!(status["rust"]["configured"], true);
    assert_eq!(status["rust"]["rustup_available"], true);
    assert_eq!(status["rust"]["cargo_available"], true);
    assert_eq!(
        status["rust"]["installed_toolchains"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        status["rust"]["toolchain_install_network_allowed_this_turn"],
        true
    );
    assert_eq!(status["rust"]["build_verified"], false);
    let encoded = status.to_string();
    assert!(!encoded.contains(root.path().to_string_lossy().as_ref()));
    assert!(!encoded.contains("https://"));

    let offline = environment.pilot_platform_status(
        false,
        json!({"configured":true,"shared_interpreter_configured":true,"shared_interpreter_available":true,"uv_available":true,"sources":[]}),
    );
    assert_eq!(offline["network"]["allowed_this_turn"], false);
    assert_eq!(
        offline["rust"]["toolchain_install_network_allowed_this_turn"],
        false
    );
    assert_eq!(offline["rust"]["cargo_offline"], true);
    assert_eq!(
        offline["python"]["dependency_preparation"]["network_fetch_allowed_this_turn"],
        false
    );
    let unprepared = environment.pilot_platform_status(
        false,
        json!({"configured":false,"shared_interpreter_available":true,"uv_available":true}),
    );
    assert_eq!(
        unprepared["python"]["private_project_pip"]["available"],
        false
    );
    let unauthorized = ToolEnvironment::default().pilot_platform_status(
        false,
        json!({"configured":true,"shared_interpreter_available":true,"uv_available":true}),
    );
    assert_eq!(
        unauthorized["python"]["private_project_pip"]["available"],
        false
    );
}

#[tokio::test]
async fn platform_status_reports_shared_python_without_authoring_or_preparation_grants() {
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let _environment = Environment::cleared(&[
        "ANCHOR_DEPENDENCIES_PYTHON",
        "ANCHOR_DEPENDENCIES_SOURCES",
        "ANCHOR_DEPENDENCIES_UV",
        "ANCHOR_DEPENDENCIES_UV_SHA256",
        "ANCHOR_DEPENDENCIES_BUILD_ROOT",
        "ANCHOR_PILOT_PYTHON",
        "ANCHOR_PILOT_PYTHON_ENABLED",
    ]);
    let root = tempfile::tempdir().unwrap();
    let shared_python = root.path().join("shared-python/bin/python3");
    fs::create_dir_all(shared_python.parent().unwrap()).unwrap();
    fs::write(&shared_python, "metadata-only interpreter fixture").unwrap();
    fs::set_permissions(&shared_python, fs::Permissions::from_mode(0o755)).unwrap();
    unsafe {
        std::env::set_var("ANCHOR_DEPENDENCIES_PYTHON", &shared_python);
    }

    let application = crate::application::RunApplication::new(
        root.path().join("state"),
        root.path().join("catalog"),
    );
    let dependency_status = application.pilot_dependency_status();
    assert_eq!(dependency_status["configured"], false);
    assert_eq!(dependency_status["shared_interpreter_configured"], true);
    assert_eq!(dependency_status["shared_interpreter_available"], true);

    let status = ToolEnvironment::default().pilot_platform_status(false, dependency_status);
    assert_eq!(status["python"]["shared_interpreter_configured"], true);
    assert_eq!(status["python"]["shared_interpreter_available"], true);
    assert_eq!(status["python"]["authoring_granted"], false);
    assert_eq!(status["python"]["pip_write_scope"], "shared_interpreter");
    assert_eq!(status["python"]["private_project_pip"]["available"], false);
    assert_eq!(
        status["python"]["dependency_preparation"]["configured"],
        false
    );
    assert_eq!(status["mutations_performed"], false);
    assert!(
        !status
            .to_string()
            .contains(root.path().to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn pilot_python_uses_the_shared_interpreter_only_with_an_explicit_grant() {
    let Some(interpreter) = available_pilot_python() else {
        eprintln!("SKIPPED (no verification): no Python 3.8+ interpreter is available");
        return;
    };
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let _environment = Environment::cleared(&[
        "ANCHOR_PILOT_PYTHON",
        "ANCHOR_PILOT_PYTHON_ENABLED",
        "ANCHOR_DEPENDENCIES_PYTHON",
    ]);
    unsafe {
        std::env::set_var("ANCHOR_DEPENDENCIES_PYTHON", &interpreter);
    }

    let disabled = ToolEnvironment::default().with_pilot_python().unwrap();
    assert!(disabled.python.is_none());

    unsafe { std::env::set_var("ANCHOR_PILOT_PYTHON_ENABLED", "1") };
    let enabled = ToolEnvironment::default().with_pilot_python().unwrap();
    assert_eq!(
        enabled.python.as_ref().unwrap().interpreter,
        interpreter.canonicalize().unwrap()
    );
}

#[tokio::test]
async fn pilot_python_grant_requires_the_shared_path_and_rejects_legacy_configuration() {
    let _guard = crate::PROCESS_ENV_LOCK.lock().await;
    let _environment = Environment::cleared(&[
        "ANCHOR_PILOT_PYTHON",
        "ANCHOR_PILOT_PYTHON_ENABLED",
        "ANCHOR_DEPENDENCIES_PYTHON",
    ]);
    unsafe { std::env::set_var("ANCHOR_PILOT_PYTHON_ENABLED", "1") };
    let error = ToolEnvironment::default().with_pilot_python().unwrap_err();
    assert!(error.contains("ANCHOR_DEPENDENCIES_PYTHON is required"));

    unsafe { std::env::set_var("ANCHOR_PILOT_PYTHON", "/opt/old/python/bin/python3") };
    let error = ToolEnvironment::default().with_pilot_python().unwrap_err();
    assert!(error.contains("ANCHOR_PILOT_PYTHON is no longer supported"));

    unsafe {
        std::env::remove_var("ANCHOR_PILOT_PYTHON");
        std::env::set_var("ANCHOR_PILOT_PYTHON_ENABLED", "yes");
    }
    let error = ToolEnvironment::default().with_pilot_python().unwrap_err();
    assert!(error.contains("ANCHOR_PILOT_PYTHON_ENABLED must be 0 or 1"));
}

#[tokio::test]
async fn pilot_python_uses_the_configured_interpreter_and_keeps_its_prefix_read_only() {
    let Some(interpreter) = available_pilot_python() else {
        return;
    };
    let tools = ToolEnvironment::default()
        .with_pilot_python_path(&interpreter)
        .unwrap();
    let python = tools.python.as_ref().unwrap();
    assert!(tools.pilot_command_aliases().contains(&"python".to_owned()));
    assert!(
        tools
            .pilot_command_aliases()
            .contains(&"python3".to_owned())
    );
    assert!(tools.pilot_command_aliases().contains(&format!(
        "python3.{}",
        python.version.split('.').nth(1).unwrap()
    )));
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    let sandbox = BubblewrapSandbox::new(
        tools.authorize(BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&work)),
    )
    .unwrap();

    let mut direct = SandboxRequest::new(
        &work,
        [
            "python3",
            "-c",
            "import sys; print(sys.executable); print(sys.version_info[:2])",
        ],
    );
    tools.apply(&mut direct);
    tools.sandbox_for_command(&sandbox, &mut direct).unwrap();
    assert_eq!(direct.command[0], format!("{PILOT_PYTHON_BIN}/python3"));
    let result = sandbox.run(direct).await.unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert!(
        result
            .stdout
            .contains(python.interpreter.to_string_lossy().as_ref()),
        "{result:?}"
    );

    let mut shell = SandboxRequest::new(
        &work,
        [
            "sh",
            "-c",
            "python3 -c 'import sys; print(sys.executable); print(sys.version_info[:2])'",
        ],
    );
    tools.apply(&mut shell);
    tools.sandbox_for_command(&sandbox, &mut shell).unwrap();
    let result = sandbox.run(shell).await.unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert!(
        result
            .stdout
            .contains(python.interpreter.to_string_lossy().as_ref()),
        "{result:?}"
    );

    let mut readonly = SandboxRequest::new(
        &work,
        [
            "python3",
            "-c",
            "import pathlib, sys; p=pathlib.Path(sys.prefix)/'anchor-pilot-write-check';\ntry:\n p.write_text('forbidden')\n print('writable')\nexcept OSError:\n print('readonly')",
        ],
    );
    tools.apply(&mut readonly);
    tools.sandbox_for_command(&sandbox, &mut readonly).unwrap();
    let result = sandbox.run(readonly).await.unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout.trim(), "readonly", "{result:?}");
}

#[tokio::test]
async fn system_python_shell_aliases_override_another_tool_without_authorizing_system_bin() {
    let Some(interpreter) = [
        "/usr/bin/python3.12",
        "/usr/bin/python3.11",
        "/usr/bin/python3.10",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file()) else {
        eprintln!("SKIPPED (no verification): no system Python 3.8+ is available");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let other_bin = root.path().join("other/bin");
    fs::create_dir(&work).unwrap();
    fs::create_dir_all(&other_bin).unwrap();
    fs::copy("/usr/bin/false", other_bin.join("python3")).unwrap();
    let mut tools = ToolEnvironment::default();
    tools.mount_at_original_path(&other_bin).unwrap();
    tools.dirs.push(other_bin);
    tools.configure_pilot_python(&interpreter).unwrap();
    assert!(!tools.dirs.iter().any(|path| path == Path::new("/usr/bin")));
    let sandbox = BubblewrapSandbox::new(
        tools.authorize(BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&work)),
    )
    .unwrap();
    for alias in tools.pilot_command_aliases() {
        let mut request = SandboxRequest::new(
            &work,
            [
                "sh".to_owned(),
                "-c".into(),
                format!("{alias} -c 'import sys; print(sys.executable)'"),
            ],
        );
        tools.apply(&mut request);
        let result = tools
            .sandbox_for_command(&sandbox, &mut request)
            .unwrap()
            .run(request)
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0), "{alias}: {result:?}");
        assert_eq!(
            Path::new(result.stdout.trim()),
            interpreter.canonicalize().unwrap(),
            "{alias}: {result:?}"
        );
    }
    let mut forbidden = SandboxRequest::new(&work, ["/usr/bin/true"]);
    tools.apply(&mut forbidden);
    assert!(
        tools
            .sandbox_for_command(&sandbox, &mut forbidden)
            .unwrap()
            .run(forbidden)
            .await
            .is_err()
    );
    let mut tamper = SandboxRequest::new(
        &work,
        [
            "sh",
            "-c",
            "printf pollution > /tools/anchor-pilot-python/bin/python3",
        ],
    );
    tools.apply(&mut tamper);
    let result = tools
        .sandbox_for_command(&sandbox, &mut tamper)
        .unwrap()
        .run(tamper)
        .await
        .unwrap();
    assert_ne!(result.exit_code, Some(0));
}

#[tokio::test]
async fn pilot_python_is_fail_closed_when_no_interpreter_is_granted() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    let tools = ToolEnvironment::default();
    let sandbox = BubblewrapSandbox::new(
        tools.authorize(BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&work)),
    )
    .unwrap();
    let mut request = SandboxRequest::new(&work, ["python3", "--version"]);
    tools.apply(&mut request);
    let result = tools
        .sandbox_for_command(&sandbox, &mut request)
        .unwrap()
        .run(request)
        .await;
    assert!(result.is_err(), "an ungranted Python command must not run");
}

#[test]
fn a_same_named_rustup_entry_cannot_get_shared_toolchain_write_permission() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("shared/bin");
    let home = root.path().join("toolchains");
    let other = root.path().join("other/bin");
    let work = root.path().join("work");
    for directory in [&bin, &home, &other, &work] {
        fs::create_dir_all(directory).unwrap();
    }
    for directory in [&bin, &other] {
        fs::copy("/usr/bin/true", directory.join("rustup")).unwrap();
    }
    let proxy = bin.join("rustup");
    let tools = ToolEnvironment {
        mounts: vec![
            ReadOnlyInput::new(&proxy, &proxy),
            ReadOnlyInput::new(&home, &home),
        ],
        dirs: vec![bin.clone(), other.clone()],
        imports: vec![],
        devices: vec![],
        plugin_environments: BTreeMap::new(),
        rust: Some(RustDevelopment {
            proxy: proxy.clone(),
            home: home.clone(),
            toolchain: None,
        }),
        python: None,
        mcp_python: None,
        _launchers: vec![],
    };
    let sandbox = BubblewrapSandbox::new(
        tools.authorize(
            BubblewrapPolicy::new("bwrap", ["rustup"])
                .authorize_workspace_root(&work)
                .allow_network(),
        ),
    )
    .unwrap();
    let mut forged = SandboxRequest::new(
        &work,
        [
            other.join("rustup").to_string_lossy().into_owned(),
            "toolchain".into(),
            "install".into(),
            "stable".into(),
        ],
    );
    forged.network = NetworkPolicy::Enabled;
    tools.apply(&mut forged);
    assert!(
        tools
            .sandbox_for_command(&sandbox, &mut forged)
            .unwrap_err()
            .contains("shared Rustup proxy")
    );
    assert!(forged.writable_inputs.is_empty());
    let mut valid = SandboxRequest::new(&work, ["rustup", "toolchain", "install", "stable"]);
    valid.network = NetworkPolicy::Enabled;
    tools.apply(&mut valid);
    tools.sandbox_for_command(&sandbox, &mut valid).unwrap();
    assert_eq!(Path::new(&valid.command[0]), proxy);
    assert!(
        valid
            .writable_inputs
            .iter()
            .any(|mount| mount.source == home)
    );
    assert!(valid.command.iter().any(|arg| arg == "--no-self-update"));
    let mut offline = SandboxRequest::new(&work, ["rustup", "toolchain", "install", "stable"]);
    tools.apply(&mut offline);
    assert!(
        tools
            .sandbox_for_command(&sandbox, &mut offline)
            .unwrap_err()
            .contains("network authorization")
    );
}

#[test]
fn plugin_only_library_needs_no_tool_registrations() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("plugins")).unwrap();
    assert!(
        ToolEnvironment::load(root.path())
            .unwrap()
            .mounts
            .is_empty()
    );
    assert!(ToolEnvironment::load(&root.path().join("missing")).is_err());
    fs::write(root.path().join("tools"), "not a directory").unwrap();
    assert!(ToolEnvironment::load(root.path()).is_err());
}

fn fixture(root: &Path) -> PathBuf {
    let library = root.join("library");
    let tool = library.join("tools/sample");
    let environment = root.join("environment");
    let imports = root.join("imports");
    fs::create_dir_all(&tool).unwrap();
    fs::create_dir_all(environment.join("bin")).unwrap();
    fs::create_dir_all(&imports).unwrap();
    // Two symlinks, including a relative one, exercise ordinary installation aliases.
    std::os::unix::fs::symlink("bash5", environment.join("bin/bash")).unwrap();
    std::os::unix::fs::symlink(
        fs::canonicalize("/usr/bin/bash").unwrap(),
        environment.join("bin/bash5"),
    )
    .unwrap();
    let entry = environment.join("bin/sample");
    fs::write(
        &entry,
        format!(
            "#!{}/bin/bash\n. {}/provided.sh\nprintf '%s\\n' \"$value\"\n",
            environment.display(),
            imports.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        imports.join("provided.sh"),
        "value='external-tool-result'\n",
    )
    .unwrap();
    fs::write(
        tool.join("tool.json"),
        json!({"entrypoint": entry, "environment": environment, "imports": [imports]}).to_string(),
    )
    .unwrap();
    library
}

#[tokio::test]
async fn existing_tool_manifest_runs_shell_with_imports_and_readonly_dependencies() {
    let root = tempfile::tempdir().unwrap();
    let library = fixture(root.path());
    // A deployed environment may be addressed through an operator-managed alias.
    let alias = root.path().join("environment-current");
    std::os::unix::fs::symlink(root.path().join("environment"), &alias).unwrap();
    let manifest = library.join("tools/sample/tool.json");
    let mut spec: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    spec["environment"] = json!(alias);
    spec["entrypoint"] = json!(alias.join("bin/sample"));
    fs::write(&manifest, spec.to_string()).unwrap();
    let tools = ToolEnvironment::load(&library).unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let sandbox = BubblewrapSandbox::new(
        tools
            .authorize(BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&workspace)),
    )
    .unwrap();
    // Direct mounted entrypoints need no interpreter-specific Runtime code.
    let mut request = SandboxRequest::new(&workspace, ["/tools/sample/run"]);
    tools.apply(&mut request);
    let result = sandbox.run(request).await.unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout.trim(), "external-tool-result");
    // Explicit canonical interpreter paths remain valid when the tool manifest
    // names the installation through a symlink alias.
    let mut request = SandboxRequest::new(
        &workspace,
        [
            root.path()
                .join("environment/bin/bash")
                .to_string_lossy()
                .into_owned(),
            "-c".into(),
            format!(
                ". {}/imports/provided.sh; printf '%s\\n' \"$value\"",
                root.path().display()
            ),
        ],
    );
    tools.apply(&mut request);
    let result = sandbox.run(request).await.unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout.trim(), "external-tool-result");
    // The original executable basename is also found in the supplied environment.
    let mut request = SandboxRequest::new(&workspace, ["sh", "-c", "sample > result.txt"]);
    tools.apply(&mut request);
    assert_eq!(sandbox.run(request).await.unwrap().exit_code, Some(0));
    assert_eq!(
        fs::read_to_string(workspace.join("result.txt"))
            .unwrap()
            .trim(),
        "external-tool-result"
    );
    let mut request = SandboxRequest::new(
        &workspace,
        vec![
            "sh".into(),
            "-c".into(),
            format!(
                "echo changed > {}/imports/provided.sh",
                root.path().display()
            ),
        ],
    );
    tools.apply(&mut request);
    assert_ne!(sandbox.run(request).await.unwrap().exit_code, Some(0));
    assert_eq!(
        fs::read_to_string(root.path().join("imports/provided.sh")).unwrap(),
        "value='external-tool-result'\n"
    );
}

#[tokio::test]
async fn plugin_scoped_tool_imports_never_reach_node_shell_or_other_plugins() {
    let root = tempfile::tempdir().unwrap();
    let library = fixture(root.path());
    let manifest = library.join("tools/sample/tool.json");
    let mut spec: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    spec["plugin"] = json!("git");
    fs::write(&manifest, spec.to_string()).unwrap();
    let tools = ToolEnvironment::load(&library).unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let base = BubblewrapSandbox::new(
        tools
            .authorize(BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&workspace)),
    )
    .unwrap();
    let imported = root.path().join("imports/provided.sh");
    for environment in [&tools, &tools.for_plugin("other", true)] {
        let mut request = SandboxRequest::new(
            &workspace,
            ["sh", "-c", &format!("test ! -e {}", imported.display())],
        );
        environment.apply(&mut request);
        assert_eq!(base.run(request).await.unwrap().exit_code, Some(0));
        let mut forged = SandboxRequest::new(&workspace, ["sh", "-c", "true"]);
        forged.readonly_inputs.push(ReadOnlyInput::new(
            imported.parent().unwrap(),
            imported.parent().unwrap(),
        ));
        assert!(base.run(forged).await.is_err());
    }
    for include_global in [true, false] {
        let environment = tools.for_plugin("git", include_global);
        let sandbox = environment.grant_sandbox(&base).unwrap();
        let mut request = SandboxRequest::new(
            &workspace,
            [
                "sh",
                "-c",
                &format!(". {}; printf '%s' \"$value\"", imported.display()),
            ],
        );
        environment.apply(&mut request);
        let result = sandbox.run(request).await.unwrap();
        assert_eq!(result.exit_code, Some(0), "{result:?}");
        assert_eq!(result.stdout, "external-tool-result");
    }
}

#[test]
fn tool_manifests_grant_exact_device_nodes_and_nothing_broader() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    let directory = library.join("tools/gpu-tool");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(directory.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    let manifest = directory.join("tool.json");

    // A registered device node reaches both the node sandbox and the request.
    fs::write(
        &manifest,
        json!({"entrypoint": "run.sh", "devices": ["/dev/null"]}).to_string(),
    )
    .unwrap();
    let tools = ToolEnvironment::load(&library).unwrap();
    assert_eq!(tools.devices, vec![PathBuf::from("/dev/null")]);
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let mut request = SandboxRequest::new(&workspace, ["true"]);
    tools.apply(&mut request);
    assert_eq!(request.devices, vec![PathBuf::from("/dev/null")]);
    assert_eq!(
        tools
            .authorize(BubblewrapPolicy::new("bwrap", ["sh"]))
            .device_grants,
        vec![PathBuf::from("/dev/null")]
    );

    // Broad, non-device, relative and directory entries are rejected at load.
    for invalid in [
        "/dev",
        "/dev/..",
        "/etc/passwd",
        "/tmp",
        "dev/null",
        "/dev/shm",
    ] {
        fs::write(
            &manifest,
            json!({"entrypoint": "run.sh", "devices": [invalid]}).to_string(),
        )
        .unwrap();
        assert!(ToolEnvironment::load(&library).is_err(), "{invalid}");
    }
}

#[test]
fn malformed_tool_grants_are_rejected_and_mcp_explicit_environment_is_preserved() {
    let root = tempfile::tempdir().unwrap();
    let library = fixture(root.path());
    let manifest = library.join("tools/sample/tool.json");
    let original = fs::read_to_string(&manifest).unwrap();
    let tools = ToolEnvironment::load(&library).unwrap();
    let mut request = SandboxRequest::new(root.path(), ["sh"]);
    request
        .environment
        .push(SandboxEnvironment::new("PYTHONPATH", "/plugins/custom"));
    request.network = NetworkPolicy::Disabled;
    tools.apply(&mut request);
    assert_eq!(request.environment.len(), 1);
    assert_eq!(request.environment[0].value(), "/plugins/custom");
    assert_eq!(request.network, NetworkPolicy::Disabled);
    for invalid in [
        json!({"imports": ["/"]}),
        json!({"imports": ["/root"]}),
        json!({"imports": ["/etc"]}),
        json!({"imports": ["relative"]}),
        json!({"environment": "/"}),
        json!({"entrypoint": "../missing"}),
    ] {
        let mut spec: serde_json::Value = serde_json::from_str(&original).unwrap();
        spec.as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        fs::write(&manifest, spec.to_string()).unwrap();
        assert!(ToolEnvironment::load(&library).is_err(), "{invalid}");
    }
}
