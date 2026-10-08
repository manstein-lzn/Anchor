use crate::{
    business::{
        self,
        rsi::{self, FrozenEvidence},
        weekly,
    },
    ecosystem::{Dependency, Ecosystem},
    evidence::{Config, DOMAINS, Evidence, safe_path},
    redact,
};
use chrono::Duration;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const HELP: &str = "anchor-rsi [serve]\n\
anchor-rsi collect --source DIR --anchor DIR --output DIR [--rust-state DIR] [--previous DIR]\n\
anchor-rsi research --evidence DIR --output DIR\n\
anchor-rsi rsi-review [--inputs /in] [--output /workspace]\n\
anchor-rsi rsi-gate [--inputs /in] [--output /workspace]\n\
anchor-rsi rsi-publish [--inputs /in] [--output /workspace]\n\
anchor-rsi native-gate --evidence DIR [--inputs /in] [--output /workspace]\n\
anchor-rsi native-publish --evidence DIR [--inputs /in] [--output /workspace]\n\
anchor-rsi weekly-collect [--codex DIR] [--deepseek DIR] [--end RFC3339] [--output DIR]\n\
anchor-rsi weekly-gate [--inputs /in] [--output /workspace]\n\
anchor-rsi weekly-publish [--inputs /in] [--output /workspace]\n\
Gate commands write feedback and call the existing anchor-route helper.\n\
Publish commands recheck evidence/review bindings before copying report assets.\n";

fn flags(arguments: &[String], allowed: &[&str]) -> Result<BTreeMap<String, String>, String> {
    if !arguments.len().is_multiple_of(2) {
        return Err("each option requires a value".into());
    }
    let mut flags = BTreeMap::new();
    for pair in arguments.chunks_exact(2) {
        if !allowed.contains(&pair[0].as_str())
            || flags.insert(pair[0].clone(), pair[1].clone()).is_some()
        {
            return Err(format!("unknown or duplicate option {}", pair[0]));
        }
    }
    Ok(flags)
}

fn path(flags: &BTreeMap<String, String>, name: &str, default: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(flags.get(name).map(String::as_str).unwrap_or(default));
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    safe_path(&path)?;
    Ok(path)
}

fn export(evidence: &Evidence, root: &Path) -> Result<(), String> {
    for entry in evidence.entries.values() {
        let source = business::relative(root, &entry.frozen_file)?;
        let destination = business::relative(root, &entry.path)?;
        business::copy(&source, &destination)?;
    }
    let window = json!({"start":evidence.captured_at-Duration::days(7),"end_exclusive":evidence.captured_at});
    for domain in DOMAINS {
        let files = evidence
            .entries
            .values()
            .filter(|entry| entry.domain == domain)
            .collect::<Vec<_>>();
        let value = json!({"domain":domain,"files":files,"captured_at":evidence.captured_at,"window":window,"issues":evidence.issues,"limitations":crate::evidence::LIMITATIONS});
        business::write_json(&root.join("domains").join(format!("{domain}.json")), &value)?;
        let filename = match domain {
            "code" => "source-index",
            "runs" => "run-index",
            "previous" => "previous-index",
            value => value,
        };
        business::write_json(&root.join(format!("{filename}.json")), &value)?;
    }
    let mut previous = Vec::new();
    let mut runs = Vec::new();
    for entry in evidence.entries.values() {
        if entry.path == "runs/schedules.json" {
            business::copy(&root.join(&entry.frozen_file), &root.join("schedules.json"))?;
            continue;
        }
        if ["previous", "runs"].contains(&entry.domain.as_str()) && entry.path.ends_with(".json") {
            let value: Value = serde_json::from_slice(&business::read(&business::relative(
                root,
                &entry.frozen_file,
            )?)?)
            .map_err(|error| error.to_string())?;
            if entry.domain == "previous" {
                previous.push(value);
            } else {
                runs.push(value);
            }
        }
    }
    business::write_json(
        &root.join("previous.json"),
        &json!({"reports":previous,"limitations":"Only committed published reports or explicitly granted report roots are included; missing history remains unknown."}),
    )?;
    business::write_json(
        &root.join("runs.json"),
        &json!({"runs":runs,"window":window}),
    )?;
    business::write_json(&root.join("collection.json"), &window)?;
    Ok(())
}

