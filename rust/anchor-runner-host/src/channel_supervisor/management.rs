//! Host-owned bot bindings, one per persistent assistant Graph, each with its
//! own supervised gateway. Persisted values are configuration facts, not a new
//! channel runtime or environment asset.
use super::status::ConnectionStatus;
use super::{ChannelSupervisor, ManagedChannelLaunch, random_token};
use crate::application::{
    ApplicationError,
    channel_management::{
        ChannelBinding, ChannelCallback, ChannelManagementPort, ChannelRuntimeState, PLATFORM,
        channel_plan,
    },
};
use crate::plugin_library::Catalog;
use anchor_graph_host::{ChannelDefinition, FileGraphBundleLoader, PluginMetadata};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::RwLock,
};
use tokio::sync::Mutex;

const FORMAT: u32 = 2;

pub(crate) struct ChannelHostConfiguration {
    pub(crate) data_root: PathBuf,
    pub(crate) catalog_root: PathBuf,
    /// The Host's configured Graph, served from its bundle root rather than
    /// the catalog; same resolution as `RunApplication::graph_bundle_path`.
    pub(crate) configured_graph: Option<(String, PathBuf)>,
    pub(crate) library_root: PathBuf,
    pub(crate) listen: String,
}

impl ChannelHostConfiguration {
    fn graph_path(&self, graph: &str) -> PathBuf {
        self.configured_graph
            .as_ref()
            .filter(|(name, _)| name == graph)
            .map(|(_, path)| path.clone())
            .unwrap_or_else(|| self.catalog_root.join(graph))
    }
}

/// `platform/channels/graphs/<graph>.json`. The file name is the Graph; the
/// reply node is derived from the Graph each time it is admitted.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ChannelFact {
    format: u32,
    owner: String,
    plugin: String,
    users: Vec<String>,
    send_users: Vec<String>,
    enabled: bool,
    authorization_digest: String,
    callback_key: String,
    #[serde(default)]
    account: Option<String>,
}

impl ChannelFact {
    fn binding(&self) -> ChannelBinding {
        ChannelBinding {
            plugin: self.plugin.clone(),
            users: self.users.clone(),
            send_users: self.send_users.clone(),
        }
    }
}

pub(crate) struct ChannelManagementAdapter {
    configuration: ChannelHostConfiguration,
    host_runtime: tokio::runtime::Handle,
    root: PathBuf,
    facts: RwLock<BTreeMap<String, ChannelFact>>,
    /// Binding files that exist but cannot be read, by Graph. Each stays out
    /// of service and refuses every change to its Graph until an operator
    /// repairs or removes it; the other bots and the Host still start.
    damaged: BTreeMap<String, String>,
    /// Running gateways by Graph. Holding this lock serializes every binding
    /// change, so account uniqueness (R4) is checked against settled facts.
    supervisors: Mutex<BTreeMap<String, ChannelSupervisor>>,
    _owner_lease: anchor_file_lease::FileLease,
}

