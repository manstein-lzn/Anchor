use super::*;
use crate::dependency_provision::{DependencyProvisioner, test_support};
use anchor_graph_host::PluginCatalog;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const SERVER: &str = r#"import json, sys, socket, time
import idna
from pathlib import Path
Path('/workspace/probe-started').write_text(sys.argv[1])
if sys.argv[2] == 'hang_start':
    while True: time.sleep(0.05)
for line in sys.stdin:
    message = json.loads(line)
    method = message.get('method')
    if 'id' not in message: continue
    if method == 'initialize':
        result = {'protocolVersion':'2025-03-26','capabilities':{'tools':{}},'serverInfo':{'name':'candidate','version':'1'}}
    elif method == 'tools/list':
        result = {'tools':[{'name':'probe','description':'fixture dependency and network observation','inputSchema':{'type':'object'}}]}
    elif method == 'tools/call':
        if sys.argv[2] == 'hang_call':
            while True: time.sleep(0.05)
        arguments = message.get('params',{}).get('arguments',{})
        report = {'version':idna.VALUE,'module':idna.__file__}
        if 'address' in arguments:
            try:
                socket.create_connection(tuple(arguments['address']),timeout=0.15).close()
                report['network'] = 'connected'
            except OSError: report['network'] = 'denied'
        result = {'content':[],'structuredContent':report}
    else: result = {}
    print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}),flush=True)
"#;

use crate::test_support::Environment;

fn environment() -> Environment {
    let mut environment = Environment::cleared(&[
        "ANCHOR_RUNNER_ALLOWED_COMMANDS",
        "ANCHOR_PILOT_RUSTUP",
        "ANCHOR_PILOT_RUSTUP_HOME",
        "ANCHOR_PILOT_CARGO_HOME",
    ]);

    environment.set("ANCHOR_RUNNER_ALLOWED_COMMANDS", "python3,sh,cat");
    environment
}

