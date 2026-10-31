mod support;

use anchor_rsi::business::{artifact_identity, weekly};
use serde_json::{Value, json};
use std::{fs, io::Read, path::Path};

fn write(root: &Path, name: &str, value: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value).unwrap();
}

fn review(artifact: &Value) -> Value {
    json!({"decision":"publish","reviewed_artifact":artifact,"summary":"Approved after independent review",
        "checks":{"facts":true,"business":true,"reasoning":true,"writing":true},"issues":[]})
}

fn approve(inputs: &Path) {
    support::artifact(
        &inputs.join("write"),
        "write",
        3,
        &["report.md", "sources.md", "assets"],
    );
    write(
        inputs,
        "gate/review.json",
        &review(&artifact_identity(&inputs.join("write")).unwrap()).to_string(),
    );
    support::artifact(
        &inputs.join("gate"),
        "gate",
        2,
        &["review.json", "review.md"],
    );
}

fn issue(status: &str) -> Value {
    json!({"id":"R1","status":status,"location":"opening","evidence":"quoted draft claim","impact":"reader cannot identify progress",
        "required_change":"identify the supported change","acceptance":"reader can restate the outcome","resolution":"checked revised opening against source"})
}

#[test]
fn review_requires_current_artifact_all_checks_and_carried_issue_resolutions() {
    let artifact = json!({"artifact":{"id":"current","node_id":"write","invocation":1},"manifest_sha256":"current-manifest"});
    let mut current = review(&artifact);
    assert_eq!(
        weekly::validate_review(&current, &json!({}), &artifact).unwrap(),
        "publish"
    );
    assert!(
        weekly::validate_review(
            &current,
            &json!({}),
            &json!({"artifact":{"id":"new-draft"},"manifest_sha256":"new-manifest"})
        )
        .unwrap_err()
        .contains("current manuscript")
    );
    let previous = json!({"issues":[issue("open")]});
    assert!(
        weekly::validate_review(&current, &previous, &artifact)
            .unwrap_err()
            .contains("carried forward")
    );
    current["issues"] = json!([issue("resolved")]);
    assert!(weekly::validate_review(&current, &previous, &artifact).is_ok());
    current["issues"][0]["resolution"] = json!("");
    assert!(weekly::validate_review(&current, &previous, &artifact).is_err());
    current["issues"] = json!([issue("open")]);
    assert!(weekly::validate_review(&current, &previous, &artifact).is_err());
    current["decision"] = json!("write");
    current["checks"]["writing"] = json!(false);
    assert_eq!(
        weekly::validate_review(&current, &previous, &artifact).unwrap(),
        "write"
    );
    current["checks"]["facts"] = json!(false);
    assert!(weekly::validate_review(&current, &previous, &artifact).is_err());
    current["decision"] = json!("understand");
    assert_eq!(
        weekly::validate_review(&current, &previous, &artifact).unwrap(),
        "understand"
    );
    current["checks"]["facts"] = json!("true");
    assert!(weekly::validate_review(&current, &previous, &artifact).is_err());
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
fn report_assembly_rechecks_artifact_links_and_svg_before_producing_output() {
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
    write(
        &inputs,
        "write/assets/nested/detail.svg",
        "<svg><text>Detail</text></svg>",
    );
    let draft = inputs.join("write");
    support::artifact(&draft, "write", 1, &["report.md", "sources.md", "assets"]);
    let head = artifact_identity(&draft).unwrap();
    write(&inputs, "gate/review.json", &review(&head).to_string());
    write(&inputs, "gate/review.md", "Independent checks passed\n");
    support::artifact(
        &inputs.join("gate"),
        "gate",
        1,
        &["review.json", "review.md"],
    );
    fs::create_dir(&output).unwrap();
    weekly::assemble(&inputs, &output).unwrap();
    assert_eq!(
        fs::read(output.join("assets/stages.svg")).unwrap(),
        fs::read(draft.join("assets/stages.svg")).unwrap()
    );
    let mut archive = tar::Archive::new(fs::File::open(output.join("assets.tar")).unwrap());
    let mut paths = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        assert!(entry.header().entry_type().is_file());
        assert!(entry.link_name().unwrap().is_none());
        let path = entry.path().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, fs::read(output.join(&path)).unwrap());
        paths.push(path);
    }
    assert_eq!(
        paths,
        [
            Path::new("assets/nested/detail.svg"),
            Path::new("assets/stages.svg")
        ]
    );
    drop(archive);
    write(&output, "notes.txt", "Keep workspace notes\n");
    fs::remove_file(draft.join("assets/nested/detail.svg")).unwrap();
    write(
        &inputs,
        "write/assets/current.svg",
        "<svg><text>Current</text></svg>",
    );
    approve(&inputs);
    weekly::assemble(&inputs, &output).unwrap();
    assert!(!output.join("assets/nested").exists());
    assert_eq!(
        fs::read(output.join("assets/current.svg")).unwrap(),
        fs::read(draft.join("assets/current.svg")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(output.join("notes.txt")).unwrap(),
        "Keep workspace notes\n"
    );
    let mut archive = tar::Archive::new(fs::File::open(output.join("assets.tar")).unwrap());
    let paths = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            Path::new("assets/current.svg"),
            Path::new("assets/stages.svg")
        ]
    );
    weekly::assemble(&inputs, &draft).unwrap();
    assert_eq!(
        fs::read_to_string(draft.join("assets/current.svg")).unwrap(),
        "<svg><text>Current</text></svg>"
    );
    #[cfg(unix)]
    {
        let unsafe_output = temp.path().join("unsafe-output");
        write(temp.path(), "external.tar", "must not overwrite");
        fs::create_dir(&unsafe_output).unwrap();
        std::os::unix::fs::symlink(
            temp.path().join("external.tar"),
            unsafe_output.join("assets.tar"),
        )
        .unwrap();
        assert!(
            weekly::assemble(&inputs, &unsafe_output)
                .unwrap_err()
                .contains("symlink")
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("external.tar")).unwrap(),
            "must not overwrite"
        );
        fs::remove_file(unsafe_output.join("assets.tar")).unwrap();
        std::os::unix::fs::symlink(draft.join("assets"), unsafe_output.join("assets")).unwrap();
        assert!(
            weekly::assemble(&inputs, &unsafe_output)
                .unwrap_err()
                .contains("symlink")
        );
        assert!(draft.join("assets/current.svg").exists());
    }
    write(
        &inputs,
        "write/report.md",
        "# Revised report\n\n![项目阶段变化](assets/stages.svg)\n",
    );
    support::artifact(&draft, "write", 2, &["report.md", "sources.md", "assets"]);
    assert!(
        weekly::assemble(&inputs, &output)
            .unwrap_err()
            .contains("current manuscript")
    );
    write(
        &inputs,
        "gate/review.json",
        &review(&artifact_identity(&draft).unwrap()).to_string(),
    );
    write(
        &inputs,
        "write/assets/stages.svg",
        "<svg><script>unsafe</script></svg>",
    );
    approve(&inputs);
    assert!(weekly::assemble(&inputs, &output).is_err());
    write(
        &inputs,
        "write/report.md",
        "# Report\n\n![wrong](assets/../../gate/review.md)\n",
    );
    approve(&inputs);
    assert!(weekly::assemble(&inputs, &output).is_err());
    assert!(
        !fs::read_to_string(output.join("report.md"))
            .unwrap()
            .contains("Revised")
    );
    assert_eq!(
        fs::read_to_string(output.join("assets/stages.svg")).unwrap(),
        "<svg><text>阶段变化</text></svg>"
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
    support::artifact(&draft, "write", 1, &["report.md", "sources.md", "assets"]);
    write(
        &inputs,
        "gate/review.json",
        &review(&artifact_identity(&draft).unwrap()).to_string(),
    );
    write(&inputs, "gate/review.md", "Approved");
    support::artifact(
        &inputs.join("gate"),
        "gate",
        1,
        &["review.json", "review.md"],
    );
    assert!(
        weekly::assemble(&inputs, &temp.path().join("output"))
            .unwrap_err()
            .contains("symlink")
    );
    assert!(!temp.path().join("output/assets.tar").exists());
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
    let roots = [("codex".into(), codex_root), ("deepseek".into(), deepseek)];
    let index = weekly::collect(&roots, &output, Some("2026-10-08T09:00:00+08:00")).unwrap();
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
    write(&output, "notes.txt", "Keep workspace notes\n");
    write(
        &output,
        "codex-manual.jsonl",
        "Manually retained evidence\n",
    );
    let repeated = weekly::collect(&roots, &output, Some("2026-10-20T09:00:00+08:00")).unwrap();
    assert_eq!(repeated["status"], "no_events");
    assert!(repeated["sessions"].as_array().unwrap().is_empty());
    for session in index["sessions"].as_array().unwrap() {
        let filename = session["file"].as_str().unwrap();
        assert!(!output.join(filename).exists());
        assert!(
            !output
                .join(filename.replace(".jsonl", ".overview.jsonl"))
                .exists()
        );
    }
    for (name, expected) in [
        ("notes.txt", "Keep workspace notes\n"),
        ("codex-manual.jsonl", "Manually retained evidence\n"),
    ] {
        assert_eq!(fs::read_to_string(output.join(name)).unwrap(), expected);
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(output.join("index.json")).unwrap()).unwrap(),
        repeated
    );
    assert!(
        weekly::collect(
            &[],
            &temp.path().join("bad-window"),
            Some("2026-10-08T09:00:00")
        )
        .is_err()
    );
}

