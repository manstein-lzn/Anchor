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
anchor-rsi weekly-discover\n\
anchor-rsi weekly-collect [--codex DIR] [--claude DIR] [--deepseek DIR] [--end RFC3339] [--output DIR]\n\
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
    let path = absolute_path(flags, name, default)?;
    safe_path(&path)?;
    Ok(path)
}

fn absolute_path(
    flags: &BTreeMap<String, String>,
    name: &str,
    default: &str,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(flags.get(name).map(String::as_str).unwrap_or(default));
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
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
    let collected = business::object(&root.join("index.json"))?;
    safe_path(output)?;
    // These generated subtrees belong to research; the rest of the stable
    // workspace remains in place. Old hashes must not enter the next delivery.
    for name in ["files", "dependencies"] {
        let directory = business::relative(output, name)?;
        if directory.exists() {
            fs::remove_dir_all(&directory).map_err(|error| error.to_string())?;
        }
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    }
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
    business::write_json(
        &output.join("index.json"),
        &json!({"format":1,"captured_at":collected["captured_at"],"window_start":collected["window_start"],"window_end":collected["window_end"],"entries":entries}),
    )?;
    business::write_json(
        &output.join("ecosystem.json"),
        &json!({"pages":results,"dependency_count":declarations.len()}),
    )?;
    fs::write(business::relative(output, "sources.md")?, sources).map_err(|error| error.to_string())
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
        "weekly-discover" => {
            flags(&arguments[1..], &[])?;
            println!("{}", crate::history_discovery::discover()?);
        }
        "weekly-collect" => {
            let flags = flags(
                &arguments[1..],
                &["--codex", "--claude", "--deepseek", "--output", "--end"],
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
                        absolute_path(&flags, "--codex", "/local-inputs/codex")?,
                    ),
                    (
                        "claude".into(),
                        absolute_path(&flags, "--claude", "/local-inputs/claude")?,
                    ),
                    (
                        "deepseek".into(),
                        absolute_path(&flags, "--deepseek", "/local-inputs/deepseek")?,
                    ),
                ],
                &path(&flags, "--output", "evidence")?,
                end,
            )?;
            println!(
                "{}",
                json!({"status":value["status"],"start":value["start"],"end_exclusive":value["end_exclusive"],"sources":value["sources"],"warnings":value["warnings"]})
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn research_replaces_generated_files_when_reentering_a_stable_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let evidence = temp.path().join("evidence");
        let output = temp.path().join("research");
        business::write_json(&evidence.join("dependencies/declarations.json"), &json!([])).unwrap();
        business::write_json(
            &evidence.join("index.json"),
            &json!({"captured_at":"2026-10-31T00:00:00Z","window_start":"2026-10-24T00:00:00Z","window_end":"2026-10-31T00:00:00Z"}),
        )
        .unwrap();
        research(&evidence, &output).await.unwrap();
        fs::write(output.join("files/obsolete.txt"), "old research").unwrap();
        fs::write(output.join("dependencies/ecosystem-obsolete.json"), "{}").unwrap();
        fs::write(output.join("notes.txt"), "retained workspace note").unwrap();

        research(&evidence, &output).await.unwrap();

        assert!(fs::read_dir(output.join("files")).unwrap().next().is_none());
        assert!(
            fs::read_dir(output.join("dependencies"))
                .unwrap()
                .next()
                .is_none()
        );
        assert_eq!(
            business::object(&output.join("index.json")).unwrap()["entries"],
            json!({})
        );
        assert_eq!(
            business::object(&output.join("ecosystem.json")).unwrap()["pages"],
            json!([])
        );
        assert_eq!(
            fs::read_to_string(output.join("notes.txt")).unwrap(),
            "retained workspace note"
        );
    }
}
