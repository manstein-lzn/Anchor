//! `record` submits an existing grade; `grade-run` independently checks an
//! ordinary immutable output against a registered quality suite.
//!
//! Both use `DIR/kernel-grades`. Exits 1 for a refused `record`, 2 on a usage
//! or process error.

use std::io::Read;
use std::path::PathBuf;

const USAGE: &str = "usage: anchor-eval record --state DIR --config DIGEST < grade.json\n       anchor-eval grade-run --state DIR --artifacts DIR --runs DIR --suite FILE --evaluator-package-digest DIGEST --run ID --node ID";

fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}

fn run() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("grade-run") {
        return grade_run(&args[1..]).map(|()| true);
    }
    let (mut state, mut config) = (None, None);
    let mut rest = args.iter();
    if rest.next().map(String::as_str) != Some("record") {
        return Err(USAGE.into());
    }
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or(USAGE)?;
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value)),
            "--config" => config = Some(value.clone()),
            _ => return Err(USAGE.into()),
        }
    }
    let (Some(state), Some(config)) = (state, config) else {
        return Err(USAGE.into());
    };
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| e.to_string())?;
    let grade: anchor_facts::Grade =
        serde_json::from_str(&input).map_err(|e| format!("grade: {e}"))?;
    let recorder = anchor_eval::GradeRecorder::open(&state)?;
    let verdict = recorder
        .record(&grade, &config)
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string(&verdict).map_err(|e| e.to_string())?
    );
    Ok(verdict.is_accepted())
}

fn grade_run(args: &[String]) -> Result<(), String> {
    let mut flags = std::collections::BTreeMap::new();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or(USAGE)?;
        if ![
            "--state",
            "--artifacts",
            "--runs",
            "--suite",
            "--evaluator-package-digest",
            "--run",
            "--node",
        ]
        .contains(&flag.as_str())
            || flags.insert(flag.as_str(), value.as_str()).is_some()
        {
            return Err(USAGE.into());
        }
    }
    let get = |name| flags.get(name).copied().ok_or_else(|| USAGE.to_string());
    let evaluator = anchor_eval::evaluation::Evaluator::open(
        std::path::Path::new(get("--state")?),
        std::path::Path::new(get("--artifacts")?),
        std::path::Path::new(get("--runs")?),
        std::path::Path::new(get("--suite")?),
        get("--evaluator-package-digest")?,
    )?;
    println!(
        "{}",
        serde_json::to_string(&evaluator.grade_ordinary(get("--run")?, get("--node")?)?)
            .map_err(|error| error.to_string())?
    );
    Ok(())
}
