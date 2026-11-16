//! Read-only fences for historical assistant replacements.
//! Remove after the installed release is retired and its handover facts are audited.
use super::{ApplicationError, RunApplication, storage};
use anchor_facts::evaluation::digest;
use anchor_kernel::{Action, PrincipalKind};
use anchor_platform_session::{ChannelAssistant, SessionStore};
use anchor_runtime::graph::InvocationKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AssistantHandoverFact {
    schema_version: u32,
    authority: Value,
    graph: String,
    owner: String,
    pub(super) source: ChannelAssistant,
    successor: String,
    pub(super) wait: InvocationKey,
}

impl AssistantHandoverFact {
    fn valid(&self) -> bool {
        self.schema_version == 1
            && !self.owner.is_empty()
            && self.wait.run_id == self.source.run_id
            && self.wait.node_id == self.source.wait_node
            && self.wait.invocation > 0
            && anchor_runtime::identifier::is_name(&self.successor)
            && self.successor
                == successor(&self.authority, &self.graph, &self.owner, &self.source)
                    .unwrap_or_default()
    }
}

fn successor(
    authority: &Value,
    graph: &str,
    owner: &str,
    source: &ChannelAssistant,
) -> Result<String, ApplicationError> {
    Ok(format!(
        "assistant-handover-{}",
        digest(&json!({"authority":authority,"graph":graph,"owner":owner,"source":source}))
            .map_err(storage)?
    ))
}

fn sessions(app: &RunApplication) -> Result<SessionStore, ApplicationError> {
    SessionStore::open(app.data_root.join("platform/sessions.sqlite"))
        .map_err(super::super::assistants::assistant_session_error)
}

impl RunApplication {
    pub(crate) fn assistant_has_pending_handover(
        &self,
        run: &str,
    ) -> Result<bool, ApplicationError> {
        let metadata = self.metadata(run)?;
        let mut observed = self.assistant_handover_fact(run)?;
        if observed.is_none()
            && let Some(previous) = metadata
                .as_ref()
                .and_then(|metadata| metadata.conversation.as_ref())
                .and_then(|binding| binding.previous_run.as_deref())
        {
            observed = self
                .assistant_handover_fact(previous)?
                .filter(|fact| fact.successor == run);
        }
        if observed.is_none()
            && metadata.is_none()
            && self.data_root.join("platform/sessions.sqlite").exists()
        {
            let store = sessions(self)?;
            for (owner, binding) in store
                .list_channel_assistants()
                .map_err(super::super::assistants::assistant_session_error)?
            {
                if binding.run_id == run
                    && let Some(previous) = store
                        .channel_assistant_predecessor(&owner, &binding.session_id, run)
                        .map_err(super::super::assistants::assistant_session_error)?
                {
                    observed = self
                        .assistant_handover_fact(&previous.run_id)?
                        .filter(|fact| fact.successor == run);
                    break;
                }
            }
        }
        let Some(fact) = observed else {
            return Ok(false);
        };
        let current = sessions(self)?
            .get_channel_assistant(&fact.owner, &fact.source.session_id)
            .map_err(super::super::assistants::assistant_session_error)?;
        let successor_incomplete =
            self.metadata(&fact.successor)?.is_none() || self.read_run(&fact.successor)?.is_none();
        Ok(current.is_some_and(|binding| {
            binding == fact.source || binding.run_id == fact.successor && successor_incomplete
        }))
    }

    /// An existing reply waiter follows its certified replacement, never
    /// re-admitting the inbound or choosing an unrelated current instance.
    pub(crate) fn assistant_handover_reply_run(
        &self,
        owner: &str,
        session: &str,
        run: &str,
    ) -> Result<Option<String>, ApplicationError> {
        let Some(fact) = self.assistant_handover_fact(run)? else {
            return Ok(Some(run.into()));
        };
        if fact.owner != owner || fact.source.session_id != session {
            return Err(ApplicationError::Conflict(
                "assistant reply handover identity changed".into(),
            ));
        }
        let current = sessions(self)?
            .get_channel_assistant(owner, session)
            .map_err(super::super::assistants::assistant_session_error)?
            .ok_or(ApplicationError::Missing)?;
        if current == fact.source {
            return Ok(None);
        }
        if current.run_id != fact.successor {
            return Err(ApplicationError::Conflict(
                "assistant reply handover target changed".into(),
            ));
        }
        if self.metadata(&current.run_id)?.is_none() || self.read_run(&current.run_id)?.is_none() {
            return Ok(None);
        }
        Ok(Some(current.run_id))
    }

    fn assistant_handover_fact(
        &self,
        run: &str,
    ) -> Result<Option<AssistantHandoverFact>, ApplicationError> {
        let Some(runs) = self.kernel_owner(run)? else {
            return Ok(None);
        };
        let mut found = None;
        for record in runs
            .kernel
            .history(&anchor_runtime::graph::validator::run_resource_id(run))
            .map_err(anchor_runtime::graph::scheduler::kernel_error)?
        {
            if record.verdict.is_accepted()
                && record.stamp.principal().kind == PrincipalKind::Code
                && record.stamp.principal().id == anchor_runtime::graph::scheduler::HOST_ID
                && record.submission.action == Action::Request
                && record.submission.content["control"] == "stop"
                && let Some(value) = record.submission.content.get("assistant_handover")
            {
                let fact: AssistantHandoverFact =
                    serde_json::from_value(value.clone()).map_err(storage)?;
                if !fact.valid() || found.as_ref().is_some_and(|prior| prior != &fact) {
                    return Err(storage("assistant handover stop evidence is inconsistent"));
                }
                found = Some(fact);
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests;
