use anchor_rsi::{
    business::{
        git_head,
        rsi::{self, FrozenEvidence},
        verify_commit,
    },
    evidence::{Config, Evidence},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn write(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn git(root: &Path, arguments: &[&str]) {
    let result = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn freeze(directory: &Path, message: &str) -> String {
    git(directory, &["init", "--quiet"]);
    git(directory, &["add", "."]);
    git(directory, &["commit", "--quiet", "-m", message]);
    git_head(directory).unwrap()
}

struct Fixture {
    temp: tempfile::TempDir,
    inputs: PathBuf,
    evidence_root: PathBuf,
    reference: Value,
    index: Value,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let inputs = temp.path().join("in");
        let source = temp.path().join("source");
        let data = temp.path().join("data");
        let previous = temp.path().join("previous");
        for root in [&inputs, &source, &data, &previous] {
            fs::create_dir(root).unwrap();
        }
        fs::write(
            source.join("policy.rs"),
            "pub const BOUNDARY: &str = \"operator grants\";\n",
        )
        .unwrap();
        write(
            &previous.join("evolution.json"),
            &json!({"proposals":[{"id":"P-before"}]}),
        );
        let evidence_root = inputs.join("collect/evidence");
        let evidence = Evidence::collect(Config {
            source,
            data,
            evidence: evidence_root.clone(),
            rust_state: None,
            previous: Some(previous),
        })
        .unwrap();
        let index: Value =
            serde_json::from_slice(&fs::read(evidence_root.join("index.json")).unwrap()).unwrap();
        let page = evidence.read("code/policy.rs", 0, 100).unwrap();
        evidence
            .audit(
                "rsi_read",
                &json!({"path":"code/policy.rs","offset":0,"limit":100}),
                true,
            )
            .unwrap();
        let reference = json!({"path":page["path"],"sha256":page["sha256"],"redacted":page["redacted"],"line_basis":page["line_basis"],"locator":"line 1","layer":"frozen_source"});
        let mut branches = Vec::new();
        for domain in ["runs", "code", "graphs", "plugins", "dependencies"] {
            let name = format!("{domain}-audit");
            let directory = inputs.join(&name);
            write(
                &directory.join("findings.json"),
                &json!({"domain":domain,"summary":"Reviewed frozen evidence with explicit limits",
                "coverage":{"read":["code/policy.rs"],"not_reviewed":["remaining evidence"],"limitations":["fixture evidence only"]},"findings":[]}),
            );
            let commit = freeze(&directory, "specialist findings");
            branches.push(json!({"output":name,"nodes":[{"node":name,"commit":commit}]}));
        }
        write(
            &inputs.join("audit-join/join.json"),
            &json!({"branches":branches}),
        );
        write(
            &inputs.join("analyze/evolution.json"),
            &json!({"proposals":[{"id":"P-new","status":"proposed","problem":"Observed contract pressure","change":"Reuse the existing port","validation":"Run the focused contract case","risk":"Behavior can change","rollback":"Restore the previous protected behavior","source_snapshot":{"captured_at":index["captured_at"]},"collection_window":{"start":index["window_start"],"end_exclusive":index["window_end"]},"source_layer":"frozen_source","claim_scope":"current_source","evidence":[reference]}],
            "carry_forward":[{"id":"P-before","status":"hold","reason":"No new implementation evidence"}]}),
        );
        fs::write(
            inputs.join("analyze/rsi-report.md"),
            "# RSI\nFrozen-source findings with explicit scope.\n",
        )
        .unwrap();
        fs::write(
            inputs.join("analyze/sources.md"),
            "code/policy.rs, frozen projection line 1\n",
        )
        .unwrap();
        let commit = freeze(&inputs.join("analyze"), "analysis");
        write(
            &inputs.join("review/review.json"),
            &json!({"passed":true,"reviewed_commit":commit,"issues":[],"coverage":["proposal source contract"],"limitations":["no real provider"]}),
        );
        fs::write(
            inputs.join("review/review.md"),
            "Approved independent review\n",
        )
        .unwrap();
        fs::create_dir(inputs.join("gate")).unwrap();
        fs::write(inputs.join("gate/gate.txt"), "PASS\ntarget=publish\n").unwrap();
        Self {
            temp,
            inputs,
            evidence_root,
            reference,
            index,
        }
    }

    fn read(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.inputs.join(name)).unwrap()).unwrap()
    }
    fn write(&self, name: &str, value: &Value) {
        write(&self.inputs.join(name), value);
    }
    fn evaluate(&self) -> String {
        rsi::evaluate(
            &self.inputs,
            Some(&FrozenEvidence::load(&self.evidence_root).unwrap()),
        )
        .0
    }
}