impl ChannelManagementAdapter {
    pub(crate) fn open(configuration: ChannelHostConfiguration) -> Result<Self, String> {
        let host_runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "channel management requires the Host runtime")?;
        let root = configuration.data_root.join("platform/channels");
        crate::host_fs::directory(&root)?;
        let metadata = fs::metadata(&root).map_err(|_| "channel configuration root unavailable")?;
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(
                "channel configuration directory requires Host ownership and mode 700".into(),
            );
        }
        let owner_lease = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(root.join("configuration.lock"))
            .map_err(|_| "channel configuration lease unavailable")?;
        if !owner_lease
            .metadata()
            .map_err(|_| "channel configuration lease unavailable")?
            .is_file()
        {
            return Err("channel configuration lease must be a regular file".into());
        }
        let owner_lease = anchor_file_lease::FileLease::acquire(
            owner_lease,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .map_err(|_| "channel configuration already has a Host owner")?;
        if fs::symlink_metadata(root.join("wecom.json")).is_ok() {
            return Err(LEGACY_BINDING.into());
        }
        let (facts, damaged) = read_facts(&root.join("graphs"))?;
        for (graph, reason) in &damaged {
            eprintln!(
                "anchor channel wecom: the bot binding of Graph {graph} is unusable ({reason}); repair or remove it"
            );
        }
        Ok(Self {
            configuration,
            host_runtime,
            root,
            facts: RwLock::new(facts),
            damaged,
            supervisors: Mutex::new(BTreeMap::new()),
            _owner_lease: owner_lease,
        })
    }

    /// Start every enabled bot. A binding whose Graph, Plugin or credentials
    /// went away stays saved but stopped; it must not keep the Host, the Pilot
    /// or the other bots from starting.
    pub(crate) async fn resume(&self) -> Result<(), ApplicationError> {
        let mut supervisors = self.supervisors.lock().await;
        for (graph, mut fact) in self.facts()? {
            if !fact.enabled || supervisors.contains_key(&graph) {
                continue;
            }
            if let Ok((definition, channel)) = self.definition(&fact.plugin)
                && let Ok(credentials) = self.credentials(&graph, &fact, &definition, &channel)
            {
                // Credentials changed outside the API take the same account
                // checks as enabling; a conflict keeps this bot stopped.
                let account = match self.register_account(&fact.owner, &credentials).and_then(
                    |account| {
                        if fact.account != account {
                            self.check_account(&graph, &fact.owner, account.as_deref())?;
                        }
                        Ok(account)
                    },
                ) {
                    Ok(account) => account,
                    Err(error) => {
                        eprintln!(
                            "anchor channel wecom: the bot of Graph {graph} stays stopped: {error:?}"
                        );
                        continue;
                    }
                };
                if fact.account != account {
                    fact.account = account;
                    self.publish(&graph, fact.clone())?;
                }
            }
            match self.start(&graph, &fact).await {
                Ok(started) => {
                    supervisors.insert(graph, started);
                }
                Err(_) => eprintln!(
                    "anchor channel wecom: the saved bot of Graph {graph} is currently unavailable"
                ),
            }
        }
        Ok(())
    }

    pub(crate) async fn shutdown(&self) {
        let mut supervisors = self.supervisors.lock().await;
        for (_, mut active) in std::mem::take(&mut *supervisors) {
            active.shutdown().await;
        }
    }

    fn facts(&self) -> Result<BTreeMap<String, ChannelFact>, ApplicationError> {
        self.facts
            .read()
            .map(|facts| facts.clone())
            .map_err(|_| storage("channel configuration unavailable"))
    }

    fn fact(&self, graph: &str) -> Result<Option<ChannelFact>, ApplicationError> {
        Ok(self.facts()?.remove(graph))
    }

    fn register_account(
        &self,
        owner: &str,
        credentials: &BTreeMap<String, String>,
    ) -> Result<Option<String>, ApplicationError> {
        credentials
            .get("WECOM_BOT_ID")
            .map(|identity| register_account(&self.configuration, owner, identity).map_err(storage))
            .transpose()
    }

    fn assert_owner(&self, owner: &str, graph: &str) -> Result<(), ApplicationError> {
        if owner.is_empty() || owner.len() > 500 || owner.chars().any(char::is_control) {
            return Err(ApplicationError::Invalid("invalid channel owner".into()));
        }
        if !anchor_runtime::identifier::is_name(graph) {
            return Err(ApplicationError::Invalid("invalid bot Graph name".into()));
        }
        self.assert_readable(graph)?;
        if self.fact(graph)?.is_some_and(|fact| fact.owner != owner) {
            return Err(ApplicationError::Conflict(
                "this Graph's bot belongs to another owner".into(),
            ));
        }
        Ok(())
    }

    fn assert_readable(&self, graph: &str) -> Result<(), ApplicationError> {
        match self.damaged.get(graph) {
            Some(reason) => Err(ApplicationError::Conflict(format!(
                "the bot binding file of Graph {graph} is unusable ({reason}); repair or remove platform/channels/graphs/{graph}.json"
            ))),
            None => Ok(()),
        }
    }

    fn path(&self, graph: &str) -> PathBuf {
        self.root.join("graphs").join(format!("{graph}.json"))
    }

    fn publish(&self, graph: &str, fact: ChannelFact) -> Result<(), ApplicationError> {
        let directory = self.root.join("graphs");
        crate::host_fs::directory(&directory).map_err(storage)?;
        // Staged beside, not inside, the bindings: any name in graphs/ is a Graph.
        let staging = self.root.join("staging");
        crate::host_fs::directory(&staging).map_err(storage)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&staging)
            .map_err(|_| storage("channel configuration staging unavailable"))?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o600))
            .map_err(|_| storage("channel configuration permissions unavailable"))?;
        temporary
            .write_all(
                &serde_json::to_vec(&fact)
                    .map_err(|_| storage("channel configuration encoding failed"))?,
            )
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|_| storage("channel configuration persistence failed"))?;
        temporary
            .persist(self.path(graph))
            .map_err(|_| storage("channel configuration publication failed"))?;
        self.facts
            .write()
            .map_err(|_| storage("channel configuration unavailable"))?
            .insert(graph.to_owned(), fact);
        if fs::File::open(&directory)
            .and_then(|directory| directory.sync_all())
            .is_err()
        {
            // The rename has already published; retain the matching in-memory
            // fact rather than roll back to a snapshot no longer on disk.
            eprintln!("channel configuration published; directory sync failed");
        }
        Ok(())
    }

    fn definition(
        &self,
        plugin: &str,
    ) -> Result<(PluginMetadata, ChannelDefinition), ApplicationError> {
        let definition = Catalog::new(&self.configuration.library_root)
            .definition(plugin)
            .map_err(|_| ApplicationError::Missing)?;
        Self::channel(definition.metadata())
    }

    fn metadata(
        &self,
        plugin: &str,
    ) -> Result<(PluginMetadata, ChannelDefinition), ApplicationError> {
        let definition = Catalog::new(&self.configuration.library_root)
            .metadata(plugin)
            .map_err(|_| ApplicationError::Missing)?;
        Self::channel(definition)
    }

    fn channel(
        definition: PluginMetadata,
    ) -> Result<(PluginMetadata, ChannelDefinition), ApplicationError> {
        let channel = definition
            .channels
            .iter()
            .find(|channel| channel.platform == PLATFORM)
            .cloned()
            .ok_or_else(|| {
                ApplicationError::Invalid(
                    "the installed Plugin has no wecom channel declaration".into(),
                )
            })?;
        Ok((definition, channel))
    }

    fn credentials(
        &self,
        graph: &str,
        fact: &ChannelFact,
        definition: &PluginMetadata,
        channel: &ChannelDefinition,
    ) -> Result<BTreeMap<String, String>, ApplicationError> {
        if crate::credential_store::channel_authorization(definition, &channel.platform)
            .map_err(storage)?
            .1
            != fact.authorization_digest
        {
            return Err(ApplicationError::Conflict(
                "channel declarations changed; disable and configure the bot again".into(),
            ));
        }
        crate::credential_store::channel_environment_values(
            &self.configuration.data_root,
            &crate::credential_store::binding_owner(&fact.owner),
            definition,
            &crate::credential_store::channel_target(PLATFORM, graph),
        )
        .map_err(storage)
    }

    fn complete(
        channel: &ChannelDefinition,
        credentials: &BTreeMap<String, String>,
    ) -> Result<(), ApplicationError> {
        match channel.required_environment.iter().find(|field| {
            credentials
                .get(*field)
                .is_none_or(|value| value.trim().is_empty() || value.chars().any(char::is_control))
        }) {
            Some(field) => Err(ApplicationError::Invalid(format!(
                "bot credential {field} is not configured for this Graph"
            ))),
            None => Ok(()),
        }
    }

    /// The Graph still admits as this binding's bot; returns its reply node.
    fn admission(&self, graph: &str, fact: &ChannelFact) -> Result<String, ApplicationError> {
        let bundle = FileGraphBundleLoader::new(self.configuration.graph_path(graph))
            .with_library_root(&self.configuration.library_root)
            .load()
            .map_err(|_| ApplicationError::Missing)?;
        match channel_plan(&bundle.snapshot, &self.configuration.library_root)? {
            Some(plan) if plan.plugin == fact.plugin => Ok(plan.work_node),
            _ => Err(ApplicationError::Invalid(
                crate::application::channel_management::NOT_A_BOT.into(),
            )),
        }
    }

    async fn start(
        &self,
        graph: &str,
        fact: &ChannelFact,
    ) -> Result<ChannelSupervisor, ApplicationError> {
        self.admission(graph, fact)?;
        let (definition, channel) = self.definition(&fact.plugin)?;
        let credentials = self.credentials(graph, fact, &definition, &channel)?;
        Self::complete(&channel, &credentials)?;
        ChannelSupervisor::start_managed(
            &self.host_runtime,
            &self.configuration.graph_path(graph),
            &self.configuration.library_root,
            &self.configuration.data_root,
            &self.configuration.listen,
            ManagedChannelLaunch {
                graph: graph.into(),
                plugin: fact.plugin.clone(),
                platform: PLATFORM.into(),
                credentials,
                users: fact.users.clone(),
                send_users: fact.send_users.clone(),
                callback_key: fact.callback_key.clone(),
            },
        )
        .await
        .map_err(|_| storage("channel gateway could not start"))
    }

    /// One account keeps one long connection: a second enabled binding would
    /// disconnect the first (R4). And a Session belongs to one Graph, so an
    /// account whose Sessions belong to another Graph cannot move here (D7).
    fn check_account(
        &self,
        graph: &str,
        owner: &str,
        account: Option<&str>,
    ) -> Result<(), ApplicationError> {
        let Some(account) = account else {
            return Ok(());
        };
        if let Some((other, _)) = self.facts()?.into_iter().find(|(other, fact)| {
            other != graph && fact.enabled && fact.account.as_deref() == Some(account)
        }) {
            return Err(ApplicationError::Conflict(format!(
                "this bot account is already in use by Graph {other}; stop that bot first"
            )));
        }
        let sessions = anchor_platform_session::SessionStore::open(
            self.configuration
                .data_root
                .join("platform/sessions.sqlite"),
        )
        .map_err(|_| storage("channel Sessions unavailable"))?;
        let mut foreign = Vec::new();
        for session in sessions
            .list(owner)
            .map_err(|_| storage("channel Sessions unavailable"))?
        {
            if session.graph == graph
                || session.channel.get("source").map(String::as_str) != Some(PLATFORM)
            {
                continue;
            }
            let saved = session.channel.get("account").map_or("", String::as_str);
            if sessions
                .canonical_channel_account(owner, PLATFORM, saved)
                .map_err(|_| storage("channel Sessions unavailable"))?
                == account
            {
                foreign.push(format!("{} ({})", session.title, session.graph));
            }
        }
        if !foreign.is_empty() {
            foreign.truncate(5);
            return Err(ApplicationError::Conflict(format!(
                "this bot account already has message Sessions of another Graph: {}. Clear them in 消息会话 before enabling it here",
                foreign.join(", ")
            )));
        }
        Ok(())
    }

    async fn public_status(&self, owner: &str, graph: &str) -> Result<Value, ApplicationError> {
        let supervisors = self.supervisors.lock().await;
        if self.damaged.contains_key(graph) {
            return Ok(json!({"graph":graph,"platform":PLATFORM,"configured":true,
                "damaged":true,"enabled":false,"listener":"unavailable","mutable":false}));
        }
        let Some(fact) = self.fact(graph)? else {
            return Ok(json!({"graph":graph,"platform":PLATFORM,"configured":false,
                "enabled":false,"listener":"stopped","mutable":true}));
        };
        if fact.owner != owner {
            return Ok(json!({"graph":graph,"platform":PLATFORM,"configured":true,
                "owner_matches":false,"mutable":false}));
        }
        let admitted = self.admission(graph, &fact);
        let (available, credentials, account) = match self.metadata(&fact.plugin) {
            Ok((definition, channel)) => {
                let values = self.credentials(graph, &fact, &definition, &channel);
                let fields = channel
                    .required_environment
                    .iter()
                    .map(|field| {
                        json!({"field":field,"configured":values.as_ref().is_ok_and(|values| {
                            values.get(field).is_some_and(|value| !value.trim().is_empty())
                        })})
                    })
                    .collect::<Vec<_>>();
                let complete = values
                    .as_ref()
                    .is_ok_and(|values| Self::complete(&channel, values).is_ok());
                let account = values.as_ref().ok().and_then(account_identity);
                (admitted.is_ok() && complete, fields, account)
            }
            Err(_) => (false, Vec::new(), None),
        };
        let supervisor = supervisors.get(graph);
        let supervised = supervisor.is_some_and(ChannelSupervisor::is_running);
        let listener = if !fact.enabled {
            ConnectionStatus::Stopped
        } else if !available {
            ConnectionStatus::Unavailable
        } else {
            self.listener_status(graph, supervisor).await
        };
        Ok(
            json!({"graph":graph,"platform":PLATFORM,"configured":true,"owner_matches":true,
            "mutable":true,"enabled":fact.enabled,"available":available,
            "reply_node":admitted.ok(),"binding":fact.binding(),"credentials":credentials,
            "listener":listener,"account":account,
            "restart_required":fact.enabled && (fact.account != account || !supervised
                || (available && matches!(listener, ConnectionStatus::Unavailable | ConnectionStatus::Stopped)))}),
        )
    }

    async fn listener_status(
        &self,
        graph: &str,
        supervisor: Option<&ChannelSupervisor>,
    ) -> ConnectionStatus {
        let Some(supervisor) = supervisor.filter(|supervisor| supervisor.is_running()) else {
            return ConnectionStatus::Unavailable;
        };
        match super::status::read(&super::descriptor_path(
            &self.configuration.data_root,
            graph,
        ))
        .await
        {
            ConnectionStatus::Unavailable => supervisor.fallback_status(),
            observed => observed,
        }
    }

    fn runtime_state(&self, graph: &str, fact: ChannelFact) -> ChannelRuntimeState {
        let reply_node = self.admission(graph, &fact).ok();
        let enabled = fact.enabled
            && reply_node.is_some()
            && self
                .metadata(&fact.plugin)
                .is_ok_and(|(definition, channel)| {
                    self.credentials(graph, &fact, &definition, &channel)
                        .is_ok_and(|values| {
                            account_identity(&values) == fact.account
                                && Self::complete(&channel, &values).is_ok()
                        })
                });
        ChannelRuntimeState {
            graph: graph.into(),
            owner: fact.owner.clone(),
            binding: fact.binding(),
            reply_node,
            enabled,
            saved_enabled: fact.enabled,
            account: fact.account.clone(),
            descriptor: fact
                .enabled
                .then(|| super::descriptor_path(&self.configuration.data_root, graph)),
        }
    }
}

