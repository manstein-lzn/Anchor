//! Trusted cross-Run inputs. Framework Session owns dialogue; this adapter
//! exposes only same-node predecessor files and the existing native trace.
use super::*;
use anchor_runtime::{
    ReadOnlyInput, ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent,
    graph::RunStatus,
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, future::Future, sync::Arc};

pub(super) const HISTORY_TOOL: &str = "anchor_conversation_history";

pub(super) struct Conversation {
    pub key: String,
    pub reply_node: String,
    previous: Vec<GraphRunRecord>,
}

impl HostIoResolver {
    pub(crate) fn conversation_predecessors(
        &self,
        key: &InvocationKey,
    ) -> Result<Vec<InvocationKey>, String> {
        let Some(conversation) = self.conversation(key)? else {
            return Ok(Vec::new());
        };
        let mut keys = Vec::new();
        let root = self.local_inputs.state_root();
        let current = crate::application::metadata::load(root, &key.run_id)
            .map_err(|error| format!("conversation metadata: {error:?}"))?
            .ok_or("conversation metadata is missing")?;
        let successors = crate::application::RunApplication::new(root.to_owned(), root.to_owned())
            .settled_session_call_successors(&current)
            .map_err(|error| format!("conversation Session continuation: {error:?}"))?;
        for invocation in (1..key.invocation).rev() {
            keys.push(InvocationKey {
                invocation,
                ..key.clone()
            });
        }
        for record in successors.into_iter().chain(conversation.previous) {
            if !record
                .snapshot
                .nodes
                .iter()
                .any(|node| node.id == key.node_id && node.agent.is_some())
            {
                continue;
            }
            for invocation in (1..=record.invocations.get(&key.node_id).copied().unwrap_or(0)).rev()
            {
                keys.push(InvocationKey {
                    run_id: record.run_id.clone(),
                    graph_digest: record.graph_digest.clone(),
                    node_id: key.node_id.clone(),
                    invocation,
                });
            }
        }
        Ok(keys)
    }
    pub(super) fn conversation(&self, key: &InvocationKey) -> Result<Option<Conversation>, String> {
        let root = self.local_inputs.state_root();
        let Some(current) = crate::application::metadata::load(root, &key.run_id)
            .map_err(|error| format!("conversation metadata: {error:?}"))?
        else {
            return Ok(None);
        };
        let Some(binding) = &current.conversation else {
            return Ok(None);
        };
        if current.graph_digest != key.graph_digest {
            return Err("conversation invocation differs from admitted Graph".into());
        }
        let hint = conversation_hint_for(&current, &key.node_id)
            .expect("conversation binding checked above");
        let graph = crate::application::RunApplication::graph_identity(&current.bundle_source)
            .map_err(|error| format!("conversation graph identity: {error:?}"))?
            .to_string_lossy()
            .into_owned();
        let mut next = binding.previous_run.clone();
        let mut seen = BTreeSet::from([key.run_id.clone()]);
        let mut previous = Vec::new();
        while let Some(id) = next {
            let Some(metadata) = crate::application::metadata::load(root, &id)
                .map_err(|error| format!("conversation predecessor: {error:?}"))?
            else {
                // A deleted link is bridged by its tombstone: it still names the
                // predecessor it held, so the walk continues past it. Only a
                // deletion from this same conversation explains the gap; a plain
                // missing predecessor stays retained corruption.
                let deletion = crate::run_deletions::load(root, &id)
                    .map_err(|error| format!("conversation deletion: {error}"))?
                    .filter(|deletion| {
                        deletion.explains(&graph, &binding.session, &binding.reply_node)
                    })
                    .ok_or("conversation predecessor metadata is missing")?;
                if !seen.insert(id.clone()) {
                    return Err("conversation lineage contains a cycle".into());
                }
                next = deletion.previous_run().map(str::to_owned);
                continue;
            };
            if !seen.insert(id.clone()) {
                return Err("conversation lineage contains a cycle".into());
            }
            let prior_binding = metadata
                .conversation
                .as_ref()
                .ok_or("conversation predecessor has no binding")?;
            if metadata.bundle_source != current.bundle_source
                || prior_binding.session != binding.session
                || prior_binding.reply_node != binding.reply_node
            {
                return Err("conversation predecessor belongs to another conversation".into());
            }
            let record = self
                .run_store
                .load(&id)
                .map_err(|error| error.to_string())?
                .ok_or("conversation predecessor has no admitted Run")?;
            if record.graph_digest != metadata.graph_digest
                || !matches!(
                    record.status,
                    RunStatus::Completed
                        | RunStatus::Stopped
                        | RunStatus::Failed
                        | RunStatus::Aborted
                )
            {
                return Err("conversation predecessor is not a settled matching Run".into());
            }
            next = prior_binding.previous_run.clone();
            previous.push(record);
        }
        Ok(Some(Conversation {
            key: hint.key,
            reply_node: binding.reply_node.clone(),
            previous,
        }))
    }

