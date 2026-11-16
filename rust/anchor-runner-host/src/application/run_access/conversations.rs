use super::RunView;
use crate::application::{ApplicationError, RunApplication};
use crate::run_metadata::RunMetadata;
use anchor_kernel::{Outcome, Phase, Waiter};
use anchor_platform_session::{SessionStore, TurnStatus};
use std::collections::BTreeSet;

/// An assistant Turn is completed by its delivered reply, never by the Run
/// settling; a settled or stopped assistant ends its running Turns.
fn channel_turn_status(phase: &Phase) -> Option<TurnStatus> {
    match phase {
        Phase::Settled {
            result: Outcome::Done,
        } => None,
        Phase::Settled { .. } => Some(TurnStatus::Failed),
        Phase::Waiting {
            waiter: Waiter::Human { reason },
        } if !matches!(reason.as_str(), "paused" | "budget") => Some(TurnStatus::Stopped),
        _ => None,
    }
}

impl RunApplication {
    pub(crate) fn settle_channel_run(&self, run_id: &str) -> Result<(), ApplicationError> {
        let Some(metadata) = self.metadata(run_id)? else {
            return Ok(());
        };
        let view = self.read_run(run_id)?.ok_or(ApplicationError::Missing)?;
        if view.has_pending_recovery() {
            return Ok(());
        }
        let phase = view.lifecycle.phase;
        let Some(source) = metadata.assistant.as_ref() else {
            return Ok(());
        };
        if self.assistant_has_pending_handover(run_id)? {
            // New user input is already a Running Turn before being claimed.
            // The observed handover stops the old executor, not that queue.
            return Ok(());
        }
        let Some(turn_status) = channel_turn_status(&phase) else {
            return Ok(());
        };
        let sessions = SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
            .map_err(|error| ApplicationError::Storage(error.to_string()))?;
        if sessions
            .get_channel_assistant(&source.owner, &source.session)
            .map_err(|error| ApplicationError::Storage(error.to_string()))?
            .is_none_or(|assistant| assistant.run_id != run_id)
        {
            return Ok(());
        }
        for turn in sessions
            .list_turns(&source.owner, &source.session)
            .map_err(|error| ApplicationError::Storage(error.to_string()))?
        {
            if turn.status == TurnStatus::Running {
                sessions
                    .finish_turn(
                        &source.owner,
                        &source.session,
                        &turn.id,
                        turn_status,
                        Some("assistant execution stopped"),
                    )
                    .map_err(|error| ApplicationError::Storage(error.to_string()))?;
            }
        }
        crate::assistant::signal(&source.session).notify_waiters();
        Ok(())
    }

