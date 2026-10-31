//! Read-only Run presentation shared by HTTP and Pilot. These observations are
//! advisory; control still validates identity, scope and leases at write time.
use anchor_runtime::graph::GraphSnapshot;
use serde_json::{Value, json};

/// Canvas data comes exclusively from the compiled, frozen snapshot, including
/// expanded inline modules. Do not expose execution input, scripts or host
/// parameters as part of this display projection, or consult the live Graph.
pub(crate) fn run_definition(snapshot: &GraphSnapshot) -> Value {
    let ops = snapshot
        .ops
        .iter()
        .map(|(name, op)| {
            let display = if let Some(call) = op.get("call") {
                json!({"call":{"graph":call["graph"],"mode":call["mode"]}})
            } else if let Some(fanout) = op.get("fanout") {
                json!({"fanout":fanout})
            } else if op.get("join").is_some() {
                json!({"join":{}})
            } else if let Some(host) = op.get("host") {
                json!({"host":{"operation":host["operation"]}})
            } else {
                json!({"run":""})
            };
            (name.clone(), display)
        })
        .collect::<serde_json::Map<_, _>>();
    json!({
        "entry":snapshot.entry,
        "nodes":snapshot.nodes.iter().map(|node| {
            let mut display = json!({"id":node.id,"plugins":node.plugins});
            if let Some(agent) = &node.agent {
                display["agent"] = json!(agent);
            }
            if let Some(op) = &node.op {
                display["op"] = json!(op);
            }
            display
        }).collect::<Vec<_>>(),
        "edges":snapshot.edges,
        "ops":ops,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{RunApplication, run_access::fixtures};
    use crate::run_metadata::RunMetadata;

    fn metadata(application: &RunApplication, id: &str) -> RunMetadata {
        let record = fixtures::record(application, id);
        RunMetadata::new(
            id.into(),
            "fixture".into(),
            record.graph_digest,
            &application.catalog_root,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn inactive_phases_reuse_control_guards_and_liveness() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
        std::fs::create_dir_all(&application.catalog_root).unwrap();
        for id in fixtures::scenarios(&application) {
            let view = fixtures::view(&application, id);
            let metadata = metadata(&application, id);
            let projection = application
                .project_run(&view, Some(&metadata))
                .await
                .unwrap();
            assert!(!projection.actions.pause, "{id}");
            assert_eq!(
                projection.actions.resume,
                matches!(
                    id,
                    "executing" | "paused" | "stopped" | "budget" | "recovery"
                ),
                "{id}"
            );
            assert_eq!(projection.actions.stop, id == "executing", "{id}");
            assert_eq!(
                projection.actions.abandon,
                !matches!(id, "done" | "failed" | "aborted"),
                "{id}"
            );
            assert_eq!(
                projection.phase == "waiting"
                    || matches!(projection.outcome, Some("failed" | "aborted"))
                    || (projection.phase == "executing" && !projection.active),
                id != "done",
                "{id}"
            );
            assert_eq!(
                view.is_quiescent(),
                !matches!(id, "executing" | "child" | "recovery"),
                "{id}"
            );
        }
    }

    #[tokio::test]
    async fn supersession_requires_an_admitted_successor_and_only_reads_its_history() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
        std::fs::create_dir_all(&application.catalog_root).unwrap();
        for id in ["parent", "unrelated"] {
            fixtures::create(&application, &fixtures::work_record(id));
        }
        let parent_metadata = metadata(&application, "parent");
        crate::run_metadata::save(&application.data_root, &parent_metadata).unwrap();
        let successor = fixtures::work_record("successor");
        let mut pending = RunMetadata::new(
            successor.run_id.clone(),
            "fixture".into(),
            successor.graph_digest.clone(),
            &application.catalog_root,
        )
        .unwrap();
        pending.conversation = Some(crate::run_metadata::ConversationSource {
            session: "session".into(),
            reply_node: "worker".into(),
            previous_run: Some("parent".into()),
        });
        crate::run_metadata::save(&application.data_root, &pending).unwrap();
        let parent = fixtures::view(&application, "parent");
        std::fs::write(
            application
                .data_root
                .join("kernel-runs/run%2Funrelated/history.jsonl"),
            b"invalid record\n",
        )
        .unwrap();
        assert!(application.read_run("unrelated").is_err());
        let projection = application
            .project_run(&parent, Some(&parent_metadata))
            .await
            .unwrap();
        assert!(!projection.superseded);
        assert!(projection.actions.resume);
        fixtures::create(&application, &successor);
        let projection = application
            .project_run(&parent, Some(&parent_metadata))
            .await
            .unwrap();
        assert!(projection.superseded);
        assert!(!projection.actions.resume);
    }

    #[tokio::test]
    async fn pending_control_and_session_coordination_are_observed_once() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
        std::fs::create_dir_all(&application.catalog_root).unwrap();
        fixtures::create(&application, &fixtures::work_record("projection-test"));
        let stale = fixtures::view(&application, "projection-test");
        let metadata = metadata(&application, "projection-test");
        let drive = fixtures::gated_drive(&application, "projection-test").await;
        application.pause("projection-test").await.unwrap();
        let projection = application
            .project_run(&stale, Some(&metadata))
            .await
            .unwrap();
        assert_eq!(projection.phase, "executing");
        assert!(projection.active);
        assert!(projection.actions.pause);
        assert!(projection.actions.stop);
        assert!(projection.actions.abandon);
        assert!(!projection.actions.resume);
        application.stop("projection-test").await.unwrap();
        let projection = application
            .project_run(&stale, Some(&metadata))
            .await
            .unwrap();
        assert_eq!(projection.phase, "waiting");
        assert!(projection.active);
        assert!(!projection.actions.resume);
        drive.drain(&application, "projection-test").await;
        let mut coordinated = metadata;
        coordinated.session_call = Some(serde_json::from_value(json!({
            "context":{"session":"session","reply_node":"worker","conversation_id":"conversation","channel":{}},
            "status":"pending","error":""
        })).unwrap());
        let projection = application
            .project_run(&stale, Some(&coordinated))
            .await
            .unwrap();
        assert!(!projection.actions.pause);
        assert!(!projection.actions.resume);
        assert_eq!(projection.phase, "waiting");
    }

    #[tokio::test]
    async fn projection_serialization_preserves_executing_paused_and_stopped_bytes() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
        std::fs::create_dir_all(&application.catalog_root).unwrap();
        fixtures::scenarios(&application);
        for (id, prefix) in [
            (
                "executing",
                r#"{"phase":"executing","reason":null,"active":false,"actions":{"pause":false,"stop":true,"resume":true,"abandon":true},"updated":"#,
            ),
            (
                "paused",
                r#"{"phase":"waiting","waiting_for":"human","reason":"paused","active":false,"actions":{"pause":false,"stop":false,"resume":true,"abandon":true},"updated":"#,
            ),
            (
                "stopped",
                r#"{"phase":"waiting","waiting_for":"human","reason":"stopped","active":false,"actions":{"pause":false,"stop":false,"resume":true,"abandon":true},"updated":"#,
            ),
        ] {
            let view = fixtures::view(&application, id);
            let projection = application
                .project_run(&view, Some(&metadata(&application, id)))
                .await
                .unwrap();
            let expected = format!(
                "{prefix}{}}}",
                serde_json::to_string(&application.run_updated(id).unwrap()).unwrap()
            );
            assert_eq!(
                serde_json::to_vec(&projection).unwrap(),
                expected.as_bytes(),
                "{id}"
            );
        }
    }

    #[test]
    fn frozen_canvas_projects_expanded_modules_and_hides_execution_input() {
        let snapshot = serde_json::from_value(json!({
            "objective":"private objective", "input":{"secret":"secret-input"},
            "entry":"module.worker", "agents":{"module.role":{"model":"fixture","instructions":"private instructions"}},
            "ops":{"call":{"call":{"graph":"target","mode":"wait","input":{"secret":"private-call-input"}}},"finish":{"run":"private script"}},
            "nodes":[{"id":"module.worker","agent":"module.role","with":{"private":"value"}}, {"id":"call","op":"call"}, {"id":"finish","op":"finish"}],
            "edges":[{"from":"module.worker","to":"call"},{"from":"call","to":"finish"}]
        })).unwrap();
        let definition = run_definition(&snapshot);
        assert_eq!(definition["entry"], "module.worker");
        assert_eq!(definition["nodes"][0]["id"], "module.worker");
        assert_eq!(definition["edges"][0]["from"], "module.worker");
        assert_eq!(
            definition["ops"]["call"]["call"],
            json!({"graph":"target","mode":"wait"})
        );
        assert!(!definition.to_string().contains("private"));
        assert!(definition.get("graphs").is_none());
    }
}