impl ChannelManagementPort for ChannelManagementAdapter {
    fn list<'a>(&'a self, owner: &'a str) -> BoxFuture<'a, Result<Value, ApplicationError>> {
        Box::pin(async move {
            let mut channels = Vec::new();
            let mut graphs = self.facts()?.into_keys().collect::<Vec<_>>();
            graphs.extend(self.damaged.keys().cloned());
            graphs.sort();
            for graph in graphs {
                channels.push(self.public_status(owner, &graph).await?);
            }
            Ok(json!({"channels":channels}))
        })
    }

    fn status<'a>(
        &'a self,
        owner: &'a str,
        graph: &'a str,
    ) -> BoxFuture<'a, Result<Value, ApplicationError>> {
        Box::pin(async move { self.public_status(owner, graph).await })
    }

    fn configure<'a>(
        &'a self,
        owner: &'a str,
        graph: &'a str,
        binding: ChannelBinding,
    ) -> BoxFuture<'a, Result<Value, ApplicationError>> {
        Box::pin(async move {
            let supervisors = self.supervisors.lock().await;
            self.assert_owner(owner, graph)?;
            binding.validate()?;
            let previous = self.fact(graph)?;
            if previous.as_ref().is_some_and(|fact| fact.enabled) {
                return Err(ApplicationError::Conflict(
                    "stop this Graph's bot before changing its members".into(),
                ));
            }
            let (definition, _) = self.definition(&binding.plugin)?;
            let digest = crate::credential_store::channel_authorization(&definition, PLATFORM)
                .map_err(storage)?
                .1;
            let changed = previous.as_ref().is_none_or(|fact| {
                fact.binding() != binding || fact.authorization_digest != digest
            });
            if changed {
                let fact = ChannelFact {
                    format: FORMAT,
                    owner: owner.into(),
                    plugin: binding.plugin,
                    users: binding.users,
                    send_users: binding.send_users,
                    enabled: false,
                    authorization_digest: digest,
                    callback_key: match previous {
                        Some(fact) => fact.callback_key,
                        None => random_token("anchor-channel-api")
                            .map_err(|_| storage("channel callback identity unavailable"))?,
                    },
                    account: None,
                };
                self.admission(graph, &fact)?;
                self.publish(graph, fact)?;
            }
            drop(supervisors);
            let mut result = self.public_status(owner, graph).await?;
            result["changed"] = json!(changed);
            Ok(result)
        })
    }

    fn set_enabled<'a>(
        &'a self,
        owner: &'a str,
        graph: &'a str,
        enabled: bool,
    ) -> BoxFuture<'a, Result<Value, ApplicationError>> {
        Box::pin(async move {
            let mut supervisors = self.supervisors.lock().await;
            self.assert_owner(owner, graph)?;
            let mut fact = self.fact(graph)?.ok_or(ApplicationError::Missing)?;
            let mut changed = fact.enabled != enabled;
            if enabled {
                // Validate all credentials before saving an enabled intent.
                self.admission(graph, &fact)?;
                let (definition, channel) = self.definition(&fact.plugin)?;
                let credentials = self.credentials(graph, &fact, &definition, &channel)?;
                Self::complete(&channel, &credentials)?;
                // Re-enabling a saved-enabled bot that is not running must
                // pass the same account checks as a first enable.
                let account = self.register_account(&fact.owner, &credentials)?;
                let needs_start = changed
                    || supervisors
                        .get(graph)
                        .is_none_or(|supervisor| !supervisor.is_running())
                    || matches!(
                        self.listener_status(graph, supervisors.get(graph)).await,
                        ConnectionStatus::Unavailable | ConnectionStatus::Stopped
                    )
                    || fact.account != account;
                if needs_start {
                    self.check_account(graph, owner, account.as_deref())?;
                }
                if changed {
                    fact.enabled = true;
                    fact.callback_key = random_token("anchor-channel-api")
                        .map_err(|_| storage("channel callback identity unavailable"))?;
                }
                if changed || fact.account != account {
                    fact.account = account;
                    self.publish(graph, fact.clone())?;
                }
                if needs_start {
                    if let Some(mut previous) = supervisors.remove(graph) {
                        previous.shutdown().await;
                    }
                    let started = self.start(graph, &fact).await?;
                    supervisors.insert(graph.to_owned(), started);
                    changed = true;
                }
            } else {
                if changed {
                    fact.enabled = false;
                    self.publish(graph, fact.clone())?;
                }
                if let Some(mut active) = supervisors.remove(graph) {
                    active.shutdown().await;
                }
            }
            drop(supervisors);
            let mut result = self.public_status(owner, graph).await?;
            result["changed"] = json!(changed);
            Ok(result)
        })
    }

    fn snapshot(&self, graph: &str) -> Result<Option<ChannelRuntimeState>, ApplicationError> {
        self.assert_readable(graph)?;
        Ok(self
            .fact(graph)?
            .map(|fact| self.runtime_state(graph, fact)))
    }

    fn snapshots(&self) -> Result<Vec<ChannelRuntimeState>, ApplicationError> {
        Ok(self
            .facts()?
            .into_iter()
            .map(|(graph, fact)| self.runtime_state(&graph, fact))
            .collect())
    }

    fn callback(&self, key: &str) -> Option<ChannelCallback> {
        self.facts().ok()?.into_iter().find_map(|(graph, fact)| {
            (fact.enabled && token_matches(&fact.callback_key, key)).then_some(ChannelCallback {
                graph,
                owner: fact.owner,
            })
        })
    }

    fn remove<'a>(&'a self, graph: &'a str) -> BoxFuture<'a, Result<(), ApplicationError>> {
        Box::pin(async move {
            let supervisors = self.supervisors.lock().await;
            self.assert_readable(graph)?;
            let fact = self.fact(graph)?;
            if fact.as_ref().is_some_and(|fact| fact.enabled) || supervisors.contains_key(graph) {
                return Err(ApplicationError::Conflict(
                    "此 Graph 的机器人正在运行，请先停用机器人".into(),
                ));
            }
            // Credentials may exist without a binding (entered before the
            // members) or under a Plugin the Graph no longer mounts.
            crate::credential_store::delete_channel_graph(
                &self.configuration.data_root,
                &crate::credential_store::channel_target(PLATFORM, graph),
            )
            .map_err(storage)?;
            let state = super::state_directory(&self.configuration.data_root, graph);
            if fs::symlink_metadata(&state).is_ok() {
                fs::remove_dir_all(&state)
                    .map_err(|_| storage("bot gateway state cleanup failed"))?;
            }
            if fact.is_some() {
                fs::remove_file(self.path(graph))
                    .map_err(|_| storage("bot binding removal failed"))?;
            }
            self.facts
                .write()
                .map_err(|_| storage("channel configuration unavailable"))?
                .remove(graph);
            Ok(())
        })
    }
}

