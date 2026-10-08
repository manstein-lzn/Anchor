#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use anchor_distribution::{PackageRequest, ToolBinary, build_package};
use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use flate2::read::GzDecoder;
use goose::{Gate, Host, Provider, Step, command, complete, digest};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

const CASE_SOURCE: &str = "tests/goose_distribution.rs";

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        assert!(!kind.is_symlink());
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            assert!(kind.is_file());
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn official_inputs(root: &Path) -> (Option<PathBuf>, Vec<ToolBinary>) {
    let inputs = [
        "ANCHOR_TEST_ACADEMIC_PLUGIN",
        "ANCHOR_TEST_SCHOLARLY_BINARY",
        "ANCHOR_TEST_WECOM_GATEWAY_BINARY",
    ]
    .map(std::env::var_os);
    assert!(
        inputs.iter().all(Option::is_none) || inputs.iter().all(Option::is_some),
        "official package inputs must be provided together"
    );
    let [Some(plugin), Some(scholarly), Some(gateway)] = inputs else {
        return (None, Vec::new());
    };
    copy_directory(
        Path::new(&plugin),
        &root.join("bundle/plugins/academic-research"),
    );
    let mut graph = fixture::read_json(root.join("bundle/graph.json"));
    graph["nodes"][0]["plugins"] = json!(["academic-research"]);
    fs::write(root.join("bundle/graph.json"), graph.to_string()).unwrap();
    let binding = FilePluginCatalog::new(root.join("bundle"))
        .resolve(&["academic-research".into()])
        .unwrap()
        .remove(0);
    fs::write(root.join("bundle/manifest.json"), json!({
        "format":1,"graph":"graph.json","plugins":[{
            "id":binding.id,"digest":binding.digest,"resources":binding.resources,"mcp_servers":binding.mcp_servers
        }]
    }).to_string()).unwrap();
    let input_scholarly = root.join("package-input-scholarly");
    let input_gateway = root.join("package-input-gateway");
    fs::copy(scholarly, &input_scholarly).unwrap();
    fs::copy(gateway, &input_gateway).unwrap();
    (
        Some(input_scholarly),
        vec![ToolBinary {
            name: "anchor-wecom-gateway".into(),
            path: input_gateway,
        }],
    )
}

fn check_web(server: &goose::HttpHost, runtime: &Path, enabled: bool) -> Value {
    if !enabled {
        return Value::Null;
    }
    let index = runtime.join("web/index.html");
    let page = server.events("/", 0);
    assert_eq!(page.as_bytes(), fs::read(&index).unwrap());
    assert!(page.contains("<html") && page.contains("/assets/"));
    let files = goose::file_inventory(&runtime.join("web"));
    let mut assets = Vec::new();
    for suffix in [".js", ".css"] {
        let file = files
            .iter()
            .find(|file| {
                file["path"]
                    .as_str()
                    .is_some_and(|path| path.starts_with("assets/") && path.ends_with(suffix))
            })
            .expect("built Web asset");
        let path = file["path"].as_str().unwrap();
        assert_eq!(
            server.events(&format!("/{path}"), 0).as_bytes(),
            fs::read(runtime.join("web").join(path)).unwrap()
        );
        assets.push(file.clone());
    }
    json!({"index_sha256":digest(&index),"assets":assets,"http_success":true})
}

