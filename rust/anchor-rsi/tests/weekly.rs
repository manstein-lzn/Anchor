use anchor_rsi::business::{git_head, weekly};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn write(root: &Path, name: &str, value: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value).unwrap();
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
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
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn review(commit: &str) -> Value {
    json!({"decision":"publish","reviewed_commit":commit,"summary":"Approved after independent review",
        "checks":{"facts":true,"business":true,"reasoning":true,"writing":true},"issues":[]})
}

fn issue(status: &str) -> Value {
    json!({"id":"R1","status":status,"location":"opening","evidence":"quoted draft claim","impact":"reader cannot identify progress",
        "required_change":"identify the supported change","acceptance":"reader can restate the outcome","resolution":"checked revised opening against source"})
}

#[test]
fn review_requires_current_commit_all_checks_and_carried_issue_resolutions() {
    let mut current = review("current");
    assert_eq!(
        weekly::validate_review(&current, &json!({}), "current").unwrap(),
        "publish"
    );
    assert!(
        weekly::validate_review(&current, &json!({}), "new-draft")
            .unwrap_err()
            .contains("current manuscript")
    );
    let previous = json!({"issues":[issue("open")]});
    assert!(
        weekly::validate_review(&current, &previous, "current")
            .unwrap_err()
            .contains("carried forward")
    );
    current["issues"] = json!([issue("resolved")]);
    assert!(weekly::validate_review(&current, &previous, "current").is_ok());
    current["issues"][0]["resolution"] = json!("");
    assert!(weekly::validate_review(&current, &previous, "current").is_err());
    current["issues"] = json!([issue("open")]);
    assert!(weekly::validate_review(&current, &previous, "current").is_err());
    current["decision"] = json!("write");
    current["checks"]["writing"] = json!(false);
    assert_eq!(
        weekly::validate_review(&current, &previous, "current").unwrap(),
        "write"
    );
    current["checks"]["facts"] = json!(false);
    assert!(weekly::validate_review(&current, &previous, "current").is_err());
    current["decision"] = json!("understand");
    assert_eq!(
        weekly::validate_review(&current, &previous, "current").unwrap(),
        "understand"
    );
    current["checks"]["facts"] = json!("true");
    assert!(weekly::validate_review(&current, &previous, "current").is_err());
}

#[test]
fn svg_preserves_static_diagrams_and_refuses_active_or_external_content() {
    for svg in [
        r##"<svg xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="fill"/></defs><rect fill="url(#fill)"/><text>阶段变化</text></svg>"##,
        r##"<svg xmlns:xlink="http://www.w3.org/1999/xlink"><path id="shape"/><use xlink:href="#shape"/></svg>"##,
    ] {
        assert!(weekly::validate_svg(svg).is_ok(), "{svg}");
    }
    for svg in [
        "<svg><script>secret</script></svg>",
        "<svg><foreignObject/></svg>",
        "<svg onload='alert(1)'/>",
        "<svg><image href='https://example.invalid/image'/></svg>",
        "<svg><style>@import 'remote';</style></svg>",
        "<svg><rect fill='url(https://example.invalid)'/></svg>",
        "<svg><animate attributeName='href' values='https://example.invalid'/></svg>",
        "<!DOCTYPE svg [<!ENTITY value 'private'>]><svg>&value;</svg>",
        "<svg><g></svg>",
        "<html/>",
    ] {
        assert!(weekly::validate_svg(svg).is_err(), "accepted {svg}");
    }
}

#[test]
fn report_assembly_rechecks_commit_links_and_svg_before_producing_output() {
    let temp = tempfile::tempdir().unwrap();
    let inputs = temp.path().join("in");
    let output = temp.path().join("output");
    write(
        &inputs,
        "write/report.md",
        "# 本周进展\n\n![项目阶段变化](assets/stages.svg)\n",
    );
    write(
        &inputs,
        "write/sources.md",
        "Frozen evidence and limitations\n",
    );
    write(
        &inputs,
        "write/assets/stages.svg",
        "<svg><text>阶段变化</text></svg>",
    );
    let draft = inputs.join("write");
    git(&draft, &["init", "--quiet"]);
    git(&draft, &["add", "."]);
    git(&draft, &["commit", "--quiet", "-m", "first draft"]);
    let head = git_head(&draft).unwrap();
    write(&inputs, "gate/review.json", &review(&head).to_string());
    write(&inputs, "gate/review.md", "Independent checks passed\n");
    fs::create_dir(&output).unwrap();
    weekly::assemble(&inputs, &output).unwrap();
    assert_eq!(
        fs::read(output.join("assets/stages.svg")).unwrap(),
        fs::read(draft.join("assets/stages.svg")).unwrap()
    );
    write(
        &inputs,
        "write/report.md",
        "# Revised report\n\n![项目阶段变化](assets/stages.svg)\n",
    );
    git(&draft, &["add", "."]);
    git(&draft, &["commit", "--quiet", "-m", "revised draft"]);
    assert!(
        weekly::assemble(&inputs, &output)
            .unwrap_err()
            .contains("current manuscript")
    );
    write(
        &inputs,
        "gate/review.json",
        &review(&git_head(&draft).unwrap()).to_string(),
    );
    write(
        &inputs,
        "write/assets/stages.svg",
        "<svg><script>unsafe</script></svg>",
    );
    assert!(weekly::assemble(&inputs, &output).is_err());
    write(
        &inputs,
        "write/report.md",
        "# Report\n\n![wrong](assets/../../gate/review.md)\n",
    );
    assert!(weekly::assemble(&inputs, &output).is_err());
    assert!(
        !fs::read_to_string(output.join("report.md"))
            .unwrap()
            .contains("Revised")
    );
}

