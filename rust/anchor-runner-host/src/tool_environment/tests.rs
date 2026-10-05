use super::*;
use anchor_runtime_rig::{NetworkPolicy, SandboxPort};
use anchor_sandbox_bwrap::BubblewrapSandbox;
use serde_json::json;

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
    // Two symlinks, including a relative one, exercise ordinary venv layout.
    std::os::unix::fs::symlink("python3", environment.join("bin/python")).unwrap();
    std::os::unix::fs::symlink("/usr/bin/python3", environment.join("bin/python3")).unwrap();
    let entry = environment.join("bin/sample");
    fs::write(
        &entry,
        format!(
            "#!{}/bin/python\nfrom provided import value\nprint(value)\n",
            environment.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        imports.join("provided.py"),
        "value = 'external-tool-result'\n",
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
async fn existing_tool_manifest_runs_python_with_imports_and_readonly_dependencies() {
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
                .join("environment/bin/python")
                .to_string_lossy()
                .into_owned(),
            "-c".into(),
            "from provided import value; print(value)".into(),
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
                "echo changed > {}/imports/provided.py",
                root.path().display()
            ),
        ],
    );
    tools.apply(&mut request);
    assert_ne!(sandbox.run(request).await.unwrap().exit_code, Some(0));
    assert_eq!(
        fs::read_to_string(root.path().join("imports/provided.py")).unwrap(),
        "value = 'external-tool-result'\n"
    );
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
