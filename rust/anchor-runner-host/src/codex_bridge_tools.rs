//! The operator grants one local service, not a host shell or a general supervisor.
use crate::{
    codex_bridge_service::CodexBridgeService,
    plugin_disclosure::{PluginDisclosureCatalog, ToolOwner},
};
use anchor_runtime::{
    Cancellation, ToolDefinition, ToolError, ToolName, ToolPort, ToolResultContent,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
};

const STATUS: &str = "codex_bridge_status";
const START: &str = "codex_bridge_start";

pub(crate) struct BridgeGrant {
    graphs: BTreeSet<String>,
    pub(crate) service: CodexBridgeService,
}

impl BridgeGrant {
    pub(crate) fn allows_graph(&self, graph: &str) -> bool {
        self.graphs.contains(graph)
    }

    pub(crate) fn new(graphs: Vec<String>, runtime: PathBuf) -> Result<Self, String> {
        if graphs.is_empty()
            || graphs
                .iter()
                .any(|id| !anchor_runtime::identifier::is_name(id))
        {
            return Err("Bridge recovery requires explicit valid Manager Graph ids".into());
        }
        Ok(Self {
            graphs: graphs.into_iter().collect(),
            service: CodexBridgeService::from_runtime_dir(runtime)?,
        })
    }
}

pub(crate) type BridgeAuthorization = Arc<dyn Fn() -> Option<Arc<BridgeGrant>> + Send + Sync>;

pub(crate) fn wrap(
    inner: Arc<dyn ToolPort>,
    authorize: BridgeAuthorization,
    cancellation: Cancellation,
    disclosure: &mut PluginDisclosureCatalog,
) -> Arc<dyn ToolPort> {
    if authorize().is_none() {
        return inner;
    }
    for definition in definitions() {
        disclosure.owners.insert(
            definition.name.clone(),
            ToolOwner {
                plugin: "codex".into(),
                server: "host".into(),
                tool: definition.name,
            },
        );
    }
    Arc::new(BridgeTools {
        inner,
        authorize,
        cancellation,
    })
}

struct BridgeTools {
    inner: Arc<dyn ToolPort>,
    authorize: BridgeAuthorization,
    cancellation: Cancellation,
}

fn definitions() -> Vec<ToolDefinition> {
    [(STATUS, "Read the fixed local Codex Bridge user-service state. This is not Codex task status or proof the MCP tools are connected. No arguments, credentials or arbitrary service access."),
     (START, "Idempotently start only the operator-granted local Codex Bridge service. Never stops/restarts a running service or resumes/replays coding, approvals or messages. No arguments. If Codex tools were unavailable at this turn's preparation, the next user turn reconnects; do not claim coding resumed.")]
        .into_iter().map(|(name, description)| ToolDefinition::new(
            ToolName::new(name).expect("fixed tool name"), description,
            json!({"type":"object","properties":{},"additionalProperties":false}),
        )).collect()
}

