//! Startup recovery of persistent channel assistants.
//!
//! A restart must give every channel Session back the assistant instance its
//! binding still owns. Recovery never revives the old Run: it hands the
//! instance over to a new Run of the same Graph, which continues the same Goose
//! conversation and inherits the old Run's workspace scene, exactly like an
//! operator-requested handover — only without the operator.

use super::*;
use crate::application::{AssistantAdmission, metadata};
use crate::assistant::AssistantPlan;
use anchor_platform_session::ChannelAssistant;
use anchor_runtime::graph::{FileRunStore, GraphRunRecord, InvocationKey, RunStore};
use sha2::{Digest, Sha256};
use std::time::Duration;

/// Read by the Host process only, at startup. `0`, `false`, `off` and `no`
/// disable automatic recovery; anything else (and an unset variable) keeps it on.
pub(super) const AUTO_RESUME_ENV: &str = "ANCHOR_ASSISTANT_AUTO_RESUME";

const MAX_RESUME_ATTEMPTS: usize = 3;
const RESUME_BACKOFF: Duration = Duration::from_millis(200);

fn auto_resume_enabled() -> bool {
    match env::var(AUTO_RESUME_ENV) {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

/// One instance's recovery, resolved from durable facts before anything moves.
struct Recovery {
    /// The Run that takes the instance over.
    target: String,
    /// The Run it is handed over from; also its conversation predecessor.
    previous: String,
    graph: String,
    path: PathBuf,
    bundle: anchor_graph_host::LoadedGraphBundle,
    plan: AssistantPlan,
}

/// Resume every live assistant instance of this Host.
///
/// The binding table is the authority: an instance that was retired by hand is
/// history and stays down, and an instance whose Run has no execution (crashed,
/// stopped, or never admitted) is recovered. Each Session is independent and
/// bounded: a failure is reported and never blocks the rest of the Host.
pub(super) async fn resume_channel_assistants(state: &ApiState) -> Result<(), String> {
    if !auto_resume_enabled() {
        eprintln!("anchor-runner-host: assistant auto resume is disabled by {AUTO_RESUME_ENV}");
        return Ok(());
    }
    let instances = store(state)
        .map_err(|_| "assistant auto resume could not open the Session store".to_owned())?
        .list_channel_assistants()
        .map_err(|error| error.to_string())?;
    for (owner, assistant) in instances {
        for attempt in 1..=MAX_RESUME_ATTEMPTS {
            // Every attempt re-reads the binding: a previous attempt may already
            // have handed the instance over, and retrying the *old* source would
            // try to make a second Run current.
            let current = match store(state)
                .map_err(|_| "the Session store is unavailable".to_owned())
                .and_then(|sessions| {
                    sessions
                        .get_channel_assistant(&owner, &assistant.session_id)
                        .map_err(|error| error.to_string())
                }) {
                Ok(current) => current,
                Err(error) => {
                    eprintln!(
                        "anchor-runner-host: assistant {}/{} could not be read: {error}",
                        owner, assistant.session_id
                    );
                    break;
                }
            };
            let Some(current) = current else {
                // Retired (or deleted) while recovering; it stays down.
                break;
            };
            match resume_instance(state, &owner, &current).await {
                Ok(()) => break,
                Err(error) if attempt < MAX_RESUME_ATTEMPTS => {
                    eprintln!(
                        "anchor-runner-host: assistant {}/{} was not resumed (attempt {attempt}): {error}",
                        owner, assistant.session_id
                    );
                    tokio::time::sleep(RESUME_BACKOFF * attempt as u32).await;
                }
                Err(error) => eprintln!(
                    "anchor-runner-host: assistant {}/{} stays unrecovered: {error}",
                    owner, assistant.session_id
                ),
            }
        }
    }
    Ok(())
}

async fn resume_instance(
    state: &ApiState,
    owner: &str,
    assistant: &ChannelAssistant,
) -> Result<(), String> {
    let session = assistant.session_id.clone();
    let from = assistant.run_id.clone();
    // Liveness, not the persisted status, decides: a crash can leave the record
    // Running while nothing executes it.
    if state.application.run_is_active(&from).await {
        return Ok(());
    }
    let recovery = match state
        .application
        .metadata(&from)
        .map_err(|error| format!("{error:?}"))?
    {
        Some(saved) => {
            let source = saved
                .assistant
                .clone()
                .ok_or("the current binding does not name an assistant instance")?;
            if source.owner != owner || source.session != session {
                return Err("the current binding belongs to another owner or Session".into());
            }
            // Metadata without a record is an admission that was interrupted
            // after its immutable Run identity was written. Completing it would
            // mean admitting a Run id the Host already refused as incomplete,
            // and replacing it would change the recovery target on every
            // attempt, so nothing is moved and the retained facts are reported.
            if run_record(state, &from)?.is_none() {
                return Err(
                    "the current assistant Run has immutable metadata but no admitted record"
                        .into(),
                );
            }
            let (path, bundle) = load_graph_definition(state, &saved.graph).map_err(|failure| {
                format!("assistant Graph is unavailable: {:?}", failure.status())
            })?;
            let plan = plan_for(&bundle, &source.work_node)?;
            let digest = bundle
                .snapshot
                .digest()
                .map_err(|error| error.to_string())?;
            // Unique per recovery, and unique across Sessions even if two
            // recoveries share a clock tick: a colliding Run id would fail
            // closed and keep the instance down.
            let session_tag = format!("{:x}", Sha256::digest(session.as_bytes()));
            let target = format!(
                "assistant-resume-{:x}-{}",
                metadata::now_nanos(),
                &session_tag[..8]
            );
            // The handover is the durable decision: the old binding is retired,
            // the new Run becomes current, and a claimed but uncommitted Turn
            // travels with the instance under the new wait key.
            let wait_key = InvocationKey {
                run_id: target.clone(),
                graph_digest: digest,
                node_id: plan.wait_node.clone(),
                invocation: 1,
            }
            .durable_key();
            store(state)
                .map_err(|_| "assistant auto resume could not open the Session store".to_owned())?
                .handover_channel_assistant(owner, &session, &from, &target, &wait_key)
                .map_err(|error| error.to_string())?;
            Recovery {
                target,
                previous: from.clone(),
                graph: saved.graph,
                path,
                bundle,
                plan,
            }
        }
        None => {
            // A crash between the handover and the admission leaves the new Run
            // current with no Run facts. The retry must admit that same Run id,
            // using the retired binding it was handed over from.
            let predecessor = store(state)
                .map_err(|_| "assistant auto resume could not open the Session store".to_owned())?
                .channel_assistant_predecessor(owner, &session, &from)
                .map_err(|error| error.to_string())?
                .ok_or("this binding has no Run facts and no handed-over predecessor")?;
            let prior = state
                .application
                .metadata(&predecessor.run_id)
                .map_err(|error| format!("{error:?}"))?
                .ok_or("the handed-over Run has no immutable identity")?;
            let source = prior
                .assistant
                .clone()
                .ok_or("the handed-over Run is not an assistant instance")?;
            if source.owner != owner || source.session != session {
                return Err("the handed-over Run belongs to another owner or Session".into());
            }
            let (path, bundle) = load_graph_definition(state, &prior.graph).map_err(|failure| {
                format!("assistant Graph is unavailable: {:?}", failure.status())
            })?;
            let plan = plan_for(&bundle, &source.work_node)?;
            Recovery {
                target: from.clone(),
                previous: predecessor.run_id,
                graph: prior.graph,
                path,
                bundle,
                plan,
            }
        }
    };

    // A crashed Run can still be persisted as running/ready; nothing executes
    // it, so settle it before admission considers it a predecessor. The
    // handover already moved the instance, so stopping the old Run cannot
    // settle the Turn it handed over.
    if !super::wecom::stop_previous_run(state, &recovery.previous)
        .await
        .map_err(|failure| {
            format!(
                "handed-over Run state is unavailable: {:?}",
                failure.status()
            )
        })?
    {
        return Err("the handed-over Run has no durable record".into());
    }
    let lease = state
        .application
        .acquire_graph_lease_waiting(&recovery.path)
        .await
        .map_err(|error| format!("{error:?}"))?;
    let target = state
        .application
        .admit_assistant(
            AssistantAdmission {
                graph: recovery.graph,
                session: session.clone(),
                owner: owner.to_owned(),
                oauth_owner: super::oauth::binding_owner(owner),
                run: recovery.target.clone(),
                previous_run: Some(recovery.previous.clone()),
            },
            &recovery.path,
            recovery.bundle,
            lease,
            recovery.plan,
        )
        .await
        .map_err(|error| format!("{error:?}"))?;
    // Observability last: this event only ever describes a completed recovery,
    // and the automatic path is distinct from the manual handover.
    store(state)
        .map_err(|_| "assistant auto resume could not open the Session store".to_owned())?
        .record_channel_assistant_auto_resume(owner, &session, &recovery.previous, &target)
        .map_err(|error| error.to_string())?;
    eprintln!(
        "anchor-runner-host: assistant {owner}/{} resumed {} -> {target}",
        session, recovery.previous
    );
    Ok(())
}

fn run_record(state: &ApiState, run: &str) -> Result<Option<GraphRunRecord>, String> {
    FileRunStore::new(state.data_root.join("runs"))
        .load(run)
        .map_err(|error| error.to_string())
}

fn plan_for(
    bundle: &anchor_graph_host::LoadedGraphBundle,
    work_node: &str,
) -> Result<AssistantPlan, String> {
    AssistantPlan::from_snapshot(&bundle.snapshot, work_node)?
        .ok_or_else(|| "assistant Graph has no input loop".to_owned())
}