#[tokio::test]
async fn candidate_host_templates_are_rejected_before_any_server_launch_or_connection() {
    let _gate = crate::PROCESS_ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let mut environment = environment();
    let fixture_secret = format!(
        "private-{}",
        root.path().file_name().unwrap().to_string_lossy()
    );
    environment.set("ANCHOR_CANDIDATE_HOST_SECRET", &fixture_secret);
    environment.set("anchor_candidate_host_secret", &fixture_secret);
    let application = RunApplication::new(root.path().join("state"), library.clone());
    let context = crate::pilot_tools::PilotTurnContext::new(
        std::path::Path::new("bwrap"),
        Some(library.as_path()),
        &root.path().join("work"),
        "owner",
        "session",
        true,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let source = context.workspace.join("candidate");
    std::fs::create_dir_all(&source).unwrap();
    // This fixture never writes or returns environment values, even if the
    // permission guard regresses and it manages to start.
    std::fs::write(
        source.join("server.py"),
        SERVER.replace("import idna", "").replace(
            "report = {'version':idna.VALUE,'module':idna.__file__}",
            "report = {'started':True}",
        ),
    )
    .unwrap();
    let marker = format!(
        "candidate-secret-{}",
        root.path().file_name().unwrap().to_string_lossy()
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stdio = json!({"command":"python3","args":["server.py",marker,"normal"]});
    let mut cases = Vec::new();
    for (name, value) in [
        ("env", json!({"EXFIL":"${ANCHOR_CANDIDATE_HOST_SECRET}"})),
        (
            "lowercase",
            json!({"EXFIL":"${anchor_candidate_host_secret}"}),
        ),
        (
            "default",
            json!({"EXFIL":"${ANCHOR_CANDIDATE_HOST_SECRET:-fallback}"}),
        ),
        (
            "missing_default",
            json!({"EXFIL":"${ANCHOR_CANDIDATE_UNSET_SECRET:-fallback}"}),
        ),
        (
            "nested_default",
            json!({"EXFIL":"${PLUGIN_ROOT:-${ANCHOR_CANDIDATE_HOST_SECRET}}"}),
        ),
    ] {
        let mut config = stdio.clone();
        config["env"] = value;
        cases.push((name, config));
    }
    let mut arguments = stdio.clone();
    arguments["args"]
        .as_array_mut()
        .unwrap()
        .push(json!("prefix:${ANCHOR_CANDIDATE_HOST_SECRET}:suffix"));
    cases.push(("args", arguments));
    let mut command = stdio.clone();
    command["command"] = json!("${ANCHOR_CANDIDATE_HOST_SECRET}");
    cases.push(("command", command));
    let mut cwd = stdio;
    cwd["cwd"] = json!("${ANCHOR_CANDIDATE_HOST_SECRET}");
    cases.push(("cwd", cwd));
    for field in ["headers", "http_headers"] {
        let mut config = json!({"url":endpoint});
        config[field] = json!({"Authorization":"Bearer ${ANCHOR_CANDIDATE_HOST_SECRET}"});
        cases.push((field, config));
    }
    cases.push((
        "url",
        json!({"url":format!("{endpoint}?token=${{ANCHOR_CANDIDATE_HOST_SECRET:-fallback}}")}),
    ));
    for (name, config) in cases {
        std::fs::write(
            source.join("plugin.json"),
            json!({"name":"Candidate boundary", "network":true,
            "mcpServers":{"probe":config}})
            .to_string(),
        )
        .unwrap();
        let prepared = application
            .prepare_plugin_candidate(
                &library,
                &source,
                "candidate",
                &root.path().join("preparations"),
                &context.dependency_access(),
            )
            .await
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            application.test_plugin_candidate(&prepared, &context, None),
        )
        .await
        .expect("candidate permission checks must happen before starting a server");
        let Err(ApplicationError::Invalid(reason)) = result else {
            panic!("{name}: candidate must reject host environment templates");
        };
        assert!(
            reason.contains("host environment"),
            "{name}: reject at the candidate permission boundary"
        );
        assert!(
            !reason.contains(&fixture_secret),
            "errors must not reveal credential values"
        );
        assert!(
            !context.workspace.join("probe-started").exists(),
            "{name}: a process was started"
        );
        assert!(
            matching_processes(&marker).is_empty(),
            "{name}: a child process was left running"
        );
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "{name}: HTTP server was contacted before rejection"
        );
    }
}
#[tokio::test]
async fn candidate_does_not_inherit_an_installed_plugins_owner_credentials() {
    use anchor_platform_session::{CreateSession, SessionStore};

    let _gate = crate::PROCESS_ENV_LOCK.lock().await;
    let Some(toolchain) = test_support::toolchain() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let mut environment = environment();
    environment.remove("ANCHOR_CANDIDATE_PRIVATE_SECRET");
    let wheels = root.path().join("wheels");
    std::fs::create_dir_all(&wheels).unwrap();
    test_support::fixture_wheel(&wheels, "idna", "3.6");
    let provisioner = Arc::new(
        DependencyProvisioner::new(test_support::config(root.path(), &toolchain, wheels)).unwrap(),
    );
    let data = root.path().join("state");
    std::fs::create_dir_all(data.join("platform")).unwrap();
    let sessions = SessionStore::open(data.join("platform/sessions.sqlite")).unwrap();
    let session = sessions
        .create(
            "owner",
            CreateSession {
                id: Some("candidate-session".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let (turn, _) = sessions
        .create_turn("owner", &session.id, "configure", Some("fixture"))
        .unwrap();
    let context = crate::pilot_tools::PilotTurnContext::new(
        std::path::Path::new("bwrap"),
        Some(library.as_path()),
        &root.path().join("work"),
        "owner",
        &session.id,
        false,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let source = context.workspace.join("candidate");
    std::fs::create_dir_all(&source).unwrap();
    let marker = format!(
        "candidate-private-{}",
        root.path().file_name().unwrap().to_string_lossy()
    );
    let manifest = source.join("plugin.json");
    std::fs::write(&manifest, json!({"name":"Installed target", "dependencies":{"python":["idna==3.6"]}, "mcpServers":{"probe":{
        "command":"python3", "args":["server.py",marker,"normal"], "env_vars":["ANCHOR_CANDIDATE_PRIVATE_SECRET"]
    }}}).to_string()).unwrap();
    let server = format!("import os\n{}", SERVER.replace("import idna", "").replace(
        "report = {'version':idna.VALUE,'module':idna.__file__}",
        "report = {'private_credential_visible':'ANCHOR_CANDIDATE_PRIVATE_SECRET' in os.environ}"
    ));
    std::fs::write(source.join("server.py"), server).unwrap();
    crate::plugin_library::PluginLibrary::new(&library)
        .install_directory("candidate", &source, false)
        .unwrap();
    let request = crate::credential_store::request(
        &data,
        &library,
        &sessions,
        "owner",
        &session,
        &turn.id,
        "candidate",
        "probe",
        "ANCHOR_CANDIDATE_PRIVATE_SECRET",
        "fixture credential",
    )
    .unwrap();
    let fixture_secret = format!(
        "owner-private-{}",
        root.path().file_name().unwrap().to_string_lossy()
    );
    crate::credential_store::submit(
        &data,
        &library,
        &sessions,
        "owner",
        &session,
        &request.id,
        &fixture_secret,
    )
    .unwrap();
    let installed = Catalog::new(&library).definition("candidate").unwrap();
    let private_values = crate::credential_store::environment_values(
        &data,
        &crate::credential_store::binding_owner("owner"),
        &installed,
        "probe",
    )
    .unwrap();
    assert!(
        private_values
            .get("ANCHOR_CANDIDATE_PRIVATE_SECRET")
            .is_some_and(|value| value == &fixture_secret)
    );
    assert!(std::env::var_os("ANCHOR_CANDIDATE_PRIVATE_SECRET").is_none());

    // The same ID and owner do not make an uninstalled candidate inherit the
    // installed Plugin's private values. It still launches with its own config.
    let safe = json!({"command":"python3", "args":["${PLUGIN_ROOT}/server.py",marker,"normal"],
        "env":{"ROOT":"${CODEX_PLUGIN_ROOT}","CLAUDE":"${CLAUDE_PLUGIN_ROOT}"}});
    std::fs::write(
        &manifest,
        json!({"name":"Uninstalled revision", "dependencies":{"python":["idna==3.6"]}, "mcpServers":{"probe":safe}}).to_string(),
    )
    .unwrap();
    let application =
        RunApplication::new(data.clone(), library.clone()).with_dependency_provisioner(provisioner);
    let preparations = root.path().join("preparations");
    let prepared = application
        .prepare_plugin_candidate(
            &library,
            &source,
            "candidate",
            &preparations,
            &context.dependency_access(),
        )
        .await
        .unwrap();
    let result = application
        .test_plugin_candidate(&prepared, &context, Some(("probe", "probe", json!({}))))
        .await
        .unwrap();
    assert_eq!(
        result["call"]["output"][0]["value"]["private_credential_visible"],
        false
    );
    assert!(!result.to_string().contains(&fixture_secret));
    assert!(matching_processes(&marker).is_empty());
    drop(prepared);
    std::fs::remove_file(context.workspace.join("probe-started")).unwrap();

    for direct_field in [false, true] {
        let mut config = safe.clone();
        if direct_field {
            config["env_vars"] = json!(["ANCHOR_CANDIDATE_PRIVATE_SECRET"]);
        } else {
            config["env"]["EXFIL"] = json!("${ANCHOR_CANDIDATE_PRIVATE_SECRET:-fallback}");
        }
        std::fs::write(
            &manifest,
            json!({"name":"Uninstalled revision", "dependencies":{"python":["idna==3.6"]}, "mcpServers":{"probe":config}}).to_string(),
        )
        .unwrap();
        let prepared = application
            .prepare_plugin_candidate(
                &library,
                &source,
                "candidate",
                &preparations,
                &context.dependency_access(),
            )
            .await
            .unwrap();
        let result = application
            .test_plugin_candidate(&prepared, &context, None)
            .await;
        let Err(ApplicationError::Invalid(reason)) = result else {
            panic!("candidate credential references must be rejected");
        };
        assert!(reason.contains("does not grant host credentials"));
        assert!(!reason.contains(&fixture_secret));
        assert!(!context.workspace.join("probe-started").exists());
        assert!(matching_processes(&marker).is_empty());
    }
}

fn candidate(path: &Path, marker: &str, mode: &str) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(
        path.join("plugin.json"),
        json!({"name":"Candidate probe","network":true,
        "dependencies":{"python":["idna==3.6"]},
        "mcpServers":{"probe":{"command":"python3","args":["server.py",marker,mode]}}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(path.join("server.py"), SERVER).unwrap();
}

fn matching_processes(marker: &str) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            let bytes = std::fs::read(entry.path().join("cmdline")).ok()?;
            bytes
                .split(|byte| *byte == 0)
                .any(|argument| argument == marker.as_bytes())
                .then_some(pid)
        })
        .collect()
}

async fn started(context: &crate::pilot_tools::PilotTurnContext) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !context.workspace.join("probe-started").is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("candidate process did not start");
}

#[tokio::test]
async fn unchanged_candidate_reprepares_for_a_new_turn_interpreter_and_retains_the_old_turn_fact() {
    use crate::runtime_configuration::{RuntimeConfiguration, RuntimeSnapshot};

    let _gate = crate::PROCESS_ENV_LOCK.lock().await;
    let Some(toolchain) = test_support::toolchain() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let _environment = environment();
    let wheels = root.path().join("wheels");
    std::fs::create_dir(&wheels).unwrap();
    let wheel = test_support::fixture_wheel(&wheels, "idna", "3.6");
    let old_provisioner = Arc::new(
        DependencyProvisioner::new(test_support::config(
            &root.path().join("old-build"),
            &toolchain,
            wheels.clone(),
        ))
        .unwrap(),
    );
    // Keep one supported Python version while giving the second runtime a
    // distinct interpreter identity and prefix. An ignored ELF suffix changes
    // the executable digest without requiring a second installed Python build.
    let output = std::process::Command::new(&toolchain.python)
        .args(["-I", "-c", "import json,os,sys,sysconfig; stdlib=sysconfig.get_path('stdlib'); print(json.dumps([stdlib,os.path.relpath(stdlib,sys.base_prefix)]))"])
        .env_clear().env("PATH", "/usr/bin:/bin").output().unwrap();
    assert!(output.status.success());
    let [stdlib, relative]: [String; 2] = serde_json::from_slice(&output.stdout).unwrap();
    // Managed installations can exceed the OS shebang limit; uv then emits its
    // shell/Python wrapper, whose console commands must still use the view.
    let python_root = root
        .path()
        .join("new-python")
        .join("managed-prefix-".repeat(11));
    let new_python = python_root
        .join("bin")
        .join(toolchain.python.file_name().unwrap());
    std::fs::create_dir_all(new_python.parent().unwrap()).unwrap();
    std::fs::copy(&toolchain.python, &new_python).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&new_python)
        .unwrap()
        .write_all(b"anchor-candidate-runtime-identity-fixture\n")
        .unwrap();
    let stdlib_link = python_root.join(relative);
    std::fs::create_dir_all(stdlib_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(stdlib, stdlib_link).unwrap();
    let mut new_config =
        test_support::config(&root.path().join("new-build"), &toolchain, wheels.clone());
    new_config.python = new_python.clone();
    let new_provisioner = Arc::new(DependencyProvisioner::new(new_config).unwrap());
    let data = root.path().join("state");
    let scope = data.join("pilot/owner-session");
    let preparations = scope.join("candidates");
    let old_snapshot = RuntimeSnapshot {
        python: Some(toolchain.python.clone()),
        python_enabled: true,
        rust: None,
        pixi: None,
        provisioner: Some(old_provisioner.clone()),
    };
    let configuration = Arc::new(RuntimeConfiguration::from_snapshot_for_test(
        &data,
        &library,
        old_provisioner.config.bwrap.clone(),
        old_snapshot.clone(),
    ));
    let application = RunApplication::new(data, library.clone())
        .with_runtime_configuration(configuration.clone());
    let old_turn = application
        .clone()
        .with_frozen_runtime_snapshot(old_snapshot.clone());
    let mut old_context = crate::pilot_tools::PilotTurnContext::new(
        std::path::Path::new("bwrap"),
        Some(library.as_path()),
        &root.path().join("work"),
        "owner",
        "session",
        false,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    old_context
        .configure(&old_snapshot, &library, &scope)
        .unwrap();
    let marker = format!(
        "candidate-runtime-{}",
        root.path().file_name().unwrap().to_string_lossy()
    );
    let source = old_context.workspace.join("candidate");
    candidate(&source, &marker, "normal");
    std::fs::write(
        source.join("server.py"),
        SERVER.replace(
            "'module':idna.__file__",
            "'module':idna.__file__,'interpreter':str(Path(sys.executable).resolve())",
        ),
    )
    .unwrap();
    let access = old_context.dependency_access();
    let prepared = old_turn
        .prepare_plugin_candidate(&library, &source, "candidate", &preparations, &access)
        .await
        .unwrap();
    let old_fact = prepared.fact.clone();
    let old_view = prepared.view.clone().unwrap();
    let legacy = preparations
        .join("candidate")
        .join(&old_fact.digest)
        .join("preparation.json");
    let legacy_bytes = std::fs::read(&legacy).unwrap();
    drop(prepared);
    configuration.publish_snapshot_for_test(RuntimeSnapshot {
        python: Some(new_python.clone()),
        python_enabled: true,
        rust: None,
        pixi: None,
        provisioner: Some(new_provisioner.clone()),
    });
    let same_turn = old_turn
        .prepare_plugin_candidate(&library, &source, "candidate", &preparations, &access)
        .await
        .unwrap();
    assert_eq!(same_turn.fact, old_fact);
    assert_eq!(same_turn.view.as_ref(), Some(&old_view));
    let result = old_turn
        .test_plugin_candidate(
            &same_turn,
            &old_context,
            Some(("probe", "probe", json!({}))),
        )
        .await
        .unwrap();
    assert_eq!(
        result["call"]["output"][0]["value"]["interpreter"],
        toolchain.python.to_string_lossy().as_ref()
    );
    std::fs::remove_file(old_context.workspace.join("probe-started")).unwrap();
    drop(same_turn);
    let new_snapshot = configuration.snapshot().unwrap();
    let new_turn = application
        .clone()
        .with_frozen_runtime_snapshot(new_snapshot.clone());
    let mut new_context = crate::pilot_tools::PilotTurnContext::new(
        std::path::Path::new("bwrap"),
        Some(library.as_path()),
        &root.path().join("work"),
        "owner",
        "session",
        false,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    new_context
        .configure(&new_snapshot, &library, &scope)
        .unwrap();
    // A failed solve must leave the previous successful selection inspectable.
    let retained_wheel = root.path().join("retained-wheel.whl");
    std::fs::rename(&wheel, &retained_wheel).unwrap();
    assert!(
        new_turn
            .prepare_plugin_candidate(&library, &source, "candidate", &preparations, &access)
            .await
            .is_err()
    );
    assert_eq!(
        load_fact(&preparations, "candidate", &old_fact.digest).unwrap(),
        Some(old_fact.clone())
    );
    assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
    std::fs::rename(retained_wheel, &wheel).unwrap();
    let prepared = new_turn
        .prepare_plugin_candidate(&library, &source, "candidate", &preparations, &access)
        .await
        .unwrap();
    let new_fact = prepared.fact.clone();
    let new_view = prepared.view.clone().unwrap();
    assert_eq!(
        new_fact.digest, old_fact.digest,
        "runtime changes need no source edit"
    );
    assert_eq!(new_fact.declaration_digest, old_fact.declaration_digest);
    assert_eq!(
        new_fact.resolved.as_ref().unwrap().interpreter_path,
        new_python
    );
    assert_ne!(
        new_fact.resolved.as_ref().unwrap().interpreter_digest,
        old_fact.resolved.as_ref().unwrap().interpreter_digest
    );
    assert_ne!(new_view, old_view);
    // A selected fact is not sufficient if its actual view was changed. Refuse
    // before launch rather than testing the same source on another interpreter.
    let view_python = new_view.join("bin/python");
    std::fs::remove_file(&view_python).unwrap();
    std::os::unix::fs::symlink(&toolchain.python, &view_python).unwrap();
    assert!(matches!(
        new_turn.test_plugin_candidate(&prepared, &new_context, None).await,
        Err(ApplicationError::Invalid(reason)) if reason.contains("prepared interpreter")
    ));
    assert!(!new_context.workspace.join("probe-started").exists());
    std::fs::remove_file(&view_python).unwrap();
    std::os::unix::fs::symlink(&new_python, &view_python).unwrap();
    let result = new_turn
        .test_plugin_candidate(&prepared, &new_context, Some(("probe", "probe", json!({}))))
        .await
        .unwrap();
    let report = &result["call"]["output"][0]["value"];
    assert_eq!(report["version"], "3.6");
    assert_eq!(report["interpreter"], new_python.to_string_lossy().as_ref());
    // An ordinary console command must enter the selected view, including when
    // uv's staged command used a long interpreter-path shell wrapper.
    let launcher = new_view.join("bin/idna-tool");
    assert!(
        !std::fs::symlink_metadata(&launcher)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let view = crate::plugin_view::PreparedView::for_candidate(
        &new_view,
        &anchor_graph_host::store_root(&library),
        &new_fact.declaration_digest,
    )
    .unwrap();
    let sandbox = view.authorize(new_context.sandbox.clone()).unwrap();
    let mut request = anchor_runtime::SandboxRequest::new(
        new_context.workspace.clone(),
        ["sh", "-c", "idna-tool"],
    );
    view.apply(&mut request);
    use anchor_runtime::SandboxPort;
    let result = sandbox.run(request).await.unwrap();
    assert_eq!(
        result.status,
        anchor_runtime::SandboxStatus::Completed,
        "{result:?}"
    );
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout.trim(), "hello 3.6");
    drop(prepared);
    // The new Turn reuses its verified selection after all source wheels and
    // resolver/fetch caches vanish, without choosing an old Turn's interpreter.
    std::fs::remove_file(wheel).unwrap();
    for provisioner in [&old_provisioner, &new_provisioner] {
        for directory in ["cache", "artifacts"] {
            let path = provisioner.config.build_root.join(directory);
            if path.exists() {
                std::fs::remove_dir_all(path).unwrap();
            }
        }
    }
    let retry = new_turn
        .prepare_plugin_candidate(&library, &source, "candidate", &preparations, &access)
        .await
        .unwrap();
    assert_eq!(retry.fact, new_fact);
    assert_eq!(retry.view, Some(new_view.clone()));
    drop(retry);
    // The shared package receipt currently verifies one interpreter digest.
    // Preparing the new binary updates that receipt, so trying to re-prepare
    // the old binary afterward fails closed. It must retain both candidate
    // facts and the current successful selection rather than switch runtimes.
    assert!(matches!(
        old_turn
            .prepare_plugin_candidate(&library, &source, "candidate", &preparations, &access)
            .await,
        Err(ApplicationError::Invalid(reason)) if reason.contains("does not match the resolved artifact or interpreter")
    ));
    assert_eq!(
        load_fact(&preparations, "candidate", &old_fact.digest).unwrap(),
        Some(new_fact.clone())
    );
    assert_eq!(
        crate::assistant::read_json::<PreparationFact>(&preparation_path(
            legacy.parent().unwrap(),
            &old_fact,
        ))
        .unwrap(),
        Some(old_fact.clone())
    );
    let latest = new_turn
        .prepare_plugin_candidate(&library, &source, "candidate", &preparations, &access)
        .await
        .unwrap();
    assert_eq!(latest.fact, new_fact);
    assert_eq!(
        std::fs::read(&legacy).unwrap(),
        legacy_bytes,
        "original preparation is immutable"
    );
    assert!(old_view.is_dir());
    assert_eq!(
        std::fs::read_dir(anchor_graph_host::store_root(&library).join("idna"))
            .unwrap()
            .count(),
        1
    );
    assert!(matching_processes(&marker).is_empty());
}

#[tokio::test]
async fn candidate_prepares_calls_offline_and_installs_independently_of_its_session() {
    let _gate = crate::PROCESS_ENV_LOCK.lock().await;
    let Some(toolchain) = test_support::toolchain() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let mut environment = environment();
    let fixture_secret = format!(
        "private-{}",
        root.path().file_name().unwrap().to_string_lossy()
    );
    for key in [
        "ANCHOR_CANDIDATE_HOST_SECRET",
        "anchor_candidate_host_secret",
        "PLUGIN_ROOT",
        "CODEX_PLUGIN_ROOT",
        "CLAUDE_PLUGIN_ROOT",
        "ANCHOR_PLUGIN_VIEW",
    ] {
        environment.set(key, &fixture_secret);
    }
    let wheels = root.path().join("wheels");
    std::fs::create_dir(&wheels).unwrap();
    test_support::fixture_wheel(&wheels, "idna", "3.6");
    let provisioner = Arc::new(
        DependencyProvisioner::new(test_support::config(root.path(), &toolchain, wheels)).unwrap(),
    );
    let application = RunApplication::new(root.path().join("state"), library.clone())
        .with_dependency_provisioner(provisioner);
    let context = crate::pilot_tools::PilotTurnContext::new(
        std::path::Path::new("bwrap"),
        Some(library.as_path()),
        &root.path().join("work"),
        "owner",
        "session",
        false,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let marker = format!(
        "candidate-{}",
        root.path().file_name().unwrap().to_string_lossy()
    );
    let source = context.workspace.join("candidate");
    candidate(&source, &marker, "normal");
    let manifest = source.join("plugin.json");
    let mut declaration: Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    declaration["mcpServers"]["probe"] = json!({
        "command":"${ANCHOR_PLUGIN_VIEW}/bin/python",
        "args":["${PLUGIN_ROOT}/server.py",marker,"normal","${CODEX_PLUGIN_ROOT}","${CLAUDE_PLUGIN_ROOT}"],
        "env":{"ROOT":"${PLUGIN_ROOT}","CODEX_ROOT":"${CODEX_PLUGIN_ROOT}",
            "CLAUDE_ROOT":"${CLAUDE_PLUGIN_ROOT}","VIEW":"${ANCHOR_PLUGIN_VIEW}"}
    });
    std::fs::write(manifest, declaration.to_string()).unwrap();
    std::fs::write(source.join("server.py"), format!("import os\n{}", SERVER.replace(
        "report = {'version':idna.VALUE,'module':idna.__file__}",
        "report = {'version':idna.VALUE,'module':idna.__file__,\n            'host_secret_visible':any(key in os.environ for key in ['ANCHOR_CANDIDATE_HOST_SECRET','anchor_candidate_host_secret']),\n            'path_tokens_visible':all(Path(os.environ.get(key,'')) == Path('/plugins/candidate') for key in ['ROOT','CODEX_ROOT','CLAUDE_ROOT']) and all(Path(value) == Path('/plugins/candidate') for value in sys.argv[3:]) and Path(os.environ['VIEW'] + '/bin/python').is_file()}"
    ))).unwrap();
    let preparations = root.path().join("preparations");
    let prepared = application
        .prepare_plugin_candidate(
            &library,
            &source,
            "candidate",
            &preparations,
            &context.dependency_access(),
        )
        .await
        .unwrap();
    assert!(!library.join("plugins/candidate").exists());
    let store = anchor_graph_host::store_root(&library);
    assert!(store.join("idna/3.6").is_dir());
    let candidate_view = prepared.view.clone().unwrap();
    assert!(candidate_view.starts_with(library.join("dependencies/development")));
    assert_eq!(
        crate::plugin_view::referenced_store_directories(&candidate_view, &store).unwrap(),
        vec![store.join("idna/3.6")]
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let result = application
        .test_plugin_candidate(
            &prepared,
            &context,
            Some((
                "probe",
                "probe",
                json!({"address":["127.0.0.1",address.port()]}),
            )),
        )
        .await
        .unwrap();
    assert_eq!(result["protocol_started"], true);
    assert_eq!(result["business_tested"], true);
    let report = result["call"]["output"][0]["value"].clone();
    assert_eq!(report["version"], "3.6", "{result}");
    assert_eq!(report["network"], "denied", "{result}");
    assert_eq!(report["host_secret_visible"], false);
    assert_eq!(report["path_tokens_visible"], true);
    assert!(!result.to_string().contains(&fixture_secret));
    assert!(matching_processes(&marker).is_empty());
    // A source-only edit gets a new code approval digest while retaining the
    // actual dependency solve and its shared view.
    let server = source.join("server.py");
    let mut changed = std::fs::read_to_string(&server).unwrap();
    changed.push_str("\n# source-only revision\n");
    std::fs::write(&server, changed).unwrap();
    let revised = application
        .prepare_plugin_candidate(
            &library,
            &source,
            "candidate",
            &preparations,
            &context.dependency_access(),
        )
        .await
        .unwrap();
    assert_ne!(revised.fact.digest, prepared.fact.digest);
    assert_eq!(
        revised.fact.declaration_digest,
        prepared.fact.declaration_digest
    );
    assert_eq!(revised.view, prepared.view);
    drop(revised);
    let precondition = application
        .plugin_install_precondition_with_access(
            library.clone(),
            source.clone(),
            "candidate".into(),
            context.dependency_access(),
        )
        .await
        .unwrap();
    application
        .install_plugin_if_unchanged(library.clone(), precondition)
        .await
        .unwrap();
    drop(prepared);
    crate::plugin_library::PluginLibrary::new(&library)
        .release_development_scope(preparations.parent().unwrap())
        .unwrap();
    assert!(!candidate_view.exists());
    std::fs::remove_dir_all(&preparations).unwrap();
    std::fs::remove_dir_all(&source).unwrap();
    let catalog = Catalog::new(&library);
    let binding = catalog.resolve(&["candidate".into()]).unwrap().remove(0);
    let views = BTreeMap::from([(
        "candidate".into(),
        crate::plugin_view::PreparedPlugin::for_candidate(
            &catalog,
            &binding.id,
            crate::plugin_view::prepare(&library, &binding).unwrap(),
        )
        .unwrap(),
    )]);
    let tools =
        crate::tool_host::McpToolConfig::from_catalog(&library, std::slice::from_ref(&binding))
            .unwrap()
            .bind_with_node_inputs(
                std::slice::from_ref(&binding),
                false,
                crate::tool_host::SandboxLaunch {
                    sandbox: &context.sandbox,
                    workspace: &context.workspace,
                    readonly_inputs: &[],
                    plugin_views: &views,
                },
            )
            .await
            .unwrap();
    let output = tools
        .call("candidate-probe_probe", json!({}))
        .await
        .unwrap();
    assert_eq!(output[0].as_json().unwrap()["version"], "3.6");
    tools.shutdown().await.unwrap();
    assert!(matching_processes(&marker).is_empty());
}

#[tokio::test]
async fn candidate_startup_and_call_cancellation_reap_the_actual_processes() {
    let _gate = crate::PROCESS_ENV_LOCK.lock().await;
    let Some(toolchain) = test_support::toolchain() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let _environment = environment();
    let wheels = root.path().join("wheels");
    std::fs::create_dir(&wheels).unwrap();
    test_support::fixture_wheel(&wheels, "idna", "3.6");
    let application = RunApplication::new(root.path().join("state"), library.clone())
        .with_dependency_provisioner(Arc::new(
            DependencyProvisioner::new(test_support::config(root.path(), &toolchain, wheels))
                .unwrap(),
        ));
    for mode in ["hang_start", "hang_call"] {
        let cancel = Arc::new(AtomicBool::new(false));
        let context = crate::pilot_tools::PilotTurnContext::new(
            std::path::Path::new("bwrap"),
            Some(library.as_path()),
            &root.path().join("work"),
            "owner",
            mode,
            false,
            cancel.clone(),
        )
        .unwrap();
        let marker = format!(
            "{mode}-{}",
            root.path().file_name().unwrap().to_string_lossy()
        );
        let source = context.workspace.join("candidate");
        candidate(&source, &marker, mode);
        let prepared = application
            .prepare_plugin_candidate(
                &library,
                &source,
                "candidate",
                &root.path().join("preparations"),
                &context.dependency_access(),
            )
            .await
            .unwrap();
        let cancel_after_start = async {
            started(&context).await;
            assert!(!matching_processes(&marker).is_empty());
            tokio::time::sleep(Duration::from_millis(150)).await;
            cancel.store(true, Ordering::Release);
        };
        let (result, ()) = tokio::join!(
            application.test_plugin_candidate(
                &prepared,
                &context,
                Some(("probe", "probe", json!({})))
            ),
            cancel_after_start
        );
        assert!(format!("{:?}", result.unwrap_err()).contains("cancel"));
        assert!(
            matching_processes(&marker).is_empty(),
            "{mode} left processes alive"
        );
        cancel.store(false, Ordering::Release);
        candidate(&source, &marker, "normal");
        let replacement = application
            .prepare_plugin_candidate(
                &library,
                &source,
                "candidate",
                &root.path().join("preparations"),
                &context.dependency_access(),
            )
            .await
            .unwrap();
        assert_ne!(prepared.fact.digest, replacement.fact.digest);
        assert!(
            application
                .test_plugin_candidate(&replacement, &context, None)
                .await
                .is_ok()
        );
        assert!(matching_processes(&marker).is_empty());
    }
}