#[test]
#[ignore = "separate distribution acceptance: requires pinned Goose, Bubblewrap and archive creation"]
fn extracted_goose_runtime_resumes_without_sources_or_replaying_workspace_effects() {
    let gate = Gate::new();
    let provider = Provider::new(
        "goose-source-free-distribution",
        vec![
            command("set -eu; test ! -e effect.txt; printf once > effect.txt; cat effect.txt"),
            complete("verify").gated(&gate),
        ],
    );
    let host = Host::new(&json!({
        "objective":"Inspect and resume the same Goose invocation from extracted Runtime binaries",
        "entry":"worker",
        "agents":{"worker":{"model":"models.worker","instructions":"Inspect workspace facts, write effect.txt exactly once, and finish with route verify."}},
        "ops":{"verify":{"run":"sh -c 'set -eu; test \"$(cat /in/worker/effect.txt)\" = once; if printf corrupt >> /in/worker/effect.txt 2>/dev/null; then exit 7; fi; cat /in/worker/effect.txt > verified.txt'"}},
        "nodes":[{"id":"worker","agent":"worker"},{"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    })).default_runtime();
    let root = host.base.root.path().to_path_buf();
    let archive_path = root.join("runtime.tar.gz");
    let input_binary = root.join("package-input-host");
    fs::copy(root.join("anchor-runner-host"), &input_binary).unwrap();
    let web = std::env::var_os("ANCHOR_TEST_WEB_DIST").map(PathBuf::from);
    let (scholarly, tools) = official_inputs(&root);
    let official = scholarly.is_some();
    let package = build_package(&PackageRequest {
        host: input_binary,
        goose: PathBuf::from(std::env::var_os("ANCHOR_GOOSE_BINARY").unwrap()),
        bundle: root.join("bundle"),
        web: web.clone(),
        tools,
        scholarly,
        docmost_tools: None,
        wecom_tools: None,
        wecom_gateway: None,
        output: archive_path.clone(),
    })
    .unwrap();
    let archive_sha256 = digest(&archive_path);
    assert_eq!(package.sha256, archive_sha256);
    let mut archive = tar::Archive::new(GzDecoder::new(fs::File::open(&archive_path).unwrap()));
    let mut packaged_paths = Vec::new();
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        assert!(entry.header().entry_type().is_file() || entry.header().entry_type().is_dir());
        let path = entry.path().unwrap().into_owned();
        assert!(path.starts_with("anchor-runtime"));
        for component in &path {
            let name = component.to_str().unwrap();
            assert!(!matches!(
                name,
                "src"
                    | "Cargo.toml"
                    | "Cargo.lock"
                    | ".env"
                    | ".git"
                    | ".venv"
                    | "node_modules"
                    | "__pycache__"
            ));
            assert!(!name.starts_with(".env."));
            assert!(
                !name.ends_with(".rs")
                    && !name.ends_with(".py")
                    && !name.ends_with(".pyc")
                    && !name.ends_with(".pyo")
            );
        }
        packaged_paths.push(path.to_str().unwrap().to_owned());
    }
    let extracted = root.join("extracted");
    fs::create_dir(&extracted).unwrap();
    tar::Archive::new(GzDecoder::new(fs::File::open(&archive_path).unwrap()))
        .unpack(&extracted)
        .unwrap();
    let runtime = extracted.join("anchor-runtime");
    assert!(runtime.join("runtime-manifest.json").is_file());
    let manifest = fixture::read_json(runtime.join("runtime-manifest.json"));
    assert_eq!(serde_json::to_value(&package.inventory).unwrap(), manifest);
    for file in &package.inventory.files {
        let path = runtime.join(&file.path);
        assert_eq!(digest(&path), file.sha256);
        assert_eq!(path.metadata().unwrap().len(), file.size);
    }
    if official {
        for (input, packaged) in [
            (
                "package-input-scholarly",
                "bundle/plugins/academic-research/bin/anchor-scholarly",
            ),
            ("package-input-gateway", "bin/anchor-wecom-gateway"),
        ] {
            assert_eq!(digest(&root.join(input)), digest(&runtime.join(packaged)));
        }
    }
    fs::rename(root.join("bundle"), root.join("retained-unpackaged-bundle")).unwrap();
    let mut host = host
        .with_runtime_binaries(
            &runtime.join("bin/anchor-runner-host"),
            &runtime.join("bin/goose"),
        )
        .with_extra_environment([
            ("ANCHOR_RUNNER_BUNDLE_ROOT", runtime.join("bundle")),
            ("ANCHOR_RUNNER_CATALOG_ROOT", runtime.clone()),
        ]);
    if web.is_some() {
        host = host.with_extra_environment([("ANCHOR_RUNNER_WEB_ROOT", runtime.join("web"))]);
    }
    let mut server = host.serve(&provider);
    let (status, ready) = server.request("GET", "/ready", None);
    assert_eq!(status, 200, "{ready}");
    assert_eq!(ready["status"], "ready");
    let web_http = check_web(&server, &runtime, web.is_some());
    let run = server.trigger();
    gate.wait_entered();
    let session = host.native_fact(&run, "worker", 1)["session_id"].clone();
    let (status, stopping) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{stopping}");
    server.wait_status(&run, "stopped");
    gate.open();
    server.kill();
    drop(server);
    fs::copy(
        provider.root.join("host-http.log"),
        provider.root.join("host-first.log"),
    )
    .unwrap();
    provider.append(vec![
        command("set -eu; test \"$(cat effect.txt)\" = once; cat effect.txt").after("once"),
        complete("verify").after("once"),
        Step::text("The extracted Runtime resumed after inspecting the workspace."),
    ]);
    let mut restarted = host.serve(&provider);
    let (status, resumed) = restarted.request("POST", &format!("/runs/{run}/resume"), None);
    assert_eq!(status, 202, "{resumed}");
    let run_http = restarted.wait_status(&run, "completed");
    let session_after = host.native_fact(&run, "worker", 1)["session_id"].clone();
    assert_eq!(session_after, session);
    let record = host.record(&run);
    assert_eq!(host.base.file(&record, "worker", "effect.txt"), b"once");
    assert_eq!(host.base.file(&record, "verify", "verified.txt"), b"once");
    assert_eq!(
        host.base.workspace_files(&run, "effect.txt"),
        vec![b"once".to_vec()]
    );
    assert_eq!(
        host.base.workspace_files(&run, "verified.txt"),
        vec![b"once".to_vec()]
    );
    for (node, filename) in [("worker", "effect.txt"), ("verify", "verified.txt")] {
        assert_eq!(record["results"][node].as_array().unwrap().len(), 1);
        assert_eq!(run_http["state"]["nodes"][node]["pass_number"], 1);
        assert_eq!(run_http["state"]["nodes"][node]["submitted"], true);
        let (status, files) = restarted.request("GET", &format!("/runs/{run}/files/{node}"), None);
        assert_eq!(status, 200, "{files}");
        assert!(
            files["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|file| file["path"] == filename && file["size"] == 4)
        );
    }
    let metadata = fixture::read_json(root.join("state/run-metadata").join(format!("{run}.json")));
    assert_eq!(
        metadata["bundle_source"],
        runtime.join("bundle").to_str().unwrap()
    );
    let history = host.native_conversation(&run, "worker", 1);
    let writes = history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter(|content| {
            content["type"] == "toolRequest"
                && content["toolCall"]
                    .to_string()
                    .contains("printf once > effect.txt")
        })
        .count();
    assert_eq!(writes, 1);
    assert!(history.to_string().contains("final_result"));
    assert_eq!(provider.requests().len(), 5);
    let requests_before_restart = provider.requests().len();
    restarted.kill();
    drop(restarted);
    fs::copy(
        provider.root.join("host-http.log"),
        provider.root.join("host-resumed.log"),
    )
    .unwrap();
    let mut completed_restart = host.serve(&provider);
    completed_restart.wait_status(&run, "completed");
    let (status, ready) = completed_restart.request("GET", "/ready", None);
    assert_eq!(status, 200, "{ready}");
    assert_eq!(ready["status"], "ready");
    assert_eq!(
        check_web(&completed_restart, &runtime, web.is_some()),
        web_http
    );
    assert_eq!(host.record(&run), record);
    let session_after_restart = host.native_fact(&run, "worker", 1)["session_id"].clone();
    assert_eq!(session_after_restart, session);
    assert_eq!(provider.requests().len(), requests_before_restart);
    assert_eq!(
        host.base.workspace_files(&run, "effect.txt"),
        vec![b"once".to_vec()]
    );
    assert_eq!(host.base.file(&record, "verify", "verified.txt"), b"once");
    completed_restart.kill();
    drop(completed_restart);
    fs::copy(&archive_path, provider.root.join("anchor-runtime.tar.gz")).unwrap();
    fs::copy(
        runtime.join("runtime-manifest.json"),
        provider.root.join("runtime-manifest.json"),
    )
    .unwrap();
    host.evidence(&provider, &run, json!({
        "case_source":CASE_SOURCE,"archive_sha256":archive_sha256,"packaged_paths":packaged_paths,
        "runtime_manifest":manifest,"test_binary":std::env::current_exe().unwrap(),
        "actual_extracted_binaries":true,"unpackaged_bundle_unavailable":true,
        "same_native_session":true,"workspace_effect_once":true,"readonly_input":true,
        "source_free":true,"package_files_verified":true,"readiness_checked":true,
        "web_served":web.is_some(),"web_index_sha256":web_http["index_sha256"],"web_http":web_http,
        "official_binaries_packaged":official,"node_history_checked":true,"native_history_checked":true,
        "artifact_exact_bytes":true,"workspace_exact_bytes":true,"run_http":run_http,
        "session_before":session,"session_after":session_after,"session_after_completed_restart":session_after_restart,
        "workspace_write_count":writes,"completed_restart_unchanged":true,
        "requests_before_completed_restart":requests_before_restart,"requests_after_completed_restart":provider.requests().len(),
        "boundary":"source-free extracted binaries, five deterministic local requests and stop/resume/completed restart; no real model, business service or production cutover"
    }));
}
