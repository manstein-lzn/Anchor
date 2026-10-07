#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use anchor_graph_host::FileGraphBundleLoader;
use anchor_library::Library;
use goose::{Host, Provider, Step, command, complete};
use serde_json::{Value, json};
use std::fs;

const CASE_SOURCE: &str = "tests/goose_library.rs";
const ORIGINAL: &str = "fixture-installed-instructions-v1";
const REPLACEMENT: &str = "fixture-installed-instructions-v2";

fn graph() -> Value {
    json!({
        "objective":"Use an installed Plugin without mutating its frozen resources",
        "entry":"worker",
        "agents":{"worker":{"model":"models.worker","instructions":"Read the demo Skill, verify that its mount is read-only, save the exact Skill in skill.txt, and finish with route verify."}},
        "ops":{"verify":{"run":"sh -c 'set -eu; cat /in/worker/skill.txt > verified.txt'"}},
        "nodes":[{"id":"worker","agent":"worker","plugins":["demo"]},{"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    })
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn installed_plugin_is_frozen_before_library_replacement_and_native_execution() {
    let provider = Provider::new(
        "goose-installed-plugin",
        vec![
            command(
                "set -eu; cat /plugins/demo/skills/demo/SKILL.md > skill.txt; test \"$(cat skill.txt)\" = fixture-installed-instructions-v1; if printf corrupt >> /plugins/demo/skills/demo/SKILL.md 2>/dev/null; then exit 7; fi; cat skill.txt",
            ),
            complete("verify").after(ORIGINAL),
            Step::text("The frozen installed Plugin was verified."),
        ],
    );
    let host = Host::new(&json!({
        "entry":"work","agents":{},"ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work"}],"edges":[]
    }))
    .default_runtime();
    let root = host.base.root.path();
    let source = root.join("checkout/demo");
    fs::create_dir_all(source.join(".codex-plugin")).unwrap();
    fs::create_dir_all(source.join("skills/demo")).unwrap();
    fs::write(
        source.join(".codex-plugin/plugin.json"),
        r#"{"name":"Installed fixture","skills":"skills/"}"#,
    )
    .unwrap();
    fs::write(source.join("skills/demo/SKILL.md"), ORIGINAL).unwrap();
    let library = Library::new(root);
    let installed = library.install_directory("demo", &source, false).unwrap();
    assert!(!installed.directory.join(".codex-plugin").exists());
    let server = host.serve(&provider);
    let (status, created) = server.request(
        "POST",
        "/graphs",
        Some(&json!({
            "name":"installed","definition":graph()
        })),
    );
    assert_eq!(status, 201, "{created}");
    let bundle_root = root.join("installed");
    let frozen = FileGraphBundleLoader::new(&bundle_root).load().unwrap();
    assert_eq!(frozen.plugins[0].digest, installed.digest);
    fs::write(source.join("skills/demo/SKILL.md"), REPLACEMENT).unwrap();
    let replaced = library.install_directory("demo", &source, true).unwrap();
    assert_ne!(installed.digest, replaced.digest);
    let (status, current) = server.request("GET", "/plugins/demo", None);
    assert_eq!(status, 200, "{current}");
    assert_eq!(current["digest"], replaced.digest);
    assert_eq!(current["instructions"], REPLACEMENT);
    let preserved = FileGraphBundleLoader::new(&bundle_root).load().unwrap();
    assert_eq!(frozen.plugins, preserved.plugins);
    let (status, accepted) =
        server.request("POST", "/trigger", Some(&json!({"graph":"installed"})));
    assert_eq!(status, 202, "{accepted}");
    let run = accepted["run"].as_str().unwrap();
    server.wait_status(run, "completed");
    let record = host.record(run);
    assert_eq!(
        host.base.file(&record, "worker", "skill.txt"),
        ORIGINAL.as_bytes()
    );
    assert_eq!(
        host.base.file(&record, "verify", "verified.txt"),
        ORIGINAL.as_bytes()
    );
    assert_eq!(
        fs::read_to_string(bundle_root.join("plugins/demo/skills/demo/SKILL.md")).unwrap(),
        ORIGINAL
    );
    assert_eq!(
        fs::read_to_string(installed.directory.join("skills/demo/SKILL.md")).unwrap(),
        REPLACEMENT
    );
    let history = host.native_conversation(run, "worker", 1);
    assert!(history.to_string().contains(ORIGINAL));
    assert!(history.to_string().contains("final_result"));
    assert!(
        provider.requests()[0]["messages"]
            .to_string()
            .contains("/plugins/demo/skills/demo/SKILL.md")
    );
    host.evidence(&provider, run, json!({
        "case_source":CASE_SOURCE,"library_install":installed,"library_replacement":replaced,
        "library_detail":current,"frozen_plugin_unchanged":true,"readonly_mount":true,
        "native_history_checked":true,"artifact_exact_bytes":true,
        "boundary":"local directory installer plus actual public Graph API and Goose; no GitHub network, OAuth or real model"
    }));
}