impl ToolPort for BridgeTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut all = self.inner.definitions();
        all.extend(definitions());
        all
    }
    fn is_read_only(&self, name: &str) -> bool {
        match name {
            STATUS => true,
            START => false,
            _ => self.inner.is_read_only(name),
        }
    }
    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if !matches!(name, STATUS | START) {
                return self.inner.call(name, arguments).await;
            }
            if !arguments.as_object().is_some_and(|a| a.is_empty()) {
                return Err(ToolError::Failed(
                    "Bridge service tools accept no arguments".into(),
                ));
            }
            if self.cancellation.load(Ordering::Acquire) {
                return Err(ToolError::Failed("this invocation was cancelled".into()));
            }
            let grant = (self.authorize)().ok_or_else(|| {
                ToolError::Failed("this active Manager has no Bridge service grant".into())
            })?;
            let value = if name == START {
                grant.service.start().await
            } else {
                grant.service.status().await
            }
            .map_err(ToolError::Failed)?;
            Ok(vec![ToolResultContent::json(json!({
                "service":value, "codex_work_resumed":false,
                "codex_tools_connected_this_turn":self.inner.definitions().iter().any(|d| d.name == "codex-codex_projects"),
                "guidance":"Service state is not task completion. Reconcile original sessions and receipts; never replay uncertain effects."
            }))])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::RunApplication;
    use anchor_platform_session::{
        ChannelIdentity, ChannelInboundRequest, SessionStore, TurnStatus,
    };
    use anchor_runtime::graph::{InvocationKey, PluginBinding};

    #[test]
    fn only_current_granted_manager_with_frozen_codex_and_network_is_authorized() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        std::fs::create_dir_all(root.join("platform")).unwrap();
        let sessions = SessionStore::open(root.join("platform/sessions.sqlite")).unwrap();
        let admitted = sessions
            .admit_channel_inbound(
                "local",
                ChannelInboundRequest {
                    inbound_id: "initial".into(),
                    identity: ChannelIdentity {
                        source: "feishu".into(),
                        account: Some("feishu:fixture".into()),
                        conversation_kind: Default::default(),
                        conversation_id: "oc_private".into(),
                        sender_id: "ou_alice".into(),
                    },
                    graph: "manager".into(),
                    reply_node: "work".into(),
                    text: Some("status".into()),
                    attachments: Default::default(),
                    run_id: None,
                    replace_running: true,
                    bot_origin: None,
                    platform_message_id: None,
                },
            )
            .unwrap();
        let session = admitted.session.id;
        sessions
            .bind_channel_assistant("local", &session, "run", "wait", "work", "reply")
            .unwrap();
        let input = sessions
            .claim_assistant_input("local", &session, "run", "initial")
            .unwrap()
            .unwrap();
        let key = InvocationKey {
            run_id: "run".into(),
            graph_digest: "digest".into(),
            node_id: "work".into(),
            invocation: 2,
        };
        let mut metadata = crate::run_metadata::RunMetadata::new(
            "run".into(),
            "manager".into(),
            "digest".into(),
            root,
        )
        .unwrap();
        metadata.assistant = Some(crate::run_metadata::AssistantSource {
            owner: "local".into(),
            session: session.clone(),
            wait_node: "wait".into(),
            work_node: "work".into(),
            reply_node: "reply".into(),
        });
        crate::run_metadata::save(root, &metadata).unwrap();
        crate::assistant::bind(root, &key, "initial".into(), &input, vec![], false, None).unwrap();
        let turn = crate::assistant::binding(root, &key).unwrap().unwrap().turn;
        let binding = PluginBinding {
            id: "codex".into(),
            digest: "codex-digest".into(),
            resources: vec![],
            mcp_servers: vec!["codex".into()],
            authorization_digest: "codex-grant".try_into().unwrap(),
        };
        let mut policy = crate::assistant_plugins::TurnPluginPolicy {
            format: 1,
            run: "run".into(),
            node: "work".into(),
            graph_digest: "digest".into(),
            mounts: crate::plugin_mounts::MountSnapshot::new(&session, &turn, &Default::default()),
            declarations: vec![],
            bindings: std::collections::BTreeMap::from([("codex".into(), binding.clone())]),
            network: true,
            reasons: vec![],
            by_declaration: vec![],
            by_switch: vec![],
            task_grants: None,
        };
        let path = crate::assistant_plugins::turn_path(root, "run", &session, &turn);
        crate::assistant::save_immutable(&path, &policy).unwrap();
        let mut application = RunApplication::new(root.into(), root.into());
        let bindings = [binding];
        assert!(
            application
                .authorized_codex_bridge(&key, &bindings)
                .is_none()
        );
        application.codex_bridge = Some(Arc::new(
            BridgeGrant::new(vec!["another-manager".into()], root.into()).unwrap(),
        ));
        assert!(
            application
                .authorized_codex_bridge(&key, &bindings)
                .is_none()
        );
        application.codex_bridge = Some(Arc::new(
            BridgeGrant::new(vec!["manager".into()], root.into()).unwrap(),
        ));
        assert!(
            application
                .authorized_codex_bridge(&key, &bindings)
                .is_some()
        );
        assert!(application.authorized_codex_bridge(&key, &[]).is_none());
        let mut different = bindings.clone();
        different[0].digest = "other".into();
        assert!(
            application
                .authorized_codex_bridge(&key, &different)
                .is_none()
        );
        policy.network = false;
        std::fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
        assert!(
            application
                .authorized_codex_bridge(&key, &bindings)
                .is_none()
        );
        policy.network = true;
        std::fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
        let mut wrong_node = key.clone();
        wrong_node.node_id = "wait".into();
        assert!(
            application
                .authorized_codex_bridge(&wrong_node, &bindings)
                .is_none()
        );
        sessions
            .finish_turn("local", &session, &turn, TurnStatus::Stopped, None)
            .unwrap();
        assert!(
            application
                .authorized_codex_bridge(&key, &bindings)
                .is_none()
        );
        sessions
            .retire_channel_assistant("local", &session, "run")
            .unwrap();
        assert!(
            application
                .authorized_codex_bridge(&key, &bindings)
                .is_none()
        );
    }
    #[test]
    fn service_grants_require_explicit_names_and_tools_accept_no_targets() {
        for graphs in [vec![], vec!["*".into()], vec!["../other".into()]] {
            assert!(BridgeGrant::new(graphs, "/run/user/1001".into()).is_err());
        }
        for tool in definitions() {
            assert_eq!(tool.parameters["additionalProperties"], false);
            assert_eq!(tool.parameters["properties"], json!({}));
            assert!(!tool.name.contains("restart"));
        }
    }

    #[tokio::test]
    async fn injected_authority_is_rechecked_before_any_service_operation() {
        let root = tempfile::tempdir().unwrap();
        let grant = Arc::new(BridgeGrant::new(vec!["manager".into()], root.path().into()).unwrap());
        let allowed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let current = allowed.clone();
        let tools = wrap(
            Arc::new(crate::project_tools::NoTools),
            Arc::new(move || current.load(Ordering::Acquire).then(|| grant.clone())),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            &mut PluginDisclosureCatalog::default(),
        );
        assert_eq!(tools.definitions().len(), 2);
        assert!(tools.is_read_only(STATUS));
        assert!(!tools.is_read_only(START));
        allowed.store(false, Ordering::Release);
        for name in [STATUS, START] {
            let error = tools.call(name, json!({})).await.unwrap_err().to_string();
            assert!(error.contains("no Bridge service grant"), "{error}");
        }
    }
}
