use anchor_graph_host::{ChannelDefinition, FileGraphBundleLoader, FilePluginCatalog};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::Notify,
    task::JoinHandle,
    time::{Instant, sleep},
};

const RESTART_DELAY: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

fn restart_delay() -> Duration {
    if cfg!(test) {
        Duration::from_millis(50)
    } else {
        RESTART_DELAY
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredChannel {
    definition: ChannelDefinition,
    executable: PathBuf,
    plugin_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct LaunchSpec {
    platform: String,
    executable: PathBuf,
    plugin_dir: PathBuf,
    environment: BTreeMap<String, OsString>,
    descriptor: PathBuf,
    control_socket: PathBuf,
}

pub(crate) struct ChannelSupervisor {
    shutdown: std::sync::Arc<Notify>,
    task: Option<JoinHandle<()>>,
    descriptors: BTreeMap<String, PathBuf>,
    cleanup_paths: Vec<(PathBuf, PathBuf)>,
}

impl ChannelSupervisor {
    pub(crate) async fn start(
        bundle_root: &Path,
        state_root: &Path,
        listen: &str,
        api_keys: &[String],
    ) -> io::Result<Option<Self>> {
        let discovered = discover(bundle_root)?;
        if discovered.is_empty() {
            return Ok(None);
        }
        let callback_override = env::var_os("ANCHOR_CHANNEL_WEBHOOK_URL");
        let callback_key = api_keys
            .first()
            .cloned()
            .unwrap_or(random_token("anchor-channel-api")?);
        let mut launches = Vec::with_capacity(discovered.len());
        let mut descriptors = BTreeMap::new();
        for channel in discovered {
            let definition = &channel.definition;
            let required_environment = definition
                .required_environment
                .iter()
                .map(|required| {
                    required_environment_value(required).map(|value| (required.clone(), value))
                })
                .collect::<io::Result<Vec<_>>>()
                .map_err(|error| {
                    io::Error::other(format!(
                        "channel {} has invalid required environment: {error}",
                        definition.platform
                    ))
                })?;
            let state_dir = state_root.join("channels").join(&definition.platform);
            private_directory(&state_dir)?;
            let descriptor = state_dir.join("control.json");
            let control_socket = state_dir.join("control.sock");
            let control_token = random_token("anchor-channel-control")?;
            let callback_url = callback_override
                .clone()
                .unwrap_or_else(|| OsString::from(callback_url(listen, &definition.platform)));
            let mut environment = BTreeMap::new();
            environment.insert("PATH".into(), parent_environment("PATH"));
            environment.insert("HOME".into(), parent_environment("HOME"));
            for (required, value) in required_environment {
                environment.insert(required, value);
            }
            environment.insert("WECOM_CHANNEL_STATE".into(), state_dir.clone().into());
            environment.insert("ANCHOR_CHANNEL_WEBHOOK_URL".into(), callback_url);
            environment.insert("ANCHOR_API_KEY".into(), callback_key.clone().into());
            environment.insert(
                "ANCHOR_CHANNEL_CONTROL_TOKEN".into(),
                control_token.clone().into(),
            );
            environment.insert(
                "ANCHOR_CHANNEL_CONTROL_DESCRIPTOR".into(),
                descriptor.clone().into(),
            );
            environment.insert(
                "ANCHOR_CHANNEL_CONTROL_SOCKET".into(),
                control_socket.clone().into(),
            );
            for name in [
                "ANCHOR_WECOM_USERS",
                "ANCHOR_WECOM_SEND_USERS",
                "ANCHOR_WECOM_WS_URL",
                "ANCHOR_WECOM_CONNECT_MS",
                "ANCHOR_WECOM_ACK_MS",
                "ANCHOR_WECOM_HEARTBEAT_MS",
                "ANCHOR_WECOM_RECONNECT_MS",
                "ANCHOR_WECOM_RECONNECT_MAX_MS",
            ] {
                if let Some(value) = env::var_os(name) {
                    environment.insert(name.into(), value);
                }
            }
            if let Some(previous) =
                descriptors.insert(definition.platform.clone(), descriptor.clone())
                && previous != descriptor
            {
                return Err(io::Error::other(format!(
                    "conflicting channel descriptors for {}",
                    definition.platform
                )));
            }
            launches.push(LaunchSpec {
                platform: definition.platform.clone(),
                executable: channel.executable,
                plugin_dir: channel.plugin_dir,
                environment,
                descriptor,
                control_socket,
            });
        }
        let shutdown = std::sync::Arc::new(Notify::new());
        let cleanup_paths = launches
            .iter()
            .map(|launch| (launch.descriptor.clone(), launch.control_socket.clone()))
            .collect();
        let task = tokio::spawn(supervise(launches, shutdown.clone()));
        Ok(Some(Self {
            shutdown,
            task: Some(task),
            descriptors,
            cleanup_paths,
        }))
    }

    pub(crate) fn descriptor(&self, platform: &str) -> Option<&Path> {
        self.descriptors.get(platform).map(PathBuf::as_path)
    }

    pub(crate) async fn shutdown(&mut self) {
        self.shutdown.notify_one();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for ChannelSupervisor {
    fn drop(&mut self) {
        self.shutdown.notify_one();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        for (descriptor, control_socket) in &self.cleanup_paths {
            remove_control_path(descriptor);
            remove_control_path(control_socket);
        }
    }
}

fn discover(bundle_root: &Path) -> io::Result<Vec<DiscoveredChannel>> {
    let bundle = FileGraphBundleLoader::new(bundle_root)
        .load()
        .map_err(|error| io::Error::other(format!("channel Graph admission failed: {error}")))?;
    let ids = bundle
        .snapshot
        .nodes
        .iter()
        .flat_map(|node| node.plugins.iter().cloned())
        .collect::<BTreeSet<_>>();
    let catalog = FilePluginCatalog::new(bundle_root);
    let mut channels = BTreeMap::<String, DiscoveredChannel>::new();
    for id in ids {
        let plugin_dir = catalog
            .plugin_directory(&id)
            .map_err(|error| io::Error::other(error.to_string()))?;
        for definition in catalog
            .channels(&id)
            .map_err(|error| io::Error::other(error.to_string()))?
        {
            let executable = plugin_dir.join(&definition.entrypoint);
            let metadata = fs::symlink_metadata(&executable)?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.permissions().mode() & 0o111 == 0
            {
                return Err(io::Error::other(format!(
                    "channel {} entrypoint is not an executable regular file",
                    definition.platform
                )));
            }
            let discovered = DiscoveredChannel {
                definition: definition.clone(),
                executable: executable
                    .canonicalize()
                    .map_err(|error| io::Error::other(error.to_string()))?,
                plugin_dir: plugin_dir.clone(),
            };
            if let Some(previous) = channels.get(&definition.platform)
                && previous != &discovered
            {
                return Err(io::Error::other(format!(
                    "conflicting channel declarations for {}",
                    definition.platform
                )));
            }
            channels.insert(definition.platform, discovered);
        }
    }
    Ok(channels.into_values().collect())
}

async fn supervise(launches: Vec<LaunchSpec>, shutdown: std::sync::Arc<Notify>) {
    let mut children = (0..launches.len())
        .map(|_| None::<Child>)
        .collect::<Vec<_>>();
    let mut restart_at = vec![Instant::now(); launches.len()];
    loop {
        let now = Instant::now();
        for (index, launch) in launches.iter().enumerate() {
            if let Some(child) = children[index].as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        eprintln!(
                            "anchor channel {} exited with {}; restarting",
                            launch.platform, status
                        );
                        children[index] = None;
                        remove_launch_control_paths(launch);
                        restart_at[index] = now + restart_delay();
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!(
                            "anchor channel {} status check failed: {}; restarting",
                            launch.platform, error
                        );
                        children[index] = None;
                        remove_launch_control_paths(launch);
                        restart_at[index] = now + restart_delay();
                    }
                }
            }
            if children[index].is_none() && now >= restart_at[index] {
                remove_launch_control_paths(launch);
                match spawn(launch) {
                    Ok(child) => children[index] = Some(child),
                    Err(error) => {
                        eprintln!(
                            "anchor channel {} failed to start: {}; retrying",
                            launch.platform, error
                        );
                        restart_at[index] = now + restart_delay();
                    }
                }
            }
        }
        tokio::select! {
            _ = shutdown.notified() => {
                for (index, launch) in launches.iter().enumerate() {
                    if let Some(mut child) = children[index].take() {
                        stop_child(&mut child).await;
                    }
                    remove_launch_control_paths(launch);
                }
                return;
            }
            _ = sleep(POLL_INTERVAL) => {}
        }
    }
}

fn spawn(launch: &LaunchSpec) -> io::Result<Child> {
    let mut command = Command::new(&launch.executable);
    command
        .args(std::iter::empty::<&str>())
        .current_dir(&launch.plugin_dir)
        .env_clear()
        .kill_on_drop(true);
    for (name, value) in &launch.environment {
        command.env(name, value);
    }
    command.spawn()
}

fn terminate(child: &mut Child) {
    if let Some(pid) = child
        .id()
        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
}

async fn stop_child(child: &mut Child) {
    terminate(child);
    if tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

fn remove_control_path(path: &Path) {
    let _ = fs::remove_file(path);
}

fn remove_launch_control_paths(launch: &LaunchSpec) {
    remove_control_path(&launch.descriptor);
    remove_control_path(&launch.control_socket);
}

fn private_directory(path: &Path) -> io::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(io::Error::other(format!(
            "channel state path is a symlink: {}",
            path.display()
        )));
    }
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn parent_environment(name: &str) -> OsString {
    env::var_os(name).unwrap_or_else(|| {
        OsString::from("/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
    })
}

fn required_environment_value(name: &str) -> io::Result<OsString> {
    let value = env::var(name).map_err(|_| {
        io::Error::other(format!(
            "required environment variable {name} is missing or not valid UTF-8"
        ))
    })?;
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(io::Error::other(format!(
            "required environment variable {name} is empty or contains control characters"
        )));
    }
    Ok(value.into())
}

fn random_token(prefix: &str) -> io::Result<String> {
    use std::io::Read;
    let mut bytes = [0_u8; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(format!("{prefix}-{}", hex(&bytes)))
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

fn callback_url(listen: &str, platform: &str) -> String {
    let port = listen
        .parse::<std::net::SocketAddr>()
        .ok()
        .map(|address| address.port())
        .or_else(|| {
            listen
                .rsplit_once(':')
                .and_then(|(_, port)| port.parse().ok())
        })
        .unwrap_or(8077);
    format!("http://127.0.0.1:{port}/channels/{platform}/events")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_graph_host::PluginCatalog;
    use serde_json::{Value, json};
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    use tempfile::TempDir;

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn bundle(channels: &[(&str, &str, &str)]) -> TempDir {
        let root = tempfile::tempdir().unwrap();
        let mut plugin_ids = Vec::new();
        let mut summaries = Vec::new();
        for (plugin, platform, entrypoint) in channels {
            let plugin_dir = root.path().join("plugins").join(plugin);
            let executable = plugin_dir.join(entrypoint);
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::write(
                plugin_dir.join("plugin.json"),
                json!({"name": plugin}).to_string(),
            )
            .unwrap();
            fs::write(
                plugin_dir.join("channel.json"),
                json!({
                    "platform": platform,
                    "transport": "websocket",
                    "entrypoint": entrypoint,
                    "required_environment": []
                })
                .to_string(),
            )
            .unwrap();
            fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            plugin_ids.push((*plugin).to_owned());
        }
        let graph = json!({
            "objective": "channel fixture",
            "entry": "worker",
            "agents": {"worker": {"model": "fixture", "instructions": "finish"}},
            "nodes": [{"id": "worker", "agent": "worker", "plugins": plugin_ids}],
            "edges": []
        });
        fs::write(root.path().join("graph.json"), graph.to_string()).unwrap();
        let bindings = FilePluginCatalog::new(root.path())
            .resolve(&plugin_ids)
            .unwrap();
        for binding in bindings {
            summaries.push(json!({
                "id": binding.id,
                "digest": binding.digest,
                "resources": binding.resources,
                "mcp_servers": binding.mcp_servers
            }));
        }
        fs::write(
            root.path().join("manifest.json"),
            json!({"format": 1, "graph": "graph.json", "plugins": summaries}).to_string(),
        )
        .unwrap();
        root
    }

    fn refresh_manifest(root: &std::path::Path, plugin_ids: &[&str]) {
        let bindings = FilePluginCatalog::new(root)
            .resolve(
                &plugin_ids
                    .iter()
                    .map(|id| (*id).to_owned())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let summaries = bindings
            .into_iter()
            .map(|binding| {
                json!({
                    "id": binding.id,
                    "digest": binding.digest,
                    "resources": binding.resources,
                    "mcp_servers": binding.mcp_servers
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            root.join("manifest.json"),
            json!({"format": 1, "graph": "graph.json", "plugins": summaries}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn discovers_only_channels_mounted_by_the_graph() {
        let root = bundle(&[("mounted", "wecom", "bin/gateway")]);
        let discovered = discover(root.path()).unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].definition.platform, "wecom");
        assert!(
            discovered[0]
                .executable
                .ends_with("plugins/mounted/bin/gateway")
        );
    }

    #[test]
    fn rejects_conflicting_platform_declarations() {
        let root = bundle(&[
            ("first", "wecom", "bin/gateway"),
            ("second", "wecom", "bin/gateway"),
        ]);
        let error = discover(root.path()).expect_err("duplicate channel must fail closed");
        assert!(
            error
                .to_string()
                .contains("conflicting channel declarations")
        );
    }

    #[tokio::test]
    async fn missing_required_channel_environment_fails_before_spawn() {
        let root = bundle(&[("mounted", "wecom", "bin/gateway")]);
        let channel = root.path().join("plugins/mounted/channel.json");
        let mut value: Value = serde_json::from_slice(&fs::read(channel).unwrap()).unwrap();
        value["required_environment"] = json!(["ANCHOR_CHANNEL_TEST_REQUIRED"]);
        fs::write(
            root.path().join("plugins/mounted/channel.json"),
            value.to_string(),
        )
        .unwrap();
        let binding = FilePluginCatalog::new(root.path())
            .resolve(&["mounted".into()])
            .unwrap()
            .remove(0);
        fs::write(
            root.path().join("manifest.json"),
            json!({
                "format": 1,
                "graph": "graph.json",
                "plugins": [{
                    "id": binding.id,
                    "digest": binding.digest,
                    "resources": binding.resources,
                    "mcp_servers": binding.mcp_servers
                }]
            })
            .to_string(),
        )
        .unwrap();
        let _guard = crate::PROCESS_ENV_LOCK.lock().await;
        let previous = env::var_os("ANCHOR_CHANNEL_TEST_REQUIRED");
        unsafe { env::remove_var("ANCHOR_CHANNEL_TEST_REQUIRED") };
        let result = ChannelSupervisor::start(
            root.path(),
            &root.path().join("state"),
            "127.0.0.1:8077",
            &[],
        )
        .await;
        if let Some(previous) = previous {
            unsafe { env::set_var("ANCHOR_CHANNEL_TEST_REQUIRED", previous) };
        }
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("missing channel credential must fail closed"),
        };
        assert!(error.to_string().contains("ANCHOR_CHANNEL_TEST_REQUIRED"));
    }

    #[tokio::test]
    async fn blank_required_channel_environment_fails_before_spawn() {
        let root = bundle(&[("mounted", "wecom", "bin/gateway")]);
        let channel = root.path().join("plugins/mounted/channel.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&channel).unwrap()).unwrap();
        value["required_environment"] = json!(["ANCHOR_CHANNEL_TEST_REQUIRED"]);
        fs::write(&channel, value.to_string()).unwrap();
        refresh_manifest(root.path(), &["mounted"]);
        let _guard = crate::PROCESS_ENV_LOCK.lock().await;
        let previous = env::var_os("ANCHOR_CHANNEL_TEST_REQUIRED");
        for blank in ["", "   "] {
            unsafe { env::set_var("ANCHOR_CHANNEL_TEST_REQUIRED", blank) };
            let result = ChannelSupervisor::start(
                root.path(),
                &root.path().join("state"),
                "127.0.0.1:8077",
                &[],
            )
            .await;
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("blank channel credential must fail closed"),
            };
            assert!(error.to_string().contains("ANCHOR_CHANNEL_TEST_REQUIRED"));
        }
        match previous {
            Some(previous) => unsafe { env::set_var("ANCHOR_CHANNEL_TEST_REQUIRED", previous) },
            None => unsafe { env::remove_var("ANCHOR_CHANNEL_TEST_REQUIRED") },
        }
    }

    #[tokio::test]
    async fn exited_channel_is_restarted_and_shutdown_reaps_it() {
        let root = bundle(&[("mounted", "wecom", "bin/gateway")]);
        let executable = root.path().join("plugins/mounted/bin/gateway");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf x >> restart-count\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        refresh_manifest(root.path(), &["mounted"]);
        let state = root
            .path()
            .join(format!("state-{}", TEST_ID.fetch_add(1, Ordering::Relaxed)));
        let mut supervisor = ChannelSupervisor::start(root.path(), &state, "127.0.0.1:8077", &[])
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(POLL_INTERVAL * 2 + restart_delay() * 2).await;
        supervisor.shutdown().await;
        let count = fs::read_to_string(root.path().join("plugins/mounted/restart-count"))
            .unwrap_or_default();
        assert!(count.len() >= 2, "channel was not restarted: {count:?}");
    }

    #[tokio::test]
    async fn restart_removes_stale_control_files() {
        let root = bundle(&[("mounted", "wecom", "bin/gateway")]);
        let state = root
            .path()
            .join(format!("state-{}", TEST_ID.fetch_add(1, Ordering::Relaxed)));
        let descriptor = state.join("channels/wecom/control.json");
        let socket = state.join("channels/wecom/control.sock");
        let executable = root.path().join("plugins/mounted/bin/gateway");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nmkdir -p '{}'\nprintf '%s' '{{}}' > '{}'\n: > '{}'\nexit 0\n",
                descriptor.parent().unwrap().display(),
                descriptor.display(),
                socket.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        refresh_manifest(root.path(), &["mounted"]);
        let mut supervisor = ChannelSupervisor::start(root.path(), &state, "127.0.0.1:8077", &[])
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(POLL_INTERVAL * 2 + restart_delay() * 2).await;
        supervisor.shutdown().await;
        assert!(!descriptor.exists());
        assert!(!socket.exists());
    }
}
