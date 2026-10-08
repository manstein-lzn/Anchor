use std::{env, path::PathBuf, process::ExitCode};

use anchor_devtools::{FixtureOptions, candidate, cutover, preflight, run_goose_fixture};
use serde_json::Value;

const HELP: &str = "anchor-devtools regression fixture|goose-fixture [--workspace-root PATH] [--target-dir PATH] [--evidence-root PATH]
anchor-devtools regression candidate [--workspace-root PATH] [--target-dir PATH] [--evidence-root PATH] [--goose PATH]
anchor-devtools preflight
anchor-devtools cutover [OPTIONS]

fixture and goose-fixture are aliases for the standard Goose small-Graph regression.
Runs standard Host goose_acp/goose_pilot/goose_elicitation/goose_media/goose_conversation/goose_channel/goose_compaction/goose_pilot_compaction/goose_trace/goose_session_calls/goose_library/native_plugins ignored fixtures with a deterministic local Provider.
Fixture requires explicit ANCHOR_GOOSE_BINARY: pinned Goose 1.53.0 x86_64 musl binary, validated by SHA256.
Candidate builds the WebUI and locked offline release binaries once, then runs the source-free goose_distribution recovery fixture.
Candidate Goose path: --goose, ANCHOR_GOOSE_BINARY or ANCHOR_DISTRIBUTION_GOOSE; evidence-root must be a new directory.
Requires stable Rust, Bubblewrap and the suite's normal Linux tools; candidate also requires installed Web dependencies.
Does not call a real model or load dotenv. Live/business/production acceptance is separate.
Preflight accepts no flags and prints a JSON report. Cutover forwards options to the inventory module; a blocked decision exits 2.
Cutover options: --legacy-root PATH --rust-state-root PATH --rust-workspace-root PATH --rust-catalog-root PATH
  [--rust-library-root PATH] [--config PATH] [--credential-path PATH]
  [--legacy-writer-stopped] [--legacy-read-only-confirmed] [--credentials-reviewed] [--prepare --output-dir PATH]";

enum Invocation {
    Help,
    Fixture(FixtureOptions),
    Candidate(candidate::CandidateOptions),
    Preflight,
    Cutover(Vec<String>),
}

fn options(arguments: Vec<String>) -> Result<Invocation, String> {
    let Some(command) = arguments.first().map(String::as_str) else {
        return Ok(Invocation::Help);
    };
    let remaining = &arguments[1..];
    let help = |arguments: &[String]| {
        arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h")
    };
    if help(&arguments) {
        return Ok(Invocation::Help);
    }
    match command {
        "preflight" => {
            return if remaining.is_empty() {
                Ok(Invocation::Preflight)
            } else if help(remaining) {
                Ok(Invocation::Help)
            } else {
                Err("preflight accepts no flags; use preflight --help".into())
            };
        }
        "cutover" => {
            return if help(remaining) {
                Ok(Invocation::Help)
            } else {
                Ok(Invocation::Cutover(remaining.to_vec()))
            };
        }
        "regression" if help(remaining) => return Ok(Invocation::Help),
        "regression" => {}
        _ => return Err("expected regression, preflight or cutover; use --help".into()),
    }
    let entry = remaining.first().map(String::as_str);
    if !matches!(entry, Some("fixture" | "goose-fixture" | "candidate")) {
        return Err("expected regression fixture, goose-fixture or candidate; use --help".into());
    }
    let remaining = &remaining[1..];
    if help(remaining) {
        return Ok(Invocation::Help);
    }
    let is_candidate = entry == Some("candidate");
    let mut workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::temp_dir().join(if is_candidate {
                "anchor-production-candidate-target"
            } else {
                "anchor-native-regression-target"
            })
        });
    let mut evidence_root = None;
    let mut goose = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut arguments = remaining.iter();
    while let Some(argument) = arguments.next() {
        if !(matches!(
            argument.as_str(),
            "--workspace-root" | "--target-dir" | "--evidence-root"
        ) || is_candidate && argument == "--goose")
        {
            return Err(format!("unknown argument: {argument}"));
        }
        if !seen.insert(argument) {
            return Err(format!("duplicate argument: {argument}"));
        }
        let value = arguments
            .next()
            .filter(|value| !value.is_empty() && !value.starts_with('-'))
            .ok_or_else(|| format!("missing path for {argument}"))?;
        match argument.as_str() {
            "--workspace-root" => workspace_root = PathBuf::from(value),
            "--target-dir" => target_dir = PathBuf::from(value),
            "--evidence-root" => evidence_root = Some(PathBuf::from(value)),
            "--goose" => goose = Some(PathBuf::from(value)),
            _ => unreachable!(),
        }
    }
    Ok(if is_candidate {
        Invocation::Candidate(candidate::CandidateOptions {
            workspace_root,
            target_dir,
            evidence_root,
            goose,
        })
    } else {
        Invocation::Fixture(FixtureOptions {
            workspace_root,
            target_dir,
            evidence_root: evidence_root.unwrap_or_else(env::temp_dir),
        })
    })
}

fn print_report(result: Result<Value, String>, failure_code: u8) -> ExitCode {
    match result {
        Ok(report) => {
            println!("{report}");
            if report["decision"] == "blocked" {
                ExitCode::from(2)
            } else if report["status"] == "failed" {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("anchor-devtools: {error}");
            ExitCode::from(failure_code)
        }
    }
}

fn main() -> ExitCode {
    let invocation = match options(env::args().skip(1).collect()) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("anchor-devtools: {error}");
            return ExitCode::from(2);
        }
    };
    match invocation {
        Invocation::Help => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Invocation::Fixture(options) => print_report(
            run_goose_fixture(&options)
                .map_err(|error| error.to_string())
                .and_then(|report| serde_json::to_value(report).map_err(|error| error.to_string())),
            1,
        ),
        Invocation::Candidate(options) => print_report(candidate::run(&options), 1),
        Invocation::Preflight => print_report(preflight::run(), 1),
        Invocation::Cutover(arguments) => print_report(cutover::run(&arguments), 2),
    }
}