#[test]
fn native_gate_accepts_bound_publication_and_refuses_decision_commit_hash_and_window_mismatches() {
    let fixture = Fixture::new();
    assert_eq!(fixture.evaluate(), "publish");
    let approved = fixture.read("review/review.json");
    let mut review = approved.clone();
    review["passed"] = json!(false);
    fixture.write("review/review.json", &review);
    assert_eq!(fixture.evaluate(), "analyze");
    review = approved.clone();
    review["reviewed_commit"] = json!("wrong-commit");
    fixture.write("review/review.json", &review);
    assert_eq!(fixture.evaluate(), "analyze");
    fixture.write("review/review.json", &approved);
    let valid = fixture.read("analyze/evolution.json");
    let mut wrong = valid.clone();
    wrong["proposals"][0]["evidence"][0]["sha256"] = json!("0".repeat(64));
    fixture.write("analyze/evolution.json", &wrong);
    assert_eq!(fixture.evaluate(), "analyze");
    wrong = valid.clone();
    wrong["proposals"][0]["collection_window"]["start"] = json!("2020-01-01T00:00:00Z");
    fixture.write("analyze/evolution.json", &wrong);
    assert_eq!(fixture.evaluate(), "analyze");
    wrong = valid.clone();
    wrong["carry_forward"] = json!([]);
    fixture.write("analyze/evolution.json", &wrong);
    assert_eq!(fixture.evaluate(), "analyze");
    fixture.write("analyze/evolution.json", &valid);
    let entry = &fixture.index["entries"]["code/policy.rs"];
    fs::write(
        fixture
            .evidence_root
            .join(entry["frozen_file"].as_str().unwrap()),
        "changed frozen source",
    )
    .unwrap();
    assert_eq!(fixture.evaluate(), "analyze");
}

