//! Human observations about the existing native tool observation. These do not
//! manufacture a model completion or rewrite Goose history.
use super::*;
use anchor_runtime::graph::{RecoveryAttempt, RecoveryDecision};

fn attempt(fact: &Fact) -> Option<RecoveryAttempt> {
    let observation = fact.tool_observation.as_ref()?;
    if fact.completion.is_some() || !observation["result"].is_null() {
        return None;
    }
    let tool = observation["tool"].as_str()?;
    let identity = serde_json::to_vec(&(
        &fact.key,
        tool,
        &observation["arguments"],
        &observation["native_tool_call"],
    ))
    .ok()?;
    let digest = Sha256::digest(identity);
    let attempt_id = (u64::from_be_bytes(digest[..8].try_into().expect("SHA256 prefix"))
        & i64::MAX as u64)
        .max(1) as i64;
    Some(RecoveryAttempt {
        attempt_id,
        step: 0,
        tool: tool.into(),
        started_at: observation["started_at"]
            .as_str()
            .unwrap_or("unknown")
            .into(),
    })
}

fn record_decision(
    fact: &mut Fact,
    attempt_id: i64,
    decision: RecoveryDecision,
) -> Result<(), GraphError> {
    let pending = attempt(fact).ok_or_else(|| {
        GraphError::CorruptRun("native invocation has no unresolved tool observation".into())
    })?;
    if pending.attempt_id != attempt_id || decision == RecoveryDecision::Retry {
        return Err(GraphError::CorruptRun(
            "recovery decision does not identify an inspected native tool observation".into(),
        ));
    }
    if matches!(&decision, RecoveryDecision::Completed { observation } if observation.trim().is_empty())
    {
        return Err(GraphError::CorruptRun(
            "recovery observation must be nonempty".into(),
        ));
    }
    let value = serde_json::to_value(decision).map_err(GraphError::RunDecode)?;
    let observation = fact.tool_observation.as_mut().expect("pending observation");
    if let Some(existing) = observation.get("operator_decision") {
        if *existing != value {
            return Err(GraphError::CorruptRun(
                "a different decision is already recorded for this native observation".into(),
            ));
        }
    } else {
        observation["operator_decision"] = value;
    }
    Ok(())
}

fn recovery_fact(facts: &Path, key: &InvocationKey) -> Result<Option<Fact>, GraphError> {
    let fact = read_fact(&facts.join(format!("{}.json", crate::host_fs::invocation_stem(key))))?;
    if fact
        .as_ref()
        .is_some_and(|f| f.version != 2 || f.key != *key)
    {
        return Err(GraphError::CorruptRun(
            "native recovery invocation identity changed".into(),
        ));
    }
    Ok(fact)
}

fn pending_attempt(fact: &Fact) -> Option<RecoveryAttempt> {
    if fact
        .tool_observation
        .as_ref()
        .is_some_and(|o| o.get("operator_decision").is_some())
    {
        None
    } else {
        attempt(fact)
    }
}

/// Read unknown effects without preparing a Provider or changing native history.
pub(crate) fn pending_invocation_recovery(
    state: &Path,
    key: &InvocationKey,
) -> Result<Vec<RecoveryAttempt>, GraphError> {
    Ok(recovery_fact(&state.join("goose-acp"), key)?
        .as_ref()
        .and_then(pending_attempt)
        .into_iter()
        .collect())
}

impl GooseNodePort {
    fn recovery_fact(&self, key: &InvocationKey) -> Result<Option<Fact>, GraphError> {
        let fact = recovery_fact(&self.facts, key)?;
        if fact
            .as_ref()
            .is_some_and(|f| self.fixture || f.binary_sha256 != self.binary_sha256)
        {
            return Err(GraphError::CorruptRun(
                "native recovery invocation identity changed".into(),
            ));
        }
        Ok(fact)
    }
    pub(crate) fn pending_recovery(
        &self,
        key: &InvocationKey,
    ) -> Result<Vec<RecoveryAttempt>, GraphError> {
        let Some(fact) = self.recovery_fact(key)? else {
            return Ok(Vec::new());
        };
        Ok(pending_attempt(&fact).into_iter().collect())
    }
    pub(crate) fn record_recovery_decision(
        &self,
        key: &InvocationKey,
        attempt_id: i64,
        decision: RecoveryDecision,
    ) -> Result<(), GraphError> {
        let mut fact = self.recovery_fact(key)?.ok_or_else(|| {
            GraphError::CorruptRun("native recovery invocation is missing".into())
        })?;
        record_decision(&mut fact, attempt_id, decision)?;
        store_fact(
            &self
                .facts
                .join(format!("{}.json", crate::host_fs::invocation_stem(key))),
            &fact,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fact() -> Fact {
        Fact {
            version: 2,
            key: InvocationKey {
                run_id: "run".into(),
                graph_digest: "graph".into(),
                node_id: "agent".into(),
                invocation: 1,
            },
            binary_sha256: "binary".into(),
            session_id: Some("native-session".into()),
            completion: None,
            reason: None,
            model_binding: None,
            conversation_scope: None,
            provider_failure: None,
            tool_observation: Some(
                json!({"tool":"anchor_run","arguments":{"command":"inspect"},"native_tool_call":{"session":"native-session","tool_call":"tool-7"},"result":null}),
            ),
        }
    }
    #[test]
    fn human_observation_is_idempotent_and_never_fakes_tool_success_or_node_completion() {
        let mut fact = fact();
        let pending = attempt(&fact).unwrap();
        let original = fact.tool_observation.clone();
        let decision = RecoveryDecision::Completed {
            observation: "checked external receipt".into(),
        };
        record_decision(&mut fact, pending.attempt_id, decision.clone()).unwrap();
        assert_eq!(attempt(&fact).unwrap(), pending);
        record_decision(&mut fact, pending.attempt_id, decision).unwrap();
        assert!(fact.completion.is_none());
        assert!(fact.tool_observation.as_ref().unwrap()["result"].is_null());
        assert_eq!(
            fact.tool_observation.as_ref().unwrap()["arguments"],
            original.as_ref().unwrap()["arguments"]
        );
        assert!(record_decision(&mut fact, pending.attempt_id, RecoveryDecision::Abort).is_err());
        assert!(
            record_decision(&mut fact, pending.attempt_id + 1, RecoveryDecision::Abort).is_err()
        );
    }
}