const LEGACY_BINDING: &str = "platform/channels/wecom.json is the single-bot binding of an earlier Host. Migrate it to platform/channels/graphs/<graph>.json with the deployment's migration script before starting this Host";

fn storage(reason: impl Into<String>) -> ApplicationError {
    ApplicationError::Storage(reason.into())
}

fn account_identity(credentials: &BTreeMap<String, String>) -> Option<String> {
    credentials
        .get("WECOM_BOT_ID")
        .map(|identity| wecom_account_identity(identity))
}

pub(crate) fn wecom_account_identity(identity: &str) -> String {
    format!("wecom:{:x}", Sha256::digest(identity.trim().as_bytes()))
}

fn register_account(
    configuration: &ChannelHostConfiguration,
    owner: &str,
    identity: &str,
) -> Result<String, String> {
    let account = wecom_account_identity(identity);
    crate::host_fs::directory(&configuration.data_root.join("platform"))?;
    anchor_platform_session::SessionStore::open(
        configuration.data_root.join("platform/sessions.sqlite"),
    )
    .and_then(|sessions| {
        sessions.register_channel_account(
            owner,
            PLATFORM,
            &account,
            &[
                identity.to_owned(),
                identity.trim().to_owned(),
                format!("wecom:{:x}", Sha256::digest(identity.as_bytes())),
            ],
        )
    })
    .map_err(|_| "channel account identity registration failed".to_owned())?;
    Ok(account)
}