#[test]
fn collected_evidence_never_keeps_the_gateway_sensitive_adjacency() {
    let temp = tempfile::tempdir().unwrap();
    let codex_root = temp.path().join("codex");
    let claude_root = temp.path().join("claude");
    let deepseek_root = temp.path().join("deepseek");
    let time = "2026-10-02T09:00:00+08:00";
    let message = "继续\u{62d2}\u{7edd}\u{5bc6}钥默认值；随机\u{5bc6}钥；\u{79d8}\u{5bc6}字段";
    let tool_text = "工具记录：\u{7edd}\u{5bc6}资料、\u{673a}\u{5bc6}资料、\u{79d8}\u{5bc6}字段";
    write_records(
        &codex_root,
        "session.jsonl",
        &[
            codex(time, "final", message),
            json!({"timestamp":time,"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":tool_text}}),
            json!({"timestamp":time,"type":"response_item","payload":{"type":"function_call_output","output":tool_text}}),
        ],
    );
    write_records(
        &claude_root,
        "project/session.jsonl",
        &[
            claude(
                "assistant",
                "message",
                json!([{"type":"text","text":message}]),
            ),
            claude(
                "assistant",
                "call",
                json!([{"type":"tool_use","id":"call","name":"Bash","input":{"command":tool_text}}]),
            ),
            claude(
                "user",
                "result",
                json!([{"type":"tool_result","tool_use_id":"call","content":tool_text}]),
            ),
        ],
    );
    write_records(
        &deepseek_root,
        "project/session/session.jsonl",
        &[
            json!({"time":time,"type":"assistant/message","data":{"content":[{"type":"text","text":message}]}}),
            json!({"time":time,"type":"tool/call","data":{"name":"shell","arguments":tool_text}}),
            json!({"time":time,"type":"tool/result","data":{"content":[{"type":"text","text":tool_text}]}}),
        ],
    );
    let output = temp.path().join("evidence");
    let index = weekly::collect(
        &[
            ("codex".into(), codex_root),
            ("claude".into(), claude_root),
            ("deepseek".into(), deepseek_root),
        ],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    assert_eq!(index["sessions"].as_array().unwrap().len(), 3);
    assert!(index["warnings"].as_array().unwrap().is_empty());
    for session in index["sessions"].as_array().unwrap() {
        let filename = session["file"].as_str().unwrap();
        for (name, expected_entries) in [
            (filename.to_owned(), 3),
            (filename.replace(".jsonl", ".overview.jsonl"), 1),
        ] {
            let evidence = fs::read_to_string(output.join(name)).unwrap();
            assert_eq!(evidence.lines().count(), expected_entries);
            for head in ['\u{7edd}', '\u{673a}', '\u{79d8}'] {
                assert!(!evidence.contains(&format!("{head}\u{5bc6}")));
            }
            assert!(evidence.contains("继续拒绝该密钥默认值"));
            assert!(evidence.contains("随机·密钥；秘·密字段"));
            if expected_entries == 3 {
                assert_eq!(
                    evidence
                        .matches("工具记录：绝·密资料、机·密资料、秘·密字段")
                        .count(),
                    2
                );
            }
        }
    }
}

