use super::*;
use crate::plugin_library::PluginLibrary;
use anchor_graph_host::{PluginCatalog, plugin_view_path, store_root};
use anchor_platform_session::{CreateSession, SessionStore};
use anchor_sandbox_bwrap::BubblewrapPolicy;
use std::{fs, sync::atomic::AtomicBool, time::Duration};

const FIELD: &str = "ANCHOR_EMPTY_MCP_PRIVATE_SECRET";
const SERVER: &str = r#"import hashlib, json, os, subprocess, sys
from pathlib import Path

def readonly(path):
    try:
        with Path(path).open('r+b'): pass
        return False
    except OSError: return True

for line in sys.stdin:
    message = json.loads(line)
    if 'id' not in message: continue
    method = message.get('method')
    if method == 'initialize':
        result = {'protocolVersion':'2025-03-26','capabilities':{'tools':{}},'serverInfo':{'name':'shared-python','version':'1'}}
    elif method == 'tools/list':
        result = {'tools':[{'name':'probe','description':'read-only runtime observation','inputSchema':{'type':'object'}}]}
    elif method == 'tools/call':
        secret = os.environ.get('ANCHOR_EMPTY_MCP_PRIVATE_SECRET', '')
        child = subprocess.run(['python3','-c','import sys; print(sys.executable)'],capture_output=True,text=True,check=True)
        marker = Path(sys.prefix) / ('anchor-mcp-write-' + sys.argv[1])
        try:
            marker.write_text('forbidden')
            marker.unlink()
            prefix_readonly = False
        except OSError: prefix_readonly = True
        report = {'executable':sys.executable,'prefix':sys.prefix,'child_executable':child.stdout.strip(),
                  'interpreter_readonly':readonly(sys.executable),'prefix_readonly':prefix_readonly,
                  'launcher_readonly':readonly('/tools/anchor-pilot-python/bin/python3'),
                  'host_private_visible':Path(sys.argv[2]).exists(),
                  'credential_configured':bool(secret),'credential_sha256':hashlib.sha256(secret.encode()).hexdigest()}
        result = {'content':[],'structuredContent':report}
    else: result = {}
    print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}),flush=True)
"#;

fn interpreter() -> Option<PathBuf> {
    std::env::var_os("ANCHOR_TEST_PILOT_PYTHON")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            PathBuf::from(
                "/home/mansteinl/.local/share/uv/python/cpython-3.12.13-linux-x86_64-gnu/bin/python3",
            ),
            PathBuf::from("/usr/bin/python3.12"),
            PathBuf::from("/usr/bin/python3.11"),
            PathBuf::from("/usr/bin/python3.10"),
        ])
        .find(|path| path.is_file())
}

fn environment(python: &Path, enabled: bool) -> crate::tool_environment::ToolEnvironment {
    crate::tool_environment::ToolEnvironment::default()
        .with_runtime_snapshot(&crate::runtime_configuration::RuntimeSnapshot {
            python: Some(python.to_owned()),
            python_enabled: enabled,
            rust: None,
            pixi: None,
            provisioner: None,
        })
        .unwrap()
}

fn sandbox(workspace: &Path) -> BubblewrapSandbox {
    // Python is deliberately absent from the host command allowlist.
    BubblewrapSandbox::new(
        BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(workspace),
    )
    .unwrap()
}

async fn bind(
    config: &McpToolConfig,
    bindings: &[PluginBinding],
    views: &BTreeMap<String, crate::plugin_view::PreparedPlugin>,
    sandbox: &BubblewrapSandbox,
    workspace: &Path,
) -> Result<LiveMcpTools, String> {
    tokio::time::timeout(
        Duration::from_secs(5),
        config.bind_with_node_inputs(
            bindings,
            false,
            SandboxLaunch {
                sandbox,
                workspace,
                readonly_inputs: &[],
                plugin_views: views,
            },
        ),
    )
    .await
    .expect("stdlib MCP handshake must complete")
}

