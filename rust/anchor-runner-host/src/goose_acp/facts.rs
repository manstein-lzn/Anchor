//! Turn one invocation's evidence into harness facts: what the host observed
//! of model turns and bridge tool calls, without their business content.

use anchor_facts::{Fact, Outcome};
use serde_json::Value;

pub(super) fn observations(
    evidence: &Value,
    started_at_ms: u64,
    ended_at_ms: u64,
    outcome: Outcome,
) -> Vec<Fact> {
    let samples = evidence["usage"]["samples"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let calls = evidence["tool_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut facts = Vec::with_capacity(samples.len() + calls.len() + 2);
    if let Ok(usage) = serde_json::from_value::<anchor_facts::ModelUsage>(evidence["usage"].clone())
    {
        facts.push(Fact::ModelUsage { usage });
    }
    for (index, sample) in samples.iter().enumerate() {
        facts.push(Fact::ModelTurn {
            index: index as u64 + 1,
            message_id: sample["message_id"].as_str().map(str::to_owned),
            observed_at_ms: sample["observed_at_ms"].as_u64(),
            elapsed_ms: sample["elapsed_ms"].as_u64(),
            time_to_first_token_ms: sample["time_to_first_token_ms"].as_u64(),
            input_tokens: sample["input_tokens"].as_u64(),
            output_tokens: sample["output_tokens"].as_u64(),
            total_tokens: sample["total_tokens"].as_u64(),
            cache_read_tokens: sample["cache_read_tokens"].as_u64(),
            cache_write_tokens: sample["cache_write_tokens"].as_u64(),
            cost_usd: sample["cost_usd"].as_f64(),
            compaction: sample["is_compaction"].as_bool().unwrap_or(false),
        });
    }
    for call in &calls {
        let ok = call["result"]["ok"].as_bool().unwrap_or(false);
        facts.push(Fact::ToolCall {
            tool: call["tool"].as_str().unwrap_or_default().to_owned(),
            tool_call_id: call["tool_call_id"].as_str().map(str::to_owned),
            model_request: call["provider_request"].as_u64().unwrap_or(0),
            arguments_shape: anchor_facts::arguments_shape(&call["arguments"]),
            path: call["arguments"]["path"]
                .as_str()
                .map(anchor_facts::path_class),
            program: anchor_facts::command_program(&call["arguments"]),
            shell_program: shell_program(&call["arguments"]),
            command: ok
                .then(|| command_result(&call["result"]))
                .flatten()
                .and_then(anchor_facts::command_end),
            started_at_ms: call["started_at_ms"].as_u64(),
            ended_at_ms: call["ended_at_ms"].as_u64(),
            ok,
            failure: (!ok).then(|| anchor_facts::classify_failure(&error_text(&call["result"]))),
        });
    }
    facts.push(Fact::Lifecycle {
        started_at_ms,
        ended_at_ms,
        outcome,
        model_turns: evidence["usage"]["messages"]
            .as_u64()
            .unwrap_or(samples.len() as u64),
        tool_calls: calls.len() as u64 + evidence["tool_calls_dropped"].as_u64().unwrap_or(0),
        tool_calls_dropped: evidence["tool_calls_dropped"].as_u64().unwrap_or(0),
        resumed: evidence["resume"].as_bool().unwrap_or(false),
    });
    facts
}

/// Heuristic attribution only (启发式), never shell execution or a full parser.
/// Quotes and common prefixes are stripped; expansions are not evaluated.
fn shell_program(arguments: &Value) -> Option<String> {
    let argv = arguments["command"].as_array()?;
    if !matches!(
        anchor_facts::command_program(arguments)?.as_str(),
        "sh" | "bash" | "dash" | "zsh" | "ash"
    ) {
        return None;
    }
    let mut args = argv.iter().skip(1);
    let script = loop {
        let flags = args.next()?.as_str()?.strip_prefix('-')?;
        if !flags.starts_with('-') && flags.contains('c') {
            break args.next()?.as_str()?;
        }
    };
    for word in shlex::Shlex::new(script) {
        let word = word.trim_start_matches(['!', '(', '{']);
        let first = word.split(['|', '&', ';', '<', '>', ')', '}']).next()?;
        let assignment = first.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty()
                && name.bytes().enumerate().all(|(i, c)| {
                    c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
                })
        });
        if first.is_empty() || assignment || matches!(first, "time" | "command" | "exec" | "env") {
            if first.len() != word.len() {
                return None;
            }
            continue;
        }
        let name = first.rsplit('/').next()?;
        return (!name.is_empty() && !first.starts_with(['$', '`'])).then(|| name.to_owned());
    }
    None
}