async fn research(root: &Path, output: &Path) -> Result<(), String> {
    let declarations: Vec<Dependency> = serde_json::from_slice(&business::read(
        &root.join("dependencies/declarations.json"),
    )?)
    .map_err(|error| error.to_string())?;
    safe_path(output)?;
    if output.exists()
        && fs::read_dir(output)
            .map_err(|error| error.to_string())?
            .next()
            .is_some()
    {
        return Err("research output must be empty".into());
    }
    fs::create_dir_all(output.join("files")).map_err(|error| error.to_string())?;
    let ecosystem = Ecosystem::new()?;
    let mut entries = serde_json::Map::new();
    let mut results = Vec::new();
    let mut sources = String::from(
        "# Public ecosystem sources\n\nFailures are recorded; metadata does not prove compatibility or installed versions.\n",
    );
    for page in declarations.chunks(20) {
        let mut value = ecosystem.research(page).await;
        redact::json(&mut value);
        let bytes = serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?;
        let hash = business::digest(&bytes);
        let path = format!("dependencies/ecosystem-{hash}.json");
        let frozen_file = format!("files/{hash}.txt");
        fs::write(output.join(&frozen_file), &bytes).map_err(|error| error.to_string())?;
        business::write_json(&business::relative(output, &path)?, &value)?;
        let entry = json!({"path":path,"domain":"dependencies","source":"credential-free public registries","frozen_file":frozen_file,"sha256":hash,"original_sha256":hash,"lines":String::from_utf8_lossy(&bytes).lines().count(),"redacted":false});
        entries.insert(path.clone(), entry);
        sources.push_str(&format!(
            "\n- {path}: captured {}; success/failure and endpoint are in the evidence.\n",
            value["retrieved_at"]
        ));
        results.push(value);
    }
    let collected = business::object(&root.join("index.json"))?;
    business::write_json(
        &output.join("index.json"),
        &json!({"format":1,"captured_at":collected["captured_at"],"window_start":collected["window_start"],"window_end":collected["window_end"],"entries":entries}),
    )?;
    business::write_json(
        &output.join("ecosystem.json"),
        &json!({"pages":results,"dependency_count":declarations.len()}),
    )?;
    fs::write(output.join("sources.md"), sources).map_err(|error| error.to_string())
}

pub async fn run(arguments: &[String]) -> Result<(), String> {
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(HELP.into());
    };
    if ["help", "--help", "-h"].contains(&command)
        || arguments.get(1).is_some_and(|flag| flag == "--help")
    {
        print!("{HELP}");
        return Ok(());
    }
    match command {
        "collect" => {
            let flags = flags(
                &arguments[1..],
                &[
                    "--source",
                    "--anchor",
                    "--output",
                    "--rust-state",
                    "--previous",
                ],
            )?;
            let config = Config {
                source: path(&flags, "--source", "/local-inputs/source")?,
                data: path(&flags, "--anchor", "/local-inputs/anchor")?,
                evidence: path(&flags, "--output", "evidence")?,
                rust_state: flags
                    .get("--rust-state")
                    .map(|_| path(&flags, "--rust-state", ""))
                    .transpose()?,
                previous: flags
                    .get("--previous")
                    .map(|_| path(&flags, "--previous", ""))
                    .transpose()?,
            };
            let evidence = Evidence::collect(config.clone())?;
            export(&evidence, &config.evidence)?;
        }
        "research" => {
            let flags = flags(&arguments[1..], &["--evidence", "--output"])?;
            research(
                &path(&flags, "--evidence", "/in/collect/evidence")?,
                &path(&flags, "--output", "research")?,
            )
            .await?;
        }
        "weekly-collect" => {
            let flags = flags(
                &arguments[1..],
                &["--codex", "--deepseek", "--output", "--end"],
            )?;
            let input: Value = serde_json::from_str(
                &std::env::var("ANCHOR_INPUT").unwrap_or_else(|_| "{}".into()),
            )
            .map_err(|error| error.to_string())?;
            let end = flags
                .get("--end")
                .map(String::as_str)
                .or_else(|| input.get("end").and_then(Value::as_str));
            let value = weekly::collect(
                &[
                    (
                        "codex".into(),
                        path(&flags, "--codex", "/local-inputs/codex")?,
                    ),
                    (
                        "deepseek".into(),
                        path(&flags, "--deepseek", "/local-inputs/deepseek")?,
                    ),
                ],
                &path(&flags, "--output", "evidence")?,
                end,
            )?;
            println!(
                "{}",
                json!({"start":value["start"],"end_exclusive":value["end_exclusive"],"sources":value["sources"]})
            );
        }
        "weekly-gate" | "weekly-publish" | "rsi-review" | "rsi-gate" | "rsi-publish"
        | "native-gate" | "native-publish" => {
            let allowed = if command.starts_with("native-") {
                vec!["--inputs", "--output", "--evidence"]
            } else {
                vec!["--inputs", "--output"]
            };
            let flags = flags(&arguments[1..], &allowed)?;
            let inputs = path(&flags, "--inputs", "/in")?;
            let output = path(&flags, "--output", "/workspace")?;
            fs::create_dir_all(&output).map_err(|error| error.to_string())?;
            let frozen = if command.starts_with("native-") {
                Some(FrozenEvidence::load(&path(
                    &flags,
                    "--evidence",
                    "/local-inputs/evidence",
                )?)?)
            } else {
                None
            };
            match command {
                "weekly-gate" => {
                    let (target, reason) = weekly::gate(&inputs, &output)?;
                    business::route(&target, &reason)?;
                }
                "weekly-publish" => weekly::assemble(&inputs, &output)?,
                "rsi-review" => rsi::save_review(&inputs, &output)?,
                "rsi-gate" | "native-gate" => {
                    let (target, reason) = rsi::gate(&inputs, &output, frozen.as_ref())?;
                    business::route(&target, &reason)?;
                }
                "rsi-publish" | "native-publish" => {
                    rsi::assemble(&inputs, &output, frozen.as_ref())?
                }
                _ => unreachable!(),
            }
        }
        _ => return Err(format!("unknown command {command}\n{HELP}")),
    }
    Ok(())
}
