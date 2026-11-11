use crate::FilePluginCatalog;
use std::fs;

pub(crate) fn system_plugin(root: &std::path::Path, plugin: crate::SystemPlugin) {
    let directory = root.join("plugins").join(plugin.id());
    for (path, bytes) in plugin.public_files() {
        let target = directory.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    for path in plugin.executables() {
        use std::os::unix::fs::PermissionsExt;
        let target = directory.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A Plugin that declares Python dependencies and launches its server on the
/// interpreter of the view that declaration maps to.
pub(crate) fn catalog_with_dependency_plugin() -> (tempfile::TempDir, FilePluginCatalog) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/probe");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("plugin.json"),
        r#"{"name":"Probe","dependencies":{"python":["idna==3.6"]},"mcpServers":{"probe":{"command":"${ANCHOR_PLUGIN_VIEW}/bin/python","args":["server.py"]}}}"#,
    )
    .unwrap();
    let catalog = FilePluginCatalog::new(tmp.path());
    (tmp, catalog)
}

pub(crate) fn catalog_with_plugin() -> (tempfile::TempDir, FilePluginCatalog) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/demo");
    fs::create_dir_all(dir.join("skills/example")).unwrap();
    fs::write(dir.join("plugin.json"), r#"{"name":"Demo","mcpServers":{"inline":{"command":"run","env":{"TOKEN":"top-secret"}},"disabled":{"enabled":false,"url":"https://example.test/mcp"}}}"#).unwrap();
    fs::write(
        dir.join(".mcp.json"),
        r#"{"mcpServers":{"config":{"url":"https://example.test/mcp","headers":{"Authorization":"secret-value"}}}}"#,
    )
    .unwrap();
    fs::write(dir.join("skills/example/SKILL.md"), "hello").unwrap();
    let catalog = FilePluginCatalog::new(tmp.path());
    (tmp, catalog)
}