/// The JSON body a command tool answered with: the bridge wraps the tool's
/// own `{ok, output: [content]}` in its `output`.
fn command_result(result: &Value) -> Option<&Value> {
    let output = &result["output"];
    let contents = output["output"].as_array().or_else(|| output.as_array())?;
    contents
        .iter()
        .find(|content| content["type"] == "json")
        .map(|content| &content["value"])
}

/// Bridge errors are text, or a JSON envelope whose `error` is text.
fn error_text(result: &Value) -> String {
    let error = &result["error"];
    match error.as_str() {
        Some(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|inner| inner["error"].as_str().map(str::to_owned))
            .unwrap_or_else(|| text.to_owned()),
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_facts::Failure;
    use serde_json::json;

    #[test]
    fn bounded_turn_details_do_not_bound_persisted_usage_even_after_interruption() {
        let tally = crate::goose_acp::usage::UsageTally::default();
        for _ in 0..70 {
            tally.observe(&json!({"method":"_goose/unstable/session/update",
                "params":{"sessionUpdate":"message_usage","usage":{"totalTokens":12}}}));
        }
        let facts = observations(
            &json!({"usage":tally.metrics(false)}),
            1,
            9,
            Outcome::Interrupted,
        );
        assert!(matches!(&facts[0], Fact::ModelUsage { usage }
            if usage.messages == 70 && usage.total_tokens == Some(840) && !usage.complete
                && usage.sampled_turns == 64 && usage.omitted_turns == 6));
        assert!(matches!(
            &facts[1],
            Fact::ModelTurn {
                input_tokens: None,
                output_tokens: None,
                total_tokens: Some(12),
                ..
            }
        ));
        assert_eq!(
            facts
                .iter()
                .filter(|f| matches!(f, Fact::ModelTurn { .. }))
                .count(),
            64
        );
        assert!(matches!(
            facts.last(),
            Some(Fact::Lifecycle {
                model_turns: 70,
                ..
            })
        ));
    }

    #[test]
    fn evidence_becomes_turns_calls_and_a_lifecycle_without_business_values() {
        let evidence = json!({
            "resume": false,
            "tool_calls_dropped": 0,
            "usage": {"messages": 2, "samples": [
                {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12, "is_compaction": false,
                 "message_id": "m1", "observed_at_ms": 100, "elapsed_ms": 40, "time_to_first_token_ms": 30,
                 "cache_read_tokens": 8, "cost_usd": 0.001},
                {"input_tokens": 20, "output_tokens": 3, "total_tokens": 23, "is_compaction": true}
            ]},
            "tool_calls": [
                {"provider_request": 1, "tool": "anchor_run", "arguments": {"command": ["find", "."]},
                 "tool_call_id": "call_1", "started_at_ms": 110, "ended_at_ms": 115,
                 "result": {"ok": false, "error": "{\"ok\":false,\"error\":\"tool failed: anchor_run not_executed: command `find` is not authorized by the host sandbox policy\",\"anchor_receipt\":\"r\"}"}},
                {"provider_request": 2, "tool": "final_result", "arguments": {"summary": "done"},
                 "result": {"ok": true, "output": {"summary": "done"}}}
            ]
        });
        let facts = observations(&evidence, 90, 200, Outcome::Completed);
        assert_eq!(facts.len(), 5);
        assert!(
            matches!(&facts[0], Fact::ModelTurn { index: 1, message_id: Some(id), cache_read_tokens: Some(8), .. } if id == "m1")
        );
        assert!(matches!(
            &facts[1],
            Fact::ModelTurn {
                index: 2,
                compaction: true,
                observed_at_ms: None,
                ..
            }
        ));
        match &facts[2] {
            Fact::ToolCall {
                tool,
                model_request,
                ok,
                failure,
                started_at_ms,
                ended_at_ms,
                tool_call_id,
                ..
            } => {
                assert_eq!(tool, "anchor_run");
                assert_eq!(*model_request, 1);
                assert!(!ok);
                assert_eq!(*failure, Some(Failure::NotAuthorized));
                assert_eq!((*started_at_ms, *ended_at_ms), (Some(110), Some(115)));
                assert_eq!(tool_call_id.as_deref(), Some("call_1"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            &facts[3],
            Fact::ToolCall {
                ok: true,
                failure: None,
                ..
            }
        ));
        assert!(matches!(
            &facts[4],
            Fact::Lifecycle {
                model_turns: 2,
                tool_calls: 2,
                outcome: Outcome::Completed,
                ..
            }
        ));
        // The program's base name is kept, as the allowlist names it; no
        // other argument or result value is carried.
        assert!(
            matches!(&facts[2], Fact::ToolCall { program: Some(p), command: None, .. } if p == "find")
        );
        let recorded = format!("{facts:?}");
        assert!(!recorded.contains("sandbox policy"));
        assert!(!recorded.contains("done"));
    }

    #[test]
    fn shell_commands_record_an_attribution_hint_without_changing_the_program() {
        for (command, expected_program, expected_hint) in [
            (json!(["sh", "-c", "cargo test"]), "sh", "cargo"),
            (json!(["/bin/sh", "-eu", "-c", "cargo test"]), "sh", "cargo"),
            (
                json!(["bash", "--noprofile", "-c", "cargo test"]),
                "bash",
                "cargo",
            ),
            (json!(["sh", "-lc", "cargo test"]), "sh", "cargo"),
            (json!(["sh", "-ec", "cargo test"]), "sh", "cargo"),
            (
                json!([
                    "sh",
                    "--",
                    "-c",
                    "A=value env B=value command exec '/bin/cargo' test; echo done"
                ]),
                "sh",
                "cargo",
            ),
            (json!(["sh", "-c", "cd x && cargo test"]), "sh", "cd"),
        ] {
            let evidence = json!({"tool_calls": [{
                "tool": "anchor_run", "arguments": {"command": command},
                "result": {"ok": true}
            }]});
            let facts = observations(&evidence, 0, 1, Outcome::Completed);
            assert!(
                matches!(
                    &facts[0],
                    Fact::ToolCall { program, shell_program: Some(hint), .. }
                        if program.as_deref() == Some(expected_program) && hint == expected_hint
                ),
                "{facts:?}"
            );
        }
    }

    #[test]
    fn commands_without_a_shell_script_do_not_record_an_attribution_hint() {
        for command in [
            json!(["cargo", "test"]),
            json!(["sh", "script.sh"]),
            json!(["sh", "-c"]),
            json!(["sh", "-c", ""]),
            json!(["python3", "-c", "print(1)"]),
        ] {
            let evidence = json!({"tool_calls": [{
                "tool": "anchor_run", "arguments": {"command": command},
                "result": {"ok": true}
            }]});
            let facts = observations(&evidence, 0, 1, Outcome::Completed);
            assert!(
                matches!(
                    &facts[0],
                    Fact::ToolCall {
                        shell_program: None,
                        ..
                    }
                ),
                "{facts:?}"
            );
        }
    }

    #[test]
    fn a_command_that_did_not_start_timed_out_or_exited_nonzero_is_told_apart_from_a_clean_one() {
        let run = |argv: Value, value: Value| {
            json!({"provider_request": 1, "tool": "anchor_run", "arguments": {"command": argv},
                   "started_at_ms": 1, "ended_at_ms": 2,
                   "result": {"ok": true, "output": {"ok": true, "output": [{"type": "json", "value": value}]}}})
        };
        let evidence = json!({"usage": {"samples": []}, "tool_calls": [
            run(json!(["python3", "-c", "secret"]),
                json!({"status": "completed", "exit_code": 1, "stdout": "", "stderr": "bwrap: execvp python3: No such file or directory\n"})),
            run(json!(["/usr/bin/sh", "-c", "sleep 99"]),
                json!({"status": "timed_out", "exit_code": null, "stdout": "", "stderr": ""})),
            run(json!(["grep", "x", "y"]),
                json!({"status": "completed", "exit_code": 2, "stdout": "", "stderr": "grep: y: No such file"})),
            run(json!(["ls"]),
                json!({"status": "completed", "exit_code": 0, "stdout": "a\n", "stderr": ""})),
        ]});
        let facts = observations(&evidence, 0, 9, Outcome::Completed);
        let ends: Vec<_> = facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::ToolCall {
                    program,
                    command,
                    ok,
                    ..
                } => Some((program.clone(), *command, *ok)),
                _ => None,
            })
            .collect();
        use anchor_facts::{CommandEnd, CommandStatus};
        let end = |status, exit_code| Some(CommandEnd { status, exit_code });
        assert_eq!(
            ends,
            vec![
                (
                    Some("python3".into()),
                    end(CommandStatus::NotStarted, Some(1)),
                    true
                ),
                (Some("sh".into()), end(CommandStatus::TimedOut, None), true),
                (
                    Some("grep".into()),
                    end(CommandStatus::Exited, Some(2)),
                    true
                ),
                (Some("ls".into()), end(CommandStatus::Exited, Some(0)), true),
            ]
        );
        assert!(ends[3].1.unwrap().clean() && !ends[2].1.unwrap().clean());
        let recorded = format!("{facts:?}");
        for value in ["secret", "sleep 99", "No such file", "a\\n"] {
            assert!(!recorded.contains(value), "{value}");
        }
    }
}
