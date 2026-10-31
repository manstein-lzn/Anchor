//! One in-memory channel port for tests above the adapter. It keeps the
//! adapter's public contract (owner scoping, stop-before-change, callback keys
//! of enabled bindings) without credentials, gateways or files.
use super::*;
use serde_json::json;
use std::{collections::BTreeMap, sync::Mutex};

type Inspect = Box<dyn Fn() + Send + Sync>;

/// Install a minimal channel Plugin `id` under `library`, so a Graph mounting
/// it on its assistant node can be a bot.
pub(crate) fn install_channel_plugin(library: &std::path::Path, id: &str) {
    let plugin = library.join("plugins").join(id);
    std::fs::create_dir_all(plugin.join("bin")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        json!({"name": "Gateway fixture"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        plugin.join("channel.json"),
        json!({"platform":"wecom","transport":"websocket","entrypoint":"bin/gateway","required_environment":["WECOM_BOT_ID","WECOM_BOT_SECRET"]}).to_string(),
    )
    .unwrap();
    std::fs::write(plugin.join("bin/gateway"), "#!/bin/sh\n").unwrap();
}

#[derive(Default)]
pub(crate) struct FakeChannels {
    states: Mutex<BTreeMap<String, ChannelRuntimeState>>,
    callbacks: Mutex<BTreeMap<String, String>>,
    inspect: Option<Inspect>,
}

impl FakeChannels {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Run `inspect` on every snapshot read.
    pub(crate) fn inspecting(inspect: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            inspect: Some(Box::new(inspect)),
            ..Self::default()
        }
    }

    /// An enabled bot of `graph` whose assistant node is `reply_node`.
    pub(crate) fn bot(
        graph: &str,
        owner: &str,
        reply_node: &str,
        users: &[&str],
    ) -> ChannelRuntimeState {
        let users = users
            .iter()
            .map(|user| (*user).to_owned())
            .collect::<Vec<_>>();
        ChannelRuntimeState {
            graph: graph.into(),
            owner: owner.into(),
            binding: ChannelBinding {
                plugin: "wecom".into(),
                users: users.clone(),
                send_users: users,
            },
            reply_node: Some(reply_node.into()),
            enabled: true,
            saved_enabled: true,
            account: Some("corp-a".into()),
            descriptor: None,
        }
    }

    pub(crate) fn with(self, state: ChannelRuntimeState) -> Self {
        self.put(state);
        self
    }

    /// Accept `key` as the gateway callback key of `graph` while it is enabled.
    pub(crate) fn with_callback(self, graph: &str, key: &str) -> Self {
        self.callbacks
            .lock()
            .unwrap()
            .insert(key.into(), graph.into());
        self
    }

    pub(crate) fn put(&self, state: ChannelRuntimeState) {
        self.states
            .lock()
            .unwrap()
            .insert(state.graph.clone(), state);
    }

    pub(crate) fn clear(&self, graph: &str) {
        self.states.lock().unwrap().remove(graph);
    }

    pub(crate) fn get(&self, graph: &str) -> Option<ChannelRuntimeState> {
        self.states.lock().unwrap().get(graph).cloned()
    }

    pub(crate) fn update(&self, graph: &str, change: impl FnOnce(&mut ChannelRuntimeState)) {
        change(
            self.states
                .lock()
                .unwrap()
                .get_mut(graph)
                .expect("fake bot"),
        );
    }

    fn public(state: Option<&ChannelRuntimeState>, owner: &str, graph: &str) -> Value {
        match state {
            None => json!({"graph":graph,"configured":false,"enabled":false}),
            Some(state) if state.owner != owner => {
                json!({"graph":graph,"configured":true,"owner_matches":false})
            }
            Some(state) => json!({"graph":graph,"configured":true,"owner_matches":true,
                "binding":state.binding,"enabled":state.enabled}),
        }
    }
}

impl ChannelManagementPort for FakeChannels {
    fn list<'a>(&'a self, owner: &'a str) -> BoxFuture<'a, Result<Value, ApplicationError>> {
        Box::pin(async move {
            let states = self.states.lock().unwrap();
            let channels = states
                .iter()
                .map(|(graph, state)| Self::public(Some(state), owner, graph))
                .collect::<Vec<_>>();
            Ok(json!({"channels":channels}))
        })
    }

    fn status<'a>(
        &'a self,
        owner: &'a str,
        graph: &'a str,
    ) -> BoxFuture<'a, Result<Value, ApplicationError>> {
        Box::pin(async move {
            let state = self.get(graph);
            let mut result = Self::public(state.as_ref(), owner, graph);
            if let Some(state) = state.filter(|state| state.owner == owner) {
                result["listener"] = if !state.enabled {
                    json!("stopped")
                } else if let Some(descriptor) = state.descriptor {
                    json!(crate::channel_supervisor::status::read(&descriptor).await)
                } else {
                    json!("unavailable")
                };
            }
            Ok(result)
        })
    }

    fn configure<'a>(
        &'a self,
        owner: &'a str,
        graph: &'a str,
        binding: ChannelBinding,
    ) -> BoxFuture<'a, Result<Value, ApplicationError>> {
        Box::pin(async move {
            let mut states = self.states.lock().unwrap();
            if let Some(state) = states.get(graph) {
                if state.owner != owner {
                    return Err(ApplicationError::Conflict("another owner".into()));
                }
                if state.saved_enabled {
                    return Err(ApplicationError::Conflict("stop the bot first".into()));
                }
            }
            let state = ChannelRuntimeState {
                graph: graph.into(),
                owner: owner.into(),
                binding,
                reply_node: None,
                enabled: false,
                saved_enabled: false,
                account: None,
                descriptor: None,
            };
            let result = Self::public(Some(&state), owner, graph);
            states.insert(graph.into(), state);
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
            let mut states = self.states.lock().unwrap();
            let state = states.get_mut(graph).ok_or(ApplicationError::Missing)?;
            if state.owner != owner {
                return Err(ApplicationError::Conflict("another owner".into()));
            }
            state.enabled = enabled;
            state.saved_enabled = enabled;
            Ok(Self::public(Some(state), owner, graph))
        })
    }

    fn snapshot(&self, graph: &str) -> Result<Option<ChannelRuntimeState>, ApplicationError> {
        if let Some(inspect) = &self.inspect {
            inspect();
        }
        Ok(self.get(graph))
    }

    fn snapshots(&self) -> Result<Vec<ChannelRuntimeState>, ApplicationError> {
        Ok(self.states.lock().unwrap().values().cloned().collect())
    }

    fn callback(&self, key: &str) -> Option<ChannelCallback> {
        let graph = self.callbacks.lock().unwrap().get(key)?.clone();
        let state = self.get(&graph).filter(|state| state.enabled)?;
        Some(ChannelCallback {
            graph,
            owner: state.owner,
        })
    }

    fn remove<'a>(&'a self, graph: &'a str) -> BoxFuture<'a, Result<(), ApplicationError>> {
        Box::pin(async move {
            let mut states = self.states.lock().unwrap();
            if states.get(graph).is_some_and(|state| state.saved_enabled) {
                return Err(ApplicationError::Conflict(STOP_FIRST.into()));
            }
            states.remove(graph);
            Ok(())
        })
    }
}
