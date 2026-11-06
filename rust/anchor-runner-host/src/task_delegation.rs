//! Trusted delegation identity. Execution and timing facts stay with their owners.
use anchor_runtime::graph::{GraphTaskPolicy, InvocationKey, PluginBinding};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphTaskGrants {
    pub(crate) bindings: BTreeMap<String, PluginBinding>,
    pub(crate) targets: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskGraphOwnership {
    pub(crate) scope: String,
    pub(crate) source: std::path::PathBuf,
    pub(crate) graph: String,
    pub(crate) request: crate::run_metadata::AdmissionRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TaskOriginSource {
    AgentDelegation {
        key: InvocationKey,
    },
    OperatorAssignment {
        schedule: String,
        node_id: String,
        instruction: String,
    },
}

fn deserialize_source<'de, D>(deserializer: D) -> Result<TaskOriginSource, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Remove the legacy key reader after this installation's persisted task
    // origins have been rewritten with explicit source provenance.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Source(TaskOriginSource),
        Legacy(InvocationKey),
    }
    Ok(match Input::deserialize(deserializer)? {
        Input::Source(source) => source,
        Input::Legacy(key) => TaskOriginSource::AgentDelegation { key },
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskOrigin {
    pub(crate) owner: String,
    pub(crate) session: Option<String>,
    pub(crate) graph: String,
    #[serde(alias = "key", deserialize_with = "deserialize_source")]
    pub(crate) source: TaskOriginSource,
    pub(crate) authorization_digest: String,
    pub(crate) policy: GraphTaskPolicy,
    pub(crate) bindings: BTreeMap<String, PluginBinding>,
    #[serde(default)]
    pub(crate) targets: BTreeMap<String, String>,
}

impl TaskOrigin {
    pub(crate) fn node_id(&self) -> &str {
        match &self.source {
            TaskOriginSource::AgentDelegation { key } => &key.node_id,
            TaskOriginSource::OperatorAssignment { node_id, .. } => node_id,
        }
    }

    pub(crate) fn agent_key(&self) -> Option<&InvocationKey> {
        match &self.source {
            TaskOriginSource::AgentDelegation { key } => Some(key),
            TaskOriginSource::OperatorAssignment { .. } => None,
        }
    }

    pub(crate) fn credential_owner(&self) -> String {
        // Sessions retain the installation identity; ordinary Run metadata
        // already stores its credential scope.
        match self.session {
            Some(_) => crate::credential_store::binding_owner(&self.owner),
            None => self.owner.clone(),
        }
    }

    pub(crate) fn scope_key(&self) -> String {
        use sha2::{Digest, Sha256};
        let scope = serde_json::to_vec(&(
            &self.owner,
            &self.graph,
            self.node_id(),
            &self.session,
            self.agent_key()
                .filter(|_| self.session.is_none())
                .map(|key| &key.run_id),
        ))
        .expect("trusted task scope is serializable");
        format!("{:x}", Sha256::digest(scope))
    }

    pub(crate) fn same_scope(&self, other: &Self) -> bool {
        self.owner == other.owner
            && self.graph == other.graph
            && self.node_id() == other.node_id()
            && match (&self.session, &other.session) {
                (Some(left), Some(right)) => left == right,
                (None, None) => match (self.agent_key(), other.agent_key()) {
                    (Some(left), Some(right)) => left.run_id == right.run_id,
                    _ => false,
                },
                _ => false,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_assignment_and_legacy_delegation_share_session_scope_without_fabricating_a_key() {
        let legacy = serde_json::json!({
            "owner":"local","session":"session","graph":"assistant",
            "key":{"run_id":"real-run","graph_digest":"digest","node_id":"work","invocation":1},
            "authorization_digest":"digest","policy":GraphTaskPolicy::default(),
            "bindings":{},"targets":{}
        });
        let delegated: TaskOrigin = serde_json::from_value(legacy).unwrap();
        assert_eq!(delegated.agent_key().unwrap().run_id, "real-run");
        let saved = serde_json::to_value(&delegated).unwrap();
        assert!(saved.get("key").is_none());
        assert_eq!(saved["source"]["kind"], "agent_delegation");
        assert_eq!(
            serde_json::from_value::<TaskOrigin>(saved).unwrap(),
            delegated
        );
        let mut assigned = delegated.clone();
        assigned.source = TaskOriginSource::OperatorAssignment {
            schedule: "schedule".into(),
            node_id: "work".into(),
            instruction: "Deliver the result to this Session".into(),
        };
        assert!(assigned.agent_key().is_none());
        assert!(assigned.same_scope(&delegated));
        assert_eq!(assigned.scope_key(), delegated.scope_key());
        let saved = serde_json::to_value(&assigned).unwrap();
        assert!(saved["source"].get("key").is_none());
        assert_eq!(
            serde_json::from_value::<TaskOrigin>(saved).unwrap(),
            assigned
        );
        assigned.session = Some("another-session".into());
        assert!(!assigned.same_scope(&delegated));
        assert_ne!(assigned.scope_key(), delegated.scope_key());
    }

    #[test]
    fn credential_owner_maps_session_identity_once_and_preserves_ordinary_run_scope() {
        let mut origin = TaskOrigin {
            owner: format!("api:{}", "a".repeat(64)),
            session: Some("session".into()),
            graph: "source".into(),
            source: TaskOriginSource::AgentDelegation {
                key: InvocationKey {
                    run_id: "source-run".into(),
                    graph_digest: "source-digest".into(),
                    node_id: "agent".into(),
                    invocation: 1,
                },
            },
            authorization_digest: "source-digest".into(),
            policy: GraphTaskPolicy::default(),
            bindings: BTreeMap::new(),
            targets: BTreeMap::new(),
        };
        let credential_owner = crate::credential_store::binding_owner(&origin.owner);
        assert_eq!(origin.credential_owner(), credential_owner);
        origin.session = None;
        origin.owner = credential_owner.clone();
        assert_eq!(origin.credential_owner(), credential_owner);
        assert_ne!(
            origin.credential_owner(),
            crate::credential_store::binding_owner(&origin.owner)
        );
        origin.owner = "local".into();
        assert_eq!(origin.credential_owner(), "local");
        origin.session = Some("session".into());
        assert_eq!(origin.credential_owner(), "local");
    }
}
