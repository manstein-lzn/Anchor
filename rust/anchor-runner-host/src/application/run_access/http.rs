use crate::HostArtifacts;
use crate::application::abandon::{AbandonAction, AbandonResult};
use crate::application::{ApplicationError, RunApplication};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Debug)]
pub(crate) enum RunHttpError {
    Application(ApplicationError),
    Missing,
    Internal(String),
    Unprocessable(String),
}

impl From<ApplicationError> for RunHttpError {
    fn from(error: ApplicationError) -> Self {
        Self::Application(error)
    }
}

impl RunApplication {
    pub(crate) async fn http_run_detail(
        &self,
        id: &str,
        workspace_root: &Path,
    ) -> Result<Value, RunHttpError> {
        let view = self
            .read_run(id)
            .map_err(|error| match error {
                ApplicationError::Storage(message) => RunHttpError::Internal(message),
                error => RunHttpError::Application(error),
            })?
            .ok_or(RunHttpError::Missing)?;
        let record = view.record.as_ref().clone();
        let updated = self.run_updated(id)?;
        let artifacts = HostArtifacts::new(
            self.data_root.join("artifacts"),
            workspace_root.to_path_buf(),
        );
        let mut nodes = serde_json::Map::new();
        for (name, results) in &record.results {
            if let Some(result) = results.last() {
                let (files, availability, artifact_error) =
                    match artifacts.availability(&result.commit) {
                        Ok(availability) => match artifacts.list_files(&result.commit) {
                            Ok(files) => (files, json!(availability), None),
                            Err(error) => (Vec::new(), json!("missing"), Some(error.to_string())),
                        },
                        Err(error) => (Vec::new(), json!("corrupt"), Some(error.to_string())),
                    };
                nodes.insert(
                    name.clone(),
                    json!({
                        "node_id":name,
                        "pass_number":result.key.invocation,
                        "submission":result.completion.submission,
                        "files":files.into_iter().map(|(path, _)| path).collect::<Vec<_>>(),
                        "submitted":result.interruption.is_none(),
                        "exit_status":result.interruption.as_ref().map(|_| "interrupted"),
                        "interruption":result.interruption,
                        "route":result.completion.route,
                        "commit":result.commit.id,
                        "artifact":result.commit,
                        "artifact_availability":availability,
                        "artifact_error":artifact_error,
                        "inputs":[]
                    }),
                );
            }
        }
        let mut plugins = record
            .snapshot
            .nodes
            .iter()
            .map(|node| {
                let bindings = node
                    .plugins
                    .iter()
                    .filter_map(|reference| record.plugin_bindings.get(&reference.id))
                    .map(|binding| {
                        json!({
                            "id":binding.id,
                            "name":binding.id,
                            "description":"",
                            "digest":binding.digest
                        })
                    })
                    .collect::<Vec<_>>();
                (node.id.clone(), json!(bindings))
            })
            .collect::<serde_json::Map<_, _>>();
        let decided = record
            .decided
            .iter()
            .map(|(key, decision)| (key.clone(), json!([decision.selected, decision.sequence])))
            .collect::<serde_json::Map<_, _>>();
        let cursor = view
            .cursor()
            .map(|cursor| json!({"node":cursor.node_id,"pass":cursor.key.invocation,"dir":""}));
        // A parallel region opens one slot per branch, so more than one node can be
        // running at once. The stored activation only records what each branch
        // completed; which branch is executing now is the live frontier, read here
        // and never written back as a second progress fact.
        let mut parallel = serde_json::to_value(&record.parallel)
            .map_err(|error| RunHttpError::Internal(error.to_string()))?;
        if let Some(branches) = parallel.get_mut("branches").and_then(Value::as_array_mut) {
            for dispatch in view.dispatches() {
                let Some(branch) = dispatch.branch.and_then(|index| branches.get_mut(index)) else {
                    continue;
                };
                if branch.get("status").and_then(Value::as_str) == Some("ready") {
                    branch["status"] = json!("running");
                }
                branch["cursor"] = json!({
                    "node_id": dispatch.node_id,
                    "key": {"invocation": dispatch.key.invocation},
                });
            }
        }
        let executed = record.executed_nodes();
        let mut traces = serde_json::Map::new();
        let mut trace_keys = record
            .results
            .values()
            .flatten()
            .map(|result| result.key.clone())
            .collect::<Vec<_>>();
        if let Some(cursor) = view.cursor() {
            trace_keys.push(cursor.key.clone());
        }
        trace_keys.extend(view.frontier.iter().map(|dispatch| dispatch.key.clone()));

        trace_keys.sort_by_key(|key| key.durable_key());
        trace_keys.dedup();
        for key in trace_keys {
            let messages = crate::goose_acp::trace_messages(&self.data_root, &key)
                .map_err(RunHttpError::Internal)?;
            if !messages.is_empty() {
                let trace_key = serde_json::to_string(&(&key.node_id, key.invocation))
                    .map_err(|error| RunHttpError::Internal(error.to_string()))?;
                traces.insert(trace_key, Value::Array(messages));
            }
        }
        let active = self.active_runs(None).await;
        let metadata = self.metadata(id)?.ok_or_else(|| {
            RunHttpError::Unprocessable("Run has no immutable Graph identity metadata".into())
        })?;
        if metadata.assistant.is_some()
            && let Some(policy) =
                crate::assistant_plugins::latest_policy(&self.data_root, &view, &metadata)
                    .map_err(RunHttpError::Internal)?
        {
            plugins.insert(
                policy.node,
                json!(
                    policy
                        .bindings
                        .values()
                        .map(|binding| json!({
                            "id":binding.id,"name":binding.id,"description":"","digest":binding.digest
                        }))
                        .collect::<Vec<_>>()
                ),
            );
        }
        let mut calls = Vec::new();
        for call in record.graph_calls.values() {
            if let Some(child_id) = &call.child_run_id {
                let child = self.read_run(child_id).map_err(RunHttpError::Application)?;
                let child_metadata = self.metadata(child_id)?;
                let source = child_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.graph_call.as_ref());
                let projection = if let Some(child) = &child {
                    Some(self.project_run(child, child_metadata.as_ref()).await?)
                } else {
                    None
                };
                calls.push(json!({
                    "run":child_id,
                    "child_id":child_id,
                    "graph":call.graph,
                    "node":call.identity.node_id,
                    "op":call.identity.node_id,
                    "target":call.graph,
                    "invocation":call.identity.invocation,
                    "mode":call.mode,
                    "active":active.contains(child_id),
                    "projection":projection,
                    "root_run":source.map(|source| &source.root_run),
                }));
            }
        }
        for child_metadata in self.child_metadata(id)? {
            let Some(source) = child_metadata.graph_call.as_ref() else {
                continue;
            };
            if calls
                .iter()
                .any(|call| call["run"] == child_metadata.run_id)
            {
                continue;
            }
            let child = self
                .read_run(&child_metadata.run_id)
                .map_err(RunHttpError::Application)?;
            let projection = if let Some(child) = &child {
                Some(self.project_run(child, Some(&child_metadata)).await?)
            } else {
                None
            };
            calls.push(json!({
                "run":child_metadata.run_id,
                "child_id":child_metadata.run_id,
                "graph":child_metadata.graph,
                "node":source.node,
                "op":source.node,
                "target":child_metadata.graph,
                "invocation":source.invocation,
                "mode":source.mode,
                "active":active.contains(&child_metadata.run_id),
                "projection":projection,
                "root_run":source.root_run,
            }));
        }
        let mut trigger = json!({"source":metadata.trigger_source,"schedule":metadata.schedule,"scheduled_at":metadata.scheduled_at});
        if let Some(source) = metadata.graph_call.as_ref() {
            trigger["graph"] = json!(source.parent_graph);
            trigger["run"] = json!(source.parent_run);
            trigger["node"] = json!(source.node);
            trigger["invocation"] = json!(source.invocation);
            trigger["mode"] = json!(source.mode);
            trigger["root_run"] = json!(source.root_run);
        }
        if let Some(source) = metadata.conversation.as_ref() {
            trigger["session"] = json!(source.session);
            trigger["reply_node"] = json!(source.reply_node);
            trigger["previous_run"] = json!(source.previous_run);
        }
        let channel_reply = metadata.conversation.is_some()
            && self
                .data_root
                .join("channel-replies")
                .join(format!("{id}.json"))
                .try_exists()
                .map_err(|error| RunHttpError::Internal(error.to_string()))?;
        let projection = self.project_run(&view, Some(&metadata)).await?;
        let definition = crate::application::run_projection::run_definition(&record.snapshot);
        let error = if projection.outcome == Some("failed") {
            projection.reason.as_deref().unwrap_or_default()
        } else {
            ""
        };
        Ok(
            json!({"session_call":metadata.session_call,"channel_reply":channel_reply,"graph":metadata.graph,"run":id,"attachments":metadata.attachments,"plugins":plugins,"definition":definition,"state":{"objective":record.snapshot.objective,"started":metadata.created,"updated":updated,"trigger":trigger,"input":record.input,"cursor":cursor,"passes":record.passes,"decided":decided,"nodes":nodes,"executed":executed,"skipped":[],"error":error,"parallel":parallel,"recovery":view.recovery},"calls":calls,"traces":traces,"nodes":record.snapshot.nodes.iter().map(|node|node.id.clone()).collect::<Vec<_>>(),"active":projection.active,"projection":projection}),
        )
    }
}