fn token_matches(expected: &str, key: &str) -> bool {
    let left = Sha256::digest(expected.as_bytes());
    let right = Sha256::digest(key.as_bytes());
    left.iter()
        .zip(right.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

type Facts = (BTreeMap<String, ChannelFact>, BTreeMap<String, String>);

/// Bindings by Graph, and the Graphs whose binding file is unusable. Entries
/// that are not `<graph>.json` (a desktop index file, an editor backup) are
/// not bindings and are left alone.
fn read_facts(directory: &Path) -> Result<Facts, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((BTreeMap::new(), BTreeMap::new()));
        }
        Err(_) => return Err("channel configuration unavailable".into()),
    };
    let mut facts = BTreeMap::new();
    let mut damaged = BTreeMap::new();
    for entry in entries {
        let path = entry
            .map_err(|_| "channel configuration unavailable")?
            .path();
        let Some(graph) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|graph| anchor_runtime::identifier::is_name(graph))
        else {
            eprintln!(
                "anchor channel wecom: ignoring {} in the bot bindings directory",
                path.display()
            );
            continue;
        };
        match read_fact(&path) {
            Ok(fact) => {
                facts.insert(graph.to_owned(), fact);
            }
            Err(reason) => {
                damaged.insert(graph.to_owned(), reason);
            }
        }
    }
    Ok((facts, damaged))
}