fn write_records(root: &Path, name: &str, records: &[Value]) {
    write(
        root,
        name,
        &records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>(),
    );
}

fn evidence_entries(index: &Value, output: &Path) -> Vec<Value> {
    index["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|session| {
            fs::read_to_string(output.join(session["file"].as_str().unwrap()))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn claude(role: &str, uuid: &str, content: Value) -> Value {
    json!({"type":role,"uuid":uuid,"timestamp":"2026-10-02T09:00:00+08:00",
        "sessionId":"claude-session","cwd":"/fixture/project","isSidechain":false,
        "message":{"role":role,"id":"same-provider-message-id","content":content}})
}

#[test]
fn claude_collects_public_text_and_tools_without_thinking_sidechains_or_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("claude");
    let mut sidechain = claude("assistant", "sidechain", json!("excluded sidechain"));
    sidechain["isSidechain"] = json!(true);
    sidechain["sessionId"] = json!("not-main-session");
    sidechain["cwd"] = json!("/excluded/project");
    let mut meta = claude("user", "meta", json!("excluded meta instruction"));
    meta["isMeta"] = json!(true);
    let mut end = claude("assistant", "end", json!("excluded at end"));
    end["timestamp"] = json!("2026-10-08T09:00:00+08:00");
    let records = [
        claude("user", "user-string", json!("public request")),
        claude(
            "assistant",
            "assistant-mixed",
            json!([
                {"type":"thinking","thinking":"excluded reasoning","signature":"excluded signature"},
                {"type":"text","text":"public explanation"},
                {"type":"tool_use","id":"call-1","name":"Bash","input":{"command":"cargo test","password":"must-hide"}}
            ]),
        ),
        claude(
            "assistant",
            "assistant-followup",
            json!([{"type":"text","text":"public followup"}]),
        ),
        claude(
            "user",
            "tool-array",
            json!([
                {"type":"text","text":"public result context"},
                {"type":"tool_result","tool_use_id":"call-1","is_error":false,"content":[
                    {"type":"text","text":"tests passed"},
                    {"type":"thinking","text":"excluded tool reasoning"},
                    {"type":"image","source":{"data":"excluded image"}}
                ]}
            ]),
        ),
        claude(
            "user",
            "tool-string",
            json!([{"type":"tool_result","tool_use_id":"call-2","is_error":true,"content":"public tool failure"}]),
        ),
        claude(
            "assistant",
            "thinking-only",
            json!([{"type":"redacted_thinking","data":"excluded redacted thinking"}]),
        ),
        claude("system", "system", json!("excluded system prompt")),
        sidechain,
        meta,
        end,
    ];
    write_records(&root, "project/a-main.jsonl", &records);
    write_records(&root, "project/z-copy.jsonl", &records);
    write_records(
        &root,
        "project/claude-session/subagents/nested/agent-child.jsonl",
        &[claude(
            "assistant",
            "child",
            json!("excluded nested subagent"),
        )],
    );
    write_records(
        &root,
        "project/agent-legacy.jsonl",
        &[claude(
            "assistant",
            "legacy",
            json!("excluded legacy subagent"),
        )],
    );
    let output = temp.path().join("output");
    let index = weekly::collect(
        &[("claude".into(), root)],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    assert_eq!(index["sources"]["claude"]["status"], "available");
    assert_eq!(index["sources"]["claude"]["files_scanned"], 2);
    assert_eq!(index["sources"]["claude"]["events_in_window"], 7);
    assert_eq!(index["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(index["sessions"][0]["session"], "claude-session");
    assert_eq!(index["sessions"][0]["project"], "/fixture/project");
    let entries = evidence_entries(&index, &output);
    assert_eq!(entries[0]["line"], 1);
    assert_eq!(entries[2]["line"], 2);
    assert_eq!(entries[2]["kind"], "tool_use");
    assert_eq!(entries[2]["uuid"], "assistant-mixed");
    assert_eq!(entries[5]["kind"], "tool_result");
    let tool: Value = serde_json::from_str(entries[5]["text"].as_str().unwrap()).unwrap();
    assert_eq!(tool["tool_use_id"], "call-1");
    assert_eq!(tool["content"], "tests passed");
    let all = serde_json::to_string(&entries).unwrap();
    for text in [
        "public request",
        "public explanation",
        "public followup",
        "cargo test",
        "public tool failure",
        "[REDACTED]",
    ] {
        assert!(all.contains(text), "{text}");
    }
    assert!(!all.contains("excluded"));
    assert!(!all.contains("must-hide"));
    let session_file = index["sessions"][0]["file"].as_str().unwrap();
    let overview =
        fs::read_to_string(output.join(session_file.replace(".jsonl", ".overview.jsonl"))).unwrap();
    assert!(!overview.contains("tool_use") && !overview.contains("tests passed"));
}

#[test]
fn claude_tool_details_and_message_overviews_keep_explicit_limits() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("claude");
    write_records(
        &root,
        "session.jsonl",
        &[
            claude("assistant", "long-text", json!("界".repeat(2000))),
            claude(
                "user",
                "long-tool",
                json!([{"type":"tool_result","tool_use_id":"long-call","content":"界".repeat(4100)}]),
            ),
        ],
    );
    let output = temp.path().join("output");
    let index = weekly::collect(
        &[("claude".into(), root)],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    let entries = evidence_entries(&index, &output);
    assert_eq!(entries[0]["text"].as_str().unwrap().chars().count(), 2000);
    assert!(entries[1]["text"].as_str().unwrap().contains("截断"));
    assert!(entries[1]["text"].as_str().unwrap().chars().count() < 4100);
    let filename = index["sessions"][0]["file"]
        .as_str()
        .unwrap()
        .replace(".jsonl", ".overview.jsonl");
    let overview = fs::read_to_string(output.join(filename)).unwrap();
    let overview: Value = serde_json::from_str(overview.trim()).unwrap();
    assert!(overview["text"].as_str().unwrap().contains("概览截断"));
    assert!(overview["text"].as_str().unwrap().chars().count() < 2000);
}

#[test]
fn collection_reads_all_three_sources_and_dsh_millisecond_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let codex_root = temp.path().join("codex");
    let claude_root = temp.path().join("claude");
    let deepseek_root = temp.path().join("deepseek");
    write_records(
        &codex_root,
        "session.jsonl",
        &[
            json!({"type":"session_meta","payload":{"cwd":"/codex/project","id":"codex-session"}}),
            codex("2026-10-02T09:00:00+08:00", "final", "Codex outcome"),
        ],
    );
    write_records(
        &claude_root,
        "project/session.jsonl",
        &[claude("assistant", "public", json!("Claude outcome"))],
    );
    let time = chrono::DateTime::parse_from_rfc3339("2026-10-02T09:00:00+08:00")
        .unwrap()
        .timestamp_millis();
    write_records(
        &deepseek_root,
        "project/session/session.v4.jsonl",
        &[
            json!({"type":"session","version":4,"id":"dsh-session","createdAt":time,"cwd":"/deepseek/project"}),
            json!({"type":"user/message","seq":0,"time":time,"data":{"content":"DSH request"}}),
            json!({"type":"assistant/message","seq":1,"time":time,"data":{"message":{"content":[{"type":"text","text":"DSH outcome"},{"type":"thinking","text":"hidden DSH thinking"}]},"stream":[{"text":"hidden raw stream"}]}}),
            json!({"type":"tool/call","seq":2,"time":time,"data":{"callId":"dsh-tool","name":"shell","arguments":"cargo test"}}),
            json!({"type":"tool/result","seq":3,"time":time,"data":{"message":{"content":[{"type":"text","text":"DSH tool result"}]}}}),
        ],
    );
    let output = temp.path().join("evidence");
    let index = weekly::collect(
        &[
            ("codex".into(), codex_root),
            ("claude".into(), claude_root),
            ("deepseek".into(), deepseek_root),
        ],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    assert_eq!(index["status"], "collected");
    assert_eq!(index["sources"]["codex"]["events_in_window"], 1);
    assert_eq!(index["sources"]["claude"]["events_in_window"], 1);
    assert_eq!(index["sources"]["deepseek"]["events_in_window"], 4);
    assert!(index["warnings"].as_array().unwrap().is_empty());
    let session = index["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["source"] == "deepseek")
        .unwrap();
    assert_eq!(session["session"], "dsh-session");
    assert_eq!(session["project"], "/deepseek/project");
    let all = serde_json::to_string(&evidence_entries(&index, &output)).unwrap();
    assert!(!all.contains("hidden"));
    assert!(all.contains("DSH tool result"));
}

#[test]
fn unavailable_sources_warn_and_do_not_block_available_history() {
    let temp = tempfile::tempdir().unwrap();
    let codex_root = temp.path().join("codex");
    write_records(
        &codex_root,
        "session.jsonl",
        &[codex("2026-10-02T09:00:00+08:00", "final", "available")],
    );
    write(temp.path(), "not-directory", "not a history root");
    let output = temp.path().join("output");
    let index = weekly::collect(
        &[
            ("codex".into(), codex_root),
            ("claude".into(), temp.path().join("not-directory")),
            ("deepseek".into(), temp.path().join("not-mounted")),
        ],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    assert_eq!(index["status"], "collected");
    assert_eq!(index["sources"]["codex"]["status"], "available");
    for source in ["claude", "deepseek"] {
        assert_eq!(index["sources"][source]["status"], "unavailable");
        assert_eq!(index["sources"][source]["events_in_window"], 0);
        assert!(
            index["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["source"] == source)
        );
    }
    assert!(output.join("index.json").is_file());
    assert!(!output.join("report.md").exists());
}

#[test]
fn empty_collection_writes_truthful_index_without_report() {
    let temp = tempfile::tempdir().unwrap();
    for (name, roots) in [
        ("none-configured", vec![]),
        (
            "none-mounted",
            vec![("claude".into(), temp.path().join("not-mounted"))],
        ),
    ] {
        let output = temp.path().join(name);
        let index = weekly::collect(&roots, &output, Some("2026-10-08T09:00:00+08:00")).unwrap();
        assert_eq!(index["status"], "no_sources");
        assert_eq!(index["sessions"], json!([]));
        assert!(!index["warnings"].as_array().unwrap().is_empty());
        assert!(output.join("index.json").is_file());
        assert!(!output.join("report.md").exists());
    }
    let root = temp.path().join("empty");
    write_records(
        &root,
        "session.jsonl",
        &[
            codex("2026-09-01T09:00:00+08:00", "final", "old event"),
            codex("2026-10-02T09:00:00+08:00", "analysis", "hidden event"),
        ],
    );
    let output = temp.path().join("no-events");
    let index = weekly::collect(
        &[("codex".into(), root)],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    assert_eq!(index["status"], "no_events");
    assert_eq!(index["sources"]["codex"]["status"], "available");
    assert_eq!(index["sources"]["codex"]["events_in_window"], 0);
    assert_eq!(fs::read_dir(output).unwrap().count(), 1);
}

#[test]
fn malformed_lines_and_public_timestamps_are_partial_not_fatal() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("claude");
    let valid = claude("assistant", "valid", json!("retained event"));
    let mut missing = claude("user", "missing", json!("missing timestamp"));
    missing.as_object_mut().unwrap().remove("timestamp");
    let mut invalid = claude("user", "invalid", json!("invalid timestamp"));
    invalid["timestamp"] = json!("not-a-date");
    let mut bytes = format!("broken JSON\nnull\n{missing}\n{invalid}\n").into_bytes();
    bytes.extend_from_slice(b"\xff\xfe\n");
    bytes.extend_from_slice(format!("{valid}\n").as_bytes());
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("session.jsonl"), bytes).unwrap();
    let output = temp.path().join("output");
    let index = weekly::collect(
        &[("claude".into(), root)],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    assert_eq!(index["sources"]["claude"]["status"], "partial");
    assert_eq!(index["sources"]["claude"]["warning_count"], 5);
    assert_eq!(index["sources"]["claude"]["events_in_window"], 1);
    assert_eq!(evidence_entries(&index, &output)[0]["line"], 6);
    for warning in index["warnings"].as_array().unwrap() {
        assert_eq!(warning["source"], "claude");
        assert!(warning["line"].as_u64().unwrap() <= 5);
    }
}

fn compress_records(root: &Path, name: &str, records: &[Value]) -> Vec<u8> {
    let payload = records
        .iter()
        .map(|record| format!("{record}\n"))
        .collect::<String>();
    let bytes = zstd::stream::encode_all(payload.as_bytes(), 0).unwrap();
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, &bytes).unwrap();
    bytes
}

#[test]
fn deepseek_generation_selection_is_numeric_and_supports_compressed_v4() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("deepseek");
    let record = |text: &str| json!({"type":"assistant/message","time":"2026-10-02T09:00:00+08:00","data":{"message":{"content":[{"type":"text","text":text}]}}});
    write_records(&root, "plain/session.jsonl", &[record("excluded v0")]);
    write_records(&root, "plain/session.v9.jsonl", &[record("excluded v9")]);
    write_records(
        &root,
        "plain/session.v10.jsonl",
        &[record("current plaintext generation")],
    );
    write_records(
        &root,
        "plain/session.v010.jsonl",
        &[record("excluded noncanonical")],
    );
    write_records(
        &root,
        "plain/session.v0.jsonl",
        &[record("excluded tagged zero")],
    );
    write_records(
        &root,
        "plain/session.v11.jsonl.tmp",
        &[record("excluded temporary")],
    );
    write_records(
        &root,
        "compressed/session.v3.jsonl",
        &[record("excluded compressed predecessor")],
    );
    compress_records(
        &root,
        "compressed/session.v4.jsonl.zstd",
        &[record("current compressed generation")],
    );
    let output = temp.path().join("output");
    let index = weekly::collect(
        &[("deepseek".into(), root)],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    assert_eq!(index["sources"]["deepseek"]["files_scanned"], 2);
    assert_eq!(index["sources"]["deepseek"]["events_in_window"], 2);
    assert_eq!(index["sources"]["deepseek"]["status"], "available");
    let all = serde_json::to_string(&evidence_entries(&index, &output)).unwrap();
    assert!(all.contains("current plaintext generation"));
    assert!(all.contains("current compressed generation"));
    assert!(!all.contains("excluded"));
}

#[test]
fn deepseek_corrupt_compressed_history_retains_complete_prefix_and_continues() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("deepseek");
    let record = |text: &str| json!({"type":"user/message","time":"2026-10-02T09:00:00+08:00","data":{"content":text}});
    let mut compressed = compress_records(
        &root,
        "partial/session.v4.jsonl.zstd",
        &[record("complete compressed prefix")],
    );
    let next = zstd::stream::encode_all(format!("{}\n", record("incomplete frame")).as_bytes(), 0)
        .unwrap();
    compressed.extend_from_slice(&next[..next.len() / 2]);
    fs::write(root.join("partial/session.v4.jsonl.zstd"), compressed).unwrap();
    write(
        &root,
        "corrupt/session.v4.jsonl.zstd",
        "not a zstandard stream",
    );
    write_records(
        &root,
        "valid/session.v4.jsonl",
        &[record("valid sibling history")],
    );
    let output = temp.path().join("output");
    let index = weekly::collect(
        &[("deepseek".into(), root)],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    assert_eq!(index["sources"]["deepseek"]["status"], "partial");
    assert_eq!(index["sources"]["deepseek"]["files_scanned"], 3);
    let all = serde_json::to_string(&evidence_entries(&index, &output)).unwrap();
    assert!(all.contains("complete compressed prefix"));
    assert!(all.contains("valid sibling history"));
    assert!(index["warnings"].as_array().unwrap().len() >= 2);
    assert!(
        index["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| warning["source"] == "deepseek")
    );
}

#[cfg(unix)]
#[test]
fn collection_never_follows_source_roots_ancestors_or_history_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let outside = temp.path().join("outside");
    write_records(
        &outside,
        "private.jsonl",
        &[claude(
            "assistant",
            "private",
            json!("outside must not be read"),
        )],
    );
    let linked = temp.path().join("linked");
    std::os::unix::fs::symlink(&outside, &linked).unwrap();
    let root = temp.path().join("claude");
    write_records(
        &root,
        "public.jsonl",
        &[claude("assistant", "public", json!("public retained"))],
    );
    std::os::unix::fs::symlink(outside.join("private.jsonl"), root.join("file-link.jsonl"))
        .unwrap();
    std::os::unix::fs::symlink(&outside, root.join("dir-link")).unwrap();
    let output = temp.path().join("output");
    let index = weekly::collect(
        &[
            ("codex".into(), linked.clone()),
            ("claude".into(), root),
            ("deepseek".into(), linked.join("nested")),
        ],
        &output,
        Some("2026-10-08T09:00:00+08:00"),
    )
    .unwrap();
    assert_eq!(index["sources"]["codex"]["status"], "unavailable");
    assert_eq!(index["sources"]["deepseek"]["status"], "unavailable");
    assert_eq!(index["sources"]["claude"]["status"], "partial");
    assert_eq!(index["sources"]["claude"]["events_in_window"], 1);
    assert!(
        !serde_json::to_string(&evidence_entries(&index, &output))
            .unwrap()
            .contains("outside must not be read")
    );
    let linked_output = temp.path().join("linked-output");
    std::os::unix::fs::symlink(&output, &linked_output).unwrap();
    assert!(
        weekly::collect(&[], &linked_output, Some("2026-10-08T09:00:00+08:00"))
            .unwrap_err()
            .contains("symlink")
    );
}

#[cfg(unix)]
#[test]
fn unreadable_histories_are_partial_and_unreadable_roots_unavailable() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("codex");
    write_records(
        &root,
        "readable.jsonl",
        &[codex(
            "2026-10-02T09:00:00+08:00",
            "final",
            "readable event",
        )],
    );
    write(&root, "unreadable.jsonl", "not readable");
    let unreadable = root.join("unreadable.jsonl");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    let inaccessible = temp.path().join("inaccessible");
    fs::create_dir(&inaccessible).unwrap();
    fs::set_permissions(&inaccessible, fs::Permissions::from_mode(0o000)).unwrap();
    let permissions_enforced =
        fs::read(&unreadable).is_err() && fs::read_dir(&inaccessible).is_err();
    let result = weekly::collect(
        &[
            ("codex".into(), root),
            ("claude".into(), inaccessible.clone()),
        ],
        &temp.path().join("output"),
        Some("2026-10-08T09:00:00+08:00"),
    );
    fs::set_permissions(unreadable, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(inaccessible, fs::Permissions::from_mode(0o700)).unwrap();
    let index = result.unwrap();
    if permissions_enforced {
        assert_eq!(index["sources"]["codex"]["status"], "partial");
        assert_eq!(index["sources"]["claude"]["status"], "unavailable");
        assert_eq!(index["sources"]["codex"]["files_read"], 1);
    }
    assert_eq!(index["sources"]["codex"]["events_in_window"], 1);
}