#[cfg(unix)]
#[test]
fn report_assembly_rejects_symlink_figures() {
    let temp = tempfile::tempdir().unwrap();
    let inputs = temp.path().join("in");
    write(
        &inputs,
        "write/report.md",
        "# Report\n![figure](assets/figure.svg)\n",
    );
    write(&inputs, "write/sources.md", "Sources");
    write(temp.path(), "outside.svg", "<svg/>");
    fs::create_dir(inputs.join("write/assets")).unwrap();
    std::os::unix::fs::symlink(
        temp.path().join("outside.svg"),
        inputs.join("write/assets/figure.svg"),
    )
    .unwrap();
    let draft = inputs.join("write");
    git(&draft, &["init", "--quiet"]);
    git(&draft, &["add", "."]);
    git(&draft, &["commit", "--quiet", "-m", "fixture"]);
    write(
        &inputs,
        "gate/review.json",
        &review(&git_head(&draft).unwrap()).to_string(),
    );
    write(&inputs, "gate/review.md", "Approved");
    assert!(
        weekly::assemble(&inputs, &temp.path().join("output"))
            .unwrap_err()
            .contains("symlink")
    );
}

fn codex(time: &str, channel: &str, message: &str) -> Value {
    json!({"timestamp":time,"type":"response_item","payload":{"type":"message","role":"assistant","channel":channel,"content":[{"type":"output_text","text":message}]}})
}

#[test]
fn collection_keeps_window_latest_generation_public_messages_and_limits() {
    let temp = tempfile::tempdir().unwrap();
    let codex_root = temp.path().join("codex");
    let deepseek = temp.path().join("deepseek");
    let records = [
        codex(
            "2026-10-01T09:00:00+08:00",
            "final",
            "api_key=must-hide; outcome",
        ),
        codex("2026-10-02T09:00:00+08:00", "analysis", "hidden reasoning"),
        codex("2026-10-08T09:00:00+08:00", "final", "end is exclusive"),
        json!({"timestamp":"2026-10-02T10:00:00+08:00","type":"response_item","payload":{"type":"function_call_output","output":"界".repeat(4100)}}),
    ];
    write(
        &codex_root,
        "session.jsonl",
        &(records
            .iter()
            .map(|value| format!("{value}\n"))
            .collect::<String>()
            + "broken record\n"),
    );
    let old = json!({"time":"2026-10-02T10:00:00+08:00","type":"assistant/message","data":{"content":[{"type":"text","text":"old generation"}]}});
    let current = json!({"time":"2026-10-02T10:00:00+08:00","type":"assistant/message","data":{"content":[{"type":"text","text":"current generation"},{"type":"thinking","text":"private thought"}]}});
    write(&deepseek, "one/session.jsonl", &format!("{old}\n"));
    write(&deepseek, "one/session.v2.jsonl", &format!("{current}\n"));
    let output = temp.path().join("evidence");
    let index = weekly::collect(
        &[("codex".into(), codex_root), ("deepseek".into(), deepseek)],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    assert_eq!(index["sources"]["codex"]["events_in_window"], 2);
    assert_eq!(index["sources"]["deepseek"]["files_scanned"], 1);
    assert_eq!(index["sources"]["deepseek"]["events_in_window"], 1);
    assert_eq!(index["warnings"].as_array().unwrap().len(), 1);
    let all = index["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| fs::read_to_string(output.join(session["file"].as_str().unwrap())).unwrap())
        .collect::<String>();
    assert!(
        all.contains("current generation") && all.contains("[REDACTED]") && all.contains("截断")
    );
    for forbidden in [
        "must-hide",
        "hidden reasoning",
        "end is exclusive",
        "old generation",
        "private thought",
    ] {
        assert!(!all.contains(forbidden), "{forbidden}");
    }
    assert!(
        weekly::collect(
            &[],
            &temp.path().join("bad-window"),
            Some("2026-10-08T09:00:00")
        )
        .is_err()
    );
}
