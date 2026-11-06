//! Graph publication and the boundary between operator grants and agent drafts.
use super::*;
use anchor_platform_session::SessionStore;

#[derive(Clone, Copy, PartialEq, Eq)]
enum UpdateOrigin {
    Operator,
    Agent,
}

pub(super) fn activation(snapshot: &GraphSnapshot) -> Result<Value, GraphManagementError> {
    let persistent = snapshot.nodes.iter().any(|node| {
        node.agent.is_some()
            && crate::assistant::AssistantPlan::from_snapshot(snapshot, &node.id)
                .is_ok_and(|plan| plan.is_some())
    });
    Ok(json!({
        "mode": if persistent { "next_turn" } else { "new_run" },
        "graph_digest": snapshot.digest().map_err(invalid)?
    }))
}

impl RunApplication {
    pub(crate) fn assistant_plugin_update(&self, run: &str) -> Result<Value, GraphManagementError> {
        let metadata = self
            .metadata(run)?
            .ok_or_else(|| GraphManagementError::Missing("助手运行记录不可用".into()))?;
        let _lease =
            crate::assistant_plugins::graph_read_lease(&self.data_root, &metadata.bundle_source)
                .map_err(ApplicationError::from)?;
        let record = crate::application::run_access::load(&self.data_root, run)
            .map_err(invalid)?
            .ok_or_else(|| GraphManagementError::Missing("助手运行记录不可用".into()))?;
        let (_, bundle) = self.load_graph(&metadata.graph)?;
        serde_json::to_value(
            crate::assistant_plugins::status(&self.data_root, &record, &metadata, &bundle.snapshot)
                .map_err(invalid)?,
        )
        .map_err(storage)
    }

    pub(crate) async fn update_graph(
        &self,
        name: &str,
        definition: Value,
    ) -> Result<Value, GraphManagementError> {
        self.publish_graph(name, definition, UpdateOrigin::Operator)
            .await
    }

    pub(crate) async fn update_graph_from_agent(
        &self,
        name: &str,
        definition: Value,
    ) -> Result<Value, GraphManagementError> {
        self.publish_graph(name, definition, UpdateOrigin::Agent)
            .await
    }

    async fn publish_graph(
        &self,
        name: &str,
        definition: Value,
        origin: UpdateOrigin,
    ) -> Result<Value, GraphManagementError> {
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        let path = self.graph_path_checked(name)?;
        let _graph_lease = self.graph_admission_lease(&path)?;
        let (path, previous) = self.load_graph(name)?;
        let next = self.validate_graph(&definition)?;
        self.check_channel_graph_update(name, &next)?;
        let effect = activation(&next)?;
        let mut sessions = None;
        for (run_id, record) in self.runs_for_graph(
            &crate::run_metadata::graph_identity(&path).map_err(ApplicationError::from)?,
        )? {
            let metadata = self.metadata(&run_id)?;
            if let Some(source) = metadata
                .as_ref()
                .and_then(|metadata| metadata.assistant.as_ref())
            {
                let store = match &sessions {
                    Some(store) => store,
                    None => sessions.insert(
                        SessionStore::open(self.data_root.join("platform/sessions.sqlite"))
                            .map_err(storage)?,
                    ),
                };
                if store
                    .get_channel_assistant(&source.owner, &source.session)
                    .map_err(storage)?
                    .is_some_and(|binding| binding.run_id == run_id)
                {
                    if !crate::assistant_plugins::compatible(
                        record.progress().snapshot,
                        &next,
                        &source.work_node,
                    ) {
                        return Err(GraphManagementError::Conflict(
                            "工作流结构或指令已改变。请先停止并退役此工作流的现有助手，再保存新版本；插件选择与联网设置可以直接保存。".into(),
                        ));
                    }
                    if origin == UpdateOrigin::Agent && previous.snapshot != next {
                        return Err(GraphManagementError::Conflict(
                            "运行中助手的插件权限需要用户在工作流页面保存；Agent 不能通过修改 Graph 为现有会话授予权限。".into(),
                        ));
                    }
                    // The frozen Turn owns any outstanding delivery. Publishing
                    // the next Turn's policy does not alter or replay that work.
                    continue;
                }
            }
            self.reject_pending_session_delivery(&run_id)?;
        }
        if origin == UpdateOrigin::Operator
            && (effect["mode"] == "next_turn"
                || next
                    .nodes
                    .iter()
                    .flat_map(|node| &node.plugins)
                    .any(|plugin| plugin.graph_tasks.is_some()))
        {
            // Record approval before publishing the Graph. The receipt has no
            // effect until the authoritative Graph matches its exact digest.
            self.publish_graph_task_permissions(&path, &next)
                .map_err(GraphManagementError::from)?;
        }
        write_graph_bundle_from_catalog(&path, &definition, self.library_root())?;
        Ok(json!({"graph":name,"definition":definition,"plugin_activation":effect}))
    }
}