#[test]
fn dual_reviews_cannot_waive_one_another_or_unsafe_proposals() {
    let fixture = Fixture::new();
    let frozen_file = fixture.index["entries"]["code/policy.rs"]["frozen_file"]
        .as_str()
        .unwrap();
    let reference =
        json!({"path":format!("/in/collect/evidence/{frozen_file}"),"locator":"line 1"});
    for domain in ["runs", "code", "graphs", "plugins", "dependencies"] {
        let name = format!("{domain}-audit/findings.json");
        let mut audit = fixture.read(&name);
        audit["coverage"]["read"] = json!([reference]);
        fixture.write(&name, &audit);
    }
    fixture.write("collect/evidence/previous.json", &json!({"reports":[]}));
    fixture.write("analyze/evolution.json", &json!({"window":{"start":fixture.index["window_start"],"end_exclusive":fixture.index["window_end"]},"proposals":[],"carry_forward":[]}));
    let commit = git_head(&fixture.inputs.join("analyze")).unwrap();
    let mut branches = Vec::new();
    for (name, checks) in [
        ("fact-review", json!({"facts":true,"research":true})),
        (
            "proposal-review",
            json!({"architecture":true,"writing":true}),
        ),
    ] {
        fixture.write(&format!("{name}/review.json"), &json!({"decision":"publish","reviewed_commit":commit,"checks":checks,"issues":[],"summary":"Independent domain checks passed"}));
        fs::write(
            fixture.inputs.join(name).join("review.md"),
            "Approved independent checks",
        )
        .unwrap();
        let head = freeze(&fixture.inputs.join(name), "review");
        branches.push(json!({"output":name,"nodes":[{"node":name,"commit":head}]}));
    }
    fixture.write("review-join/join.json", &json!({"branches":branches}));
    let approved = rsi::aggregate(&fixture.inputs, &json!({})).unwrap();
    assert_eq!(approved["decision"], "publish");
    fixture.write("review/review.json", &approved);
    assert_eq!(rsi::evaluate(&fixture.inputs, None).0, "publish");
    let mut review = fixture.read("fact-review/review.json");
    review["decision"] = json!("revise");
    review["checks"]["facts"] = json!(false);
    fixture.write("fact-review/review.json", &review);
    assert_eq!(
        rsi::aggregate(&fixture.inputs, &json!({})).unwrap()["decision"],
        "revise"
    );
    assert_eq!(rsi::evaluate(&fixture.inputs, None).0, "analyze");
    fs::write(
        fixture.inputs.join("analyze/rsi-report.md"),
        "# RSI\n关闭脱敏\n",
    )
    .unwrap();
    assert!(
        rsi::aggregate(&fixture.inputs, &json!({})).unwrap()["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["id"].as_str().unwrap().starts_with("safety/"))
    );
}

#[test]
fn typed_join_reference_binds_identity_and_mounted_bytes() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("findings.json"), "{}\n").unwrap();
    let identity = format!("fs2-{}", "a".repeat(64));
    let expected = json!({"id":identity,"node_id":"code-audit","invocation":1});
    let message = format!(
        "Anchor Artifact {identity}\nArtifact-Node: code-audit\nArtifact-Invocation: 1\nManifest-SHA256: {}",
        "b".repeat(64)
    );
    freeze(temp.path(), &message);
    verify_commit(temp.path(), &expected, "code-audit").unwrap();
    let mut wrong = expected.clone();
    wrong["invocation"] = json!(2);
    assert!(verify_commit(temp.path(), &wrong, "code-audit").is_err());
    fs::write(temp.path().join("findings.json"), "{\"tampered\":true}\n").unwrap();
    assert!(
        verify_commit(temp.path(), &expected, "code-audit")
            .unwrap_err()
            .contains("mounted artifact bytes")
    );
}

#[test]
fn business_cli_publishes_only_after_rechecking_native_review_and_frozen_hashes() {
    let fixture = Fixture::new();
    let output = fixture.temp.path().join("published");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_anchor-rsi"))
            .args(["native-publish", "--inputs"])
            .arg(&fixture.inputs)
            .arg("--evidence")
            .arg(&fixture.evidence_root)
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap()
    };
    let result = run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for name in [
        "rsi-report.md",
        "evolution.json",
        "sources.md",
        "review.json",
        "review.md",
        "audit-manifest.json",
        "gate.txt",
    ] {
        assert!(output.join(name).is_file(), "{name}");
    }
    let mut review = fixture.read("review/review.json");
    review["passed"] = json!(false);
    fixture.write("review/review.json", &review);
    assert!(!run().status.success());
    review["passed"] = json!(true);
    review["reviewed_commit"] = json!("stale");
    fixture.write("review/review.json", &review);
    assert!(!run().status.success());
    review["reviewed_commit"] = json!(git_head(&fixture.inputs.join("analyze")).unwrap());
    fixture.write("review/review.json", &review);
    let mut proposals = fixture.read("analyze/evolution.json");
    proposals["proposals"][0]["evidence"][0]["sha256"] = json!("bad hash");
    fixture.write("analyze/evolution.json", &proposals);
    assert!(!run().status.success());
    let frozen = FrozenEvidence::load(&fixture.evidence_root).unwrap();
    frozen.validate_reference(&fixture.reference, true).unwrap();
}
