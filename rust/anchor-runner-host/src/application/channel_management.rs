//! Host-owned channel bindings: one bot per persistent assistant Graph, keyed
//! by the Graph name. The existing Session/Turn admission and ChannelSupervisor
//! retain execution and transport semantics.
use super::{ApplicationError, RunApplication};
use crate::plugin_library::Catalog;
use anchor_runtime::graph::GraphSnapshot;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// The only channel platform this Host supports.
pub(crate) const PLATFORM: &str = "wecom";

/// The operator-approved settings of one Graph's bot. The Graph is the
/// binding's key; the reply node is derived from the Graph, never configured.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChannelBinding {
    pub(crate) plugin: String,
    pub(crate) users: Vec<String>,
    pub(crate) send_users: Vec<String>,
}

/// The member lists an operator chooses; the Plugin comes from the Graph.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChannelMembers {
    pub(crate) users: Vec<String>,
    pub(crate) send_users: Vec<String>,
}

impl ChannelBinding {
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
        self
    }
}

/// How a Graph serves as a bot: its assistant work node and the one channel
/// Plugin mounted on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelPlan {
    pub(crate) work_node: String,
    pub(crate) plugin: String,
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
    let mut channels = node.plugins.iter().filter(|reference| {
        catalog
            .channels(&reference.id)
            .is_ok_and(|channels| channels.iter().any(|channel| channel.platform == PLATFORM))
    });
    match (channels.next(), channels.next()) {
        (None, _) => Ok(None),
        (Some(plugin), None) => Ok(Some(ChannelPlan {
            work_node: plan.work_node,
            plugin: plugin.id.clone(),
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
            .filter(|current| source == PLATFORM && current.enabled)
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
        };
        binding.validate()?;
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
            if plan.plugin != state.binding.plugin {
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
            && plan.is_none_or(|plan| plan.plugin != state.binding.plugin)
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
