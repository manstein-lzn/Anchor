use super::RunView;
use crate::{assistant::TurnBinding, run_metadata::RunMetadata};
use anchor_runtime::graph::{FileRunLeaseProvider, GraphError, InvocationKey, RunLease};
use std::path::Path;

pub(crate) fn graph_lease(root: &Path, source: &Path) -> Result<Box<dyn RunLease>, GraphError> {
    FileRunLeaseProvider::new(root.join("admission-locks"))
        .acquire_lease(&crate::assistant_plugins::graph_lease_key(source)?)
}

pub(crate) fn graph_read_lease(
    root: &Path,
    source: &Path,
) -> Result<Box<dyn RunLease>, GraphError> {
    FileRunLeaseProvider::new(root.join("admission-locks"))
        .acquire_read_lease(&crate::assistant_plugins::graph_lease_key(source)?)
}

impl RunView<'_> {
    pub(crate) fn executable_assistant_binding(
        &self,
        root: &Path,
        metadata: &RunMetadata,
    ) -> Result<Option<TurnBinding>, String> {
        let Some(source) = &metadata.assistant else {
            return Ok(None);
        };
        let Some(cursor) = self.cursor() else {
            return Ok(None);
        };
        let mut binding = crate::assistant::binding(root, &cursor.key)?;
        if binding.is_none() && cursor.node_id != source.wait_node {
            for commit in &cursor.input_commits {
                let key = InvocationKey {
                    node_id: commit.node_id.clone(),
                    invocation: commit.invocation,
                    ..cursor.key.clone()
                };
                if let Some(prior) = crate::assistant::binding(root, &key)? {
                    binding = Some(prior);
                    break;
                }
            }
        }
        if binding.is_none() && cursor.node_id != source.wait_node {
            return Err("assistant active invocation has no Turn binding".into());
        }
        Ok(binding)
    }

    pub(crate) fn latest_assistant_binding(
        &self,
        root: &Path,
    ) -> Result<Option<TurnBinding>, String> {
        let last = self
            .record
            .results
            .values()
            .flatten()
            .max_by_key(|result| result.sequence);
        let cursor_binding = self
            .cursor()
            .map(|cursor| crate::assistant::binding(root, &cursor.key))
            .transpose()?
            .flatten();
        match cursor_binding {
            Some(binding) => Ok(Some(binding)),
            None => last
                .map(|result| crate::assistant::binding(root, &result.key))
                .transpose()
                .map(Option::flatten),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;
    use crate::application::RunApplication;
    use crate::run_metadata::AssistantSource;
    use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;

    fn fixture(
        parallel: bool,
    ) -> (
        tempfile::TempDir,
        RunApplication,
        GraphRunRecord,
        RunMetadata,
    ) {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
        let graph = if parallel {
            json!({
                "objective":"assistant", "entry":"fork",
                "agents":{"worker":{"model":"fixture","instructions":"work"}},
                "ops":{"fork":{"fanout":{"join":"join"}},"join":{"join":{}}},
                "nodes":[{"id":"fork","op":"fork"},{"id":"left","agent":"worker"},
                    {"id":"right","agent":"worker"},{"id":"join","op":"join"}],
                "edges":[{"from":"fork","to":"left"},{"from":"fork","to":"right"},
                    {"from":"left","to":"join"},{"from":"right","to":"join"}]
            })
        } else {
            json!({
                "objective":"assistant", "entry":"wait",
                "agents":{"worker":{"model":"fixture","instructions":"work"}},
                "ops":{"wait":{"host":{"operation":"session.wait_input"}},"reply":{"host":{"operation":"session.reply"}}},
                "nodes":[{"id":"wait","op":"wait"},{"id":"agent","agent":"worker"},{"id":"reply","op":"reply"}],
                "edges":[{"from":"wait","to":"agent"},{"from":"agent","to":"reply"},{"from":"reply","to":"wait"}]
            })
        };
        let record = GraphRunRecord::create_with_id(
            GraphSnapshot::from_authoring(graph).unwrap(),
            json!({}),
            "assistant-policy-test",
        )
        .unwrap();
        fixtures::create(&application, &record);
        let mut metadata = RunMetadata::new(
            record.run_id.clone(),
            "assistant".into(),
            record.graph_digest.clone(),
            root.path(),
        )
        .unwrap();
        metadata.assistant = Some(AssistantSource {
            owner: "owner".into(),
            session: "session".into(),
            wait_node: "wait".into(),
            work_node: "agent".into(),
            reply_node: "reply".into(),
        });
        (root, application, record, metadata)
    }

    fn invocation(record: &GraphRunRecord, node: &str, number: u64) -> InvocationKey {
        InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: node.into(),
            invocation: number,
        }
    }

    fn advance_to(application: &RunApplication, node: &str, number: u64) {
        for _ in 0..20 {
            let keys = application.dispatch_keys("assistant-policy-test").unwrap();
            if keys
                .iter()
                .any(|key| key.node_id == node && key.invocation == number)
            {
                return;
            }
            let next = keys.first().expect("fixture frontier");
            fixtures::complete(
                application,
                &next.run_id,
                &next.node_id,
                fixtures::completion(json!({})),
                None,
            );
        }
        panic!("fixture did not reach requested invocation");
    }

    fn binding_path(root: &Path, key: &InvocationKey) -> PathBuf {
        root.join("assistant-invocations").join(format!(
            "{:x}.json",
            Sha256::digest(key.durable_key().as_bytes())
        ))
    }

    fn save_binding(root: &Path, key: &InvocationKey, turn: &str) -> TurnBinding {
        let binding = TurnBinding {
            key: key.clone(),
            wait_key: "wait".into(),
            inbound: "inbound".into(),
            turn: turn.into(),
            pending_inbounds: Vec::new(),
            pending_truncated: false,
            notification: None,
        };
        crate::assistant::save_immutable(&binding_path(root, key), &binding).unwrap();
        binding
    }

    #[test]
    fn executable_binding_prefers_frontier_then_first_bound_input_commit() {
        let (root, application, record, metadata) = fixture(true);
        advance_to(&application, "join", 1);
        let current = invocation(&record, "join", 1);
        let first = invocation(&record, "left", 1);
        let second = invocation(&record, "right", 1);
        let expected = save_binding(root.path(), &first, "first");
        save_binding(root.path(), &second, "second");
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .executable_assistant_binding(root.path(), &metadata)
                .unwrap(),
            Some(expected)
        );
        let expected = save_binding(root.path(), &current, "current");
        std::fs::write(binding_path(root.path(), &first), b"{invalid").unwrap();
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .executable_assistant_binding(root.path(), &metadata)
                .unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn executable_binding_requires_work_binding_but_does_not_scan_idle_history() {
        let (root, application, record, mut metadata) = fixture(false);
        advance_to(&application, "agent", 1);
        save_binding(root.path(), &invocation(&record, "agent", 1), "prior");
        advance_to(&application, "wait", 2);
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .executable_assistant_binding(root.path(), &metadata)
                .unwrap(),
            None
        );
        advance_to(&application, "agent", 2);
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .executable_assistant_binding(root.path(), &metadata)
                .unwrap_err(),
            "assistant active invocation has no Turn binding"
        );
        fixtures::control(&application, &record.run_id, "abandon", "operator");
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .executable_assistant_binding(root.path(), &metadata)
                .unwrap(),
            None
        );
        metadata.assistant = None;
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .executable_assistant_binding(root.path(), &metadata)
                .unwrap(),
            None
        );
    }

    #[test]
    fn latest_binding_prefers_frontier_then_highest_sequence_result() {
        let (root, application, record, _) = fixture(false);
        advance_to(&application, "wait", 2);
        save_binding(root.path(), &invocation(&record, "wait", 1), "earlier");
        let latest = invocation(&record, "reply", 1);
        let latest_binding = save_binding(root.path(), &latest, "latest");
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .latest_assistant_binding(root.path())
                .unwrap(),
            Some(latest_binding.clone())
        );
        let current_binding = save_binding(root.path(), &invocation(&record, "wait", 2), "current");
        std::fs::write(binding_path(root.path(), &latest), b"{invalid").unwrap();
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .latest_assistant_binding(root.path())
                .unwrap(),
            Some(current_binding)
        );
        fixtures::control(&application, &record.run_id, "abandon", "operator");
        assert!(
            fixtures::view(&application, &record.run_id)
                .latest_assistant_binding(root.path())
                .is_err()
        );
        std::fs::write(
            binding_path(root.path(), &latest),
            serde_json::to_vec(&latest_binding).unwrap(),
        )
        .unwrap();
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .latest_assistant_binding(root.path())
                .unwrap(),
            Some(latest_binding)
        );
    }

    #[test]
    fn latest_binding_does_not_search_older_results_or_input_commits() {
        let (root, application, record, _) = fixture(true);
        advance_to(&application, "join", 1);
        save_binding(root.path(), &invocation(&record, "left", 1), "earlier");
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .latest_assistant_binding(root.path())
                .unwrap(),
            None
        );
    }

    #[test]
    fn corrupt_frontier_binding_is_not_masked_by_commits_or_history() {
        let (root, application, record, metadata) = fixture(false);
        advance_to(&application, "agent", 1);
        let current = invocation(&record, "agent", 1);
        save_binding(root.path(), &current, "current");
        save_binding(root.path(), &invocation(&record, "wait", 1), "prior");
        std::fs::write(binding_path(root.path(), &current), b"{invalid").unwrap();
        let expected = crate::assistant::binding(root.path(), &current).unwrap_err();
        let view = fixtures::view(&application, &record.run_id);
        assert_eq!(
            view.executable_assistant_binding(root.path(), &metadata)
                .unwrap_err(),
            expected
        );
        assert_eq!(
            view.latest_assistant_binding(root.path()).unwrap_err(),
            expected
        );
    }

    #[test]
    fn corrupt_input_binding_stops_the_ordered_commit_scan() {
        let (root, application, record, metadata) = fixture(true);
        advance_to(&application, "join", 1);
        let first = invocation(&record, "left", 1);
        save_binding(root.path(), &first, "first");
        save_binding(root.path(), &invocation(&record, "right", 1), "second");
        std::fs::write(binding_path(root.path(), &first), b"{invalid").unwrap();
        assert_eq!(
            fixtures::view(&application, &record.run_id)
                .executable_assistant_binding(root.path(), &metadata)
                .unwrap_err(),
            crate::assistant::binding(root.path(), &first).unwrap_err()
        );
    }

    #[test]
    fn graph_read_leases_share_identity_and_exclude_writers() {
        let root = tempfile::tempdir().unwrap();
        let first = graph_read_lease(root.path(), root.path()).unwrap();
        let second = graph_read_lease(root.path(), &root.path().join(".")).unwrap();
        assert!(graph_lease(root.path(), root.path()).is_err());
        drop(first);
        assert!(graph_lease(root.path(), root.path()).is_err());
        drop(second);
        let writer = graph_lease(root.path(), root.path()).unwrap();
        assert!(graph_read_lease(root.path(), root.path()).is_err());
        drop(writer);
        assert!(graph_read_lease(root.path(), root.path()).is_ok());
    }
}
