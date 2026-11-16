//! Host-owned channel bindings: one bot per persistent assistant Graph, keyed
//! by the Graph name. The existing Session/Turn admission and ChannelSupervisor
//! retain execution and transport semantics.
use super::{ApplicationError, RunApplication};
use crate::{channel_platform::ChannelPlatform, plugin_library::Catalog};
use anchor_runtime::graph::GraphSnapshot;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// The operator-approved settings of one Graph's bot. The Graph is the
/// binding's key; the reply node is derived from the Graph, never configured.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChannelBinding {
    pub(crate) plugin: String,
    pub(crate) users: Vec<String>,
    pub(crate) send_users: Vec<String>,
    /// Explicit Feishu chat IDs; an empty list never authorizes any group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) groups: Vec<String>,
    /// Explicit peer bot open IDs, independent of human member grants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) peer_bots: Vec<String>,
    /// Answer only one-to-one conversations, without shared group context.
    #[serde(default)]
    pub(crate) private_only: bool,
    /// Explicitly grant this bot the published application API capabilities.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) cli_enabled: bool,
}

/// The member lists an operator chooses; the Plugin comes from the Graph.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChannelMembers {
    pub(crate) users: Vec<String>,
    pub(crate) send_users: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) groups: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) peer_bots: Vec<String>,
    #[serde(default)]
    pub(crate) private_only: bool,
    /// Explicitly grant this bot the published application API capabilities.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) cli_enabled: bool,
}

