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
        "objective":"Use the current installed Plugin after a Library update without mutating its resources",
        "entry":"gate",
        "agents":{"worker":{"model":"models.worker","instructions":"Discover the current demo Skill with anchor_plugin_read section skills, read the selected Skill, verify that its mount is read-only, save the exact Skill in skill.txt, and finish with route verify."}},
        "ops":{
            "gate":{"run":"sh -c 'set -eu; : > gate-entered; while [ ! -e proceed ]; do sleep 0.05; done'"},
            "verify":{"writes":["verified.txt"],"run":"sh -c 'set -eu; cat /in/worker/skill.txt > verified.txt'"}
        },
        "nodes":[{"id":"gate","op":"gate"},{"id":"worker","agent":"worker","plugins":["demo"]},{"id":"verify","op":"verify"}],
        "edges":[{"from":"gate","to":"worker"},{"from":"worker","to":"verify"}]
    })
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn a_library_update_during_a_run_reaches_native_execution_with_current_skills() {
    let provider = Provider::new(
        "goose-installed-plugin",
        vec![
            Step::tool(
                "anchor_plugin_read",
                json!({"plugin":"demo","section":"skills"}),
            ),
            Step::tool(
                "anchor_plugin_read",
                json!({"plugin":"demo","section":"skill","path":"skills/current/SKILL.md"}),
            )
            .after("skills/current/SKILL.md"),
            command(
                "set -eu; cat /plugins/demo/skills/current/SKILL.md > skill.txt; test \"$(cat skill.txt)\" = fixture-installed-instructions-v2; if printf corrupt >> /plugins/demo/skills/current/SKILL.md 2>/dev/null; then exit 7; fi; cat skill.txt",
            )
            .after(REPLACEMENT),
            complete("verify").files(&["skill.txt"]).after(REPLACEMENT),
        ],
    );
    let host = Host::new(&json!({
        "entry":"work","agents":{},"ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work"}],"edges":[]
    }))
    .default_runtime()
    .with_allowed_commands("sh,cat,git,true,sleep");
    let root = host.base.root.path();
    let library_root = root.to_path_buf();
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
    // A bundle references the Plugin by id: the Plugin's code and digest live in
    // the Library, and upgrading it needs no bundle change at all.
    let frozen = FileGraphBundleLoader::new(&bundle_root)
        .with_library_root(&library_root)
        .load()
        .unwrap();
    assert_eq!(frozen.plugins, ["demo"]);
    assert!(!bundle_root.join("plugins").exists());
    let (status, accepted) =
        server.request("POST", "/trigger", Some(&json!({"graph":"installed"})));
    assert_eq!(status, 202, "{accepted}");
    let run = accepted["run"].as_str().unwrap();
    goose::wait_until("the admitted Run's Op gate", || {
        host.base.workspace_files(run, "gate-entered").len() == 1
    });
    let admitted = host.record(run);
    assert_eq!(
        admitted["plugin_bindings"]["demo"]["digest"],
        installed.digest
    );
    assert!(provider.requests().is_empty());
    let gate_workspace = host.base.workspace(run, "gate");
    fs::remove_dir_all(source.join("skills/demo")).unwrap();
    fs::create_dir_all(source.join("skills/current")).unwrap();
    fs::write(source.join("skills/current/SKILL.md"), REPLACEMENT).unwrap();
    let replaced = library.install_directory("demo", &source, true).unwrap();
    assert_ne!(installed.digest, replaced.digest);
    let (status, current) = server.request("GET", "/plugins/demo", None);
    assert_eq!(status, 200, "{current}");
    assert_eq!(current["digest"], replaced.digest);
    assert_eq!(current["instructions"], REPLACEMENT);
    let preserved = FileGraphBundleLoader::new(&bundle_root)
        .with_library_root(&library_root)
        .load()
        .unwrap();
    assert_eq!(frozen.plugins, preserved.plugins);
    fs::write(gate_workspace.join("proceed"), b"continue").unwrap();
    server.wait_status(run, "completed");
    let record = host.record(run);
    assert_eq!(
        host.base.file(&record, "worker", "skill.txt"),
        REPLACEMENT.as_bytes()
    );
    assert_eq!(
        host.base.file(&record, "verify", "verified.txt"),
        REPLACEMENT.as_bytes()
    );
    assert!(!bundle_root.join("plugins").exists());
    assert!(!installed.directory.join("skills/demo/SKILL.md").exists());
    assert_eq!(
        fs::read_to_string(installed.directory.join("skills/current/SKILL.md")).unwrap(),
        REPLACEMENT
    );
    let history = host.native_conversation(run, "worker", 1);
    assert!(history.to_string().contains(REPLACEMENT));
    assert!(history.to_string().contains("final_result"));
    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    let directory = goose::tool_feedback(&requests[1]).pop().unwrap();
    assert!(directory.to_string().contains("skills/current/SKILL.md"));
    assert!(!directory.to_string().contains("skills/demo/SKILL.md"));
    let selected_skill = goose::tool_feedback(&requests[2]).pop().unwrap();
    assert!(
        selected_skill
            .to_string()
            .contains("skills/current/SKILL.md")
    );
    assert!(selected_skill.to_string().contains(REPLACEMENT));
    assert!(!selected_skill.to_string().contains(ORIGINAL));
    assert_eq!(record["plugin_bindings"], admitted["plugin_bindings"]);
    assert_eq!(
        record["results"]["worker"][0]["plugin_digests"]["demo"],
        replaced.digest
    );
    host.evidence(&provider, run, json!({
        "case_source":CASE_SOURCE,"library_install":installed,"library_replacement":replaced,
        "library_detail":current,"admitted_authorization_unchanged":true,
        "discovered_current_skill_directory":directory,"selected_current_skill":selected_skill,
        "update_during_run_uses_current_code_and_skill_paths":true,"bundle_contains_only_references":true,"readonly_mount":true,
        "native_history_checked":true,"artifact_exact_bytes":true,
        "boundary":"local directory installer plus actual public Graph API and Goose; no GitHub network, OAuth or real model"
    }));
}