#[tokio::test]
async fn empty_dependency_mcp_uses_shared_python_and_private_credentials_without_authoring_grant() {
    let _gate = crate::PROCESS_ENV_LOCK.lock().await;
    let Some(python) = interpreter() else {
        eprintln!("SKIPPED (no verification): no Python interpreter is available");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    let source = root.path().join("source");
    let workspace = root.path().join("workspace");
    let data = root.path().join("state");
    for directory in [&source, &workspace, &data.join("platform")] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(data.join("private-host-file"), "private").unwrap();
    let marker = root.path().file_name().unwrap().to_string_lossy();
    fs::write(source.join("server.py"), SERVER).unwrap();
    fs::write(
        source.join("plugin.json"),
        json!({"name":"Empty dependency MCP","dependencies":{"python":[]},"mcpServers":{"service":{
            "command":"python3","args":["server.py",marker,data.join("private-host-file")],"env_vars":[FIELD]
        }}})
        .to_string(),
    )
    .unwrap();
    PluginLibrary::new(&library)
        .install_directory("stdlib", &source, false)
        .unwrap();
    let binding = Catalog::new(&library)
        .resolve(&["stdlib".into()])
        .unwrap()
        .remove(0);
    let sessions = SessionStore::open(data.join("platform/sessions.sqlite")).unwrap();
    let session = sessions.create("owner", CreateSession::default()).unwrap();
    let (turn, _) = sessions
        .create_turn("owner", &session.id, "configure", Some("fixture"))
        .unwrap();
    let request = crate::credential_store::request(
        &data,
        &library,
        &sessions,
        "owner",
        &session,
        &turn.id,
        "stdlib",
        "service",
        FIELD,
        "fixture credential",
    )
    .unwrap();
    let secret = format!("private-fixture-{marker}");
    crate::credential_store::submit(
        &data,
        &library,
        &sessions,
        "owner",
        &session,
        &request.id,
        &secret,
    )
    .unwrap();
    let prepared = crate::plugin_library::tools::prepare_plugins_for_owner_async(
        vec![binding],
        Some(library.clone()),
        PluginToolHost::new(Vec::<String>::new()),
        Arc::new(AtomicBool::new(false)),
        Some((data, crate::credential_store::binding_owner("owner"))),
    )
    .await
    .unwrap();
    assert!(prepared.views["stdlib"].view().is_none());
    let policy = sandbox(&workspace);
    let mut ungranted = prepared.mcp.clone();
    ungranted.environment = Default::default();
    let error = bind(
        &ungranted,
        &prepared.bindings,
        &prepared.views,
        &policy,
        &workspace,
    )
    .await
    .err()
    .expect("ungranted Python must refuse startup");
    assert!(error.contains("command `python3` is not authorized"));

    for enabled in [false, true] {
        let mut config = prepared.mcp.clone();
        config.environment = environment(&python, enabled);
        if !enabled {
            assert!(config.environment.pilot_command_aliases().is_empty());
            let mut authoring = SandboxRequest::new(&workspace, ["python3", "--version"]);
            config.environment.apply(&mut authoring);
            let authoring_sandbox = config
                .environment
                .sandbox_for_command(&policy, &mut authoring)
                .unwrap();
            assert!(authoring_sandbox.isolated_command(authoring).is_err());
        }
        let mut forged = config.clone();
        let decoy = root.path().join("forged/bin/python3");
        fs::create_dir_all(decoy.parent().unwrap()).unwrap();
        fs::copy("/usr/bin/true", &decoy).unwrap();
        forged.servers.get_mut("stdlib-service").unwrap().config["command"] = json!(decoy);
        let error = bind(
            &forged,
            &prepared.bindings,
            &prepared.views,
            &policy,
            &workspace,
        )
        .await
        .err()
        .expect("same-name ungranted interpreter must refuse startup");
        assert!(error.contains("sandbox launch rejected"));
        assert!(error.contains("command must select"));
        drop(forged);
        let tools = bind(
            &config,
            &prepared.bindings,
            &prepared.views,
            &policy,
            &workspace,
        )
        .await
        .unwrap();
        // The server invokes the launcher again during the business call.
        // Keeping it alive cannot depend on this temporary binding config.
        drop(config);
        let result = tools.call("stdlib-service_probe", json!({})).await.unwrap();
        let report = result.iter().find_map(ToolResultContent::as_json).unwrap();
        assert_eq!(report["executable"], json!(python.canonicalize().unwrap()));
        assert_eq!(report["child_executable"], report["executable"]);
        assert_eq!(report["interpreter_readonly"], true);
        assert_eq!(report["prefix_readonly"], true);
        assert_eq!(report["launcher_readonly"], true);
        assert_eq!(report["host_private_visible"], false);
        assert_eq!(report["credential_configured"], true);
        assert_eq!(
            report["credential_sha256"],
            format!("{:x}", Sha256::digest(secret.as_bytes()))
        );
        tools.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn dependency_view_mcp_keeps_its_own_interpreter_with_shared_python_authorized() {
    let _gate = crate::PROCESS_ENV_LOCK.lock().await;
    let Some(python) = interpreter() else {
        eprintln!("SKIPPED (no verification): no Python interpreter is available");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let plugin = root.path().join("plugins/viewed");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&plugin).unwrap();
    fs::create_dir(&workspace).unwrap();
    fs::write(plugin.join("server.py"), SERVER).unwrap();
    fs::write(
        plugin.join("plugin.json"),
        json!({"name":"View MCP","dependencies":{"python":["idna==3.6"]},"mcpServers":{"service":{
            "command":"${ANCHOR_PLUGIN_VIEW}/bin/python3","args":["server.py","view","/ungranted/private-host-file"]
        }}})
        .to_string(),
    )
    .unwrap();
    let catalog = Catalog::new(root.path());
    let definition = catalog.definition("viewed").unwrap();
    let view = plugin_view_path(
        root.path(),
        "viewed",
        &definition.dependencies.declaration_digest(),
    );
    let store = store_root(root.path());
    fs::create_dir_all(&store).unwrap();
    let output = std::process::Command::new(&python)
        .args(["-m", "venv", "--without-pip"])
        .arg(&view)
        .output()
        .unwrap();
    assert!(output.status.success(), "venv fixture must be created");
    let prepared = crate::plugin_library::tools::prepare_plugins_async(
        vec![definition.binding()],
        Some(root.path().to_owned()),
        PluginToolHost::new(Vec::<String>::new()),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    let mut config = prepared.mcp;
    config.environment = environment(&python, true);
    let tools = bind(
        &config,
        &prepared.bindings,
        &prepared.views,
        &sandbox(&workspace),
        &workspace,
    )
    .await
    .unwrap();
    let result = tools.call("viewed-service_probe", json!({})).await.unwrap();
    let report = result.iter().find_map(ToolResultContent::as_json).unwrap();
    assert_eq!(report["executable"], json!(view.join("bin/python3")));
    assert_eq!(report["prefix"], json!(view));
    assert_eq!(report["child_executable"], report["executable"]);
    assert_eq!(report["prefix_readonly"], true);
    assert_eq!(report["credential_configured"], false);
    tools.shutdown().await.unwrap();
}