impl ChannelBinding {
    pub(crate) fn validate_for_platform(
        &self,
        platform: ChannelPlatform,
    ) -> Result<(), ApplicationError> {
        self.validate()?;
        if platform == ChannelPlatform::Wecom
            && (!self.groups.is_empty() || !self.peer_bots.is_empty() || self.cli_enabled)
        {
            return Err(ApplicationError::Invalid(
                "group authorization and application CLI tools are supported only for Feishu"
                    .into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn allows_inbound(&self, userid: &str) -> bool {
        Self::allows_user(&self.users, userid)
    }

    pub(crate) fn allows_outbound(&self, userid: &str) -> bool {
        Self::allows_user(&self.send_users, userid)
    }

    fn allows_user(users: &[String], userid: &str) -> bool {
        userid != "@all" && users.iter().any(|user| user == "*" || user == userid)
    }

    pub(crate) fn validate(&self) -> Result<(), ApplicationError> {
        if !anchor_runtime::identifier::is_plugin_id(&self.plugin) {
            return Err(ApplicationError::Invalid("invalid channel binding".into()));
        }
        for users in [&self.users, &self.send_users] {
            if users.len() > 1000
                || users.iter().any(|user| {
                    user.trim().is_empty()
                        || user.len() > 200
                        || user.contains(',')
                        || user.chars().any(char::is_control)
                })
            {
                return Err(ApplicationError::Invalid(
                    "invalid channel user authorization".into(),
                ));
            }
        }
        // GatewayConfig's empty outbound list inherits inbound users. Make that
        // existing behavior explicit in the approved binding rather than save
        // an empty list that misleadingly appears to prohibit outbound replies.
        if self.send_users.is_empty() && !self.users.is_empty() {
            return Err(ApplicationError::Invalid(
                "send_users must explicitly authorize the intended reply users".into(),
            ));
        }
        if self.groups.len() > 1000
            || self.groups.iter().any(|group| {
                let group = group.trim();
                !group.starts_with("oc_")
                    || group.len() <= 3
                    || group.len() > 200
                    || !group
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
        {
            return Err(ApplicationError::Invalid(
                "groups must contain at most 1000 explicit Feishu oc_ chat IDs".into(),
            ));
        }
        if self.peer_bots.len() > 1000
            || self.peer_bots.iter().any(|bot| {
                !bot.starts_with("ou_")
                    || bot.len() <= 3
                    || bot.len() > 200
                    || !bot
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
            || (!self.peer_bots.is_empty() && self.groups.is_empty())
        {
            return Err(ApplicationError::Invalid(
                "peer_bots requires explicit Feishu ou_ bot IDs and authorized groups".into(),
            ));
        }
        if self.private_only && !self.groups.is_empty() {
            return Err(ApplicationError::Invalid(
                "private_only requires an empty groups list".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn normalized(mut self) -> Self {
        self.users = self
            .users
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self.send_users = self
            .send_users
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self.groups = self
            .groups
            .into_iter()
            .map(|group| group.trim().to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self.peer_bots = self
            .peer_bots
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self
    }
}

/// How a Graph serves as a bot: its assistant work node and the one channel
/// Plugin mounted on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelPlan {
    pub(crate) work_node: String,
    pub(crate) plugin: String,
    pub(crate) platform: ChannelPlatform,
}

pub(crate) struct ActiveChannelTurn {
    pub(crate) source: crate::run_metadata::AssistantSource,
    pub(crate) input: anchor_platform_session::AssistantInput,
    pub(crate) identity: anchor_platform_session::ChannelIdentity,
}

/// The single rule for "this Graph is a bot Graph": a persistent assistant
/// whose Agent node mounts a channel Plugin. `None` for any other Graph, which
/// may mount the same Plugin for application tools and explicitly addressed
/// notifications (R1). More than one channel Plugin on the assistant is
/// refused (R2).
pub(crate) fn channel_plan(
    snapshot: &GraphSnapshot,
    library: &Path,
) -> Result<Option<ChannelPlan>, ApplicationError> {
    let Ok(Some(plan)) = crate::assistant::AssistantPlan::discover(snapshot) else {
        return Ok(None);
    };
    let catalog = Catalog::new(library);
    let node = snapshot
        .nodes
        .iter()
        .find(|node| node.id == plan.work_node)
        .ok_or(ApplicationError::Missing)?;
    let mut channels = node.plugins.iter().flat_map(|reference| {
        catalog
            .channels(&reference.id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(move |channel| {
                ChannelPlatform::parse(&channel.platform).map(|platform| (reference, platform))
            })
    });
    match (channels.next(), channels.next()) {
        (None, _) => Ok(None),
        (Some((plugin, platform)), None) => Ok(Some(ChannelPlan {
            work_node: plan.work_node,
            plugin: plugin.id.clone(),
            platform,
        })),
        (Some(_), Some(_)) => Err(ApplicationError::Invalid(TWO_CHANNELS.into())),
    }
}

/// Internal routing data. Credentials and callback tokens are deliberately not
/// part of this public projection; the adapter retains them privately.
#[derive(Clone)]
pub(crate) struct ChannelRuntimeState {
    pub(crate) graph: String,
    pub(crate) owner: String,
    pub(crate) platform: ChannelPlatform,
    pub(crate) binding: ChannelBinding,
    /// The Graph's assistant node while the Graph still admits as this bot.
    pub(crate) reply_node: Option<String>,
    /// Effective: saved enabled and still able to run (Graph admits, credentials
    /// complete and of the saved account). Inbound routing reads this.
    pub(crate) enabled: bool,
    /// The saved intent. Changes that a running gateway depends on (deleting
    /// the Graph, dropping its channel, credentials) are refused while it is
    /// set, whether or not the gateway currently manages to run.
    pub(crate) saved_enabled: bool,
    pub(crate) account: Option<String>,
    pub(crate) descriptor: Option<PathBuf>,
}

/// The binding a gateway callback key belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelCallback {
    pub(crate) graph: String,
    pub(crate) owner: String,
}

pub(crate) trait ChannelManagementPort: Send + Sync {
    /// Every binding the owner can see, with its live gateway status.
    fn list<'a>(&'a self, owner: &'a str) -> BoxFuture<'a, Result<Value, ApplicationError>>;
    fn status<'a>(
        &'a self,
        owner: &'a str,
        graph: &'a str,
    ) -> BoxFuture<'a, Result<Value, ApplicationError>>;
    fn configure<'a>(
        &'a self,
        owner: &'a str,
        graph: &'a str,
        binding: ChannelBinding,
    ) -> BoxFuture<'a, Result<Value, ApplicationError>>;
    fn set_enabled<'a>(
        &'a self,
        owner: &'a str,
        graph: &'a str,
        enabled: bool,
    ) -> BoxFuture<'a, Result<Value, ApplicationError>>;
    fn snapshot(&self, graph: &str) -> Result<Option<ChannelRuntimeState>, ApplicationError>;
    fn snapshots(&self) -> Result<Vec<ChannelRuntimeState>, ApplicationError>;
    fn callback(&self, key: &str) -> Option<ChannelCallback>;
    /// Remove a stopped binding with its bot credentials and gateway state.
    fn remove<'a>(&'a self, graph: &'a str) -> BoxFuture<'a, Result<(), ApplicationError>>;
}

pub(crate) const NOT_A_BOT: &str = "a bot Graph must be a persistent assistant (a session.wait_input, Agent, session.reply loop) whose Agent node mounts the channel Plugin";
pub(crate) const TWO_CHANNELS: &str =
    "a persistent assistant can mount at most one channel Plugin on its Agent node";
const STOP_FIRST: &str = "此 Graph 的机器人正在运行，请先停用机器人";

impl RunApplication {
    /// Tool calls use the current claimed Turn, including group sender identity;
    /// model arguments and a retained Run alone cannot select a conversation.
    pub(crate) fn active_channel_turn(
        &self,
        key: &anchor_runtime::graph::InvocationKey,
    ) -> Result<ActiveChannelTurn, ApplicationError> {
        use anchor_platform_session::{AssistantInput, ChannelConversationKind, TurnStatus};
        let invalid = || {
            ApplicationError::Invalid(
                "this input no longer belongs to an active assistant Turn".into(),
            )
        };
        let metadata = self.metadata(&key.run_id)?.ok_or_else(invalid)?;
        let source = metadata
            .assistant
            .as_ref()
            .filter(|source| source.work_node == key.node_id)
            .ok_or_else(invalid)?
            .clone();
        self.ensure_current_assistant(&metadata)?;
        let input = crate::assistant::input(&self.data_root, key)
            .map_err(super::storage)?
            .ok_or_else(invalid)?;
        let run = match &input {
            AssistantInput::User(input) => input.relation.run_id.as_deref(),
            AssistantInput::Task(input) => Some(input.run_id.as_str()),
        };
        if run != Some(key.run_id.as_str()) || input.turn().status != TurnStatus::Running {
            return Err(invalid());
        }
        let sessions = crate::assistant::sessions(&self.data_root).map_err(super::storage)?;
        let identity = sessions
            .channel_identity(&source.owner, &source.session)
            .map_err(super::assistants::assistant_session_error)?;
        if ChannelPlatform::parse(&identity.source).is_none() || identity.conversation_id.is_empty()
        {
            return Err(invalid());
        }
        if let AssistantInput::User(user) = &input {
            let actual = &user.request.identity;
            if actual.source != identity.source
                || actual.account != identity.account
                || actual.conversation_id != identity.conversation_id
                || actual.conversation_kind != identity.conversation_kind
                || (identity.conversation_kind == ChannelConversationKind::Single
                    && actual.sender_id != identity.sender_id)
            {
                return Err(invalid());
            }
        }
        Ok(ActiveChannelTurn {
            source,
            input,
            identity,
        })
    }

    pub(crate) fn ensure_channel_session_eligible(
        &self,
        owner: &str,
        session: &str,
    ) -> Result<(), ApplicationError> {
        let sessions = anchor_platform_session::SessionStore::open(
            self.data_root.join("platform/sessions.sqlite"),
        )
        .map_err(super::assistants::assistant_session_error)?;
        if !sessions
            .channel_session_is_selected(owner, session)
            .map_err(super::assistants::assistant_session_error)?
        {
            return Err(ApplicationError::Conflict(
                "this channel Session is superseded by its canonical account Session".into(),
            ));
        }
        let Some(port) = self.channels.as_deref() else {
            return Ok(());
        };
        let saved = sessions
            .get(owner, session)
            .map_err(super::assistants::assistant_session_error)?;
        let source = saved.channel.get("source").ok_or_else(|| {
            ApplicationError::Conflict("channel Session has no platform identity".into())
        })?;
        let current = port
            .snapshot(&saved.graph)?
            .filter(|current| source == current.platform.as_str() && current.enabled)
            .ok_or_else(|| {
                ApplicationError::Conflict("channel Session's Graph has no enabled bot".into())
            })?;
        let canonical = |account: &str| {
            sessions
                .canonical_channel_account(owner, source, account)
                .map_err(super::assistants::assistant_session_error)
        };
        let saved_account = canonical(saved.channel.get("account").map_or("", String::as_str))?;
        let current_account = canonical(current.account.as_deref().unwrap_or(""))?;
        if current.owner != owner
            || current.reply_node.as_deref() != Some(saved.reply_node.as_str())
            || saved_account != current_account
        {
            return Err(ApplicationError::Conflict(
                "channel Session does not match its Graph's enabled bot".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn channel_management(
        &self,
    ) -> Result<&dyn ChannelManagementPort, ApplicationError> {
        self.channels.as_deref().ok_or_else(|| {
            ApplicationError::Invalid("this Host does not provide channel management".into())
        })
    }

    pub(crate) async fn channel_list(&self, owner: &str) -> Result<Value, ApplicationError> {
        self.channel_management()?.list(owner).await
    }

    pub(crate) async fn channel_status(
        &self,
        owner: &str,
        graph: &str,
    ) -> Result<Value, ApplicationError> {
        self.channel_management()?.status(owner, graph).await
    }

    pub(crate) async fn configure_channel(
        &self,
        owner: &str,
        graph: &str,
        members: ChannelMembers,
    ) -> Result<Value, ApplicationError> {
        let port = self.channel_management()?;
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        let _library_lease = self.plugin_reference_lease().await?;
        let plan = self.channel_bot_plan(graph)?;
        let binding = ChannelBinding {
            plugin: plan.plugin,
            users: members.users,
            send_users: members.send_users,
            groups: members.groups,
            peer_bots: members.peer_bots,
            private_only: members.private_only,
            cli_enabled: members.cli_enabled,
        };
        binding.validate_for_platform(plan.platform)?;
        port.configure(owner, graph, binding.normalized()).await
    }

    #[cfg(test)]
    pub(crate) async fn set_channel_enabled(
        &self,
        owner: &str,
        graph: &str,
        enabled: bool,
    ) -> Result<Value, ApplicationError> {
        self.set_channel_enabled_inner(owner, graph, enabled, None)
            .await
    }

    pub(crate) async fn set_channel_enabled_checked(
        &self,
        owner: &str,
        graph: &str,
        enabled: bool,
        expected: &ChannelBinding,
    ) -> Result<Value, ApplicationError> {
        self.set_channel_enabled_inner(owner, graph, enabled, Some(expected))
            .await
    }

    async fn set_channel_enabled_inner(
        &self,
        owner: &str,
        graph: &str,
        enabled: bool,
        expected: Option<&ChannelBinding>,
    ) -> Result<Value, ApplicationError> {
        let port = self.channel_management()?;
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        let _library_lease = self.plugin_reference_lease().await?;
        let state = port.snapshot(graph)?.ok_or(ApplicationError::Missing)?;
        if expected.is_some_and(|expected| state.owner != owner || &state.binding != expected) {
            return Err(ApplicationError::Conflict(
                "channel binding changed while confirmation was pending; request a fresh form"
                    .into(),
            ));
        }
        if enabled {
            let plan = self.channel_bot_plan(graph)?;
            if plan.plugin != state.binding.plugin || plan.platform != state.platform {
                return Err(ApplicationError::Conflict(
                    "the Graph mounts a different channel Plugin; configure the bot again".into(),
                ));
            }
        }
        // Disabling remains possible after a Graph or Plugin has disappeared.
        port.set_enabled(owner, graph, enabled).await
    }

    /// The saved Graph as a bot, or the reason it cannot be one.
    pub(crate) fn channel_bot_plan(&self, graph: &str) -> Result<ChannelPlan, ApplicationError> {
        let (_, bundle) = self
            .load_graph(graph)
            .map_err(|_| ApplicationError::Missing)?;
        channel_plan(&bundle.snapshot, self.library_root())?
            .ok_or_else(|| ApplicationError::Invalid(NOT_A_BOT.into()))
    }

    /// Saving a Graph must not silently take away a running bot's channel.
    /// The caller holds the catalog guard, which also serializes enabling.
    pub(crate) fn check_channel_graph_update(
        &self,
        graph: &str,
        next: &GraphSnapshot,
    ) -> Result<(), ApplicationError> {
        let plan = channel_plan(next, self.library_root())?;
        let Some(port) = self.channels.as_deref() else {
            return Ok(());
        };
        if let Some(state) = port.snapshot(graph)?
            && state.saved_enabled
            && plan.is_none_or(|plan| {
                plan.plugin != state.binding.plugin || plan.platform != state.platform
            })
        {
            return Err(ApplicationError::Conflict(STOP_FIRST.into()));
        }
        Ok(())
    }

    /// A Graph with an enabled bot cannot be deleted (D6); a stopped binding
    /// is removed together with the Graph.
    pub(crate) fn check_channel_graph_deletion(&self, graph: &str) -> Result<(), ApplicationError> {
        match self.channels.as_deref() {
            Some(port)
                if port
                    .snapshot(graph)?
                    .is_some_and(|state| state.saved_enabled) =>
            {
                Err(ApplicationError::Conflict(STOP_FIRST.into()))
            }
            _ => Ok(()),
        }
    }

    /// The one rule for changing a bot's credentials, from the HTTP API or a
    /// Pilot credential form: the Graph is a bot, the caller owns its binding,
    /// and the bot is stopped, so its account cannot change under a running
    /// gateway or bypass the account checks made when enabling.
    pub(crate) fn check_channel_credentials_mutable(
        &self,
        owner: &str,
        graph: &str,
    ) -> Result<ChannelPlan, ApplicationError> {
        let plan = self.channel_bot_plan(graph)?;
        if let Some(state) = self.channel_snapshot(graph)? {
            if state.owner != owner {
                return Err(ApplicationError::Conflict(
                    "this Graph's bot belongs to another owner".into(),
                ));
            }
            if state.saved_enabled {
                return Err(ApplicationError::Conflict(
                    "stop this Graph's bot before changing its credentials".into(),
                ));
            }
        }
        Ok(plan)
    }

    pub(crate) async fn remove_channel_binding(&self, graph: &str) -> Result<(), ApplicationError> {
        match self.channels.as_deref() {
            Some(port) => port.remove(graph).await,
            None => Ok(()),
        }
    }

    pub(crate) fn channel_snapshot(
        &self,
        graph: &str,
    ) -> Result<Option<ChannelRuntimeState>, ApplicationError> {
        match self.channels.as_deref() {
            Some(port) => port.snapshot(graph),
            None => Ok(None),
        }
    }

    pub(crate) fn channel_snapshots(&self) -> Result<Vec<ChannelRuntimeState>, ApplicationError> {
        match self.channels.as_deref() {
            Some(port) => port.snapshots(),
            None => Ok(Vec::new()),
        }
    }

    pub(crate) fn channel_callback(&self, key: &str) -> Option<ChannelCallback> {
        self.channels.as_deref()?.callback(key)
    }
}

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;