    pub(super) fn previous_mount(
        &self,
        key: &InvocationKey,
        conversation: &Conversation,
    ) -> Result<Option<ReadOnlyInput>, String> {
        if key.invocation != 1 {
            return Ok(None);
        }
        for prior in &conversation.previous {
            let Some(&invocation) = prior.invocations.get(&key.node_id) else {
                continue;
            };
            let source = InvocationKey {
                run_id: prior.run_id.clone(),
                graph_digest: prior.graph_digest.clone(),
                node_id: key.node_id.clone(),
                invocation,
            };
            let committed = prior
                .results
                .get(&key.node_id)
                .and_then(|results| {
                    results
                        .iter()
                        .find(|result| result.key == source && result.interruption.is_none())
                })
                .map(|result| &result.commit);
            if committed.is_none()
                && !self
                    .artifacts
                    .workspace_path(&source)
                    .map_err(|error| error.to_string())?
                    .exists()
            {
                continue;
            }
            return self
                .artifacts
                .previous_input(key, &source, committed)
                .map(Some)
                .map_err(|error| error.to_string());
        }
        Ok(None)
    }

    pub(super) fn conversation_tools(
        &self,
        inner: Arc<dyn ToolPort>,
        key: &InvocationKey,
        conversation: Conversation,
    ) -> Arc<dyn ToolPort> {
        Arc::new(HistoryTools {
            inner,
            node: key.node_id.clone(),
            data_root: self.local_inputs.state_root().to_path_buf(),
            previous: conversation.previous,
        })
    }
}

