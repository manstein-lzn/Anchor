#![forbid(unsafe_code)]

use std::{path::PathBuf, process::ExitCode};

use anchor_graph_host::{FilePluginCatalog, SystemPlugin};
use anchor_library::{InstallRequest, Library};
use clap::{Parser, Subcommand};
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "anchor-library",
    about = "Operator Plugin installation, review and dependency environments"
)]
struct Cli {
    #[arg(long)]
    root: PathBuf,
    #[command(subcommand)]
    command: Operation,
}

#[derive(Subcommand)]
enum Operation {
    /// Review a candidate Plugin, or install it when `--review` is absent.
    Install {
        #[arg(
            long,
            conflicts_with = "directory",
            required_unless_present = "directory"
        )]
        source: Option<String>,
        #[arg(long, conflicts_with = "source", requires = "id")]
        directory: Option<PathBuf>,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        replace_existing: bool,
        /// Print what the candidate declares and would publish, then stop
        /// without touching the Library.
        #[arg(long)]
        review: bool,
    },
    /// Bootstrap one missing System Plugin from an operator-admitted Anchor build.
    /// Canonical public resources and the exact reviewed digest must match; no replacement.
    InstallSystem {
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        digest: String,
    },
    /// Inspect installed old-release bytes without granting an execution identity.
    InspectSystem {
        #[arg(long)]
        id: String,
    },
    /// Explicitly upgrade reviewed old bytes to an admitted Anchor release.
    UpgradeSystem {
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        existing_digest: String,
    },
    /// Remove dependency views and store entries no living view references.
    ///
    /// Reports by default; `--apply` performs the deletion.
    Reclaim {
        #[arg(long)]
        apply: bool,
    },
}

/// Everything the CLI can print is JSON.
fn main() -> ExitCode {
    let cli = Cli::parse();
    let (value, status) = match run(cli) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    match serde_json::to_string(&value) {
        Ok(json) => {
            println!("{json}");
            status
        }
        Err(_) => {
            eprintln!("cannot encode the result as JSON");
            ExitCode::FAILURE
        }
    }
}

/// Refuse a Plugin whose run-time dependencies no host in this command can serve.
///
/// Dependencies belong to the Plugin, and the host solves them when a human
/// confirms the installation. The operator CLI is not that host: it has no
/// authorized sources, no approved set and nowhere to publish a view, so it
/// refuses the candidate instead of installing something that cannot run.
fn refuse_declared_dependencies(review: &anchor_library::PluginReview) -> Result<(), String> {
    if review.dependencies.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Plugin `{}` declares run-time dependencies ({}); only the host can solve them and land their view, so install this Plugin through the steward's confirmed installation",
        review.id,
        review.dependencies.python.join(", ")
    ))
}

fn run(cli: Cli) -> Result<(Value, ExitCode), String> {
    let root = cli.root;
    let library = Library::new(root.clone());
    match cli.command {
        Operation::InspectSystem { id } => {
            let system = system_id(&id)?;
            let _guard = library
                .catalog_read_guard()
                .map_err(|error| error.to_string())?;
            let inspection = FilePluginCatalog::new(&root)
                .inspect_system_plugin(system)
                .map_err(|error| error.to_string())?;
            Ok((
                serde_json::json!({"id": id, "digest": inspection.digest,
                "resources": inspection.resources, "mutable": inspection.mutable,
                "executionAuthorized": false}),
                ExitCode::SUCCESS,
            ))
        }
        Operation::UpgradeSystem {
            directory,
            id,
            digest,
            existing_digest,
        } => {
            let system = system_id(&id)?;
            library
                .upgrade_system_directory(system, directory, &digest, &existing_digest)
                .map(|outcome| {
                    (
                        serde_json::to_value(outcome).unwrap_or(Value::Null),
                        ExitCode::SUCCESS,
                    )
                })
                .map_err(|error| error.to_string())
        }
        Operation::InstallSystem {
            directory,
            id,
            digest,
        } => {
            let system = system_id(&id)?;
            library
                .install_system_directory(system, directory, &digest)
                .map(|outcome| {
                    (
                        serde_json::to_value(outcome).unwrap_or(Value::Null),
                        ExitCode::SUCCESS,
                    )
                })
                .map_err(|error| error.to_string())
        }
        Operation::Install {
            source,
            directory,
            id,
            replace_existing,
            review,
        } => {
            // Installing is the operator's second door, and this one has no host
            // that could solve a Plugin's declared dependencies or land their
            // view. Such a Plugin is refused here (after the same read-only
            // review every installation performs), because publishing it would
            // produce a Plugin whose tools can never start. `--review` still
            // describes it in full.
            if !review {
                let declared = match (&directory, id.as_deref()) {
                    (Some(directory), Some(id)) => library.review_directory(id, directory),
                    (Some(_), None) => {
                        return Err("--directory requires --id".to_owned());
                    }
                    (None, _) => library.review(&InstallRequest {
                        source: source.clone().unwrap_or_default(),
                        id: id.clone(),
                        replace_existing,
                    }),
                }
                .map_err(|error| error.to_string())?;
                refuse_declared_dependencies(&declared)?;
            }
            let report = match (directory, review) {
                (Some(directory), true) => library
                    .review_directory(id.as_deref().unwrap(), directory)
                    .map(|review| serde_json::to_value(review).unwrap_or(Value::Null)),
                (Some(directory), false) => library
                    .install_directory(id.as_deref().unwrap(), directory, replace_existing)
                    .map(|outcome| serde_json::to_value(outcome).unwrap_or(Value::Null)),
                (None, true) => library
                    .review(&InstallRequest {
                        source: source.unwrap_or_default(),
                        id,
                        replace_existing,
                    })
                    .map(|review| serde_json::to_value(review).unwrap_or(Value::Null)),
                (None, false) => library
                    .install(&InstallRequest {
                        source: source.unwrap_or_default(),
                        id,
                        replace_existing,
                    })
                    .map(|outcome| serde_json::to_value(outcome).unwrap_or(Value::Null)),
            };
            report
                .map(|value| (value, ExitCode::SUCCESS))
                .map_err(|error| error.to_string())
        }
        Operation::Reclaim { apply } => anchor_library::reclaim(&root, apply)
            .map(|report| {
                (
                    serde_json::to_value(report).unwrap_or(Value::Null),
                    ExitCode::SUCCESS,
                )
            })
            .map_err(|error| error.to_string()),
    }
}

fn system_id(id: &str) -> Result<SystemPlugin, String> {
    SystemPlugin::from_id(id)
        .ok_or_else(|| "this operation requires an Anchor-owned System Plugin ID".to_owned())
}