    pub(in crate::application) fn ensure_predecessor_settled<'record>(
        &self,
        record: impl Into<RunView<'record>>,
        active: &(),
    ) -> Result<(), ApplicationError> {
        self.ensure_wait_closure_settled(record, active, false)
    }

    pub(in crate::application) fn ensure_conversation_delete_settled<'record>(
        &self,
        record: impl Into<RunView<'record>>,
        active: &(),
    ) -> Result<(), ApplicationError> {
        self.ensure_wait_closure_settled(record, active, true)
    }

    fn ensure_wait_closure_settled<'record>(
        &self,
        record: impl Into<RunView<'record>>,
        _active: &(),
        allow_deleted_children: bool,
    ) -> Result<(), ApplicationError> {
        let mut pending = vec![record.into()];
        let mut seen = BTreeSet::new();
        while let Some(record) = pending.pop() {
            let run_id = record.progress().run_id.to_owned();
            if !seen.insert(run_id.clone()) {
                continue;
            }
            let current = match self.read_run(&run_id)? {
                Some(current) => current,
                None => record,
            };
            let worker = current.active();
            let record = current.with_active(worker);
            // A physically parked conversation still owns its current turn
            // while paused or held for budget. Session handoff requires an
            // explicit stop or settlement, as before the facade migration.
            let turn_held = matches!(
                record.phase(),
                super::RunPhase::Waiting {
                    for_what: super::RunWait::Human,
                    reason: Some("paused" | "budget"),
                }
            );
            if turn_held || record.has_pending_recovery() || !record.is_quiescent() {
                return Err(ApplicationError::Conflict(
                    "previous conversation execution has not stopped; wait for it to become inactive".into(),
                ));
            }
            let mut children = record
                .record
                .graph_calls
                .values()
                .filter(|call| call.mode == "wait")
                .filter_map(|call| call.child_run_id.clone())
                .collect::<BTreeSet<_>>();
            children.extend(
                self.child_metadata(&run_id)?
                    .into_iter()
                    .filter_map(|metadata| {
                        metadata
                            .graph_call
                            .as_ref()
                            .filter(|source| source.mode == "wait")
                            .map(|_| metadata.run_id.clone())
                    }),
            );
            for id in children {
                match self.read_run(&id)? {
                    Some(child) => pending.push(child),
                    None if allow_deleted_children => {}
                    None => {
                        return Err(ApplicationError::Conflict(
                            "previous conversation has a missing wait child".into(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// True while this later Run can still read the Run it names as its
    /// predecessor. Every such read happens while the later Run executes the
    /// first invocation of a node, so a recorded first invocation is the
    /// durable evidence that the inheritance is complete.
    pub(in crate::application) fn successor_reads_predecessor<'record>(
        &self,
        predecessor: impl Into<RunView<'record>>,
        successor_metadata: &RunMetadata,
        successor: Option<&RunView<'_>>,
    ) -> Result<bool, ApplicationError> {
        let predecessor = predecessor.into();
        let Some(successor) = successor else {
            // A handover that committed its binding and metadata but not its Run
            // record still needs this Run to be re-admitted, so its progress is
            // unknown and this Run stays retained.
            return Ok(true);
        };
        // Callers can hold an earlier projection. Refresh the authority before
        // deciding whether this successor can ever execute another inheritance.
        let current = self.read_run(successor.progress().run_id)?;
        let successor = match current.as_ref() {
            Some(current) => current,
            None => successor,
        };
        if successor.is_settled() {
            return Ok(false);
        }
        // A Run with its own successor can never resume or recover: the
        // conversation scope check refuses it, so its history is frozen.
        if self.has_conversation_successor(successor.progress().run_id)? {
            return Ok(false);
        }
        let mut reading = BTreeSet::new();
        for dispatch in predecessor.dispatches() {
            // An open slot has not necessarily executed. Protect uncommitted
            // history only when the execution port has durably claimed it.
            if !matches!(
                crate::node_host::load_host_completion_fact(
                    &self.data_root.join("facts"),
                    &dispatch.key,
                )?,
                anchor_runtime::graph::CompletionFact::NotStarted
            ) || crate::goose_acp::has_invocation_fact(&self.data_root, &dispatch.key)?
            {
                reading.insert(dispatch.node_id.clone());
            }
        }
        let predecessor = predecessor.progress();
        let successor = successor.progress();
        for node in &predecessor.snapshot.nodes {
            let executed = predecessor.invocations.get(&node.id).copied().unwrap_or(0) > 0
                || predecessor
                    .results
                    .get(&node.id)
                    .is_some_and(|results| !results.is_empty());
            if executed {
                reading.insert(node.id.clone());
            }
        }
        if let Some(assistant) = successor_metadata.assistant.as_ref() {
            reading.insert(assistant.work_node.clone());
        }
        for node in reading {
            if !successor
                .snapshot
                .nodes
                .iter()
                .any(|candidate| candidate.id == node)
            {
                continue;
            }
            let injected = successor
                .results
                .get(&node)
                .is_some_and(|results| results.iter().any(|result| result.key.invocation == 1));
            if !injected {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;
    use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot};
    use serde_json::json;
    use sha2::{Digest, Sha256};

    fn record(run_id: &str) -> GraphRunRecord {
        GraphRunRecord::create_with_id(
            GraphSnapshot::from_authoring(json!({
                "entry":"work", "agents":{}, "ops":{"work":{"run":"true"}},
                "nodes":[{"id":"work","op":"work"},{"id":"later","op":"work"}],
                "edges":[{"from":"work","to":"later"}]
            }))
            .unwrap(),
            json!({}),
            run_id,
        )
        .unwrap()
    }

    #[test]
    fn assistant_settlement_leaves_completion_to_the_delivered_reply() {
        for (phase, expected) in [
            (Phase::Executing, None),
            (
                Phase::Waiting {
                    waiter: Waiter::Human {
                        reason: "paused".into(),
                    },
                },
                None,
            ),
            (
                Phase::Waiting {
                    waiter: Waiter::Human {
                        reason: "budget".into(),
                    },
                },
                None,
            ),
            (
                Phase::Waiting {
                    waiter: Waiter::Human {
                        reason: "stopped".into(),
                    },
                },
                Some(TurnStatus::Stopped),
            ),
            (
                Phase::Waiting {
                    waiter: Waiter::Child {
                        task: "child".into(),
                    },
                },
                None,
            ),
            (
                Phase::Settled {
                    result: Outcome::Done,
                },
                None,
            ),
            (
                Phase::Settled {
                    result: Outcome::Failed {
                        reason: anchor_kernel::Reason::Unclassified,
                        detail: "known failure".into(),
                    },
                },
                Some(TurnStatus::Failed),
            ),
            (
                Phase::Settled {
                    result: Outcome::Aborted {
                        detail: "operator".into(),
                    },
                },
                Some(TurnStatus::Failed),
            ),
        ] {
            assert_eq!(channel_turn_status(&phase), expected, "{phase:?}");
        }
    }

    #[tokio::test]
    async fn predecessor_checks_keep_kernel_workers_and_parked_states_distinct() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
        for id in fixtures::scenarios(&application) {
            let view = fixtures::view(&application, id);
            let expected = matches!(id, "stopped" | "done" | "failed" | "aborted");
            assert_eq!(
                application.ensure_predecessor_settled(&view, &()).is_ok(),
                expected,
                "{id}"
            );
            assert_eq!(
                application
                    .ensure_conversation_delete_settled(&view, &())
                    .is_ok(),
                expected,
                "{id}"
            );
        }
        fixtures::create(&application, &fixtures::work_record("draining"));
        let drive = fixtures::gated_drive(&application, "draining").await;
        fixtures::control(&application, "draining", "stop", "stopped");
        let view = fixtures::view(&application, "draining");
        assert!(view.active());
        assert!(application.ensure_predecessor_settled(&view, &()).is_err());
        assert!(
            application
                .ensure_conversation_delete_settled(&view, &())
                .is_err()
        );
        drive.drain(&application, "draining").await;
        assert!(application.ensure_predecessor_settled(&view, &()).is_ok());
        assert!(
            application
                .ensure_conversation_delete_settled(&view, &())
                .is_ok()
        );
    }

    #[test]
    fn kernel_predecessor_closure_uses_authoritative_phase_and_session_turn_holds() {
        let root = tempfile::tempdir().unwrap();
        let application =
            RunApplication::new(root.path().join("state"), root.path().join("catalog"));
        fixtures::create(&application, &record("kernel-predecessor"));
        let stale = fixtures::view(&application, "kernel-predecessor");
        assert!(application.ensure_predecessor_settled(&stale, &()).is_err());
        for (reason, allowed) in [("paused", false), ("budget", false), ("stopped", true)] {
            fixtures::control(&application, "kernel-predecessor", "stop", reason);
            assert_eq!(
                application.ensure_predecessor_settled(&stale, &()).is_ok(),
                allowed,
                "{reason}"
            );
            assert_eq!(
                application
                    .ensure_conversation_delete_settled(&stale, &())
                    .is_ok(),
                allowed,
                "{reason}"
            );
            fixtures::control(&application, "kernel-predecessor", "resume", "");
        }
    }

    #[test]
    fn kernel_successor_retention_refreshes_authoritative_settlement() {
        let root = tempfile::tempdir().unwrap();
        let application =
            RunApplication::new(root.path().join("state"), root.path().join("catalog"));
        fixtures::create(&application, &record("predecessor"));
        fixtures::complete(
            &application,
            "predecessor",
            "work",
            fixtures::completion(json!({})),
            None,
        );
        let predecessor = fixtures::view(&application, "predecessor");
        let successor = record("kernel-successor");
        fixtures::create(&application, &successor);
        let stale = fixtures::view(&application, "kernel-successor");
        let metadata = RunMetadata::new(
            successor.run_id.clone(),
            "fixture".into(),
            successor.graph_digest.clone(),
            root.path(),
        )
        .unwrap();
        assert!(
            application
                .successor_reads_predecessor(&predecessor, &metadata, Some(&stale))
                .unwrap()
        );
        fixtures::control(&application, "kernel-successor", "abandon", "operator");
        assert!(
            !application
                .successor_reads_predecessor(&predecessor, &metadata, Some(&stale))
                .unwrap()
        );
    }

    #[test]
    fn successor_retains_uncommitted_goose_history_but_not_unstarted_slots() {
        for (directory, version) in [("goose-acp", 2), ("goose-acp-spike", 1)] {
            let root = tempfile::tempdir().unwrap();
            let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
            let snapshot = GraphSnapshot::from_authoring(json!({
                "entry":"work", "agents":{"role":{"model":"fixture","instructions":"work"}},
                "nodes":[{"id":"work","agent":"role"}], "edges":[]
            }))
            .unwrap();
            for id in ["predecessor", "successor"] {
                let record =
                    GraphRunRecord::create_with_id(snapshot.clone(), json!({}), id).unwrap();
                fixtures::create(&application, &record);
            }
            let predecessor = fixtures::view(&application, "predecessor");
            let successor = fixtures::view(&application, "successor");
            let metadata = RunMetadata::new(
                "successor".into(),
                "fixture".into(),
                successor.record.graph_digest.clone(),
                root.path(),
            )
            .unwrap();
            assert!(predecessor.record.invocations.is_empty());
            assert!(predecessor.record.results.is_empty());
            assert!(
                !application
                    .successor_reads_predecessor(&predecessor, &metadata, Some(&successor))
                    .unwrap()
            );
            let key = &predecessor.dispatches()[0].key;
            let path = root.path().join(directory).join(format!(
                "{:x}.json",
                Sha256::digest(key.durable_key().as_bytes())
            ));
            crate::create_durable_directory(path.parent().unwrap()).unwrap();
            crate::write_durable(
                &path,
                &serde_json::to_vec(&json!({
                    "version":version,"key":key,"binary_sha256":"fixture",
                    "session_id":"native-history","completion":null,"reason":null
                }))
                .unwrap(),
            )
            .unwrap();
            assert!(
                application
                    .successor_reads_predecessor(&predecessor, &metadata, Some(&successor))
                    .unwrap()
            );
            fixtures::complete(
                &application,
                "successor",
                "work",
                fixtures::completion(json!({})),
                None,
            );
            assert!(
                !application
                    .successor_reads_predecessor(&predecessor, &metadata, Some(&successor))
                    .unwrap()
            );
        }
    }

    #[test]
    fn successor_retention_keeps_missing_and_first_invocation_evidence_distinct() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
        fixtures::create(&application, &record("predecessor"));
        fixtures::complete(
            &application,
            "predecessor",
            "work",
            fixtures::completion(json!({})),
            None,
        );
        let predecessor = fixtures::view(&application, "predecessor");
        let successor = record("successor");
        fixtures::create(&application, &successor);
        let metadata = RunMetadata::new(
            successor.run_id.clone(),
            "fixture".into(),
            successor.graph_digest.clone(),
            root.path(),
        )
        .unwrap();
        assert!(
            application
                .successor_reads_predecessor(&predecessor, &metadata, None)
                .unwrap()
        );
        let stale = fixtures::view(&application, "successor");
        assert!(
            application
                .successor_reads_predecessor(&predecessor, &metadata, Some(&stale))
                .unwrap()
        );
        fixtures::control(&application, "successor", "stop", "paused");
        assert!(
            application
                .successor_reads_predecessor(&predecessor, &metadata, Some(&stale))
                .unwrap()
        );
        fixtures::control(&application, "successor", "resume", "");
        fixtures::complete(
            &application,
            "successor",
            "work",
            fixtures::completion(json!({"inherited":true})),
            None,
        );
        assert!(!fixtures::view(&application, "successor").is_settled());
        assert!(
            !application
                .successor_reads_predecessor(&predecessor, &metadata, Some(&stale))
                .unwrap()
        );
        fixtures::control(&application, "successor", "stop", "stopped");
        assert!(
            !application
                .successor_reads_predecessor(&predecessor, &metadata, Some(&stale))
                .unwrap()
        );
    }
}