fn read_fact(path: &Path) -> Result<ChannelFact, String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .map_err(|_| "channel configuration unavailable")?;
    let metadata = file
        .metadata()
        .map_err(|_| "channel configuration metadata unavailable")?;
    if !metadata.is_file()
        || metadata.len() > 512 * 1024
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(
            "channel configuration requires a bounded Host-owned regular file with mode 600".into(),
        );
    }
    let mut bytes = Vec::new();
    file.take(512 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "channel configuration unavailable")?;
    if bytes.len() > 512 * 1024 {
        return Err("channel configuration exceeds the size limit".into());
    }
    let fact: ChannelFact =
        serde_json::from_slice(&bytes).map_err(|_| "channel configuration is invalid")?;
    fact.binding()
        .validate()
        .map_err(|_| "channel configuration binding is invalid")?;
    if fact.format != FORMAT
        || fact.owner.is_empty()
        || fact.owner.len() > 500
        || fact.owner.chars().any(char::is_control)
        || fact.authorization_digest.len() != 64
        || !fact
            .authorization_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !fact.callback_key.starts_with("anchor-channel-api-")
        || fact.callback_key.len() != 83
    {
        return Err("unsupported or invalid channel configuration identity".into());
    }
    Ok(fact)
}

#[cfg(test)]
mod tests;