struct HistoryTools {
    inner: Arc<dyn ToolPort>,
    node: String,
    data_root: PathBuf,
    previous: Vec<GraphRunRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryArguments {
    #[serde(default)]
    run: Option<String>,
}

impl ToolPort for HistoryTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = self.inner.definitions();
        tools.push(ToolDefinition::new(ToolName::new(HISTORY_TOOL).expect("static name"),
            "Read this node's actual model/tool records from one prior Run in this conversation. Omit run for the nearest prior Run; follow previous_run to go back. Interrupted calls without observations have unknown outcomes; verify reality before continuing work.",
            json!({"type":"object", "properties":{"run":{"type":"string"}}, "additionalProperties":false})));
        tools
    }

    fn is_read_only(&self, name: &str) -> bool {
        name == HISTORY_TOOL || self.inner.is_read_only(name)
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if name != HISTORY_TOOL {
                return self.inner.call(name, arguments).await;
            }
            let args: HistoryArguments = serde_json::from_value(arguments).map_err(|error| {
                ToolError::Failed(format!("invalid history arguments: {error}"))
            })?;
            let index = match args.run {
                Some(run) => self
                    .previous
                    .iter()
                    .position(|record| record.run_id == run)
                    .ok_or_else(|| {
                        ToolError::Failed("Run is outside this node's conversation history".into())
                    })?,
                None if self.previous.is_empty() => {
                    return Ok(vec![ToolResultContent::json(
                        json!({"run":null, "previous_run":null, "invocations":[]}),
                    )]);
                }
                None => 0,
            };
            let record = &self.previous[index];
            let mut invocations = Vec::new();
            for invocation in 1..=record.invocations.get(&self.node).copied().unwrap_or(0) {
                let key = InvocationKey {
                    run_id: record.run_id.clone(),
                    graph_digest: record.graph_digest.clone(),
                    node_id: self.node.clone(),
                    invocation,
                };
                let messages = crate::goose_acp::trace_messages(&self.data_root, &key)
                    .map_err(ToolError::Failed)?;
                invocations.push(json!({"invocation":invocation, "messages":messages}));
            }
            Ok(vec![ToolResultContent::json(
                json!({"run":record.run_id, "status":record.status,
                "node":self.node, "invocations":invocations,
                "previous_run":self.previous.get(index + 1).map(|record| &record.run_id),
                "incomplete_outcomes":"A tool call without its result is unknown, not proof of success."}),
            )])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{ConversationSource, RunMetadata, metadata};
    use anchor_runtime::graph::GraphSnapshot;
    use anchor_sandbox_bwrap::BubblewrapPolicy;

    fn resolver(root: &Path) -> HostIoResolver {
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        HostIoResolver::new(
            HostArtifacts::new(root.join("artifacts"), work.clone()),
            Arc::new(
                BubblewrapSandbox::new(
                    BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(work),
                )
                .unwrap(),
            ),
            Arc::new(tool_host::PluginToolHost::new(Vec::<String>::new())),
            tool_host::McpToolConfig::default(),
            BTreeMap::new(),
            FileRunStore::new(root.join("runs")),
            LocalInputs::new(root.to_owned(), None).unwrap(),
        )
    }

    fn admit(
        resolver: &HostIoResolver,
        id: &str,
        session: &str,
        previous: Option<&str>,
    ) -> InvocationKey {
        let snapshot =
            GraphSnapshot::admit(json!({"objective":"conversation test", "entry":"agent", "agents":{"a":{"model":"fixture"}},
            "nodes":[{"id":"agent", "agent":"a"}], "edges":[]}))
            .unwrap();
        let mut run = GraphRunRecord::create_with_id(snapshot, json!({}), id).unwrap();
        run.status = RunStatus::Stopped;
        run.invocations.insert("agent".into(), 1);
        resolver.run_store.save(&run).unwrap();
        let root = resolver.local_inputs.state_root();
        let mut meta =
            RunMetadata::new(id.into(), "graph".into(), run.graph_digest.clone(), root).unwrap();
        meta.conversation = Some(ConversationSource {
            session: session.into(),
            reply_node: "agent".into(),
            previous_run: previous.map(str::to_owned),
        });
        metadata::save(root, &meta).unwrap();
        InvocationKey {
            run_id: id.into(),
            graph_digest: run.graph_digest,
            node_id: "agent".into(),
            invocation: 1,
        }
    }

    #[test]
    fn hints_isolate_sessions_nodes_and_ignore_untrusted_input() {
        let root = tempfile::tempdir().unwrap();
        let resolver = resolver(root.path());
        let first = admit(&resolver, "first", "alice", None);
        let second = admit(&resolver, "second", "alice", Some("first"));
        let other = admit(&resolver, "other", "bob", None);
        assert_eq!(
            resolver.conversation(&first).unwrap().unwrap().key,
            resolver.conversation(&second).unwrap().unwrap().key
        );
        assert_ne!(
            resolver.conversation(&first).unwrap().unwrap().key,
            resolver.conversation(&other).unwrap().unwrap().key
        );
        let another_node = InvocationKey {
            node_id: "another-node".into(),
            ..first.clone()
        };
        assert_ne!(
            resolver.conversation(&first).unwrap().unwrap().key,
            resolver.conversation(&another_node).unwrap().unwrap().key
        );
        let injected = admit(&resolver, "injected", "alice", Some("other"));
        assert!(resolver.conversation(&injected).is_err());
        let mut alias = metadata::load(root.path(), "second").unwrap().unwrap();
        alias.graph = "alias-of-graph".into();
        metadata::save(root.path(), &alias).unwrap();
        assert_eq!(
            resolver.conversation(&first).unwrap().unwrap().key,
            resolver.conversation(&second).unwrap().unwrap().key
        );
    }

    #[test]
    fn only_pending_session_calls_include_settled_successors_for_native_resume() {
        use crate::application::{
            metadata::GraphCallSource,
            session_calls::{SessionCall, SessionContext},
        };
        let root = tempfile::tempdir().unwrap();
        let resolver = resolver(root.path());
        let first = admit(&resolver, "first", "alice", None);
        let background = admit(&resolver, "background", "alice", Some("first"));
        let foreground = admit(&resolver, "foreground", "alice", Some("background"));
        assert_eq!(
            resolver.conversation_predecessors(&background).unwrap(),
            vec![first.clone()]
        );
        let mut metadata = metadata::load(root.path(), "background").unwrap().unwrap();
        metadata.graph_call = Some(GraphCallSource {
            parent_run: "parent".into(),
            parent_graph: "source".into(),
            parent_graph_digest: "digest".into(),
            node: "invoke".into(),
            invocation: 1,
            mode: "wait".into(),
            root_run: "parent".into(),
        });
        metadata.session_call = Some(SessionCall {
            context: SessionContext {
                session: "alice".into(),
                reply_node: "agent".into(),
                conversation_id: "conversation".into(),
                channel: json!({}),
            },
            status: "pending".into(),
            error: String::new(),
        });
        metadata::save(root.path(), &metadata).unwrap();
        assert_eq!(
            resolver.conversation_predecessors(&background).unwrap(),
            vec![foreground.clone(), first.clone()]
        );
        assert_eq!(
            resolver.conversation_predecessors(&first).unwrap(),
            Vec::<InvocationKey>::new()
        );
        let mut active = resolver.run_store.load("foreground").unwrap().unwrap();
        active.status = RunStatus::Running;
        resolver.run_store.save(&active).unwrap();
        assert!(
            resolver
                .conversation_predecessors(&background)
                .unwrap_err()
                .contains("not settled")
        );
        active.status = RunStatus::Completed;
        resolver.run_store.save(&active).unwrap();
        metadata.session_call.as_mut().unwrap().context.session = "bob".into();
        metadata::save(root.path(), &metadata).unwrap();
        assert!(
            resolver
                .conversation_predecessors(&background)
                .unwrap_err()
                .contains("identity changed")
        );
        metadata.session_call.as_mut().unwrap().context.session = "alice".into();
        metadata.session_call.as_mut().unwrap().status = "delivered".into();
        metadata::save(root.path(), &metadata).unwrap();
        assert_eq!(
            resolver.conversation_predecessors(&background).unwrap(),
            vec![first]
        );
    }

    #[test]
    fn only_a_deletion_from_the_same_conversation_explains_a_missing_predecessor() {
        let root = tempfile::tempdir().unwrap();
        let resolver = resolver(root.path());
        admit(&resolver, "first", "alice", None);
        let second = admit(&resolver, "second", "alice", Some("first"));
        std::fs::remove_file(root.path().join("run-metadata/first.json")).unwrap();
        std::fs::remove_file(root.path().join("runs/first.json")).unwrap();
        assert!(
            resolver
                .conversation(&second)
                .err()
                .unwrap()
                .contains("metadata is missing")
        );
        let graph = crate::application::RunApplication::graph_identity(
            &root.path().canonicalize().unwrap(),
        )
        .unwrap()
        .to_string_lossy()
        .into_owned();
        // A tombstone from another conversation explains nothing.
        crate::run_deletions::save(
            root.path(),
            &crate::run_deletions::RunDeletion::new("first", &graph, "bob", "agent", None),
        )
        .unwrap();
        assert!(
            resolver
                .conversation(&second)
                .err()
                .unwrap()
                .contains("metadata is missing")
        );
        std::fs::remove_file(root.path().join("run-deletions/first.json")).unwrap();
        crate::run_deletions::save(
            root.path(),
            &crate::run_deletions::RunDeletion::new("first", &graph, "alice", "agent", None),
        )
        .unwrap();
        assert!(
            resolver
                .conversation(&second)
                .unwrap()
                .unwrap()
                .previous
                .is_empty()
        );
    }

    /// A deleted middle link keeps its place: the walk carries on to the older
    /// Run that still exists instead of stopping at the tombstone.
    #[test]
    fn a_deleted_link_bridges_the_walk_to_the_surviving_predecessor() {
        let root = tempfile::tempdir().unwrap();
        let resolver = resolver(root.path());
        let first = admit(&resolver, "first", "alice", None);
        admit(&resolver, "second", "alice", Some("first"));
        let third = admit(&resolver, "third", "alice", Some("second"));
        let graph = crate::application::RunApplication::graph_identity(
            &root.path().canonicalize().unwrap(),
        )
        .unwrap()
        .to_string_lossy()
        .into_owned();
        crate::run_deletions::save(
            root.path(),
            &crate::run_deletions::RunDeletion::new(
                "second",
                &graph,
                "alice",
                "agent",
                Some("first"),
            ),
        )
        .unwrap();
        std::fs::remove_file(root.path().join("run-metadata/second.json")).unwrap();
        std::fs::remove_file(root.path().join("runs/second.json")).unwrap();
        let previous = resolver.conversation(&third).unwrap().unwrap().previous;
        assert_eq!(
            previous
                .iter()
                .map(|record| record.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first"]
        );
        // The deleted link's own records are gone; the older one it bridged to
        // is still offered.
        assert_eq!(
            resolver.conversation_predecessors(&third).unwrap(),
            vec![first]
        );
    }

    #[test]
    fn previous_uses_nearest_available_unfinished_node_once() {
        let root = tempfile::tempdir().unwrap();
        let resolver = resolver(root.path());
        let prior = admit(&resolver, "prior", "alice", None);
        admit(&resolver, "empty", "alice", Some("prior"));
        let current = admit(&resolver, "current", "alice", Some("empty"));
        let workspace = resolver.artifacts.workspace_path(&prior).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("draft.txt"), "unfinished").unwrap();
        let conversation = resolver.conversation(&current).unwrap().unwrap();
        let mount = resolver
            .previous_mount(&current, &conversation)
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(mount.source.join("draft.txt")).unwrap(),
            "unfinished"
        );
        assert!(
            resolver
                .previous_mount(
                    &InvocationKey {
                        invocation: 2,
                        ..current
                    },
                    &conversation
                )
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn interruption_control_artifact_does_not_replace_the_predecessor_draft() {
        use anchor_runtime::graph::{ArtifactFreezeContext, ArtifactKind, ArtifactPort, RunResult};
        let root = tempfile::tempdir().unwrap();
        let resolver = resolver(root.path());
        let prior = admit(&resolver, "prior", "alice", None);
        resolver
            .artifacts
            .bind_node_workspace(&prior.run_id, &prior.graph_digest, &prior.node_id)
            .unwrap();
        let workspace = resolver.artifacts.prepare_workspace(&prior, &[]).unwrap();
        std::fs::write(workspace.join("draft.txt"), "unfinished draft retained").unwrap();
        resolver
            .artifacts
            .retain_interrupted_workspace(&prior)
            .unwrap();
        let completion = NodeCompletion {
            submission: "input superseded".into(),
            route: None,
            model_requests: 0,
            output: json!({"interrupted":true,"reason":"input superseded"}),
        };
        let commit = resolver
            .artifacts
            .freeze_with_context(
                &prior,
                &completion,
                &ArtifactFreezeContext {
                    kind: ArtifactKind::Interruption,
                    input_commits: Vec::new(),
                },
            )
            .await
            .unwrap();
        let mut previous = resolver.run_store.load(&prior.run_id).unwrap().unwrap();
        previous.sequence = 1;
        previous.results.insert(
            "agent".into(),
            vec![RunResult {
                sequence: 1,
                node_id: "agent".into(),
                key: prior.clone(),
                completion,
                commit,
                interruption: Some("input superseded".into()),
            }],
        );
        resolver.run_store.save(&previous).unwrap();
        let current = admit(&resolver, "current", "alice", Some("prior"));
        let conversation = resolver.conversation(&current).unwrap().unwrap();
        let mount = resolver
            .previous_mount(&current, &conversation)
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read(mount.source.join("draft.txt")).unwrap(),
            b"unfinished draft retained"
        );
        assert!(!mount.source.join("interruption.json").exists());
        assert_eq!(
            std::fs::read(workspace.join("draft.txt")).unwrap(),
            b"unfinished draft retained"
        );
    }

    struct EmptyTools;
    impl ToolPort for EmptyTools {
        fn definitions(&self) -> Vec<ToolDefinition> {
            Vec::new()
        }
        fn call<'a>(
            &'a self,
            name: &'a str,
            _: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>>
        {
            Box::pin(async move { Err(ToolError::Unknown(name.into())) })
        }
    }

    #[tokio::test]
    async fn history_cannot_select_another_session_or_node() {
        let root = tempfile::tempdir().unwrap();
        let resolver = resolver(root.path());
        admit(&resolver, "prior", "alice", None);
        let current = admit(&resolver, "current", "alice", Some("prior"));
        admit(&resolver, "foreign", "bob", None);
        let port = resolver.conversation_tools(
            Arc::new(EmptyTools),
            &current,
            resolver.conversation(&current).unwrap().unwrap(),
        );
        assert!(port.is_read_only(HISTORY_TOOL));
        assert!(
            port.call(HISTORY_TOOL, json!({"run":"foreign"}))
                .await
                .is_err()
        );
        assert!(
            port.call(HISTORY_TOOL, json!({"node":"other"}))
                .await
                .is_err()
        );
        let result = port.call(HISTORY_TOOL, json!({})).await.unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("prior"));
        assert!(text.contains("unknown"));
        assert!(!text.contains("foreign"));
    }
}
