use std::{env, path::PathBuf, process::ExitCode};

use anchor_devtools::{FixtureOptions, run_fixture, run_goose_fixture};

const HELP: &str = "anchor-devtools regression fixture|goose-fixture [--workspace-root PATH] [--target-dir PATH] [--evidence-root PATH]\n\nfixture runs the full deterministic Host/runtime_contract suite and two explicitly selected native_plugins fixtures with legacy-regression.\nGoose tests are excluded from fixture.\ngoose-fixture runs standard Host goose_acp/goose_pilot/goose_elicitation/goose_media/goose_conversation/goose_channel/goose_compaction/goose_pilot_compaction/goose_trace/goose_session_calls/goose_library/native_plugins ignored fixtures without legacy features.\nRequires explicit ANCHOR_GOOSE_BINARY: pinned Goose 1.53.0 x86_64 musl binary, validated by SHA256.\nRequires stable Rust, Bubblewrap and the suite's normal Linux tools, not Python.\nDoes not call a real model or load dotenv. Live/business/production acceptance is separate.";

enum Regression {
    Legacy,
    Goose,
}

fn options() -> Result<Option<(Regression, FixtureOptions)>, String> {
    let mut arguments = env::args().skip(1);
    let Some(command) = arguments.next() else {
        return Ok(None);
    };
    if command == "--help" || command == "-h" {
        return Ok(None);
    }
    if command != "regression" {
        return Err("expected regression fixture or goose-fixture; use --help".into());
    }
    let regression = match arguments.next().as_deref() {
        Some("fixture") => Regression::Legacy,
        Some("goose-fixture") => Regression::Goose,
        _ => return Err("expected regression fixture or goose-fixture; use --help".into()),
    };
    let mut options = FixtureOptions {
        workspace_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        target_dir: env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| env::temp_dir().join("anchor-native-regression-target")),
        evidence_root: env::temp_dir(),
    };
    while let Some(argument) = arguments.next() {
        if argument == "--help" || argument == "-h" {
            return Ok(None);
        }
        let destination = match argument.as_str() {
            "--workspace-root" => &mut options.workspace_root,
            "--target-dir" => &mut options.target_dir,
            "--evidence-root" => &mut options.evidence_root,
            _ => return Err(format!("unknown argument: {argument}")),
        };
        let value = arguments
            .next()
            .filter(|value| !value.starts_with("--"))
            .ok_or_else(|| format!("missing path for {argument}"))?;
        *destination = PathBuf::from(value);
    }
    Ok(Some((regression, options)))
}

fn main() -> ExitCode {
    let (regression, options) = match options() {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("anchor-devtools: {error}");
            return ExitCode::from(2);
        }
    };
    let result = match regression {
        Regression::Legacy => run_fixture(&options),
        Regression::Goose => run_goose_fixture(&options),
    };
    match result {
        Ok(result) => {
            println!("{}", serde_json::to_string(&result).unwrap());
            if result.status == "passed" {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("anchor-devtools: {error}");
            ExitCode::FAILURE
        }
    }
}