pub(crate) fn abandon_response(run: &str, result: &AbandonResult) -> Value {
    json!({
        "run": run,
        "action": result.action.as_str(),
        "abandoned": matches!(result.action, AbandonAction::Abandoned),
        "reason": result.reason,
        "requested_at": result.requested_at,
    })
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;
    use crate::run_metadata::{self as metadata, RunMetadata};
    use anchor_runtime::graph::{ArtifactPort, RecoveryAttempt};

    #[tokio::test]
    async fn detail_preserves_phase_json_bytes_for_actual_kernel_facts() {
        for (id, phase_fields, reason, resume, stop, abandon) in [
            (
                "executing",
                r#""phase":"executing""#,
                "null",
                true,
                true,
                true,
            ),
            (
                "paused",
                r#""phase":"waiting","waiting_for":"human""#,
                r#""paused""#,
                true,
                false,
                true,
            ),
            (
                "stopped",
                r#""phase":"waiting","waiting_for":"human""#,
                r#""stopped""#,
                true,
                false,
                true,
            ),
            (
                "budget",
                r#""phase":"waiting","waiting_for":"human""#,
                r#""budget""#,
                true,
                false,
                true,
            ),
            (
                "recovery",
                r#""phase":"waiting","waiting_for":"human""#,
                r#""unknown external tool effects""#,
                true,
                false,
                true,
            ),
            (
                "done",
                r#""phase":"settled","outcome":"done""#,
                "null",
                false,
                false,
                false,
            ),
            (
                "failed",
                r#""phase":"settled","outcome":"failed""#,
                r#""worker failed: recorded reason""#,
                false,
                false,
                false,
            ),
            (
                "aborted",
                r#""phase":"settled","outcome":"aborted""#,
                r#""recorded reason""#,
                false,
                false,
                false,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
            let record = fixtures::work_record(id);
            fixtures::create(&application, &record);
            let workspace = root.path().join("workspaces");
            match id {
                "paused" | "stopped" | "budget" => fixtures::control(&application, id, "stop", id),
                "recovery" => {
                    fixtures::recovery(
                        &application,
                        id,
                        "worker",
                        vec![RecoveryAttempt {
                            attempt_id: 1,
                            step: 1,
                            tool: "fixture-effect".into(),
                            started_at: "2026-10-19T00:00:00Z".into(),
                        }],
                    );
                }
                "done" => {
                    let key = application.dispatch_keys(id).unwrap().remove(0);
                    let artifacts =
                        HostArtifacts::new(root.path().join("artifacts"), workspace.clone());
                    artifacts.prepare_workspace(&key, &[]).unwrap();
                    let completion = fixtures::completion(json!({}));
                    let commit = artifacts.freeze(&key, &completion).await.unwrap();
                    fixtures::complete(&application, id, "worker", completion, Some(commit));
                }
                "failed" => fixtures::fail(&application, id, "worker", "recorded reason"),
                "aborted" => fixtures::control(&application, id, "abandon", "recorded reason"),
                _ => {}
            }
            let view = fixtures::view(&application, id);
            let record = view.record.as_ref();
            let mut metadata = RunMetadata::new(
                id.into(),
                "fixture".into(),
                record.graph_digest.clone(),
                root.path(),
            )
            .unwrap();
            metadata.created = "2026-10-19T00:00:00+00:00".into();
            metadata::save(root.path(), &metadata).unwrap();
            let updated = serde_json::to_string(&application.run_updated(id).unwrap()).unwrap();
            let projection = format!(
                r#"{{{phase_fields},"reason":{reason},"active":false,"actions":{{"pause":false,"stop":{stop},"resume":{resume},"abandon":{abandon}}},"updated":{updated}}}"#
            );
            let cursor = if matches!(id, "done" | "failed" | "aborted") {
                "null"
            } else {
                r#"{"node":"worker","pass":1,"dir":""}"#
            };
            let nodes = if id == "done" {
                let result = &record.results["worker"][0];
                serde_json::to_string(&json!({"worker":{
                    "node_id":"worker","pass_number":1,"submission":"fixture result","files":[],
                    "submitted":true,"exit_status":null,"interruption":null,"route":null,
                    "commit":result.commit.id,"artifact":result.commit,
                    "artifact_availability":"available","artifact_error":null,"inputs":[]
                }}))
                .unwrap()
            } else {
                "{}".into()
            };
            let passes = serde_json::to_string(&record.passes).unwrap();
            let recovery = serde_json::to_string(&view.recovery).unwrap();
            let error = serde_json::to_string(if id == "failed" {
                "worker failed: recorded reason"
            } else {
                ""
            })
            .unwrap();
            let executed = if id == "done" { r#"["worker"]"# } else { "[]" };
            let expected = format!(
                r#"{{"session_call":null,"channel_reply":false,"graph":"fixture","run":"{id}","attachments":[],"plugins":{{"worker":[]}},"definition":{{"entry":"worker","nodes":[{{"id":"worker","plugins":[],"agent":"role"}}],"edges":[],"ops":{{}}}},"state":{{"objective":"test","started":"2026-10-19T00:00:00+00:00","updated":{updated},"trigger":{{"source":"manual","schedule":null,"scheduled_at":null}},"input":{{}},"cursor":{cursor},"passes":{passes},"decided":{{}},"nodes":{nodes},"executed":{executed},"skipped":[],"error":{error},"parallel":null,"recovery":{recovery}}},"calls":[],"traces":{{}},"nodes":["worker"],"active":false,"projection":{projection}}}"#
            );
            let detail = application.http_run_detail(id, &workspace).await.unwrap();
            assert_eq!(
                serde_json::to_vec(&detail).unwrap(),
                expected.as_bytes(),
                "{id}"
            );
        }
    }

    #[test]
    fn abandon_response_preserves_phase_contract_bytes_and_action_distinctions() {
        for (action, wire, abandoned) in [
            (AbandonAction::Abandoned, "abandoned", true),
            (AbandonAction::AlreadyAborted, "already_aborted", false),
        ] {
            let result = AbandonResult {
                action,
                reason: Some("operator".into()),
                requested_at: Some("2026-10-19T00:00:00+00:00".into()),
            };
            let expected = format!(
                r#"{{"run":"http-run","action":"{wire}","abandoned":{abandoned},"reason":"operator","requested_at":"2026-10-19T00:00:00+00:00"}}"#
            );
            assert_eq!(
                serde_json::to_vec(&abandon_response("http-run", &result)).unwrap(),
                expected.as_bytes()
            );
        }
    }

    #[tokio::test]
    async fn detail_keeps_store_and_application_errors_distinct_and_ordered() {
        let root = tempfile::tempdir().unwrap();
        let application = RunApplication::new(root.path().into(), root.path().join("catalog"));
        let workspace = root.path().join("workspaces");
        assert!(matches!(
            application.http_run_detail("http-run", &workspace).await,
            Err(RunHttpError::Missing)
        ));
        fixtures::create(&application, &fixtures::work_record("http-run"));
        assert!(
            matches!(application.http_run_detail("http-run", &workspace).await,
            Err(RunHttpError::Unprocessable(message)) if message == "Run has no immutable Graph identity metadata")
        );
        std::fs::create_dir_all(root.path().join("run-metadata")).unwrap();
        std::fs::write(root.path().join("run-metadata/http-run.json"), "{}").unwrap();
        assert!(
            matches!(application.http_run_detail("http-run", &workspace).await,
            Err(RunHttpError::Internal(message)) if message.contains("missing field"))
        );
        // The removed Run store is rejected by name before metadata interpretation.
        std::fs::create_dir_all(root.path().join("runs")).unwrap();
        std::fs::write(root.path().join("runs/old-run.json"), "{}").unwrap();
        assert!(
            matches!(application.http_run_detail("old-run", &workspace).await,
            Err(RunHttpError::Internal(message)) if message.contains("legacy"))
        );
    }
}
