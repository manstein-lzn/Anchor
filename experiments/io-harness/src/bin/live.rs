//! Opt-in text provider smoke for an OpenAI-compatible endpoint.

use io_harness::provider::{Auth, Compatible};
use io_harness::{ApproveAll, Policy, RunOutcome, Store, TaskContract, Verification, run_with};

fn required(name: &str) -> io_harness::Result<String> {
    std::env::var(name).map_err(|_| io_harness::Error::Config(format!("missing {name}")))
}

fn endpoint_host(base: &str) -> &str {
    base.split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base)
        .split('/')
        .next()
        .unwrap_or(base)
}

#[tokio::main]
async fn main() -> io_harness::Result<()> {
    let base = required("ANCHOR_IO_HARNESS_URL")?;
    let key = required("ANCHOR_IO_HARNESS_API_KEY")?;
    let model = required("ANCHOR_IO_HARNESS_MODEL")?;
    let provider = Compatible::new(base.clone(), Auth::Bearer, key, model);
    let workspace = tempfile::tempdir()?;
    let contract = TaskContract::workspace(
        "Reply with exactly one short sentence confirming that the Rust runtime smoke test reached the model. Do not call any tool and do not inspect files.",
        workspace.path(),
    )
    .with_verification(Verification::None)
    .with_max_steps(4);
    let policy = Policy::default()
        .layer("disposable-live-smoke")
        .allow_read("*")
        .allow_write("*")
        .allow_net(endpoint_host(&base));
    let store = Store::memory()?;
    let result = run_with(&contract, &provider, &store, &policy, &ApproveAll).await?;
    println!("outcome={:?}", result.outcome);
    println!("steps={}", store.steps(result.run_id)?.len());
    if !matches!(
        result.outcome,
        RunOutcome::Finished { .. } | RunOutcome::Success { .. }
    ) {
        return Err(io_harness::Error::Config(format!(
            "live smoke did not finish: {:?}",
            result.outcome
        )));
    }
    Ok(())
}
