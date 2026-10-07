#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use anchor_distribution::{PackageRequest, build_package};
use flate2::read::GzDecoder;
use goose::{Gate, Host, Provider, Step, command, complete, digest};
use serde_json::json;
use std::{fs, path::PathBuf};

const CASE_SOURCE: &str = "tests/goose_distribution.rs";

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
    build_package(&PackageRequest {
        host: input_binary,
        goose: PathBuf::from(std::env::var_os("ANCHOR_GOOSE_BINARY").unwrap()),
        bundle: root.join("bundle"),
        web: None,
        tools: Vec::new(),
        output: archive_path.clone(),
    })
    .unwrap();
    let archive_sha256 = digest(&archive_path);
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
                "src" | "Cargo.toml" | "Cargo.lock" | ".env" | ".git" | ".venv" | "node_modules"
            ));
            assert!(!name.ends_with(".rs") && !name.ends_with(".py") && !name.ends_with(".pyc"));
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
    fs::rename(root.join("bundle"), root.join("retained-unpackaged-bundle")).unwrap();
    let host = host
        .with_runtime_binaries(
            &runtime.join("bin/anchor-runner-host"),
            &runtime.join("bin/goose"),
        )
        .with_extra_environment([
            ("ANCHOR_RUNNER_BUNDLE_ROOT", runtime.join("bundle")),
            ("ANCHOR_RUNNER_CATALOG_ROOT", runtime.clone()),
        ]);
    let mut server = host.serve(&provider);
    let run = server.trigger();
    gate.wait_entered();
    let session = host.native_fact(&run, "worker", 1)["session_id"].clone();
    let (status, stopping) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{stopping}");
    server.wait_status(&run, "stopped");
    gate.open();
    server.kill();
    drop(server);
    provider.append(vec![
        command("set -eu; test \"$(cat effect.txt)\" = once; cat effect.txt").after("once"),
        complete("verify").after("once"),
        Step::text("The extracted Runtime resumed after inspecting the workspace."),
    ]);
    let restarted = host.serve(&provider);
    let (status, resumed) = restarted.request("POST", &format!("/runs/{run}/resume"), None);
    assert_eq!(status, 202, "{resumed}");
    restarted.wait_status(&run, "completed");
    assert_eq!(host.native_fact(&run, "worker", 1)["session_id"], session);
    let record = host.record(&run);
    assert_eq!(host.base.file(&record, "worker", "effect.txt"), b"once");
    assert_eq!(host.base.file(&record, "verify", "verified.txt"), b"once");
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
    assert_eq!(provider.requests().len(), 5);
    host.evidence(&provider, &run, json!({
        "case_source":CASE_SOURCE,"archive_sha256":archive_sha256,"packaged_paths":packaged_paths,
        "runtime_manifest":fixture::read_json(runtime.join("runtime-manifest.json")),
        "actual_extracted_binaries":true,"unpackaged_bundle_unavailable":true,
        "same_native_session":true,"workspace_effect_once":true,"readonly_input":true,
        "boundary":"source-free extracted binaries and no Python execution; not an OS without Python, a real model or production cutover"
    }));
}
