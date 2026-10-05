//! Trusted cross-Run inputs. Framework Session owns dialogue; this adapter
//! exposes only same-node predecessor files and the existing native trace.
use super::*;
use anchor_runtime_rig::{ReadOnlyInput, ToolError, ToolPort, graph::RunStatus};
use rig_agent::core::{
    completion::ToolDefinition,
    message::{ToolName, ToolResultContent},
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, future::Future, sync::Arc};

const HISTORY_TOOL: &str = "anchor_conversation_history";

pub(super) struct Conversation {
    pub key: String,
    pub reply_node: String,
    previous: Vec<GraphRunRecord>,
}

impl HostIoResolver {
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
        let mut next = binding.previous_run.clone();
        let mut seen = BTreeSet::from([key.run_id.clone()]);
        let mut previous = Vec::new();
        while let Some(id) = next {
            if !seen.insert(id.clone()) {
                return Err("conversation lineage contains a cycle".into());
            }
            let metadata = crate::application::metadata::load(root, &id)
                .map_err(|error| format!("conversation predecessor: {error:?}"))?
                .ok_or("conversation predecessor metadata is missing")?;
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
                .and_then(|results| results.iter().find(|result| result.key == source))
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
            io_store: self.local_inputs.state_root().join("io-harness/store"),
            previous: conversation.previous,
        })
    }
}

struct HistoryTools {
    inner: Arc<dyn ToolPort>,
    node: String,
    io_store: PathBuf,
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
                let messages =
                    anchor_io_harness_runtime::node_port::trace_messages(&self.io_store, &key)
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
    use anchor_runtime_rig::graph::GraphSnapshot;
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
